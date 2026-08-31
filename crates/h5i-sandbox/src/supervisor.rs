//! `isolation=supervised`: the seccomp-notify supervisor tier
//! (`docs/supervisor-design.md`).
//!
// Platform-conditional machinery: `probe()` is cross-platform (the dashboard
// reports supervisor readiness on any host), but the socket-gate verdicts and
// nftables egress builder (`Decision`, `decide_socket`, `build_nft_ruleset`, …)
// are reached only from the `#[cfg(target_os = "linux")]` run path, so they read
// as dead code on non-Linux targets. Allow it module-wide.
#![allow(dead_code)]
//!
//! This is the security keystone: the first tier that may claim untrusted-code
//! containment. Its defining property, implemented and tested here in phase A,
//! is *fail-closed admission*: the claim is satisfiable only when *every*
//! component probes green, and is otherwise *refused*, never downgraded to a
//! weaker tier. A half-present stack is a refusal, not a "best-effort pass".
//!
//! This module: the honest [`probe`], the pure syscall-decision model, and a
//! fully-enforcing [`run`]. The shared process-tier confinement plus an
//! always-on network namespace and the live seccomp-notify socket gate
//! ([`serve_with_pidfd`]), with an optional netns+nftables+slirp4netns egress
//! allowlist. Because the full stack does not probe green on every host
//! (WSL2/CI lack cgroup delegation and rootless nftables), the tier fail-closed
//! refuses there rather than downgrading.

use crate::error::H5iError;

/// One component of the supervised stack and whether the host provides it.
#[derive(Debug, Clone)]
pub struct ComponentStatus {
    pub name: &'static str,
    pub ok: bool,
    pub detail: Option<String>,
}

/// Host readiness for `isolation=supervised`. `usable` is true only when every
/// required component is `ok`: the single source of truth `resolve` consults.
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
    // Process-local only: repeated policy resolution/preflight in one h5i
    // invocation should not redo the functional probes, but the next command
    // still re-checks the host before claiming supervised containment.
    static SUPERVISOR_CAPS: std::sync::OnceLock<SupervisorCaps> = std::sync::OnceLock::new();
    SUPERVISOR_CAPS.get_or_init(probe_uncached).clone()
}

