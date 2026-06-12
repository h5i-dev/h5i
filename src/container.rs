//! The `isolation=container` backend: run an environment's command inside a
//! **rootless Podman** container, and — uniquely —
//! enforce a `net.egress` **domain allowlist** that the static `process` tier
//! cannot (docs/environments-design.md §5–§7, rollout phase 4).
//!
//! Hardening (EscapeBench footguns, §2): `--rm`, `--cap-drop=ALL`,
//! `--security-opt=no-new-privileges`, a read-only rootfs with a private
//! `/tmp` tmpfs, `--userns=keep-id` so files in the bind-mounted workspace keep
//! the caller's ownership, memory/pid limits, an env-var allowlist, and **never**
//! a Docker socket mount. The container is an *opt-in adapter* that shells out
//! to Podman if the user already has it — it adds no Rust dependency. Docker is
//! intentionally not accepted in this phase: its daemon/socket model has a
//! different trust boundary and is easy to misconfigure for agent workloads.
//!
//! ### Egress enforcement — honestly scoped
//!
//! When `net.egress` is non-empty, h5i resolves+pins the allowlisted domains to
//! IPs at startup (kills DNS-rebinding) and runs a small **HTTP/HTTPS CONNECT
//! allowlist proxy** on the host; the container is pointed at it via the
//! `HTTP(S)_PROXY` env vars. This is **L7** enforcement: it blocks the dominant
//! exfiltration path — `curl`/`wget`/`pip`/`npm`/`requests` to a non-allowlisted
//! host — fail-closed (anything not on the list gets `403`). It does *not* by
//! itself stop a process that ignores the proxy env and opens a raw connection
//! to an arbitrary IP the rootless NAT permits; airtight L3/L4 egress filtering
//! is the `hardened-container`/`microvm` tier (or the future seccomp-notify
//! supervisor). We state this rather than pretend the box is sealed.

use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::error::H5iError;
use crate::objects::{EgressHost, EgressSummary, MAX_EGRESS_HOSTS};
use crate::sandbox::{ExecOutcome, NetMode, Profile, ResolvedPolicy};

/// Per `host:port` allow/deny tally accumulated by the egress proxy across a
/// run. Shared between the proxy's worker threads and the run, behind a mutex;
/// snapshotted into a bounded [`EgressSummary`] once the container exits.
#[derive(Default)]
struct EgressTally {
    allowed: u64,
    denied: u64,
    /// `(host, port) -> (allowed, denied)`. `BTreeMap` keeps the snapshot
    /// deterministic (sorted) for stable manifests and tests.
    hosts: BTreeMap<(String, u16), (u64, u64)>,
}

impl EgressTally {
    fn record(&mut self, host: &str, port: u16, permitted: bool) {
        let slot = self.hosts.entry((host.to_string(), port)).or_insert((0, 0));
        if permitted {
            self.allowed += 1;
            slot.0 += 1;
        } else {
            self.denied += 1;
            slot.1 += 1;
        }
    }

    /// Build the bounded summary the capture manifest carries. Denied hosts
    /// (boundary trips) are surfaced first so the clamp never drops the signal
    /// that matters most.
    fn snapshot(&self) -> EgressSummary {
        let mut hosts: Vec<EgressHost> = self
            .hosts
            .iter()
            .map(|((host, port), (allowed, denied))| EgressHost {
                host: host.clone(),
                port: *port,
                allowed: *allowed,
                denied: *denied,
            })
            .collect();
        // Denials first, then by descending traffic — most interesting on top.
        hosts.sort_by(|a, b| {
            (b.denied > 0)
                .cmp(&(a.denied > 0))
                .then((b.allowed + b.denied).cmp(&(a.allowed + a.denied)))
                .then(a.host.cmp(&b.host))
        });
        let hosts_truncated = hosts.len() > MAX_EGRESS_HOSTS;
        hosts.truncate(MAX_EGRESS_HOSTS);
        EgressSummary {
            allowed: self.allowed,
            denied: self.denied,
            hosts,
            hosts_truncated,
            log: None,
        }
    }
}

/// A detected container runtime.
#[derive(Debug, Clone)]
pub struct Runtime {
    /// The binary to invoke (`podman` in this build).
    pub bin: String,
    /// True for rootless Podman. Always true for a runtime returned by
    /// [`probe`], but retained so argv construction stays explicit/testable.
    pub rootless: bool,
}

/// Detect the only container runtime this phase supports: **rootless Podman**.
/// Returns `None` when Podman is absent, broken, or running as root. Cheap:
/// only runs `--version` and one `podman info` field read.
pub fn probe() -> Option<Runtime> {
    if !version_ok("podman") {
        return None;
    }
    if podman_rootless()? {
        Some(Runtime {
            bin: "podman".into(),
            rootless: true,
        })
    } else {
        None
    }
}

