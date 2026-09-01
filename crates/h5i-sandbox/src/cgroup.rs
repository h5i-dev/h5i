//! cgroup v2 resource control for the `process` tier (rootless, best-effort).
//!
// Platform-conditional machinery: `probe()` is cross-platform (the dashboard
// reports cgroup availability on any host), but the limit/accounting helpers
// (`parse_count`, `CgroupUsage`, `format_limit`, …) are reached only from the
// `#[cfg(target_os = "linux")]` run path, so they read as dead code on non-Linux
// targets. Allow it module-wide rather than cfg-gating every helper + its tests.
#![allow(dead_code)]
//! The rlimit the process tier falls back to for `mem` (`RLIMIT_DATA`) caps the writable data
//! segment, not resident memory, and is per-process, not per-tree.

#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};

/// What the host supports for cgroup v2 management by *this* (rootless) process.
#[derive(Debug, Clone, Default)]
pub struct CgroupCaps {
    /// cgroup2 is the mounted hierarchy.
    pub v2_mounted: bool,
    /// Controllers available in the delegated subtree (subset of
    /// cpu/memory/pids/io/...). Empty when nothing is delegated.
    pub controllers: Vec<String>,
    /// True iff we actually created+enabled+removed a probe cgroup. The only
    /// honest signal that limits will be enforced. `false` ⇒ fall back.
    ///
    /// Driven by `memory.max`, because that is the write [`try_make_usable`]
    /// proves. See [`Self::procs_enforceable`] for why the other cap needs its
    /// own answer.
    pub usable: bool,
    /// Whether `pids.max` is genuinely writable in a run cgroup.
    ///
    /// Separate from [`Self::usable`] because delegation is *per controller*: a
    /// systemd unit with `Delegate=memory` hands out one and not the other, and
    /// `cgroup.subtree_control` accepts `+memory +pids` partially. The probe used
    /// to prove `memory.max` alone and every caller then treated one boolean as
    /// the answer for both caps, so on such a host a profile's `procs` limit was
    /// silently not applied while `box status` printed it unmarked.
    pub procs_enforceable: bool,
    /// The delegated parent under which run cgroups are created (when usable).
    #[cfg(target_os = "linux")]
    pub parent: Option<PathBuf>,
    /// Why it's not usable (for the dashboard / probe output).
    pub detail: Option<String>,
}

impl CgroupCaps {
    pub fn has(&self, controller: &str) -> bool {
        self.controllers.iter().any(|c| c == controller)
    }
}

/// Render a `memory.max`/`pids.max` value: a number, or `"max"` for unbounded.
pub fn format_limit(v: Option<u64>) -> String {
    v.map(|n| n.to_string()).unwrap_or_else(|| "max".to_string())
}

/// Parse the `usage_usec` line from a `cpu.stat` file (microseconds of CPU).
pub fn parse_cpu_usage_usec(cpu_stat: &str) -> Option<u64> {
    cpu_stat
        .lines()
        .find_map(|l| l.strip_prefix("usage_usec "))
        .and_then(|n| n.trim().parse().ok())
}

/// Parse a single-integer cgroup file (`memory.peak`, `pids.peak`). `"max"`
/// returns `None` (unbounded), as do non-numeric contents.
pub fn parse_count(text: &str) -> Option<u64> {
    let t = text.trim();
    if t == "max" {
        None
    } else {
        t.parse().ok()
    }
}

/// Post-run resource usage read back from a run's cgroup.
#[derive(Debug, Clone, Default)]
pub struct CgroupUsage {
    /// Peak memory in bytes (`memory.peak`), when available.
    pub mem_peak_bytes: Option<u64>,
    /// Total CPU time in microseconds (`cpu.stat usage_usec`).
    pub cpu_usec: Option<u64>,
    /// Peak concurrent pids (`pids.peak`), when available.
    pub pids_peak: Option<u64>,
}

#[cfg(target_os = "linux")]
const CG_ROOT: &str = "/sys/fs/cgroup";

/// Our own cgroup path under the v2 mount, from `/proc/self/cgroup`
/// (`0::<path>`), or `None` if not on a v2 host.
#[cfg(target_os = "linux")]
fn self_cgroup() -> Option<PathBuf> {
    let text = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let rel = text.lines().find_map(|l| l.strip_prefix("0::"))?.trim();
    Some(PathBuf::from(CG_ROOT).join(rel.trim_start_matches('/')))
}

