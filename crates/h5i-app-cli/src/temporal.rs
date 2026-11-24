//! Optional Veil temporal verification (`h5i app temporal-check`, `prove --temporal`,
//! `model-check`) and real Rust counterexample replay (`h5i app trace-replay`).
//!
//! Design split:
//! - **Veil** (`verse-lab/veil`) owns transition-system model checking
//!   (`#model_check`), temporal/liveness/fairness checking (`#temporal_check`
//!   via Lasso & Lentil), and SMT invariant checking (`#check_invariants`).
//! - **h5i** owns the optional CLI integration (Veil is never required unless
//!   explicitly enabled) and **Counterexample Replay** directly against the
//!   actual Rust implementations (`h5i_app_embedded::store::FlashStore`,
//!   `h5i_app_embedded::kernel::DeviceEngine`, `h5i_app_core::MemoryEngine`).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};
use h5i_app_embedded::replay::{
    DeviceEngineReplayAction, FlashReplayAction, MotorCmd, StoreFault,
    replay_real_device_engine, replay_real_flash_store,
};
use h5i_app_report::{TemporalReport, TemporalStatus};
use serde::{Deserialize, Serialize};

use crate::manifest::Project;
use crate::util::{self, Out};

/// CLI arguments for `h5i app model-check` (delegates to Veil / Lake).
#[derive(Args, Debug, Clone)]
pub struct ModelCheckArgs {
    /// Directory of the Veil Lake package (defaults to the project's
    /// `temporal_dir`, or workspace `crates/h5i-app-veil/proofs`).
    pub dir: Option<PathBuf>,
    /// Optional Lake build target(s), e.g. `H5iAppVeil.Embedded.FlashStore`.
    #[arg(long = "target", value_name = "MODULE")]
    pub targets: Vec<String>,
}

/// Target Rust runtime implementation for `h5i app trace-replay`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReplayTarget {
    FlashStore,
    DeviceEngine,
}

/// CLI arguments for `h5i app trace-replay`.
#[derive(Args, Debug, Clone)]
pub struct TraceReplayArgs {
    /// Path to a Veil JSON counterexample trace (`Trace.toJsonWithPicks`) or
    /// action trace JSON file.
    pub trace: PathBuf,
    /// Target Rust runtime (`flash-store` or `device-engine`). Inferred from
    /// the trace's transition names or `"target"` field if omitted.
    #[arg(long, value_enum)]
    pub target: Option<ReplayTarget>,
    /// For `flash-store`: seed region 0 at `seq = u32::MAX` (4,294,967,295)
    /// to reproduce 32-bit wraparound on the real `FlashStore` directly.
    #[arg(long)]
    pub seed_wraparound: bool,
    /// For `device-engine`: replay against the latched motor kernel.
    #[arg(long)]
    pub latched: bool,
    /// Print replay report as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Availability of the optional Veil backend on this machine / project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VeilAvailability {
    Available { detail: String },
    Missing { reason: String },
}