#[cfg(target_os = "linux")]
fn probe_uncached() -> SupervisorCaps {
    // Supervised readiness reads only the kernel bits (userns/Landlock/seccomp),
    // never the container runtime. Use the kernel-only probe so resolving a
    // supervised claim doesn't shell out to `podman info`.
    let host = crate::sandbox::probe_host_kernel();
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
    // A netns is created via unshare(NEWNET) inside our userns. Functionally probed.
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

/// macOS readiness for `isolation=supervised`.
///
/// The Linux stack (seccomp user-notification, a network namespace, nftables,
/// cgroup delegation) does not exist on Darwin and is not faked. What the tier
/// promises is *untrusted-code containment plus an enforced domain egress
/// allowlist*, and macOS reaches both a different way:
///
/// - containment: Seatbelt's `(deny default)` covers filesystem, network, mach
///   and sysctl in one policy, and (unlike Landlock) it can subtract, so
///   `fs.deny` and the agent-config lock are enforced rather than linted;
/// - egress allowlist: the box is left with *no* outbound route except h5i's
///   own DNS-pinned allowlist proxy on loopback, which is the same proxy the
///   container tier uses and is enforced by the kernel, not by proxy env vars.
///
/// What is genuinely absent is the syscall filter: there is no macOS equivalent
/// of a seccomp deny-list, so native code in the box can attempt any syscall.
/// It just cannot reach a path or a socket the profile does not name. That is
/// reported here rather than glossed, and it is why the component below is
/// spelled out instead of being asserted `ok`.
#[cfg(target_os = "macos")]
pub fn probe() -> SupervisorCaps {
    static SUPERVISOR_CAPS: std::sync::OnceLock<SupervisorCaps> = std::sync::OnceLock::new();
    SUPERVISOR_CAPS
        .get_or_init(|| {
            let sb = crate::seatbelt::probe();
            let mut components = vec![ComponentStatus {
                name: "seatbelt",
                ok: sb.usable(),
                detail: sb.detail.clone(),
            }];
            // The allowlist proxy is h5i's own code and needs only a loopback
            // listener, but a host that cannot bind loopback cannot enforce
            // egress, so probe it rather than assume it.
            let loopback = std::net::TcpListener::bind("127.0.0.1:0");
            components.push(ComponentStatus {
                name: "loopback-egress-proxy",
                ok: loopback.is_ok(),
                detail: loopback
                    .err()
                    .map(|e| format!("cannot bind 127.0.0.1 for the allowlist proxy: {e}")),
            });
            components.push(ComponentStatus {
                name: "seatbelt-network-gate",
                ok: sb.usable(),
                detail: (!sb.usable())
                    .then(|| "needs Seatbelt to pin the box to the proxy port".into()),
            });
            // Stated, never silently assumed: this tier does NOT filter syscalls
            // on macOS. It is `ok` because the tier does not claim to, the
            // claim is filesystem/network containment, but it is listed so
            // `env probe` shows the difference from a Linux box.
            components.push(ComponentStatus {
                name: "syscall-filter",
                ok: true,
                detail: Some(
                    "not applicable on macOS: Darwin has no seccomp; containment here is \
                     Seatbelt's filesystem/network policy"
                        .into(),
                ),
            });
            let usable = components.iter().all(|c| c.ok);
            SupervisorCaps { usable, components }
        })
        .clone()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn probe() -> SupervisorCaps {
    SupervisorCaps {
        usable: false,
        components: vec![ComponentStatus {
            name: "platform",
            ok: false,
            detail: Some("isolation=supervised needs Linux or macOS".into()),
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
    /// real guard, nftables / Landlock, is the enforcement layer). For sockets
    /// this is the common path: the packet still hits nftables.
    Continue,
    /// Refuse with `errno` (no pointer deref → no TOCTOU). Used for dangerous
    /// shapes the netns/Landlock layers don't already cover.
    Deny(i32),
}

/// Coarse *default-deny* gate on `socket(domain, type, protocol)` (Codex's
/// review): only a "boring" inet TCP/UDP socket, or an explicitly granted
/// `AF_UNIX`, is allowed to `Continue` (after which nftables is the L3/L4
/// enforcement for *where* its packets may go). Everything else is denied with
/// `EPERM`: raw/packet sockets and `IPPROTO_RAW` (bypass L3/L4), `AF_NETLINK`
/// / `AF_VSOCK` / `AF_BLUETOOTH` / `AF_CAN` and any other non-inet family, and,
/// critically, any *unknown* family/type/protocol. We never "observe and
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
        // AF_PACKET, SOCK_RAW, AF_NETLINK, AF_VSOCK, unknown families/types. All deny.
        Decision::Deny(libc::EPERM)
    }
}

/// Gate on `socketpair(domain, type, protocol)`. An `AF_UNIX` socketpair is an
/// anonymous connected pair. It has no address, cannot `connect` anywhere,
/// and both ends are born inside the sandbox, so it grants no authority the
/// process didn't already have (it could only leave over an already-granted
/// channel). It is also load-bearing: tokio's signal handling, Node's
/// child-process IPC, and most modern runtimes create one at startup, so
/// denying it bricks coding agents in the box. Every other shape (socketpair
/// is AF_UNIX-only in practice; anything else is suspicious) falls through to
/// the default-deny [`decide_socket`] gate.
pub fn decide_socketpair(domain: i32, sock_type: i32, protocol: i32, unix_granted: bool) -> Decision {
    const AF_UNIX: i32 = 1;
    if domain == AF_UNIX {
        return Decision::Continue;
    }
    decide_socket(domain, sock_type, protocol, unix_granted)
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
/// pinned to (for a private `/etc/hosts`). Both come from *one* resolution
/// pass, so the address the program connects to is exactly the address nftables
/// allows, no second lookup that a CDN could answer differently.
#[derive(Debug, Clone, Default)]
pub struct ResolvedEgress {
    pub dests: Vec<EgressDest>,
    /// `(hostname, ip)`: only for non-IP-literal entries; pins DNS via files.
    pub host_pins: Vec<(String, IpAddr)>,
    /// `(entry, ip)` for an answer that named somewhere the box must not be
    /// pointed at: link-local, multicast, broadcast. Reported rather than
    /// dropped, and *fatal* at [`setup_egress`]: the entry looked reasonable
    /// and resolved to somewhere a box has no business dialling, and that is the
    /// operator's news, not a detail to swallow.
    pub refused: Vec<(String, IpAddr)>,
    /// `(entry, ip)` for an answer of `0.0.0.0` / `::`, which is what a
    /// filtering resolver returns for a name it blocks. Unpinnable like the
    /// above and reported like it, but *not* fatal: see [`is_sinkhole`].
    pub sinkholed: Vec<(String, IpAddr)>,
}

/// May a `net.egress` entry pin to this address?
///
/// The policy an operator reads is a *hostname*, and the thing nftables
/// enforces is whatever that name resolved to, so the readable policy and the
/// enforced destination are only as close as DNS chooses to make them. A repo
/// that ships `.h5i/env.toml` picks the names, and it can equally pick what
/// they answer. `net.egress = ["assets.example-cdn.com"]` reads like a CDN and
/// pins to whatever that zone returns.
///
/// What that buys, on the tier whose whole claim is airtight L3/L4 filtering:
///
/// * *`169.254.169.254`*: the cloud instance metadata service, and the
///   highest-value target on any cloud host. slirp4netns NATs through the
///   host's routing, and `--disable-host-loopback` does not cover it: it hides
///   the host's *loopback*, not its link-local. A name resolving here hands the
///   box the instance's role credentials.
/// * *`fe80::/10`*, the same thing over IPv6, and `::ffff:169.254.169.254`,
///   the same thing spelled as a mapped address so a v4-only test misses it.
/// * multicast, broadcast, unspecified, no reachable service a repo could
///   mean, and each of them means something else here. The unspecified address
///   is unpinnable for the same reason as the rest but says something different
///   about *why*, which is what [`is_sinkhole`] separates.
///
/// Loopback is deliberately *not* refused. Inside the netns `127.0.0.1` is the
/// box's own, `oif "lo" accept` already permits it, and the host's loopback is
/// reached (when it is reached at all) through the slirp gateway rather than
/// through this address, so a name answering `127.0.0.1` is redundant, not
/// dangerous, and refusing it would break naming a service the box itself runs.
///
/// RFC1918 and IPv6 unique-local are deliberately *not* refused: an internal
/// registry or a company mirror is a real thing to name, and refusing it would
/// break boxes that work. They are reachable through the host's routing and
/// that is worth knowing, which is what `refused` reporting and the receipt's
/// pinned addresses are for. This function only closes the cases where no
/// legitimate entry exists at all.
fn is_pinnable(ip: &IpAddr) -> bool {
    // A v4 address written as `::ffff:a.b.c.d` is the same address. Unwrap it
    // and judge it once, or every check below is one spelling short.
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(*ip),
        v4 => *v4,
    };
    if ip.is_multicast() || ip.is_unspecified() {
        return false;
    }
    match ip {
        IpAddr::V4(v4) => !v4.is_link_local() && !v4.is_broadcast(),
        // `fe80::/10`. Spelled out rather than via `is_unicast_link_local`,
        // which is unstable.
        IpAddr::V6(v6) => v6.segments()[0] & 0xffc0 != 0xfe80,
    }
}

/// Is this answer a resolver saying "blocked" rather than a name pointing
/// somewhere it should not?
///
/// `0.0.0.0` and `::` are what a filtering resolver returns for a name on a
/// blocklist: pi-hole, a corporate DNS policy, several consumer ISPs. That is a
/// statement about the *operator's network*, not about the repo's policy, and
/// it is the one unpinnable answer an ordinary well-meaning `net.egress` entry
/// runs into. Treating it like a link-local answer refuses the whole box over a
/// name the resolver had already made unreachable anyway, and the box would
/// have been fine, because an entry that cannot be pinned is an entry the box
/// cannot dial. So it is separated here: still never pinned, still reported,
/// but it costs that one name instead of the run. See [`setup_egress`].
///
/// It is also the safe half of the split. `0.0.0.0` is not a destination this
/// tier could be tricked into allowing: nothing about it reaches the host's
/// link-local (the address the rest of [`is_pinnable`] exists for), it gets no
/// nftables rule and no `/etc/hosts` pin, and inside the netns a connect to it
/// lands on the box's own loopback, which `oif "lo" accept` already permits
/// and [`is_pinnable`] deliberately allows by name.
fn is_sinkhole(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(*ip),
        v4 => *v4,
    }
    .is_unspecified()
}

/// Resolve `net.egress` entries (`host`, `host:port`, defaulting to 443) to
/// pinned destinations + host pins. A host that fails to resolve contributes
/// nothing (fail-closed: it simply won't be reachable), and an answer
/// [`is_pinnable`] refuses is dropped and reported, as `sinkholed` when
/// [`is_sinkhole`] explains it, as `refused` otherwise. Pure apart from DNS.
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
            if !is_pinnable(&a.ip()) {
                if is_sinkhole(&a.ip()) {
                    r.sinkholed.push((raw.to_string(), a.ip()));
                } else {
                    r.refused.push((raw.to_string(), a.ip()));
                }
                continue;
            }
            let dest = EgressDest { ip: a.ip(), port };
            // The *pinned* address has to be one that survived the check too:
            // this is what goes into the box's `/etc/hosts`, and pinning a
            // refused answer would point the name at an address nftables then
            // has no rule for. A name that reads as allowed and never works.
            first_ip.get_or_insert(a.ip());
            if !r.dests.contains(&dest) {
                r.dests.push(dest);
            }
        }
        // Pin DNS for a *hostname* (an IP literal needs no /etc/hosts entry).
        if host.parse::<IpAddr>().is_err()
            && let Some(ip) = first_ip
        {
            r.host_pins.push((host.to_string(), ip));
        }
    }
    r
}