fn version_ok(bin: &str) -> bool {
    std::process::Command::new(bin)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn podman_rootless() -> Option<bool> {
    let out = std::process::Command::new("podman")
        .args(["info", "--format", "{{.Host.Security.Rootless}}"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim() == "true")
}

// ─── egress allowlist ────────────────────────────────────────────────────────

/// One parsed `net.egress` entry, e.g. `pypi.org`, `github.com:443`,
/// `.githubusercontent.com` (subdomain wildcard).
#[derive(Debug, Clone, PartialEq, Eq)]
struct AllowEntry {
    /// Lower-cased host (without the leading dot for wildcards).
    host: String,
    /// True for `.suffix` / `*.suffix` subdomain matches.
    wildcard: bool,
    /// Restrict to a single port when present; `None` = any port.
    port: Option<u16>,
}

/// A resolved egress allowlist: parsed host rules plus the set of IPs the
/// allowed domains pinned to at startup (so a client connecting by a pinned IP
/// is permitted, and the proxy is DNS-rebinding resistant).
#[derive(Debug, Clone, Default)]
pub struct AllowList {
    entries: Vec<AllowEntry>,
    pinned_ips: HashSet<IpAddr>,
}

impl AllowList {
    /// Parse `net.egress` entries (no DNS yet — pure, for tests).
    pub fn parse(egress: &[String]) -> AllowList {
        let mut entries = Vec::new();
        for raw in egress {
            let raw = raw.trim();
            if raw.is_empty() {
                continue;
            }
            let (host_part, port) = match raw.rsplit_once(':') {
                // Only treat the suffix as a port if it's numeric (IPv6 has colons).
                Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => {
                    (h, p.parse::<u16>().ok())
                }
                _ => (raw, None),
            };
            let lower = host_part.to_ascii_lowercase();
            let (host, wildcard) = if let Some(s) = lower.strip_prefix("*.") {
                (s.to_string(), true)
            } else if let Some(s) = lower.strip_prefix('.') {
                (s.to_string(), true)
            } else {
                (lower, false)
            };
            if !host.is_empty() {
                entries.push(AllowEntry {
                    host,
                    wildcard,
                    port,
                });
            }
        }
        AllowList {
            entries,
            pinned_ips: HashSet::new(),
        }
    }

    /// Resolve every allowed host to IPs and pin them. Best-effort: a host that
    /// fails to resolve simply contributes no pinned IPs (it can still match by
    /// name at CONNECT time). Returns the count pinned.
    pub fn pin_dns(&mut self) -> usize {
        let mut pinned = HashSet::new();
        for e in &self.entries {
            if e.wildcard {
                continue; // can't enumerate a wildcard's IPs
            }
            let port = e.port.unwrap_or(443);
            if let Ok(addrs) = (e.host.as_str(), port).to_socket_addrs() {
                for a in addrs {
                    pinned.insert(a.ip());
                }
            }
        }
        let n = pinned.len();
        self.pinned_ips = pinned;
        n
    }

    /// Decide whether a CONNECT/request to `host:port` is allowed (fail-closed:
    /// the empty allowlist permits nothing).
    pub fn allows(&self, host: &str, port: u16) -> bool {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        // Direct connection to a pinned IP of an allowed host.
        if let Ok(ip) = host.parse::<IpAddr>() {
            if self.pinned_ips.contains(&ip) {
                return true;
            }
        }
        for e in &self.entries {
            if let Some(p) = e.port {
                if p != port {
                    continue;
                }
            }
            let name_ok = if e.wildcard {
                host == e.host || host.ends_with(&format!(".{}", e.host))
            } else {
                host == e.host
            };
            if name_ok {
                return true;
            }
        }
        false
    }
}

/// A running egress proxy: a localhost TCP listener gating CONNECT/HTTP by the
/// allowlist. Dropping the handle shuts the accept loop down.
pub struct ProxyHandle {
    pub port: u16,
    stop: Arc<AtomicBool>,
    tally: Arc<Mutex<EgressTally>>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl ProxyHandle {
    /// Snapshot the egress verdicts seen so far into a bounded summary. Called
    /// after the container exits; a poisoned lock degrades to an empty summary
    /// rather than panicking the run.
    pub fn egress_summary(&self) -> EgressSummary {
        match self.tally.lock() {
            Ok(t) => t.snapshot(),
            Err(_) => EgressSummary {
                allowed: 0,
                denied: 0,
                hosts: Vec::new(),
                hosts_truncated: false,
                log: None,
            },
        }
    }
}

impl Drop for ProxyHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Unblock the accept poll promptly.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Spawn the allowlist proxy bound to loopback. `allow` should already be
/// DNS-pinned. The proxy speaks just enough HTTP to gate egress: `CONNECT`
/// tunnels (HTTPS) and absolute-form requests (plain HTTP).
pub fn spawn_proxy(allow: AllowList) -> Result<ProxyHandle, H5iError> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(H5iError::Io)?;
    let port = listener.local_addr().map_err(H5iError::Io)?.port();
    listener.set_nonblocking(true).map_err(H5iError::Io)?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let allow = Arc::new(allow);
    let tally = Arc::new(Mutex::new(EgressTally::default()));
    let tally_thread = tally.clone();

    let join = std::thread::spawn(move || {
        while !stop_thread.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((client, _)) => {
                    if stop_thread.load(Ordering::SeqCst) {
                        break;
                    }
                    let allow = allow.clone();
                    let tally = tally_thread.clone();
                    std::thread::spawn(move || {
                        let _ = handle_proxy_client(client, &allow, &tally);
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(_) => break,
            }
        }
    });

    Ok(ProxyHandle {
        port,
        stop,
        tally,
        join: Some(join),
    })
}

/// Read the request head (up to the blank line) from `s`.
fn read_head(s: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    loop {
        let n = s.read(&mut byte)?;
        if n == 0 {
            break;
        }
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") || buf.len() > 16 * 1024 {
            break;
        }
    }
    Ok(buf)
}

/// Host:port target from a CONNECT line or an absolute-form request line.
/// Returns `(host, port, is_connect)`.
fn parse_target(head: &[u8]) -> Option<(String, u16, bool)> {
    let text = String::from_utf8_lossy(head);
    let first = text.lines().next()?;
    let mut parts = first.split_whitespace();
    let method = parts.next()?;
    let target = parts.next()?;
    if method.eq_ignore_ascii_case("CONNECT") {
        let (h, p) = target.rsplit_once(':')?;
        return Some((h.to_string(), p.parse().ok()?, true));
    }
    // Absolute-form: GET http://host[:port]/path
    let (rest, default_port) = if let Some(rest) = target.strip_prefix("http://") {
        (rest, 80)
    } else {
        // `?` returns None for any non-http(s) absolute target.
        (target.strip_prefix("https://")?, 443)
    };
    let authority = rest.split('/').next().unwrap_or(rest);
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => (h.to_string(), p.parse().ok()?),
        _ => (authority.to_string(), default_port),
    };
    Some((host, port, false))
}

fn handle_proxy_client(
    mut client: TcpStream,
    allow: &AllowList,
    tally: &Arc<Mutex<EgressTally>>,
) -> std::io::Result<()> {
    client.set_read_timeout(Some(Duration::from_secs(30)))?;
    let head = read_head(&mut client)?;
    let Some((host, port, is_connect)) = parse_target(&head) else {
        // A malformed/empty request (incl. the shutdown probe) records no
        // verdict — only real CONNECT/HTTP targets count toward egress.
        let _ = client.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n");
        return Ok(());
    };
    let permitted = allow.allows(&host, port);
    if let Ok(mut t) = tally.lock() {
        t.record(&host, port, permitted);
    }
    if !permitted {
        let _ = client.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n");
        return Ok(());
    }
    let mut upstream = match TcpStream::connect((host.as_str(), port)) {
        Ok(s) => s,
        Err(_) => {
            let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n");
            return Ok(());
        }
    };
    if is_connect {
        client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
    } else {
        // Replay the original request head to the origin server.
        upstream.write_all(&head)?;
    }
    splice(client, upstream);
    Ok(())
}

/// Bidirectionally copy until either side closes.
fn splice(a: TcpStream, b: TcpStream) {
    let (mut a1, mut b1) = (a, b);
    let a2 = a1.try_clone();
    let b2 = b1.try_clone();
    if let (Ok(mut a2), Ok(mut b2)) = (a2, b2) {
        let t = std::thread::spawn(move || {
            let _ = std::io::copy(&mut a2, &mut b2);
            let _ = b2.shutdown(std::net::Shutdown::Write);
        });
        let _ = std::io::copy(&mut b1, &mut a1);
        let _ = a1.shutdown(std::net::Shutdown::Write);
        let _ = t.join();
    }
}

// ─── run argv construction (pure; unit-tested) ───────────────────────────────

/// How networking is wired for a container run.
pub enum NetPlan {
    /// No network at all (`net.mode = deny`, no egress allowlist).
    None,
    /// Full rootless egress (`net.mode = host`).
    Host,
    /// Egress only via the host allowlist proxy on `port`.
    Proxy(u16),
}

/// The container-tier observation shim (`env shell`): the host paths of the
/// generated shim script and the spool directory it tees into. Both are
/// mounted into the box by [`build_run_argv`]; `env shell` ingests the spool
/// into tagged captures at session end.
pub struct ShimPlan {
    /// Host path of the generated POSIX shim script (mounted over the image's
    /// `/bin/sh` and `/bin/bash`).
    pub shim: std::path::PathBuf,
    /// Host spool directory (mounted rw at `/.h5i/spool`). Everything the box
    /// writes here is **untrusted** — the ingest side caps and sanitizes.
    pub spool: std::path::PathBuf,
}

/// In-container mountpoints for the shim machinery. `ORIG` is the container's
/// own image self-mounted read-only, so the *unmodified* shell stays reachable
/// at `/.h5i/orig/bin/sh` for any image — the shim is a `#!/.h5i/orig/bin/sh`
/// script that uses only the image's own interpreter and utilities (no host
/// binary enters the box, so there is no glibc/arch compatibility surface).
const SHIM_ORIG_MOUNT: &str = "/.h5i/orig";
const SHIM_SPOOL_MOUNT: &str = "/.h5i/spool";

/// Generate the observation shim: a pass-through POSIX `sh` script that tees
/// stdout/stderr of a non-interactive command-string invocation into the spool
/// while preserving argv, stdin, the TTY decision, and the exit code.
///
/// The command flag is detected by **scanning argv for a short-option cluster
/// ending in `c`** (`-c`, `-lc`, `-ic`, `-lic`, …) rather than checking `$1`
/// literally — different runtimes spell it differently: Claude Code runs
/// `bash -c`, Codex runs `bash -lc`. The command string is the argument that
/// follows that flag. A leading non-`c` option (e.g. `bash -i -c …`) is skipped
/// over; scanning stops at `--` or the first non-option (a `sh script.sh`
/// invocation, which is not observed).
///
/// Every guard fails OPEN to `exec` of the real shell — observation must never
/// change what a command does or whether it runs. Pure; unit + live tested.
pub fn shim_script(orig_prefix: &str, spool_dir: &str) -> String {
    format!(
        r#"#!{orig}/bin/sh
# h5i observation shim (env shell, container tier). Pass-through tee:
# real shell + image utilities only; every failure path execs the real shell.
case "$0" in
  /*) real="{orig}$0" ;;
  *)  real="{orig}/bin/${{0##*/}}" ;;
esac
[ -x "$real" ] || real="{orig}/bin/sh"
# Locate a `-c` short option (possibly clustered: -c, -lc, -ic) and the command
# string that follows it. Stop at `--` or the first non-option (a script run,
# which we do not observe).
want= ; cmd= ; found=
for a in "$@"; do
  if [ -n "$want" ]; then cmd=$a ; found=1 ; break ; fi
  case "$a" in
    --) break ;;
    -*c) want=1 ;;
    -*) ;;
    *) break ;;
  esac