/// The systemd *user-manager* cgroup for the current uid, where systemd
/// delegates `cpu/memory/pids` to the unprivileged user (the standard rootless
/// path: `man systemd.resource-control`, "Delegate"). h5i can create + limit
/// child cgroups *under* this even when its own process sits elsewhere (e.g.
/// parked in a root-owned `/init.scope`), because management only needs write
/// access to the directory, not residency in it. The same thing rootless
/// crun/runc do in cgroupfs mode.
#[cfg(target_os = "linux")]
fn user_service_cgroup() -> Option<PathBuf> {
    let uid = unsafe { libc::geteuid() };
    let path = PathBuf::from(CG_ROOT)
        .join("user.slice")
        .join(format!("user-{uid}.slice"))
        .join(format!("user@{uid}.service"));
    path.is_dir().then_some(path)
}

/// Candidate base cgroups under which to create h5i's managed run cgroups, in
/// priority order: our own cgroup first (correct when h5i is launched inside a
/// delegated user session), then the user-manager service (covers the common
/// case where h5i is parked in a non-delegated cgroup but the user *does* have a
/// delegated subtree).
#[cfg(target_os = "linux")]
fn candidate_bases() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(own) = self_cgroup() {
        v.push(own);
    }
    if let Some(svc) = user_service_cgroup()
        && !v.contains(&svc)
    {
        v.push(svc);
    }
    v
}

/// Detect whether this rootless process can actually manage cgroup v2 limits, by
/// performing a real create → enable-controllers → set-limit → remove probe
/// against each candidate base. Cheap and side-effect-free (the probe cgroup is
/// removed). The first base that passes is the one used at run time.
#[cfg(target_os = "linux")]
pub fn probe() -> CgroupCaps {
    // Process-local only: supervised preflight and run setup may ask the same
    // question in one h5i invocation, while each new command still re-probes.
    // Per-run cgroup creation remains a real operation after this admission.
    static CGROUP_CAPS: std::sync::OnceLock<CgroupCaps> = std::sync::OnceLock::new();
    CGROUP_CAPS.get_or_init(probe_uncached).clone()
}

#[cfg(target_os = "linux")]
fn probe_uncached() -> CgroupCaps {
    let mut caps = CgroupCaps::default();
    if std::fs::metadata(format!("{CG_ROOT}/cgroup.controllers")).is_err() {
        caps.detail = Some("no cgroup2 hierarchy mounted".into());
        return caps;
    }
    caps.v2_mounted = true;

    let bases = candidate_bases();
    if bases.is_empty() {
        caps.detail = Some("could not read /proc/self/cgroup".into());
        return caps;
    }

    let mut last_err = String::from("no delegated, writable cgroup base found");
    for base in &bases {
        let probe = base.join("h5i.probe");
        match try_make_usable(&probe) {
            Ok(proven) => {
                let _ = std::fs::remove_dir(&probe);
                // Controllers the *winning* base can delegate to children.
                if let Ok(s) = std::fs::read_to_string(base.join("cgroup.controllers")) {
                    caps.controllers = s.split_whitespace().map(String::from).collect();
                }
                caps.usable = true;
                // What the probe actually wrote, not what the base advertises:
                // `cgroup.controllers` says what *could* be delegated, and the
                // question here is what was.
                caps.procs_enforceable = proven.pids;
                // The real parent used at run time (created lazily).
                caps.parent = Some(base.join("h5i"));
                return caps;
            }
            Err(e) => {
                let _ = std::fs::remove_dir(&probe);
                last_err = format!("{}: {e}", base.display());
            }
        }
    }
    caps.detail = Some(format!("cgroup delegation unavailable ({last_err})"));
    caps
}

/// Which run-cgroup limits this host actually lets us write.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy)]
struct Proven {
    mem: bool,
    pids: bool,
}