/// Detect whether the optional Veil toolchain is available for `project` (if any).
pub fn detect_veil(project: Option<&Project>) -> VeilAvailability {
    if util::on_path("lake").is_none() {
        return VeilAvailability::Missing {
            reason: "`lake` is not on PATH; Veil requires Lean 4 / Lake (install elan and Veil, or keep `[verification] temporal = false`)".into(),
        };
    }
    if let Ok(path) = std::env::var("H5I_VEIL_PATH")
        && !path.is_empty()
    {
        let p = PathBuf::from(&path);
        if p.join("lakefile.lean").is_file() || p.join("lakefile.toml").is_file() {
            return VeilAvailability::Available {
                detail: format!("local Veil at {}", p.display()),
            };
        }
        return VeilAvailability::Missing {
            reason: format!("H5I_VEIL_PATH={} does not contain a Lake package", p.display()),
        };
    }
    if let Some(proj) = project {
        let tdir = proj.temporal_dir();
        if tdir.join(".lake/packages/veil").is_dir() {
            return VeilAvailability::Available {
                detail: format!("fetched Veil in {}", tdir.display()),
            };
        }
        if proj.proofs().join(".lake/packages/veil").is_dir() {
            return VeilAvailability::Available {
                detail: format!("fetched Veil in {}", proj.proofs().display()),
            };
        }
        if let Some(top) = util::git_toplevel(&proj.root) {
            let ws_veil = top.join("crates/h5i-app-veil/proofs/.lake/packages/veil");
            if ws_veil.is_dir() {
                return VeilAvailability::Available {
                    detail: format!("workspace Veil in {}", ws_veil.display()),
                };
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir()
        && let Some(top) = util::git_toplevel(&cwd)
    {
        let ws_veil = top.join("crates/h5i-app-veil/proofs/.lake/packages/veil");
        if ws_veil.is_dir() {
            return VeilAvailability::Available {
                detail: format!("workspace Veil in {}", ws_veil.display()),
            };
        }
    }
    VeilAvailability::Missing {
        reason: "Veil package is not installed (set `H5I_VEIL_PATH=/path/to/veil` or run `lake build` inside `crates/h5i-app-veil/proofs` / `proofs/temporal`)".into(),
    }
}

/// Run optional Veil temporal verification for `p`.
///
/// Policy:
/// - Temporal verification disabled (`!force_enable && !p.manifest.verification.temporal`):
///   cleanly returns `Ok(Skipped)`.
/// - Temporal verification enabled, Veil missing: records `Error` and returns
///   `Err(...)` (never silently skipped).
/// - Veil present: invokes `lake build` in `temporal_dir` (running Veil's
///   `#check_invariants`, `#model_check`, `#temporal_check`, `prove_temporal_by`).
pub fn temporal_check(
    p: &Project,
    force_enable: bool,
    backend_override: Option<&str>,
    out: &Out,
) -> Result<TemporalReport> {
    let backend = backend_override
        .unwrap_or(p.manifest.verification.temporal_backend.as_str())
        .to_string();
    let enabled = force_enable || p.manifest.verification.temporal;

    if !enabled {
        let rep = TemporalReport {
            enabled: false,
            backend,
            status: TemporalStatus::Skipped,
            detail: Some("not enabled (set `[verification] temporal = true` or pass `--temporal`)".into()),
        };
        out.note(&format!("{}: temporal verification skipped (not enabled)", p.display()));
        return Ok(rep);
    }

    if backend != "veil" {
        bail!(
            "{}: unknown temporal backend {:?} (expected \"veil\")",
            p.display(),
            backend
        );
    }

    let mut record = crate::report::start(p, "temporal", vec![backend.clone()])?;
    let outcome = run_veil_backend(p, out, &mut record);

    match outcome {
        Ok(rep) => {
            let run_status = match rep.status {
                TemporalStatus::Passed | TemporalStatus::Skipped => "passed",
                _ => "failed",
            };
            record.run.temporal = Some(rep.clone());
            record.finish(run_status)?;
            match rep.status {
                TemporalStatus::Passed | TemporalStatus::Skipped => Ok(rep),
                TemporalStatus::Counterexample => bail!(
                    "{}: Veil temporal verification found counterexample ({})",
                    p.display(),
                    rep.detail.as_deref().unwrap_or("see output")
                ),
                TemporalStatus::Unknown => bail!(
                    "{}: Veil temporal verification inconclusive ({})",
                    p.display(),
                    rep.detail.as_deref().unwrap_or("see output")
                ),
                TemporalStatus::Failed | TemporalStatus::Error | TemporalStatus::NotRun => bail!(
                    "{}: Veil temporal verification failed ({})",
                    p.display(),
                    rep.detail.as_deref().unwrap_or("see output")
                ),
            }
        }
        Err(e) => {
            record.run.temporal = Some(TemporalReport {
                enabled: true,
                backend,
                status: TemporalStatus::Error,
                detail: Some(format!("{e:#}")),
            });
            record.run.error = Some(format!("{e:#}"));
            record.finish("failed")?;
            Err(e)
        }
    }
}

fn run_veil_backend(
    p: &Project,
    out: &Out,
    record: &mut h5i_app_report::Recorder,
) -> Result<TemporalReport> {
    if let VeilAvailability::Missing { reason } = detect_veil(Some(p)) {
        bail!(
            "{}: temporal verification is enabled (backend = \"veil\"), but Veil is unavailable: {reason}",
            p.display()
        );
    }
    let tdir = p.temporal_dir();
    if !tdir.join("lakefile.lean").is_file() && !tdir.join("lakefile.toml").is_file() {
        bail!(
            "{}: temporal verification is enabled (backend = \"veil\"), but {} is not a Lake project \
             (create {}/lakefile.lean requiring `veil`, or set `[verification] temporal_dir`)",
            p.display(),
            tdir.display(),
            p.manifest.verification.temporal_dir
        );
    }
    let mut build = Command::new("lake");
    build.arg("build").current_dir(&tdir);
    match crate::report::step(record, "veil_build", out, |log| util::run(&mut build, log)) {
        Ok(()) => Ok(TemporalReport {
            enabled: true,
            backend: "veil".into(),
            status: TemporalStatus::Passed,
            detail: Some(format!("lake build (Veil) passed in {}", p.manifest.verification.temporal_dir)),
        }),
        Err(e) => {
            let msg = format!("{e:#}");
            let status = if msg.contains("Violation:") || msg.contains("safety_failure") || msg.contains("liveness_failure") {
                TemporalStatus::Counterexample
            } else {
                TemporalStatus::Failed
            };
            Ok(TemporalReport {
                enabled: true,
                backend: "veil".into(),
                status,
                detail: Some(msg),
            })
        }
    }
}

/// Run `h5i app model-check`: delegates directly to Veil via `lake build`.
pub fn model_check(args: &ModelCheckArgs, out: &Out) -> Result<()> {
    let dir = match &args.dir {
        Some(d) => d.clone(),
        None => {
            let cwd = std::env::current_dir()?;
            if let Some(m) = crate::manifest::locate(&cwd) {
                let p = Project::load(&m)?;
                let td = p.temporal_dir();
                if td.join("lakefile.lean").is_file() || td.join("lakefile.toml").is_file() {
                    td
                } else if let Some(top) = util::git_toplevel(&cwd) {
                    top.join("crates/h5i-app-veil/proofs")
                } else {
                    td
                }
            } else if let Some(top) = util::git_toplevel(&cwd) {
                top.join("crates/h5i-app-veil/proofs")
            } else {
                bail!("no Veil Lake package found; pass a directory containing a Veil lakefile.lean");
            }
        }
    };
    if !dir.join("lakefile.lean").is_file() && !dir.join("lakefile.toml").is_file() {
        bail!("{} is not a Veil Lake project", dir.display());
    }
    if util::on_path("lake").is_none() {
        bail!(
            "`lake` is not on PATH; `h5i app model-check` delegates to Veil (optional Lean 4 backend)"
        );
    }
    if !dir.join(".lake/packages/veil").is_dir() && std::env::var("H5I_VEIL_PATH").is_err() {
        bail!(
            "Veil is not installed in {} (run `lake update` there or set `H5I_VEIL_PATH`)",
            dir.display()
        );
    }
    let mut cmd = Command::new("lake");
    cmd.arg("build").args(&args.targets).current_dir(&dir);
    util::run(&mut cmd, out)
}

/* -------------------------------------------------------------------------- */
/* Real Rust Counterexample Trace Replay (parses Veil's Trace.toJsonWithPicks)*/
/* -------------------------------------------------------------------------- */

/// Veil's `Trace.toJsonWithPicks` state entry (`Veil/Core/Tools/ModelChecker/Trace.lean:101`).
#[derive(Clone, Debug, Deserialize)]
struct VeilTraceState {
    #[allow(dead_code)]
    index: usize,
    transition: serde_json::Value,
    #[serde(default)]
    #[allow(dead_code)]
    fields: serde_json::Value,
}

/// Veil's `Trace.toJsonWithPicks` top-level JSON (`{"theory": ..., "states": [...]}`),
/// with optional h5i metadata (`target`, `seed_wraparound`, `latched`).
#[derive(Clone, Debug, Deserialize)]
struct VeilTraceJson {
    #[serde(default)]
    target: Option<ReplayTarget>,
    #[serde(default)]
    seed_wraparound: Option<bool>,
    #[serde(default)]
    latched: Option<bool>,
    states: Vec<VeilTraceState>,
}

/// Output of `h5i app trace-replay`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplaySummary {
    pub target: ReplayTarget,
    pub steps_replayed: usize,
    pub reproduced_violation: bool,
    pub detail: String,
}

/// Run `h5i app trace-replay` against the actual Rust `FlashStore` or `DeviceEngine`.
pub fn trace_replay(args: &TraceReplayArgs) -> Result<()> {
    let summary = replay_veil_trace_file(
        &args.trace,
        args.target,
        args.seed_wraparound,
        args.latched,
    )?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        println!("target:       {:?}", summary.target);
        println!("steps:        {}", summary.steps_replayed);
        println!("reproduced:   {}", summary.reproduced_violation);
        println!("detail:       {}", summary.detail);
    }
    Ok(())
}

/// Parse a Veil JSON trace (`Trace.toJsonWithPicks`) and replay it directly
/// against the real `h5i_app_embedded::store::FlashStore` or
/// `h5i_app_embedded::kernel::DeviceEngine`.
pub fn replay_veil_trace_file(
    path: &Path,
    target_override: Option<ReplayTarget>,
    seed_wraparound: bool,
    latched_override: bool,
) -> Result<ReplaySummary> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading Veil trace {}", path.display()))?;
    let trace: VeilTraceJson = serde_json::from_str(&text)
        .with_context(|| format!("parsing Veil JSON trace {}", path.display()))?;

    let transitions: Vec<String> = trace
        .states
        .iter()
        .skip(1) // index 0 is `after_init`
        .map(|s| match &s.transition {
            serde_json::Value::String(name) => name.clone(),
            other => other.to_string(),
        })
        .collect();

    let target = target_override
        .or(trace.target)
        .or_else(|| infer_target(&transitions))
        .context("could not infer replay target from Veil trace; pass `--target flash-store` or `--target device-engine`")?;

    match target {
        ReplayTarget::FlashStore => {
            let wrap = seed_wraparound || trace.seed_wraparound.unwrap_or(false);
            let mut actions = Vec::new();
            let mut pending_val: Option<u64> = None;
            let mut next_val = 1u64;
            for tr in &transitions {
                let name = tr.trim();
                if name.starts_with("beginSave") {
                    // Extract bool/int argument if present (`beginSave true` / `beginSave false` / `beginSave 2`).
                    let v = if name.contains("true") {
                        2
                    } else if name.contains("false") {
                        1
                    } else {
                        next_val += 1;
                        next_val
                    };
                    pending_val = Some(v);
                } else if name.starts_with("writeComplete") {
                    let v = pending_val.take().unwrap_or_else(|| {
                        next_val += 1;
                        next_val
                    });
                    actions.push(FlashReplayAction::Save(v));
                } else if name.starts_with("powerCut") {
                    if let Some(v) = pending_val.take() {
                        actions.push(FlashReplayAction::SaveWithCut { val: v, budget: 10 });
                    }
                } else if name.starts_with("rebootScan") {
                    actions.push(FlashReplayAction::RebootLoad);
                }
            }
            // Append a final reboot load when checking durability on an idle trace.
            if !matches!(actions.last(), Some(FlashReplayAction::RebootLoad)) {
                actions.push(FlashReplayAction::RebootLoad);
            }
            let seed = if wrap { Some((u32::MAX, 1u64)) } else { None };
            let out = replay_real_flash_store(seed, &actions);
            let reproduced = out.durability_violated || out.crash_consistency_violated;
            Ok(ReplaySummary {
                target,
                steps_replayed: out.steps_replayed,
                reproduced_violation: reproduced,
                detail: format!(
                    "real h5i_app_embedded::store::FlashStore: last_acked={:?}, last_loaded={:?}, durability_violated={}, crash_consistency_violated={}, erases={}",
                    out.last_acked,
                    out.last_loaded,
                    out.durability_violated,
                    out.crash_consistency_violated,
                    out.erases
                ),
            })
        }
        ReplayTarget::DeviceEngine => {
            let latched = latched_override || trace.latched.unwrap_or(false);
            let mut actions = Vec::new();
            let mut pending_cmd: Option<MotorCmd> = None;
            for tr in &transitions {
                let name = tr.trim();
                if name.starts_with("startEmergency") {
                    pending_cmd = Some(MotorCmd::RequestEmergency);
                } else if name.starts_with("startClearEmergency") {
                    pending_cmd = Some(MotorCmd::ClearEmergency);
                } else if name.starts_with("publish") {
                    if let Some(cmd) = pending_cmd.take() {
                        actions.push(DeviceEngineReplayAction::Execute(cmd));
                    }
                } else if name.starts_with("saveFailClean") {
                    if let Some(cmd) = pending_cmd.take() {
                        actions.push(DeviceEngineReplayAction::ExecuteWithStoreFault(
                            cmd,
                            StoreFault::FailClean,
                        ));
                    }
                } else if name.starts_with("saveFailAmbiguous") {
                    if let Some(cmd) = pending_cmd.take() {
                        actions.push(DeviceEngineReplayAction::ExecuteWithStoreFault(
                            cmd,
                            StoreFault::FailAmbiguous,
                        ));
                    }
                } else if name.starts_with("actuatorPoll") {
                    actions.push(DeviceEngineReplayAction::ActuatorPoll);
                } else if name.starts_with("crashAndRestore") {
                    actions.push(DeviceEngineReplayAction::CrashAndRestore);
                }
            }
            let out = if latched {
                replay_real_device_engine::<true>(&actions)
            } else {
                replay_real_device_engine::<false>(&actions)
            };
            let reproduced = out.emergency_committed && !out.actuator_observed_stop;
            Ok(ReplaySummary {
                target,
                steps_replayed: out.steps_replayed,
                reproduced_violation: reproduced,
                detail: format!(
                    "real h5i_app_embedded::kernel::DeviceEngine (latched={latched}): emergency_committed={}, actuator_observed_stop={}, clear_refused={}, final_version={}",
                    out.emergency_committed,
                    out.actuator_observed_stop,
                    out.clear_refused_count,
                    out.final_version
                ),
            })
        }
    }
}

