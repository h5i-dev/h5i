//! `isolation=supervised` — the seccomp-notify supervisor tier
//! (`docs/supervisor-design.md`).
//!
//! This is the security keystone: the first tier that may claim untrusted-code
//! containment. Its defining property — implemented and tested here in phase A —
//! is **fail-closed admission**: the claim is satisfiable only when *every*
//! component probes green, and is otherwise **refused**, never downgraded to a
//! weaker tier. A half-present stack is a refusal, not a "best-effort pass".
//!
//! Phase A (this module): the honest [`probe`], the pure syscall-decision model,
//! and a fail-closed [`run`] (enforcement wiring is phase B). Because the full
//! stack does not probe green on current hosts (WSL2/CI lack cgroup delegation
//! and rootless nftables), the tier correctly refuses everywhere today.

use crate::error::H5iError;

/// One component of the supervised stack and whether the host provides it.
#[derive(Debug, Clone)]
pub struct ComponentStatus {
    pub name: &'static str,
    pub ok: bool,
    pub detail: Option<String>,
}

/// Host readiness for `isolation=supervised`. `usable` is true only when every
/// required component is `ok` — the single source of truth `resolve` consults.
#[derive(Debug, Clone)]
pub struct SupervisorCaps {
    pub usable: bool,
    pub components: Vec<ComponentStatus>,
}

impl SupervisorCaps {
    /// Human-readable list of what's missing (for the refusal message / UI).
    pub fn missing(&self) -> Vec<String> {
        self.components
            .iter()
            .filter(|c| !c.ok)
            .map(|c| match &c.detail {
                Some(d) => format!("{}: {d}", c.name),
                None => c.name.to_string(),
            })
            .collect()
    }
}

/// Probe every component of the supervised stack. Fail-closed: anything we
/// cannot positively confirm is reported `ok = false` (the tier then refuses).
#[cfg(target_os = "linux")]
pub fn probe() -> SupervisorCaps {
    let host = crate::sandbox::probe_host();
    let cg = crate::cgroup::probe();

    let mut components = Vec::new();
    let mut add = |name: &'static str, ok: bool, detail: Option<String>| {
        components.push(ComponentStatus { name, ok, detail });
    };

    add(
        "user-namespace",
        host.userns,
        (!host.userns).then(|| "unprivileged userns unavailable (AppArmor/WSL2)".into()),
    );
    // A netns is created via unshare(NEWNET) inside our userns — functionally probed.
    let netns = host.userns && can_unshare_netns();
    add("network-namespace", netns, (!netns).then(|| "cannot unshare NEWNET".into()));
    // nftables is the airtight L3/L4 egress guard; we need the binary AND
    // (phase B) usability inside the child netns. Phase A checks the binary.
    let nft = nft_present();
    add("nftables", nft, (!nft).then(|| "`nft` binary not found on PATH".into()));
    let notify = seccomp_notify_supported();
    add(
        "seccomp-user-notif",
        notify,
        (!notify).then(|| "kernel lacks SECCOMP_FILTER_FLAG_NEW_LISTENER".into()),
    );
    add(
        "landlock",
        host.landlock_abi.is_some(),
        host.landlock_abi.is_none().then(|| "Landlock LSM unavailable".into()),
    );
    add("seccomp-bpf", host.seccomp, (!host.seccomp).then(|| "seccomp-bpf unavailable".into()));
    add(
        "cgroup-v2-delegation",
        cg.usable,
        (!cg.usable).then(|| cg.detail.unwrap_or_else(|| "no delegated cgroup".into())),
    );
    // no_new_privs + cap-drop are always achievable on Linux via prctl.
    add("no-new-privs+cap-drop", true, None);

    let usable = components.iter().all(|c| c.ok);
    SupervisorCaps { usable, components }
}

#[cfg(not(target_os = "linux"))]
pub fn probe() -> SupervisorCaps {
    SupervisorCaps {
        usable: false,
        components: vec![ComponentStatus {
            name: "platform",
            ok: false,
            detail: Some("isolation=supervised is Linux-only".into()),
        }],
    }
}

/// Functionally test that we can create a network namespace (in a child, so
/// h5i's own namespaces are untouched). Fail-closed on any error.
#[cfg(target_os = "linux")]
fn can_unshare_netns() -> bool {
    // SAFETY: fork + unshare in the child only; child exits immediately.
    unsafe {
        let pid = libc::fork();
        if pid == 0 {
            // Child: a userns first (for unprivileged NEWNET), then NEWNET.
            let rc = libc::unshare(libc::CLONE_NEWUSER);
            let rc2 = if rc == 0 { libc::unshare(libc::CLONE_NEWNET) } else { rc };
            libc::_exit(if rc2 == 0 { 0 } else { 1 });
        }
        if pid < 0 {
            return false;
        }
        let mut status = 0;
        libc::waitpid(pid, &mut status, 0);
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0
    }
}

