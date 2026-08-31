//! The `isolation=container` backend: run an environment's command inside a
//! *rootless Podman* container, and, uniquely,
//! enforce a `net.egress` *domain allowlist* that the static `process` tier
//! cannot (docs/environments-design.md §5–§7, rollout phase 4).
//!
//! Hardening (EscapeBench footguns, §2): `--rm`, `--cap-drop=ALL`,
//! `--security-opt=no-new-privileges`, a read-only rootfs with a private
//! `/tmp` tmpfs, `--userns=keep-id` so files in the bind-mounted workspace keep
//! the caller's ownership, memory/pid limits, an env-var allowlist, and *never*
//! a Docker socket mount. The container is an *opt-in adapter* that shells out
//! to Podman if the user already has it. It adds no Rust dependency. Docker is
//! intentionally not accepted in this phase: its daemon/socket model has a
//! different trust boundary and is easy to misconfigure for agent workloads.
//!
//! ### Egress enforcement. Honestly scoped
//!
//! When `net.egress` is non-empty, h5i resolves+pins the allowlisted domains to
//! IPs at startup (kills DNS-rebinding) and runs a small HTTP/HTTPS CONNECT
//! allowlist proxy on the host; the container is pointed at it via the
//! `HTTP(S)_PROXY` env vars. This is *L7* enforcement: it blocks the dominant
//! exfiltration path, `curl`/`wget`/`pip`/`npm`/`requests` to a non-allowlisted
//! host, fail-closed (anything not on the list gets `403`). It does *not* by
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
use crate::sandbox_policy::{EgressHost, EgressSummary, MAX_EGRESS_HOSTS};
use crate::sandbox_policy::{ExecOutcome, InteractiveOutcome, NetMode, Profile, ResolvedPolicy};

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
        // Denials first, then by descending traffic. Most interesting on top.
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

/// Cheap presence check: is the `podman` binary on PATH at all? Runs only
/// `podman --version` (~tens of ms), *not* the `podman info` rootless probe
/// (~1s). Use for discoverability hints that just need "is Podman installed?",
/// not "is rootless Podman usable?". The latter is [`probe`].
pub fn podman_present() -> bool {
    version_ok("podman")
}

/// Detect the only container runtime this phase supports: *rootless Podman*.
/// Returns `None` when Podman is absent, broken, or running as root.
///
/// *Cached*. The underlying `podman info` costs ~1.3s, and `env run`/`env
/// shell` pay it on every invocation. Two layers, both skippable with
/// `H5I_NO_PROBE_CACHE=1`:
/// - in-process `OnceLock` (a single h5i invocation never probes twice);
/// - a per-boot file under `$XDG_RUNTIME_DIR/h5i/` (tmpfs: vanishes at
///   reboot, the natural invalidation for a host-capability fact), validated
///   against the resolved `podman` binary's path + mtime + size so an
///   upgraded/moved Podman re-probes.
///
/// Only the *positive* result is cached: "no rootless podman" answers fast
/// anyway and must self-heal the moment the user installs/fixes Podman.
pub fn probe() -> Option<Runtime> {
    if std::env::var_os("H5I_NO_PROBE_CACHE").is_some() {
        return probe_uncached();
    }
    static PROBE: std::sync::OnceLock<Option<Runtime>> = std::sync::OnceLock::new();
    PROBE
        .get_or_init(|| {
            let cache = probe_cache_path();
            if let Some(rt) = cache.as_deref().and_then(read_probe_cache) {
                return Some(rt);
            }
            let rt = probe_uncached();
            if let (Some(rt), Some(cache)) = (&rt, cache.as_deref()) {
                write_probe_cache(cache, rt);
            }
            rt
        })
        .clone()
}

/// [`probe`] with both caches bypassed, for the diagnostic paths (`box probe`,
/// `box capabilities`, the console's `/api/probe`) that must report the host as
/// it is right now.
///
/// A function rather than the `H5I_NO_PROBE_CACHE` environment variable: those
/// callers used to set that variable at run time, which mutates process-global
/// state shared with every other thread (unsound in a threaded program, and a
/// side effect that outlived the one call that wanted it). The variable stays as
/// an operator escape hatch. Read, never written.
pub fn probe_fresh() -> Option<Runtime> {
    probe_uncached()
}

fn probe_uncached() -> Option<Runtime> {
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

/// Per-boot cache location: `$XDG_RUNTIME_DIR` is a per-user tmpfs, so the
/// cache dies with the boot. No runtime dir (rare: cron without a session,
/// containers) → no file cache, probe normally.
fn probe_cache_path() -> Option<std::path::PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")?;
    if dir.is_empty() {
        return None;
    }
    Some(Path::new(&dir).join("h5i").join("podman-probe.json"))
}

/// Identity of the podman binary the cached verdict was probed against.
/// Path + mtime + size. Enough to catch an upgrade, downgrade, or a PATH
/// change putting a different podman first.
fn podman_fingerprint() -> Option<(String, u64, u64)> {
    let path = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|d| {
            let c = d.join("podman");
            c.is_file().then_some(c)
        })
    })?;
    let md = std::fs::metadata(&path).ok()?;
    let mtime = md
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some((path.display().to_string(), mtime, md.len()))
}

fn read_probe_cache(cache: &Path) -> Option<Runtime> {
    let text = std::fs::read_to_string(cache).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let (path, mtime, size) = podman_fingerprint()?;
    if v.get("podman_path").and_then(|x| x.as_str()) != Some(path.as_str())
        || v.get("podman_mtime").and_then(|x| x.as_u64()) != Some(mtime)
        || v.get("podman_size").and_then(|x| x.as_u64()) != Some(size)
        || v.get("rootless").and_then(|x| x.as_bool()) != Some(true)
    {
        return None;
    }
    let bin = v.get("bin").and_then(|x| x.as_str())?.to_string();
    Some(Runtime { bin, rootless: true })
}

/// Best-effort (a probe cache must never fail a run): tmp-file + rename so a
/// concurrent h5i invocation reads either the old or the new entry, never a
/// torn one.
fn write_probe_cache(cache: &Path, rt: &Runtime) {
    let Some((path, mtime, size)) = podman_fingerprint() else {
        return;
    };
    let Some(dir) = cache.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let entry = serde_json::json!({
        "bin": rt.bin,
        "rootless": rt.rootless,
        "podman_path": path,
        "podman_mtime": mtime,
        "podman_size": size,
    });
    let tmp = cache.with_extension(format!("tmp.{}", std::process::id()));
    if std::fs::write(&tmp, entry.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, cache);
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
    /// Addresses this exact host resolved to at startup. Empty for a wildcard
    /// (unenumerable) or when startup resolution failed.
    pinned: Vec<IpAddr>,
}

/// Parse one `net.egress` entry into `(host, wildcard, port)`, fail-closed.
///
/// This is the single definition of the grammar. It used to exist twice, here
/// and in `microvm::egress_rule_tokens`, and the copies drifted: the microvm
/// side refused an out-of-range port, a single-label wildcard, an IPv6 literal
/// and the reserved `,`/`@`, while this side silently widened or mangled all
/// four. `example.com:99999` in particular became an *any-port* rule, the exact
/// opposite of what the entry asked for.
///
/// `Ok(None)` is a blank entry (skip it); anything malformed is an error.
pub fn parse_egress_rule(raw: &str) -> Result<Option<(String, bool, Option<u16>)>, H5iError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    if raw.contains(',') || raw.contains('@') {
        return Err(H5iError::Metadata(format!(
            "net.egress entry '{raw}' contains ',' or '@', which the rule grammar reserves — \
             refusing rather than emitting a rule that means something else (fail-closed). \
             Split it into separate entries."
        )));
    }
    // An IPv6 literal is full of colons and the port split cannot tell one from
    // a `host:port`: `2001:db8::1` came out as host `2001:db8:` port 1. There is
    // no unambiguous spelling here, so refuse rather than translate it wrong.
    if raw.matches(':').count() > 1 {
        return Err(H5iError::Metadata(format!(
            "net.egress entry '{raw}' looks like an IPv6 literal (more than one ':'), which the \
             rule grammar cannot carry unambiguously — refusing rather than emitting a rule that \
             means something else (fail-closed). Use a hostname, or an IPv4 address or CIDR."
        )));
    }
    let (host_part, port) = match raw.rsplit_once(':') {
        // Only a trailing all-digit segment is a port.
        Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
            let port = p.parse::<u16>().map_err(|_| {
                H5iError::Metadata(format!(
                    "net.egress entry '{raw}' has a port outside 1-65535 — refusing rather than \
                     falling back to an any-port rule (fail-closed)."
                ))
            })?;
            (h, Some(port))
        }
        _ => (raw, None),
    };
    let lower = host_part.to_ascii_lowercase();
    let (host, wildcard) = match lower.strip_prefix("*.").or_else(|| lower.strip_prefix('.')) {
        Some(rest) => (rest.to_string(), true),
        None => (lower, false),
    };
    if host.is_empty() {
        return Ok(None);
    }
    if wildcard && host.split('.').filter(|l| !l.is_empty()).count() < 2 {
        return Err(H5iError::Metadata(format!(
            "net.egress wildcard '{raw}' covers a single label — a suffix rule must name at \
             least two (e.g. '*.example.com', not '*.com'). Refusing (fail-closed)."
        )));
    }
    // And the host has to be a host. Everything above constrains the
    // *separators* (`,`, `@`, the colon, the wildcard prefix) and nothing
    // constrained what was left, so any leftover string became a rule.
    //
    // That is where the two tiers stopped agreeing, which is the thing this
    // function exists to prevent. A second colon that is not a port is kept as
    // part of the host: `example.com:any` parses to the host
    // `"example.com:any"`, which the container proxy compares against real
    // hostnames and never matches, an inert rule, while the microvm tier
    // emits it as the token `allow@example.com:any`, where the colon is
    // microsandbox's *protocol* qualifier. One policy, denied on one tier and
    // widened on the other.
    //
    // A hostname, an IPv4 address or an IPv4 CIDR, and nothing else. A trailing
    // dot is refused rather than trimmed: `allows` trims it from the request
    // host, so a rule carrying one matches nothing today, and trimming it here
    // would turn a rule that never fired into one that does.
    if !is_rule_host(&host) {
        return Err(H5iError::Metadata(format!(
            "net.egress entry '{raw}' is not a hostname, an IPv4 address or a CIDR — refusing \
             rather than emitting a rule whose meaning depends on which tier reads it \
             (fail-closed)."
        )));
    }
    Ok(Some((host, wildcard, port)))
}

/// Is `host` something a rule may name: a DNS name, an IPv4 address, or an IPv4
/// CIDR? See [`parse_egress_rule`] for why this is checked at all.
fn is_rule_host(host: &str) -> bool {
    let (name, prefix) = match host.split_once('/') {
        Some((n, p)) => (n, Some(p)),
        None => (host, None),
    };
    if let Some(p) = prefix {
        // A prefix length only means anything on an address.
        let sane = !p.is_empty()
            && p.len() <= 2
            && p.bytes().all(|b| b.is_ascii_digit())
            && p.parse::<u8>().is_ok_and(|n| n <= 32);
        if !sane || name.parse::<std::net::Ipv4Addr>().is_err() {
            return false;
        }
    }
    !name.is_empty()
        && name.len() <= 253
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                // RFC 1123: a label neither begins nor ends with a hyphen. Also
                // what stops a bare `-`, which is a host to a charset check
                // and an option to anything that reads argv.
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
}

