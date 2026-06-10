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

/// Resolve `net.egress` entries (`host`, `host:port`, defaulting to 443) to
/// pinned `IP:port` destinations. A host that fails to resolve contributes
/// nothing (fail-closed: it simply won't be reachable). Pure apart from DNS.
pub fn pin_egress(egress: &[String]) -> Vec<EgressDest> {
    use std::net::ToSocketAddrs;
    let mut out = Vec::new();
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
        if let Ok(addrs) = (host, port).to_socket_addrs() {
            for a in addrs {
                let dest = EgressDest { ip: a.ip(), port };
                if !out.contains(&dest) {
                    out.push(dest);
                }
            }
        }
    }
    out
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

// ─── run (phase A: fail-closed) ───────────────────────────────────────────────

/// Run under the supervised tier. Phase A: enforcement is not wired, so this
/// **fails closed** — it must never execute an untrusted command without the
/// full mediation stack. `resolve` already refuses the claim before reaching
/// here on every current host; this is defense in depth.
pub fn run(
    _policy: &crate::sandbox::ResolvedPolicy,
    _work: &std::path::Path,
    _argv: &[String],
    _injected_env: &[(String, String)],
) -> Result<crate::sandbox::ExecOutcome, H5iError> {
    let caps = probe();
    Err(H5iError::Metadata(format!(
        "isolation=supervised: live enforcement (netns+nftables + seccomp-notify loop) is not \
         wired in this build (design phase B). The tier fails closed rather than run untrusted \
         code unmediated.{}",
        if caps.usable {
            String::new()
        } else {
            format!(" Host is also missing: {}.", caps.missing().join(", "))
        }
    )))
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
    fn run_fails_closed() {
        // Even if somehow reached, run() must never execute unmediated.
        let p = crate::sandbox::Profile::builtin("p", crate::sandbox::IsolationClaim::Supervised);
        let pol = crate::sandbox::ResolvedPolicy { claim: p.isolation, profile: p };
        let dir = std::env::temp_dir();
        let err = run(&pol, &dir, &["true".to_string()], &[]).unwrap_err();
        assert!(format!("{err}").contains("fails closed") || format!("{err}").contains("phase B"));
    }
}