/// Is the `nft` binary available? (Phase B additionally verifies it works inside
/// the child netns.)
#[cfg(target_os = "linux")]
fn nft_present() -> bool {
    std::process::Command::new("nft")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Functionally test seccomp user-notification by installing a minimal
/// "allow-all" filter with `SECCOMP_FILTER_FLAG_NEW_LISTENER` in a forked child;
/// success yields a listener fd. The child exits without affecting h5i.
#[cfg(target_os = "linux")]
fn seccomp_notify_supported() -> bool {
    const SECCOMP_SET_MODE_FILTER: libc::c_uint = 1;
    const SECCOMP_FILTER_FLAG_NEW_LISTENER: libc::c_ulong = 1 << 3;
    const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
    // struct sock_filter { u16 code; u8 jt; u8 jf; u32 k; }
    #[repr(C)]
    struct SockFilter {
        code: u16,
        jt: u8,
        jf: u8,
        k: u32,
    }
    #[repr(C)]
    struct SockFprog {
        len: u16,
        filter: *const SockFilter,
    }
    // BPF_RET (0x06) | BPF_K (0x00) → return SECCOMP_RET_ALLOW
    let insns = [SockFilter { code: 0x06, jt: 0, jf: 0, k: SECCOMP_RET_ALLOW }];
    let prog = SockFprog { len: 1, filter: insns.as_ptr() };

    // SAFETY: all effects (no_new_privs, seccomp filter) are confined to the
    // forked child, which exits immediately with the result.
    unsafe {
        let pid = libc::fork();
        if pid == 0 {
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                libc::_exit(1);
            }
            let fd = libc::syscall(
                libc::SYS_seccomp,
                SECCOMP_SET_MODE_FILTER,
                SECCOMP_FILTER_FLAG_NEW_LISTENER,
                &prog as *const SockFprog,
            );
            libc::_exit(if fd >= 0 { 0 } else { 1 });
        }
        if pid < 0 {
            return false;
        }
        let mut status = 0;
        libc::waitpid(pid, &mut status, 0);
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0
    }
}

// ─── pure syscall-decision model (phase B uses this in the notify loop) ───────

/// What the supervisor does with an intercepted syscall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Let the kernel run the original syscall unmediated (safe only when the
    /// real guard — nftables / Landlock — is the enforcement layer). For sockets
    /// this is the common path: the packet still hits nftables.
    Continue,
    /// Refuse with `errno` (no pointer deref → no TOCTOU). Used for dangerous
    /// shapes the netns/Landlock layers don't already cover.
    Deny(i32),
}

/// Coarse **default-deny** gate on `socket(domain, type, protocol)` (Codex's
/// review): only a "boring" inet TCP/UDP socket — or an explicitly granted
/// `AF_UNIX` — is allowed to `Continue` (after which nftables is the L3/L4
/// enforcement for *where* its packets may go). Everything else is denied with
/// `EPERM`: raw/packet sockets and `IPPROTO_RAW` (bypass L3/L4), `AF_NETLINK`
/// / `AF_VSOCK` / `AF_BLUETOOTH` / `AF_CAN` and any other non-inet family, and —
/// critically — any **unknown** family/type/protocol. We never "observe and
/// allow" an unrecognized socket shape.
///
/// `unix_granted` reflects whether the policy explicitly permits `AF_UNIX`
/// (SCM_RIGHTS fd-passing is an authority-smuggling vector, so it is off by
/// default).
pub fn decide_socket(domain: i32, sock_type: i32, protocol: i32, unix_granted: bool) -> Decision {
    const AF_UNIX: i32 = 1;
    const AF_INET: i32 = 2;
    const AF_INET6: i32 = 10;
    const SOCK_STREAM: i32 = 1;
    const SOCK_DGRAM: i32 = 2;
    const IPPROTO_RAW: i32 = 255;

    // Strip SOCK_NONBLOCK/SOCK_CLOEXEC to get the base type.
    let base_type = sock_type & 0xf;

    // AF_UNIX only by explicit grant (SCM_RIGHTS authority passing).
    if domain == AF_UNIX {
        return if unix_granted { Decision::Continue } else { Decision::Deny(libc::EPERM) };
    }
    // The one allowed shape: inet TCP/UDP, never IPPROTO_RAW. nftables governs
    // the destination from here.
    let boring_inet = (domain == AF_INET || domain == AF_INET6)
        && (base_type == SOCK_STREAM || base_type == SOCK_DGRAM)
        && protocol != IPPROTO_RAW;
    if boring_inet {
        Decision::Continue
    } else {
        // AF_PACKET, SOCK_RAW, AF_NETLINK, AF_VSOCK, unknown families/types — all deny.
        Decision::Deny(libc::EPERM)
    }
}