impl AllowEntry {
    /// Does this entry open `port`? `None` means any.
    fn port_ok(&self, port: u16) -> bool {
        self.port.is_none_or(|p| p == port)
    }

    /// Does this entry cover `host:port`? `host` must already be lower-cased
    /// and free of a trailing dot.
    fn matches(&self, host: &str, port: u16) -> bool {
        self.port_ok(port)
            && if self.wildcard {
                host == self.host
                    || host
                        .strip_suffix(self.host.as_str())
                        .is_some_and(|p| p.ends_with('.'))
            } else {
                host == self.host
            }
    }
}

/// Is this address one the box has no business reaching through the proxy?
///
/// Used only where a destination could not be pinned. The proxy runs on the
/// host with full host network access, so an unpinned name that resolves to
/// loopback, a private range, or the cloud metadata link-local address turns
/// the egress boundary into an SSRF gadget. `IpAddr::is_global` is still
/// unstable, so the ranges are spelled out.
fn is_internal(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => {
            let [o0, o1, ..] = a.octets();
            a.is_loopback()
                || a.is_private()
                || a.is_link_local()
                || a.is_broadcast()
                || a.is_documentation()
                || a.is_unspecified()
                || a.is_multicast()
                || o0 == 0
                || (o0 == 100 && (64..128).contains(&o1)) // CGNAT 100.64/10
                || (o0 == 198 && (o1 & 0xfe) == 18) // benchmarking 198.18/15
                || o0 >= 240 // reserved 240/4
        }
        IpAddr::V6(a) => {
            if let Some(v4) = a.to_ipv4_mapped() {
                return is_internal(&IpAddr::V4(v4));
            }
            let s0 = a.segments()[0];
            a.is_loopback()
                || a.is_unspecified()
                || a.is_multicast()
                || (s0 & 0xfe00) == 0xfc00 // unique-local fc00::/7
                || (s0 & 0xffc0) == 0xfe80 // link-local fe80::/10
        }
    }
}

/// The agent hook-config files that every image-backed tier mounts read-only
/// over their place in `$WORK`, so the in-box agent cannot rewrite the file
/// that defines its own observation hook.
pub const AGENT_CONFIG_RELS: [&str; 2] = [".claude/settings.json", ".codex/config.toml"];

/// Resolve one agent config file into a mount source, refusing anything that is
/// not a regular file that really lives inside `$WORK`.
///
/// `$WORK` holds the branch under review, so this path and every directory
/// above it are supplied by the repository. `Path::exists()` follows symlinks
/// and the *unresolved* path is what would reach the mount spec, which the
/// kernel then resolves host-side: a branch shipping `.claude/settings.json`
/// as a symlink to `~/.ssh` would have h5i mount that directory into the box.
/// The exposure is created entirely by the mount. Left alone, the link dangles
/// inside the box's mount namespace because the target is not in it.
///
/// `symlink_metadata` refuses a symlinked final component; canonicalizing and
/// re-checking the prefix refuses a symlinked *ancestor*, which is the same
/// trick one directory up.
pub fn agent_config_mount_source(work: &Path, rel: &str) -> Option<PathBuf> {
    let source = work.join(rel);
    if !std::fs::symlink_metadata(&source).is_ok_and(|md| md.file_type().is_file()) {
        return None;
    }
    let canon_work = work.canonicalize().ok()?;
    let canon = source.canonicalize().ok()?;
    canon.starts_with(&canon_work).then_some(canon)
}

/// Combine the digested profile allowlist with the host-side user extras
/// (`h5i box allow`) into the rule set the proxy enforces. Fail-closed
/// widening rule: when the profile's own `net.egress` is empty (deny-all),
/// the user extras are IGNORED. State outside the digested policy must never
/// turn a no-network profile into a networked one; it may only add hosts to a
/// profile that already opted into egress. Order-preserving, deduplicated.
pub fn effective_egress(profile_egress: &[String], user_allow: &[String]) -> Vec<String> {
    if profile_egress.iter().all(|e| e.trim().is_empty()) {
        return Vec::new();
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for rule in profile_egress.iter().chain(user_allow.iter()) {
        let rule = rule.trim();
        if rule.is_empty() {
            continue;
        }
        if seen.insert(rule.to_ascii_lowercase()) {
            out.push(rule.to_string());
        }
    }
    out
}

/// A resolved egress allowlist: parsed host rules, each carrying the addresses
/// its host pinned to at startup.
///
/// The pin is what makes the proxy DNS-rebinding resistant, and it only works
/// if the proxy *dials* the pinned address. Matching a name and then calling
/// `TcpStream::connect((host, port))` re-resolves, which is how an allowlisted
/// domain under attacker DNS control used to reach host loopback or a metadata
/// endpoint through a proxy that has full host network access. See
/// [`AllowList::dial_addrs`].
#[derive(Debug, Clone, Default)]
pub struct AllowList {
    entries: Vec<AllowEntry>,
}

impl AllowList {
    /// Parse `net.egress` entries through [`parse_egress_rule`] (no DNS yet,
    /// pure, for tests). Fail-closed: a malformed entry is an error, never a
    /// rule that means something wider than what was written.
    pub fn parse(egress: &[String]) -> Result<AllowList, H5iError> {
        let mut entries = Vec::new();
        for raw in egress {
            if let Some((host, wildcard, port)) = parse_egress_rule(raw)? {
                entries.push(AllowEntry {
                    host,
                    wildcard,
                    port,
                    pinned: Vec::new(),
                });
            }
        }
        Ok(AllowList { entries })
    }

    /// Resolve every exact allowed host and pin the answers onto its entry.
    /// Best-effort: a host that fails to resolve keeps an empty pin and falls
    /// back to a guarded resolve at CONNECT time (see [`Self::dial_addrs`]).
    /// Returns the number of distinct addresses pinned.
    pub fn pin_dns(&mut self) -> usize {
        let mut distinct = HashSet::new();
        for e in &mut self.entries {
            if e.wildcard {
                continue; // can't enumerate a wildcard's IPs
            }
            let port = e.port.unwrap_or(443);
            if let Ok(addrs) = (e.host.as_str(), port).to_socket_addrs() {
                e.pinned = addrs.map(|a| a.ip()).collect();
                distinct.extend(e.pinned.iter().copied());
            }
        }
        distinct.len()
    }

    /// The addresses the proxy may dial for an already-[`allowed`](Self::allows)
    /// request. Empty means refuse.
    ///
    /// Pinned addresses win, so the name the allowlist matched and the address
    /// the socket reaches are decided by the *same* DNS answer. Only a wildcard
    /// entry (or a host that would not resolve at startup) falls through to a
    /// live lookup, and that answer is filtered to globally routable addresses:
    /// without a pin there is nothing else standing between a rebound record
    /// and the host's own loopback.
    pub fn dial_addrs(&self, host: &str, port: u16) -> Vec<std::net::SocketAddr> {
        let h = host.trim().trim_end_matches('.').to_ascii_lowercase();
        if let Ok(ip) = h.parse::<IpAddr>() {
            return vec![std::net::SocketAddr::new(ip, port)];
        }
        let mut out: Vec<std::net::SocketAddr> = Vec::new();
        for e in self.entries.iter().filter(|e| e.matches(&h, port)) {
            out.extend(e.pinned.iter().map(|ip| std::net::SocketAddr::new(*ip, port)));
        }
        if !out.is_empty() {
            out.sort();
            out.dedup();
            return out;
        }
        (h.as_str(), port)
            .to_socket_addrs()
            .map(|it| it.filter(|a| !is_internal(&a.ip())).collect())
            .unwrap_or_default()
    }

    /// Decide whether a CONNECT/request to `host:port` is allowed (fail-closed:
    /// the empty allowlist permits nothing).
    pub fn allows(&self, host: &str, port: u16) -> bool {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        // Direct connection by address: permitted only when some entry pinned
        // that exact IP *and* opened this port. Ignoring the port here let
        // `CONNECT <pinned-ip>:22` through on an allowlist of `host:443`.
        if let Ok(ip) = host.parse::<IpAddr>() {
            return self.entries.iter().any(|e| {
                e.port_ok(port)
                    // Either an address the allowlist pinned for one of its
                    // names, or an address the operator listed literally.
                    && (e.pinned.contains(&ip) || (!e.wildcard && e.host == host))
            });
        }
        for e in &self.entries {
            if e.matches(&host, port) {
                return true;
            }
        }
        false
    }
}

/// Resource caps this tier cannot apply, named so a run can say so instead of
/// silently dropping them. Mirrors the honesty machinery Seatbelt already uses
/// for the same situation.
pub fn unmapped_resources(profile: &Profile) -> Vec<String> {
    let mut out = Vec::new();
    if profile.cpu_secs.is_some() {
        out.push(
            "resources.cpu (a CPU-seconds budget; podman caps CPU rate, not total)".to_string(),
        );
    }
    out
}

/// Most bytes the tee shim records per stream, per command. The box decides how
/// much it writes, so this is a disk bound on the host, not a display limit.
/// The command's own output still passes through in full.
const SPOOL_STREAM_CAP: u64 = 4 * 1024 * 1024;

/// Most connections the egress proxy will serve at once. Generous for real
/// tooling (parallel `cargo`/`npm` fetches) and far below what it takes to
/// exhaust host threads.
const MAX_PROXY_CONNECTIONS: usize = 64;

/// How many *consecutive* failed `accept()` calls end the accept loop. Sized so
/// transient per-connection and fd-pressure errors (which retry at 25 ms) never
/// reach it, while a permanently broken listener still stops the thread instead
/// of spinning. Mirrors the credential proxy's bound, for the same reason.
const MAX_CONSECUTIVE_ACCEPT_ERRORS: usize = 256;

/// Holds one of the [`MAX_PROXY_CONNECTIONS`] slots and releases it on drop,
/// including when the worker unwinds.
///
/// The count is the only thing between the box and an unbounded number of host
/// threads, so the release has to be unconditional. A bare `fetch_sub` after
/// the call is not: `splice` calls `std::thread::spawn`, which *panics* when
/// the OS refuses a thread, and that is reachable from exactly the load this
/// cap exists to bound. A leaked slot is permanent, and after
/// `MAX_PROXY_CONNECTIONS` of them the proxy answers 503 for the life of the
/// box, which, since the proxy is the box's only route out, is the box losing
/// the network entirely. `auth_proxy::InFlightSlot` is the same guard for the
/// same reason; this sibling never got it.
struct ProxySlot(Arc<std::sync::atomic::AtomicUsize>);

impl Drop for ProxySlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
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
    spawn_proxy_on(allow, None)
}

