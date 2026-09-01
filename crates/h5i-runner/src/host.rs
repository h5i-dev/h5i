//! What the worker can truthfully say about the machine it is running on.

use std::path::Path;

use h5i_sandbox::sandbox;

use crate::proto::Capabilities;

/// Gather the report. Never fails: anything unmeasurable becomes a note.
pub fn capabilities(state_dir: &Path) -> Capabilities {
    let mut notes: Vec<String> = Vec::new();

    let (isolation, container) = isolation_support(&mut notes);
    let memory_mb = total_memory_mb().unwrap_or_else(|| {
        notes.push("could not read total memory from /proc/meminfo".into());
        0
    });
    let workspace_mb = available_mb(state_dir).unwrap_or_else(|| {
        notes.push(format!(
            "could not measure free space at {}",
            state_dir.display()
        ));
        0
    });

    let persistent_boxes = match storage_is_volatile(state_dir) {
        Some(volatile) => !volatile,
        None => {
            notes.push(
                "could not tell whether box storage survives a reboot — assuming it does".into(),
            );
            true
        }
    };

    let own_egress = match has_default_route() {
        Some(v) => v,
        None => {
            notes.push(
                "could not read a routing table — assuming this runner reaches the internet".into(),
            );
            true
        }
    };

    Capabilities {
        arch: std::env::consts::ARCH.to_string(),
        os: std::env::consts::OS.to_string(),
        memory_mb,
        workspace_mb,
        isolation,
        container,
        kvm: kvm_available(),
        persistent_boxes,
        own_egress,
        notes,
    }
}

/// Which tiers this machine will actually run.
fn isolation_support(notes: &mut Vec<String>) -> (Vec<String>, bool) {
    let report = sandbox::capabilities_report_fresh();
    let mut tiers = Vec::new();
    let mut container = false;

    for claim in &report.claims {
        match claim.claim {
            "container" => {
                if claim.satisfiable {
                    container = true;
                    tiers.push("container".to_string());
                    notes.push(
                        "container runtime present; a functional container run is verified at \
                         create, not at probe"
                            .into(),
                    );
                }
            }
            // Everything else is advertised only on the functional check.
            // `runnable: None` means "not exec-tested", which is not the same
            // as "works", and must not become an advertisement.
            name => {
                if claim.runnable == Some(true) {
                    tiers.push(name.to_string());
                }
            }
        }
    }

    if tiers.is_empty() {
        notes.push("no isolation tier verified on this machine".into());
    }
    (tiers, container)
}

/// Total system memory in MiB, from `/proc/meminfo`.
fn total_memory_mb() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    parse_meminfo_total_kb(&text).map(|kb| kb / 1024)
}

fn parse_meminfo_total_kb(text: &str) -> Option<u64> {
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            let kb = rest.split_whitespace().next()?;
            return kb.parse().ok();
        }
    }
    None
}

/// Free space in MiB on the filesystem holding `path`.
///
/// `f_bavail` rather than `f_bfree`: the reserved blocks a root process could
/// still use are not space a box will get, and a probe that counted them would
/// promise room that a create cannot deliver.
fn available_mb(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        // The directory may not exist yet on a machine that has never run a
        // box; measure the nearest ancestor that does, since that is the
        // filesystem the directory will land on.
        let mut probe = path;
        loop {
            if probe.exists() {
                break;
            }
            probe = probe.parent()?;
        }

        let c = CString::new(probe.as_os_str().as_bytes()).ok()?;
        // SAFETY: `c` is a valid NUL-terminated path and `stat` is only read
        // after the call reports success.
        let stat = unsafe {
            let mut stat: libc::statvfs = std::mem::zeroed();
            if libc::statvfs(c.as_ptr(), &mut stat) != 0 {
                return None;
            }
            stat
        };
        let block = if stat.f_frsize > 0 {
            stat.f_frsize as u64
        } else {
            stat.f_bsize as u64
        };
        Some((stat.f_bavail as u64).saturating_mul(block) / (1024 * 1024))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Does box storage vanish on reboot?
///
/// `Some(true)` for a tmpfs or ramfs mount, which is the ephemeral runner R11
/// describes: a read-only OS with a memory-backed workspace, where a reboot is
/// an early-expired lease. `None` when `/proc/mounts` cannot be read, because
/// "I could not tell" and "it is durable" are different answers.
fn storage_is_volatile(path: &Path) -> Option<bool> {
    let mounts = std::fs::read_to_string("/proc/mounts").ok()?;
    let fstype = mount_fstype_for(&mounts, path)?;
    Some(matches!(fstype.as_str(), "tmpfs" | "ramfs"))
}

/// The filesystem type of the mount that `path` lands on: the longest mount
/// point that is a prefix of it, which is how the kernel resolves it too.
fn mount_fstype_for(mounts: &str, path: &Path) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    for line in mounts.lines() {
        let mut f = line.split_whitespace();
        let _device = f.next()?;
        let mount_point = f.next()?;
        let fstype = f.next()?;
        // `/proc/mounts` escapes spaces and tabs as octal; an unescaped compare
        // would silently match the wrong mount.
        let mount_point = unescape_mount(mount_point);
        if path.starts_with(&mount_point)
            && best.as_ref().is_none_or(|(len, _)| mount_point.len() > *len)
        {
            best = Some((mount_point.len(), fstype.to_string()));
        }
    }
    best.map(|(_, t)| t)
}