// ─── netns + nftables egress guard (the airtight L3/L4 layer) ─────────────────

use std::net::IpAddr;

/// One allowed egress destination: a pinned IP and port. Built by resolving the
/// policy's `net.egress` domains at run time (DNS-rebinding resistant).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressDest {
    pub ip: IpAddr,
    pub port: u16,
}

/// The result of resolving `net.egress`: the pinned `IP:port` destinations (for
/// the nftables allowlist) and, for each *hostname* entry, the single IP it was
/// pinned to (for a private `/etc/hosts`). Both come from **one** resolution
/// pass, so the address the program connects to is exactly the address nftables
/// allows — no second lookup that a CDN could answer differently.
#[derive(Debug, Clone, Default)]
pub struct ResolvedEgress {
    pub dests: Vec<EgressDest>,
    /// `(hostname, ip)` — only for non-IP-literal entries; pins DNS via files.
    pub host_pins: Vec<(String, IpAddr)>,
}

/// Resolve `net.egress` entries (`host`, `host:port`, defaulting to 443) to
/// pinned destinations + host pins. A host that fails to resolve contributes
/// nothing (fail-closed: it simply won't be reachable). Pure apart from DNS.
pub fn resolve_egress(egress: &[String]) -> ResolvedEgress {
    use std::net::ToSocketAddrs;
    let mut r = ResolvedEgress::default();
    for raw in egress {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        // Split a trailing :port only when numeric (IPv6 literals have colons).
        let (host, port) = match raw.rsplit_once(':') {
            Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => {
                (h, p.parse::<u16>().unwrap_or(443))
            }
            _ => (raw, 443u16),
        };
        let Ok(addrs) = (host, port).to_socket_addrs() else { continue };
        let mut first_ip: Option<IpAddr> = None;
        for a in addrs {
            let dest = EgressDest { ip: a.ip(), port };
            first_ip.get_or_insert(a.ip());
            if !r.dests.contains(&dest) {
                r.dests.push(dest);
            }
        }
        // Pin DNS for a *hostname* (an IP literal needs no /etc/hosts entry).
        if host.parse::<IpAddr>().is_err() {
            if let Some(ip) = first_ip {
                r.host_pins.push((host.to_string(), ip));
            }
        }
    }
    r
}

/// Just the pinned `IP:port` destinations (the nftables allowlist input).
pub fn pin_egress(egress: &[String]) -> Vec<EgressDest> {
    resolve_egress(egress).dests
}

/// Build the **default-drop** nftables ruleset for a supervised run's network
/// namespace. Only loopback, established/related return traffic, the controlled
/// resolver (port 53), and the pinned `IP:port` allowlist may leave; everything
/// else — including raw IP connects, other ports, and unlisted hosts — is
/// dropped at L3/L4, independent of whether the process respects any proxy.
/// Pure (string in, ruleset out) so it is unit-tested without touching the host.
pub fn build_nft_ruleset(allow: &[EgressDest], resolver: Option<IpAddr>) -> String {
    let mut v4 = String::new();
    let mut v6 = String::new();
    let mut push = |dst: &str, ip: &IpAddr, line: String| {
        match ip {
            IpAddr::V4(_) => v4.push_str(&line),
            IpAddr::V6(_) => v6.push_str(&line),
        }
        let _ = dst;
    };
    if let Some(r) = resolver {
        let fam = if r.is_ipv4() { "ip" } else { "ip6" };
        push(fam, &r, format!("    {fam} daddr {r} udp dport 53 accept\n"));
        push(fam, &r, format!("    {fam} daddr {r} tcp dport 53 accept\n"));
    }
    for d in allow {
        let fam = if d.ip.is_ipv4() { "ip" } else { "ip6" };
        push(fam, &d.ip, format!("    {fam} daddr {} tcp dport {} accept\n", d.ip, d.port));
    }
    format!(
        "table inet h5i_egress {{\n  \
         chain output {{\n    \
         type filter hook output priority 0; policy drop;\n    \
         ct state established,related accept\n    \
         oif \"lo\" accept\n{v4}{v6}  }}\n}}\n"
    )
}

// ─── run ──────────────────────────────────────────────────────────────────────