done
# Observe only top-level, non-interactive command invocations: a TTY session, a
# script run, or a nested shell under an already-observed command pass through.
if [ -z "$found" ] || [ -n "$H5I_SHIM" ] || [ -t 1 ]; then
  exec "$real" "$@"
fi
H5I_SHIM=1
export H5I_SHIM
# h5i's own invocations are already captured by h5i itself: the wrap-bash hook
# rewrites the agent's command to `h5i capture run -- <cmd>`, which captures it.
# Recording it here would double-capture (and tee h5i's own summary output).
# Pass through WITHOUT recording; H5I_SHIM (set above) also keeps any sub-shell
# h5i spawns unrecorded. Mirrors the hook's own `first_base == "h5i"` skip.
case "$cmd" in
  h5i|h5i\ *) exec "$real" "$@" ;;
esac
d="{spool}"
if [ ! -d "$d" ] || ! command -v tee >/dev/null 2>&1 || ! command -v mkfifo >/dev/null 2>&1; then
  exec "$real" "$@"
fi
n=0
while [ -e "$d/cmd-$$-$n.cmd" ] && [ "$n" -lt 1000 ]; do n=$((n+1)); done
b="$d/cmd-$$-$n"
{{ printf '%s' "$cmd" > "$b.cmd"; }} 2>/dev/null || exec "$real" "$@"
mkfifo "$b.po" "$b.pe" 2>/dev/null || {{ rm -f "$b.cmd"; exec "$real" "$@"; }}
tee "$b.out" < "$b.po" &
po=$!
tee "$b.err" < "$b.pe" >&2 &
pe=$!
"$real" "$@" > "$b.po" 2> "$b.pe"
rc=$?
wait "$po" "$pe" 2>/dev/null
rm -f "$b.po" "$b.pe"
printf '%s' "$rc" > "$b.exit" 2>/dev/null
exit "$rc"
"#,
        orig = orig_prefix,
        spool = spool_dir,
    )
}

/// Prepare the shim for an interactive session: write the script + spool dir
/// under the env directory (`work/..`). Returns `None` — observation disabled,
/// session unaffected — on any failure, including paths Podman's `--mount`
/// syntax can't carry safely.
fn prepare_shim(work: &Path) -> Option<ShimPlan> {
    let env_dir = work.parent()?;
    let spool = env_dir.join("spool");
    let shim_dir = env_dir.join("shim");
    std::fs::create_dir_all(&spool).ok()?;
    std::fs::create_dir_all(&shim_dir).ok()?;
    let shim = shim_dir.join("sh");
    std::fs::write(&shim, shim_script(SHIM_ORIG_MOUNT, SHIM_SPOOL_MOUNT)).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).ok()?;
    }
    if shim.display().to_string().contains(',') || spool.display().to_string().contains(',') {
        return None;
    }
    Some(ShimPlan { shim, spool })
}

/// Write the managed-settings.json (carrying the unkillable wrap-bash
/// observation hook) under the env dir, to be bind-mounted read-only into the
/// box at [`crate::hooks::CLAUDE_MANAGED_SETTINGS_PATH`]. Best-effort: returns
/// `None` (injection skipped, session otherwise unaffected) on any I/O failure
/// or a path Podman's `--mount` syntax can't carry. Only the in-box Claude
/// reads this file; it is inert for any other tooling.
fn prepare_managed_settings(work: &Path) -> Option<PathBuf> {
    let env_dir = work.parent()?;
    let dir = env_dir.join("managed");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("managed-settings.json");
    std::fs::write(&path, crate::hooks::managed_settings_wrap_bash_json()).ok()?;
    if path.display().to_string().contains(',') {
        return None;
    }
    Some(path)
}

