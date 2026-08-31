//! The signatures: from an event stream to a set of detections.
//!
//! Pure, in the sense that matters: no I/O, no clock, no privileges, no kernel.
//! [`Engine::observe`] is a fold over events and [`Engine::finish`] reads the
//! accumulator out. That is not tidiness for its own sake: it is the only way
//! this layer can be tested at all, because attaching a probe needs capabilities
//! no CI runner grants, and a detection engine nobody can test is one nobody
//! should believe.
//!
//! The split is the one Tracee draws (ROADMAP.md D3): the collector below knows
//! about syscalls and nothing about meaning; this layer knows about meaning and
//! has never seen a ring buffer.
//!
//! ## What a rule is allowed to claim
//!
//! Every path in an event is a string a process passed to a syscall, captured at
//! `sys_enter`. It is not the kernel's resolution of that string: symlinks are
//! unfollowed, `..` is unresolved, a relative path is relative to a directory fd
//! this probe does not know, and in principle the bytes can change between the
//! read and the kernel's use of them. So a path-matching rule is a heuristic
//! over caller-supplied strings and is documented as one (ROADMAP.md D13.3).
//! That is the price of a CO-RE-free probe, and the right price for an
//! observation-only lane.

use std::collections::{BTreeMap, HashSet};

use crate::event::{Event, EventKind, Family};
use crate::evidence::{Detection, MAX_EXAMPLE_LEN, MAX_EXAMPLES, Severity};

/// One signature, as data. `h5i box detect rules` prints this table, so what
/// the detector looks for is inspectable without reading Rust.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuleSpec {
    pub id: &'static str,
    pub family: &'static str,
    pub severity: Severity,
    /// One line, carried into the receipt with every detection.
    pub title: &'static str,
    /// Why it is worth knowing. Printed by `detect rules`, not stored.
    pub detail: &'static str,
}

/// The catalogue. Adding a rule means adding a row here and a case in
/// [`Engine::observe`]; the test at the bottom of this file fails if the two
/// ever disagree, so a rule can never be listed and unimplemented.
pub const RULES: &[RuleSpec] = &[
    // ---------------------------------------------------------------- net
    RuleSpec {
        id: "net.direct-egress",
        family: "net",
        severity: Severity::Alert,
        title: "dialled a routable address directly, around the egress allowlist",
        detail: "A `connect(2)` to a routable address from a box whose network policy is an \
                 allowlist or a denial. The CONNECT proxy can only rule on traffic that goes \
                 through it, so a socket dialled at a literal address is invisible to it by \
                 construction — and on the workspace tier, where there is no network namespace \
                 to force the issue, this is the only thing that would notice. It reports the \
                 *attempt*: the probe sees the syscall going in, not the answer coming back, so \
                 a connect the netns refused looks exactly like one that succeeded. On a \
                 `net.mode = deny` box that is the useful reading — the box tried.",
    },
    RuleSpec {
        id: "net.raw-socket",
        family: "net",
        severity: Severity::Alert,
        title: "opened a raw or packet socket",
        detail: "`SOCK_RAW`, or any `AF_PACKET` socket. A development box has no honest use for \
                 either; they are how traffic is sniffed or forged below the level anything else \
                 here inspects.",
    },
    RuleSpec {
        id: "net.unix-socket",
        family: "net",
        severity: Severity::Notice,
        title: "dialled a Unix socket on a profile that did not grant them",
        detail: "The supervised tier denies `AF_UNIX` unless the profile sets `unix_sockets`. \
                 The other tiers do not, so on those this is the only report that a box reached \
                 a local daemon — a D-Bus, a container runtime, an agent socket.",
    },
    RuleSpec {
        id: "net.dns-direct",
        family: "net",
        severity: Severity::Notice,
        title: "resolved names against a nameserver of its own choosing",
        detail: "A connect to port 53 or 853. Ordinary for a box with unrestricted networking, \
                 and worth a line on one that is supposed to be proxied: DNS is a serviceable \
                 exfiltration channel and it is not HTTP, so the proxy never sees it.",
    },
    // ------------------------------------------------------------- secret
    RuleSpec {
        id: "secret.read",
        family: "secret",
        severity: Severity::Alert,
        title: "opened a credential file",
        detail: "An open of a well-known credential path: `.ssh`, `.aws/credentials`, \
                 `.config/gh`, `.git-credentials`, `.netrc`, `.kube/config`, \
                 `.docker/config.json`, `.npmrc`, `.pypirc`, `.gnupg`. Filesystem grants are \
                 directories and credentials are files inside them, so a profile that grants \
                 `$HOME` cannot express this and only observation can report it.",
    },
    RuleSpec {
        id: "secret.dotenv",
        family: "secret",
        severity: Severity::Notice,
        title: "opened a .env file outside the workspace",
        detail: "A `.env`-family file that is not the project's own. Reading the workspace's \
                 `.env` is the job; reading one from somewhere else is somebody else's secrets.",
    },
    RuleSpec {
        id: "secret.proc-environ",
        family: "secret",
        severity: Severity::Alert,
        title: "read another process's environment",
        detail: "An open of `/proc/<pid>/environ` for a pid that is not part of this box. That \
                 file is where a host process keeps its API keys, and reading it is the classic \
                 way out of a boundary that only guarded the filesystem.",
    },
    RuleSpec {
        id: "secret.h5i-state",
        family: "secret",
        severity: Severity::Alert,
        title: "opened h5i's own control directory for writing",
        detail: "A write-intent open under the box's `.h5i` directory. That is where the \
                 manifest, the policy and the receipts live: a box editing it is a box editing \
                 the evidence about itself.",
    },
    // --------------------------------------------------------------- exec
    RuleSpec {
        id: "exec.from-tmp",
        family: "exec",
        severity: Severity::Notice,
        title: "executed a binary from a temporary directory",
        detail: "An exec of a path under `/tmp`, `/var/tmp` or `/dev/shm`. Build systems do \
                 this legitimately and so does every dropper ever written; the value is that \
                 the two are now distinguishable by looking rather than by assuming.",
    },
    RuleSpec {
        id: "exec.memfd",
        family: "exec",
        severity: Severity::Alert,
        title: "executed a file that was never on disk",
        detail: "A `memfd_create` followed by an exec of the resulting descriptor by the same \
                 process. Fileless execution: nothing is written, so nothing can be scanned, \
                 and the only trace is the pair of syscalls. This is the reason `memfd_create` \
                 is collected at all.",
    },
    RuleSpec {
        id: "exec.interpreter-pipe",
        family: "exec",
        severity: Severity::Notice,
        title: "piped a download into a shell",
        detail: "A shell invoked with `-c` whose command line has the download-and-execute \
                 shape (`curl … | sh`). It is how half the software on the internet is \
                 installed and how the other half is compromised.",
    },
    RuleSpec {
        id: "exec.package-manager",
        family: "exec",
        severity: Severity::Info,
        title: "ran a package manager",
        detail: "npm, pnpm, yarn, pip, cargo, gem, go, bundle, poetry or uv. Not suspicious. \
                 Present because \"what installed things, and when\" is the first question \
                 asked of any supply-chain incident, and it is a miserable question to answer \
                 after the fact.",
    },
    // --------------------------------------------------------- privilege
    RuleSpec {
        id: "priv.ptrace",
        family: "priv",
        severity: Severity::Alert,
        title: "attached a debugger to a process outside the box",
        detail: "A `ptrace` request against a pid this run never spawned. Attaching to a host \
                 process reads its memory, which is every secret it holds.",
    },
    RuleSpec {
        id: "priv.namespace",
        family: "priv",
        severity: Severity::Notice,
        title: "changed namespaces",
        detail: "`unshare` or `setns`. The supervised tier denies `unshare` outright; the other \
                 tiers do not, and a new user namespace is the first step of most attempts to \
                 acquire capabilities the box was not given.",
    },
    RuleSpec {
        id: "kernel.bpf",
        family: "kernel",
        severity: Severity::Alert,
        title: "called bpf(2)",
        detail: "The box loading BPF of its own. Worth an alert on its own terms, and worth it \
                 twice over here: this lane is itself BPF, and a box that can load programs is \
                 a box in a position to argue with the thing watching it.",
    },
    RuleSpec {
        id: "kernel.module",
        family: "kernel",
        severity: Severity::Alert,
        title: "tried to load a kernel module",
        detail: "`init_module` or `finit_module`. Nothing a development box does needs this. \
                 It will fail without `CAP_SYS_MODULE`; that it was attempted is the finding.",
    },
    RuleSpec {
        id: "mount.change",
        family: "mount",
        severity: Severity::Notice,
        title: "changed the mount table",
        detail: "`mount` or `pivot_root`. Expected of a container runtime, unexpected of a \
                 build, and the mechanism by which a filesystem grant is made to point \
                 somewhere else.",
    },
];