/// Run `argv` under the supervised tier. Re-verifies the full mediation stack is
/// green (fail-closed), then executes the command with the shared process-tier
/// confinement (Landlock + seccomp deny-list + userns/mountns/ipc/uts + cgroup +
/// no-new-privs + cap-drop) **plus** an always-on network namespace and the
/// live seccomp-notify socket gate ([`serve_with_pidfd`]), which denies
/// raw/packet/netlink/ungranted-unix sockets and records every verdict.
///
/// v1 scope: `net.mode = deny` (an empty netns — airtight, no egress). A
/// non-empty `net.egress` allowlist (netns + nftables + slirp4netns) is the next
/// increment and is **refused** here rather than silently ignored.
pub fn run(
    policy: &crate::sandbox::ResolvedPolicy,
    work: &std::path::Path,
    argv: &[String],
    injected_env: &[(String, String)],
) -> Result<crate::sandbox::ExecOutcome, H5iError> {
    preflight(policy)?;
    run_supervised(policy, work, argv, injected_env, false)
}

/// Shared fail-closed admission for both supervised entry points: the full
/// mediation stack must probe green, and — when a `net.egress` allowlist is set
/// — `slirp4netns` must be present (it provides the netns uplink).
fn preflight(policy: &crate::sandbox::ResolvedPolicy) -> Result<(), H5iError> {
    let caps = probe();
    if !caps.usable {
        return Err(H5iError::Metadata(format!(
            "isolation=supervised cannot run — the mediation stack is not fully present \
             (fail-closed). Missing: {}.",
            caps.missing().join(", ")
        )));
    }
    if !policy.profile.net_egress.is_empty() && slirp4netns_path().is_none() {
        return Err(H5iError::Metadata(
            "isolation=supervised net.egress requires `slirp4netns` on PATH (it provides the \
             network-namespace uplink) — install it, or drop net.egress for an airtight \
             net.mode=deny run (fail-closed)."
                .into(),
        ));
    }
    Ok(())
}

/// The **agent-in-box** path at the supervised tier: an interactive confined
/// session (stdio inherited, nothing captured), returning the child's exit code.
/// Same fail-closed gating as [`run`]; the seccomp-notify socket gate, netns,
/// Landlock, and cgroup limits all still apply.
pub fn run_interactive(
    policy: &crate::sandbox::ResolvedPolicy,
    work: &std::path::Path,
    argv: &[String],
    injected_env: &[(String, String)],
) -> Result<i32, H5iError> {
    preflight(policy)?;
    let outcome = run_supervised(policy, work, argv, injected_env, true)?;
    Ok(outcome.exit_code.unwrap_or(130))
}

// ─── egress: netns uplink (slirp4netns) + nftables allowlist (increment 2) ────

/// Find an executable by name, searching `$PATH` plus the sbin dirs where
/// network tools commonly live but a user's `$PATH` may omit.
#[cfg(target_os = "linux")]
fn find_bin(name: &str) -> Option<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for extra in ["/usr/sbin", "/sbin", "/usr/bin", "/bin"] {
        let p = std::path::PathBuf::from(extra);
        if !dirs.contains(&p) {
            dirs.push(p);
        }
    }
    dirs.into_iter().map(|d| d.join(name)).find(|c| c.is_file())
}

#[cfg(target_os = "linux")]
fn slirp4netns_path() -> Option<std::path::PathBuf> {
    find_bin("slirp4netns")
}

/// Distinct temp-dir suffixes for concurrent supervised egress runs.
#[cfg(target_os = "linux")]
static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(target_os = "linux")]
fn pipe_cloexec() -> std::io::Result<(std::os::unix::io::RawFd, std::os::unix::io::RawFd)> {
    let mut fds = [0i32; 2];
    let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((fds[0], fds[1]))
}

/// The host side of a supervised egress run: the temp files (nft ruleset +
/// pinned `/etc/hosts`), the handshake pipes, and the `slirp4netns` uplink
/// process. Built before the confined child is spawned; it hands the child an
/// [`crate::sandbox::EgressJail`] and tears the uplink down on drop.
#[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
struct EgressNetns {
    tmp_dir: std::path::PathBuf,
    // Parent ends (read child pid, signal "uplink ready").
    pid_read_fd: std::os::unix::io::RawFd,
    ready_write_fd: std::os::unix::io::RawFd,
    // Child ends — handed to the jail (CLOEXEC: gone at the untrusted exec).
    child_pid_write_fd: std::os::unix::io::RawFd,
    child_ready_read_fd: std::os::unix::io::RawFd,
    nft_path: std::ffi::CString,
    rules_path: std::ffi::CString,
    hosts_src: std::ffi::CString,
    helper: Option<std::thread::JoinHandle<()>>,
    slirp: std::sync::Arc<std::sync::Mutex<Option<std::process::Child>>>,
}

#[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
impl EgressNetns {
    fn jail(&self) -> crate::sandbox::EgressJail {
        crate::sandbox::EgressJail {
            ready_read_fd: self.child_ready_read_fd,
            pid_write_fd: self.child_pid_write_fd,
            nft_path: self.nft_path.clone(),
            nft_rules_path: self.rules_path.clone(),
            nft_envp: std::ffi::CString::new("PATH=/usr/sbin:/usr/bin:/sbin:/bin").unwrap(),
            hosts_src: self.hosts_src.clone(),
        }
    }
}