/// Build the `podman run` argv for `argv` under `policy`, fully
/// hardened. `image` is the resolved base image; `name` is the (unique)
/// container name used for cleanup. Pure — no process is spawned, so this is
/// unit-tested for the security-critical flag set.
#[allow(clippy::too_many_arguments)] // a pure argv builder; a params struct would obscure more than it helps
pub fn build_run_argv(
    rt: &Runtime,
    profile: &Profile,
    work: &Path,
    image: &str,
    name: &str,
    net: &NetPlan,
    argv: &[String],
    injected_env: &[(String, String)],
    // `None` → capture run (no stdin). `Some(tty)` → interactive: keep stdin open
    // (`-i`), allocating a pseudo-TTY (`-t`) when the caller has one — the
    // agent-in-box shell. Flags slot right after `run`, before the image.
    tty: Option<bool>,
    // Interactive observation shim (`env shell`): self-mount the image at
    // `/.h5i/orig`, shadow `/bin/sh` + `/bin/bash` with the tee shim, and mount
    // the spool rw. `None` → no observation (captured runs, or shim prep failed).
    shim: Option<&ShimPlan>,
    // In-box git plumbing (env::box_git_plumbing): each host path bind-mounted
    // at its IDENTICAL path inside the container so the worktree's
    // gitdir/commondir pointer files resolve. Empty → no extra mounts.
    box_git: &[crate::sandbox::BoxGitPath],
    // Managed-settings.json (the unkillable wrap-bash hook) to bind-mount
    // read-only at the Claude managed-settings path. `None` → no injection
    // (non-Claude box, captured run, or prep failed).
    managed_settings: Option<&Path>,
) -> Vec<String> {
    let mut a: Vec<String> = vec![
        rt.bin.clone(),
        "run".into(),
        "--rm".into(),
        "--pull=never".into(),
        "--name".into(),
        name.into(),
        // EscapeBench hardening: no ambient capabilities, no privilege gain,
        // read-only rootfs with a private writable /tmp, no host PID/IPC share.
        "--cap-drop=ALL".into(),
        "--security-opt=no-new-privileges".into(),
        "--read-only".into(),
        "--tmpfs".into(),
        "/tmp:rw,nosuid,nodev,size=256m".into(),
        // The env workspace, mounted at /work (the in-box git plumbing below
        // adds the only other writable host paths — the env's own .git
        // surface). Use --mount rather than -v so ':' in a repository path
        // cannot be parsed as a bind-mount option suffix by Podman.
        "--mount".into(),
        format!("type=bind,source={},target=/work,rw", work.display()),
        "-w".into(),
        "/work".into(),
        "--ipc=private".into(),
    ];
    for rel in [".claude/settings.json", ".codex/config.toml"] {
        let source = work.join(rel);
        if source.exists() && !source.display().to_string().contains(',') {
            a.push("--mount".into());
            a.push(format!(
                "type=bind,source={},target=/work/{rel},ro",
                source.display()
            ));
        }
    }
    // In-box git plumbing: every path mounted at its identical host path (the
    // worktree's pointer files contain host-absolute paths). The list arrives
    // parent-before-child (`refs` ro before its nested rw children) and is
    // emitted in order — mount targets that don't exist in the image are
    // auto-created on the rootfs overlay, same as the shim mounts below.
    // Podman's `--mount` syntax cannot carry a comma in a path: in that case
    // skip the WHOLE set (a partially mounted .git is worse than the old
    // fail-closed "not a git repository").
    if !box_git.is_empty()
        && !box_git
            .iter()
            .any(|b| b.host.display().to_string().contains(','))
    {
        for b in box_git {
            a.push("--mount".into());
            a.push(format!(
                "type=bind,source={p},target={p},{mode}",
                p = b.host.display(),
                mode = if b.rw { "rw" } else { "ro" },
            ));
        }
    }
    // Managed-settings injection: bind our wrap-bash hook read-only at Claude's
    // managed-settings path. The in-box agent cannot write the (root-owned)
    // path and cannot disable a managed hook from its own config, so in-box
    // observation cannot be silenced. Podman auto-creates the nested target on
    // the read-only rootfs overlay; the mount lives only in this box's ns.
    if let Some(ms) = managed_settings {
        if !ms.display().to_string().contains(',') {
            a.push("--mount".into());
            a.push(format!(
                "type=bind,source={},target={},ro",
                ms.display(),
                crate::hooks::CLAUDE_MANAGED_SETTINGS_PATH
            ));
        }
    }
    // Interactive (agent-in-box) flags, right after `run`.
    if let Some(want_tty) = tty {
        a.insert(2, "-i".into());
        if want_tty {
            a.insert(3, "-t".into());
        }
    }
    // Observation shim (interactive only): the image self-mount keeps the real
    // shell reachable at /.h5i/orig for ANY image; the shim shadows /bin/sh and
    // /bin/bash (on merged-usr images the bind lands on the resolved
    // /usr/bin/* file, covering both spellings); the spool is the only extra
    // writable surface, and its contents are treated as untrusted on ingest.
    if let Some(s) = shim {
        a.push("--mount".into());
        a.push(format!(
            "type=image,source={image},destination={SHIM_ORIG_MOUNT}"
        ));
        for target in ["/bin/sh", "/bin/bash"] {
            a.push("--mount".into());
            a.push(format!(
                "type=bind,source={},target={target},ro",
                s.shim.display()
            ));
        }
        a.push("--mount".into());
        a.push(format!(
            "type=bind,source={},target={SHIM_SPOOL_MOUNT},rw",
            s.spool.display()
        ));
    }
    // Rootless podman: keep the caller's uid so files in /work stay owned by us.
    if rt.rootless {
        a.push("--userns=keep-id".into());
    }
    if let Some(bytes) = profile.mem_bytes {
        a.push("--memory".into());
        a.push(bytes.to_string());
    }
    if let Some(n) = profile.max_procs {
        a.push("--pids-limit".into());
        a.push(n.to_string());
    }

    // Network.
    match net {
        NetPlan::None => {
            a.push("--network=none".into());
        }
        NetPlan::Host => {
            // Default rootless network (slirp4netns/pasta) gives NAT'd egress.
        }
        NetPlan::Proxy(port) => {
            // slirp4netns with allow_host_loopback exposes the host's loopback
            // (where our proxy listens) at the gateway address 10.0.2.2 — NOT at
            // `host.containers.internal`, which maps to a different gateway IP
            // that does not forward to host loopback.
            a.push("--network=slirp4netns:allow_host_loopback=true".into());
            let proxy = format!("http://10.0.2.2:{port}");
            for var in [
                "HTTP_PROXY",
                "HTTPS_PROXY",
                "http_proxy",
                "https_proxy",
                "ALL_PROXY",
            ] {
                a.push("--env".into());
                a.push(format!("{var}={proxy}"));
            }
            a.push("--env".into());
            a.push("NO_PROXY=localhost,127.0.0.1".into());
        }
    }

    // Env-var allowlist (nothing inherited wholesale). Pass by NAME only:
    // Podman forwards each value from its own (h5i's) environment, so the
    // value never lands in the container's argv — which is world-readable on a
    // default host via `/proc/<podman-pid>/cmdline`. (`--env KEY=VALUE` would
    // leak it there.)
    for key in &profile.env_pass {
        if std::env::var_os(key).is_some() {
            a.push("--env".into());
            a.push(key.clone());
        }
    }

    // Brokered secrets (env-injected), applied after the allowlist. Also passed
    // by NAME only — the caller seeds the value into Podman's environment (see
    // `run`/`run_interactive`), keeping the credential out of the process argv.
    for (key, _value) in injected_env {
        a.push("--env".into());
        a.push(key.clone());
    }

    a.push(image.to_string());
    a.extend(argv.iter().cloned());
    a
}

// ─── run ─────────────────────────────────────────────────────────────────────

/// Run `argv` for `policy` inside a hardened rootless container. Spawns the
/// egress proxy when `net.egress` is non-empty, enforces the wall clock (and
/// force-removes the container on timeout), and returns the captured output.
pub fn run(
    policy: &ResolvedPolicy,
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
) -> Result<ExecOutcome, H5iError> {
    let p = &policy.profile;
    let rt = probe().ok_or_else(|| {
        H5iError::Metadata(
            "isolation=container requires rootless Podman on PATH; Docker and rootful Podman are \
             intentionally not accepted in this Linux/WSL backend — install/configure rootless \
             podman or re-request --isolation workspace/process"
                .into(),
        )
    })?;
    let image = p.image.clone().ok_or_else(|| {
        H5iError::Metadata(format!(
            "profile '{}' uses isolation=container but sets no image — add `container.image = \
             \"…\"` (e.g. a toolchain image) to the profile",
            p.name
        ))
    })?;
    if work.display().to_string().contains(',') {
        return Err(H5iError::Metadata(format!(
            "container workspace path contains ',' and cannot be represented safely in Podman's \
             --mount syntax: {}",
            work.display()
        )));
    }

    // Networking + optional egress proxy (held for the container's lifetime).
    let mut _proxy: Option<ProxyHandle> = None;
    let net = if !p.net_egress.is_empty() {
        let mut allow = AllowList::parse(&p.net_egress);
        allow.pin_dns();
        let handle = spawn_proxy(allow)?;
        let port = handle.port;
        _proxy = Some(handle);
        NetPlan::Proxy(port)
    } else if p.net_mode == NetMode::Host {
        NetPlan::Host
    } else {
        NetPlan::None
    };

    // A unique, filesystem-safe container name for cleanup on timeout.
    let name = format!(
        "h5i-{}-{}",
        std::process::id(),
        PROBE_SEQ.fetch_add(1, Ordering::Relaxed)
    );
    let full = build_run_argv(
        &rt,
        p,
        work,
        &image,
        &name,
        &net,
        argv,
        injected_env,
        None,
        None,
        &policy.box_git,
        None,
    );

    let started = std::time::Instant::now();
    let mut cmd = std::process::Command::new(&full[0]);
    cmd.args(&full[1..]);
    // Seed brokered secret values into Podman's environment so the `--env NAME`
    // flags forward them into the container without ever exposing the value in
    // argv. Podman injects only the named vars; the rest of its env stays put.
    for (k, v) in injected_env {
        cmd.env(k, v);
    }
    let outcome = wait_container(cmd, &rt.bin, &name, p.wall(), &full)?;
    // Snapshot the proxy's allow/deny verdicts (only present in the Proxy plan)
    // before the handle drops and the listener shuts down.
    let egress = _proxy.as_ref().map(|h| h.egress_summary());
    Ok(ExecOutcome {
        wall_ms: started.elapsed().as_millis(),
        egress,
        ..outcome
    })
}