/// Look a rule up by id.
pub fn rule(id: &str) -> Option<&'static RuleSpec> {
    RULES.iter().find(|r| r.id == id)
}

/// Resolve a selector list (rule ids, family names, or `*`) into the set of
/// rule ids it enables.
///
/// Unknown selectors are returned separately rather than ignored: a typo in a
/// profile that silently disables a rule is exactly the kind of quiet failure
/// this whole part of the product exists to avoid.
pub fn select(selectors: &[String]) -> (HashSet<&'static str>, Vec<String>) {
    let mut on: HashSet<&'static str> = HashSet::new();
    let mut unknown = Vec::new();
    for sel in selectors {
        if sel == "*" {
            on.extend(RULES.iter().map(|r| r.id));
            continue;
        }
        if let Some(r) = rule(sel) {
            on.insert(r.id);
            continue;
        }
        let family: Vec<&'static str> = RULES
            .iter()
            .filter(|r| r.family == sel)
            .map(|r| r.id)
            .collect();
        if family.is_empty() {
            unknown.push(sel.clone());
        } else {
            on.extend(family);
        }
    }
    (on, unknown)
}

/// What the engine needs to know about the box to judge an event.
///
/// All of it comes from the resolved policy and the box's own layout, so a
/// detection is always relative to what *this* box was allowed, never to a
/// global idea of suspicious.
#[derive(Debug, Clone, Default)]
pub struct RuleContext {
    /// The profile's `net.mode`: `deny`, `proxy`, or `allow`.
    pub net_mode: String,
    /// Whether the profile granted `unix_sockets`.
    pub unix_sockets: bool,
    /// The box's workspace root, as the box sees it. A `.env` inside is the
    /// project's; one outside is somebody else's.
    pub workspace: String,
    /// The home directory whose dotfiles the credential rules are about.
    pub home: String,
    /// The box's `.h5i` control directory.
    pub control_dir: String,
    /// Addresses the box is *supposed* to dial: the egress proxy's own
    /// endpoints. Without these, every proxied request would be reported as
    /// direct egress, which would make the loudest rule in the catalogue the
    /// least believable one.
    pub proxy_peers: Vec<String>,
    /// Rule ids that are on. Empty means every rule.
    pub enabled: HashSet<&'static str>,
}

impl RuleContext {
    fn on(&self, id: &str) -> bool {
        self.enabled.is_empty() || self.enabled.contains(id)
    }
}

/// A rule's running total.
#[derive(Debug, Clone)]
struct Acc {
    spec: &'static RuleSpec,
    count: u64,
    first_ns: u64,
    last_ns: u64,
    examples: Vec<String>,
    truncated: bool,
}