/// Best-effort: create `parent`, enable `+memory +pids` in its subtree_control,
/// create a leaf, and remove the leaf. Proving we can manage run cgroups.
///
/// Both limits are written, not just one. Delegation is per controller, so
/// `Delegate=memory` without `pids` leaves `memory.max` writable and `pids.max`
/// absent, and proving only the first made `usable` the answer to a question it
/// had not asked.
#[cfg(target_os = "linux")]
fn try_make_usable(parent: &Path) -> std::io::Result<Proven> {
    std::fs::create_dir_all(parent)?;
    // Enabling controllers in the parent's subtree_control is what actually
    // requires the base to delegate memory+pids to us; the leaf's memory.max is
    // then writable. This is the step that fails when the base isn't delegated.
    let _ = std::fs::write(parent.join("cgroup.subtree_control"), "+memory +pids");
    let leaf = parent.join("probe-leaf");
    std::fs::create_dir_all(&leaf)?;
    // Each must be genuinely writable for its cap to mean anything. Require
    // the write itself to succeed, not merely that the file exists.
    let proven = Proven {
        mem: std::fs::write(leaf.join("memory.max"), "max").is_ok(),
        pids: std::fs::write(leaf.join("pids.max"), "max").is_ok(),
    };
    let _ = std::fs::remove_dir(&leaf);
    if proven.mem {
        Ok(proven)
    } else {
        Err(std::io::Error::other("controllers not delegated to child cgroup"))
    }
}

/// A run's cgroup: a leaf under the delegated parent, with limits applied. The
/// process tier's child joins it (via [`Self::procs_path`] written in its
/// `pre_exec`); usage is read after the run; the cgroup is removed on drop.
#[cfg(target_os = "linux")]
pub struct ScopedCgroup {
    path: PathBuf,
}

#[cfg(target_os = "linux")]
impl ScopedCgroup {
    /// Create a run cgroup under `parent` and apply `mem`/`procs` limits.
    /// `seq` disambiguates concurrent runs.
    pub fn create(
        parent: &Path,
        seq: u64,
        mem_bytes: Option<u64>,
        max_procs: Option<u64>,
    ) -> std::io::Result<ScopedCgroup> {
        std::fs::create_dir_all(parent)?;
        // Parent must delegate memory+pids to its children.
        let _ = std::fs::write(parent.join("cgroup.subtree_control"), "+memory +pids");
        let path = parent.join(format!("run-{}-{}", std::process::id(), seq));
        std::fs::create_dir_all(&path)?;
        if let Some(m) = mem_bytes {
            std::fs::write(path.join("memory.max"), m.to_string())?;
            // No swap headroom beyond the limit.
            let _ = std::fs::write(path.join("memory.swap.max"), "0");
        }
        if let Some(p) = max_procs {
            // Best-effort, deliberately: a host that delegates `memory` and not
            // `pids` still gets its memory cap, and `limit_support` reports the
            // process cap as unenforced so `box status` marks it rather than
            // claiming it. That is the same answer macOS already gets, and it
            // is the documented one. Refusing the run here would take a
            // working memory limit away over a cap the platform cannot hold.
            let _ = std::fs::write(path.join("pids.max"), p.to_string());
        }
        Ok(ScopedCgroup { path })
    }

    /// Path of this cgroup's `cgroup.procs`: the child writes its own pid here
    /// in `pre_exec` to join the cgroup before it allocates anything.
    pub fn procs_path(&self) -> PathBuf {
        self.path.join("cgroup.procs")
    }