/// Just the pinned `IP:port` destinations (the nftables allowlist input).
pub fn pin_egress(egress: &[String]) -> Vec<EgressDest> {
    resolve_egress(egress).dests
}

/// Build the *default-drop* nftables ruleset for a supervised run's network
/// namespace. Only loopback, established/related return traffic, the controlled
/// resolver (port 53), and the pinned `IP:port` allowlist may leave; everything
/// else (including raw IP connects, other ports, and unlisted hosts) is
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
/// no-new-privs + cap-drop) *plus* an always-on network namespace and the
/// live seccomp-notify socket gate ([`serve_with_pidfd`]), which denies
/// raw/packet/netlink/ungranted-unix sockets and records every verdict.
///
/// Scope: `net.mode = deny` (an empty netns (airtight, no egress), or a
/// non-empty `net.egress` allowlist enforced via netns + nftables + slirp4netns
/// (which requires `slirp4netns` on PATH, else the run is refused) fail-closed).
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
/// mediation stack must probe green, and, when a `net.egress` allowlist is set,
/// `slirp4netns` must be present (it provides the netns uplink).
fn preflight(policy: &crate::sandbox::ResolvedPolicy) -> Result<(), H5iError> {
    let caps = probe();
    if !caps.usable {
        return Err(H5iError::Metadata(format!(
            "isolation=supervised cannot run — the mediation stack is not fully present \
             (fail-closed). Missing: {}.",
            caps.missing().join(", ")
        )));
    }
    egress_preflight(policy)
}

/// The egress uplink is per-platform, so its admission check is too.
///
/// Linux runs the allowlist *inside* the box's network namespace, which needs
/// `slirp4netns` to provide that namespace's uplink. An external binary that
/// can be missing, hence a check.
#[cfg(not(target_os = "macos"))]
fn egress_preflight(policy: &crate::sandbox::ResolvedPolicy) -> Result<(), H5iError> {
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

/// macOS has no namespace to uplink and therefore no external binary to
/// require: the box keeps the host's network stack and the Seatbelt profile
/// leaves it no outbound route except h5i's own loopback proxy. Readiness for
/// that is already covered by [`probe`] (Seatbelt usable + loopback bindable),
/// which `preflight` checked above, so there is nothing further to assert.
#[cfg(target_os = "macos")]
fn egress_preflight(_policy: &crate::sandbox::ResolvedPolicy) -> Result<(), H5iError> {
    Ok(())
}

/// The *agent-in-box* path at the supervised tier: an interactive confined
/// session (stdio inherited, nothing captured), returning the child's exit code.
/// Same fail-closed gating as [`run`]; the seccomp-notify socket gate, netns,
/// Landlock, and cgroup limits all still apply.
pub fn run_interactive(
    policy: &crate::sandbox::ResolvedPolicy,
    work: &std::path::Path,
    argv: &[String],
    injected_env: &[(String, String)],
    // Supervised tier does not inject managed-settings (no bind-mount ns);
    // accepted for a uniform interactive-backend signature, ignored.
    _managed_settings_content: Option<&str>,
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

/// Non-Linux stub: `slirp4netns` is a Linux-only netns uplink, so there is never
/// a path to find. Keeps the cross-platform `preflight` (called by the public
/// `run`/`run_interactive`) compiling on the macOS/Windows release targets,
/// where the supervised tier is already refused by `probe()` / `run_supervised`.
#[cfg(not(target_os = "linux"))]
fn slirp4netns_path() -> Option<std::path::PathBuf> {
    None
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
    // Child ends. Handed to the jail (CLOEXEC: gone at the untrusted exec).
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
        // Stop the uplink first, so the helper has nothing left to supervise.
        if let Ok(mut g) = self.slirp.lock()
            && let Some(mut c) = g.take()
        {
            let _ = c.kill();
            let _ = c.wait();
        }
        // Close the pid pipe's WRITE ends BEFORE joining. The helper parks in
        // `read(pid_r, .., 4)` waiting for the child to report its pid, and its
        // only early exit is a short read, which needs every writer gone. The
        // child's copy closes when it `_exit`s, but ours did not until after
        // the join, so any pre_exec failure before the pid write (a userns
        // ENOSPC from concurrent boxes, a uid_map write failure) deadlocked
        // teardown permanently.
        for fd in [self.child_pid_write_fd, self.pid_read_fd] {
            unsafe { libc::close(fd) };
        }
        if let Some(h) = self.helper.take() {
            let _ = h.join();
        }
        for fd in [self.ready_write_fd, self.child_ready_read_fd] {
            unsafe { libc::close(fd) };
        }
        let _ = std::fs::remove_dir_all(&self.tmp_dir);
    }
}

/// Build the egress jail: resolve the allowlist (once), write the nft ruleset +
/// pinned `/etc/hosts`, and launch a helper that spawns the `slirp4netns` uplink
/// for the confined child's netns and signals readiness. Fails closed if nothing
/// resolves or the tools are missing.
/// The slirp gateway address a supervised netns routes through; also where the
/// host's loopback (and our auth proxy) appears when host-loopback is enabled.
#[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
const SLIRP_GATEWAY: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 2, 2));

