//! The effective configuration — what the kernel tiers actually apply
//! (ROADMAP.md §V2).
//!
//! `policy.resolved.toml` is the digested *intent*. `ResolvedPolicy` also
//! carries runtime-only, never-serialized fields (the bind lists, readonly
//! mode, egress extras) that are enforced all the same, so a model that reads
//! only the toml verifies less than what a box gets. This module serializes
//! the enforced state: [`compute_effective`] produces it, and
//! [`crate::sandbox::build_confined_command`] consumes the *same* result to
//! build its Landlock path sets and bind lists — one computation, two readers,
//! so the dump and the sandbox cannot drift apart. That is the apply-seam rule:
//! the dump is never a parallel pretty-printer.
//!
//! Paths are serialized as UTF-8 strings (lossy). Grants and bind sources come
//! from TOML and h5i-created directories, which are UTF-8 in practice; a
//! non-UTF-8 host path would mangle here, and the enforcement that now reads
//! these strings would then miss the grant or fail the bind — both the
//! fail-closed direction.
//!
//! Linux kernel tiers (`process`, `supervised`) only, per §V2's v1 scope. The
//! Seatbelt, container and microvm backends enforce through mechanisms this
//! schema does not describe; they are excluded rather than half-described.
//!
//! Every runtime-only (`serde(skip)`) field of [`ResolvedPolicy`] is either in
//! this dump or excluded here by name, with the reason (§V2's exit criterion):
//!
//! - `private_binds`, `home_binds`, `ro_binds`, `cache_write`,
//!   `work_readonly`, `user_egress_allow`, `loopback_ports`: **in the dump**
//!   (`binds`, `work_readonly`, `net`).
//! - `box_git`: container-backend mounts. On the kernel tiers
//!   `grant_box_git` pushes the same paths into `fs_read`/`fs_write`, so what
//!   they enforce here already appears under `landlock`.
//! - `env_capture_spool`, `env_inbox`: container-mount plumbing; the kernel
//!   tiers reach them through fs grants and the env allowlist, both recorded.
//! - `hosts_services`: a microvm-only idle-stop hint, no kernel-tier effect.
//! - `egress_proxy_port`: container-tier proxy wiring.
//! - `effective_out`: where this very dump is written — self-reference, not
//!   enforcement.

use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::H5iError;
use crate::sandbox_policy::{IsolationClaim, NetMode, Profile, ResolvedPolicy};

/// File name of the dump, beside `policy.resolved.toml` in the env dir.
pub const EFFECTIVE_CONFIG_FILE: &str = "policy.effective.json";

/// Version of this schema. Bump on any field change — the digest is pinned in
/// capture records, and a reader must know what it is hashing.
pub const EFFECTIVE_SCHEMA: u32 = 1;

/// Name of the fixed seccomp deny-list template the kernel tiers install
/// (`crate::sandbox::seccomp_deny_program`). The filter is a fixed artifact;
/// the dump names it rather than modeling BPF. Bump the suffix when the
/// deny-list changes.
pub const SECCOMP_DENY_TEMPLATE: &str = "deny-v1";

/// The per-invocation flags that change what gets applied. Two runs of the
/// same policy differ in exactly these, so they are part of the dump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunShape {
    /// Always create a fresh network namespace (supervised does; process only
    /// when `net.mode = deny`).
    pub force_netns: bool,
    /// The seccomp user-notification gate is stacked on the deny-list
    /// (supervised tier).
    pub notify: bool,
    /// The netns egress allowlist (nftables + slirp4netns uplink) is
    /// installed.
    pub egress: bool,
    /// Fresh PID namespace with a private procfs.
    pub pidns: bool,
    /// Interactive agent session: config lockdown binds, no setsid, no wall
    /// clock.
    pub interactive: bool,
}

/// What a bind is for. Serialized in the order the child applies them
/// (`build_confined_command` steps 1d–1h).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BindKind {
    /// Agent config pinned read-only over itself (interactive sessions).
    ConfigLock,
    /// Per-env private path bound over its workspace-relative target.
    Private,
    /// Per-env HOME-state copy bound over the real `~/.claude`/`~/.codex`.
    HomeState,
    /// Warm dependency cache, bind then remount read-only.
    CacheRo,
    /// The one writable cache bind (`box cache refresh` only).
    CacheRw,
}