fn infer_target(transitions: &[String]) -> Option<ReplayTarget> {
    for tr in transitions {
        if tr.contains("beginSave") || tr.contains("writeComplete") || tr.contains("rebootScan") {
            return Some(ReplayTarget::FlashStore);
        }
        if tr.contains("startEmergency") || tr.contains("actuatorPoll") || tr.contains("saveFailAmbiguous") {
            return Some(ReplayTarget::DeviceEngine);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::FILE;

    #[test]
    fn disabled_temporal_verification_is_cleanly_skipped_without_veil() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(FILE), "[check]\n").unwrap();
        let p = Project::load(&d.path().join(FILE)).unwrap();
        let rep = temporal_check(&p, false, None, &Out::Log(d.path().join("log"))).unwrap();
        assert!(!rep.enabled);
        assert_eq!(rep.status, TemporalStatus::Skipped);
    }

    #[test]
    fn enabled_veil_without_installation_returns_error_not_skip() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join(FILE),
            "[verification]\ntemporal = true\ntemporal_backend = \"veil\"\n",
        )
        .unwrap();
        let p = Project::load(&d.path().join(FILE)).unwrap();
        let err = temporal_check(&p, false, None, &Out::Log(d.path().join("log"))).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("Veil is unavailable") || msg.contains("is not a Lake project"),
            "expected explicit error when Veil/temporal project missing, got: {msg}"
        );
    }

    #[test]
    fn replays_veil_flash_store_wraparound_json_trace_against_real_flash_store() {
        let d = tempfile::tempdir().unwrap();
        let trace_path = d.path().join("veil-flash-trace.json");
        // Veil `Trace.toJsonWithPicks` shape from `FlashStorePlain`.
        let veil_json = serde_json::json!({
            "theory": {},
            "seed_wraparound": true,
            "states": [
                { "index": 0, "transition": "after_init", "fields": {} },
                { "index": 1, "transition": "beginSave true", "fields": {} },
                { "index": 2, "transition": "eraseOther", "fields": {} },
                { "index": 3, "transition": "writeComplete", "fields": {} }
            ]
        });
        std::fs::write(&trace_path, veil_json.to_string()).unwrap();

        let rep = replay_veil_trace_file(&trace_path, None, false, false).unwrap();
        assert_eq!(rep.target, ReplayTarget::FlashStore);
        assert!(
            rep.reproduced_violation,
            "real FlashStore MUST reproduce Veil's wraparound durability violation: {rep:?}"
        );
    }

    #[test]
    fn replays_veil_device_engine_coalescing_json_trace_against_real_device_engine() {
        let d = tempfile::tempdir().unwrap();
        let trace_path = d.path().join("veil-de-trace.json");
        // Veil `Trace.toJsonWithPicks` shape from `DeviceEngineUnlatched`.
        let veil_json = serde_json::json!({
            "theory": {},
            "states": [
                { "index": 0, "transition": "after_init", "fields": {} },
                { "index": 1, "transition": "startEmergency", "fields": {} },
                { "index": 2, "transition": "compute", "fields": {} },
                { "index": 3, "transition": "beginSave", "fields": {} },
                { "index": 4, "transition": "savePersist", "fields": {} },
                { "index": 5, "transition": "publish", "fields": {} },
                { "index": 6, "transition": "notify", "fields": {} },
                { "index": 7, "transition": "finish", "fields": {} },
                { "index": 8, "transition": "startClearEmergency", "fields": {} },
                { "index": 9, "transition": "compute", "fields": {} },
                { "index": 10, "transition": "beginSave", "fields": {} },
                { "index": 11, "transition": "savePersist", "fields": {} },
                { "index": 12, "transition": "publish", "fields": {} },
                { "index": 13, "transition": "notify", "fields": {} },
                { "index": 14, "transition": "finish", "fields": {} },
                { "index": 15, "transition": "actuatorPoll", "fields": {} }
            ]
        });
        std::fs::write(&trace_path, veil_json.to_string()).unwrap();

        let rep = replay_veil_trace_file(&trace_path, None, false, false).unwrap();
        assert_eq!(rep.target, ReplayTarget::DeviceEngine);
        assert!(
            rep.reproduced_violation,
            "real DeviceEngine MUST reproduce Veil's coalescing missed-emergency trace: {rep:?}"
        );
    }
}