#[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
impl Drop for EgressNetns {
    fn drop(&mut self) {
        // Stop the uplink, then reap the helper and close every pipe end.
        if let Ok(mut g) = self.slirp.lock() {
            if let Some(mut c) = g.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
        if let Some(h) = self.helper.take() {
            let _ = h.join();
        }
        for fd in [self.pid_read_fd, self.ready_write_fd, self.child_pid_write_fd, self.child_ready_read_fd] {
            unsafe { libc::close(fd) };
        }
        let _ = std::fs::remove_dir_all(&self.tmp_dir);
    }
}

/// Build the egress jail: resolve the allowlist (once), write the nft ruleset +
/// pinned `/etc/hosts`, and launch a helper that spawns the `slirp4netns` uplink
/// for the confined child's netns and signals readiness. Fails closed if nothing
/// resolves or the tools are missing.
#[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
fn setup_egress(policy: &crate::sandbox::ResolvedPolicy) -> Result<EgressNetns, H5iError> {
    let resolved = resolve_egress(&policy.profile.net_egress);
    if resolved.dests.is_empty() {
        return Err(H5iError::Metadata(
            "net.egress resolved to no reachable address — refusing (fail-closed)".into(),
        ));
    }
    let nft = find_bin("nft").ok_or_else(|| H5iError::Metadata("`nft` not found on PATH".into()))?;
    let slirp = slirp4netns_path()
        .ok_or_else(|| H5iError::Metadata("`slirp4netns` not found on PATH".into()))?;

    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!("h5i-egress-{}-{seq}", std::process::id()));
    std::fs::create_dir_all(&tmp).map_err(H5iError::Io)?;
    // No resolver port: DNS is pinned via /etc/hosts, so port 53 stays closed.
    let rules = build_nft_ruleset(&resolved.dests, None);
    let rules_path = tmp.join("egress.nft");
    std::fs::write(&rules_path, rules).map_err(H5iError::Io)?;

    let mut hosts = String::from("127.0.0.1 localhost\n::1 localhost\n");
    for (h, ip) in &resolved.host_pins {
        hosts.push_str(&format!("{ip} {h}\n"));
    }
    let hosts_path = tmp.join("hosts");
    std::fs::write(&hosts_path, hosts).map_err(H5iError::Io)?;

    let to_c = |p: &std::path::Path| -> Result<std::ffi::CString, H5iError> {
        std::ffi::CString::new(p.as_os_str().as_encoded_bytes())
            .map_err(|_| H5iError::Metadata("path has interior NUL".into()))
    };
    let nft_path = to_c(&nft)?;
    let rules_c = to_c(&rules_path)?;
    let hosts_c = to_c(&hosts_path)?;

    // Two CLOEXEC pipes: child→parent (pid), parent→child (ready).
    let (pid_r, pid_w) = pipe_cloexec().map_err(H5iError::Io)?;
    let (ready_r, ready_w) = pipe_cloexec().map_err(H5iError::Io)?;