/// One bind mount, exactly as applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveBind {
    pub kind: BindKind,
    pub source: String,
    pub target: String,
    pub writable: bool,
}

/// The Landlock allowlist as granted: post `$WORK`/tilde expansion, post
/// exists-filter. A grant whose path is absent on this host is *skipped*,
/// which narrows the sandbox (fail-closed); it is recorded rather than
/// silently dropped so the dump says what was asked for and not given.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandlockEffective {
    /// Probed kernel ABI the ruleset was built against.
    pub abi: i32,
    /// Read-only grants (allowlist), as handed to `path_beneath_rules`.
    pub ro: Vec<String>,
    /// Read-write grants, `$WORK` included unless the session is readonly.
    pub rw: Vec<String>,
    /// Declared grants absent on this host, so not granted.
    pub skipped_missing: Vec<String>,
}

/// Which namespaces the child unshares, and how the user namespace maps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamespacesEffective {
    pub user: bool,
    pub ipc: bool,
    pub uts: bool,
    pub net: bool,
    pub mount: bool,
    pub pid: bool,
    /// `identity` (uid→uid) or `root` (0→uid, the egress path: `nft` needs
    /// root-in-userns).
    pub userns_map: String,
}

/// The network posture as enforced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetEffective {
    pub mode: NetMode,
    /// Profile egress allowlist (digested).
    pub egress: Vec<String>,
    /// Host-side user allowlist extras (`h5i box allow`), runtime-only.
    pub user_egress_allow: Vec<String>,
    /// Profile-declared loopback ports (digested).
    pub loopback: Vec<u16>,
    /// h5i-allocated loopback ports for this box, runtime-only.
    pub loopback_runtime: Vec<u16>,
    pub unix_sockets: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeccompEffective {
    pub template: String,
    pub notify: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RlimitsEffective {
    pub mem_bytes: Option<u64>,
    pub max_procs: Option<u64>,
    pub fsize_bytes: Option<u64>,
    pub cpu_secs: Option<u64>,
    pub wall_secs: u64,
}

/// Resolution metadata: facts about the *policy*, not kernel rules. `fs_deny`
/// lives here and nowhere else — Landlock is allowlist-only, and `fs_deny` is
/// a preflight refusal condition on resolution, so a theorem about it can only
/// ever be "resolution refuses", never "the kernel denies" (§V2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionMeta {
    pub profile: String,
    pub fs_deny: Vec<String>,
}

/// The effective configuration of one kernel-tier invocation. Field order is
/// the canonical serialization order — digest stability depends on it, exactly
/// as with [`Profile`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectiveConfig {
    pub schema: u32,
    pub platform: String,
    pub claim: String,
    pub work: String,
    pub work_readonly: bool,
    pub run: RunShape,
    pub landlock: LandlockEffective,
    pub binds: Vec<EffectiveBind>,
    pub namespaces: NamespacesEffective,
    pub net: NetEffective,
    pub seccomp: SeccompEffective,
    pub rlimits: RlimitsEffective,
    pub env_pass: Vec<String>,
    pub tools: Vec<String>,
    pub resolution: ResolutionMeta,
}

impl EffectiveConfig {
    /// Canonical JSON — the exact bytes [`Self::write_to`] persists and
    /// [`Self::digest`] hashes.
    pub fn to_json(&self) -> Result<String, H5iError> {
        serde_json::to_string_pretty(self)
            .map(|mut s| {
                s.push('\n');
                s
            })
            .map_err(|e| H5iError::Metadata(format!("effective config serialization failed: {e}")))
    }

    /// sha256 over the canonical JSON.
    pub fn digest(&self) -> Result<String, H5iError> {
        let json = self.to_json()?;
        let mut hasher = Sha256::new();
        hasher.update(json.as_bytes());
        Ok(format!("{:x}", hasher.finalize()))
    }

    /// Write the canonical JSON to `path` and return its digest.
    pub fn write_to(&self, path: &Path) -> Result<String, H5iError> {
        let json = self.to_json()?;
        std::fs::write(path, &json).map_err(|e| H5iError::with_path(e, path))?;
        let mut hasher = Sha256::new();
        hasher.update(json.as_bytes());
        Ok(format!("{:x}", hasher.finalize()))
    }
}

