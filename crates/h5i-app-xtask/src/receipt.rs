//! The run's receipt: what `cargo app-verify` checked, written to
//! `.h5i/app-verify/latest.json`. `h5i ui` reads it; nothing here grades it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{AuthzRow, Outcome, PROJECTS};

pub const VERSION: u32 = 1;
pub const DIR: &str = ".h5i/app-verify";
pub const FILE: &str = "latest.json";

#[derive(Serialize, Clone)]
pub struct StepRec {
    pub name: String,
    pub project: Option<String>,
    pub outcome: &'static str,
    pub detail: String,
    pub secs: f32,
}

impl StepRec {
    pub fn new(name: &str, project: Option<&str>, out: &Outcome, secs: f32) -> Self {
        let (outcome, detail) = match out {
            Outcome::Pass => ("pass", String::new()),
            Outcome::Fail(log) => ("fail", log.clone()),
            Outcome::Skip(why) => ("skip", why.clone()),
        };
        StepRec { name: name.to_string(), project: project.map(str::to_string), outcome, detail, secs }
    }
}

#[derive(Serialize)]
pub struct Git {
    pub head: String,
    pub branch: String,
    pub dirty: bool,
}

#[derive(Serialize)]
pub struct ProjectRec {
    pub dir: String,
    pub app: String,
    pub kernel: Option<String>,
    pub generated: Option<String>,
    /// SHA-256 over the kernel's `.rs` sources, so a reader can tell whether
    /// the tree it looks at is the one this receipt is about.
    pub kernel_digest: Option<String>,
    /// SHA-256 over the hand-written `.lean` files.
    pub proofs_digest: String,
    pub lean: &'static str,
    pub extraction: &'static str,
    pub authorization: Option<&'static str>,
}

#[derive(Serialize)]
pub struct AxiomRec {
    pub dir: String,
    pub theorem: String,
    pub axioms: Vec<String>,
    pub verdict: String,
}

#[derive(Serialize)]
pub struct Difftest {
    pub cases: u64,
    pub outcomes: String,
}

#[derive(Serialize)]
pub struct Summary {
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
}

#[derive(Serialize)]
pub struct Receipt {
    pub version: u32,
    pub started: String,
    pub finished: String,
    pub secs: f64,
    pub git: Git,
    pub full: bool,
    pub extract: bool,
    pub summary: Summary,
    pub steps: Vec<StepRec>,
    pub projects: Vec<ProjectRec>,
    pub axioms: Vec<AxiomRec>,
    pub mutants: Vec<serde_json::Value>,
    pub difftest: Option<Difftest>,
}

impl Receipt {
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        root: &Path,
        started: SystemTime,
        full: bool,
        extract: bool,
        steps: &[StepRec],
        authz: Vec<AuthzRow>,
        axioms_log: &Path,
        mutants: Vec<serde_json::Value>,
        difftest: Option<Difftest>,
    ) -> Self {
        let finished = SystemTime::now();
        let failed = steps.iter().filter(|s| s.outcome == "fail").count();
        let skipped = steps.iter().filter(|s| s.outcome == "skip").count();
        let projects = PROJECTS.iter().map(|(dir, _, generated)| project_rec(root, dir, *generated, steps, &authz)).collect();
        Receipt {
            version: VERSION,
            started: iso8601(started),
            finished: iso8601(finished),
            secs: finished.duration_since(started).map(|d| d.as_secs_f64()).unwrap_or(0.0),
            git: git_state(root),
            full,
            extract,
            summary: Summary { passed: steps.len() - failed - skipped, failed, skipped },
            steps: steps.to_vec(),
            projects,
            axioms: read_axioms(axioms_log),
            mutants,
            difftest,
        }
    }

    pub fn write(&self, root: &Path) -> std::io::Result<PathBuf> {
        let dir = scratch_dir(root);
        let path = dir.join(FILE);
        let tmp = dir.join(format!("{FILE}.tmp"));
        std::fs::write(&tmp, serde_json::to_string_pretty(self).expect("receipt serializes"))?;
        std::fs::rename(&tmp, &path)?;
        Ok(path)
    }
}

/// `.h5i/app-verify/`, created; also where the scripts' JSON lands.
pub fn scratch_dir(root: &Path) -> PathBuf {
    let dir = root.join(DIR);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn project_rec(root: &Path, dir: &str, generated: Option<&str>, steps: &[StepRec], authz: &[AuthzRow]) -> ProjectRec {
    let proofs = root.join(dir);
    let parent = proofs.parent().unwrap_or(&proofs);
    let app = parent.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let kernel = ["kernel", "src"].iter().map(|k| parent.join(k)).find(|p| p.is_dir());
    let of = |prefix: &str| steps.iter().find(|s| s.project.as_deref() == Some(dir) && s.name.starts_with(prefix));
    let lean = match of("lean ").map(|s| s.outcome) {
        Some("pass") => "pass",
        Some("fail") => "fail",
        Some(_) => "skip",
        None => "none",
    };
    let extraction = match of("extract ") {
        Some(s) if s.outcome == "pass" => "match",
        Some(s) if s.outcome == "fail" && s.detail.contains("diff --git") => "drift",
        Some(s) if s.outcome == "fail" => "fail",
        Some(_) => "skip",
        None => "none",
    };
    ProjectRec {
        dir: dir.to_string(),
        app,
        kernel: kernel.as_ref().map(|k| rel(root, k)),
        generated: generated.map(str::to_string),
        kernel_digest: kernel.as_ref().map(|k| digest_tree(k, "rs")),
        proofs_digest: digest_tree(&proofs, "lean"),
        lean,
        extraction,
        authorization: authz.iter().find(|a| a.dir == dir).map(|a| a.status),
    }
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).to_string_lossy().to_string()
}