    let slirp_slot = std::sync::Arc::new(std::sync::Mutex::new(None));
    let slot_for_helper = slirp_slot.clone();
    // Barrier so the helper is parked in read() (not allocating) at the moment
    // the caller forks the confined child — preserving the single-threaded-fork
    // invariant the pre_exec allocations rely on.
    let (parked_tx, parked_rx) = std::sync::mpsc::channel::<()>();
    let helper = std::thread::spawn(move || {
        parked_tx.send(()).ok();
        // Park here until the child reports its pid. No allocation before this.
        let mut pidbuf = [0u8; 4];
        let n = unsafe { libc::read(pid_r, pidbuf.as_mut_ptr().cast(), 4) };
        if n != 4 {
            return;
        }
        let pid = u32::from_ne_bytes(pidbuf);
        // Spawn the uplink for the child's netns (by pid). --configure sets up
        // tap0 (10.0.2.100/24, gw 10.0.2.2); --disable-host-loopback blocks the
        // child from reaching host services via the gateway.
        let child = std::process::Command::new(&slirp)
            .args([
                "--configure",
                "--disable-host-loopback",
                "--mtu=65520",
                &pid.to_string(),
                "tap0",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        let child = match child {
            Ok(c) => c,
            Err(_) => return, // child will time out waiting for ready
        };
        // Poll the child's netns interface list (visible at /proc/<pid>/net/dev)
        // until tap0 appears — slirp has then configured the uplink.
        let dev = format!("/proc/{pid}/net/dev");
        let mut ready = false;
        for _ in 0..600 {
            if std::fs::read_to_string(&dev).map(|s| s.contains("tap0")).unwrap_or(false) {
                ready = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        *slot_for_helper.lock().unwrap() = Some(child);
        if ready {
            let _ = unsafe { libc::write(ready_w, [1u8].as_ptr().cast(), 1) };
        }
        // On failure we simply don't signal; the child's poll() times out and
        // its run fails closed.
    });
    parked_rx.recv().ok();

    Ok(EgressNetns {
        tmp_dir: tmp,
        pid_read_fd: pid_r,
        ready_write_fd: ready_w,
        child_pid_write_fd: pid_w,
        child_ready_read_fd: ready_r,
        nft_path,
        rules_path: rules_c,
        hosts_src: hosts_c,
        helper: Some(helper),
        slirp: slirp_slot,
    })
}

#[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
fn run_supervised(
    policy: &crate::sandbox::ResolvedPolicy,
    work: &std::path::Path,
    argv: &[String],
    injected_env: &[(String, String)],
    interactive: bool,
) -> Result<crate::sandbox::ExecOutcome, H5iError> {
    use crate::seccomp_notify::{pidfd_open, recv_fd, serve_with_pidfd};
    use std::io::Read;
    use std::process::Stdio;

    // A CLOEXEC socketpair for the SCM_RIGHTS listener handoff: the child sends
    // its seccomp listener fd over `sv[1]`; we receive it on `sv[0]`. CLOEXEC so
    // neither end leaks into the exec'd (untrusted) program.
    let mut sv = [0i32; 2];
    let rc = unsafe {
        libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0, sv.as_mut_ptr())
    };
    if rc != 0 {
        return Err(H5iError::Io(std::io::Error::last_os_error()));
    }
    let (sv_parent, sv_child) = (sv[0], sv[1]);
    let close = |fd: i32| unsafe {
        libc::close(fd);
    };

    // Egress allowlist (increment 2): when net.egress is set, stand up the
    // slirp4netns uplink + nftables jail. `_egress` lives for the whole run; its
    // Drop tears the uplink down. `None` ⇒ net.mode=deny (airtight empty netns).
    let _egress = if !policy.profile.net_egress.is_empty() {
        match setup_egress(policy) {
            Ok(e) => Some(e),
            Err(e) => {
                close(sv_parent);
                close(sv_child);
                return Err(e);
            }
        }
    } else {
        None
    };
    let egress_jail = _egress.as_ref().map(|e| e.jail());

    // Shared confinement + always-netns + the seccomp-notify gate.
    let mut cmd = match crate::sandbox::build_confined_command(
        policy,
        work,
        argv,
        injected_env,
        true,
        Some(sv_child),
        egress_jail,
    ) {
        Ok(c) => c,
        Err(e) => {
            close(sv_parent);
            close(sv_child);
            return Err(e);
        }
    };
    if interactive {
        // Agent-in-box: inherit the real stdio (a TTY for the shell/agent).
        // Confinement still comes from netns + the seccomp gate + Landlock.
    } else {
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    }

    let p = &policy.profile;
    let cg = crate::sandbox::make_run_cgroup(p.mem_bytes, p.max_procs);

    let started = std::time::Instant::now();
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            close(sv_parent);
            close(sv_child);
            return Err(H5iError::Metadata(format!("supervised spawn failed: {e}")));
        }
    };
    // The child has its own (CLOEXEC) copy of sv_child; drop ours.
    close(sv_child);

    // Join the child to its cgroup as early as possible.
    if let Some(cgrp) = &cg {
        let _ = std::fs::write(cgrp.procs_path(), child.id().to_string());
    }

    // Receive the seccomp listener the child installed in pre_exec. spawn()
    // returns Ok only after pre_exec (hence send_fd) completed, so this does not
    // block on a healthy child; a failure means the child died mid-setup.
    let listener = match unsafe { recv_fd(sv_parent) } {
        Ok(fd) => fd,
        Err(e) => {
            close(sv_parent);
            let _ = child.kill();
            let _ = child.wait();
            return Err(H5iError::Metadata(format!(
                "supervised: did not receive the seccomp listener from the child: {e}"
            )));
        }
    };
    close(sv_parent);
    let pidfd = match pidfd_open(child.id() as libc::pid_t) {
        Ok(fd) => fd,
        Err(e) => {
            close(listener);
            let _ = child.kill();
            let _ = child.wait();
            return Err(H5iError::Metadata(format!("supervised: pidfd_open failed: {e}")));
        }
    };

    // Stream output while the supervisor serves syscall notifications. In
    // interactive mode stdio was inherited (not piped), so there is nothing to
    // drain — the session writes straight to the terminal.
    let out_h = child.stdout.take().map(|mut out_pipe| {
        std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = out_pipe.read_to_end(&mut b);
            b
        })
    });
    let err_h = child.stderr.take().map(|mut err_pipe| {
        std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = err_pipe.read_to_end(&mut b);
            b
        })
    });

    // AF_UNIX is not granted by default (SCM_RIGHTS authority passing).
    let unix_granted = false;
    let serve_h = std::thread::spawn(move || serve_with_pidfd(listener, pidfd, unix_granted));

    // Wall-clock kill + rusage (the child called setsid → killpg reaps the tree).
    let (exit_code, timed_out, mut cpu_ms, mut max_rss_kb) =
        crate::sandbox::wait_loop(&mut child, p.wall());

    // The serve loop self-terminates when the child's pidfd signals exit.
    let stats = serve_h.join().unwrap_or_default();
    close(listener);
    close(pidfd);
    let stdout = out_h.map(|h| h.join().unwrap_or_default()).unwrap_or_default();
    let stderr = err_h.map(|h| h.join().unwrap_or_default()).unwrap_or_default();

    // Prefer cgroup accounting where present.
    if let Some(cgrp) = &cg {
        let u = cgrp.usage();
        if let Some(bytes) = u.mem_peak_bytes {
            max_rss_kb = Some((bytes / 1024) as i64);
        }
        if let Some(usec) = u.cpu_usec {
            cpu_ms = (usec / 1000) as u128;
        }
    }

    // Surface the socket-gate verdicts as the run's egress summary (the gate is
    // the supervised tier's network-creation enforcement). `denied > 0` is a
    // boundary block the dashboard's NET lane shows.
    let egress = Some(crate::objects::EgressSummary {
        allowed: stats.allowed,
        denied: stats.denied,
        hosts: Vec::new(),
        hosts_truncated: false,
        log: None,
    });

    Ok(crate::sandbox::ExecOutcome {
        stdout,
        stderr,
        exit_code,
        timed_out,
        wall_ms: started.elapsed().as_millis(),
        cpu_ms,
        max_rss_kb,
        egress,
    })
}

