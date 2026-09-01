//! What this host can actually do, asked rather than assumed.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use serde::{Deserialize, Serialize};

/// The kernel this collector needs, and why: `BPF_MAP_TYPE_RINGBUF` landed in
/// 5.8. Older kernels get a refusal, never a silent fall back to perf buffers.
/// A quieter, lossier transport that would make the numbers in a receipt
/// mean something different without saying so.
pub const MIN_KERNEL: (u32, u32) = (5, 8);

/// `CAP_PERFMON`, `CAP_BPF`: the two the loader needs on a modern kernel.
/// `CAP_SYS_ADMIN` subsumes both and is what a root process has.
const CAP_SYS_ADMIN: u32 = 21;
const CAP_PERFMON: u32 = 38;
const CAP_BPF: u32 = 39;

/// What the host offers the runtime-detection lane.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BpfCaps {
    pub os: String,
    /// `major.minor.rest`, verbatim from the kernel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel: Option<String>,
    /// Whether [`MIN_KERNEL`] is met.
    pub ringbuf: bool,
    /// This build carries a compiled probe. False when `clang` could not
    /// target BPF at build time, or when the `load` feature is off.
    pub object: bool,
    /// `CAP_BPF` (or `CAP_SYS_ADMIN`) is in this process's effective set.
    pub cap_bpf: bool,
    /// `CAP_PERFMON` (or `CAP_SYS_ADMIN`) is in this process's effective set.
    pub cap_perfmon: bool,
    /// `/sys/kernel/btf/vmlinux` exists. *Not* required, this probe is
    /// CO-RE-free (design-detect.md D5), and reported because its absence is
    /// the first thing anyone familiar with other eBPF tools will ask about.
    pub kernel_btf: bool,
    /// `/sys/kernel/tracing/events` is readable, so tracepoint `format` files
    /// can be parsed and the probe's assumed field offsets verified rather
    /// than trusted. Not required either; its absence downgrades a check, not
    /// the collector.
    pub tracefs: bool,
    /// The whole question, answered once.
    pub usable: bool,
    /// Why not, when not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The command that would fix it, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

impl BpfCaps {
    /// The one-line reason a run would carry in its `runtime` block. `None`
    /// when the detector can attach.
    pub fn unavailable_reason(&self) -> Option<String> {
        if self.usable {
            None
        } else {
            Some(
                self.detail
                    .clone()
                    .unwrap_or_else(|| "runtime detection unavailable".to_string()),
            )
        }
    }
}

/// True when this build carries a compiled probe object.
pub const fn has_object() -> bool {
    cfg!(all(target_os = "linux", feature = "load", h5i_bpf_object))
}

/// Ask the host. Cheap: three small file reads, no privileged syscall, no
/// process spawn, so it is safe to call from a status path.
pub fn probe() -> BpfCaps {
    #[cfg(target_os = "linux")]
    {
        probe_linux()
    }
    #[cfg(not(target_os = "linux"))]
    {
        BpfCaps {
            os: std::env::consts::OS.to_string(),
            usable: false,
            detail: Some(format!(
                "eBPF is a Linux facility; this host is {}. The confinement tier on this \
                 platform is Seatbelt, which is enforcement rather than observation, and there \
                 is no equivalent lane to offer here.",
                std::env::consts::OS
            )),
            ..Default::default()
        }
    }
}

#[cfg(target_os = "linux")]
fn probe_linux() -> BpfCaps {
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .ok()
        .map(|s| s.trim().to_string());
    let version = kernel.as_deref().and_then(parse_kernel);
    let ringbuf = version.map(|v| v >= MIN_KERNEL).unwrap_or(false);
    let (cap_bpf, cap_perfmon) = effective_caps();
    let object = has_object();

    let mut caps = BpfCaps {
        os: "linux".into(),
        kernel,
        ringbuf,
        object,
        cap_bpf,
        cap_perfmon,
        kernel_btf: std::path::Path::new("/sys/kernel/btf/vmlinux").exists(),
        tracefs: tracefs_readable(),
        usable: false,
        detail: None,
        fix: None,
    };

    // Ordered by what a user can do about it: a missing object is a build
    // problem, a missing capability is a one-command problem, an old kernel is
    // neither.
    if !object {
        caps.detail = Some(
            "this build carries no eBPF probe — it was compiled without the `bpf` feature, or \
             with no clang that can target BPF"
                .into(),
        );
        caps.fix = Some("cargo install --path . --features bpf".into());
        return caps;
    }
    if !ringbuf {
        caps.detail = Some(format!(
            "kernel {} is older than {}.{}, which is where BPF_MAP_TYPE_RINGBUF landed",
            caps.kernel.as_deref().unwrap_or("(unknown)"),
            MIN_KERNEL.0,
            MIN_KERNEL.1
        ));
        return caps;
    }
    if !cap_bpf || !cap_perfmon {
        let missing = match (cap_bpf, cap_perfmon) {
            (false, false) => "CAP_BPF and CAP_PERFMON",
            (false, true) => "CAP_BPF",
            _ => "CAP_PERFMON",
        };
        caps.detail = Some(format!(
            "h5i is missing {missing}; loading a BPF program and attaching it to a tracepoint \
             both require them"
        ));
        // The binary's own path, so the command can be pasted. h5i never runs
        // this itself: granting capabilities to a process is the user's
        // decision, and asking for them silently is what this product exists
        // to stop other software doing.
        let exe = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "$(command -v h5i)".into());
        caps.fix = Some(format!("sudo setcap cap_bpf,cap_perfmon=ep {exe}"));
        return caps;
    }

    caps.usable = true;
    caps
}