/// The fold. Feed it events; ask it for detections.
#[derive(Debug, Clone)]
pub struct Engine {
    ctx: RuleContext,
    acc: BTreeMap<&'static str, Acc>,
    /// Every tid and tgid the stream has mentioned. Used to tell "a process of
    /// this box" from "a process of this host", which is what makes
    /// `priv.ptrace` and `secret.proc-environ` mean anything.
    known_pids: HashSet<u32>,
    /// Processes that have called `memfd_create` and not yet execed, for
    /// `exec.memfd`.
    memfd_pending: HashSet<u32>,
    /// Parent of each pid, learned from `Fork`. The probe cannot supply it
    /// (ROADMAP.md D5), and lineage is what most of the interesting questions
    /// are actually about.
    parents: BTreeMap<u32, u32>,
    seen: u64,
}

/// Credential paths, matched as a suffix against the home-relative part of an
/// open. Suffixes rather than absolute paths because the box's home is not the
/// host's, and a rule that hardcoded one would fire on neither.
const CREDENTIAL_MARKERS: &[&str] = &[
    "/.ssh/",
    "/.aws/credentials",
    "/.aws/config",
    "/.config/gh/",
    "/.git-credentials",
    "/.netrc",
    "/.kube/config",
    "/.docker/config.json",
    "/.npmrc",
    "/.pypirc",
    "/.gnupg/",
    "/.config/gcloud/",
    "/.config/h5i/",
];

const PACKAGE_MANAGERS: &[&str] = &[
    "npm", "npx", "pnpm", "yarn", "pip", "pip3", "cargo", "gem", "go", "bundle", "poetry", "uv",
    "composer", "maven", "gradle",
];

const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ash", "ksh"];

const TMP_PREFIXES: &[&str] = &["/tmp/", "/var/tmp/", "/dev/shm/"];

/// `SOCK_RAW`. `AF_PACKET` is 17.
const SOCK_RAW: i64 = 3;
const AF_PACKET: i64 = 17;
/// `PTRACE_TRACEME`. A process asking to be traced is not attaching to
/// anything, so it is not what the rule is about.
const PTRACE_TRACEME: i64 = 0;

impl Engine {
    pub fn new(ctx: RuleContext) -> Self {
        Self {
            ctx,
            acc: BTreeMap::new(),
            known_pids: HashSet::new(),
            memfd_pending: HashSet::new(),
            parents: BTreeMap::new(),
            seen: 0,
        }
    }

    pub fn events_seen(&self) -> u64 {
        self.seen
    }

    /// The parent of a pid, as far as the Fork events said. Used by the
    /// session to fill `Event::ppid`, which the probe cannot.
    pub fn parent_of(&self, pid: u32) -> Option<u32> {
        self.parents.get(&pid).copied()
    }

    fn fire(&mut self, id: &'static str, ev: &Event, example: String) {
        if !self.ctx.on(id) {
            return;
        }
        let Some(spec) = rule(id) else { return };
        let entry = self.acc.entry(id).or_insert_with(|| Acc {
            spec,
            count: 0,
            first_ns: ev.ts_ns,
            last_ns: ev.ts_ns,
            examples: Vec::new(),
            truncated: false,
        });
        entry.count += 1;
        entry.first_ns = entry.first_ns.min(ev.ts_ns);
        entry.last_ns = entry.last_ns.max(ev.ts_ns);
        if entry.examples.len() < MAX_EXAMPLES {
            let line = sanitize(&format!("{} [{}:{}]", example, ev.comm, ev.tgid));
            if !entry.examples.contains(&line) {
                entry.examples.push(line);
            }
        } else {
            entry.truncated = true;
        }
    }

    /// Fold one event in.
    pub fn observe(&mut self, ev: &Event) {
        self.seen += 1;
        self.known_pids.insert(ev.tid);
        self.known_pids.insert(ev.tgid);

        match ev.kind {
            EventKind::Fork => {
                let child = u32::try_from(ev.a0).unwrap_or(0);
                if child != 0 {
                    self.known_pids.insert(child);
                    self.parents.insert(child, ev.tid);
                }
            }
            EventKind::Exit => {}
            EventKind::Memfd => {
                self.memfd_pending.insert(ev.tgid);
            }
            EventKind::Exec => self.on_exec(ev),
            EventKind::Open => self.on_open(ev),
            EventKind::Connect => self.on_connect(ev),
            EventKind::Socket => self.on_socket(ev),
            EventKind::Ptrace => self.on_ptrace(ev),
            EventKind::Bpf => {
                self.fire("kernel.bpf", ev, format!("bpf(cmd={})", ev.a0));
            }
            EventKind::Nsop => {
                let verb = if ev.a1 == 0 { "unshare" } else { "setns" };
                self.fire("priv.namespace", ev, format!("{verb}(flags={:#x})", ev.a0));
            }
            EventKind::Module => {
                let verb = if ev.a0 == 0 {
                    "init_module"
                } else {
                    "finit_module"
                };
                self.fire("kernel.module", ev, format!("{verb} {}", ev.path));
            }
            EventKind::Mount => {
                let verb = if ev.a0 == 0 { "mount" } else { "pivot_root" };
                self.fire("mount.change", ev, format!("{verb} {} <- {}", ev.path, ev.aux));
            }
            EventKind::Unknown(_) => {}
        }
    }