#[cfg(not(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]
fn run_supervised(
    _policy: &crate::sandbox::ResolvedPolicy,
    _work: &std::path::Path,
    _argv: &[String],
    _injected_env: &[(String, String)],
    _interactive: bool,
) -> Result<crate::sandbox::ExecOutcome, H5iError> {
    Err(H5iError::Metadata(
        "isolation=supervised requires Linux + x86_64/aarch64 (seccomp user-notif)".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_is_fail_closed_and_explained() {
        let caps = probe();
        // Whatever the host, an unusable claim must enumerate what's missing,
        // and a usable one must have every component ok.
        if caps.usable {
            assert!(caps.components.iter().all(|c| c.ok));
            assert!(caps.missing().is_empty());
        } else {
            assert!(!caps.missing().is_empty(), "refusal must explain what's missing");
        }
    }

    #[test]
    fn socket_gate_is_default_deny() {
        const AF_UNIX: i32 = 1;
        const AF_INET: i32 = 2;
        const AF_INET6: i32 = 10;
        const AF_PACKET: i32 = 17;
        const AF_NETLINK: i32 = 16;
        const AF_VSOCK: i32 = 40;
        const SOCK_STREAM: i32 = 1;
        const SOCK_DGRAM: i32 = 2;
        const SOCK_RAW: i32 = 3;
        const SOCK_CLOEXEC: i32 = 0o2000000;
        const IPPROTO_RAW: i32 = 255;

        let allow = |d, t, p| decide_socket(d, t, p, false);

        // The only allowed shape: boring inet TCP/UDP.
        assert_eq!(allow(AF_INET, SOCK_STREAM, 0), Decision::Continue);
        assert_eq!(allow(AF_INET6, SOCK_DGRAM, 0), Decision::Continue);
        assert_eq!(allow(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0), Decision::Continue);

        // Everything else denies — raw/packet, IPPROTO_RAW, non-inet families.
        assert_eq!(allow(AF_INET, SOCK_RAW, 0), Decision::Deny(libc::EPERM));
        assert_eq!(allow(AF_PACKET, SOCK_DGRAM, 0), Decision::Deny(libc::EPERM));
        assert_eq!(allow(AF_INET, SOCK_STREAM, IPPROTO_RAW), Decision::Deny(libc::EPERM));
        assert_eq!(allow(AF_NETLINK, SOCK_DGRAM, 0), Decision::Deny(libc::EPERM));
        assert_eq!(allow(AF_VSOCK, SOCK_STREAM, 0), Decision::Deny(libc::EPERM));
        // Unknown family/type → deny, never observe-and-allow.
        assert_eq!(allow(999, 999, 0), Decision::Deny(libc::EPERM));

        // AF_UNIX only by explicit grant.
        assert_eq!(decide_socket(AF_UNIX, SOCK_STREAM, 0, false), Decision::Deny(libc::EPERM));
        assert_eq!(decide_socket(AF_UNIX, SOCK_STREAM, 0, true), Decision::Continue);
    }

    #[test]
    fn nft_ruleset_is_default_drop_with_allowlist() {
        let allow = vec![
            EgressDest { ip: "93.184.216.34".parse().unwrap(), port: 443 },
            EgressDest { ip: "2606:2800:220:1:248:1893:25c8:1946".parse().unwrap(), port: 443 },
        ];
        let resolver = Some("10.0.2.3".parse().unwrap());
        let rs = build_nft_ruleset(&allow, resolver);
        // Fail-closed default.
        assert!(rs.contains("policy drop;"), "must default-drop:\n{rs}");
        // Loopback + established always allowed.
        assert!(rs.contains("oif \"lo\" accept"));
        assert!(rs.contains("ct state established,related accept"));
        // Resolver on 53 only.
        assert!(rs.contains("ip daddr 10.0.2.3 udp dport 53 accept"));
        // The pinned v4 + v6 allowlist with their port.
        assert!(rs.contains("ip daddr 93.184.216.34 tcp dport 443 accept"));
        assert!(rs.contains("ip6 daddr 2606:2800:220:1:248:1893:25c8:1946 tcp dport 443 accept"));
    }

    #[test]
    fn nft_empty_allowlist_drops_everything_but_lo() {
        let rs = build_nft_ruleset(&[], None);
        assert!(rs.contains("policy drop;"));
        assert!(rs.contains("oif \"lo\" accept"));
        // No accept for any external destination.
        assert!(!rs.contains("daddr"), "empty allowlist must add no daddr rule:\n{rs}");
    }

    #[test]
    fn nft_rule_pins_the_exact_port() {
        // A non-default port must appear verbatim in the allow rule (the gate is
        // host:port, not just host).
        let allow = vec![EgressDest { ip: "10.1.2.3".parse().unwrap(), port: 8443 }];
        let rs = build_nft_ruleset(&allow, None);
        assert!(rs.contains("ip daddr 10.1.2.3 tcp dport 8443 accept"), "{rs}");
        // And nothing on the conventional 443 for that host.
        assert!(!rs.contains("dport 443"));
    }

    #[test]
    fn pin_egress_parses_host_and_explicit_port() {
        // localhost resolves deterministically on every host; assert the port
        // parsing (default 443 vs explicit) without depending on public DNS.
        let pinned = pin_egress(&["localhost".into(), "localhost:8080".into()]);
        assert!(pinned.iter().any(|d| d.port == 443), "default port should be 443");
        assert!(pinned.iter().any(|d| d.port == 8080), "explicit port should be honored");
        assert!(pinned.iter().all(|d| d.ip.is_loopback()));
        // An empty/garbage entry contributes nothing (fail-closed).
        assert!(pin_egress(&["".into(), "   ".into()]).is_empty());
    }

    #[test]
    fn resolve_egress_pins_hostnames_not_ip_literals() {
        // One resolution pass yields both the nft dests and the /etc/hosts pins.
        // A *hostname* gets a pin (so DNS resolves to exactly the allowed IP); an
        // IP literal needs none (the program connects to it directly).
        let r = resolve_egress(&["localhost".into(), "127.0.0.1:8080".into()]);
        assert!(!r.dests.is_empty());
        assert!(r.host_pins.iter().any(|(h, _)| h == "localhost"), "hostname pinned");
        assert!(
            r.host_pins.iter().all(|(h, _)| h != "127.0.0.1"),
            "IP literal needs no /etc/hosts pin"
        );
        // Every pinned host's IP is among the nft-allowed destinations.
        for (_, ip) in &r.host_pins {
            assert!(r.dests.iter().any(|d| &d.ip == ip), "pin IP is in the nft allowlist");
        }
    }

    #[test]
    fn run_egress_fails_closed_when_unsupported() {
        // With a net.egress allowlist, run() still fails closed when the host
        // can't satisfy the supervised stack OR slirp4netns is absent — never a
        // silent partial run. (On a fully-capable host the e2e test in
        // tests/env_integration.rs proves real enforcement.)
        let mut p = crate::sandbox::Profile::builtin("p", crate::sandbox::IsolationClaim::Supervised);
        p.net_egress = vec!["example.com".into()];
        let pol = crate::sandbox::ResolvedPolicy { claim: p.isolation, profile: p };
        let usable = probe().usable && slirp4netns_path().is_some();
        if usable {
            // Can't assert a refusal on a capable host; that path is the e2e test.
            return;
        }
        let err = run(&pol, &std::env::temp_dir(), &["true".to_string()], &[]).unwrap_err();
        let m = format!("{err}");
        assert!(
            m.contains("Missing") || m.contains("slirp4netns"),
            "must fail closed with the missing component, got: {m}"
        );
    }
}