    /// Read back peak memory / cpu / pids after the run.
    pub fn usage(&self) -> CgroupUsage {
        let read = |f: &str| std::fs::read_to_string(self.path.join(f)).ok();
        CgroupUsage {
            mem_peak_bytes: read("memory.peak").and_then(|s| parse_count(&s)),
            cpu_usec: read("cpu.stat").and_then(|s| parse_cpu_usage_usec(&s)),
            pids_peak: read("pids.peak").and_then(|s| parse_count(&s)),
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for ScopedCgroup {
    fn drop(&mut self) {
        // The wall-clock killpg has already reaped the run's process tree, so
        // the cgroup is empty and removable. Best-effort.
        let _ = std::fs::remove_dir(&self.path);
    }
}

/// Non-Linux: cgroup v2 is a Linux feature; always unavailable.
#[cfg(not(target_os = "linux"))]
pub fn probe() -> CgroupCaps {
    CgroupCaps {
        v2_mounted: false,
        controllers: Vec::new(),
        usable: false,
        procs_enforceable: false,
        detail: Some("cgroup v2 is Linux-only".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// cgroup delegation is *per controller*, so one probe result is not the
    /// answer for both caps. `Delegate=memory` without `pids` is a real systemd
    /// configuration: `memory.max` is writable, `pids.max` is not, and the
    /// probe proved only the first, so `limit_support` reported the process
    /// cap as enforced and `box status` printed it unmarked on a host that had
    /// silently not applied it. SECURITY.md: a cap the platform cannot hold is
    /// reported as unenforced rather than listed as applied.
    #[test]
    fn the_two_caps_are_reported_separately_because_delegation_is_separate() {
        // On a host with no cgroup2 at all (every macOS one, and this is what
        // the non-Linux stub answers) neither is claimed.
        let caps = probe();
        if !caps.usable {
            assert!(
                !caps.procs_enforceable,
                "nothing is enforceable when the hierarchy is not usable"
            );
        }
        // And the tier answer never claims a cap the probe did not prove.
        let support = crate::sandbox::limit_support(crate::sandbox::IsolationClaim::Process);
        assert!(
            !support.procs || caps.procs_enforceable,
            "procs was claimed without `pids.max` having been written"
        );
        assert!(
            !support.mem || caps.usable,
            "mem was claimed without `memory.max` having been written"
        );
    }

    #[test]
    fn format_limit_renders_max_or_number() {
        assert_eq!(format_limit(None), "max");
        assert_eq!(format_limit(Some(4 * 1024 * 1024 * 1024)), "4294967296");
    }

    #[test]
    fn parse_cpu_usage_from_cpu_stat() {
        let stat = "usage_usec 1234567\nuser_usec 1000000\nsystem_usec 234567\n";
        assert_eq!(parse_cpu_usage_usec(stat), Some(1234567));
        assert_eq!(parse_cpu_usage_usec("nr_periods 0\n"), None);
    }

    #[test]
    fn parse_count_handles_max_and_numbers() {
        assert_eq!(parse_count("max\n"), None);
        assert_eq!(parse_count("268435456\n"), Some(268435456));
        assert_eq!(parse_count("garbage"), None);
    }

    #[test]
    fn probe_is_honest_about_this_host() {
        // On any host this must not panic and must agree with itself: if it
        // claims usable, it must name a parent; if not, it must explain why.
        let caps = probe();
        if caps.usable {
            #[cfg(target_os = "linux")]
            assert!(caps.parent.is_some(), "usable cgroups must name a parent");
        } else {
            assert!(caps.detail.is_some(), "unusable cgroups must explain why");
        }
    }

    /// Live, capability-gated: where the host actually delegates cgroups (a
    /// systemd user session), exercise the full ScopedCgroup lifecycle. Create
    /// under the discovered base, apply limits, join a process, read accounting,
    /// and clean up on drop. Skips cleanly where delegation is unavailable.
    #[cfg(target_os = "linux")]
    #[test]
    fn live_scoped_cgroup_applies_and_accounts() {
        let caps = probe();
        if !caps.usable {
            eprintln!("skipping: no delegated cgroup on this host ({:?})", caps.detail);
            return;
        }
        let parent = caps.parent.expect("usable ⇒ parent");
        // 256 MiB memory cap, 64 pids. NOTE: we never move the test process into
        // this cgroup. A tight memory.max on the multi-hundred-MB test harness
        // would OOM-kill it. We validate that the limits are *written* and the
        // accounting files are *readable* on an empty run cgroup, which proves
        // the rootless create/limit/account/cleanup path end to end.
        let cg = match ScopedCgroup::create(&parent, 999_999, Some(256 << 20), Some(64)) {
            Ok(c) => c,
            Err(e) => {
                // Delegation probed OK but creation raced/failed. Don't fail
                // the suite on a transient host condition.
                eprintln!("skipping: ScopedCgroup::create failed: {e}");
                return;
            }
        };
        // The limits are actually written into the run cgroup.
        let max = std::fs::read_to_string(cg.path.join("memory.max")).unwrap_or_default();
        assert_eq!(max.trim(), (256u64 << 20).to_string(), "memory.max must be enforced");
        let pids = std::fs::read_to_string(cg.path.join("pids.max")).unwrap_or_default();
        assert_eq!(pids.trim(), "64");
        // Accounting is readable (an empty cgroup reports 0, but the file must
        // exist: that's what the run path reads after a real run).
        let usage = cg.usage();
        assert!(usage.mem_peak_bytes.is_some(), "memory.peak must be readable in the run cgroup");
        assert!(usage.cpu_usec.is_some(), "cpu.stat must be readable in the run cgroup");
        // Drop removes the (empty) leaf; assert it's gone.
        let path = cg.path.clone();
        drop(cg);
        assert!(!path.exists(), "run cgroup must be removed on drop");
    }
}
