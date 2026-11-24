//! Optional Veil temporal verification (`h5i app temporal-check`, `prove --temporal`,
//! `model-check`) and strict Rust counterexample replay (`h5i app trace-replay`).
//!
//! Design split:
//! - **Veil** (`verse-lab/veil`) owns transition-system model checking
//!   (`Veil.ModelChecker.Concrete.findReachableThen`), temporal/liveness/fairness
//!   checking (`Veil.LivenessMC.checkAllWithProgress`), and Lentil temporal logic.
//! - **H5iAppVeil.Gate** (`crates/h5i-app-veil/proofs/H5iAppVeil/Gate.lean`) is
//!   the type-checked Lean module implementing theorem gating (`ci.isTheorem`,
//!   standard axioms only, optional `Kernel.isDefEq` against `statement`) and
//!   structured Veil model/temporal checker invocation.
//! - **h5i** owns:
//!   1. Optional CLI integration (Veil is never required unless opted in;
//!      dependency resolution is delegated directly to Lake).
//!   2. Generating a minimal `.lake/h5i/TemporalGate.lean` driver that imports
//!      `H5iAppVeil.Gate` and the project's built targets, runs each declared
//!      obligation with a per-run delimiter tag, and parses the structured JSON.
//!      Note: the delimiter tag prevents accidental collision with normal
//!      Lean/Veil log output in developer and trusted-CI workflows; it is **not**
//!      a security boundary against arbitrary untrusted Lean metaprograms running
//!      in the same process/filesystem.
//!   3. Strict counterexample & projected-cycle replay directly against the
//!      unmodified Rust implementations (`FlashStore`, `DeviceEngine`).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};
use h5i_app_embedded::replay::{
    DeviceEngineReplayAction, FlashReplayAction, MotorCmd, StoreFault,
    replay_real_device_engine, replay_real_device_engine_lasso, replay_real_flash_store,
};
use h5i_app_report::{
    TemporalCheckKind, TemporalProperty, TemporalPropertyStatus, TemporalReport, TemporalStatus,
    VerificationScope,
};
use serde::{Deserialize, Serialize};

use crate::check::STANDARD_AXIOMS;
use crate::manifest::{Project, TemporalTheoremSpec, TemporalVeilCheckSpec};
use crate::util::{self, Out};

/// CLI arguments for `h5i app model-check` (delegates directly to Lake / Veil).
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

/// Distinguishes exact trace replay from regression probes with synthetic setup/suffixes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayMode {
    /// Replays only the transitions present in the trace file without synthetic actions.
    ExactTrace,
    /// Uses explicit probe options (`--seed-wraparound`, `--append-reboot`, etc.).
    RegressionProbe,
}

/// Honest classification of what trace replay actually demonstrated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayVerdict {
    /// A safety invariant (e.g. `FlashStore` durability or crash consistency) was violated.
    SafetyViolationReproduced,
    /// A cycle closed on the 3-field observable projection
    /// `(snapshot, notify_pending, actuator_motor_on)` while `ActuatorPoll` ran
    /// without observing stop. This checks projected state closure rather than
    /// full concrete runtime state (`version`, `seen`, wakers, `Store`).
    ProjectedCycleReproduced,
    /// A finite trace reached an unstopped state, which does NOT by itself prove a cycle.
    PartialFiniteTrace,
    /// No violation was reproduced on the real runtime.
    NoViolation,
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
    /// to probe the 32-bit wraparound boundary on the real `FlashStore`
    /// (switches mode to `regression_probe`).
    #[arg(long)]
    pub seed_wraparound: bool,
    /// For `flash-store`: append a final `RebootLoad` after the trace even if
    /// the trace does not end with `rebootScan` (switches mode to `regression_probe`).
    #[arg(long)]
    pub append_reboot: bool,
    /// For `flash-store`: explicit byte-operation budget for `powerCut*` transitions
    /// (overrides `"power_cut_budget"` in JSON; required when replaying mid-write/erase cuts).
    #[arg(long)]
    pub power_cut_budget: Option<usize>,
    /// For `device-engine`: replay against the latched motor kernel.
    #[arg(long)]
    pub latched: bool,
    /// Print replay report as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Availability of the optional Veil toolchain (`h5i app doctor`).
/// Dependency resolution itself is delegated directly to Lake.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VeilAvailability {
    Available { detail: String },
    Missing { reason: String },
}

/// Lightweight check for `h5i app doctor`: verifies `lake` is on PATH and
/// reports where the optional Veil Lake package lives.
pub fn detect_veil(project: Option<&Project>) -> VeilAvailability {
    if util::on_path("lake").is_none() {
        return VeilAvailability::Missing {
            reason: "`lake` is not on PATH; Veil requires Lean 4 / Lake (install elan, or keep `[verification] temporal = false`)".into(),
        };
    }
    if let Some(proj) = project {
        let tdir = proj.temporal_dir();
        if tdir.join("lakefile.lean").is_file() || tdir.join("lakefile.toml").is_file() {
            return VeilAvailability::Available {
                detail: format!("lake on PATH; temporal Lake project at {}", tdir.display()),
            };
        }
    }
    if let Ok(cwd) = std::env::current_dir()
        && let Some(top) = util::git_toplevel(&cwd)
    {
        let ws = top.join("crates/h5i-app-veil/proofs");
        if ws.join("lakefile.lean").is_file() {
            return VeilAvailability::Available {
                detail: format!("lake on PATH; workspace Veil package at {}", ws.display()),
            };
        }
    }
    VeilAvailability::Available {
        detail: "lake on PATH (Lake resolves Veil dependencies from each project's lakefile)".into(),
    }
}