    fn on_exec(&mut self, ev: &Event) {
        let path = ev.path.clone();

        // Fileless execution: the descriptor was made by this process and is
        // now what it is running. Both halves are needed. A `/proc/self/fd/N`
        // exec on its own is an ordinary way to run a downloaded script.
        let is_fd_exec = path.starts_with("/proc/self/fd/")
            || (path.starts_with("/proc/") && path.contains("/fd/"))
            || path.starts_with("/memfd:");
        if is_fd_exec && self.memfd_pending.contains(&ev.tgid) {
            self.fire("exec.memfd", ev, format!("exec {path}"));
        }
        // Whatever it was, the pending memfd has been consumed.
        self.memfd_pending.remove(&ev.tgid);

        if TMP_PREFIXES.iter().any(|p| path.starts_with(p)) {
            self.fire("exec.from-tmp", ev, format!("exec {path}"));
        }

        let base = basename(&path);
        if PACKAGE_MANAGERS.contains(&base) {
            self.fire("exec.package-manager", ev, ev.cmdline());
        }
        if SHELLS.contains(&base) && ev.aux == "-c" && looks_like_download_pipe(&ev.aux2) {
            self.fire("exec.interpreter-pipe", ev, ev.cmdline());
        }
    }

    fn on_open(&mut self, ev: &Event) {
        let path = &ev.path;

        if CREDENTIAL_MARKERS.iter().any(|m| path.contains(m)) {
            self.fire("secret.read", ev, format!("open {path}"));
        }

        if is_dotenv(path) && !self.under(path, &self.ctx.workspace) {
            self.fire("secret.dotenv", ev, format!("open {path}"));
        }

        if let Some(pid) = proc_environ_pid(path) {
            // A box reading its own processes' environments is a box reading
            // its own environment, which it already has.
            if !self.known_pids.contains(&pid) {
                self.fire(
                    "secret.proc-environ",
                    ev,
                    format!("open /proc/{pid}/environ"),
                );
            }
        }

        if ev.write_intent() && self.under(path, &self.ctx.control_dir) {
            self.fire("secret.h5i-state", ev, format!("write {path}"));
        }
    }

    fn on_connect(&mut self, ev: &Event) {
        let Some(family) = ev.family() else { return };
        match family {
            Family::Unix => {
                if !self.ctx.unix_sockets {
                    let peer = ev.peer.clone().unwrap_or_else(|| "<unnamed>".into());
                    self.fire("net.unix-socket", ev, format!("connect unix {peer}"));
                }
            }
            Family::Inet | Family::Inet6 => {
                let Some(peer) = ev.peer.clone() else { return };
                let port = ev.port().unwrap_or(0);
                if port == 53 || port == 853 {
                    self.fire("net.dns-direct", ev, format!("connect {peer}:{port}"));
                }
                let allowlisted = self.ctx.net_mode == "allow";
                let expected = self.ctx.proxy_peers.iter().any(|p| p == &peer);
                if !allowlisted && !expected && !is_local(&peer) {
                    self.fire("net.direct-egress", ev, format!("connect {peer}:{port}"));
                }
            }
            Family::Other(_) => {}
        }
    }

    fn on_socket(&mut self, ev: &Event) {
        // `type` carries flags (SOCK_CLOEXEC, SOCK_NONBLOCK) in its high bits.
        let sock_type = ev.a1 & 0xf;
        if sock_type == SOCK_RAW || ev.a0 == AF_PACKET {
            self.fire(
                "net.raw-socket",
                ev,
                format!("socket(family={}, type={}, proto={})", ev.a0, sock_type, ev.a2),
            );
        }
    }

    fn on_ptrace(&mut self, ev: &Event) {
        if ev.a0 == PTRACE_TRACEME {
            return;
        }
        let target = u32::try_from(ev.a1).unwrap_or(0);
        if target != 0 && !self.known_pids.contains(&target) {
            self.fire(
                "priv.ptrace",
                ev,
                format!("ptrace(request={}, pid={target})", ev.a0),
            );
        }
    }

    /// Is `path` inside `root`? False when `root` is empty, which is what an
    /// unconfigured context looks like. A rule must never fire on "everything
    /// is inside nothing".
    fn under(&self, path: &str, root: &str) -> bool {
        if root.is_empty() {
            return false;
        }
        let root = root.trim_end_matches('/');
        path == root || path.starts_with(&format!("{root}/"))
    }

    /// Read the accumulator out, worst first, then by rule id so the order is
    /// stable across runs and a diff of two receipts is readable.
    pub fn finish(self) -> Vec<Detection> {
        let mut out: Vec<Detection> = self
            .acc
            .into_values()
            .map(|a| Detection {
                rule: a.spec.id.to_string(),
                family: a.spec.family.to_string(),
                severity: a.spec.severity,
                title: a.spec.title.to_string(),
                count: a.count,
                first_ns: a.first_ns,
                last_ns: a.last_ns,
                examples: a.examples,
                examples_truncated: a.truncated,
            })
            .collect();
        out.sort_by(|x, y| y.severity.cmp(&x.severity).then_with(|| x.rule.cmp(&y.rule)));
        out
    }
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn is_dotenv(path: &str) -> bool {
    let base = basename(path);
    base == ".env" || base.starts_with(".env.")
}

/// `/proc/<pid>/environ`, and only that. `/proc/self/environ` is the process's
/// own and is not a finding.
fn proc_environ_pid(path: &str) -> Option<u32> {
    let rest = path.strip_prefix("/proc/")?;
    let (head, tail) = rest.split_once('/')?;
    if tail != "environ" {
        return None;
    }
    head.parse::<u32>().ok()
}

/// Loopback, link-local, and the unspecified address. Not "private": a box
/// dialling `192.168.1.10` has reached the user's LAN, which is exactly the
/// kind of egress the allowlist is about.
fn is_local(peer: &str) -> bool {
    if peer.starts_with("127.") || peer == "0.0.0.0" {
        return true;
    }
    if peer.starts_with("169.254.") {
        return true;
    }
    // IPv6, as this build renders it: eight colon-separated groups.
    let v6_loopback = peer
        .split(':')
        .enumerate()
        .all(|(i, g)| if i == 7 { g == "1" || g == "0" } else { g == "0" });
    peer.contains(':') && v6_loopback
}

/// The download-and-pipe shape, as a shell writes it.
fn looks_like_download_pipe(cmd: &str) -> bool {
    let fetches = ["curl ", "wget ", "fetch ", "iwr ", "http "];
    let sinks = ["| sh", "|sh", "| bash", "|bash", "| python", "|python", "| zsh", "|zsh"];
    fetches.iter().any(|f| cmd.contains(f)) && sinks.iter().any(|s| cmd.contains(s))
}

/// Make an exemplar safe to print and bounded in size.
///
/// The string came out of a box. It reaches a terminal (the CLI), an HTML page
/// (the console) and a git ref (the export), so the control sequences go
/// first. The same treatment every other box-written string in h5i gets, via
/// the same helper.
fn sanitize(s: &str) -> String {
    let cleaned = h5i_error::redact::sanitize_display(s);
    if cleaned.len() <= MAX_EXAMPLE_LEN {
        return cleaned;
    }
    let mut cut = MAX_EXAMPLE_LEN;
    while cut > 0 && !cleaned.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &cleaned[..cut])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{AUX_HALF, AUX_LEN, EVENT_MAGIC, EVENT_VERSION, PATH_LEN, RawEvent};

