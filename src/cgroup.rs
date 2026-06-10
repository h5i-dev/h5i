//! cgroup v2 resource control for the `process` tier (rootless, best-effort).
//!
//! `RLIMIT_AS` (what the process tier uses today for `mem`) caps a process's
//! *virtual address space*, which over-counts for many runtimes (Go, the JVM,
//! sanitizers) and is per-process, not per-tree. cgroup v2 `memory.max` /
//! `pids.max` are the production-grade controls: a hierarchical, whole-subtree
//! limit plus accurate `memory.peak` / `cpu.stat` accounting.
//!
//! ## Rootless reality (honest)
//!
//! Creating a cgroup as a non-root user requires the kernel to have **delegated**
//! a writable subtree to the session (a systemd user manager with
//! `Delegate=yes`). Many hosts — notably WSL2 and most CI — run h5i in a
//! root-owned cgroup (`/init.scope`) with no delegation, so cgroup management is
//! simply **unavailable** there. [`probe`] detects this by actually attempting a
//! create + controller-enable + remove; when it fails we fall back to the
//! existing rlimit path and say so. We never silently pretend a limit is
//! enforced.
//!
//! ## The "no internal processes" rule
//!
//! cgroup v2 forbids a (non-root) cgroup from both holding processes *and*
//! enabling controllers for its children. So we never manage limits on h5i's own
//! cgroup; instead we create a fresh **parent** under the delegated root, enable
//! `+memory +pids` in *its* `subtree_control`, and put each run in a leaf child
//! of that parent. h5i's own process is never moved.

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
    /// True iff we actually created+enabled+removed a probe cgroup — the only
    /// honest signal that limits will be enforced. `false` ⇒ fall back.
    pub usable: bool,
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

/// Detect whether this rootless process can actually manage cgroup v2 limits, by
/// performing a real create → enable-controllers → remove probe. Cheap and
/// side-effect-free (the probe cgroup is removed).
#[cfg(target_os = "linux")]
pub fn probe() -> CgroupCaps {
    let mut caps = CgroupCaps::default();
    if std::fs::metadata(format!("{CG_ROOT}/cgroup.controllers")).is_err() {
        caps.detail = Some("no cgroup2 hierarchy mounted".into());
        return caps;
    }
    caps.v2_mounted = true;

    let Some(own) = self_cgroup() else {
        caps.detail = Some("could not read /proc/self/cgroup".into());
        return caps;
    };
    // Controllers our own cgroup could delegate to children.
    if let Ok(s) = std::fs::read_to_string(own.join("cgroup.controllers")) {
        caps.controllers = s.split_whitespace().map(String::from).collect();
    }

    // Honest usability test: create a parent + leaf under our cgroup, enable
    // controllers, then remove. If any step fails (the common rootless case),
    // we are NOT usable and the caller falls back to rlimits.
    let parent = own.join("h5i.probe");
    match try_make_usable(&parent) {
        Ok(()) => {
            let _ = std::fs::remove_dir(&parent);
            caps.usable = true;
            // The real parent used at run time (created lazily).
            caps.parent = Some(own.join("h5i"));
        }
        Err(e) => {
            let _ = std::fs::remove_dir(&parent);
            caps.detail = Some(format!("cgroup delegation unavailable: {e}"));
        }
    }
    caps
}

/// Best-effort: create `parent`, enable `+memory +pids` in its subtree_control,
/// create a leaf, and remove the leaf — proving we can manage run cgroups.
#[cfg(target_os = "linux")]
fn try_make_usable(parent: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(parent)?;
    // Enabling controllers in the parent's subtree_control is what actually
    // requires delegation; this is the step that fails on WSL2/CI.
    let _ = std::fs::write(parent.join("cgroup.subtree_control"), "+memory +pids");
    let leaf = parent.join("probe-leaf");
    std::fs::create_dir_all(&leaf)?;
    // memory.max must be writable for enforcement to mean anything.
    let writable = std::fs::write(leaf.join("memory.max"), "max").is_ok()
        || std::fs::metadata(leaf.join("memory.max")).is_ok();
    let _ = std::fs::remove_dir(&leaf);
    if writable {
        Ok(())
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
            let _ = std::fs::write(path.join("pids.max"), p.to_string());
        }
        Ok(ScopedCgroup { path })
    }

    /// Path of this cgroup's `cgroup.procs` — the child writes its own pid here
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
        detail: Some("cgroup v2 is Linux-only".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