/// The **agent-in-box** path: run `argv` (a shell or a coding agent) inside the
/// hardened rootless container with stdio **inherited** — a real interactive
/// session whose every command is confined by the box (cap-drop, read-only
/// rootfs, the `net.egress` allowlist). Unlike [`run`] it captures nothing and
/// applies no wall-clock (the operator owns the session); it returns the child's
/// exit code. The egress proxy is held for the whole session.
pub fn run_interactive(
    policy: &ResolvedPolicy,
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
) -> Result<i32, H5iError> {
    use std::io::IsTerminal;
    let p = &policy.profile;
    let rt = probe().ok_or_else(|| {
        H5iError::Metadata(
            "isolation=container requires rootless Podman on PATH — install/configure rootless \
             podman or re-request --isolation workspace/process"
                .into(),
        )
    })?;
    let image = p.image.clone().ok_or_else(|| {
        H5iError::Metadata(format!(
            "profile '{}' uses isolation=container but sets no image — add `container.image`",
            p.name
        ))
    })?;
    if work.display().to_string().contains(',') {
        return Err(H5iError::Metadata(
            "container workspace path contains ',' — unsafe in Podman --mount syntax".into(),
        ));
    }

    let mut _proxy: Option<ProxyHandle> = None;
    let net = if !p.net_egress.is_empty() {
        let mut allow = AllowList::parse(&p.net_egress);
        allow.pin_dns();
        let handle = spawn_proxy(allow)?;
        let port = handle.port;
        _proxy = Some(handle);
        NetPlan::Proxy(port)
    } else if p.net_mode == NetMode::Host {
        NetPlan::Host
    } else {
        NetPlan::None
    };

    // Allocate a TTY only when we actually have one on both ends (a piped/CI
    // invocation must not request `-t`, which Podman would reject).
    let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let name = format!(
        "h5i-{}-{}",
        std::process::id(),
        PROBE_SEQ.fetch_add(1, Ordering::Relaxed)
    );
    // Observation shim: best-effort. A session without it is fully functional —
    // it just produces no in-box command evidence.
    let shim = prepare_shim(work);
    if shim.is_none() {
        eprintln!("note: shell observation shim unavailable — session runs unobserved");
    }
    // Managed-settings injection (Claude only): pins the wrap-bash hook
    // unkillably in-box. Skip for a known-Codex profile — the file is Claude's
    // managed scope and inert for Codex (whose own hook hardening is separate);
    // for `default`/custom profiles we inject, since they may run Claude.
    let is_codex = crate::sandbox::AgentRuntime::from_profile_name(&p.name)
        == Some(crate::sandbox::AgentRuntime::Codex);
    let managed_settings = if is_codex { None } else { prepare_managed_settings(work) };
    let full = build_run_argv(
        &rt,
        p,
        work,
        &image,
        &name,
        &net,
        argv,
        injected_env,
        Some(tty),
        shim.as_ref(),
        &policy.box_git,
        managed_settings.as_deref(),
    );

    // Inherited stdio (the default) — this is the interactive session. Secret
    // values are seeded into Podman's environment (forwarded by the `--env NAME`
    // flags) rather than placed in argv, so they never appear in the host's
    // process list.
    let mut cmd = std::process::Command::new(&full[0]);
    cmd.args(&full[1..]);
    for (k, v) in injected_env {
        cmd.env(k, v);
    }
    let status = cmd
        .status()
        .map_err(|e| H5iError::Metadata(format!("failed to start container session: {e}")))?;
    Ok(status.code().unwrap_or(130))
}