    /// Build an event without going near a kernel. Every test in this module
    /// is a synthetic stream, which is the point: the rules are the layer that
    /// can be held to account on a machine with no privileges at all.
    fn ev(kind: EventKind, tgid: u32) -> Event {
        Event {
            kind,
            ts_ns: 1_000,
            tgid,
            tid: tgid,
            ppid: 0,
            uid: 1000,
            a0: 0,
            a1: 0,
            a2: 0,
            comm: "test".into(),
            path: String::new(),
            aux: String::new(),
            aux2: String::new(),
            peer: None,
        }
    }

    fn ctx() -> RuleContext {
        RuleContext {
            net_mode: "proxy".into(),
            unix_sockets: false,
            workspace: "/work".into(),
            home: "/home/box".into(),
            control_dir: "/work/.h5i".into(),
            proxy_peers: vec!["10.0.2.2".into()],
            enabled: HashSet::new(),
        }
    }

    fn run(events: Vec<Event>) -> Vec<Detection> {
        let mut e = Engine::new(ctx());
        for x in &events {
            e.observe(x);
        }
        e.finish()
    }

    fn fired(d: &[Detection], id: &str) -> bool {
        d.iter().any(|x| x.rule == id)
    }

    // ------------------------------------------------------------- network

    #[test]
    fn a_direct_connection_out_is_an_alert() {
        let mut c = ev(EventKind::Connect, 10);
        c.a0 = 2;
        c.a1 = 443;
        c.peer = Some("93.184.216.34".into());
        let d = run(vec![c]);
        assert!(fired(&d, "net.direct-egress"));
        assert_eq!(d[0].severity, Severity::Alert);
    }

    /// The proxy's own endpoint must be exempt, or the loudest rule in the
    /// catalogue fires on every correctly-proxied request and stops meaning
    /// anything.
    #[test]
    fn the_egress_proxy_itself_is_not_direct_egress() {
        let mut c = ev(EventKind::Connect, 10);
        c.a0 = 2;
        c.a1 = 8080;
        c.peer = Some("10.0.2.2".into());
        assert!(!fired(&run(vec![c]), "net.direct-egress"));
    }

    #[test]
    fn loopback_is_not_egress() {
        for addr in ["127.0.0.1", "0:0:0:0:0:0:0:1"] {
            let mut c = ev(EventKind::Connect, 10);
            c.a0 = if addr.contains(':') { 10 } else { 2 };
            c.a1 = 3000;
            c.peer = Some(addr.into());
            assert!(
                !fired(&run(vec![c]), "net.direct-egress"),
                "{addr} should be local"
            );
        }
    }

    /// A LAN address is *not* local for this purpose. Reaching the user's
    /// router or NAS is exactly the egress an allowlist is about.
    #[test]
    fn a_lan_address_is_still_egress() {
        let mut c = ev(EventKind::Connect, 10);
        c.a0 = 2;
        c.a1 = 445;
        c.peer = Some("192.168.1.10".into());
        assert!(fired(&run(vec![c]), "net.direct-egress"));
    }

    #[test]
    fn an_unrestricted_box_is_not_reported_for_using_its_network() {
        let mut c = ev(EventKind::Connect, 10);
        c.a0 = 2;
        c.a1 = 443;
        c.peer = Some("93.184.216.34".into());
        let mut cx = ctx();
        cx.net_mode = "allow".into();
        let mut e = Engine::new(cx);
        e.observe(&c);
        assert!(!fired(&e.finish(), "net.direct-egress"));
    }

    #[test]
    fn dns_is_reported_separately_from_egress() {
        let mut c = ev(EventKind::Connect, 10);
        c.a0 = 2;
        c.a1 = 53;
        c.peer = Some("8.8.8.8".into());
        let d = run(vec![c]);
        assert!(fired(&d, "net.dns-direct"));
        assert!(fired(&d, "net.direct-egress"));
    }

    #[test]
    fn raw_and_packet_sockets_both_fire() {
        let mut raw = ev(EventKind::Socket, 10);
        raw.a0 = 2;
        raw.a1 = SOCK_RAW;
        assert!(fired(&run(vec![raw]), "net.raw-socket"));

        let mut packet = ev(EventKind::Socket, 10);
        packet.a0 = AF_PACKET;
        packet.a1 = 2; // SOCK_DGRAM
        assert!(fired(&run(vec![packet]), "net.raw-socket"));
    }

    /// `SOCK_CLOEXEC` and `SOCK_NONBLOCK` ride in the high bits of `type`. A
    /// naive equality test misses every socket opened the way modern code
    /// opens them.
    #[test]
    fn socket_type_flags_do_not_hide_a_raw_socket() {
        let mut raw = ev(EventKind::Socket, 10);
        raw.a0 = 2;
        raw.a1 = SOCK_RAW | 0o2000000 | 0o4000; // CLOEXEC | NONBLOCK
        assert!(fired(&run(vec![raw]), "net.raw-socket"));
    }

