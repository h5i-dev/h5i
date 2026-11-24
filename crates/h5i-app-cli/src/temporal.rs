//! Optional Veil temporal verification (`h5i app temporal-check`, `prove --temporal`,
//! `model-check`) and strict Rust counterexample / lasso replay (`h5i app trace-replay`).
//!
//! Design split:
//! - **Veil** (`verse-lab/veil`) owns transition-system model checking
//!   (`#model_check`), temporal/liveness/fairness checking (`#temporal_check`
//!   via Lasso & Lentil), and SMT invariant checking (`#check_invariants`).
//! - **h5i** owns:
//!   1. Optional CLI integration (Veil is never required unless opted in;
//!      dependency resolution is delegated directly to Lake).
//!   2. Explicit obligation gating: `lake build == 0` is NEVER equated with
//!      verification success. `temporal-check` validates that the package uses
//!      Veil, scans for `sorry`/`native_decide`/`axiom`, parses per-property
//!      Veil check outputs, rejects `veil.violationIsError false` on required
//!      properties, and runs a Lean axiom gate on `temporal_theorems`.
//!   3. Strict counterexample & lasso replay directly against the unmodified
//!      Rust implementations (`FlashStore`, `DeviceEngine`, `MemoryEngine`).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};
use h5i_app_embedded::replay::{
    DeviceEngineReplayAction, FlashReplayAction, MotorCmd, StoreFault,
    replay_real_device_engine, replay_real_device_engine_lasso, replay_real_flash_store,
};
use h5i_app_report::{
    TemporalCheckKind, TemporalProperty, TemporalPropertyStatus, TemporalReport, TemporalStatus,
};
use serde::{Deserialize, Serialize};

use crate::check::STANDARD_AXIOMS;
use crate::manifest::Project;
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
    /// A closed fair lasso (`prefix · cycle^ω`) reproduced a liveness violation on the real runtime.
    LivenessLassoReproduced,
    /// A finite trace reached an unstopped state, which does NOT by itself prove a liveness violation.
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
/// reports where the optional Veil Lake package lives. Does NOT reject valid
/// unfetched Lake projects; Lake resolves Veil dependencies when built.
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
///
/// Policy:
/// - Disabled (`!force_enable && !p.manifest.verification.temporal`): cleanly
///   returns `Ok(Skipped)` without requiring `lake` or Veil.
/// - Enabled: validates the Veil Lake project, scans for `sorry`/`axiom`/
///   `native_decide`, builds via Lake, verifies every required Veil property
///   and Lean theorem obligation, and records property-level results.
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

/// One `#model_check` or `#temporal_check` directive discovered in temporal `.lean` sources.
#[derive(Clone, Debug, PartialEq, Eq)]
struct DiscoveredCheck {
    rel_file: String,
    line: usize,
    kind: TemporalCheckKind,
    name: String,
    suppressed: bool,
}

fn lakefile_uses_veil(tdir: &Path) -> Result<bool> {
    for fname in ["lakefile.lean", "lakefile.toml"] {
        let p = tdir.join(fname);
        if p.is_file() {
            let text = std::fs::read_to_string(&p)
                .with_context(|| format!("reading {}", p.display()))?;
            let uses = text.lines().any(|line| {
                let code = line.split("--").next().unwrap_or("").trim();
                (code.contains("require") && (code.contains("veil") || code.contains("h5i_app_veil")))
                    || code.contains("name = \"veil\"")
                    || code.contains("name = \"h5i_app_veil\"")
            });
            return Ok(uses);
        }
    }
    Ok(false)
}

fn collect_temporal_lean_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name();
        if p.is_dir() {
            if !matches!(name.to_string_lossy().as_ref(), ".lake" | "target" | ".git") {
                collect_temporal_lean_files(&p, out);
            }
        } else if p.extension().is_some_and(|x| x == "lean") && name != "lakefile.lean" {
            out.push(p);
        }
    }
}