/// Run optional Veil temporal verification for `p`.
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
            inputs_current: true,
            properties: Vec::new(),
            detail: Some(
                "not enabled (set `[verification] temporal = true` or pass `--temporal`)".into(),
            ),
        };
        out.note(&format!(
            "{}: temporal verification skipped (not enabled)",
            p.display()
        ));
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
                TemporalStatus::Failed
                | TemporalStatus::Error
                | TemporalStatus::NotRun
                | TemporalStatus::PassedOnOtherInputs => bail!(
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
                inputs_current: true,
                properties: Vec::new(),
                detail: Some(format!("{e:#}")),
            });
            record.run.error = Some(format!("{e:#}"));
            record.finish("failed")?;
            Err(e)
        }
    }
}

/* -------------------------------------------------------------------------- */
/* Structured Lean / Veil Gate Driver (delegates to H5iAppVeil.Gate)          */
/* -------------------------------------------------------------------------- */

fn lean_str(s: &str) -> String {
    serde_json::to_string(s).expect("string serializes as JSON string literal")
}

/// Per-run delimiter tag to distinguish gate JSON lines from normal Lean/Veil
/// log output in developer and trusted-CI workflows. Not a security boundary
/// against untrusted Lean metaprograms running in the same process/filesystem.
fn gate_run_tag() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let mut x = (nanos as u64) ^ ((nanos >> 64) as u64).rotate_left(17) ^ ((pid as u64) << 32);
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^= x >> 31;
    format!("{x:016x}")
}

/// Generate a thin `.lake/h5i/TemporalGate.lean` driver invoking the
/// type-checked `H5iAppVeil.Gate` module on each declared obligation.
///
/// Importing `H5iAppVeil.Gate` (which imports `Veil`) lets Lake/Lean itself
/// enforce that Veil and `H5iAppVeil` are real dependencies of the project.
fn temporal_gate_program(
    targets: &[String],
    theorems: &[TemporalTheoremSpec],
    checks: &[TemporalVeilCheckSpec],
    run_tag: &str,
) -> String {
    let mut s = String::from("import H5iAppVeil.Gate\n");
    for t in targets {
        s.push_str(&format!("import {t}\n"));
    }

    let thm_tag = format!("H5I-TEMPORAL-THM-{run_tag}");
    let veil_tag = format!("H5I-TEMPORAL-VEIL-{run_tag}");
    let done_tag = format!("H5I-TEMPORAL-DONE-{run_tag}");
    let std_list = STANDARD_AXIOMS
        .iter()
        .map(|a| format!("`{a}"))
        .collect::<Vec<_>>()
        .join(", ");

    for thm in theorems {
        let stmt_opt = match thm.statement() {
            Some(st) => format!("(some {})", lean_str(st)),
            None => "none".to_string(),
        };
        s.push_str(&format!(
            "\n#eval H5iAppVeil.Gate.checkTheorem `{} {} [{std_list}] {}\n",
            thm.name(),
            stmt_opt,
            lean_str(&thm_tag)
        ));
    }

    for chk in checks {
        let prop_opt = match chk.kind.as_str() {
            "safety" => "none".to_string(),
            "temporal" => match &chk.property {
                Some(pr) => format!("(some `{pr})"),
                None => format!("(some `{})", chk.name),
            },
            _ => "none".to_string(),
        };
        s.push_str(&format!(
            "\n#eval H5iAppVeil.Gate.runVeilCheck {} `{mod_name} {prop_opt} {} {} {}\n",
            lean_str(&chk.name),
            lean_str(&chk.instantiation),
            lean_str(&chk.theory),
            lean_str(&veil_tag),
            mod_name = chk.module,
        ));
    }

    s.push_str(&format!("\n#eval IO.println {}\n", lean_str(&done_tag)));
    s
}