    #[test]
    fn unix_sockets_fire_only_when_the_profile_did_not_grant_them() {
        let mut c = ev(EventKind::Connect, 10);
        c.a0 = 1;
        c.peer = Some("@h5i.browser".into());
        assert!(fired(&run(vec![c.clone()]), "net.unix-socket"));

        let mut cx = ctx();
        cx.unix_sockets = true;
        let mut e = Engine::new(cx);
        e.observe(&c);
        assert!(!fired(&e.finish(), "net.unix-socket"));
    }

    // ------------------------------------------------------------- secrets

    #[test]
    fn credential_paths_fire() {
        for p in [
            "/home/box/.ssh/id_ed25519",
            "/home/box/.aws/credentials",
            "/root/.config/gh/hosts.yml",
            "/home/box/.git-credentials",
            "/home/box/.docker/config.json",
        ] {
            let mut o = ev(EventKind::Open, 10);
            o.path = p.into();
            assert!(fired(&run(vec![o]), "secret.read"), "{p}");
        }
    }

    #[test]
    fn an_ordinary_source_file_fires_nothing() {
        let mut o = ev(EventKind::Open, 10);
        o.path = "/work/src/main.rs".into();
        assert!(run(vec![o]).is_empty());
    }

    #[test]
    fn the_projects_own_dotenv_is_the_job_not_a_finding() {
        let mut inside = ev(EventKind::Open, 10);
        inside.path = "/work/.env".into();
        assert!(!fired(&run(vec![inside]), "secret.dotenv"));

        let mut outside = ev(EventKind::Open, 10);
        outside.path = "/home/box/other-project/.env.production".into();
        assert!(fired(&run(vec![outside]), "secret.dotenv"));
    }

    #[test]
    fn reading_a_foreign_processs_environment_is_an_alert() {
        let mut o = ev(EventKind::Open, 10);
        o.path = "/proc/4242/environ".into();
        assert!(fired(&run(vec![o]), "secret.proc-environ"));
    }

    /// A box reading the environment of its *own* process learns nothing it
    /// did not already have. Firing there would bury the case that matters.
    #[test]
    fn reading_its_own_processs_environment_is_not() {
        let mut o = ev(EventKind::Open, 10);
        o.path = "/proc/10/environ".into();
        assert!(!fired(&run(vec![o]), "secret.proc-environ"));

        let mut own = ev(EventKind::Open, 10);
        own.path = "/proc/self/environ".into();
        assert!(!fired(&run(vec![own]), "secret.proc-environ"));
    }

    #[test]
    fn writing_h5is_control_directory_is_an_alert_but_reading_it_is_not() {
        let mut w = ev(EventKind::Open, 10);
        w.path = "/work/.h5i/env.toml".into();
        w.a1 = 1; // write intent
        assert!(fired(&run(vec![w]), "secret.h5i-state"));

        let mut r = ev(EventKind::Open, 10);
        r.path = "/work/.h5i/env.toml".into();
        assert!(!fired(&run(vec![r]), "secret.h5i-state"));
    }

    /// An unconfigured context must not turn every path into a match. The
    /// naive `starts_with("")` is true for every string on earth.
    #[test]
    fn an_empty_root_matches_nothing() {
        let mut w = ev(EventKind::Open, 10);
        w.path = "/etc/passwd".into();
        w.a1 = 1;
        let mut cx = ctx();
        cx.control_dir = String::new();
        cx.workspace = String::new();
        let mut e = Engine::new(cx);
        e.observe(&w);
        assert!(!fired(&e.finish(), "secret.h5i-state"));
    }

    /// `/work/.h5i-notes` is not inside `/work/.h5i`.
    #[test]
    fn a_sibling_with_a_shared_prefix_is_not_inside() {
        let mut w = ev(EventKind::Open, 10);
        w.path = "/work/.h5i-notes/scratch".into();
        w.a1 = 1;
        assert!(!fired(&run(vec![w]), "secret.h5i-state"));
    }

    // ---------------------------------------------------------- execution

    #[test]
    fn fileless_execution_needs_both_halves() {
        let memfd = ev(EventKind::Memfd, 10);
        let mut exec = ev(EventKind::Exec, 10);
        exec.path = "/proc/self/fd/3".into();
        assert!(fired(&run(vec![memfd, exec.clone()]), "exec.memfd"));

        // The exec alone is an ordinary way to run a downloaded script.
        assert!(!fired(&run(vec![exec]), "exec.memfd"));
    }

    /// The pending memfd belongs to the process that created it. Another
    /// process's exec must not consume it.
    #[test]
    fn a_memfd_does_not_arm_a_different_process() {
        let memfd = ev(EventKind::Memfd, 10);
        let mut exec = ev(EventKind::Exec, 11);
        exec.path = "/proc/self/fd/3".into();
        assert!(!fired(&run(vec![memfd, exec]), "exec.memfd"));
    }

    #[test]
    fn execution_from_tmp_fires() {
        let mut e = ev(EventKind::Exec, 10);
        e.path = "/tmp/.build-helper".into();
        assert!(fired(&run(vec![e]), "exec.from-tmp"));
    }

    #[test]
    fn package_managers_are_recorded_at_info() {
        let mut e = ev(EventKind::Exec, 10);
        e.path = "/usr/bin/npm".into();
        e.aux = "ci".into();
        let d = run(vec![e]);
        assert!(fired(&d, "exec.package-manager"));
        assert_eq!(d[0].severity, Severity::Info);
    }