/// SHA-256 over every `*.ext` file under `dir` (path and bytes), skipping
/// `target`, `.lake` and `generated`. Order is by relative path.
pub fn digest_tree(dir: &Path, ext: &str) -> String {
    let mut files = Vec::new();
    collect(dir, ext, &mut files);
    files.sort();
    let mut h = Sha256::new();
    for f in files {
        let relp = f.strip_prefix(dir).unwrap_or(&f).to_string_lossy().replace('\\', "/");
        h.update(relp.as_bytes());
        h.update([0]);
        h.update(std::fs::read(&f).unwrap_or_default());
        h.update([0]);
    }
    hex(&h.finalize())
}

fn collect(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if !p.file_name().is_some_and(|n| n == "target" || n == ".lake" || n == "generated") {
                collect(&p, ext, out);
            }
        } else if p.extension().is_some_and(|x| x == ext) {
            out.push(p);
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let o = Command::new("git").args(args).current_dir(root).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn git_state(root: &Path) -> Git {
    Git {
        head: git(root, &["rev-parse", "HEAD"]).unwrap_or_default(),
        branch: git(root, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default(),
        dirty: git(root, &["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty()),
    }
}

/// `ci-axioms.sh` appends `dir<TAB>theorem<TAB>axioms<TAB>verdict` lines.
pub fn read_axioms(log: &Path) -> Vec<AxiomRec> {
    std::fs::read_to_string(log).map(|t| parse_axioms(&t)).unwrap_or_default()
}

pub fn parse_axioms(text: &str) -> Vec<AxiomRec> {
    text.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let dir = f.next()?.to_string();
            let theorem = f.next()?.to_string();
            let axioms = f.next()?.split(',').filter(|a| !a.is_empty()).map(str::to_string).collect();
            let verdict = f.next()?.to_string();
            Some(AxiomRec { dir, theorem, axioms, verdict })
        })
        .collect()
}

/// Mutant records from the scripts' `--json` files that exist.
pub fn read_mutants(paths: &[&Path]) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for p in paths {
        if let Ok(text) = std::fs::read_to_string(p)
            && let Ok(serde_json::Value::Array(rows)) = serde_json::from_str(&text)
        {
            out.extend(rows);
        }
    }
    out
}

/// The difftest's success line: `N cases agree; outcome mix: {...}`.
pub fn parse_difftest(text: &str) -> Option<Difftest> {
    let line = text.lines().find(|l| l.contains(" cases agree; outcome mix: "))?;
    let (n, mix) = line.trim().split_once(" cases agree; outcome mix: ")?;
    Some(Difftest { cases: n.trim().parse().ok()?, outcomes: mix.trim().to_string() })
}

/// RFC 3339 UTC, to the second.
pub fn iso8601(t: SystemTime) -> String {
    let secs = t.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn iso8601_known_instants() {
        assert_eq!(iso8601(SystemTime::UNIX_EPOCH), "1970-01-01T00:00:00Z");
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000);
        assert_eq!(iso8601(t), "2026-09-21T14:13:20Z");
        let leap = SystemTime::UNIX_EPOCH + Duration::from_secs(951_782_400);
        assert_eq!(iso8601(leap), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn difftest_line_parses() {
        let out = "running 1 test\n5000 cases agree; outcome mix: {\"ERR 3\": 12, \"OK\": 4988}\ntest ok\n";
        let d = parse_difftest(out).unwrap();
        assert_eq!(d.cases, 5000);
        assert!(d.outcomes.starts_with('{'));
        assert!(parse_difftest("case 7 differs").is_none());
    }

    #[test]
    fn axioms_log_parses() {
        let text = "examples/app/docs/proofs\tdocs_kernel.Theorems.authorized\tpropext,Classical.choice,Quot.sound\tok\n\
                    examples/app/docs/proofs\tdocs_kernel.Theorems.gone\t\tmissing\n";
        let rows = parse_axioms(text);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].axioms, ["propext", "Classical.choice", "Quot.sound"]);
        assert!(rows[1].axioms.is_empty());
        assert_eq!(rows[1].verdict, "missing");
    }

    #[test]
    fn digest_is_stable_and_skips_generated() {
        let dir = std::env::temp_dir().join(format!("h5i-digest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("generated")).unwrap();
        std::fs::write(dir.join("A.lean"), "theorem a : True := trivial").unwrap();
        std::fs::write(dir.join("generated/K.lean"), "extracted").unwrap();
        let first = digest_tree(&dir, "lean");
        std::fs::write(dir.join("generated/K.lean"), "changed").unwrap();
        assert_eq!(first, digest_tree(&dir, "lean"));
        std::fs::write(dir.join("A.lean"), "theorem a : True := by trivial").unwrap();
        assert_ne!(first, digest_tree(&dir, "lean"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