fn discover_veil_checks(tdir: &Path) -> Result<(Vec<PathBuf>, Vec<DiscoveredCheck>)> {
    let mut files = Vec::new();
    collect_temporal_lean_files(tdir, &mut files);
    files.sort();
    let mut checks = Vec::new();
    for f in &files {
        let rel = f
            .strip_prefix(tdir)
            .unwrap_or(f)
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(f)
            .with_context(|| format!("reading {}", f.display()))?;
        let mut current_module: Option<String> = None;
        let mut suppress_next = false;
        let mut block_comment_depth: usize = 0;
        for (idx, raw) in text.lines().enumerate() {
            let line_no = idx + 1;
            let stripped = strip_lean_comments_line(raw, &mut block_comment_depth);
            let code = stripped.trim();
            if code.is_empty() {
                continue;
            }
            if let Some(rest) = code.strip_prefix("veil module ") {
                current_module = rest.split_whitespace().next().map(str::to_string);
            }
            let line_suppresses = code.contains("veil.violationIsError") && code.contains("false");
            let suppressed = suppress_next || line_suppresses;
            if code.starts_with("#model_check") || code.contains(" #model_check") {
                let name = match &current_module {
                    Some(m) => format!("{m}#model_check"),
                    None => format!("{rel}:{line_no}#model_check"),
                };
                checks.push(DiscoveredCheck {
                    rel_file: rel.clone(),
                    line: line_no,
                    kind: TemporalCheckKind::FiniteModelCheck,
                    name,
                    suppressed,
                });
                suppress_next = false;
            } else if let Some(pos) = code.find("#temporal_check")
                && (pos == 0 || code.as_bytes()[pos - 1].is_ascii_whitespace())
            {
                let after = &code[pos + "#temporal_check".len()..];
                let prop = extract_bracketed_name(after).unwrap_or_else(|| match &current_module {
                    Some(m) => format!("{m}#temporal_check:{line_no}"),
                    None => format!("{rel}:{line_no}#temporal_check"),
                });
                checks.push(DiscoveredCheck {
                    rel_file: rel.clone(),
                    line: line_no,
                    kind: TemporalCheckKind::FiniteTemporalCheck,
                    name: prop,
                    suppressed,
                });
                suppress_next = false;
            } else if line_suppresses && code.ends_with("in") {
                suppress_next = true;
            } else if !line_suppresses {
                suppress_next = false;
            }
        }
    }
    Ok((files, checks))
}

/// Strip `--` line comments and nested `/- ... -/` block comments from one Lean source line.
fn strip_lean_comments_line(line: &str, block_depth: &mut usize) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < bytes.len() {
        if *block_depth == 0 {
            if i + 1 < bytes.len() && bytes[i] == b'-' && bytes[i + 1] == b'-' {
                break;
            }
            if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'-' {
                *block_depth += 1;
                i += 2;
                continue;
            }
            if let Some(ch) = line[i..].chars().next() {
                out.push(ch);
                i += ch.len_utf8();
            } else {
                break;
            }
        } else if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'-' {
            *block_depth += 1;
            i += 2;
        } else if i + 1 < bytes.len() && bytes[i] == b'-' && bytes[i + 1] == b'/' {
            *block_depth -= 1;
            i += 2;
        } else {
            i += 1;
        }
    }
    out
}

fn extract_bracketed_name(s: &str) -> Option<String> {
    let start = s.find('[')?;
    let end = s[start + 1..].find(']')?;
    let inner = s[start + 1..start + 1 + end].trim();
    if inner.is_empty() {
        None
    } else {
        Some(inner.to_string())
    }
}

/// Parse Veil info/error lines from `lake build` output keyed by `(rel_file, line)`.
fn parse_veil_build_output(output: &str) -> BTreeMap<(String, usize), (TemporalPropertyStatus, String)> {
    let mut map = BTreeMap::new();
    for line in output.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed
            .strip_prefix("info: ")
            .or_else(|| trimmed.strip_prefix("error: "))
            .or_else(|| trimmed.strip_prefix("warning: "))
        else {
            continue;
        };
        // Format: `<file.lean>:<line>:<col>: <message>`
        let mut parts = rest.splitn(4, ':');
        let (Some(file), Some(line_s), Some(_col), Some(msg)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let Ok(line_no) = line_s.trim().parse::<usize>() else {
            continue;
        };
        let msg = msg.trim();
        let status = if msg.contains("No liveness violation") || msg.contains("No violation") {
            TemporalPropertyStatus::Passed
        } else if msg.contains("Violation:")
            || msg.contains("safety_failure")
            || msg.contains("liveness_failure")
        {
            TemporalPropertyStatus::Counterexample
        } else if msg.contains("Unknown")
            || msg.contains("inconclusive")
            || msg.contains("timed out")
        {
            TemporalPropertyStatus::Unknown
        } else {
            continue;
        };
        map.insert((file.replace('\\', "/"), line_no), (status, msg.to_string()));
    }
    map
}