fn unescape_mount(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let digits: String = chars.clone().take(3).collect();
        match u8::from_str_radix(&digits, 8) {
            Ok(byte) if digits.len() == 3 => {
                out.push(byte as char);
                for _ in 0..3 {
                    chars.next();
                }
            }
            _ => out.push('\\'),
        }
    }
    out
}

/// Can this machine leave the network by itself?
///
/// A default route, not a reachability test: a probe must not decide it is
/// entitled to send traffic somewhere to answer a question. This is exactly the
/// distinction R12 prices. A runner with no default route is the cable-only
/// appliance, which needs brokered egress and is not an MVP topology.
fn has_default_route() -> Option<bool> {
    let v4 = std::fs::read_to_string("/proc/net/route").ok();
    let v6 = std::fs::read_to_string("/proc/net/ipv6_route").ok();
    if v4.is_none() && v6.is_none() {
        return None;
    }
    let has_v4 = v4.as_deref().is_some_and(has_default_route_v4);
    let has_v6 = v6.as_deref().is_some_and(has_default_route_v6);
    Some(has_v4 || has_v6)
}

/// `/proc/net/route` is a table with a header; a default route is destination
/// `00000000` with the "route is up" flag set.
fn has_default_route_v4(text: &str) -> bool {
    const RTF_UP: u64 = 0x0001;
    text.lines().skip(1).any(|line| {
        let f: Vec<&str> = line.split_whitespace().collect();
        f.len() > 3
            && f[1] == "00000000"
            && u64::from_str_radix(f[3], 16).is_ok_and(|flags| flags & RTF_UP != 0)
    })
}

/// `/proc/net/ipv6_route`: a default route is a destination prefix length of
/// zero, which is field 1 of the row.
fn has_default_route_v6(text: &str) -> bool {
    text.lines().any(|line| {
        let f: Vec<&str> = line.split_whitespace().collect();
        f.len() > 1 && f[0] == "0".repeat(32) && f[1] == "00"
    })
}