#[derive(Debug, Deserialize)]
struct GateTheoremRow {
    name: String,
    status: String,
    #[serde(default)]
    axioms: Vec<String>,
    #[serde(default)]
    detail: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GateVeilRow {
    name: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    detail: Option<String>,
    #[serde(default)]
    veil: Option<serde_json::Value>,
}

fn classify_veil_json(
    chk: &TemporalVeilCheckSpec,
    row: &GateVeilRow,
) -> TemporalProperty {
    let kind = if chk.kind == "temporal" {
        TemporalCheckKind::FiniteTemporalCheck
    } else {
        TemporalCheckKind::FiniteModelCheck
    };
    if let Some(st) = &row.status
        && st != "ok"
    {
        return TemporalProperty {
            name: chk.name.clone(),
            kind,
            declared_scope: VerificationScope::ModelLevel,
            status: TemporalPropertyStatus::Failed,
            detail: row.detail.clone(),
        };
    }
    let Some(veil) = &row.veil else {
        return TemporalProperty {
            name: chk.name.clone(),
            kind,
            declared_scope: VerificationScope::ModelLevel,
            status: TemporalPropertyStatus::Failed,
            detail: Some("Veil gate returned no structured result".into()),
        };
    };
    let result = veil.get("result").and_then(|v| v.as_str()).unwrap_or("");
    let explored = veil
        .get("explored_states")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    match (chk.kind.as_str(), result) {
        ("safety", "no_violation_found") => {
            let term_kind = veil
                .pointer("/termination_reason/kind")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if term_kind == "explored_all_reachable_states" {
                TemporalProperty {
                    name: chk.name.clone(),
                    kind,
                    declared_scope: VerificationScope::ModelLevel,
                    status: TemporalPropertyStatus::Passed,
                    detail: Some(format!(
                        "explored {explored} reachable states (complete exhaustive search)"
                    )),
                }
            } else {
                TemporalProperty {
                    name: chk.name.clone(),
                    kind,
                    declared_scope: VerificationScope::ModelLevel,
                    status: TemporalPropertyStatus::Unknown,
                    detail: Some(format!(
                        "search did not complete exhaustively (termination_reason={term_kind:?}, explored={explored})"
                    )),
                }
            }
        }
        ("temporal", "no_violation_found") => {
            let checked = veil
                .get("properties_checked")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let verdict = veil
                .pointer("/timing/properties/0/verdict")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if checked == 1 && verdict == "holds" {
                TemporalProperty {
                    name: chk.name.clone(),
                    kind,
                    declared_scope: VerificationScope::ModelLevel,
                    status: TemporalPropertyStatus::Passed,
                    detail: Some(format!(
                        "Lasso liveness holds (explored {explored} product states)"
                    )),
                }
            } else {
                TemporalProperty {
                    name: chk.name.clone(),
                    kind,
                    declared_scope: VerificationScope::ModelLevel,
                    status: TemporalPropertyStatus::Unknown,
                    detail: Some(format!(
                        "inconclusive liveness result (properties_checked={checked}, verdict={verdict:?})"
                    )),
                }
            }
        }
        (_, "found_violation") => {
            let vkind = veil
                .pointer("/violation/kind")
                .and_then(|v| v.as_str())
                .unwrap_or("violation");
            let violates = veil
                .pointer("/violation/violates")
                .map(|v| v.to_string())
                .unwrap_or_default();
            TemporalProperty {
                name: chk.name.clone(),
                kind,
                declared_scope: VerificationScope::ModelLevel,
                status: TemporalPropertyStatus::Counterexample,
                detail: Some(format!("Veil found {vkind} (violates: {violates})")),
            }
        }
        (_, other) => TemporalProperty {
            name: chk.name.clone(),
            kind,
            declared_scope: VerificationScope::ModelLevel,
            status: TemporalPropertyStatus::Unknown,
            detail: Some(format!("Veil returned result={other:?}")),
        },
    }
}

fn parse_temporal_gate(
    output: &str,
    run_tag: &str,
    success: bool,
    theorems: &[TemporalTheoremSpec],
    checks: &[TemporalVeilCheckSpec],
) -> Result<Vec<TemporalProperty>> {
    let thm_prefix = format!("H5I-TEMPORAL-THM-{run_tag} ");
    let veil_prefix = format!("H5I-TEMPORAL-VEIL-{run_tag} ");
    let done_marker = format!("H5I-TEMPORAL-DONE-{run_tag}");

    let mut thm_rows: std::collections::BTreeMap<String, GateTheoremRow> =
        std::collections::BTreeMap::new();
    let mut veil_rows: std::collections::BTreeMap<String, GateVeilRow> =
        std::collections::BTreeMap::new();
    let mut saw_done = false;

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed == done_marker {
            saw_done = true;
        } else if let Some(json) = trimmed.strip_prefix(&thm_prefix) {
            let row: GateTheoremRow = serde_json::from_str(json)
                .with_context(|| format!("parsing theorem gate JSON: {json}"))?;
            thm_rows.insert(row.name.clone(), row);
        } else if let Some(json) = trimmed.strip_prefix(&veil_prefix) {
            let row: GateVeilRow = serde_json::from_str(json)
                .with_context(|| format!("parsing Veil gate JSON: {json}"))?;
            veil_rows.insert(row.name.clone(), row);
        }
    }

    if !success || !saw_done {
        bail!(
            "temporal Lean/Veil gate did not complete cleanly (exit_ok={success}, saw_done={saw_done}):\n{}",
            output.trim()
        );
    }

    let mut properties = Vec::new();

    for thm in theorems {
        let name = thm.name();
        let declared_scope = thm.declared_scope();
        match thm_rows.get(name) {
            Some(row) if row.status == "ok" => {
                let ax_desc = if row.axioms.is_empty() {
                    "standard axioms only (none)".to_string()
                } else {
                    format!("standard axioms only ({})", row.axioms.join(", "))
                };
                let stmt_desc = if thm.statement().is_some() {
                    "; statement verified by Kernel.isDefEq"
                } else {
                    ""
                };
                properties.push(TemporalProperty {
                    name: name.to_string(),
                    kind: TemporalCheckKind::ProvenTheorem,
                    declared_scope,
                    status: TemporalPropertyStatus::Passed,
                    detail: Some(format!("theorem verified: {ax_desc}{stmt_desc}")),
                });
            }
            Some(row) => {
                properties.push(TemporalProperty {
                    name: name.to_string(),
                    kind: TemporalCheckKind::ProvenTheorem,
                    declared_scope,
                    status: TemporalPropertyStatus::Failed,
                    detail: Some(format!(
                        "{}: {}",
                        row.status,
                        row.detail.as_deref().unwrap_or("verification failed")
                    )),
                });
            }
            None => {
                properties.push(TemporalProperty {
                    name: name.to_string(),
                    kind: TemporalCheckKind::ProvenTheorem,
                    declared_scope,
                    status: TemporalPropertyStatus::Unchecked,
                    detail: Some("gate did not report theorem".into()),
                });
            }
        }
    }