/// [`spawn_proxy`] on a caller-chosen loopback port
/// ([`ResolvedPolicy::egress_proxy_port`]: a box whose browser outlives the
/// run needs the proxy to still be at the address that browser memorised).
///
/// Falls back to an ephemeral port when the requested one cannot be bound: a
/// squatted port is a reason to lose browser continuity, not a reason to refuse
/// the run. The note says which one happened, because the symptom otherwise
/// surfaces much later as a proxy error inside the box.
pub fn spawn_proxy_on(allow: AllowList, want: Option<u16>) -> Result<ProxyHandle, H5iError> {
    let listener = match want {
        Some(p) => match TcpListener::bind(("127.0.0.1", p)) {
            Ok(l) => l,
            Err(e) => {
                eprintln!(
                    "note: egress proxy could not take this box's pinned port {p} ({e}); using an \
                     ephemeral one — a browser left running by an earlier run will report \
                     ERR_PROXY_CONNECTION_FAILED until it is restarted"
                );
                TcpListener::bind("127.0.0.1:0").map_err(H5iError::Io)?
            }
        },
        None => TcpListener::bind("127.0.0.1:0").map_err(H5iError::Io)?,
    };
    let port = listener.local_addr().map_err(H5iError::Io)?.port();
    listener.set_nonblocking(true).map_err(H5iError::Io)?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let allow = Arc::new(allow);
    let tally = Arc::new(Mutex::new(EgressTally::default()));
    let tally_thread = tally.clone();

    // Bounded concurrency. One unbounded thread per accepted connection let a
    // box open thousands of proxy connections and exhaust host threads; the
    // proxy is the box's only route out, so the box is also the only thing that
    // benefits from unbounded parallelism here.
    let live = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let join = std::thread::spawn(move || {
        // `accept` reports per-connection and per-host conditions that say
        // nothing about the listener: `ECONNABORTED` when a client RSTs between
        // SYN and accept, `EMFILE`/`ENFILE` under fd pressure. Treating any of
        // them as fatal ended the accept loop for good, and this proxy is the
        // box's only route out, so that is the box losing the network. The
        // listener is loopback and unauthenticated by design (see the note on
        // `NetPlan::Proxy`), which means any local process could do it with a
        // connect and a reset.
        let mut consecutive_errors = 0usize;
        while !stop_thread.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut client, _)) => {
                    consecutive_errors = 0;
                    if stop_thread.load(Ordering::SeqCst) {
                        break;
                    }
                    // The accepted socket may arrive non-blocking (see
                    // `handle_proxy_client`, which is where that is corrected:
                    // at the point the blocking reads are actually made).
                    let allow = allow.clone();
                    let tally = tally_thread.clone();
                    // Claimed with one atomic, not a `load` followed by a
                    // `fetch_add`: between those two, every thread that read
                    // the same value took a slot, so the cap was advisory.
                    let taken = live.fetch_add(1, Ordering::SeqCst);
                    let slot = ProxySlot(live.clone());
                    if taken >= MAX_PROXY_CONNECTIONS {
                        // Refuse rather than queue: a queued connection still
                        // holds an fd, and the client sees a clear 503. The
                        // guard returns the slot as it goes out of scope.
                        drop(slot);
                        let _ = client.write_all(
                            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n",
                        );
                        continue;
                    }
                    // `Builder::spawn`, not `thread::spawn`: the latter
                    // *panics* when the OS refuses a thread, and that panic is
                    // in the accept loop, so the one condition the cap above
                    // exists to bound would end the loop anyway, which is the
                    // box losing the network. Refused with the same 503 as a
                    // full slot table, and the guard hands the slot back.
                    let spawned = std::thread::Builder::new().spawn(move || {
                        // Moved in, so the release happens on unwind too.
                        let _slot = slot;
                        let _ = handle_proxy_client(client, &allow, &tally, HEAD_READ_TIMEOUT);
                    });
                    if let Err(_e) = spawned {
                        // `client` moved into the closure, which was not run;
                        // it is dropped with the closure, so the peer sees a
                        // close rather than a status. Nothing else is leaked:
                        // the slot went with it.
                        continue;
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    consecutive_errors = 0;
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(_) => {
                    consecutive_errors += 1;
                    if consecutive_errors > MAX_CONSECUTIVE_ACCEPT_ERRORS {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
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
///
/// Bounded overall, not just per `read()`. The socket's read timeout bounds a
/// single syscall, so a client dribbling one header byte just under that
/// interval held its slot indefinitely, and sixty-four such connections take
/// the box's only route out away from it. The listener is loopback and
/// unauthenticated by design, so the actor need not even be the box.
fn read_head(s: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let deadline = std::time::Instant::now() + HEAD_DEADLINE;
    let mut buf = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    loop {
        if std::time::Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "request head exceeded its deadline",
            ));
        }
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

/// Whole-head deadline, as distinct from the per-`read()` timeout below.
/// Generous for a real client on a slow link, short enough that a stalled
/// connection cannot hold a slot for the life of the box.
const HEAD_DEADLINE: Duration = Duration::from_secs(30);

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

/// The box env var naming h5i's host-side egress allowlist proxy, set by every
/// tier that runs one (this one, and the macOS supervised tier) and by nothing
/// else.
///
/// It exists alongside `HTTPS_PROXY` rather than instead of it. The conventional
/// vars are what proxy-respecting tooling reads, and they are also ordinary
/// shell state a box's own rc or tooling may set for unrelated reasons, so a
/// consumer that must know "is this address h5i's proxy?" (the browser shim,
/// which has to pass Chrome an explicit `--proxy-server` because Chrome ignores
/// the environment on macOS) cannot answer it from `HTTPS_PROXY`. This variable
/// carries that fact under h5i's own name.
///
/// Not a trust boundary: a box can set any variable in its own environment, and
/// nothing here is authority. The policy grants the port the *host* bound, so a
/// box that points its Chrome elsewhere reaches a port it is denied anyway.
pub const EGRESS_PROXY_VAR: &str = "H5I_EGRESS_PROXY";

/// Environment variables that carry the egress proxy wiring. A profile's
/// `env.pass` must never re-export one of these: podman applies `--env` in
/// order, so a later name-only pass would replace h5i's value with the host's
/// and route the box around the allowlist entirely.
pub const PROXY_WIRING_VARS: [&str; 8] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "http_proxy",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
    "NO_PROXY",
    "no_proxy",
];

/// Is `key` part of the proxy wiring (case-insensitively)?
pub fn is_proxy_wiring_var(key: &str) -> bool {
    key.eq_ignore_ascii_case(EGRESS_PROXY_VAR)
        || PROXY_WIRING_VARS.iter().any(|v| key.eq_ignore_ascii_case(v))
}

/// How long a connection may take to finish sending its request head. It bounds
/// only the head: a client that connects and says nothing must not hold a thread,
/// but once the tunnel is up the relay has no clock of its own (see the reset
/// below). Passed in rather than read here so the regression test for that reset
/// can run in milliseconds instead of half a minute.
const HEAD_READ_TIMEOUT: Duration = Duration::from_secs(30);

fn handle_proxy_client(
    mut client: TcpStream,
    allow: &AllowList,
    tally: &Arc<Mutex<EgressTally>>,
    head_timeout: Duration,
) -> std::io::Result<()> {
    // Everything below reads with blocking calls, so say so rather than assume
    // it. The listener is non-blocking (the accept loop polls `stop`) and on
    // macOS (BSD `accept`, where Linux uses `accept4`) the accepted socket
    // *inherits* that flag. Left inherited, the first read that has to wait for
    // a packet returns `WouldBlock`, the relay treats it as fatal and drops the
    // connection: the box sees a CONNECT tunnel open with `200` and then reset
    // mid-TLS-handshake, which reads like an upstream fault.
    //
    // `?`, not `let _ =`: silently continuing with a non-blocking socket
    // reinstates that exact bug. Dropping the connection is the honest failure.
    client.set_nonblocking(false)?;
    client.set_read_timeout(Some(head_timeout))?;
    let head = read_head(&mut client)?;
    let Some((host, port, is_connect)) = parse_target(&head) else {
        // A malformed/empty request (incl. the shutdown probe) records no
        // verdict. Only real CONNECT/HTTP targets count toward egress.
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
    // Dial the address the allowlist pinned, not a fresh lookup of the name it
    // matched: re-resolving here is what let a rebound record send the proxy,
    // a host process with full host network access, at loopback instead.
    let addrs = allow.dial_addrs(&host, port);
    let mut upstream = match addrs.iter().find_map(|a| TcpStream::connect(a).ok()) {
        Some(s) => s,
        None => {
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
    // The 30s timeout above bounds *head* parsing. A client that opens a
    // connection and says nothing must not hold a thread. It must not survive
    // into the tunnel: `splice` hands a `try_clone` of this socket to the
    // client→upstream copy, and a clone shares `SO_RCVTIMEO`, so 30s of client
    // silence would end that copy with a timeout error and shut the upstream
    // write half down. Client silence is the normal state of a tunnel that is
    // waiting for a response, so anything with a slow first byte (a streaming
    // completion, an SSE subscription, a large clone over HTTPS) died at 30s
    // mid-flight. Once the head is parsed, the relay's only clock is the
    // caller's.
    client.set_read_timeout(None)?;
    splice(client, upstream);
    Ok(())
}

/// Bidirectionally copy until either side closes.
fn splice(a: TcpStream, b: TcpStream) {
    let (mut a1, mut b1) = (a, b);
    let a2 = a1.try_clone();
    let b2 = b1.try_clone();
    if let (Ok(mut a2), Ok(mut b2)) = (a2, b2) {
        // `Builder::spawn`, not `thread::spawn`: the latter *panics* when the
        // OS refuses a thread. The in-flight guard means that unwind costs one
        // connection rather than the slot, but a panic is still the wrong way
        // to say "out of threads", and a half-open tunnel is not a tunnel, so
        // the honest answer is to close both ends.
        let Ok(t) = std::thread::Builder::new().spawn(move || {
            let _ = std::io::copy(&mut a2, &mut b2);
            let _ = b2.shutdown(std::net::Shutdown::Write);
        }) else {
            return;
        };
        let _ = std::io::copy(&mut b1, &mut a1);
        let _ = a1.shutdown(std::net::Shutdown::Write);
        let _ = t.join();
    }
}

// ─── run argv construction (pure; unit-tested) ───────────────────────────────

/// How a container reaches h5i's *host-side* proxies (the egress allowlist
/// proxy and the credential-injecting auth proxy). This differs by platform
/// because the network topology between the box and h5i does:
///
/// - *Linux*: Podman is rootless and local, so the container is one hop from
///   h5i. `slirp4netns` with `allow_host_loopback` exposes the host's loopback,
///   where the proxies listen, at the gateway address `10.0.2.2`. Note this is
///   *not* `host.containers.internal`, which maps to a different gateway IP that
///   does not forward to host loopback.
/// - *macOS*: Podman runs in a `podman machine` VM, so there are two hops. The
///   container's own `10.0.2.2` is the VM's loopback, where nothing of ours
///   listens; the macOS host is reached through gvproxy at
///   `host.containers.internal`, and the default machine network already routes
///   there, so no `--network` override is wanted.
pub struct HostRoute {
    /// Value for `--network=…` on the proxy plan, when the platform needs one.
    pub network_arg: Option<&'static str>,
    /// Address the box dials h5i's host-side proxies on.
    pub host_addr: &'static str,
}

#[cfg(not(target_os = "macos"))]
pub const HOST_ROUTE: HostRoute = HostRoute {
    network_arg: Some("slirp4netns:allow_host_loopback=true"),
    host_addr: "10.0.2.2",
};

#[cfg(target_os = "macos")]
pub const HOST_ROUTE: HostRoute = HostRoute {
    network_arg: None,
    host_addr: "host.containers.internal",
};

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
    /// writes here is *untrusted*. The ingest side caps and sanitizes.
    pub spool: std::path::PathBuf,
}

/// In-container mountpoints for the shim machinery. `ORIG` is the container's
/// own image self-mounted read-only, so the *unmodified* shell stays reachable
/// at `/.h5i/orig/bin/sh` for any image. The shim is a `#!/.h5i/orig/bin/sh`
/// script that uses only the image's own interpreter and utilities (no host
/// binary enters the box, so there is no glibc/arch compatibility surface).
const SHIM_ORIG_MOUNT: &str = "/.h5i/orig";
const SHIM_SPOOL_MOUNT: &str = "/.h5i/spool";
/// Per-env inbound mailbox, bind-mounted READ-ONLY (host fans messages in).
const ENV_INBOX_MOUNT: &str = "/.h5i/inbox";

/// Generate the observation shim: a pass-through POSIX `sh` script that tees
/// stdout/stderr of a non-interactive command-string invocation into the spool
/// while preserving argv, stdin, the TTY decision, and the exit code.
///
/// The command flag is detected by scanning argv for a short-option cluster
/// ending in `c` (`-c`, `-lc`, `-ic`, `-lic`, …) rather than checking `$1`
/// literally. Different runtimes spell it differently: Claude Code runs
/// `bash -c`, Codex runs `bash -lc`. The command string is the argument that
/// follows that flag. A leading non-`c` option (e.g. `bash -i -c …`) is skipped
/// over; scanning stops at `--` or the first non-option (a `sh script.sh`
/// invocation, which is not observed).
///
/// Every guard fails OPEN to `exec` of the real shell. Observation must never
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
# Cap what lands in the host-side spool. The box chooses how much it writes
# here, so an uncapped `tee` let `yes > /dev/stdout` fill the operator's disk —
# the ingest side caps what it READS, which is a different thing.
#
# POSIX sh, so no process substitution: tee's file target is a second fifo whose
# reader keeps only the first $cap bytes and then drains the rest to /dev/null.
# Draining matters — without it tee would take EPIPE once the cap was hit and
# the command's passthrough output would stop with it.
cap={spool_cap}
mkfifo "$b.oc" "$b.ec" 2>/dev/null || {{ rm -f "$b.cmd" "$b.po" "$b.pe"; exec "$real" "$@"; }}
{{ head -c "$cap" > "$b.out"; cat > /dev/null; }} < "$b.oc" &
oc=$!
{{ head -c "$cap" > "$b.err"; cat > /dev/null; }} < "$b.ec" &
ec=$!
tee "$b.oc" < "$b.po" &
po=$!
tee "$b.ec" < "$b.pe" >&2 &
pe=$!
"$real" "$@" > "$b.po" 2> "$b.pe"
rc=$?
wait "$po" "$pe" 2>/dev/null
wait "$oc" "$ec" 2>/dev/null
rm -f "$b.po" "$b.pe" "$b.oc" "$b.ec"
printf '%s' "$rc" > "$b.exit" 2>/dev/null
exit "$rc"
"#,
        orig = orig_prefix,
        spool = spool_dir,
        spool_cap = SPOOL_STREAM_CAP,
    )
}

/// Prepare the shim for an interactive session: write the script + spool dir
/// under the env directory (`work/..`). Returns `None` (observation disabled,
/// session unaffected) on any failure, including paths Podman's `--mount`
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
/// box at [`crate::sandbox_policy::CLAUDE_MANAGED_SETTINGS_PATH`]. Best-effort: returns
/// `None` (injection skipped, session otherwise unaffected) on any I/O failure
/// or a path Podman's `--mount` syntax can't carry. Only the in-box Claude
/// reads this file; it is inert for any other tooling.
fn prepare_managed_settings(work: &Path, content: &str) -> Option<PathBuf> {
    let env_dir = work.parent()?;
    let dir = env_dir.join("managed");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("managed-settings.json");
    std::fs::write(&path, content).ok()?;
    if path.display().to_string().contains(',') {
        return None;
    }
    Some(path)
}


/// `/dev/shm` size in MiB for this profile: an eighth of the memory limit,
/// floored at Podman's own 64 MiB default and capped at 1 GiB. Derived from
/// existing policy rather than a new field, so the resolved policy's digest
/// (and every pinned box) is unchanged.
fn shm_size_mb(profile: &Profile) -> u64 {
    const DEFAULT_MB: u64 = 64;
    const CAP_MB: u64 = 1024;
    match profile.mem_bytes {
        Some(bytes) => (bytes / 8 / (1024 * 1024)).clamp(DEFAULT_MB, CAP_MB),
        None => DEFAULT_MB,
    }
}

#[cfg(test)]
mod shm_tests {
    use super::*;
    use crate::sandbox::IsolationClaim;

    fn profile_with_mem(bytes: Option<u64>) -> Profile {
        let mut p = Profile::builtin("default", IsolationClaim::Container);
        p.mem_bytes = bytes;
        p
    }

    #[test]
    fn shm_scales_with_the_memory_limit_but_stays_bounded() {
        // A browser profile's 12 GiB gives it room; the cap keeps a generous
        // limit from handing the box a gigabyte-plus of shared memory.
        assert_eq!(shm_size_mb(&profile_with_mem(Some(12 * 1024 * 1024 * 1024))), 1024);
        assert_eq!(shm_size_mb(&profile_with_mem(Some(4 * 1024 * 1024 * 1024))), 512);
        // Small limits floor at Podman's own default rather than going below it.
        assert_eq!(shm_size_mb(&profile_with_mem(Some(64 * 1024 * 1024))), 64);
        assert_eq!(shm_size_mb(&profile_with_mem(None)), 64);
    }
}

/// Build the `podman run` argv for `argv` under `policy`, fully
/// hardened. `image` is the resolved base image; `name` is the (unique)
/// container name used for cleanup. Pure, no process is spawned, so this is
/// unit-tested for the security-critical flag set.
#[allow(clippy::too_many_arguments)] // a pure argv builder; a params struct would obscure more than it helps
pub fn build_run_argv(
    rt: &Runtime,
    profile: &Profile,
    // Warm dependency caches to expose read-only (roadmap 5.8). Separate from
    // the profile because they are runtime state, never part of the digest.
    ro_binds: &[crate::sandbox_policy::RoBind],
    // The one writable cache bind, set only by `cache refresh`.
    cache_write: Option<&crate::sandbox_policy::RoBind>,
    work: &Path,
    image: &str,
    name: &str,
    net: &NetPlan,
    argv: &[String],
    injected_env: &[(String, String)],
    // `None` → capture run (no stdin). `Some(tty)` → interactive: keep stdin open
    // (`-i`), allocating a pseudo-TTY (`-t`) when the caller has one. The
    // agent-in-box shell. Flags slot right after `run`, before the image.
    tty: Option<bool>,
    // Interactive observation shim (`env shell`): self-mount the image at
    // `/.h5i/orig`, shadow `/bin/sh` + `/bin/bash` with the tee shim, and mount
    // the spool rw. `None` → no observation (captured runs, or shim prep failed).
    shim: Option<&ShimPlan>,
    // In-box git plumbing (env::box_git_plumbing): each host path bind-mounted
    // at its IDENTICAL path inside the container so the worktree's
    // gitdir/commondir pointer files resolve. Empty → no extra mounts.
    box_git: &[crate::sandbox_policy::BoxGitPath],
    // Managed-settings.json (the unkillable wrap-bash hook) to bind-mount
    // read-only at the Claude managed-settings path. `None` → no injection
    // (non-Claude box, captured run, or prep failed).
    managed_settings: Option<&Path>,
    // Env capture spool for in-box `h5i capture run`. Mounted at
    // `/.h5i/spool`, matching the tee-shim spool path.
    env_capture_spool: Option<&Path>,
    // Per-env inbound mailbox, bind-mounted READ-ONLY at `/.h5i/inbox`. The host
    // fans cross-agent messages in at send time; the box reads but can't write.
    env_inbox: Option<&Path>,
    // Per-env private paths (Idea 3): each backing dir bind-mounted rw at
    // `/work/<rel>` so concurrent envs of the same repo don't share inode-level
    // locks / build caches. Empty → no extra mounts.
    private_binds: &[crate::sandbox_policy::PrivateBind],
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
        // Podman's default /dev/shm is 64 MiB, which a browser renderer eats
        // through in one heavy page and then dies with a message that blames
        // everything except the shared-memory segment. Size it from the
        // policy's own memory limit (deterministic, so the argv stays pure and
        // testable) and cap it at 1 GiB.
        "--shm-size".into(),
        format!("{}m", shm_size_mb(profile)),
        // The env workspace, mounted at /work (the in-box git plumbing below
        // adds the only other writable host paths: the env's own .git
        // surface). Use --mount rather than -v so ':' in a repository path
        // cannot be parsed as a bind-mount option suffix by Podman.
        "--mount".into(),
        format!("type=bind,source={},target=/work,rw", work.display()),
        "-w".into(),
        "/work".into(),
        "--ipc=private".into(),
    ];
    // Warm dependency caches, read-only at the package manager's own path. A
    // cache a box could write is a mutable surface shared between boxes, which
    // is the thing the design refuses.
    for b in ro_binds {
        a.push("--mount".into());
        a.push(format!(
            "type=bind,source={},target={},ro",
            b.backing.display(),
            b.target.display()
        ));
    }
    if let Some(b) = cache_write {
        a.push("--mount".into());
        a.push(format!(
            "type=bind,source={},target={},rw",
            b.backing.display(),
            b.target.display()
        ));
    }
    for rel in AGENT_CONFIG_RELS {
        let Some(source) = agent_config_mount_source(work, rel) else {
            continue;
        };
        if !source.display().to_string().contains(',') {
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
    // emitted in order. Mount targets that don't exist in the image are
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
    if let Some(ms) = managed_settings
        && !ms.display().to_string().contains(',')
    {
        a.push("--mount".into());
        a.push(format!(
            "type=bind,source={},target={},ro",
            ms.display(),
            crate::sandbox_policy::CLAUDE_MANAGED_SETTINGS_PATH
        ));
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
    } else if let Some(spool) = env_capture_spool
        && !spool.display().to_string().contains(',')
    {
        a.push("--mount".into());
        a.push(format!(
            "type=bind,source={},target={SHIM_SPOOL_MOUNT},rw",
            spool.display()
        ));
    }
    // Per-env inbound mailbox: read-only so the box can receive cross-agent
    // messages without any write access to the shared coordination store.
    if let Some(inbox) = env_inbox
        && !inbox.display().to_string().contains(',')
    {
        a.push("--mount".into());
        a.push(format!(
            "type=bind,source={},target={ENV_INBOX_MOUNT},ro",
            inbox.display()
        ));
    }
    // Private-path binds (Idea 3): each per-env backing dir mounted rw over its
    // workspace path inside the box (`/work/<rel>`), giving distinct inodes per
    // env. Comma-bearing paths (which Podman's `--mount` syntax can't carry) are
    // rejected upstream, fail-closed in `validate_private_paths` (rel) and
    // `env::prepare_private_paths` (backing), so by here every bind is safe to
    // emit; an enforcement feature must never silently drop a required bind.
    for b in private_binds {
        let rel = b.rel.trim_matches('/');
        a.push("--mount".into());
        a.push(format!(
            "type=bind,source={},target=/work/{rel},rw",
            b.backing.display()
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
    // `resources.cpu` as a CPU-seconds budget has no podman equivalent (podman
    // caps *rate*, not total), and `--ulimit fsize` is the file-size cap. Map
    // what maps; the caller reports the rest through `unmapped_resources`
    // rather than letting a profile written to bound a runaway build quietly
    // get no bound at all.
    if let Some(bytes) = profile.fsize_bytes {
        a.push("--ulimit".into());
        a.push(format!("fsize={bytes}"));
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
            // HONESTY: `allow_host_loopback=true` is what lets the box reach the
            // host-side proxy at the gateway address, and it exposes every
            // *other* host loopback service there too. Choosing the allowlist
            // plan therefore widens the box's reach relative to plain rootless
            // NAT, which is the opposite of how an allowlist reads.
            //
            // Two consequences worth naming: the box can reach `h5i ui`'s
            // console API and any local database on 127.0.0.1, and it can reach
            // another concurrent box's egress proxy, which has no client
            // authentication. Borrowing that box's wider allowlist.
            //
            // Not fixable at this layer: the proxy has to be reachable, and
            // slirp4netns exposes loopback all-or-nothing. Narrowing it needs
            // the proxy inside the box's network namespace instead. The
            // supervised tiers already avoid it (nftables narrows the jail to
            // the proxy port; Seatbelt refuses host loopback wholesale), and
            // `announce_egress` states the caveat at session start.
            if let Some(mode) = HOST_ROUTE.network_arg {
                a.push(format!("--network={mode}"));
            }
            let proxy = format!("http://{}:{port}", HOST_ROUTE.host_addr);
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
            // h5i's own name for the same address. See `EGRESS_PROXY_VAR`.
            a.push("--env".into());
            a.push(format!("{EGRESS_PROXY_VAR}={proxy}"));
            a.push("--env".into());
            a.push(format!(
                "NO_PROXY=localhost,127.0.0.1,{}",
                HOST_ROUTE.host_addr
            ));
        }
    }

    // Env-var allowlist (nothing inherited wholesale). Pass by NAME only:
    // Podman forwards each value from its own (h5i's) environment, so the
    // value never lands in the container's argv, which is world-readable on a
    // default host via `/proc/<podman-pid>/cmdline`. (`--env KEY=VALUE` would
    // leak it there.)
    let proxied = matches!(net, NetPlan::Proxy(_));
    for key in &profile.env_pass {
        // Podman applies `--env` in order, and the proxy wiring was pushed
        // above. A profile passing HTTPS_PROXY or NO_PROXY through (plausible
        // behind a corporate proxy, and repo-supplied like the rest of the
        // policy) would otherwise replace h5i's value with the host's and
        // egress unfiltered through the rootless NAT.
        if proxied && is_proxy_wiring_var(key) {
            continue;
        }
        if std::env::var_os(key).is_some() {
            a.push("--env".into());
            a.push(key.clone());
        }
    }

    // Brokered secrets (env-injected), applied after the allowlist. Also passed
    // by NAME only. The caller seeds the value into Podman's environment (see
    // `run`/`run_interactive`), keeping the credential out of the process argv.
    for (key, _value) in injected_env {
        a.push("--env".into());
        a.push(key.clone());
    }

    a.push(image.to_string());
    a.extend(argv.iter().cloned());
    a
}

// ─── credential-injecting auth proxy wiring ─────────────────────────────────

/// When a container run is an *agent-runtime* box on the egress-proxy net plan
/// *and* a host-side credential is resolvable, spawn the credential-injecting
/// [`crate::auth_proxy`] and return its handle plus the box env additions
/// (base-URL override + per-run dummy token + `NO_PROXY`). The genuine token
/// stays host-side in the proxy; the box never receives it. This is "option 2":
/// the box can authenticate to its provider API without ever holding the token.
///
/// `Ok(None)` (keeping the box on its existing in-box-login path) when: the net
/// plan has no proxy to reach the host loopback, the profile is not a known
/// agent runtime, or no host credential is available. Those never break a
/// working interactive-login flow that has no host token to broker.
///
/// A proxy that was supposed to start and could not is an `Err`, not a `None`:
/// falling back would leave the real credential in the box while also widening
/// its egress, which is the opposite of what this exists to do.
/// A live credential proxy and the env additions the box needs to reach it.
type AuthProxyEngagement = (crate::auth_proxy::AuthProxyHandle, Vec<(String, String)>);

fn maybe_auth_proxy(
    profile: &Profile,
    net: &NetPlan,
) -> Result<Option<AuthProxyEngagement>, H5iError> {
    // The box reaches the host proxy only on the egress-proxy net plan, and at
    // whichever address this platform routes to the host ([`HOST_ROUTE`]).
    // `engage_at` handles opt-out + runtime + credential resolution; the
    // container tier ignores the returned runtime (there is no per-env HOME copy
    // to scrub: the rootfs never mounts host HOME).
    let Some(e) = crate::auth_proxy::engage_at(
        &profile.name,
        matches!(net, NetPlan::Proxy(_)),
        HOST_ROUTE.host_addr,
    )?
    else {
        return Ok(None);
    };
    Ok(Some((e.handle, e.box_env)))
}

/// Live proxies for a box's authenticated-egress grants, and the env the box
/// is given to reach them.
pub type AuthGrantEngagement = (Vec<crate::auth_proxy::AuthProxyHandle>, Vec<(String, String)>);

/// Engage every profile-declared authenticated-egress grant.
///
/// Returns the live handles (held for the box's lifetime) and the env the box
/// is given. Fail-closed: a grant whose host-side credential is missing is an
/// error, never a box that reaches the upstream unauthenticated.
pub fn engage_auth_grants(
    profile: &Profile,
    tier_ok: bool,
    host_addr: &str,
) -> Result<AuthGrantEngagement, H5iError> {
    let mut handles = Vec::new();
    let mut env = Vec::new();
    for grant in &profile.auth {
        if let Some(e) = crate::auth_proxy::engage_grant_at(grant, tier_ok, host_addr)? {
            handles.push(e.handle);
            env.extend(e.box_env);
        }
    }
    Ok((handles, env))
}

/// Where a box on `claim` reaches a host-side grant proxy, or `None` when it
/// cannot reach one at all.
///
/// This used to be hard-wired to [`HOST_ROUTE`], which is the *container*
/// answer. On macOS that is `host.containers.internal`, a podman-machine name
/// that does not resolve inside a Seatbelt box sharing host loopback; and on
/// Linux `supervised` the box lives in its own netns whose nftables set only
/// ever opens the runtime auth-proxy port or `net.egress`, never a grant's
/// port, so the connection is dropped whatever address it names. Saying so is
/// better than engaging a proxy nothing can dial.
pub fn grant_host_addr(claim: crate::sandbox_policy::IsolationClaim) -> Option<&'static str> {
    use crate::sandbox_policy::IsolationClaim;
    match claim {
        IsolationClaim::Container | IsolationClaim::Microvm => Some(HOST_ROUTE.host_addr),
        // macOS supervised/process share the host's loopback.
        IsolationClaim::Supervised | IsolationClaim::Process if cfg!(target_os = "macos") => {
            Some("127.0.0.1")
        }
        _ => None,
    }
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
            "profile '{}' uses isolation=container but sets no image — pass `--image` at env \
             create, or set `container.image = \"…\"` in the profile / a repo-level \
             `[container] image` in .h5i/env.toml",
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
    // The enforced rule set = digested profile allowlist + host-side user
    // extras (`h5i box allow`); a deny-all profile ignores the extras.
    let egress_rules = effective_egress(&p.net_egress, &policy.user_egress_allow);
    let mut _proxy: Option<ProxyHandle> = None;
    let net = if !egress_rules.is_empty() {
        let mut allow = AllowList::parse(&egress_rules)?;
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

    // Credential-injecting auth proxy (option 2): held for the container's
    // lifetime. When engaged, the real API token stays host-side and the box is
    // handed only a base-URL override + per-run dummy (appended to the env).
    let (_auth_proxy, effective_env) = match maybe_auth_proxy(p, &net)? {
        Some((handle, extra)) => {
            let mut env = injected_env.to_vec();
            env.extend(extra);
            (Some(handle), env)
        }
        None => (None, injected_env.to_vec()),
    };
    let injected_env: &[(String, String)] = &effective_env;

    // A unique, filesystem-safe container name for cleanup on timeout.
    let name = format!(
        "h5i-{}-{}",
        std::process::id(),
        PROBE_SEQ.fetch_add(1, Ordering::Relaxed)
    );
    let full = build_run_argv(
        &rt,
        p,
        &policy.ro_binds,
        policy.cache_write.as_ref(),
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
        policy.env_capture_spool.as_deref(),
        policy.env_inbox.as_deref(),
        &policy.private_binds,
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

/// The *agent-in-box* path: run `argv` (a shell or a coding agent) inside the
/// hardened rootless container with stdio *inherited*. A real interactive
/// session whose every command is confined by the box (cap-drop, read-only
/// rootfs, the `net.egress` allowlist). Unlike [`run`] it captures nothing and
/// applies no wall-clock (the operator owns the session); it returns the child's
/// exit code plus the egress proxy's allow/deny tally (the proxy is held for
/// the whole session), so an interactive session leaves network evidence too.
pub fn run_interactive(
    policy: &ResolvedPolicy,
    work: &Path,
    argv: &[String],
    injected_env: &[(String, String)],
    managed_settings_content: Option<&str>,
) -> Result<InteractiveOutcome, H5iError> {
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
            "profile '{}' uses isolation=container but sets no image — pass `--image` at env \
             create or set `container.image` / a repo-level `[container] image`",
            p.name
        ))
    })?;
    if work.display().to_string().contains(',') {
        return Err(H5iError::Metadata(
            "container workspace path contains ',' — unsafe in Podman --mount syntax".into(),
        ));
    }

    let egress_rules = effective_egress(&p.net_egress, &policy.user_egress_allow);
    let mut _proxy: Option<ProxyHandle> = None;
    let net = if !egress_rules.is_empty() {
        let mut allow = AllowList::parse(&egress_rules)?;
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

    // Credential-injecting auth proxy (option 2), held for the session lifetime.
    let (_auth_proxy, effective_env) = match maybe_auth_proxy(p, &net)? {
        Some((handle, extra)) => {
            let mut env = injected_env.to_vec();
            env.extend(extra);
            (Some(handle), env)
        }
        None => (None, injected_env.to_vec()),
    };
    let injected_env: &[(String, String)] = &effective_env;

    // Allocate a TTY only when we actually have one on both ends (a piped/CI
    // invocation must not request `-t`, which Podman would reject).
    let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let name = format!(
        "h5i-{}-{}",
        std::process::id(),
        PROBE_SEQ.fetch_add(1, Ordering::Relaxed)
    );
    // Observation shim: best-effort. A session without it is fully functional.
    // It just produces no in-box command evidence.
    let shim = prepare_shim(work);
    if shim.is_none() {
        eprintln!("note: shell observation shim unavailable — session runs unobserved");
    }
    // Managed-settings injection (Claude only): pins the wrap-bash hook
    // unkillably in-box. Skip for a known-Codex profile. The file is Claude's
    // managed scope and inert for Codex (whose own hook hardening is separate);
    // for `default`/custom profiles we inject, since they may run Claude.
    let is_codex = crate::sandbox_policy::AgentRuntime::from_profile_name(&p.name)
        == Some(crate::sandbox_policy::AgentRuntime::Codex);
    // The managed-settings content is generated host-side (in `hooks`, which
    // owns the hook-entry machinery) and passed in. The sandbox layer writes
    // and mounts it but never depends on core to build it.
    let managed_settings = match (is_codex, managed_settings_content) {
        (false, Some(content)) => prepare_managed_settings(work, content),
        _ => None,
    };
    let full = build_run_argv(
        &rt,
        p,
        &policy.ro_binds,
        policy.cache_write.as_ref(),
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
        policy.env_capture_spool.as_deref(),
        policy.env_inbox.as_deref(),
        &policy.private_binds,
    );

    // Inherited stdio (the default). This is the interactive session. Secret
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
    // Snapshot the session's egress verdicts before the proxy handle drops.
    // This is the only channel the tally has back to the host.
    let egress = _proxy.as_ref().map(|h| h.egress_summary());
    Ok(InteractiveOutcome {
        exit_code: status.code().unwrap_or(130),
        egress,
    })
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
    let out_h = std::thread::spawn(move || crate::sandbox::drain_capped(&mut out_pipe));
    let err_h = std::thread::spawn(move || crate::sandbox::drain_capped(&mut err_pipe));

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

    /// Probe-cache round-trip: a written entry reads back as a Runtime, and any
    /// fingerprint drift (podman upgraded) or a non-rootless verdict rejects
    /// the cache instead of trusting it. Skips when podman isn't on PATH. The
    /// fingerprint (real binary identity) is what's under test.
    #[test]
    fn probe_cache_round_trips_and_rejects_stale_fingerprints() {
        let Some((path, mtime, size)) = podman_fingerprint() else {
            eprintln!("skipping: no podman on PATH to fingerprint");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("h5i").join("podman-probe.json");

        // Round-trip.
        write_probe_cache(&cache, &rt());
        let got = read_probe_cache(&cache).expect("fresh cache must hit");
        assert_eq!(got.bin, "podman");
        assert!(got.rootless);

        // Stale mtime (podman upgraded since the probe) → miss, not a wrong hit.
        let stale = serde_json::json!({
            "bin": "podman", "rootless": true,
            "podman_path": path, "podman_mtime": mtime + 1, "podman_size": size,
        });
        std::fs::write(&cache, stale.to_string()).unwrap();
        assert!(read_probe_cache(&cache).is_none(), "drifted fingerprint must re-probe");

        // A cached non-rootless verdict is never served (positive-only cache).
        let rootful = serde_json::json!({
            "bin": "podman", "rootless": false,
            "podman_path": path, "podman_mtime": mtime, "podman_size": size,
        });
        std::fs::write(&cache, rootful.to_string()).unwrap();
        assert!(read_probe_cache(&cache).is_none(), "non-rootless entry must be ignored");

        // Garbage/missing file → clean miss.
        std::fs::write(&cache, b"not json").unwrap();
        assert!(read_probe_cache(&cache).is_none());
        std::fs::remove_file(&cache).unwrap();
        assert!(read_probe_cache(&cache).is_none());
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
        ]).unwrap();
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
        let a = AllowList::parse(&[]).unwrap();
        assert!(!a.allows("anything.com", 443));
    }

    /// Build an allowlist with a hand-placed pin, so the pinning behaviour is
    /// testable without touching the network.
    fn pinned(rule: &str, ips: &[&str]) -> AllowList {
        let mut a = AllowList::parse(&[rule.to_string()]).unwrap();
        a.entries[0].pinned = ips.iter().map(|i| i.parse().unwrap()).collect();
        a
    }

    /// Matching by name and then re-resolving let an allowlisted domain under
    /// attacker DNS control point the proxy at anything. The proxy must dial
    /// the address the allowlist pinned.
    #[test]
    fn dial_uses_the_pinned_address_not_a_fresh_lookup() {
        let a = pinned("api.example.com", &["203.0.113.10", "203.0.113.11"]);
        let addrs = a.dial_addrs("api.example.com", 443);
        assert_eq!(addrs.len(), 2);
        assert!(addrs.iter().all(|x| x.ip().to_string().starts_with("203.0.113.")));
        assert!(addrs.iter().all(|x| x.port() == 443));
    }

    /// A pinned IP opens only the port its entry named. Ignoring the port here
    /// turned an allowlist of `host:443` into a free pass to `host:22`.
    #[test]
    fn a_pinned_ip_does_not_open_every_port() {
        let a = pinned("github.com:443", &["140.82.121.4"]);
        assert!(a.allows("140.82.121.4", 443));
        assert!(!a.allows("140.82.121.4", 22));
        // An address nobody pinned stays refused on any port.
        assert!(!a.allows("140.82.121.5", 443));
    }

    /// A wildcard cannot be pinned, so its lookup happens at CONNECT time,
    /// and that answer must not be allowed to name an internal address.
    #[test]
    fn unpinnable_destinations_refuse_internal_addresses() {
        for ip in [
            "127.0.0.1",
            "169.254.169.254", // cloud metadata
            "10.1.2.3",
            "192.168.1.1",
            "172.16.0.1",
            "100.64.0.1", // CGNAT
            "0.0.0.0",
            "::1",
            "fd00::1",   // unique-local
            "fe80::1",   // link-local
            "::ffff:127.0.0.1",
        ] {
            assert!(is_internal(&ip.parse().unwrap()), "{ip} should be internal");
        }
        for ip in ["93.184.216.34", "8.8.8.8", "2606:4700::1111"] {
            assert!(!is_internal(&ip.parse().unwrap()), "{ip} should be routable");
        }
    }

    /// A literal IP target is dialled verbatim: `allows` has already checked
    /// it against the pins, so there is nothing left to resolve.
    #[test]
    fn a_literal_ip_target_is_dialled_as_given() {
        let a = pinned("api.example.com", &["203.0.113.10"]);
        let addrs = a.dial_addrs("203.0.113.10", 443);
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0].to_string(), "203.0.113.10:443");
    }

    /// Podman applies `--env` in order and the proxy wiring is pushed first, so
    /// an `env.pass` entry naming one of those variables would replace h5i's
    /// value with the host's and route the box around the allowlist.
    #[test]
    fn env_pass_cannot_shadow_the_egress_proxy_wiring() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = Profile::builtin("default", crate::sandbox_policy::IsolationClaim::Container);
        p.env_pass = vec!["HTTPS_PROXY".into(), "no_proxy".into(), "PATH".into()];
        // Safety: single-threaded test; the vars are read back immediately.
        unsafe {
            std::env::set_var("HTTPS_PROXY", "http://attacker.invalid:3128");
            std::env::set_var("no_proxy", "*");
        }
        let argv = build_run_argv(
            &rt(), &p, &[], None, tmp.path(), "img", "n", &NetPlan::Proxy(9999),
            &["true".into()], &[], None, None, &[], None, None, None, &[],
        );
        // The wiring h5i pushed is present exactly once and is never re-passed
        // by name (a name-only `--env HTTPS_PROXY` would take the host value).
        assert!(argv.iter().any(|a| a == "HTTPS_PROXY=http://10.0.2.2:9999"
            || a.starts_with("HTTPS_PROXY=http://")));
        assert!(!argv.iter().any(|a| a == "HTTPS_PROXY"), "{argv:?}");
        assert!(!argv.iter().any(|a| a == "no_proxy"), "{argv:?}");
        // Unrelated allowlist entries still pass.
        assert!(argv.iter().any(|a| a == "PATH"));
        unsafe {
            std::env::remove_var("HTTPS_PROXY");
            std::env::remove_var("no_proxy");
        }
    }

    /// The container tier used to widen or mangle four inputs the microvm tier
    /// already refused. One parser now, so they cannot disagree again.
    #[test]
    fn the_egress_grammar_is_fail_closed_for_both_tiers() {
        // An out-of-range port became an ANY-port rule. The opposite of the ask.
        for bad in [
            "example.com:99999",
            "*.com",                  // a whole TLD
            "2001:db8::1",            // mangled into host "2001:db8:" port 1
            "a,b.example.com",        // reserved separator
            "user@example.com",
            // Not a host. The colon that is not a port stayed part of the host,
            // so the container tier held an entry that matches nothing while the
            // microvm tier emitted `allow@example.com:any`: where that colon is
            // microsandbox's protocol qualifier. One policy, two meanings.
            "example.com:any",
            "example.com:tcp",
            "exa mple.com",
            "https://example.com",
            "example.com/path",
            "example.com.",           // trims to a rule that never fires
            "..example.com",
            "-",
        ] {
            assert!(
                AllowList::parse(&[bad.to_string()]).is_err(),
                "container tier accepted {bad:?}"
            );
            assert!(
                crate::microvm::egress_rule_tokens(&[bad.to_string()]).is_err(),
                "microvm tier accepted {bad:?}"
            );
        }
        // And the well-formed shapes still parse on both.
        for good in [
            "example.com",
            "example.com:443",
            ".example.com",
            "*.example.com",
            "localhost",
            "1.2.3.4",
            "1.2.3.4:8080",
            "10.0.0.0/8",
            "_dnslink.example.com",
            "my-host.example.com",
        ] {
            assert!(AllowList::parse(&[good.to_string()]).is_ok(), "{good}");
            assert!(crate::microvm::egress_rule_tokens(&[good.to_string()]).is_ok(), "{good}");
        }
    }

    /// An operator may list a literal address. That entry must keep matching
    /// itself even though nothing pinned it (the proxy's relay tests dial a
    /// local upstream this way).
    #[test]
    fn a_literal_ip_entry_still_matches_itself() {
        let a = AllowList::parse(&["127.0.0.1:8080".into()]).unwrap();
        assert!(a.allows("127.0.0.1", 8080));
        assert!(!a.allows("127.0.0.1", 9090), "the port on the entry still binds");
        assert_eq!(a.dial_addrs("127.0.0.1", 8080)[0].to_string(), "127.0.0.1:8080");
    }

    /// Wildcard matching stays anchored on a label boundary.
    #[test]
    fn wildcard_matching_is_label_anchored() {
        let a = AllowList::parse(&[".githubusercontent.com".into()]).unwrap();
        assert!(a.allows("raw.githubusercontent.com", 443));
        assert!(a.allows("githubusercontent.com", 443));
        assert!(!a.allows("evilgithubusercontent.com", 443));
    }

    #[test]
    fn effective_egress_merges_user_extras_onto_profile() {
        let merged = effective_egress(
            &["api.anthropic.com".into(), "crates.io".into()],
            &["pypi.org".into(), "crates.io".into(), "CRATES.IO".into()],
        );
        // Profile order first, extras appended, case-insensitive dedupe.
        assert_eq!(merged, vec!["api.anthropic.com", "crates.io", "pypi.org"]);
    }

    #[test]
    fn effective_egress_never_widens_a_deny_all_profile() {
        // The fail-closed widening rule: user extras must not turn a profile
        // with no net.egress (deny-all) into a networked one.
        assert!(effective_egress(&[], &["pypi.org".into()]).is_empty());
        assert!(effective_egress(&["  ".into()], &["pypi.org".into()]).is_empty());
    }

    #[test]
    fn allowlist_does_not_treat_ipv6_as_port() {
        // A bare IPv6-ish string must not be mis-split on its colons.
        let a = AllowList::parse(&["example.org".into()]).unwrap();
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
        let mut p = Profile::builtin("default", crate::sandbox_policy::IsolationClaim::Container);
        p.mem_bytes = Some(2 * 1024 * 1024 * 1024);
        p.max_procs = Some(128);
        let argv = build_run_argv(
            &rt(),
            &p,
            &[],
            None,
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
            None,
            None,
            &[],
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
        // Canonicalized: the mount source is the resolved path, so the test has
        // to agree on hosts where the temp root is itself a symlink.
        let work = &tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(work.join(".claude")).unwrap();
        std::fs::create_dir_all(work.join(".codex")).unwrap();
        std::fs::write(work.join(".claude/settings.json"), "{}").unwrap();
        std::fs::write(work.join(".codex/config.toml"), "").unwrap();

        let p = Profile::builtin("default", crate::sandbox_policy::IsolationClaim::Container);
        let argv = build_run_argv(
            &rt(),
            &p,
            &[],
            None,
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
            None,
            None,
            &[],
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

    /// A branch under review can ship `.claude/settings.json` as a symlink.
    /// `exists()` follows it and the unresolved path is what the kernel would
    /// mount, so h5i would bind the link's target, a host path the box has no
    /// grant on, into `/work`. Refuse the mount instead.
    #[test]
    fn run_argv_refuses_a_symlinked_agent_config() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let work = root.join("work");
        let secret_dir = root.join("secrets");
        std::fs::create_dir_all(work.join(".claude")).unwrap();
        std::fs::create_dir_all(&secret_dir).unwrap();
        std::fs::write(secret_dir.join("id_rsa"), "PRIVATE KEY").unwrap();

        // (a) the file itself is a symlink out of $WORK
        std::os::unix::fs::symlink(secret_dir.join("id_rsa"), work.join(".claude/settings.json"))
            .unwrap();
        assert_eq!(agent_config_mount_source(&work, ".claude/settings.json"), None);

        // (b) a real file, but reached through a symlinked ancestor
        std::fs::create_dir_all(root.join("elsewhere/.codex")).unwrap();
        std::fs::write(root.join("elsewhere/.codex/config.toml"), "").unwrap();
        std::os::unix::fs::symlink(root.join("elsewhere/.codex"), work.join(".codex")).unwrap();
        assert_eq!(agent_config_mount_source(&work, ".codex/config.toml"), None);

        // Neither reaches the argv.
        let p = Profile::builtin("default", crate::sandbox_policy::IsolationClaim::Container);
        let argv = build_run_argv(
            &rt(), &p, &[], None, &work, "img", "n", &NetPlan::None,
            &["true".into()], &[], None, None, &[], None, None, None, &[],
        );
        let joined = argv.join(" ");
        assert!(!joined.contains("id_rsa"), "leaked the symlink target: {joined}");
        assert!(!joined.contains("elsewhere"), "leaked through a symlinked parent: {joined}");
        assert!(!joined.contains("target=/work/.claude/settings.json"));
        assert!(!joined.contains("target=/work/.codex/config.toml"));

        // A real regular file inside $WORK still mounts.
        std::fs::remove_file(work.join(".claude/settings.json")).unwrap();
        std::fs::write(work.join(".claude/settings.json"), "{}").unwrap();
        assert_eq!(
            agent_config_mount_source(&work, ".claude/settings.json"),
            Some(work.join(".claude/settings.json")),
        );
    }

    // In-box git plumbing mounts: identical source/target host paths, ro/rw
    // honored, list order preserved (nested binds need their parent first),
    // and a comma in ANY path skips the whole set (Podman's `--mount` syntax
    // cannot carry it; a partially mounted .git would be worse than none).
    #[test]
    fn run_argv_mounts_box_git_plumbing_at_identical_paths() {
        use crate::sandbox_policy::BoxGitPath;
        let p = Profile::builtin("default", crate::sandbox_policy::IsolationClaim::Container);
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
            &[],
            None,
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
            None,
            None,
            &[],
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
            &[],
            None,
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
            None,
            None,
            &[],
        );
        let joined = argv.join(" ");
        assert!(
            !joined.contains("target=/repo/.git/objects"),
            "comma path must disable the whole box-git mount set: {joined}"
        );
    }

    #[test]
    fn run_argv_mounts_private_paths_rw_under_work() {
        let p = Profile::builtin("default", crate::sandbox_policy::IsolationClaim::Container);
        let binds = vec![
            crate::sandbox_policy::PrivateBind {
                backing: "/envdir/private/target".into(),
                rel: "target".into(),
            },
            crate::sandbox_policy::PrivateBind {
                backing: "/envdir/private/.next".into(),
                rel: ".next".into(),
            },
        ];
        let argv = build_run_argv(
            &rt(),
            &p,
            &[],
            None,
            Path::new("/work/dir"),
            "img",
            "n",
            &NetPlan::None,
            &["sh".into()],
            &[],
            None,
            None,
            &[],
            None,
            None,
            None,
            &binds,
        );
        let joined = argv.join(" ");
        // Each per-env backing dir is bound rw at its workspace-relative path.
        assert!(joined.contains("type=bind,source=/envdir/private/target,target=/work/target,rw"));
        assert!(joined.contains("type=bind,source=/envdir/private/.next,target=/work/.next,rw"));
        // Comma-bearing paths are rejected upstream (validate_private_paths /
        // prepare_private_paths fail closed), so build_run_argv only ever sees
        // safe binds and emits every one, no silent fail-open skip here.
    }

    // Managed-settings injection: when present, a read-only bind mount lands at
    // the Claude managed-settings path; when absent, no such mount appears.
    #[test]
    fn run_argv_mounts_managed_settings_read_only() {
        let p = Profile::builtin("default", crate::sandbox_policy::IsolationClaim::Container);
        let ms = Path::new("/env/managed/managed-settings.json");
        let with = build_run_argv(
            &rt(),
            &p,
            &[],
            None,
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
            None,
            None,
            &[],
        );
        let joined = with.join(" ");
        assert!(
            joined.contains(&format!(
                "type=bind,source=/env/managed/managed-settings.json,target={},ro",
                crate::sandbox_policy::CLAUDE_MANAGED_SETTINGS_PATH
            )),
            "expected ro managed-settings mount: {joined}"
        );

        // None → no managed-settings mount at all.
        let without = build_run_argv(
            &rt(),
            &p,
            &[],
            None,
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
            None,
            None,
            &[],
        );
        assert!(
            !without.join(" ").contains(crate::sandbox_policy::CLAUDE_MANAGED_SETTINGS_PATH),
            "no managed-settings mount when not requested"
        );
    }

    // The Codex agent profile is detected as Codex (so the Claude-specific
    // managed-settings injection is skipped for it); default/custom are not.
    #[test]
    fn codex_profile_is_detected_for_managed_settings_gating() {
        use crate::sandbox_policy::AgentRuntime;
        assert_eq!(AgentRuntime::from_profile_name("agent-codex"), Some(AgentRuntime::Codex));
        assert_eq!(AgentRuntime::from_profile_name("agent-claude"), Some(AgentRuntime::Claude));
        assert_eq!(AgentRuntime::from_profile_name("default"), None);
    }

    #[test]
    fn run_argv_proxy_mode_sets_proxy_env() {
        let p = Profile::builtin("default", crate::sandbox_policy::IsolationClaim::Container);
        let argv = build_run_argv(
            &rt(),
            &p,
            &[],
            None,
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
            None,
            None,
            &[],
        );
        let joined = argv.join(" ");
        // The route to h5i's host-side proxy is per-platform ([`HOST_ROUTE`]):
        // one hop on Linux (slirp gateway), two on macOS (through the podman
        // machine VM to gvproxy). Assert against the platform's own route so
        // this stays a real check on both rather than a Linux-only one.
        let addr = HOST_ROUTE.host_addr;
        match HOST_ROUTE.network_arg {
            Some(mode) => assert!(joined.contains(&format!("--network={mode}")), "{joined}"),
            None => assert!(!joined.contains("--network="), "{joined}"),
        }
        assert!(joined.contains(&format!("HTTP_PROXY=http://{addr}:8123")));
        assert!(joined.contains(&format!("HTTPS_PROXY=http://{addr}:8123")));
        // h5i's own name for the same address, which the browser shim keys off
        // (the conventional vars are box-settable, so it cannot read those).
        assert!(joined.contains(&format!("{EGRESS_PROXY_VAR}=http://{addr}:8123")));
        assert!(joined.contains(&format!("NO_PROXY=localhost,127.0.0.1,{addr}")));
    }

    #[test]
    fn host_route_is_a_real_address_on_every_platform() {
        // A blank address would silently produce `http://:1234` and the box
        // would lose egress with no explanation.
        assert!(!HOST_ROUTE.host_addr.is_empty());
        assert!(!HOST_ROUTE.host_addr.contains(' '));
    }

    #[test]
    fn run_argv_interactive_adds_i_and_optional_t() {
        let p = Profile::builtin("default", crate::sandbox_policy::IsolationClaim::Container);
        let mk = |tty| {
            build_run_argv(
                &rt(),
                &p,
                &[],
                None,
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
                None,
                None,
                &[],
            )
        };
        // Capture run: no interactive flags at all.
        let cap = mk(None);
        assert!(!cap.contains(&"-i".to_string()) && !cap.contains(&"-t".to_string()));
        // Interactive, no TTY (piped/CI): `-i` only. Never `-t` (Podman rejects
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
        let p = Profile::builtin("default", crate::sandbox_policy::IsolationClaim::Container);
        let plan = ShimPlan {
            shim: "/envdir/shim/sh".into(),
            spool: "/envdir/spool".into(),
        };
        let argv = build_run_argv(
            &rt(),
            &p,
            &[],
            None,
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
            None,
            None,
            &[],
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
            &[],
            None,
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
            None,
            None,
            &[],
        );
        assert!(!plain.join(" ").contains("/.h5i/"));
    }

    #[test]
    fn run_argv_mounts_env_capture_spool_without_shim() {
        let p = Profile::builtin("default", crate::sandbox_policy::IsolationClaim::Container);
        let argv = build_run_argv(
            &rt(),
            &p,
            &[],
            None,
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
            Some(Path::new("/envdir/spool")),
            None,
            &[],
        );
        let joined = argv.join(" ");
        assert!(joined.contains("type=bind,source=/envdir/spool,target=/.h5i/spool,rw"));
        assert!(!joined.contains("target=/bin/sh,ro"));
        assert!(!joined.contains("target=/bin/bash,ro"));
    }

    #[test]
    fn run_argv_mounts_env_inbox_read_only() {
        let p = Profile::builtin("default", crate::sandbox_policy::IsolationClaim::Container);
        let argv = build_run_argv(
            &rt(),
            &p,
            &[],
            None,
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
            None,
            Some(Path::new("/envdir/inbox")),
            &[],
        );
        let joined = argv.join(" ");
        // Read-only: the box receives messages but cannot write the mailbox.
        assert!(joined.contains("type=bind,source=/envdir/inbox,target=/.h5i/inbox,ro"));
    }

    // Live on-host: the generated shim is real POSIX sh. Run it against the
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

        // Settle a possible ETXTBSY before the assertions start.
        //
        // We just wrote a file and are about to exec it. `cargo test` runs the
        // other tests in this binary on sibling threads, and any of them that
        // spawns a process forks a child which inherits every open descriptor,
        // including, if the fork lands in the window while `fs::write` still
        // holds it, the write handle to this very file. Linux then refuses to
        // exec it with ETXTBSY until that child reaches its own exec, a window
        // of milliseconds. That made this test fail on CI for a reason that has
        // nothing to do with the shim.
        //
        // `H5I_SHIM=1` marks the run as nested, which the shim passes through
        // unobserved, so this warm-up writes no spool entry and the spool
        // assertions below still count only what they mean to.
        for attempt in 0..100 {
            match std::process::Command::new(&shim)
                .args(["-c", ":"])
                .env("H5I_SHIM", "1")
                .output()
            {
                Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => {
                    assert!(attempt < 99, "shim stayed ETXTBSY for 2s");
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                _ => break,
            }
        }

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
    // the shim *observes* (writes a `.cmd` for) and the command it extracted,
    // not the output, so the dummy commands "failing" under the real shell is
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

        // Observed. Every command-flag spelling is detected, command extracted.
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

        // Not observed, no command flag, or it's a nested/script invocation.
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
        // captures them via `h5i capture run`), no double-capture, no overhead.
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
        let p = Profile::builtin("default", crate::sandbox_policy::IsolationClaim::Container);
        let injected = vec![("GITHUB_TOKEN".to_string(), "ghp_secret".to_string())];
        let argv = build_run_argv(
            &rt(),
            &p,
            &[],
            None,
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
            None,
            None,
            &[],
        );
        // The broker's env grant is passed to the container by NAME only. The
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
        let allow = AllowList::parse(&["allowed.invalid:443".into()]).unwrap();
        let proxy = spawn_proxy(allow).unwrap();

        // A connect-and-reset used to end the accept loop for good, and this
        // proxy is the box's only route out, so any local process could take
        // the box's network away with one socket. Do it several times, then
        // prove the proxy is still answering.
        #[cfg(unix)]
        for _ in 0..8 {
            use std::os::fd::AsRawFd;
            if let Ok(s) = TcpStream::connect(("127.0.0.1", proxy.port)) {
                // `SO_LINGER` with a zero timeout makes `close` send an RST
                // rather than a FIN, which is what produces `ECONNABORTED` on
                // the accepting side. (`TcpStream::set_linger` is still
                // unstable, so this goes through libc.)
                let l = libc::linger { l_onoff: 1, l_linger: 0 };
                unsafe {
                    libc::setsockopt(
                        s.as_raw_fd(),
                        libc::SOL_SOCKET,
                        libc::SO_LINGER,
                        (&raw const l).cast(),
                        std::mem::size_of::<libc::linger>() as libc::socklen_t,
                    );
                }
            }
        }

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
        // `allowed.invalid` doesn't resolve, we get 502 (not 403). Proving the
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

    /// Bytes actually cross an established tunnel. The half the allow/deny test
    /// above never reaches, because 403 and 502 both return before the relay.
    ///
    /// It is worth a test of its own: the accept loop polls a non-blocking
    /// listener, and on macOS the accepted socket inherits that flag, which made
    /// the relay read `WouldBlock`, treat it as fatal and drop every tunnel the
    /// moment it carried traffic. The gate verdicts stayed correct throughout,
    /// so nothing here failed. The box just lost every connection mid-TLS.
    /// Local origin only: no network, no DNS.
    #[test]
    fn proxy_relays_bytes_over_an_established_tunnel() {
        use std::io::Read;

        let origin = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin_port = origin.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut s, _) = origin.accept().unwrap();
            let mut buf = [0u8; 4];
            s.read_exact(&mut buf).unwrap();
            assert_eq!(&buf, b"PING");
            s.write_all(b"PONG").unwrap();
        });

        let allow = AllowList::parse(&[format!("127.0.0.1:{origin_port}")]).unwrap();
        let proxy = spawn_proxy(allow).unwrap();
        let mut c = TcpStream::connect(("127.0.0.1", proxy.port)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c.write_all(format!("CONNECT 127.0.0.1:{origin_port} HTTP/1.1\r\n\r\n").as_bytes())
            .unwrap();

        // Read exactly the response head, so the origin's bytes stay in the
        // stream for the assertion below.
        let mut head = Vec::new();
        let mut b = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            assert_eq!(c.read(&mut b).unwrap(), 1, "tunnel closed during CONNECT");
            head.push(b[0]);
        }
        assert!(
            String::from_utf8_lossy(&head).contains("200"),
            "expected an established tunnel, got: {}",
            String::from_utf8_lossy(&head)
        );

        c.write_all(b"PING").unwrap();
        let mut echo = [0u8; 4];
        c.read_exact(&mut echo)
            .expect("the tunnel must carry bytes both ways once established");
        assert_eq!(&echo, b"PONG");
    }

    /// An established tunnel has no read timeout of its own. The head timeout is
    /// set on the client socket, `splice` gives the client→upstream copy a
    /// `try_clone` of it, and a clone shares `SO_RCVTIMEO`, so leaving it set
    /// ended that copy (and shut the upstream write half) after a plain silence
    /// the length of the timeout. A tunnel waiting for a response *is* silent in
    /// that direction, so every slow first byte died mid-flight.
    ///
    /// Driven through `handle_proxy_client` directly with a short head timeout,
    /// so the idle stretch is milliseconds rather than the real 30 seconds.
    #[test]
    fn an_established_tunnel_survives_a_client_silence_longer_than_the_head_timeout() {
        use std::io::Read;

        let head_timeout = Duration::from_millis(150);

        // Origin: replies only after the client's post-idle byte arrives.
        let origin = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin_port = origin.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut s, _) = origin.accept().unwrap();
            let mut buf = [0u8; 4];
            s.read_exact(&mut buf).unwrap();
            s.write_all(b"PONG").unwrap();
        });

        // One connection into `handle_proxy_client`, no accept loop involved.
        let front = TcpListener::bind("127.0.0.1:0").unwrap();
        let front_port = front.local_addr().unwrap().port();
        let allow = AllowList::parse(&[format!("127.0.0.1:{origin_port}")]).unwrap();
        std::thread::spawn(move || {
            let (s, _) = front.accept().unwrap();
            let tally = Arc::new(Mutex::new(EgressTally::default()));
            let _ = handle_proxy_client(s, &allow, &tally, head_timeout);
        });

        let mut c = TcpStream::connect(("127.0.0.1", front_port)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c.write_all(format!("CONNECT 127.0.0.1:{origin_port} HTTP/1.1\r\n\r\n").as_bytes())
            .unwrap();
        let mut head = Vec::new();
        let mut b = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            assert_eq!(c.read(&mut b).unwrap(), 1, "tunnel closed during CONNECT");
            head.push(b[0]);
        }
        assert!(String::from_utf8_lossy(&head).contains("200"));

        // The client says nothing for longer than the head timeout. Exactly
        // what waiting on a slow first byte looks like from the proxy's side.
        std::thread::sleep(head_timeout * 4);

        c.write_all(b"PING")
            .expect("the tunnel must still accept bytes after an idle stretch");
        let mut echo = [0u8; 4];
        c.read_exact(&mut echo)
            .expect("the tunnel must still carry the response after an idle stretch");
        assert_eq!(&echo, b"PONG");
    }

    /// The relay must work on a socket that arrives non-blocking.
    ///
    /// That is what macOS hands the accept loop (BSD `accept` inherits the
    /// listener's flag; Linux's `accept4` does not), and it made every tunnel
    /// reset the moment it carried traffic. A `WouldBlock` the relay treated as
    /// fatal. Driven through `handle_proxy_client` with the flag set
    /// deliberately, so the Darwin condition is reproduced on every platform
    /// rather than tested only where the OS happens to create it.
    #[test]
    fn a_socket_that_arrives_non_blocking_still_relays() {
        use std::io::Read;

        let origin = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin_port = origin.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut s, _) = origin.accept().unwrap();
            let mut buf = [0u8; 4];
            s.read_exact(&mut buf).unwrap();
            s.write_all(b"PONG").unwrap();
        });

        let front = TcpListener::bind("127.0.0.1:0").unwrap();
        let front_port = front.local_addr().unwrap().port();
        let allow = AllowList::parse(&[format!("127.0.0.1:{origin_port}")]).unwrap();
        std::thread::spawn(move || {
            let (s, _) = front.accept().unwrap();
            s.set_nonblocking(true).unwrap();
            let tally = Arc::new(Mutex::new(EgressTally::default()));
            let _ = handle_proxy_client(s, &allow, &tally, Duration::from_secs(5));
        });

        let mut c = TcpStream::connect(("127.0.0.1", front_port)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        // Nothing buffered when the handler makes its first read: without the
        // correction that read returns `WouldBlock` and the connection dies.
        std::thread::sleep(Duration::from_millis(100));
        c.write_all(format!("CONNECT 127.0.0.1:{origin_port} HTTP/1.1\r\n\r\n").as_bytes())
            .unwrap();
        let mut head = Vec::new();
        let mut b = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            assert_eq!(
                c.read(&mut b).unwrap(),
                1,
                "tunnel closed during CONNECT (head so far: {})",
                String::from_utf8_lossy(&head)
            );
            head.push(b[0]);
        }
        assert!(String::from_utf8_lossy(&head).contains("200"));

        c.write_all(b"PING").unwrap();
        let mut echo = [0u8; 4];
        c.read_exact(&mut echo)
            .expect("a non-blocking socket must still carry the tunnel");
        assert_eq!(&echo, b"PONG");
    }

    /// A browser that outlives the run memorised one proxy address, so the port
    /// has to be the caller's to choose.
    ///
    /// The "free port" is found by bind-read-drop, so another test in this
    /// binary can take it in the gap. That is a lost race, not a bug. Retried
    /// rather than asserted once, so a green run stays green. The fallback half
    /// has no such window (the port is held for the duration) and prints the
    /// ephemeral-fallback note on success; that stderr line is expected here.
    #[test]
    fn proxy_binds_the_requested_port_and_falls_back_when_it_cannot() {
        let allow = || AllowList::parse(&["allowed.invalid".into()]).unwrap();

        let mut honoured = None;
        for _ in 0..5 {
            let want = {
                // A port we know is free right now: bind, read it, drop.
                let l = TcpListener::bind("127.0.0.1:0").unwrap();
                l.local_addr().unwrap().port()
            };
            let p = spawn_proxy_on(allow(), Some(want)).unwrap();
            if p.port == want {
                honoured = Some((p, want));
                break;
            }
        }
        let Some((_held, want)) = honoured else {
            panic!("pinned port was never honoured in 5 attempts");
        };

        // Same port again, while the first proxy holds it: the run continues on
        // an ephemeral port rather than failing.
        let p2 = spawn_proxy_on(allow(), Some(want)).unwrap();
        assert_ne!(p2.port, want);
        assert_ne!(p2.port, 0);
    }
}