fn lossy(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Compute the effective configuration for one invocation. This is the single
/// source `build_confined_command` builds its Landlock path sets and bind
/// lists from — change enforcement here, never beside it.
///
/// `work` must already be canonicalized (the caller does, before Landlock).
/// `landlock_abi` is the probed ABI the ruleset will be built against.
pub fn compute_effective(
    policy: &ResolvedPolicy,
    work: &Path,
    landlock_abi: i32,
    shape: &RunShape,
) -> EffectiveConfig {
    let p = &policy.profile;
    let ro_work = policy.work_readonly;
    let mut skipped: Vec<String> = Vec::new();

    // Mirrors build_confined_command exactly: rw = $WORK (unless readonly) +
    // fs.write; ro = fs.read (+ $WORK when readonly); grants whose paths do
    // not exist on this host are skipped — the fail-closed direction.
    let mut rw: Vec<String> = Vec::new();
    if !ro_work {
        rw.push(lossy(work));
    }
    for s in p.fs_write.iter().filter(|s| s.as_str() != "$WORK") {
        let expanded = crate::sandbox::expand_tilde(s);
        if Path::new(&expanded).exists() {
            rw.push(expanded);
        } else {
            skipped.push(expanded);
        }
    }
    let mut ro: Vec<String> = Vec::new();
    for s in &p.fs_read {
        let expanded = crate::sandbox::expand_tilde(s);
        if Path::new(&expanded).exists() {
            ro.push(expanded);
        } else {
            skipped.push(expanded);
        }
    }
    if ro_work {
        ro.push(lossy(work));
    }

    // Binds, in the order the child applies them (steps 1d–1h).
    let mut binds: Vec<EffectiveBind> = Vec::new();
    if shape.interactive {
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        for path in crate::sandbox::config_lock_paths(work, home.as_deref()) {
            let s = lossy(&path);
            binds.push(EffectiveBind {
                kind: BindKind::ConfigLock,
                source: s.clone(),
                target: s,
                writable: false,
            });
        }
    }
    for b in &policy.private_binds {
        binds.push(EffectiveBind {
            kind: BindKind::Private,
            source: lossy(&b.backing),
            target: lossy(&work.join(&b.rel)),
            writable: true,
        });
    }
    for b in crate::sandbox::home_binds_in_mount_order(&policy.home_binds) {
        binds.push(EffectiveBind {
            kind: BindKind::HomeState,
            source: lossy(&b.backing),
            target: lossy(&b.target),
            writable: true,
        });
    }
    for b in &policy.ro_binds {
        binds.push(EffectiveBind {
            kind: BindKind::CacheRo,
            source: lossy(&b.backing),
            target: lossy(&b.target),
            writable: false,
        });
    }
    if let Some(b) = &policy.cache_write {
        binds.push(EffectiveBind {
            kind: BindKind::CacheRw,
            source: lossy(&b.backing),
            target: lossy(&b.target),
            writable: true,
        });
    }

    let want_netns = p.net_mode == NetMode::Deny || shape.force_netns;
    let namespaces = NamespacesEffective {
        user: true,
        ipc: true,
        uts: true,
        net: want_netns,
        mount: shape.pidns || shape.egress || !binds.is_empty(),
        pid: shape.pidns,
        userns_map: if shape.egress { "root".into() } else { "identity".into() },
    };

    EffectiveConfig {
        schema: EFFECTIVE_SCHEMA,
        platform: "linux".into(),
        claim: policy.claim.as_str().to_string(),
        work: lossy(work),
        work_readonly: ro_work,
        run: *shape,
        landlock: LandlockEffective { abi: landlock_abi, ro, rw, skipped_missing: skipped },
        binds,
        namespaces,
        net: NetEffective {
            mode: p.net_mode,
            egress: p.net_egress.clone(),
            user_egress_allow: policy.user_egress_allow.clone(),
            loopback: p.loopback_ports.clone(),
            loopback_runtime: policy.loopback_ports.clone(),
            unix_sockets: p.unix_sockets,
        },
        seccomp: SeccompEffective {
            template: SECCOMP_DENY_TEMPLATE.into(),
            notify: shape.notify,
        },
        rlimits: RlimitsEffective {
            mem_bytes: p.mem_bytes,
            max_procs: p.max_procs,
            fsize_bytes: p.fsize_bytes,
            cpu_secs: p.cpu_secs,
            wall_secs: p.wall_secs,
        },
        env_pass: p.env_pass.clone(),
        tools: p.tools.clone(),
        resolution: ResolutionMeta { profile: p.name.clone(), fs_deny: p.fs_deny.clone() },
    }
}

/// The canonical captured-run shape per kernel tier — what `env create` dumps
/// before any run exists. `None` for the tiers this schema does not describe
/// (workspace runs unconfined; Seatbelt, container and microvm enforce through
/// other mechanisms).
pub fn captured_run_shape(claim: IsolationClaim, profile: &Profile) -> Option<RunShape> {
    match claim {
        // run_confined: netns only when net.mode = deny, no notify gate,
        // pidns + private procfs, non-interactive.
        IsolationClaim::Process => Some(RunShape {
            force_netns: false,
            notify: false,
            egress: false,
            pidns: true,
            interactive: false,
        }),
        // supervisor::run: always-netns, the notify gate, the egress jail when
        // the profile declares an allowlist, pidns, non-interactive.
        IsolationClaim::Supervised => Some(RunShape {
            force_netns: true,
            notify: true,
            egress: !profile.net_egress.is_empty(),
            pidns: true,
            interactive: false,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox_policy::{IsolationClaim, ResolvedPolicy};

    fn policy(work_readonly: bool) -> ResolvedPolicy {
        let profile = Profile::builtin("default", IsolationClaim::Process);
        let mut p = ResolvedPolicy::new(IsolationClaim::Process, profile);
        p.work_readonly = work_readonly;
        p
    }

    fn shape() -> RunShape {
        RunShape { force_netns: false, notify: false, egress: false, pidns: true, interactive: false }
    }

    #[test]
    fn digest_is_stable_across_recomputation() {
        let tmp = tempfile::tempdir().unwrap();
        let p = policy(false);
        let a = compute_effective(&p, tmp.path(), 3, &shape());
        let b = compute_effective(&p, tmp.path(), 3, &shape());
        assert_eq!(a, b);
        assert_eq!(a.digest().unwrap(), b.digest().unwrap());
    }

    #[test]
    fn readonly_session_moves_work_from_rw_to_ro() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().to_string_lossy().into_owned();
        let rw_cfg = compute_effective(&policy(false), tmp.path(), 3, &shape());
        assert!(rw_cfg.landlock.rw.contains(&work));
        assert!(!rw_cfg.landlock.ro.contains(&work));
        let ro_cfg = compute_effective(&policy(true), tmp.path(), 3, &shape());
        assert!(!ro_cfg.landlock.rw.contains(&work));
        assert!(ro_cfg.landlock.ro.contains(&work));
        // The two shapes must not collide on digest.
        assert_ne!(rw_cfg.digest().unwrap(), ro_cfg.digest().unwrap());
    }

    #[test]
    fn missing_grant_is_skipped_and_recorded() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = policy(false);
        let ghost = tmp.path().join("does-not-exist");
        p.profile.fs_read.push(ghost.to_string_lossy().into_owned());
        let cfg = compute_effective(&p, tmp.path(), 3, &shape());
        let ghost = ghost.to_string_lossy().into_owned();
        assert!(!cfg.landlock.ro.contains(&ghost));
        assert!(cfg.landlock.skipped_missing.contains(&ghost));
    }

    #[test]
    fn json_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = compute_effective(&policy(false), tmp.path(), 3, &shape());
        let json = cfg.to_json().unwrap();
        let back: EffectiveConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn write_to_digest_matches_file_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = compute_effective(&policy(false), tmp.path(), 3, &shape());
        let out = tmp.path().join(EFFECTIVE_CONFIG_FILE);
        let digest = cfg.write_to(&out).unwrap();
        let bytes = std::fs::read(&out).unwrap();
        let mut h = Sha256::new();
        h.update(&bytes);
        assert_eq!(digest, format!("{:x}", h.finalize()));
        assert_eq!(digest, cfg.digest().unwrap());
    }

    #[test]
    fn captured_run_shape_covers_kernel_tiers_only() {
        let profile = Profile::builtin("default", IsolationClaim::Process);
        assert!(captured_run_shape(IsolationClaim::Process, &profile).is_some());
        let sup = captured_run_shape(IsolationClaim::Supervised, &profile).unwrap();
        assert!(sup.force_netns && sup.notify);
        assert!(captured_run_shape(IsolationClaim::Workspace, &profile).is_none());
        assert!(captured_run_shape(IsolationClaim::Container, &profile).is_none());
    }
}