    for chk in checks {
        match veil_rows.get(&chk.name) {
            Some(row) => {
                let _ = &row.kind;
                properties.push(classify_veil_json(chk, row));
            }
            None => {
                let kind = if chk.kind == "temporal" {
                    TemporalCheckKind::FiniteTemporalCheck
                } else {
                    TemporalCheckKind::FiniteModelCheck
                };
                properties.push(TemporalProperty {
                    name: chk.name.clone(),
                    kind,
                    declared_scope: VerificationScope::ModelLevel,
                    status: TemporalPropertyStatus::Unchecked,
                    detail: Some("gate did not report Veil check".into()),
                });
            }
        }
    }

    Ok(properties)
}

fn run_temporal_gate(
    tdir: &Path,
    targets: &[String],
    theorems: &[TemporalTheoremSpec],
    checks: &[TemporalVeilCheckSpec],
    out: &Out,
) -> Result<Vec<TemporalProperty>> {
    let run_tag = gate_run_tag();
    let gate_dir = tdir.join(".lake/h5i");
    std::fs::create_dir_all(&gate_dir)?;
    let gate_file = gate_dir.join("TemporalGate.lean");
    let src = temporal_gate_program(targets, theorems, checks, &run_tag);
    std::fs::write(&gate_file, src)?;

    out.note(&format!("$ lake env lean {}", gate_file.display()));
    let output = Command::new("lake")
        .args(["env", "lean"])
        .arg(&gate_file)
        .current_dir(tdir)
        .output()
        .with_context(|| format!("running `lake env lean` in {}", tdir.display()))?;
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    parse_temporal_gate(&combined, &run_tag, output.status.success(), theorems, checks)
}

fn run_veil_backend(
    p: &Project,
    out: &Out,
    record: &mut h5i_app_report::Recorder,
) -> Result<TemporalReport> {
    if util::on_path("lake").is_none() {
        bail!(
            "{}: temporal verification is enabled (backend = \"veil\"), but `lake` is not on PATH",
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

    let targets = &p.manifest.verification.temporal_targets;
    let req_checks = &p.manifest.verification.temporal_checks;
    let req_theorems = &p.manifest.verification.temporal_theorems;
    if req_checks.is_empty() && req_theorems.is_empty() {
        return Ok(TemporalReport {
            enabled: true,
            backend: "veil".into(),
            status: TemporalStatus::Failed,
            inputs_current: true,
            properties: Vec::new(),
            detail: Some(format!(
                "no verification obligations configured in `[verification]` \
                 (declare `temporal_theorems` and/or `temporal_checks` for {})",
                p.manifest.verification.temporal_dir
            )),
        });
    }
    if targets.is_empty() {
        return Ok(TemporalReport {
            enabled: true,
            backend: "veil".into(),
            status: TemporalStatus::Failed,
            inputs_current: true,
            properties: Vec::new(),
            detail: Some(
                "`[verification] temporal_targets` must name the Lean module(s) to build and gate"
                    .into(),
            ),
        });
    }
    for chk in req_checks {
        if chk.kind != "safety" && chk.kind != "temporal" {
            bail!(
                "{}: invalid temporal_checks kind {:?} for {:?} (expected \"safety\" or \"temporal\")",
                p.display(),
                chk.kind,
                chk.name
            );
        }
    }

    // Textual axiom / sorry / native_decide scan (reusing `h5i app check`'s scanner).
    let bad = crate::check::scan(&tdir)?;
    if !bad.is_empty() {
        return Ok(TemporalReport {
            enabled: true,
            backend: "veil".into(),
            status: TemporalStatus::Failed,
            inputs_current: true,
            properties: Vec::new(),
            detail: Some(format!(
                "forbidden sorry/axiom/native_decide in {}: {}",
                p.manifest.verification.temporal_dir,
                bad.join("; ")
            )),
        });
    }

    // 1. Delegate dependency resolution & build directly to Lake.
    let build_res = crate::report::step(record, "veil_build", out, |log| {
        let mut cmd = Command::new("lake");
        cmd.arg("build").args(targets).current_dir(&tdir);
        util::run(&mut cmd, log)
    });
    if let Err(e) = build_res {
        return Ok(TemporalReport {
            enabled: true,
            backend: "veil".into(),
            status: TemporalStatus::Failed,
            inputs_current: true,
            properties: Vec::new(),
            detail: Some(format!("`lake build` failed: {e:#}")),
        });
    }

    // 2. Run the structured Lean/Veil gate (`H5iAppVeil.Gate`) after `lake build`.
    let properties = crate::report::step(record, "temporal_gate", out, |log| {
        run_temporal_gate(&tdir, targets, req_theorems, req_checks, log)
    })?;

    let passed_count = properties
        .iter()
        .filter(|pr| pr.status == TemporalPropertyStatus::Passed)
        .count();
    let kernel_passed = properties
        .iter()
        .filter(|pr| {
            pr.declared_scope == VerificationScope::KernelImplementationLevel
                && pr.status == TemporalPropertyStatus::Passed
        })
        .count();
    let comp_passed = properties
        .iter()
        .filter(|pr| {
            pr.declared_scope == VerificationScope::Compositional
                && pr.status == TemporalPropertyStatus::Passed
        })
        .count();
    let model_passed = properties
        .iter()
        .filter(|pr| {
            pr.declared_scope == VerificationScope::ModelLevel
                && pr.status == TemporalPropertyStatus::Passed
        })
        .count();
    let unspec_passed = properties
        .iter()
        .filter(|pr| {
            pr.declared_scope == VerificationScope::Unspecified
                && pr.status == TemporalPropertyStatus::Passed
        })
        .count();

    let status = if properties
        .iter()
        .any(|pr| pr.status == TemporalPropertyStatus::Counterexample)
    {
        TemporalStatus::Counterexample
    } else if properties
        .iter()
        .any(|pr| pr.status == TemporalPropertyStatus::Unknown)
    {
        TemporalStatus::Unknown
    } else if properties.is_empty()
        || properties.iter().any(|pr| {
            matches!(
                pr.status,
                TemporalPropertyStatus::Failed | TemporalPropertyStatus::Unchecked
            )
        })
    {
        TemporalStatus::Failed
    } else {
        TemporalStatus::Passed
    };

    let detail = Some(format!(
        "{passed_count}/{} verification obligations passed (declared scopes: {kernel_passed} kernel-impl, {comp_passed} compositional, {model_passed} model, {unspec_passed} unspecified) in {}",
        properties.len(),
        p.manifest.verification.temporal_dir
    ));

    Ok(TemporalReport {
        enabled: true,
        backend: "veil".into(),
        status,
        inputs_current: true,
        properties,
        detail,
    })
}

/// Run `h5i app model-check`: delegates directly to Lake / Veil (Lake resolves dependencies).
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
    let mut cmd = Command::new("lake");
    cmd.arg("build").args(&args.targets).current_dir(&dir);
    util::run(&mut cmd, out)
}

/* -------------------------------------------------------------------------- */
/* Strict Real Rust Counterexample & Projected-Cycle Replay                   */
/* -------------------------------------------------------------------------- */

#[derive(Clone, Debug, Deserialize)]
struct VeilTraceState {
    #[allow(dead_code)]
    index: usize,
    transition: serde_json::Value,
    #[serde(default)]
    #[allow(dead_code)]
    fields: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize)]