    #[test]
    fn curl_piped_into_a_shell_fires() {
        let mut e = ev(EventKind::Exec, 10);
        e.path = "/bin/sh".into();
        e.aux = "-c".into();
        e.aux2 = "curl -fsSL https://example.com/i.sh | sh".into();
        assert!(fired(&run(vec![e]), "exec.interpreter-pipe"));
    }

    #[test]
    fn an_ordinary_shell_command_does_not() {
        let mut e = ev(EventKind::Exec, 10);
        e.path = "/bin/sh".into();
        e.aux = "-c".into();
        e.aux2 = "cargo test --all".into();
        assert!(!fired(&run(vec![e]), "exec.interpreter-pipe"));
    }

    // ---------------------------------------------------------- privilege

    #[test]
    fn ptrace_of_a_host_process_fires_and_of_our_own_does_not() {
        let mut foreign = ev(EventKind::Ptrace, 10);
        foreign.a0 = 16; // PTRACE_ATTACH
        foreign.a1 = 9999;
        assert!(fired(&run(vec![foreign]), "priv.ptrace"));

        let mut sibling = ev(EventKind::Ptrace, 10);
        sibling.a0 = 16;
        sibling.a1 = 10;
        assert!(!fired(&run(vec![sibling]), "priv.ptrace"));
    }

    /// A process asking to be traced (a debugger's own child, a crash handler)
    /// is not attaching to anybody.
    #[test]
    fn ptrace_traceme_is_not_an_attach() {
        let mut me = ev(EventKind::Ptrace, 10);
        me.a0 = PTRACE_TRACEME;
        me.a1 = 0;
        assert!(!fired(&run(vec![me]), "priv.ptrace"));
    }

    /// A pid learned from a Fork counts as ours even though it never raised an
    /// event of its own.
    #[test]
    fn fork_teaches_the_engine_which_pids_are_ours() {
        let mut fork = ev(EventKind::Fork, 10);
        fork.a0 = 77;
        let mut trace = ev(EventKind::Ptrace, 10);
        trace.a0 = 16;
        trace.a1 = 77;
        assert!(!fired(&run(vec![fork, trace]), "priv.ptrace"));
    }

    #[test]
    fn lineage_is_reconstructed_from_forks() {
        let mut fork = ev(EventKind::Fork, 10);
        fork.a0 = 77;
        let mut e = Engine::new(ctx());
        e.observe(&fork);
        assert_eq!(e.parent_of(77), Some(10));
        assert_eq!(e.parent_of(78), None);
    }

    #[test]
    fn kernel_facilities_fire() {
        assert!(fired(&run(vec![ev(EventKind::Bpf, 10)]), "kernel.bpf"));
        assert!(fired(&run(vec![ev(EventKind::Module, 10)]), "kernel.module"));
        assert!(fired(&run(vec![ev(EventKind::Nsop, 10)]), "priv.namespace"));
        assert!(fired(&run(vec![ev(EventKind::Mount, 10)]), "mount.change"));
    }

    // ------------------------------------------------------- folding & io