fn verify_temporal_theorems(
    tdir: &Path,
    lean_files: &[PathBuf],
    targets: &[String],
    theorems: &[String],
) -> Result<Vec<TemporalProperty>> {
    if theorems.is_empty() {
        return Ok(Vec::new());
    }
    let mut imports: BTreeSet<String> = BTreeSet::new();
    if !targets.is_empty() {
        for t in targets {
            imports.insert(t.clone());
        }
    } else {
        for f in lean_files {
            if let Ok(rel) = f.strip_prefix(tdir) {
                let mod_name = rel
                    .with_extension("")
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(".");
                if !mod_name.is_empty() {
                    imports.insert(mod_name);
                }
            }
        }
    }

    let mut script = String::new();
    for m in &imports {
        script.push_str(&format!("import {m}\n"));
    }
    script.push_str("import Lean\nopen Lean in\n#eval show CoreM Unit from do\n");
    script.push_str("  let env ← getEnv\n");
    for thm in theorems {
        script.push_str(&format!(
            "  let n := `{thm}\n  match env.find? n with\n  | none => IO.println s!\"H5I_THM_MISSING {thm}\"\n  | some _ =>\n    let axsArr ← Lean.collectAxioms n\n    let axs := String.intercalate \",\" (axsArr.toList.map toString)\n    IO.println s!\"H5I_THM_AXIOMS {thm} {{axs}}\"\n"
        ));
    }

    let mut child = Command::new("lake")
        .args(["env", "lean", "--stdin"])
        .current_dir(tdir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning `lake env lean --stdin` in {}", tdir.display()))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(script.as_bytes())?;
    }
    let out = child.wait_with_output()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        bail!(
            "Lean axiom gate for temporal_theorems failed: {}{}",
            stdout.trim(),
            stderr.trim()
        );
    }

    let mut results = Vec::new();
    for thm in theorems {
        let missing_tag = format!("H5I_THM_MISSING {thm}");
        let axioms_prefix = format!("H5I_THM_AXIOMS {thm} ");
        if stdout.lines().any(|l| l.trim() == missing_tag) {
            results.push(TemporalProperty {
                name: thm.clone(),
                kind: TemporalCheckKind::ProvenTheorem,
                status: TemporalPropertyStatus::Failed,
                detail: Some("theorem not found in built environment".into()),
            });
        } else if let Some(line) = stdout.lines().find(|l| l.starts_with(&axioms_prefix)) {
            let raw_axs = line[axioms_prefix.len()..].trim();
            let axs: Vec<&str> = if raw_axs.is_empty() {
                Vec::new()
            } else {
                raw_axs.split(',').map(str::trim).filter(|s| !s.is_empty()).collect()
            };
            let bad: Vec<&str> = axs
                .iter()
                .copied()
                .filter(|a| !STANDARD_AXIOMS.contains(a))
                .collect();
            if bad.is_empty() {
                results.push(TemporalProperty {
                    name: thm.clone(),
                    kind: TemporalCheckKind::ProvenTheorem,
                    status: TemporalPropertyStatus::Passed,
                    detail: Some(if axs.is_empty() {
                        "standard axioms only (none)".into()
                    } else {
                        format!("standard axioms only ({})", axs.join(", "))
                    }),
                });
            } else {
                results.push(TemporalProperty {
                    name: thm.clone(),
                    kind: TemporalCheckKind::ProvenTheorem,
                    status: TemporalPropertyStatus::Failed,
                    detail: Some(format!("disallowed axioms: {}", bad.join(", "))),
                });
            }
        } else {
            results.push(TemporalProperty {
                name: thm.clone(),
                kind: TemporalCheckKind::ProvenTheorem,
                status: TemporalPropertyStatus::Unchecked,
                detail: Some("axiom gate did not report theorem".into()),
            });
        }
    }
    Ok(results)
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
    if !lakefile_uses_veil(&tdir)? {
        return Ok(TemporalReport {
            enabled: true,
            backend: "veil".into(),
            status: TemporalStatus::Failed,
            inputs_current: true,
            properties: Vec::new(),
            detail: Some(format!(
                "{} does not require `veil` or `h5i_app_veil` in its lakefile",
                p.manifest.verification.temporal_dir
            )),
        });
    }

    let (lean_files, discovered) = discover_veil_checks(&tdir)?;
    if lean_files.is_empty() {
        return Ok(TemporalReport {
            enabled: true,
            backend: "veil".into(),
            status: TemporalStatus::Failed,
            inputs_current: true,
            properties: Vec::new(),
            detail: Some(format!(
                "no .lean files found in {}",
                p.manifest.verification.temporal_dir
            )),
        });
    }

    // Textual axiom / sorry / native_decide scan (same gate as `h5i app check`).
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

    let req_props = &p.manifest.verification.temporal_properties;
    let req_theorems = &p.manifest.verification.temporal_theorems;
    let has_unsuppressed_check = discovered.iter().any(|c| !c.suppressed);
    if req_props.is_empty() && req_theorems.is_empty() && !has_unsuppressed_check {
        return Ok(TemporalReport {
            enabled: true,
            backend: "veil".into(),
            status: TemporalStatus::Failed,
            inputs_current: true,
            properties: Vec::new(),
            detail: Some(format!(
                "no verification obligations found in {} (declare unsuppressed #model_check/#temporal_check or set `[verification] temporal_properties` / `temporal_theorems`)",
                p.manifest.verification.temporal_dir
            )),
        });
    }

    // Delegate dependency resolution & build directly to Lake; capture output for property-level parsing.
    let mut build_output = String::new();
    let targets = &p.manifest.verification.temporal_targets;
    let build_res = crate::report::step(record, "veil_build", out, |log| {
        let mut cmd = Command::new("lake");
        cmd.arg("build").args(targets).current_dir(&tdir);
        let output = cmd
            .output()
            .with_context(|| format!("running `lake build` in {}", tdir.display()))?;
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if let Out::Log(path) = log {
            let _ = std::fs::write(path, &combined);
        }
        build_output = combined;
        if !output.status.success() {
            bail!("`lake build` exited with {}", output.status);
        }
        Ok(())
    });

    let parsed = parse_veil_build_output(&build_output);
    let mut properties: Vec<TemporalProperty> = Vec::new();
    let mut seen_names: BTreeSet<String> = BTreeSet::new();

    for c in &discovered {
        let is_required = req_props.contains(&c.name);
        if c.suppressed && !is_required {
            // Intentional negative/regression check (e.g. `set_option veil.violationIsError false`),
            // not a required passing obligation; still record its actual status transparently.
            let (status, detail) = match parsed.get(&(c.rel_file.clone(), c.line)) {
                Some((st, msg)) => (*st, format!("suppressed regression check: {msg}")),
                None => (
                    TemporalPropertyStatus::Unchecked,
                    "suppressed regression check (not observed in build output)".into(),
                ),
            };
            properties.push(TemporalProperty {
                name: c.name.clone(),
                kind: c.kind,
                status,
                detail: Some(detail),
            });
            continue;
        }
        seen_names.insert(c.name.clone());
        if c.suppressed && is_required {
            properties.push(TemporalProperty {
                name: c.name.clone(),
                kind: c.kind,
                status: TemporalPropertyStatus::Failed,
                detail: Some(
                    "required property must not use `set_option veil.violationIsError false`".into(),
                ),
            });
            continue;
        }
        match parsed.get(&(c.rel_file.clone(), c.line)) {
            Some((st, msg)) => properties.push(TemporalProperty {
                name: c.name.clone(),
                kind: c.kind,
                status: *st,
                detail: Some(msg.clone()),
            }),
            None => properties.push(TemporalProperty {
                name: c.name.clone(),
                kind: c.kind,
                status: TemporalPropertyStatus::Unchecked,
                detail: Some(format!(
                    "not observed in `lake build` output ({}:{}; ensure module is in build targets)",
                    c.rel_file, c.line
                )),
            }),
        }
    }

    // Any required property not even declared in the sources is explicitly Unchecked.
    for req in req_props {
        if !seen_names.contains(req) {
            properties.push(TemporalProperty {
                name: req.clone(),
                kind: TemporalCheckKind::FiniteTemporalCheck,
                status: TemporalPropertyStatus::Unchecked,
                detail: Some("required property not found in temporal sources".into()),
            });
        }
    }

    if let Err(e) = build_res {
        let has_ce = properties
            .iter()
            .any(|pr| pr.status == TemporalPropertyStatus::Counterexample)
            || build_output.contains("Violation:")
            || build_output.contains("safety_failure")
            || build_output.contains("liveness_failure");
        let has_unknown = properties
            .iter()
            .any(|pr| pr.status == TemporalPropertyStatus::Unknown);
        let status = if has_ce {
            TemporalStatus::Counterexample
        } else if has_unknown {
            TemporalStatus::Unknown
        } else {
            TemporalStatus::Failed
        };
        return Ok(TemporalReport {
            enabled: true,
            backend: "veil".into(),
            status,
            inputs_current: true,
            properties,
            detail: Some(format!("{e:#}")),
        });
    }

    // Gate required Lean theorems on standard axioms only.
    if !req_theorems.is_empty() {
        let thm_results = verify_temporal_theorems(&tdir, &lean_files, targets, req_theorems)?;
        properties.extend(thm_results);
    }

    // Evaluate only active obligations (ignore explicitly non-required suppressed regression checks).
    let active: Vec<&TemporalProperty> = properties
        .iter()
        .filter(|pr| {
            !pr.detail
                .as_deref()
                .is_some_and(|d| d.starts_with("suppressed regression check"))
        })
        .collect();

    let passed_count = active
        .iter()
        .filter(|pr| pr.status == TemporalPropertyStatus::Passed)
        .count();
    let status = if active
        .iter()
        .any(|pr| pr.status == TemporalPropertyStatus::Counterexample)
    {
        TemporalStatus::Counterexample
    } else if active
        .iter()
        .any(|pr| pr.status == TemporalPropertyStatus::Unknown)
    {
        TemporalStatus::Unknown
    } else if active.is_empty()
        || active.iter().any(|pr| {
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
        "{passed_count}/{} verification obligations passed in {}",
        active.len(),
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
/* Strict Real Rust Counterexample & Lasso Replay                             */
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
/// with optional h5i replay metadata (`target`, `loop_start`, `seed_wraparound`,
/// `append_reboot`, `power_cut_budget`, `latched`).
#[derive(Clone, Debug, Deserialize)]
struct VeilTraceJson {
    #[serde(default)]
    target: Option<ReplayTarget>,
    /// Index into `states` (0-based) where the lasso cycle begins (`prefix = states[1..=loop_start]`,
    /// `cycle = states[loop_start+1..]`). Required to confirm a liveness counterexample.
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
    /// `true` ONLY when a genuine safety violation OR closed fair liveness lasso
    /// was reproduced on the real Rust implementation.
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
///
/// Strictness guarantees:
/// - Unrecognized transitions are rejected with an error (never silently ignored).
/// - Exact trace replay (`ReplayMode::ExactTrace`) never injects synthetic
///   `RebootLoad` actions or fabricated power-cut budgets.
/// - Liveness violations on `DeviceEngine` require an explicit lasso (`loop_start`)
///   whose cycle closes an identical concrete state and exercises `ActuatorPoll`;
///   finite traces without a closed fair lasso are reported as `PartialFiniteTrace`
///   with `reproduced_violation = false`.
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
                    "eraseOther" => {
                        // Internal step of `FlashStore::save` before write completes.
                    }
                    "writeComplete" | "writeLanded" => {
                        let v = pending_val.take().unwrap_or_else(|| {
                            next_val += 1;
                            next_val
                        });
                        actions.push(FlashReplayAction::Save(v));
                    }
                    "ackSave" => {
                        // Completion of `FlashStore::save` after `writeLanded`.
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
                    "powerCutClean" => {
                        // Idle or post-write power cut; next `rebootScan` reloads from flash.
                    }
                    "rebootScan" => {
                        actions.push(FlashReplayAction::RebootLoad);
                    }
                    _ => {
                        bail!(
                            "unsupported transition {:?} at step {} for target `flash-store`",
                            name,
                            step_idx + 1
                        );
                    }
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
                        "compute" | "beginSave" | "savePersist" | "notify" | "finish" | "monitorPoll" | "powerLoss" => {
                            // Internal phases / non-actuator observer steps of `DeviceEngine`.
                        }
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
                        _ => {
                            bail!(
                                "unsupported transition {:?} at step {} for target `device-engine`",
                                name,
                                offset + i + 1
                            );
                        }
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
                // `transitions[k]` is the transition leading into `states[k + 1]`.
                // Prefix ends at `states[loop_start]` => `transitions[..loop_start]`.
                // Cycle starts from `states[loop_start]` => `transitions[loop_start..]`.
                let mut pending = None;
                let prefix_actions = translate(&transitions[..loop_start], 0, &mut pending)?;
                let cycle_actions = translate(&transitions[loop_start..], loop_start, &mut pending)?;
                let out = if latched {
                    replay_real_device_engine_lasso::<true>(&prefix_actions, &cycle_actions)
                } else {
                    replay_real_device_engine_lasso::<false>(&prefix_actions, &cycle_actions)
                };
                let verdict = if out.liveness_lasso_confirmed {
                    ReplayVerdict::LivenessLassoReproduced
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
                    reproduced_violation: out.liveness_lasso_confirmed,
                    detail: format!(
                        "real h5i_app_embedded::kernel::DeviceEngine lasso (latched={latched}): emergency_committed={}, cycle_closed={}, actuator_poll_in_cycle={}, actuator_observed_stop={}, clear_refused={}, final_version={}",
                        out.emergency_committed,
                        out.cycle_closed,
                        out.actuator_poll_in_cycle,
                        out.actuator_observed_stop,
                        out.clear_refused_count,
                        out.final_version
                    ),
                });
            }

            // Finite trace without a lasso (`loop_start` absent): cannot confirm a liveness violation.
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
                    "real h5i_app_embedded::kernel::DeviceEngine finite trace (latched={latched}, no lasso `loop_start`): emergency_committed={}, actuator_observed_stop={}, clear_refused={}, final_version={} (finite trace alone does not confirm a liveness counterexample)",
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
    fn unrelated_lake_project_without_veil_is_rejected_not_passed() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join(FILE),
            "[verification]\ntemporal = true\ntemporal_backend = \"veil\"\n",
        )
        .unwrap();
        let tdir = d.path().join("proofs/temporal");
        std::fs::create_dir_all(&tdir).unwrap();
        std::fs::write(tdir.join("lakefile.lean"), "import Lake\nopen Lake DSL\npackage decoy\n")
            .unwrap();
        std::fs::write(tdir.join("Decoy.lean"), "theorem trivial_ok : True := trivial\n").unwrap();
        assert!(!lakefile_uses_veil(&tdir).unwrap());
    }

    #[test]
    fn parses_veil_checks_and_detects_suppressed_required_property() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("Model.lean"),
            "veil module Demo\n#model_check interpreted {} {}\nset_option veil.violationIsError false in\n#temporal_check [bad_prop] interpreted {} {}\n#temporal_check [good_prop] interpreted {} {}\nend Demo\n",
        )
        .unwrap();
        let (_files, checks) = discover_veil_checks(d.path()).unwrap();
        assert_eq!(checks.len(), 3);
        assert_eq!(checks[0].name, "Demo#model_check");
        assert!(!checks[0].suppressed);
        assert_eq!(checks[1].name, "bad_prop");
        assert!(checks[1].suppressed);
        assert_eq!(checks[2].name, "good_prop");
        assert!(!checks[2].suppressed);

        let parsed = parse_veil_build_output(
            "info: Model.lean:2:0: ✅ No violation (explored 12 states)\n\
             info: Model.lean:4:0: ❌ Violation: liveness_failure\n\
             info: Model.lean:5:0: ✅ No liveness violation (checked 1 properties, explored 24 product states)\n",
        );
        assert_eq!(parsed[&("Model.lean".into(), 2)].0, TemporalPropertyStatus::Passed);
        assert_eq!(parsed[&("Model.lean".into(), 4)].0, TemporalPropertyStatus::Counterexample);
        assert_eq!(parsed[&("Model.lean".into(), 5)].0, TemporalPropertyStatus::Passed);
    }

    #[test]
    fn replays_veil_flash_store_wraparound_json_trace_with_explicit_reboot() {
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
        assert_eq!(rep.verdict, ReplayVerdict::SafetyViolationReproduced);
        assert!(rep.reproduced_violation);
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
    fn finite_device_engine_trace_without_lasso_is_partial_not_confirmed_liveness_violation() {
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
        assert!(
            !rep.reproduced_violation,
            "finite trace ending before actuatorPoll MUST NOT be claimed as a confirmed liveness violation: {rep:?}"
        );
    }

    #[test]
    fn replays_veil_device_engine_coalescing_lasso_and_confirms_liveness_violation() {
        let d = tempfile::tempdir().unwrap();
        let trace_path = d.path().join("veil-de-lasso.json");
        // Explicit lasso (`loop_start: 0`): cycle = emergency -> clear -> actuatorPoll repeats forever.
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
        assert_eq!(rep.verdict, ReplayVerdict::LivenessLassoReproduced);
        assert!(
            rep.reproduced_violation,
            "closed fair coalescing lasso MUST confirm liveness violation on real DeviceEngine: {rep:?}"
        );
    }
}