static PROBE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Spawn the container client, stream output, and enforce the wall clock. On
/// timeout, force-remove the container (the client dying may not stop it) then
/// kill the client. Resource accounting (cpu/rss) is the container's, not the
/// client's, so we report wall time only.
fn wait_container(
    mut cmd: std::process::Command,
    bin: &str,
    name: &str,
    wall: Duration,
    full: &[String],
) -> Result<ExecOutcome, H5iError> {
    use std::process::Stdio;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| H5iError::Metadata(format!("failed to run `{}`: {e}", full.join(" "))))?;

    let mut out_pipe = child.stdout.take().expect("piped stdout");
    let mut err_pipe = child.stderr.take().expect("piped stderr");
    let out_h = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out_pipe.read_to_end(&mut b);
        b
    });
    let err_h = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = err_pipe.read_to_end(&mut b);
        b
    });

    let deadline = std::time::Instant::now() + wall;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait().map_err(H5iError::Io)? {
            Some(s) => break s,
            None => {
                if std::time::Instant::now() >= deadline {
                    timed_out = true;
                    // Stop the container itself, then the client.
                    let _ = std::process::Command::new(bin)
                        .args(["rm", "-f", name])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                    let _ = child.kill();
                    break child.wait().map_err(H5iError::Io)?;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    };

    Ok(ExecOutcome {
        stdout: out_h.join().unwrap_or_default(),
        stderr: err_h.join().unwrap_or_default(),
        exit_code: status.code(),
        timed_out,
        wall_ms: 0, // set by caller
        cpu_ms: 0,  // container-side accounting not collected here
        max_rss_kb: None,
        egress: None, // set by caller from the proxy tally
    })
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn rt() -> Runtime {
        Runtime {
            bin: "podman".into(),
            rootless: true,
        }
    }

    #[test]
    fn egress_tally_counts_and_orders_verdicts() {
        let mut t = EgressTally::default();
        t.record("pypi.org", 443, true);
        t.record("pypi.org", 443, true);
        t.record("evil.example", 443, false); // a boundary trip
        t.record("github.com", 443, true);

        let s = t.snapshot();
        assert_eq!(s.allowed, 3);
        assert_eq!(s.denied, 1);
        assert!(!s.hosts_truncated);
        // Denied host is surfaced first regardless of traffic volume.
        assert_eq!(s.hosts[0].host, "evil.example");
        assert_eq!(s.hosts[0].denied, 1);
        assert_eq!(s.hosts[0].allowed, 0);
        // pypi.org (2 allowed) outranks github.com (1 allowed) among permits.
        let pypi = s.hosts.iter().find(|h| h.host == "pypi.org").unwrap();
        assert_eq!(pypi.allowed, 2);
        assert_eq!(pypi.denied, 0);
    }

    #[test]
    fn egress_tally_clamps_to_max_hosts() {
        let mut t = EgressTally::default();
        for i in 0..(MAX_EGRESS_HOSTS + 10) {
            t.record(&format!("host{i}.example"), 443, false);
        }
        let s = t.snapshot();
        assert_eq!(s.denied as usize, MAX_EGRESS_HOSTS + 10, "all counted");
        assert_eq!(s.hosts.len(), MAX_EGRESS_HOSTS, "but host list is bounded");
        assert!(s.hosts_truncated);
    }

    #[test]
    fn allowlist_exact_wildcard_and_port() {
        let a = AllowList::parse(&[
            "pypi.org".into(),
            "github.com:443".into(),
            ".githubusercontent.com".into(),
            "*.pythonhosted.org".into(),
        ]);
        // Exact host, any port.
        assert!(a.allows("pypi.org", 443));
        assert!(a.allows("pypi.org", 80));
        // Port-restricted.
        assert!(a.allows("github.com", 443));
        assert!(
            !a.allows("github.com", 80),
            "port 80 not allowed for github.com:443"
        );
        // Subdomain wildcard (both . and *. forms).
        assert!(a.allows("raw.githubusercontent.com", 443));
        assert!(a.allows("files.pythonhosted.org", 443));
        assert!(
            a.allows("pythonhosted.org", 443),
            "apex matches the wildcard too"
        );
        // Not on the list → fail closed.
        assert!(!a.allows("evil.example.com", 443));
        assert!(!a.allows("notgithub.com", 443));
    }

    #[test]
    fn empty_allowlist_denies_everything() {
        let a = AllowList::parse(&[]);
        assert!(!a.allows("anything.com", 443));
    }

    #[test]
    fn allowlist_does_not_treat_ipv6_as_port() {
        // A bare IPv6-ish string must not be mis-split on its colons.
        let a = AllowList::parse(&["example.org".into()]);
        assert!(a.allows("example.org", 443));
    }

    #[test]
    fn parse_target_connect_and_absolute() {
        let (h, p, c) = parse_target(b"CONNECT pypi.org:443 HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!((h.as_str(), p, c), ("pypi.org", 443, true));
        let (h, p, c) =
            parse_target(b"GET http://example.com/x HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        assert_eq!((h.as_str(), p, c), ("example.com", 80, false));
        let (h, p, c) =
            parse_target(b"GET https://example.com/x HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        assert_eq!((h.as_str(), p, c), ("example.com", 443, false));
        let (h, p, _) = parse_target(b"GET http://example.com:8080/y HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!((h.as_str(), p), ("example.com", 8080));
    }

    #[test]
    fn run_argv_is_hardened() {
        let mut p = Profile::builtin("default", crate::sandbox::IsolationClaim::Container);
        p.mem_bytes = Some(2 * 1024 * 1024 * 1024);
        p.max_procs = Some(128);
        let argv = build_run_argv(
            &rt(),
            &p,
            Path::new("/work/dir"),
            "docker.io/library/debian:stable-slim",
            "h5i-test",
            &NetPlan::None,
            &["sh".into(), "-c".into(), "echo hi".into()],
            &[],
            None,
            None,
            &[],
            None,
        );
        let joined = argv.join(" ");
        assert_eq!(argv[0], "podman");
        assert!(joined.contains("--rm"));
        assert!(joined.contains("--pull=never"));
        assert!(joined.contains("--cap-drop=ALL"));
        assert!(joined.contains("--security-opt=no-new-privileges"));
        assert!(joined.contains("--read-only"));
        assert!(joined.contains("--network=none"));
        assert!(joined.contains("type=bind,source=/work/dir,target=/work,rw"));
        assert!(joined.contains("--ipc=private"));
        assert!(joined.contains("--userns=keep-id"));
        assert!(joined.contains("--memory 2147483648"));
        assert!(joined.contains("--pids-limit 128"));
        // No docker socket is ever mounted.
        assert!(!joined.contains("docker.sock"));
        // Image precedes the command.
        let img_idx = argv.iter().position(|x| x.contains("debian")).unwrap();
        let cmd_idx = argv.iter().position(|x| x == "echo hi").unwrap();
        assert!(img_idx < cmd_idx);
    }

    #[test]
    fn run_argv_mounts_agent_hook_configs_read_only() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path();
        std::fs::create_dir_all(work.join(".claude")).unwrap();
        std::fs::create_dir_all(work.join(".codex")).unwrap();
        std::fs::write(work.join(".claude/settings.json"), "{}").unwrap();
        std::fs::write(work.join(".codex/config.toml"), "").unwrap();

        let p = Profile::builtin("default", crate::sandbox::IsolationClaim::Container);
        let argv = build_run_argv(
            &rt(),
            &p,
            work,
            "img",
            "n",
            &NetPlan::None,
            &["true".into()],
            &[],
            None,
            None,
            &[],
            None,
        );
        let joined = argv.join(" ");
        assert!(joined.contains(&format!(
            "type=bind,source={},target=/work/.claude/settings.json,ro",
            work.join(".claude/settings.json").display()
        )));
        assert!(joined.contains(&format!(
            "type=bind,source={},target=/work/.codex/config.toml,ro",
            work.join(".codex/config.toml").display()
        )));
    }

    // In-box git plumbing mounts: identical source/target host paths, ro/rw
    // honored, list order preserved (nested binds need their parent first),
    // and a comma in ANY path skips the whole set (Podman's `--mount` syntax
    // cannot carry it; a partially mounted .git would be worse than none).
    #[test]
    fn run_argv_mounts_box_git_plumbing_at_identical_paths() {
        use crate::sandbox::BoxGitPath;
        let p = Profile::builtin("default", crate::sandbox::IsolationClaim::Container);
        let box_git = vec![
            BoxGitPath {
                host: "/repo/.git/refs".into(),
                rw: false,
            },
            BoxGitPath {
                host: "/repo/.git/objects".into(),
                rw: true,
            },
            BoxGitPath {
                host: "/repo/.git/refs/h5i/context".into(),
                rw: true,
            },
        ];
        let argv = build_run_argv(
            &rt(),
            &p,
            Path::new("/w"),
            "img",
            "n",
            &NetPlan::None,
            &["true".into()],
            &[],
            None,
            None,
            &box_git,
            None,
        );
        let joined = argv.join(" ");
        assert!(joined.contains("type=bind,source=/repo/.git/refs,target=/repo/.git/refs,ro"));
        assert!(joined.contains("type=bind,source=/repo/.git/objects,target=/repo/.git/objects,rw"));
        // Parent `refs` (ro) is mounted before the rw child nested under it.
        let parent = argv
            .iter()
            .position(|x| x.ends_with("target=/repo/.git/refs,ro"))
            .unwrap();
        let child = argv
            .iter()
            .position(|x| x.ends_with("target=/repo/.git/refs/h5i/context,rw"))
            .unwrap();
        assert!(parent < child, "parent mount must precede nested child");

        // A comma anywhere → the whole set is skipped, nothing partial.
        let weird = vec![
            BoxGitPath {
                host: "/repo/.git/objects".into(),
                rw: true,
            },
            BoxGitPath {
                host: "/re,po/.git/refs".into(),
                rw: false,
            },
        ];
        let argv = build_run_argv(
            &rt(),
            &p,
            Path::new("/w"),
            "img",
            "n",
            &NetPlan::None,
            &["true".into()],
            &[],
            None,
            None,
            &weird,
            None,
        );
        let joined = argv.join(" ");
        assert!(
            !joined.contains("target=/repo/.git/objects"),
            "comma path must disable the whole box-git mount set: {joined}"
        );
    }

    // Managed-settings injection: when present, a read-only bind mount lands at
    // the Claude managed-settings path; when absent, no such mount appears.
    #[test]
    fn run_argv_mounts_managed_settings_read_only() {
        let p = Profile::builtin("default", crate::sandbox::IsolationClaim::Container);
        let ms = Path::new("/env/managed/managed-settings.json");
        let with = build_run_argv(
            &rt(),
            &p,
            Path::new("/w"),
            "img",
            "n",
            &NetPlan::None,
            &["true".into()],
            &[],
            Some(true),
            None,
            &[],
            Some(ms),
        );
        let joined = with.join(" ");
        assert!(
            joined.contains(&format!(
                "type=bind,source=/env/managed/managed-settings.json,target={},ro",
                crate::hooks::CLAUDE_MANAGED_SETTINGS_PATH
            )),
            "expected ro managed-settings mount: {joined}"
        );

        // None → no managed-settings mount at all.
        let without = build_run_argv(
            &rt(),
            &p,
            Path::new("/w"),
            "img",
            "n",
            &NetPlan::None,
            &["true".into()],
            &[],
            Some(true),
            None,
            &[],
            None,
        );
        assert!(
            !without.join(" ").contains(crate::hooks::CLAUDE_MANAGED_SETTINGS_PATH),
            "no managed-settings mount when not requested"
        );
    }

    // The Codex agent profile is detected as Codex (so the Claude-specific
    // managed-settings injection is skipped for it); default/custom are not.
    #[test]
    fn codex_profile_is_detected_for_managed_settings_gating() {
        use crate::sandbox::AgentRuntime;
        assert_eq!(AgentRuntime::from_profile_name("agent-codex"), Some(AgentRuntime::Codex));
        assert_eq!(AgentRuntime::from_profile_name("agent-claude"), Some(AgentRuntime::Claude));
        assert_eq!(AgentRuntime::from_profile_name("default"), None);
    }

    #[test]
    fn run_argv_proxy_mode_sets_proxy_env() {
        let p = Profile::builtin("default", crate::sandbox::IsolationClaim::Container);
        let argv = build_run_argv(
            &rt(),
            &p,
            Path::new("/w"),
            "img",
            "n",
            &NetPlan::Proxy(8123),
            &["true".into()],
            &[],
            None,
            None,
            &[],
            None,
        );
        let joined = argv.join(" ");
        assert!(joined.contains("--network=slirp4netns:allow_host_loopback=true"));
        assert!(joined.contains("HTTP_PROXY=http://10.0.2.2:8123"));
        assert!(joined.contains("HTTPS_PROXY=http://10.0.2.2:8123"));
        assert!(joined.contains("NO_PROXY=localhost,127.0.0.1"));
    }

    #[test]
    fn run_argv_interactive_adds_i_and_optional_t() {
        let p = Profile::builtin("default", crate::sandbox::IsolationClaim::Container);
        let mk = |tty| {
            build_run_argv(
                &rt(),
                &p,
                Path::new("/w"),
                "img",
                "n",
                &NetPlan::None,
                &["bash".into()],
                &[],
                tty,
                None,
                &[],
                None,
            )
        };
        // Capture run: no interactive flags at all.
        let cap = mk(None);
        assert!(!cap.contains(&"-i".to_string()) && !cap.contains(&"-t".to_string()));
        // Interactive, no TTY (piped/CI): `-i` only — never `-t` (Podman rejects
        // a TTY request without one).
        let piped = mk(Some(false));
        assert!(piped.contains(&"-i".to_string()) && !piped.contains(&"-t".to_string()));
        // Interactive with a TTY: both, and they sit before the image.
        let tty = mk(Some(true));
        assert!(tty.contains(&"-i".to_string()) && tty.contains(&"-t".to_string()));
        let img = tty.iter().position(|a| a == "img").unwrap();
        assert!(tty.iter().position(|a| a == "-i").unwrap() < img);
    }

    #[test]
    fn run_argv_shim_mounts_image_shells_and_spool() {
        let p = Profile::builtin("default", crate::sandbox::IsolationClaim::Container);
        let plan = ShimPlan {
            shim: "/envdir/shim/sh".into(),
            spool: "/envdir/spool".into(),
        };
        let argv = build_run_argv(
            &rt(),
            &p,
            Path::new("/w"),
            "img",
            "n",
            &NetPlan::None,
            &["bash".into()],
            &[],
            Some(true),
            Some(&plan),
            &[],
            None,
        );
        let joined = argv.join(" ");
        // The image self-mount keeps the real shell reachable for any image.
        assert!(joined.contains("type=image,source=img,destination=/.h5i/orig"));
        // Both shell spellings are shadowed by the same shim, read-only.
        assert!(joined.contains("type=bind,source=/envdir/shim/sh,target=/bin/sh,ro"));
        assert!(joined.contains("type=bind,source=/envdir/shim/sh,target=/bin/bash,ro"));
        // The spool is the only extra writable mount.
        assert!(joined.contains("type=bind,source=/envdir/spool,target=/.h5i/spool,rw"));
        // Captured runs must stay shim-free.
        let plain = build_run_argv(
            &rt(),
            &p,
            Path::new("/w"),
            "img",
            "n",
            &NetPlan::None,
            &["true".into()],
            &[],
            None,
            None,
            &[],
            None,
        );
        assert!(!plain.join(" ").contains("/.h5i/"));
    }

    // Live on-host: the generated shim is real POSIX sh — run it against the
    // host's /bin/sh (as the "image" shell) and prove pass-through semantics
    // (stdout, stderr, exit code, stdin) plus a complete spool record.
    #[test]
    #[cfg(unix)]
    fn shim_script_tees_and_passes_through() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = std::env::temp_dir().join(format!("h5i-shim-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let orig_bin = tmp.join("orig/bin");
        let spool = tmp.join("spool");
        std::fs::create_dir_all(&orig_bin).unwrap();
        std::fs::create_dir_all(&spool).unwrap();
        // The "image's real shell": a symlink to the host's /bin/sh.
        std::os::unix::fs::symlink("/bin/sh", orig_bin.join("sh")).unwrap();
        let shim = tmp.join("sh");
        std::fs::write(
            &shim,
            shim_script(
                &tmp.join("orig").display().to_string(),
                &spool.display().to_string(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();

        // Observed: `-c`, no TTY (Command pipes) → tee + spool + exit code.
        let out = std::process::Command::new(&shim)
            .args(["-c", "echo visible; echo err-line >&2; exit 3"])
            .output()
            .expect("run shim");
        assert_eq!(out.status.code(), Some(3), "exit code must pass through");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "visible\n",
            "stdout must pass through"
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("err-line"),
            "stderr must pass through"
        );
        let entries: Vec<_> = std::fs::read_dir(&spool)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect();
        let cmd_file = entries
            .iter()
            .find(|n| n.ends_with(".cmd"))
            .expect("spooled .cmd");
        let base = cmd_file.trim_end_matches(".cmd");
        assert!(std::fs::read_to_string(spool.join(cmd_file))
            .unwrap()
            .contains("echo visible"));
        assert_eq!(
            std::fs::read_to_string(spool.join(format!("{base}.out"))).unwrap(),
            "visible\n"
        );
        assert!(std::fs::read_to_string(spool.join(format!("{base}.err")))
            .unwrap()
            .contains("err-line"));
        assert_eq!(
            std::fs::read_to_string(spool.join(format!("{base}.exit"))).unwrap(),
            "3"
        );

        // Nested invocation (H5I_SHIM set) passes through unobserved.
        let before = std::fs::read_dir(&spool).unwrap().count();
        let nested = std::process::Command::new(&shim)
            .args(["-c", "echo nested"])
            .env("H5I_SHIM", "1")
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&nested.stdout), "nested\n");
        assert_eq!(
            std::fs::read_dir(&spool).unwrap().count(),
            before,
            "no new spool entries"
        );

        // Non-`-c` (script execution) passes through unobserved.
        let script = tmp.join("inner.sh");
        std::fs::write(&script, "echo from-script\n").unwrap();
        let run = std::process::Command::new(&shim)
            .arg(&script)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&run.stdout), "from-script\n");
        assert_eq!(
            std::fs::read_dir(&spool).unwrap().count(),
            before,
            "scripts are not observed"
        );

        // Stdin flows through an observed command untouched.
        use std::io::Write as _;
        let mut child = std::process::Command::new(&shim)
            .args(["-c", "cat"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"ping\n").unwrap();
        let fin = child.wait_with_output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&fin.stdout),
            "ping\n",
            "stdin must pass through"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // The argv-scan: different runtimes spell the command flag differently
    // (Claude `bash -c`, Codex `bash -lc`). We assert only which invocations
    // the shim *observes* (writes a `.cmd` for) and the command it extracted —
    // not the output — so the dummy commands "failing" under the real shell is
    // irrelevant; the `.cmd` is written before the real shell ever runs.
    #[test]
    #[cfg(unix)]
    fn shim_detects_combined_c_flags_and_skips_non_commands() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = std::env::temp_dir().join(format!("h5i-shim-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let orig_bin = tmp.join("orig/bin");
        let spool = tmp.join("spool");
        std::fs::create_dir_all(&orig_bin).unwrap();
        std::fs::create_dir_all(&spool).unwrap();
        // The shim's shebang interpreter *and* its exec target both resolve to
        // orig/bin/sh, so it must be the real shell (a stub would short-circuit
        // the shim body itself).
        std::os::unix::fs::symlink("/bin/sh", orig_bin.join("sh")).unwrap();
        let shim = tmp.join("sh");
        std::fs::write(
            &shim,
            shim_script(
                &tmp.join("orig").display().to_string(),
                &spool.display().to_string(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();

        // Run the shim with `args`, stdout piped (so `[ -t 1 ]` is false), and
        // return the single `.cmd` payload it spooled this run (or None).
        let observe = |args: &[&str], envs: &[(&str, &str)]| -> Option<String> {
            for e in std::fs::read_dir(&spool).unwrap() {
                let _ = std::fs::remove_file(e.unwrap().path());
            }
            let mut c = std::process::Command::new(&shim);
            c.args(args)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null());
            for (k, v) in envs {
                c.env(k, v);
            }
            c.output().unwrap();
            std::fs::read_dir(&spool)
                .unwrap()
                .filter_map(|e| e.ok())
                .find(|e| e.file_name().to_string_lossy().ends_with(".cmd"))
                .map(|e| std::fs::read_to_string(e.path()).unwrap())
        };

        // Observed — every command-flag spelling is detected, command extracted.
        assert_eq!(
            observe(&["-c", "AAA"], &[]).as_deref(),
            Some("AAA"),
            "plain -c (Claude)"
        );
        assert_eq!(
            observe(&["-lc", "BBB"], &[]).as_deref(),
            Some("BBB"),
            "-lc (Codex)"
        );
        assert_eq!(observe(&["-ic", "CCC"], &[]).as_deref(), Some("CCC"), "-ic");
        assert_eq!(
            observe(&["-i", "-c", "DDD"], &[]).as_deref(),
            Some("DDD"),
            "option before -c"
        );

        // Not observed — no command flag, or it's a nested/script invocation.
        assert_eq!(
            observe(&["script.sh"], &[]),
            None,
            "a script run is not a command"
        );
        assert_eq!(
            observe(&["-i"], &[]),
            None,
            "an interactive shell has no -c"
        );
        assert_eq!(
            observe(&["--", "-c", "X"], &[]),
            None,
            "`--` ends option scan"
        );
        assert_eq!(
            observe(&["-c", "EEE"], &[("H5I_SHIM", "1")]),
            None,
            "nested shell passes through"
        );

        // h5i's own invocations are NOT recorded (the wrap-bash hook already
        // captures them via `h5i capture run`) — no double-capture, no overhead.
        assert_eq!(
            observe(&["-lc", "h5i capture run -- cargo test"], &[]),
            None,
            "hook-wrapped `h5i capture run …` passes through unrecorded"
        );
        assert_eq!(observe(&["-c", "h5i"], &[]), None, "a bare `h5i` passes through");
        // …but a command that merely CONTAINS h5i as an argument is still ours.
        assert_eq!(
            observe(&["-c", "grep h5i log.txt"], &[]).as_deref(),
            Some("grep h5i log.txt"),
            "h5i as a non-leading word is still observed"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn run_argv_injects_brokered_secret_env() {
        let p = Profile::builtin("default", crate::sandbox::IsolationClaim::Container);
        let injected = vec![("GITHUB_TOKEN".to_string(), "ghp_secret".to_string())];
        let argv = build_run_argv(
            &rt(),
            &p,
            Path::new("/w"),
            "img",
            "n",
            &NetPlan::None,
            &["true".into()],
            &injected,
            None,
            None,
            &[],
            None,
        );
        // The broker's env grant is passed to the container by NAME only — the
        // value is forwarded from Podman's own env, never placed in argv (which
        // is world-readable via /proc/<pid>/cmdline).
        let joined = argv.join(" ");
        assert!(
            !joined.contains("ghp_secret"),
            "secret VALUE must never appear in the container argv: {argv:?}"
        );
        let pos = argv.iter().position(|a| a == "GITHUB_TOKEN");
        assert!(
            pos.is_some(),
            "injected secret must appear as a --env NAME: {argv:?}"
        );
        // ...preceded by --env, and BEFORE the image (so it's a podman flag, not
        // an argument to the command).
        let i = pos.unwrap();
        assert_eq!(argv[i - 1], "--env");
        let img = argv.iter().position(|a| a == "img").unwrap();
        assert!(i < img, "secret env must precede the image in argv");
    }

    #[test]
    fn proxy_gates_connect_by_allowlist() {
        use std::io::{BufRead, BufReader};

        // Allow only an unreachable host so we never actually open egress; we
        // only assert the gate's accept/deny verdict.
        let allow = AllowList::parse(&["allowed.invalid:443".into()]);
        let proxy = spawn_proxy(allow).unwrap();

        // Denied host → 403, fail-closed.
        let mut c = TcpStream::connect(("127.0.0.1", proxy.port)).unwrap();
        c.write_all(b"CONNECT evil.example.com:443 HTTP/1.1\r\n\r\n")
            .unwrap();
        let mut line = String::new();
        BufReader::new(c.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert!(
            line.contains("403"),
            "denied host must get 403, got: {line:?}"
        );

        // Allowed host → the gate passes and tries to connect upstream; since
        // `allowed.invalid` doesn't resolve, we get 502 (not 403) — proving the
        // allowlist verdict was "permit".
        let mut c2 = TcpStream::connect(("127.0.0.1", proxy.port)).unwrap();
        c2.write_all(b"CONNECT allowed.invalid:443 HTTP/1.1\r\n\r\n")
            .unwrap();
        let mut line2 = String::new();
        BufReader::new(c2.try_clone().unwrap())
            .read_line(&mut line2)
            .unwrap();
        assert!(
            line2.contains("502") || line2.contains("200"),
            "allowed host must pass the gate (502/200), got: {line2:?}"
        );
    }
}