    #[test]
    fn repeats_fold_into_one_detection_with_a_count() {
        let mut events = Vec::new();
        for i in 0..400 {
            let mut o = ev(EventKind::Open, 10);
            o.ts_ns = 1000 + i;
            o.path = format!("/home/box/.ssh/key{i}");
            events.push(o);
        }
        let d = run(events);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].count, 400);
        assert_eq!(d[0].examples.len(), MAX_EXAMPLES);
        assert!(d[0].examples_truncated);
        assert_eq!(d[0].first_ns, 1000);
        assert_eq!(d[0].last_ns, 1399);
    }

    #[test]
    fn detections_come_out_worst_first() {
        let mut pm = ev(EventKind::Exec, 10);
        pm.path = "/usr/bin/npm".into();
        let mut cred = ev(EventKind::Open, 10);
        cred.path = "/home/box/.ssh/id_rsa".into();
        let d = run(vec![pm, cred]);
        assert_eq!(d[0].severity, Severity::Alert);
        assert_eq!(d[1].severity, Severity::Info);
    }

    /// An exemplar is a string a box chose. It reaches a terminal, an HTML
    /// page and a git ref, so an escape sequence in it must not survive.
    #[test]
    fn exemplars_are_stripped_of_control_sequences_and_bounded() {
        let mut o = ev(EventKind::Open, 10);
        o.path = format!("/home/box/.ssh/\x1b[2J{}", "A".repeat(500));
        let d = run(vec![o]);
        let ex = &d[0].examples[0];
        assert!(!ex.contains('\x1b'), "{ex}");
        assert!(ex.len() <= MAX_EXAMPLE_LEN + 4, "{}", ex.len());
    }

    // ------------------------------------------------------------ config

    #[test]
    fn selectors_resolve_ids_families_and_star() {
        let (all, unknown) = select(&["*".into()]);
        assert_eq!(all.len(), RULES.len());
        assert!(unknown.is_empty());

        let (net, _) = select(&["net".into()]);
        assert_eq!(net.len(), RULES.iter().filter(|r| r.family == "net").count());

        let (one, _) = select(&["kernel.bpf".into()]);
        assert_eq!(one.len(), 1);
    }

    /// A typo must be reported, not silently swallowed. A profile that thinks
    /// it enabled a rule and did not is the exact failure this lane exists to
    /// stop happening elsewhere.
    #[test]
    fn an_unknown_selector_is_reported() {
        let (on, unknown) = select(&["net.direct-egres".into()]);
        assert!(on.is_empty());
        assert_eq!(unknown, vec!["net.direct-egres".to_string()]);
    }

    #[test]
    fn a_disabled_rule_does_not_fire() {
        let mut cx = ctx();
        cx.enabled = select(&["net".into()]).0;
        let mut e = Engine::new(cx);
        let mut o = ev(EventKind::Open, 10);
        o.path = "/home/box/.ssh/id_rsa".into();
        e.observe(&o);
        assert!(e.finish().is_empty());
    }

    // ----------------------------------------------------------- catalogue

    /// Every rule in the table must be reachable from `observe`. A row nobody
    /// implements is a promise in `detect rules` that the detector does not
    /// keep.
    #[test]
    fn every_listed_rule_can_actually_fire() {
        let unreachable: Vec<&str> = RULES
            .iter()
            .map(|r| r.id)
            .filter(|id| !fires_somewhere(id))
            .collect();
        assert!(unreachable.is_empty(), "listed but never fired: {unreachable:?}");
    }

    /// Drive one rule with the stream that should trip it.
    fn fires_somewhere(id: &str) -> bool {
        let d = match id {
            "net.direct-egress" | "net.dns-direct" => {
                let mut c = ev(EventKind::Connect, 10);
                c.a0 = 2;
                c.a1 = if id == "net.dns-direct" { 53 } else { 443 };
                c.peer = Some("8.8.8.8".into());
                run(vec![c])
            }
            "net.raw-socket" => {
                let mut s = ev(EventKind::Socket, 10);
                s.a0 = 2;
                s.a1 = SOCK_RAW;
                run(vec![s])
            }
            "net.unix-socket" => {
                let mut c = ev(EventKind::Connect, 10);
                c.a0 = 1;
                c.peer = Some("@x".into());
                run(vec![c])
            }
            "secret.read" => {
                let mut o = ev(EventKind::Open, 10);
                o.path = "/home/box/.ssh/id_rsa".into();
                run(vec![o])
            }
            "secret.dotenv" => {
                let mut o = ev(EventKind::Open, 10);
                o.path = "/elsewhere/.env".into();
                run(vec![o])
            }
            "secret.proc-environ" => {
                let mut o = ev(EventKind::Open, 10);
                o.path = "/proc/4242/environ".into();
                run(vec![o])
            }
            "secret.h5i-state" => {
                let mut o = ev(EventKind::Open, 10);
                o.path = "/work/.h5i/manifest.json".into();
                o.a1 = 1;
                run(vec![o])
            }
            "exec.from-tmp" => {
                let mut e = ev(EventKind::Exec, 10);
                e.path = "/tmp/x".into();
                run(vec![e])
            }
            "exec.memfd" => {
                let mut e = ev(EventKind::Exec, 10);
                e.path = "/proc/self/fd/3".into();
                run(vec![ev(EventKind::Memfd, 10), e])
            }
            "exec.interpreter-pipe" => {
                let mut e = ev(EventKind::Exec, 10);
                e.path = "/bin/bash".into();
                e.aux = "-c".into();
                e.aux2 = "wget -qO- http://x | bash".into();
                run(vec![e])
            }
            "exec.package-manager" => {
                let mut e = ev(EventKind::Exec, 10);
                e.path = "/usr/bin/cargo".into();
                run(vec![e])
            }
            "priv.ptrace" => {
                let mut p = ev(EventKind::Ptrace, 10);
                p.a0 = 16;
                p.a1 = 9999;
                run(vec![p])
            }
            "priv.namespace" => run(vec![ev(EventKind::Nsop, 10)]),
            "kernel.bpf" => run(vec![ev(EventKind::Bpf, 10)]),
            "kernel.module" => run(vec![ev(EventKind::Module, 10)]),
            "mount.change" => run(vec![ev(EventKind::Mount, 10)]),
            _ => Vec::new(),
        };
        fired(&d, id)
    }

    #[test]
    fn rule_ids_are_unique_and_family_prefixed() {
        let mut seen = HashSet::new();
        for r in RULES {
            assert!(seen.insert(r.id), "duplicate rule id {}", r.id);
            assert!(
                r.id.starts_with(&format!("{}.", r.family)),
                "{} is not prefixed by its family {}",
                r.id,
                r.family
            );
            assert!(!r.title.is_empty() && !r.detail.is_empty(), "{}", r.id);
        }
    }

    /// The Rust constants and the C header describe one wire format. If they
    /// drift, everything above this line is reasoning about the wrong bytes.
    #[test]
    fn the_rust_constants_match_the_c_header() {
        let header = include_str!("../bpf/h5i_event.h");
        let define = |name: &str| -> Option<i64> {
            header.lines().find_map(|l| {
                let rest = l.strip_prefix(&format!("#define {name} "))?;
                let tok = rest.split_whitespace().next()?;
                let tok = tok.trim_end_matches('u');
                if let Some(hex) = tok.strip_prefix("0x") {
                    i64::from_str_radix(hex, 16).ok()
                } else {
                    tok.parse::<i64>().ok()
                }
            })
        };
        assert_eq!(define("H5I_EVENT_MAGIC"), Some(EVENT_MAGIC as i64));
        assert_eq!(define("H5I_EVENT_VERSION"), Some(EVENT_VERSION as i64));
        assert_eq!(define("H5I_PATH_LEN"), Some(PATH_LEN as i64));
        assert_eq!(define("H5I_AUX_LEN"), Some(AUX_LEN as i64));
        assert_eq!(define("H5I_AUX_HALF"), Some(AUX_HALF as i64));
        assert_eq!(define("H5I_MAX_PREFIX"), Some(crate::event::MAX_PREFIX as i64));
        assert_eq!(define("H5I_PREFIX_LEN"), Some(crate::event::PREFIX_LEN as i64));
        assert_eq!(define("H5I_COMM_LEN"), Some(crate::event::COMM_LEN as i64));
        assert_eq!(std::mem::size_of::<RawEvent>(), 520);

        // Every kind's wire value, read out of the header rather than trusted.
        for k in EventKind::ALL {
            let name = format!("H5I_KIND_{}", k.as_str().to_uppercase());
            assert_eq!(
                define(&name),
                Some(k.to_wire() as i64),
                "{name} disagrees with the Rust enum"
            );
        }
    }
}