/// `5.15.0-91-generic` → `(5, 15)`.
fn parse_kernel(s: &str) -> Option<(u32, u32)> {
    let mut it = s.split(['.', '-', '+']);
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    Some((major, minor))
}

/// Read the effective capability set out of `/proc/self/status`.
///
/// `capget(2)` would be the direct answer and needs no parsing, but it is a
/// raw syscall with a versioned struct, and this is a diagnostic: the cost of
/// getting the struct subtly wrong is worse than the cost of parsing a hex
/// number. Returns `(cap_bpf, cap_perfmon)`, with `CAP_SYS_ADMIN` counting for
/// both, which is what makes running as root work.
#[cfg(target_os = "linux")]
fn effective_caps() -> (bool, bool) {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return (false, false);
    };
    let Some(line) = status.lines().find_map(|l| l.strip_prefix("CapEff:")) else {
        return (false, false);
    };
    let Ok(mask) = u64::from_str_radix(line.trim(), 16) else {
        return (false, false);
    };
    let has = |bit: u32| mask & (1u64 << bit) != 0;
    let admin = has(CAP_SYS_ADMIN);
    (admin || has(CAP_BPF), admin || has(CAP_PERFMON))
}

#[cfg(target_os = "linux")]
fn tracefs_readable() -> bool {
    ["/sys/kernel/tracing/events", "/sys/kernel/debug/tracing/events"]
        .iter()
        .any(|p| std::fs::read_dir(p).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_versions_parse() {
        assert_eq!(parse_kernel("5.15.0-91-generic"), Some((5, 15)));
        assert_eq!(parse_kernel("6.6.87.2-microsoft-standard-WSL2"), Some((6, 6)));
        assert_eq!(parse_kernel("4.19.0"), Some((4, 19)));
        assert_eq!(parse_kernel("nonsense"), None);
        assert_eq!(parse_kernel(""), None);
    }

    #[test]
    fn the_ringbuf_floor_is_where_ringbuf_landed() {
        assert!((5, 8) >= MIN_KERNEL);
        assert!((6, 6) >= MIN_KERNEL);
        assert!((5, 4) < MIN_KERNEL);
        assert!((4, 19) < MIN_KERNEL);
    }

    /// The probe must never panic or block: it is called from `box status`,
    /// from the console's probe route, and from the run path.
    #[test]
    fn probing_is_safe_to_call_anywhere() {
        let c = probe();
        assert!(!c.os.is_empty());
        if !c.usable {
            assert!(c.detail.is_some(), "an unusable host must say why");
        }
    }

    /// The unusable answer has to be actionable where it can be. A missing
    /// capability is the common case on a real install, and "unavailable" with
    /// no next step is how a security feature stays off forever.
    #[test]
    fn a_fixable_refusal_carries_its_fix() {
        let c = probe();
        if !c.usable && c.object && c.ringbuf {
            assert!(c.fix.is_some(), "a capability refusal must name the command");
        }
    }

    #[test]
    fn caps_round_trip_through_json() {
        let c = probe();
        let j = serde_json::to_string(&c).unwrap();
        let back: BpfCaps = serde_json::from_str(&j).unwrap();
        assert_eq!(c, back);
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn a_non_linux_host_says_so_rather_than_pretending() {
        let c = probe();
        assert!(!c.usable);
        assert!(c.detail.unwrap().contains("Linux"));
    }
}