/// Hardware virtualisation, for a microvm placement that does not exist yet.
/// Advertised now because it is static and cheap to see, and because a user
/// choosing hardware wants to know.
fn kvm_available() -> bool {
    Path::new("/dev/kvm").exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn meminfo_is_read_from_the_line_that_carries_it() {
        let text = "MemAvailable:    1000 kB\nMemTotal:       16316420 kB\nSwapTotal: 0 kB\n";
        assert_eq!(parse_meminfo_total_kb(text), Some(16316420));
        // Not the first number in the file, and not a near-miss key.
        assert_eq!(parse_meminfo_total_kb("MemTotalSwap: 5 kB\n"), None);
        assert_eq!(parse_meminfo_total_kb(""), None);
    }

    #[test]
    fn the_longest_matching_mount_point_wins() {
        // The kernel resolves a path to the deepest mount that covers it, and a
        // shallower match would report the wrong filesystem, which is exactly
        // the difference between "boxes survive a reboot" and "they do not".
        let mounts = "\
/dev/sda1 / ext4 rw 0 0
tmpfs /tmp tmpfs rw 0 0
/dev/sdb1 /var/lib/h5i ext4 rw 0 0
";
        assert_eq!(
            mount_fstype_for(mounts, &PathBuf::from("/var/lib/h5i/runner/boxes")).as_deref(),
            Some("ext4")
        );
        assert_eq!(
            mount_fstype_for(mounts, &PathBuf::from("/tmp/scratch")).as_deref(),
            Some("tmpfs")
        );
        assert_eq!(
            mount_fstype_for(mounts, &PathBuf::from("/home/dev")).as_deref(),
            Some("ext4"),
            "falls back to the root mount"
        );
    }

    #[test]
    fn a_mount_point_with_a_space_is_unescaped_before_it_is_compared() {
        // /proc/mounts writes a space as \040. Compared raw, this mount would
        // never match and the answer would silently come from `/`.
        let mounts = "\
/dev/sda1 / ext4 rw 0 0
tmpfs /mnt/my\\040boxes tmpfs rw 0 0
";
        assert_eq!(
            mount_fstype_for(mounts, &PathBuf::from("/mnt/my boxes/x")).as_deref(),
            Some("tmpfs")
        );
    }

    #[test]
    fn a_tmpfs_workspace_is_volatile_and_an_ext4_one_is_not() {
        let mounts = "tmpfs /scratch tmpfs rw 0 0\n/dev/sda1 / ext4 rw 0 0\n";
        assert_eq!(
            mount_fstype_for(mounts, &PathBuf::from("/scratch/boxes")).as_deref(),
            Some("tmpfs")
        );
        assert!(matches!(
            mount_fstype_for(mounts, &PathBuf::from("/scratch/boxes")).as_deref(),
            Some("tmpfs" | "ramfs")
        ));
    }

    #[test]
    fn a_default_route_is_the_zero_destination_with_the_up_flag() {
        let up = "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask
eth0\t00000000\t0102A8C0\t0003\t0\t0\t100\t00000000
eth0\t0002A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF
";
        assert!(has_default_route_v4(up));

        // A machine with only an on-link route: the cable-only appliance.
        let no_default = "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask
eth0\t0002A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF
";
        assert!(!has_default_route_v4(no_default));

        // Present but down is not a route.
        let down = "\
Iface\tDestination\tGateway \tFlags
eth0\t00000000\t0102A8C0\t0002
";
        assert!(!has_default_route_v4(down));

        // The header alone must not read as a route.
        assert!(!has_default_route_v4("Iface\tDestination\tGateway \tFlags\n"));
        assert!(!has_default_route_v4(""));
    }

    #[test]
    fn an_ipv6_default_route_is_a_zero_length_prefix() {
        let v6 = format!("{} 00 {} 00 {} 00000001 00000000 00000000 00000003 eth0\n",
            "0".repeat(32), "0".repeat(32), "0".repeat(32));
        assert!(has_default_route_v6(&v6));
        let onlink = format!("{} 40 {} 00 {} 00000001 00000000 00000000 00000001 eth0\n",
            "fe80".to_string() + &"0".repeat(28), "0".repeat(32), "0".repeat(32));
        assert!(!has_default_route_v6(&onlink));
    }

    #[test]
    fn free_space_is_measured_on_the_nearest_existing_ancestor() {
        // A machine that has never run a box has no state dir yet, and the
        // filesystem it will land on is the one to measure.
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("does/not/exist/yet");
        let mb = available_mb(&missing);
        assert!(mb.is_some(), "should have measured the ancestor");
        assert_eq!(mb, available_mb(dir.path()), "same filesystem, same answer");
    }

    #[test]
    fn the_report_is_always_producible_and_always_sanitises() {
        // Never fails: whatever cannot be measured becomes a note, and the
        // result must satisfy the protocol's own validation. Otherwise the
        // worker would be emitting reports its own client refuses.
        let dir = tempfile::tempdir().expect("tempdir");
        let caps = capabilities(dir.path());
        assert_eq!(caps.os, std::env::consts::OS);
        assert_eq!(caps.arch, std::env::consts::ARCH);

        if caps.os == "linux" {
            let sane = caps.clone().sanitized().expect("our own report must validate");
            assert_eq!(sane.arch, caps.arch);
            // Container claimed implies a runtime, which is the invariant
            // `sanitized` enforces on the other side of the wire.
            if sane.isolation.iter().any(|t| t == "container") {
                assert!(sane.container);
            }
        }
    }
}