/// `slirp4netns` argv for a child netns (pid). `--disable-host-loopback` is
/// present UNLESS host-loopback is intentionally allowed (auth proxy engaged),
/// in which case the box can reach the host proxy via the gateway (nftables
/// still restricts egress to the single proxy port). Pure. Unit-tested.
#[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
fn slirp_args(pid: u32, allow_host_loopback: bool) -> Vec<String> {
    let mut a: Vec<String> = vec!["--configure".into()];
    if !allow_host_loopback {
        a.push("--disable-host-loopback".into());
    }
    a.push("--mtu=65520".into());
    a.push(pid.to_string());
    a.push("tap0".into());
    a
}

/// The credential files to remove from the box's per-env HOME copies for `rt`
/// (pure: maps runtime + home binds → backing paths). A bind is matched by its
/// *target* dir name (the real `~/.claude` / `~/.codex`); the returned path is
/// under the env's own `backing`, never the real HOME. See
/// [`scrub_box_credentials`].
#[cfg(any(
    all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")),
    target_os = "macos"
))]
fn cred_scrub_paths(
    rt: crate::sandbox_policy::AgentRuntime,
    home_binds: &[crate::sandbox_policy::HomeBind],
) -> Vec<std::path::PathBuf> {
    use crate::sandbox_policy::AgentRuntime;
    let (dir_name, cred_file) = match rt {
        AgentRuntime::Claude => (".claude", ".credentials.json"),
        AgentRuntime::Codex => (".codex", "auth.json"),
    };
    home_binds
        .iter()
        .filter(|b| b.target.file_name().and_then(|n| n.to_str()) == Some(dir_name))
        .map(|b| b.backing.join(cred_file))
        .collect()
}

/// Remove the runtime's credential file from the box's per-env HOME copy when
/// the auth proxy is engaged, so the token is absent from the box (not merely
/// inert). Auth then flows entirely through the proxy + dummy env token. Only
/// ever touches the env's own backing copy (`policy.home_binds[..].backing`).
/// Never the real HOME, which `prepare_home_state` only reads. Best-effort and
/// idempotent; a later in-box login self-heals the copy if the proxy is disabled.
#[cfg(any(
    all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")),
    target_os = "macos"
))]
fn scrub_box_credentials(
    policy: &crate::sandbox::ResolvedPolicy,
    rt: crate::sandbox_policy::AgentRuntime,
) {
    for path in cred_scrub_paths(rt, &policy.home_binds) {
        let _ = std::fs::remove_file(path);
    }
}