struct VeilTraceJson {
    #[serde(default)]
    target: Option<ReplayTarget>,
    /// Index into `states` (0-based) where the lasso cycle begins (`prefix = states[1..=loop_start]`,
    /// `cycle = states[loop_start+1..]`).
    #[serde(default, alias = "lasso_start")]
    loop_start: Option<usize>,
    #[serde(default)]
    seed_wraparound: Option<bool>,
    #[serde(default)]
    append_reboot: Option<bool>,
    #[serde(default)]
    power_cut_budget: Option<usize>,
    #[serde(default)]
    latched: Option<bool>,
    states: Vec<VeilTraceState>,
}

/// Output of `h5i app trace-replay`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplaySummary {
    pub target: ReplayTarget,
    pub mode: ReplayMode,
    pub verdict: ReplayVerdict,
    pub steps_replayed: usize,
    /// `true` when a safety violation or projected cycle was reproduced on the real runtime.
    pub reproduced_violation: bool,
    pub detail: String,
}

/// Run `h5i app trace-replay` against the actual Rust `FlashStore` or `DeviceEngine`.
pub fn trace_replay(args: &TraceReplayArgs) -> Result<()> {
    let summary = replay_veil_trace_file(
        &args.trace,
        args.target,
        args.seed_wraparound,
        args.append_reboot,
        args.power_cut_budget,
        args.latched,
    )?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        println!("target:       {:?}", summary.target);
        println!("mode:         {:?}", summary.mode);
        println!("verdict:      {:?}", summary.verdict);
        println!("steps:        {}", summary.steps_replayed);
        println!("reproduced:   {}", summary.reproduced_violation);
        println!("detail:       {}", summary.detail);
    }
    Ok(())
}

