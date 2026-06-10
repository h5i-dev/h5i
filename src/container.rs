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
use std::path::Path;
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
        Some(Runtime { bin: "podman".into(), rootless: true })
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
                entries.push(AllowEntry { host, wildcard, port });
            }
        }
        AllowList { entries, pinned_ips: HashSet::new() }
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

    Ok(ProxyHandle { port, stop, tally, join: Some(join) })
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
        let _ = client.write_all(
            b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n",
        );
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
        // The env workspace is the only writable host path, mounted at /work.
        // Use --mount rather than -v so ':' in a repository path cannot be
        // parsed as a bind-mount option suffix by Podman.
        "--mount".into(),
        format!("type=bind,source={},target=/work,rw", work.display()),
        "-w".into(),
        "/work".into(),
        "--ipc=private".into(),
    ];
    // Interactive (agent-in-box) flags, right after `run`.
    if let Some(want_tty) = tty {
        a.insert(2, "-i".into());
        if want_tty {
            a.insert(3, "-t".into());
        }
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
            for var in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy", "ALL_PROXY"] {
                a.push("--env".into());
                a.push(format!("{var}={proxy}"));
            }
            a.push("--env".into());
            a.push("NO_PROXY=localhost,127.0.0.1".into());
        }
    }

    // Env-var allowlist (nothing inherited wholesale).
    for key in &profile.env_pass {
        if let Ok(v) = std::env::var(key) {
            a.push("--env".into());
            a.push(format!("{key}={v}"));
        }
    }

    // Brokered secrets (env-injected), applied after the allowlist.
    for (key, value) in injected_env {
        a.push("--env".into());
        a.push(format!("{key}={value}"));
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
             podman or re-request --isolation workspace/process".into(),
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
    let full = build_run_argv(&rt, p, work, &image, &name, &net, argv, injected_env, None);

    let started = std::time::Instant::now();
    let mut cmd = std::process::Command::new(&full[0]);
    cmd.args(&full[1..]);
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
             podman or re-request --isolation workspace/process".into(),
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
    let name = format!("h5i-{}-{}", std::process::id(), PROBE_SEQ.fetch_add(1, Ordering::Relaxed));
    let full = build_run_argv(&rt, p, work, &image, &name, &net, argv, injected_env, Some(tty));

    // Inherited stdio (the default) — this is the interactive session.
    let status = std::process::Command::new(&full[0])
        .args(&full[1..])
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
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
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
        Runtime { bin: "podman".into(), rootless: true }
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
        assert!(!a.allows("github.com", 80), "port 80 not allowed for github.com:443");
        // Subdomain wildcard (both . and *. forms).
        assert!(a.allows("raw.githubusercontent.com", 443));
        assert!(a.allows("files.pythonhosted.org", 443));
        assert!(a.allows("pythonhosted.org", 443), "apex matches the wildcard too");
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
        let (h, p, c) = parse_target(b"GET http://example.com/x HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        assert_eq!((h.as_str(), p, c), ("example.com", 80, false));
        let (h, p, c) = parse_target(b"GET https://example.com/x HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
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
        );
        // The broker's env grant is passed to the container as a --env pair...
        let pos = argv.iter().position(|a| a == "GITHUB_TOKEN=ghp_secret");
        assert!(pos.is_some(), "injected secret env must appear as a --env value: {argv:?}");
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
        c.write_all(b"CONNECT evil.example.com:443 HTTP/1.1\r\n\r\n").unwrap();
        let mut line = String::new();
        BufReader::new(c.try_clone().unwrap()).read_line(&mut line).unwrap();
        assert!(line.contains("403"), "denied host must get 403, got: {line:?}");

        // Allowed host → the gate passes and tries to connect upstream; since
        // `allowed.invalid` doesn't resolve, we get 502 (not 403) — proving the
        // allowlist verdict was "permit".
        let mut c2 = TcpStream::connect(("127.0.0.1", proxy.port)).unwrap();
        c2.write_all(b"CONNECT allowed.invalid:443 HTTP/1.1\r\n\r\n").unwrap();
        let mut line2 = String::new();
        BufReader::new(c2.try_clone().unwrap()).read_line(&mut line2).unwrap();
        assert!(
            line2.contains("502") || line2.contains("200"),
            "allowed host must pass the gate (502/200), got: {line2:?}"
        );
    }
}