/// Build the egress jail. When `auth_port` is `Some`, the credential-injecting
/// auth proxy is engaged: the box's egress is locked to *only* the proxy at
/// `10.0.2.2:<auth_port>` (host-loopback re-enabled for that single port; the
/// default-drop policy still blocks every other host port and all direct API
/// egress), so a token in the box, even if present, is inert (unusable
/// directly, unexfiltratable). Otherwise the box egresses to the resolved
/// `net.egress` allowlist with host-loopback disabled (airtight).
#[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
fn setup_egress(
    policy: &crate::sandbox::ResolvedPolicy,
    auth_port: Option<u16>,
) -> Result<EgressNetns, H5iError> {
    let (dests, host_pins): (Vec<EgressDest>, Vec<(String, IpAddr)>) = match auth_port {
        // Proxy-only egress: one accept, for the host-side auth proxy. No DNS
        // needed (the base URL is the gateway IP), so no host pins.
        Some(port) => (vec![EgressDest { ip: SLIRP_GATEWAY, port }], Vec::new()),
        None => {
            let resolved = resolve_egress(&policy.profile.net_egress);
            // Loud, not silent. The entry read like an ordinary host and
            // answered with an address this tier will not dial, and the two
            // facts only make sense together. Dropping it quietly would leave
            // a box that fails to reach something its policy plainly allows,
            // with the reason nowhere.
            if let Some((entry, ip)) = resolved.refused.first() {
                return Err(H5iError::Metadata(format!(
                    "net.egress entry {entry:?} resolves to {ip}, which this tier refuses to \
                     pin: it is a link-local, multicast or broadcast address, not a host on \
                     the network. 169.254.169.254 is the cloud instance metadata service; a \
                     name answering there would hand the box the instance's credentials, and \
                     nothing in the policy text would show it (fail-closed)."
                )));
            }
            // Reported, not fatal. A sinkholed answer is the operator's resolver
            // saying it blocks the name, so refusing the run would take the
            // whole box down over one entry the box could not have reached
            // either way, and would blame the policy for the resolver.
            for (entry, ip) in &resolved.sinkholed {
                eprintln!(
                    "h5i: net.egress entry {entry:?} resolves to {ip} — a resolver returns that \
                     for a name it blocks, so this entry is unreachable and was not pinned. The \
                     rest of net.egress is unaffected."
                );
            }
            if resolved.dests.is_empty() {
                // Which of the two it is decides where to look, so say so.
                let why = if resolved.sinkholed.is_empty() {
                    ""
                } else {
                    " (every entry was sinkholed by the resolver — the names are blocked on \
                     this network, so check the resolver rather than the policy)"
                };
                return Err(H5iError::Metadata(format!(
                    "net.egress resolved to no reachable address — refusing (fail-closed){why}"
                )));
            }
            (resolved.dests, resolved.host_pins)
        }
    };
    let allow_host_loopback = auth_port.is_some();
    let nft = find_bin("nft").ok_or_else(|| H5iError::Metadata("`nft` not found on PATH".into()))?;
    let slirp = slirp4netns_path()
        .ok_or_else(|| H5iError::Metadata("`slirp4netns` not found on PATH".into()))?;

    // Unguessable and 0700: the ruleset written here is handed to `nft -f` by a
    // child holding CAP_NET_ADMIN, so a pre-planted directory or symlink at a
    // predictable path would let another local user choose the box's egress
    // policy outright.
    let tmp = crate::sandbox::private_scratch_dir("h5i-egress")?;
    // No resolver port: DNS is pinned via /etc/hosts, so port 53 stays closed.
    let rules = build_nft_ruleset(&dests, None);
    let rules_path = tmp.join("egress.nft");
    std::fs::write(&rules_path, rules).map_err(H5iError::Io)?;

    let mut hosts = String::from("127.0.0.1 localhost\n::1 localhost\n");
    for (h, ip) in &host_pins {
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
    // the caller forks the confined child. Preserving the single-threaded-fork
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
        // tap0 (10.0.2.100/24, gw 10.0.2.2). --disable-host-loopback blocks the
        // child from reaching host services via the gateway. Kept UNLESS the
        // auth proxy is engaged, which needs the gateway to forward to the host
        // proxy on loopback (nftables still restricts egress to that one port).
        let child = std::process::Command::new(&slirp)
            .args(slirp_args(pid, allow_host_loopback))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        let child = match child {
            Ok(c) => c,
            Err(_) => return, // child will time out waiting for ready
        };
        // Poll the child's netns interface list (visible at /proc/<pid>/net/dev)
        // until tap0 appears. Slirp has then configured the uplink.
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
    use std::process::Stdio;

    // Check the notify ABI *before* committing to this tier. The module doc
    // promises the `seccomp_notif` structs are validated against
    // SECCOMP_GET_NOTIF_SIZES and refused on mismatch, but nothing called the
    // validator on any production path, so on a kernel with a different layout
    // the listener installed fine and then `ioctl(NOTIF_RECV)`, whose request
    // number embeds our struct size, failed with an unexpected errno. The serve
    // loop broke on the first notification and the box's first socket() blocked
    // unanswered: a hang where a refusal was documented.
    //
    // Here rather than at `install_listener`, which runs in the forked child
    // and must not allocate an error.
    crate::seccomp_notify::validate_notif_sizes()?;

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

    // Credential-injecting auth proxy (option 2), host-side, held for the whole
    // run. Engages for an agent box with a resolvable host credential when egress
    // is active (the netns reaches the host proxy through the slirp uplink). When
    // engaged, the box's egress is locked to the proxy alone (see `setup_egress`)
    // and the real credential is scrubbed from the box's per-env HOME copy, so
    // the token is absent from the box entirely.
    let auth = if !policy.profile.net_egress.is_empty() {
        crate::auth_proxy::engage(&policy.profile.name, true)?
    } else {
        None
    };
    let (_auth_proxy, effective_env, auth_port) = match auth {
        Some(e) => {
            scrub_box_credentials(policy, e.runtime);
            let port = e.handle.port;
            let mut env = injected_env.to_vec();
            env.extend(e.box_env);
            (Some(e.handle), env, Some(port))
        }
        None => (None, injected_env.to_vec(), None),
    };
    let injected_env: &[(String, String)] = &effective_env;

    // Egress allowlist (increment 2): when net.egress is set, stand up the
    // slirp4netns uplink + nftables jail. `_egress` lives for the whole run; its
    // Drop tears the uplink down. `None` ⇒ net.mode=deny (airtight empty netns).
    let _egress = if !policy.profile.net_egress.is_empty() {
        match setup_egress(policy, auth_port) {
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

    let p = &policy.profile;
    // The run cgroup is created BEFORE the command so its `cgroup.procs` path can
    // be handed to the PID-namespace supervisor: with a pidns the workload is a
    // grandchild whose pid only exists inside `pre_exec`, so the supervisor there
    // joins it to the cgroup. Otherwise `memory.max` and the accounting would
    // bind the thin supervisor instead of the process they are meant to bound.
    let cg = crate::sandbox::make_run_cgroup(p.mem_bytes, p.max_procs);
    let procs = cg.as_ref().map(|c| c.procs_path());

    // Shared confinement + always-netns + the seccomp-notify gate.
    let mut cmd = match crate::sandbox::build_confined_command(
        policy,
        work,
        argv,
        injected_env,
        true,
        Some(sv_child),
        egress_jail,
        // A PID namespace, exactly as the process tier gets. Without it the box
        // shares the host's PID namespace, which is not a cosmetic difference:
        // it can enumerate host processes, read their `/proc/<pid>/cmdline`, and,
        // because the userns maps back to the operator's real uid, send
        // signals to any of their processes, h5i itself included. A tier that
        // claims untrusted-code containment cannot leave `kill -9` pointed at
        // the host. `/proc/<pid>/environ` was already denied (the userns fails
        // `ptrace_may_access`), so this closes the reachable half.
        //
        // Ordering note: the netns/egress handshake in `pre_exec` runs BEFORE the
        // pidns fork and reports `getpid()`, which CLONE_NEWPID leaves in the
        // host namespace, so `slirp4netns` still targets a pid it can see, and
        // the workload shares that netns by inheritance.
        true,
        procs.as_deref(),
        interactive,
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

    // Join the thin supervisor to the cgroup too, so nothing in the tree escapes
    // accounting. The workload itself was joined from inside `pre_exec` (it is a
    // grandchild and has no host-visible pid out here until then).
    if let Some(cgrp) = &cg {
        let _ = std::fs::write(cgrp.procs_path(), child.id().to_string());
    }

    // Receive the seccomp listener the workload installed in pre_exec. `spawn()`
    // returns once the workload reaches `execve`, the listener handoff happens
    // just before that, so this does not block on a healthy child; a failure
    // means it died mid-setup.
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
    // drain. The session writes straight to the terminal.
    let out_h = child.stdout.take().map(|mut out_pipe| {
        std::thread::spawn(move || crate::sandbox::drain_capped(&mut out_pipe))
    });
    let err_h = child.stderr.take().map(|mut err_pipe| {
        std::thread::spawn(move || crate::sandbox::drain_capped(&mut err_pipe))
    });

    // AF_UNIX is not granted by default (SCM_RIGHTS authority passing). A
    // profile has to ask for it. What that grant does and does not widen is on
    // `Profile::unix_sockets`; the short version is that abstract sockets stay
    // inside the box's private netns and filesystem-bound ones stay inside its
    // Landlock grants, so the residual is a host socket under a granted path.
    let unix_granted = policy.profile.unix_sockets;
    let serve_h = std::thread::spawn(move || serve_with_pidfd(listener, pidfd, unix_granted));

    // Wall-clock kill + rusage (the child called setsid → killpg reaps the tree).
    // Interactive sessions get no deadline: they are bounded by the operator,
    // not a timer (the same rule as the process tier's interactive path), and
    // having skipped setsid they have no dedicated process group to killpg.
    let wall = if interactive { None } else { Some(p.wall()) };
    let (exit_code, timed_out, mut cpu_ms, mut max_rss_kb) =
        crate::sandbox::wait_loop(&mut child, wall);

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
    let egress = Some(crate::sandbox_policy::EgressSummary {
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

/// The macOS supervised tier: Seatbelt confinement plus a host-side allowlist
/// proxy that the box has no way to route around.
///
/// The security argument is the mirror image of the Linux one, and it is worth
/// stating because "point the box at a proxy" is normally *not* an enforcement
/// mechanism. A program that ignores `HTTPS_PROXY` and opens its own socket
/// escapes it. That is not the case here: the Seatbelt profile denies
/// `network-outbound` to everything except the proxy's loopback port, so
/// ignoring the env var gets the box a connection refused at the kernel, not a
/// direct route. The proxy env vars are a *convenience* for well-behaved
/// clients; the kernel rule is the boundary.
///
/// Two shapes, matching the Linux path exactly:
///
/// - credential proxy engaged (an agent box with a resolvable host token):
///   the only reachable port is the credential-injecting auth proxy, the real
///   token never enters the box, and it is scrubbed from the box's per-env HOME
///   copy so it is absent rather than merely inert. No general egress at all.
/// - *otherwise*: the DNS-pinned `net.egress` allowlist proxy is the only
///   reachable port, and its allow/deny tally becomes the run's egress evidence.
#[cfg(target_os = "macos")]
fn run_supervised(
    policy: &crate::sandbox::ResolvedPolicy,
    work: &std::path::Path,
    argv: &[String],
    injected_env: &[(String, String)],
    interactive: bool,
) -> Result<crate::sandbox::ExecOutcome, H5iError> {
    use crate::auth_proxy::LOOPBACK_HOST;

    let has_egress = !policy.profile.net_egress.is_empty();

    // Credential-injecting auth proxy (option 2). `tier_ok` is true because the
    // box shares the host's loopback and the profile will open exactly this port.
    let auth = if has_egress {
        crate::auth_proxy::engage_at(&policy.profile.name, true, LOOPBACK_HOST)?
    } else {
        None
    };
    let (_auth_proxy, effective_env, auth_port) = match auth {
        Some(e) => {
            scrub_box_credentials(policy, e.runtime);
            let port = e.handle.port;
            let mut env = injected_env.to_vec();
            env.extend(e.box_env);
            (Some(e.handle), env, Some(port))
        }
        None => (None, injected_env.to_vec(), None),
    };

    // The general egress allowlist proxy. Skipped entirely when the credential
    // proxy is engaged: the box then has no business reaching anything else, and
    // opening a second port would widen it (this mirrors `setup_egress`, which
    // narrows the nftables ruleset to the auth proxy alone in that case).
    let mut env = effective_env;
    let (_egress_proxy, egress_port) = if has_egress && auth_port.is_none() {
        // Through `effective_egress`, like every other tier: parsing the profile
        // list directly meant `h5i box allow` extras applied on container and
        // microvm but silently did nothing here.
        let mut allow = crate::container::AllowList::parse(&crate::container::effective_egress(
            &policy.profile.net_egress,
            &policy.user_egress_allow,
        ))?;
        // Pin now, so a later DNS answer cannot move the allowlist under us.
        allow.pin_dns();
        // On the pinned port when the box has one: a `browser` box's Chrome
        // outlives this run and keeps dialing whatever address it was launched
        // with (see `ResolvedPolicy::egress_proxy_port`).
        let handle = crate::container::spawn_proxy_on(allow, policy.egress_proxy_port)?;
        let port = handle.port;
        let url = format!("http://{LOOPBACK_HOST}:{port}");
        for var in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy", "ALL_PROXY"] {
            env.push((var.to_string(), url.clone()));
        }
        // h5i's own name for the same address, for the in-box tooling that has to
        // be *told* about the proxy rather than reading the conventional vars
        // (the browser shim's `--proxy-server`). Those vars are ordinary shell
        // state a box's rc or tooling may set for its own reasons; this one is
        // set by the tier that actually runs the proxy and by nothing else, so a
        // consumer keying off it is reading a fact rather than a convention.
        env.push((crate::container::EGRESS_PROXY_VAR.to_string(), url));
        // The proxy itself is on loopback; without this a client would try to
        // reach the proxy *through* the proxy.
        env.push(("NO_PROXY".into(), "localhost,127.0.0.1".into()));
        env.push(("no_proxy".into(), "localhost,127.0.0.1".into()));
        (Some(handle), Some(port))
    } else {
        (None, None)
    };

    let ports: Vec<u16> = [auth_port, egress_port].into_iter().flatten().collect();
    let opts = crate::sandbox::seatbelt_opts(interactive, &ports);

    let outcome = if interactive {
        let code = crate::seatbelt::run_interactive(policy, work, argv, &env, &opts)?;
        crate::sandbox::ExecOutcome {
            stdout: Vec::new(),
            stderr: Vec::new(),
            exit_code: Some(code),
            timed_out: false,
            wall_ms: 0,
            cpu_ms: 0,
            max_rss_kb: None,
            egress: None,
        }
    } else {
        crate::seatbelt::run(policy, work, argv, &env, &opts)?
    };

    // Snapshot the allowlist proxy's verdicts before its handle drops, so a
    // supervised macOS run leaves the same egress evidence a container run does.
    Ok(crate::sandbox::ExecOutcome {
        egress: _egress_proxy.as_ref().map(|h| h.egress_summary()),
        ..outcome
    })
}

#[cfg(not(any(
    all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")),
    target_os = "macos"
)))]
fn run_supervised(
    _policy: &crate::sandbox::ResolvedPolicy,
    _work: &std::path::Path,
    _argv: &[String],
    _injected_env: &[(String, String)],
    _interactive: bool,
) -> Result<crate::sandbox::ExecOutcome, H5iError> {
    Err(H5iError::Metadata(
        "isolation=supervised requires Linux + x86_64/aarch64 (seccomp user-notif) or macOS \
         (Seatbelt)"
            .into(),
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

        // Everything else denies. Raw/packet, IPPROTO_RAW, non-inet families.
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
    fn socketpair_gate_allows_anonymous_unix_pairs() {
        const AF_UNIX: i32 = 1;
        const AF_INET: i32 = 2;
        const AF_PACKET: i32 = 17;
        const SOCK_STREAM: i32 = 1;
        const SOCK_DGRAM: i32 = 2;
        const SOCK_RAW: i32 = 3;
        const SOCK_CLOEXEC: i32 = 0o2000000;

        // An anonymous AF_UNIX pair is pure intra-box IPC. Allowed without a
        // grant (tokio signals / Node child IPC depend on it).
        assert_eq!(decide_socketpair(AF_UNIX, SOCK_STREAM, 0, false), Decision::Continue);
        assert_eq!(
            decide_socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, false),
            Decision::Continue
        );
        assert_eq!(decide_socketpair(AF_UNIX, SOCK_DGRAM, 0, false), Decision::Continue);

        // Everything else keeps the default-deny socket gate's verdicts.
        assert_eq!(decide_socketpair(AF_PACKET, SOCK_DGRAM, 0, false), Decision::Deny(libc::EPERM));
        assert_eq!(decide_socketpair(AF_INET, SOCK_RAW, 0, false), Decision::Deny(libc::EPERM));
        assert_eq!(decide_socketpair(999, 999, 0, false), Decision::Deny(libc::EPERM));
        // inet socketpair is kernel-rejected anyway; the gate's verdict matches
        // decide_socket so nothing widens.
        assert_eq!(decide_socketpair(AF_INET, SOCK_STREAM, 0, false), Decision::Continue);
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

    /// The policy an operator reads is a hostname; the thing nftables enforces
    /// is whatever it resolved to. A repo ships `.h5i/env.toml`, so it picks
    /// the names *and* what they answer, and `169.254.169.254` is the cloud
    /// instance metadata service, reachable through slirp's NAT, invisible in
    /// the policy text, and worth the instance's credentials.
    #[test]
    fn an_egress_name_cannot_pin_to_somewhere_no_host_lives() {
        let never: &[IpAddr] = &[
            // The one that matters.
            "169.254.169.254".parse().unwrap(),
            // The rest of the link-local range, and the v6 spelling of it.
            "169.254.1.1".parse().unwrap(),
            "fe80::1".parse().unwrap(),
            // Same address as the first, written so a v4-only check misses it.
            "::ffff:169.254.169.254".parse().unwrap(),
            "224.0.0.1".parse().unwrap(),
            "ff02::1".parse().unwrap(),
            "255.255.255.255".parse().unwrap(),
            "0.0.0.0".parse().unwrap(),
            "::".parse().unwrap(),
        ];
        for ip in never {
            assert!(!is_pinnable(ip), "{ip} must not be pinnable");
        }

        // And the ones that must keep working, or this refuses boxes that work:
        // an ordinary internet host, a company mirror on RFC1918, its v6
        // equivalent, and the box's own loopback (already `oif lo accept`, and
        // named by services the box itself runs).
        let fine: &[IpAddr] = &[
            "93.184.216.34".parse().unwrap(),
            "2606:2800:220:1:248:1893:25c8:1946".parse().unwrap(),
            "10.1.2.3".parse().unwrap(),
            "192.168.1.10".parse().unwrap(),
            "172.16.0.1".parse().unwrap(),
            "fd00::1".parse().unwrap(),
            "127.0.0.1".parse().unwrap(),
            "::1".parse().unwrap(),
        ];
        for ip in fine {
            assert!(is_pinnable(ip), "{ip} must stay pinnable");
        }
    }

    /// Unpinnable is not one thing. `0.0.0.0` is what a filtering resolver
    /// (pi-hole, a corporate policy, a consumer ISP) answers for a name it
    /// blocks, so it turns up on ordinary well-meaning entries, and refusing
    /// the run over it would take the whole box down on the operator's DNS,
    /// over a name that was already unreachable. `169.254.169.254` is the
    /// opposite: nothing legitimate answers there, and it stays fatal.
    #[test]
    fn a_sinkholed_answer_and_a_metadata_answer_are_not_the_same_refusal() {
        for ip in ["0.0.0.0", "::", "::ffff:0.0.0.0"] {
            let ip: IpAddr = ip.parse().unwrap();
            assert!(!is_pinnable(&ip), "{ip} must still never be pinned");
            assert!(is_sinkhole(&ip), "{ip} is a resolver saying 'blocked'");
        }
        for ip in ["169.254.169.254", "::ffff:169.254.169.254", "fe80::1", "224.0.0.1"] {
            let ip: IpAddr = ip.parse().unwrap();
            assert!(!is_sinkhole(&ip), "{ip} must stay a hard refusal, not a sinkhole");
        }

        // End to end through `resolve_egress`, which sorts them into the two
        // lists `setup_egress` treats differently. IP literals go through the
        // same path as a resolved name and need no DNS to test.
        let r = resolve_egress(&["0.0.0.0".into(), "169.254.169.254".into(), "127.0.0.1".into()]);
        assert_eq!(r.sinkholed.len(), 1, "{:?}", r.sinkholed);
        assert_eq!(r.sinkholed[0].0, "0.0.0.0");
        assert_eq!(r.refused.len(), 1, "{:?}", r.refused);
        assert_eq!(r.refused[0].0, "169.254.169.254");
        // Neither is pinned, either way, and the good entry is untouched.
        assert_eq!(r.dests, vec![EgressDest { ip: "127.0.0.1".parse().unwrap(), port: 443 }]);
        assert!(r.host_pins.is_empty(), "IP literals need no /etc/hosts pin");
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
        // cannot satisfy the supervised stack. Never a silent partial run.
        // (On a fully-capable host the e2e test in tests/env_integration.rs
        // proves real enforcement.)
        let mut p = crate::sandbox::Profile::builtin("p", crate::sandbox::IsolationClaim::Supervised);
        p.net_egress = vec!["example.com".into()];
        let pol = crate::sandbox::ResolvedPolicy::new(p.isolation, p);
        // Ask the admission check itself whether this host is capable, rather
        // than re-deriving it from `slirp4netns`. What makes a host ready is
        // per-platform now, macOS needs no uplink binary at all, so a
        // hand-rolled predicate here would drift from the code it guards, which
        // is exactly what it did.
        if preflight(&pol).is_ok() {
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

    /// The netns uplink is Linux-only machinery (`slirp_args`, `SLIRP_GATEWAY`
    /// are `cfg(target_os = "linux")`), so its test is too. macOS reaches the
    /// same guarantee through the loopback proxy instead. See
    /// `seatbelt::tests::egress_allows_only_the_proxy_port`.
    #[cfg(target_os = "linux")]
    #[test]
    fn slirp_args_toggle_host_loopback() {
        // Airtight default: host-loopback disabled.
        let off = slirp_args(4321, false);
        assert!(off.contains(&"--disable-host-loopback".to_string()));
        // Auth proxy engaged: host-loopback allowed so the box can reach the
        // host proxy via the gateway.
        let on = slirp_args(4321, true);
        assert!(!on.contains(&"--disable-host-loopback".to_string()));
        // Common structure preserved either way.
        for a in [&off, &on] {
            assert_eq!(a.first().unwrap(), "--configure");
            assert!(a.contains(&"4321".to_string()), "pid passed through");
            assert!(a.contains(&"tap0".to_string()));
            assert!(a.iter().any(|s| s == "--mtu=65520"));
        }
    }

    /// The netns uplink is Linux-only machinery (`slirp_args`, `SLIRP_GATEWAY`
    /// are `cfg(target_os = "linux")`), so its test is too. macOS reaches the
    /// same guarantee through the loopback proxy instead. See
    /// `seatbelt::tests::egress_allows_only_the_proxy_port`.
    #[cfg(target_os = "linux")]
    #[test]
    fn proxy_only_egress_ruleset_allows_just_the_gateway_port() {
        assert_eq!(SLIRP_GATEWAY, "10.0.2.2".parse::<IpAddr>().unwrap());
        // What setup_egress builds when the auth proxy is engaged: a single
        // accept for the host proxy at the gateway, default-drop for the rest.
        let rs = build_nft_ruleset(&[EgressDest { ip: SLIRP_GATEWAY, port: 8080 }], None);
        assert!(rs.contains("policy drop;"), "must default-drop:\n{rs}");
        assert!(rs.contains("ip daddr 10.0.2.2 tcp dport 8080 accept"), "{rs}");
        assert!(rs.contains("oif \"lo\" accept"));
        // No DNS (port 53) and no other external destination is opened.
        assert!(!rs.contains("dport 53"), "proxy-only egress needs no resolver:\n{rs}");
        assert!(!rs.contains("dport 443"), "direct API egress must NOT be opened:\n{rs}");
    }

    #[test]
    fn cred_scrub_targets_backing_copy_not_real_home() {
        use crate::sandbox_policy::{AgentRuntime, HomeBind};
        use std::path::PathBuf;
        let binds = vec![
            HomeBind {
                backing: PathBuf::from("/env/home/.claude"),
                target: PathBuf::from("/home/u/.claude"),
            },
            // The `~/.claude.json` bind must NOT be matched (token isn't there).
            HomeBind {
                backing: PathBuf::from("/env/home/.claude.json"),
                target: PathBuf::from("/home/u/.claude.json"),
            },
        ];
        let paths = cred_scrub_paths(AgentRuntime::Claude, &binds);
        assert_eq!(paths, vec![PathBuf::from("/env/home/.claude/.credentials.json")]);
        // Only the env's own backing copy is ever named. Never the real HOME.
        assert!(paths.iter().all(|p| p.starts_with("/env/home")));

        let codex = vec![HomeBind {
            backing: PathBuf::from("/env/home/.codex"),
            target: PathBuf::from("/home/u/.codex"),
        }];
        assert_eq!(
            cred_scrub_paths(AgentRuntime::Codex, &codex),
            vec![PathBuf::from("/env/home/.codex/auth.json")]
        );
        // No matching bind → nothing scrubbed.
        assert!(cred_scrub_paths(AgentRuntime::Claude, &codex).is_empty());
    }
}