/// Strictly parse a Veil JSON trace (`Trace.toJsonWithPicks`) and replay it directly
/// against the real `h5i_app_embedded::store::FlashStore` or
/// `h5i_app_embedded::kernel::DeviceEngine`.
pub fn replay_veil_trace_file(
    path: &Path,
    target_override: Option<ReplayTarget>,
    seed_wraparound: bool,
    append_reboot: bool,
    power_cut_budget_override: Option<usize>,
    latched_override: bool,
) -> Result<ReplaySummary> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading Veil trace {}", path.display()))?;
    let trace: VeilTraceJson = serde_json::from_str(&text)
        .with_context(|| format!("parsing Veil JSON trace {}", path.display()))?;

    let transitions: Vec<String> = trace
        .states
        .iter()
        .skip(1)
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
            let append = append_reboot || trace.append_reboot.unwrap_or(false);
            let cut_budget = power_cut_budget_override.or(trace.power_cut_budget);
            let mode = if wrap || append {
                ReplayMode::RegressionProbe
            } else {
                ReplayMode::ExactTrace
            };

            let mut actions = Vec::new();
            let mut pending_val: Option<u64> = None;
            let mut next_val = 1u64;
            for (step_idx, tr) in transitions.iter().enumerate() {
                let name = tr.trim();
                let op = name.split_whitespace().next().unwrap_or("");
                match op {
                    "beginSave" | "beginSaveAppend" | "beginSaveSwitch" => {
                        let v = if name.contains("true") {
                            2
                        } else if name.contains("false") {
                            1
                        } else {
                            next_val += 1;
                            next_val
                        };
                        pending_val = Some(v);
                    }
                    "saveUnchanged" => {
                        let v = if name.contains("true") {
                            2
                        } else if name.contains("false") {
                            1
                        } else {
                            next_val
                        };
                        actions.push(FlashReplayAction::Save(v));
                    }
                    "eraseOther" | "ackSave" | "powerCutClean" => {}
                    "writeComplete" | "writeLanded" => {
                        let v = pending_val.take().unwrap_or_else(|| {
                            next_val += 1;
                            next_val
                        });
                        actions.push(FlashReplayAction::Save(v));
                    }
                    "powerCut" | "powerCutPartialErase" | "powerCutTornWrite" => {
                        if let Some(v) = pending_val.take() {
                            let budget = cut_budget.ok_or_else(|| {
                                anyhow::anyhow!(
                                    "step {}: `{name}` during an in-flight write/erase requires an explicit \
                                     `--power-cut-budget <N>` (or JSON `\"power_cut_budget\"`) for faithful replay",
                                    step_idx + 1
                                )
                            })?;
                            actions.push(FlashReplayAction::SaveWithCut { val: v, budget });
                        }
                    }
                    "rebootScan" => actions.push(FlashReplayAction::RebootLoad),
                    _ => bail!(
                        "unsupported transition {:?} at step {} for target `flash-store`",
                        name,
                        step_idx + 1
                    ),
                }
            }
            if append && !matches!(actions.last(), Some(FlashReplayAction::RebootLoad)) {
                actions.push(FlashReplayAction::RebootLoad);
            }
            let seed = if wrap { Some((u32::MAX, 1u64)) } else { None };
            let out = replay_real_flash_store(seed, &actions);
            let reproduced = out.durability_violated || out.crash_consistency_violated;
            let verdict = if reproduced {
                ReplayVerdict::SafetyViolationReproduced
            } else {
                ReplayVerdict::NoViolation
            };
            Ok(ReplaySummary {
                target,
                mode,
                verdict,
                steps_replayed: out.steps_replayed,
                reproduced_violation: reproduced,
                detail: format!(
                    "real h5i_app_embedded::store::FlashStore (mode={mode:?}): last_acked={:?}, last_loaded={:?}, durability_violated={}, crash_consistency_violated={}, erases={}",
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
            let translate = |slice: &[String], offset: usize, pending_cmd: &mut Option<MotorCmd>| -> Result<Vec<DeviceEngineReplayAction>> {
                let mut actions = Vec::new();
                for (i, tr) in slice.iter().enumerate() {
                    let name = tr.trim();
                    let op = name.split_whitespace().next().unwrap_or("");
                    match op {
                        "startEmergency" => *pending_cmd = Some(MotorCmd::RequestEmergency),
                        "startClearEmergency" => *pending_cmd = Some(MotorCmd::ClearEmergency),
                        "compute" | "beginSave" | "savePersist" | "notify" | "finish"
                        | "monitorPoll" | "powerLoss" => {}
                        "publish" => {
                            if let Some(cmd) = pending_cmd.take() {
                                actions.push(DeviceEngineReplayAction::Execute(cmd));
                            }
                        }
                        "saveFailClean" => {
                            if let Some(cmd) = pending_cmd.take() {
                                actions.push(DeviceEngineReplayAction::ExecuteWithStoreFault(
                                    cmd,
                                    StoreFault::FailClean,
                                ));
                            }
                        }
                        "saveFailAmbiguous" => {
                            if let Some(cmd) = pending_cmd.take() {
                                actions.push(DeviceEngineReplayAction::ExecuteWithStoreFault(
                                    cmd,
                                    StoreFault::FailAmbiguous,
                                ));
                            }
                        }
                        "actuatorPoll" => actions.push(DeviceEngineReplayAction::ActuatorPoll),
                        "restore" | "crashAndRestore" => {
                            *pending_cmd = None;
                            actions.push(DeviceEngineReplayAction::Restore);
                        }
                        _ => bail!(
                            "unsupported transition {:?} at step {} for target `device-engine`",
                            name,
                            offset + i + 1
                        ),
                    }
                }
                Ok(actions)
            };

            if let Some(loop_start) = trace.loop_start {
                if loop_start >= trace.states.len() {
                    bail!(
                        "invalid `loop_start` {} for trace with {} states",
                        loop_start,
                        trace.states.len()
                    );
                }
                let mut pending = None;
                let prefix_actions = translate(&transitions[..loop_start], 0, &mut pending)?;
                let cycle_actions = translate(&transitions[loop_start..], loop_start, &mut pending)?;
                let out = if latched {
                    replay_real_device_engine_lasso::<true>(&prefix_actions, &cycle_actions)
                } else {
                    replay_real_device_engine_lasso::<false>(&prefix_actions, &cycle_actions)
                };
                let verdict = if out.projected_cycle_reproduced {
                    ReplayVerdict::ProjectedCycleReproduced
                } else if out.emergency_committed && !out.actuator_observed_stop {
                    ReplayVerdict::PartialFiniteTrace
                } else {
                    ReplayVerdict::NoViolation
                };
                return Ok(ReplaySummary {
                    target,
                    mode: ReplayMode::ExactTrace,
                    verdict,
                    steps_replayed: out.prefix_steps + out.cycle_steps * 2,
                    reproduced_violation: out.projected_cycle_reproduced,
                    detail: format!(
                        "real h5i_app_embedded::kernel::DeviceEngine projected-cycle replay (latched={latched}, projection=(snapshot, notify_pending, actuator_motor_on)): emergency_committed={}, projected_cycle_closed={}, actuator_poll_in_cycle={}, actuator_observed_stop={}, clear_refused={}, final_version={}",
                        out.emergency_committed,
                        out.projected_cycle_closed,
                        out.actuator_poll_in_cycle,
                        out.actuator_observed_stop,
                        out.clear_refused_count,
                        out.final_version
                    ),
                });
            }

            let mut pending = None;
            let actions = translate(&transitions, 0, &mut pending)?;
            let out = if latched {
                replay_real_device_engine::<true>(&actions)
            } else {
                replay_real_device_engine::<false>(&actions)
            };
            let verdict = if out.emergency_committed && !out.actuator_observed_stop {
                ReplayVerdict::PartialFiniteTrace
            } else {
                ReplayVerdict::NoViolation
            };
            Ok(ReplaySummary {
                target,
                mode: ReplayMode::ExactTrace,
                verdict,
                steps_replayed: out.steps_replayed,
                reproduced_violation: false,
                detail: format!(
                    "real h5i_app_embedded::kernel::DeviceEngine finite trace (latched={latched}, no lasso `loop_start`): emergency_committed={}, actuator_observed_stop={}, clear_refused={}, final_version={} (finite trace alone does not reproduce a cycle)",
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
        if tr.contains("beginSave")
            || tr.contains("saveUnchanged")
            || tr.contains("writeComplete")
            || tr.contains("writeLanded")
            || tr.contains("rebootScan")
        {
            return Some(ReplayTarget::FlashStore);
        }
        if tr.contains("startEmergency")
            || tr.contains("actuatorPoll")
            || tr.contains("saveFailAmbiguous")
        {
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
    fn enabled_veil_without_lake_project_returns_error_not_skip() {
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
            msg.contains("is not on PATH") || msg.contains("is not a Lake project"),
            "expected explicit error when lake/temporal project missing, got: {msg}"
        );
    }

    #[test]
    fn gate_program_delegates_to_type_checked_h5i_app_veil_gate_module() {
        let thms = vec![TemporalTheoremSpec::Name("Demo.thm".into())];
        let checks = vec![TemporalVeilCheckSpec {
            name: "Demo#model_check".into(),
            module: "Demo".into(),
            kind: "safety".into(),
            property: None,
            instantiation: "{}".into(),
            theory: "{}".into(),
        }];
        let src = temporal_gate_program(&["Demo".into()], &thms, &checks, "tag123");
        assert!(src.contains("import H5iAppVeil.Gate"));
        assert!(src.contains("#eval H5iAppVeil.Gate.checkTheorem `Demo.thm"));
        assert!(src.contains("#eval H5iAppVeil.Gate.runVeilCheck \"Demo#model_check\" `Demo none"));
        assert_eq!(thms[0].declared_scope(), VerificationScope::Unspecified);
    }

    #[test]
    fn gate_parser_rejects_def_and_statement_mismatch_and_mismatched_tag() {
        let theorems = vec![
            TemporalTheoremSpec::Name("Demo.fake_def".into()),
            TemporalTheoremSpec::Detailed {
                name: "Demo.wrong_prop".into(),
                statement: Some("∀ n : Nat, n = n".into()),
                scope: Some(VerificationScope::Compositional),
            },
            TemporalTheoremSpec::Detailed {
                name: "Demo.real_thm".into(),
                statement: Some("∀ n : Nat, n = n".into()),
                scope: Some(VerificationScope::KernelImplementationLevel),
            },
        ];
        let checks = vec![
            TemporalVeilCheckSpec {
                name: "Demo#model_check".into(),
                module: "Demo".into(),
                kind: "safety".into(),
                property: None,
                instantiation: "{}".into(),
                theory: "{}".into(),
            },
            TemporalVeilCheckSpec {
                name: "live_prop".into(),
                module: "Demo".into(),
                kind: "temporal".into(),
                property: Some("live_prop".into()),
                instantiation: "{}".into(),
                theory: "{}".into(),
            },
        ];
        let run_tag = "deadbeef01234567";
        // Lines with a different delimiter tag must be ignored.
        let output = format!(
            "H5I-TEMPORAL-THM-other00000000000 {{\"name\":\"Demo.fake_def\",\"status\":\"ok\",\"axioms\":[]}}\n\
             H5I-TEMPORAL-THM-{run_tag} {{\"name\":\"Demo.fake_def\",\"status\":\"not_theorem\",\"detail\":\"expected a theorem, found `def`\"}}\n\
             H5I-TEMPORAL-THM-{run_tag} {{\"name\":\"Demo.wrong_prop\",\"status\":\"statement_mismatch\",\"detail\":\"expected `∀ n, n = n`, found `True`\"}}\n\
             H5I-TEMPORAL-THM-{run_tag} {{\"name\":\"Demo.real_thm\",\"status\":\"ok\",\"axioms\":[\"propext\"]}}\n\
             H5I-TEMPORAL-VEIL-{run_tag} {{\"name\":\"Demo#model_check\",\"kind\":\"safety\",\"veil\":{{\"result\":\"no_violation_found\",\"explored_states\":576,\"termination_reason\":{{\"kind\":\"explored_all_reachable_states\"}}}}}}\n\
             H5I-TEMPORAL-VEIL-{run_tag} {{\"name\":\"live_prop\",\"kind\":\"temporal\",\"veil\":{{\"result\":\"no_violation_found\",\"properties_checked\":1,\"explored_states\":1152,\"timing\":{{\"properties\":[{{\"property\":\"live_prop\",\"verdict\":\"holds\"}}]}}}}}}\n\
             H5I-TEMPORAL-DONE-{run_tag}\n"
        );
        let props = parse_temporal_gate(&output, run_tag, true, &theorems, &checks).unwrap();
        assert_eq!(props.len(), 5);
        assert_eq!(props[0].status, TemporalPropertyStatus::Failed);
        assert_eq!(props[0].declared_scope, VerificationScope::Unspecified);
        assert!(props[0].detail.as_deref().unwrap().contains("not_theorem"));
        assert_eq!(props[1].status, TemporalPropertyStatus::Failed);
        assert_eq!(props[1].declared_scope, VerificationScope::Compositional);
        assert!(props[1].detail.as_deref().unwrap().contains("statement_mismatch"));
        assert_eq!(props[2].status, TemporalPropertyStatus::Passed);
        assert_eq!(props[2].declared_scope, VerificationScope::KernelImplementationLevel);
        assert_eq!(props[3].status, TemporalPropertyStatus::Passed);
        assert_eq!(props[3].declared_scope, VerificationScope::ModelLevel);
        assert_eq!(props[4].status, TemporalPropertyStatus::Passed);
        assert_eq!(props[4].declared_scope, VerificationScope::ModelLevel);

        // Missing DONE marker must fail even if exit status is true.
        let missing_done = output.replace(&format!("H5I-TEMPORAL-DONE-{run_tag}"), "");
        assert!(parse_temporal_gate(&missing_done, run_tag, true, &theorems, &checks).is_err());
    }

    #[test]
    fn gate_parser_marks_incomplete_depth_bounded_veil_search_as_unknown() {
        let checks = vec![TemporalVeilCheckSpec {
            name: "Demo#model_check".into(),
            module: "Demo".into(),
            kind: "safety".into(),
            property: None,
            instantiation: "{}".into(),
            theory: "{}".into(),
        }];
        let nonce = "0123456789abcdef";
        let output = format!(
            "H5I-TEMPORAL-VEIL-{nonce} {{\"name\":\"Demo#model_check\",\"kind\":\"safety\",\"veil\":{{\"result\":\"no_violation_found\",\"explored_states\":42,\"termination_reason\":{{\"kind\":\"early_termination\"}}}}}}\n\
             H5I-TEMPORAL-DONE-{nonce}\n"
        );
        let props = parse_temporal_gate(&output, nonce, true, &[], &checks).unwrap();
        assert_eq!(props[0].status, TemporalPropertyStatus::Unknown);
    }

    #[test]
    fn replays_veil_flash_store_wraparound_json_trace_and_verifies_fix() {
        let d = tempfile::tempdir().unwrap();
        let trace_path = d.path().join("veil-flash-trace.json");
        let veil_json = serde_json::json!({
            "theory": {},
            "seed_wraparound": true,
            "states": [
                { "index": 0, "transition": "after_init", "fields": {} },
                { "index": 1, "transition": "beginSave true", "fields": {} },
                { "index": 2, "transition": "eraseOther", "fields": {} },
                { "index": 3, "transition": "writeComplete", "fields": {} },
                { "index": 4, "transition": "rebootScan", "fields": {} }
            ]
        });
        std::fs::write(&trace_path, veil_json.to_string()).unwrap();

        let rep = replay_veil_trace_file(&trace_path, None, false, false, None, false).unwrap();
        assert_eq!(rep.target, ReplayTarget::FlashStore);
        assert_eq!(rep.mode, ReplayMode::RegressionProbe);
        assert_eq!(rep.verdict, ReplayVerdict::NoViolation);
        assert!(!rep.reproduced_violation);
    }

    #[test]
    fn rejects_unsupported_transitions_strictly() {
        let d = tempfile::tempdir().unwrap();
        let trace_path = d.path().join("bad-trace.json");
        let veil_json = serde_json::json!({
            "target": "flash-store",
            "states": [
                { "index": 0, "transition": "after_init", "fields": {} },
                { "index": 1, "transition": "totallyUnknownAction", "fields": {} }
            ]
        });
        std::fs::write(&trace_path, veil_json.to_string()).unwrap();

        let err = replay_veil_trace_file(&trace_path, None, false, false, None, false).unwrap_err();
        assert!(format!("{err:#}").contains("unsupported transition"));
    }

    #[test]
    fn finite_device_engine_trace_without_lasso_is_partial_not_reproduced_cycle() {
        let d = tempfile::tempdir().unwrap();
        let trace_path = d.path().join("finite-de-trace.json");
        let veil_json = serde_json::json!({
            "theory": {},
            "states": [
                { "index": 0, "transition": "after_init", "fields": {} },
                { "index": 1, "transition": "startEmergency", "fields": {} },
                { "index": 2, "transition": "compute", "fields": {} },
                { "index": 3, "transition": "beginSave", "fields": {} },
                { "index": 4, "transition": "savePersist", "fields": {} },
                { "index": 5, "transition": "publish", "fields": {} }
            ]
        });
        std::fs::write(&trace_path, veil_json.to_string()).unwrap();

        let rep = replay_veil_trace_file(&trace_path, None, false, false, None, false).unwrap();
        assert_eq!(rep.target, ReplayTarget::DeviceEngine);
        assert_eq!(rep.verdict, ReplayVerdict::PartialFiniteTrace);
        assert!(!rep.reproduced_violation);
    }

    #[test]
    fn replays_veil_device_engine_coalescing_lasso_as_projected_cycle() {
        let d = tempfile::tempdir().unwrap();
        let trace_path = d.path().join("veil-de-lasso.json");
        let veil_json = serde_json::json!({
            "theory": {},
            "loop_start": 0,
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

        let rep = replay_veil_trace_file(&trace_path, None, false, false, None, false).unwrap();
        assert_eq!(rep.target, ReplayTarget::DeviceEngine);
        assert_eq!(rep.verdict, ReplayVerdict::ProjectedCycleReproduced);
        assert!(rep.reproduced_violation);
        assert!(rep.detail.contains("projection=(snapshot, notify_pending, actuator_motor_on)"));
    }
}
