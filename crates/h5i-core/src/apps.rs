//! The repository's proof projects: what the Lean proofs state, what each
//! app says it leaves to the shell (`proofs/scope.toml`), and what the last
//! `cargo app-verify` recorded (`.h5i/app-verify/latest.json`).
//!
//! Everything here reads files and counts. It never grades: a receipt is
//! shown with its date and its digests, and the reader decides whether the
//! tree in front of them is the one it describes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const RECEIPT: &str = ".h5i/app-verify/latest.json";
pub const SCOPE: &str = "scope.toml";
const LEDGER: &str = "docs/app/ROADMAP.md";
const CODE_LINES: usize = 60;
const BUILDS_CAP: usize = 12;

// ── scope.toml ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Scope {
    #[serde(default)]
    pub app: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<String>,
    #[serde(default)]
    pub assumes: Vec<String>,
    #[serde(default)]
    pub trusted_input: Vec<TrustedInput>,
    #[serde(default)]
    pub out_of_scope: Vec<OutOfScope>,
    #[serde(default)]
    pub counterexample: Vec<Counterexample>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrustedInput {
    pub name: String,
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OutOfScope {
    pub item: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub r#ref: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Counterexample {
    pub theorem: String,
    #[serde(default)]
    pub bug: String,
    #[serde(default)]
    pub before: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub current: bool,
}

// ── the receipt, as written by crates/h5i-app-xtask ──────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Receipt {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub started: String,
    #[serde(default)]
    pub finished: String,
    #[serde(default)]
    pub git: ReceiptGit,
    #[serde(default)]
    pub full: bool,
    #[serde(default)]
    pub extract: bool,
    #[serde(default)]
    pub summary: ReceiptSummary,
    #[serde(default)]
    pub steps: Vec<Step>,
    #[serde(default)]
    pub projects: Vec<ReceiptProject>,
    #[serde(default)]
    pub axioms: Vec<AxiomRec>,
    #[serde(default)]
    pub mutants: Vec<Mutant>,
    #[serde(default)]
    pub difftest: Option<Difftest>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReceiptGit {
    #[serde(default)]
    pub head: String,
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub dirty: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReceiptSummary {
    #[serde(default)]
    pub passed: usize,
    #[serde(default)]
    pub failed: usize,
    #[serde(default)]
    pub skipped: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Step {
    pub name: String,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub outcome: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub secs: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReceiptProject {
    pub dir: String,
    #[serde(default)]
    pub app: String,
    #[serde(default)]
    pub kernel: Option<String>,
    #[serde(default)]
    pub generated: Option<String>,
    #[serde(default)]
    pub kernel_digest: Option<String>,
    #[serde(default)]
    pub proofs_digest: String,
    #[serde(default)]
    pub lean: String,
    #[serde(default)]
    pub extraction: String,
    #[serde(default)]
    pub authorization: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AxiomRec {
    pub dir: String,
    pub theorem: String,
    #[serde(default)]
    pub axioms: Vec<String>,
    #[serde(default)]
    pub verdict: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Mutant {
    #[serde(default)]
    pub suite: String,
    pub name: String,
    #[serde(default)]
    pub app: Option<String>,
    #[serde(default)]
    pub bug: String,
    #[serde(default)]
    pub expect: Vec<String>,
    #[serde(default)]
    pub verdict: String,
    #[serde(default)]
    pub stage: String,
    #[serde(default)]
    pub failed: Vec<Failure>,
    #[serde(default)]
    pub secs: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Failure {
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub line: usize,
    #[serde(default)]
    pub col: usize,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub decl: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Difftest {
    #[serde(default)]
    pub cases: u64,
    #[serde(default)]
    pub outcomes: String,
}

pub fn read_receipt(repo: &Path) -> Option<Receipt> {
    let text = std::fs::read_to_string(repo.join(RECEIPT)).ok()?;
    serde_json::from_str(&text).ok()
}

// ── what the console shows ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct MutantCounts {
    pub prepared: usize,
    pub caught: usize,
    pub survived: usize,
    pub invalid: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct AxiomCounts {
    pub ok: usize,
    pub bad: usize,
    pub missing: usize,
}

/// The receipt's say about one project, next to whether the tree still
/// matches it.
#[derive(Debug, Clone, Serialize)]
pub struct ReceiptView {
    pub started: String,
    pub head: String,
    pub branch: String,
    pub dirty: bool,
    pub full: bool,
    pub lean: String,
    pub extraction: String,
    pub authorization: Option<String>,
    /// `None` when the project has no kernel.
    pub kernel_match: Option<bool>,
    pub proofs_match: bool,
    pub axioms: AxiomCounts,
    pub mutants: MutantCounts,
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    /// The proofs directory, relative to the repository.
    pub dir: String,
    pub app: String,
    pub title: String,
    /// `server`, `kernel` or `library`.
    pub kind: String,
    pub has_scope: bool,
    pub theorems: usize,
    pub counterexamples: usize,
    pub receipt: Option<ReceiptView>,
    /// What a reader should look at first. Empty means the receipt is
    /// current and reported nothing failing; it does not mean "verified".
    pub flags: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Theorem {
    pub name: String,
    pub qualified: Option<String>,
    pub file: Option<String>,
    pub line: Option<usize>,
    pub doc: String,
    /// The README's plain statement, when the table has one.
    pub statement: String,
    /// States `¬ ...`: refutes a property of some code, by construction.
    pub counterexample: bool,
    pub bug: Option<Counterexample>,
    pub axioms: Option<AxiomRec>,
    pub expected_by: Vec<String>,
    pub caught: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Field {
    pub name: String,
    pub doc: String,
    pub prop: String,
    /// Mutants whose failing proof text names this clause.
    pub mutants: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Matrix {
    pub rows: Vec<String>,
    pub cols: Vec<String>,
    pub cells: Vec<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpecItem {
    pub kind: String,
    pub name: String,
    pub line: usize,
    pub doc: String,
    pub code: String,
    pub truncated: bool,
    pub fields: Vec<Field>,
    pub matrix: Option<Matrix>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpecSection {
    pub heading: String,
    pub items: Vec<SpecItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Spec {
    pub file: String,
    pub sections: Vec<SpecSection>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Assume {
    pub id: String,
    pub text: String,
    pub today: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Edge {
    /// `no-actor`, `privileged-field`, `assume_authenticated`.
    pub kind: String,
    pub file: String,
    pub line: usize,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BoxBuild {
    pub env_id: String,
    pub at: String,
    pub cmd: String,
    pub git_tree: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Version {
    pub receipt_head: Option<String>,
    pub receipt_dirty: Option<bool>,
    pub current_head: Option<String>,
    pub kernel_match: Option<bool>,
    pub proofs_match: Option<bool>,
    pub kernel_digest: Option<String>,
    pub proofs_digest: String,
    pub builds: Vec<BoxBuild>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Trust {
    pub assumes: Vec<Assume>,
    pub trusted_inputs: Vec<TrustedInput>,
    pub out_of_scope: Vec<OutOfScope>,
    pub edges: Vec<Edge>,
    pub version: Version,
}

#[derive(Debug, Clone, Serialize)]
pub struct MutantView {
    pub name: String,
    pub bug: String,
    pub expect: Vec<String>,
    pub verdict: String,
    pub stage: String,
    pub failed: Vec<String>,
    /// `Some(true)` when an expected theorem failed, `Some(false)` when the
    /// mutant was caught elsewhere, `None` without a prediction.
    pub as_expected: Option<bool>,
    pub secs: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Evidence {
    pub steps: Vec<Step>,
    pub mutants: Vec<MutantView>,
    pub counts: MutantCounts,
    pub difftest: Option<Difftest>,
    pub summary: Option<ReceiptSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Detail {
    pub summary: Summary,
    pub scope: Option<Scope>,
    pub readme: Option<String>,
    pub spec: Option<Spec>,
    pub theorems: Vec<Theorem>,
    pub trust: Trust,
    pub evidence: Evidence,
}

// ── discovery ────────────────────────────────────────────────────────────────

/// Proof projects under `repo`: directories holding a `lakefile.lean` and
/// hand-written `.lean` files, outside build output.
pub fn discover(repo: &Path) -> Vec<Summary> {
    let mut dirs = Vec::new();
    walk(repo, 0, &mut dirs);
    dirs.sort();
    let receipt = read_receipt(repo);
    let mut out: Vec<Summary> = dirs.iter().filter_map(|d| summary(repo, d, receipt.as_ref())).collect();
    // Apps before the libraries they share.
    let rank = |k: &str| match k {
        "server" => 0,
        "kernel" => 1,
        _ => 2,
    };
    out.sort_by(|a, b| rank(&a.kind).cmp(&rank(&b.kind)).then_with(|| a.app.cmp(&b.app)));
    out
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 5 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut subdirs = Vec::new();
    let mut lake = false;
    let mut lean = false;
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if p.is_dir() {
            if !matches!(name.as_str(), ".git" | ".lake" | "target" | "node_modules" | "generated" | ".h5i") {
                subdirs.push(p);
            }
        } else if name == "lakefile.lean" {
            lake = true;
        } else if name.ends_with(".lean") {
            lean = true;
        }
    }
    if lake && lean {
        out.push(dir.to_path_buf());
        return;
    }
    for s in subdirs {
        walk(&s, depth + 1, out);
    }
}

fn rel(repo: &Path, p: &Path) -> String {
    p.strip_prefix(repo).unwrap_or(p).to_string_lossy().replace('\\', "/")
}

fn read_scope(proofs: &Path) -> Option<Scope> {
    let text = std::fs::read_to_string(proofs.join(SCOPE)).ok()?;
    toml::from_str(&text).ok()
}

/// `README.md` beside the proofs, else the app's README or tutorial.
fn readme_path(proofs: &Path) -> Option<PathBuf> {
    let parent = proofs.parent()?;
    [proofs.join("README.md"), parent.join("README.md"), parent.join("TUTORIAL.md")].into_iter().find(|p| p.is_file())
}

fn kernel_dir(proofs: &Path, scope: Option<&Scope>) -> Option<PathBuf> {
    if let Some(k) = scope.and_then(|s| s.kernel.as_deref()) {
        let p = proofs.join(k);
        return p.is_dir().then_some(p);
    }
    let parent = proofs.parent()?;
    [parent.join("kernel"), parent.join("src")].into_iter().find(|p| p.is_dir())
}

fn server_dir(proofs: &Path, scope: Option<&Scope>) -> Option<PathBuf> {
    if let Some(s) = scope.and_then(|s| s.server.as_deref()) {
        let p = proofs.join(s);
        return p.is_dir().then_some(p);
    }
    let p = proofs.parent()?.join("server");
    p.is_dir().then_some(p)
}

fn summary(repo: &Path, proofs: &Path, receipt: Option<&Receipt>) -> Option<Summary> {
    let dir = rel(repo, proofs);
    let scope = read_scope(proofs);
    let parent_name = proofs.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let app = scope.as_ref().filter(|s| !s.app.is_empty()).map(|s| s.app.clone()).unwrap_or(parent_name);
    let kernel = kernel_dir(proofs, scope.as_ref());
    let server = server_dir(proofs, scope.as_ref());
    let kind = if server.is_some() {
        "server"
    } else if kernel.as_ref().is_some_and(|k| k.file_name().is_some_and(|n| n == "kernel")) {
        "kernel"
    } else {
        "library"
    };
    let table = readme_path(proofs).and_then(|p| std::fs::read_to_string(p).ok()).map(|t| theorem_table(&t)).unwrap_or_default();
    let lean_files = lean_sources(proofs);
    let counterexamples = table.iter().filter(|(n, _)| is_counterexample(&lean_files, n)).count()
        + scope.as_ref().map_or(0, |s| s.counterexample.iter().filter(|c| !table.iter().any(|(n, _)| n == &c.theorem)).count());
    let mut flags = Vec::new();
    let view = receipt.and_then(|r| receipt_view(proofs, &dir, kernel.as_deref(), r, &mut flags));
    if view.is_none() {
        flags.push("no receipt".into());
    }
    if scope.is_none() && kind != "library" {
        flags.push("no scope.toml".into());
    }
    Some(Summary {
        dir,
        app,
        title: scope.as_ref().map(|s| s.title.clone()).unwrap_or_default(),
        kind: kind.into(),
        has_scope: scope.is_some(),
        theorems: table.len(),
        counterexamples,
        receipt: view,
        flags,
    })
}

fn receipt_view(
    proofs: &Path,
    dir: &str,
    kernel: Option<&Path>,
    r: &Receipt,
    flags: &mut Vec<String>,
) -> Option<ReceiptView> {
    let p = r.projects.iter().find(|p| p.dir == dir)?;
    let kernel_now = kernel.map(|k| digest_tree(k, "rs"));
    let kernel_match = match (&p.kernel_digest, &kernel_now) {
        (Some(a), Some(b)) => Some(a == b),
        _ => None,
    };
    let proofs_match = p.proofs_digest == digest_tree(proofs, "lean");
    let axioms = r.axioms.iter().filter(|a| a.dir == dir).fold(AxiomCounts { ok: 0, bad: 0, missing: 0 }, |mut c, a| {
        match a.verdict.as_str() {
            "ok" => c.ok += 1,
            "bad" => c.bad += 1,
            _ => c.missing += 1,
        }
        c
    });
    let mutants = mutant_counts(mutants_of(r, dir));
    if p.lean == "fail" {
        flags.push("lean build failed".into());
    }
    if p.extraction == "drift" {
        flags.push("extraction drift".into());
    } else if p.extraction == "fail" {
        flags.push("extraction failed".into());
    }
    if axioms.bad > 0 || axioms.missing > 0 {
        flags.push("axiom gate".into());
    }
    if mutants.survived > 0 {
        flags.push("mutant survived".into());
    }
    if kernel_match == Some(false) {
        flags.push("kernel changed since".into());
    }
    if !proofs_match {
        flags.push("proofs changed since".into());
    }
    if r.git.dirty {
        flags.push("receipt from a dirty tree".into());
    }
    if p.lean == "skip" {
        flags.push("lean skipped".into());
    }
    Some(ReceiptView {
        started: r.started.clone(),
        head: r.git.head.clone(),
        branch: r.git.branch.clone(),
        dirty: r.git.dirty,
        full: r.full,
        lean: p.lean.clone(),
        extraction: p.extraction.clone(),
        authorization: p.authorization.clone(),
        kernel_match,
        proofs_match,
        axioms,
        mutants,
    })
}

fn mutants_of<'a>(r: &'a Receipt, dir: &str) -> impl Iterator<Item = &'a Mutant> + 'a {
    let dir = dir.to_string();
    r.mutants.iter().filter(move |m| match m.suite.as_str() {
        "docs" => dir == "examples/app/docs/proofs",
        _ => m.app.as_deref().is_some_and(|a| format!("{a}/proofs") == dir),
    })
}

fn mutant_counts<'a>(ms: impl Iterator<Item = &'a Mutant>) -> MutantCounts {
    let mut c = MutantCounts { prepared: 0, caught: 0, survived: 0, invalid: 0 };
    for m in ms {
        if m.name == "baseline" {
            continue;
        }
        c.prepared += 1;
        match m.verdict.as_str() {
            "caught" => c.caught += 1,
            "survived" => c.survived += 1,
            _ => c.invalid += 1,
        }
    }
    c
}

// ── digests (the same arithmetic as crates/h5i-app-xtask/src/receipt.rs) ─────

/// SHA-256 over every `*.ext` file under `dir`, path then bytes, skipping
/// `target`, `.lake` and `generated`.
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
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
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

// ── Lean sources ─────────────────────────────────────────────────────────────

struct LeanFile {
    name: String,
    text: String,
}

/// Hand-written `.lean` files directly in the proofs directory.
fn lean_sources(proofs: &Path) -> Vec<LeanFile> {
    let Ok(entries) = std::fs::read_dir(proofs) else { return Vec::new() };
    let mut out: Vec<LeanFile> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "lean") && !p.ends_with("lakefile.lean"))
        .filter_map(|p| {
            let name = p.file_name()?.to_string_lossy().to_string();
            Some(LeanFile { name, text: std::fs::read_to_string(&p).ok()? })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '.' | '\'' | '!' | '?')
}

/// `(kind, name)` when `line` opens a declaration.
fn decl_head(line: &str) -> Option<(&'static str, String)> {
    if line.starts_with(char::is_whitespace) {
        return None;
    }
    let mut rest = line;
    while rest.starts_with("@[") {
        let end = rest.find(']')?;
        rest = rest[end + 1..].trim_start();
    }
    for m in ["private ", "protected ", "noncomputable ", "nonrec ", "partial "] {
        if let Some(r) = rest.strip_prefix(m) {
            rest = r.trim_start();
        }
    }
    const KINDS: [&str; 9] = ["theorem", "lemma", "def", "abbrev", "instance", "example", "structure", "inductive", "opaque"];
    for k in KINDS {
        if let Some(r) = rest.strip_prefix(k)
            && r.starts_with(char::is_whitespace)
        {
            let name: String = r.trim_start().chars().take_while(|c| is_ident(*c)).collect();
            if !name.is_empty() {
                return Some((k, name));
            }
        }
    }
    None
}

/// Where `theorem NAME` is declared: file, 1-based line, docstring and the
/// namespace-qualified name.
fn find_theorem(files: &[LeanFile], name: &str) -> Option<(String, usize, String, String, bool)> {
    let short = name.rsplit('.').next().unwrap_or(name);
    for f in files {
        let lines: Vec<&str> = f.text.lines().collect();
        let mut stack: Vec<String> = Vec::new();
        let mut doc_lines: Vec<String> = Vec::new();
        let mut in_doc = false;
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim();
            if in_doc {
                let (body, done) = match t.find("-/") {
                    Some(j) => (&t[..j], true),
                    None => (t, false),
                };
                doc_lines.push(body.trim().to_string());
                in_doc = !done;
                continue;
            }
            if let Some(r) = t.strip_prefix("/--") {
                match r.find("-/") {
                    Some(j) => doc_lines = vec![r[..j].trim().to_string()],
                    None => {
                        doc_lines = vec![r.trim().to_string()];
                        in_doc = true;
                    }
                }
                continue;
            }
            if let Some(ns) = t.strip_prefix("namespace ") {
                stack.push(ns.trim().to_string());
                doc_lines.clear();
                continue;
            }
            if t == "end" || t.starts_with("end ") {
                let which = t.strip_prefix("end").unwrap_or("").trim();
                if !stack.is_empty() && (which.is_empty() || stack.last().is_some_and(|s| s == which)) {
                    stack.pop();
                }
                doc_lines.clear();
                continue;
            }
            if let Some((kind, found)) = decl_head(line) {
                if (kind == "theorem" || kind == "lemma" || kind == "def") && (found == name || found == short) {
                    let qualified = if stack.is_empty() { found.clone() } else { format!("{}.{found}", stack.join(".")) };
                    let statement = statement_text(&lines, i);
                    let neg = statement.trim_start().starts_with('¬');
                    let doc = doc_lines.join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
                    return Some((f.name.clone(), i + 1, doc, qualified, neg));
                }
                doc_lines.clear();
            } else if t.is_empty() {
                doc_lines.clear();
            }
        }
    }
    None
}

/// The text after the last top-level `:` of a signature, up to `:=`.
fn statement_text(lines: &[&str], start: usize) -> String {
    let mut sig = String::new();
    for l in lines.iter().skip(start).take(12) {
        sig.push_str(l.trim());
        sig.push(' ');
        if l.contains(":=") || l.trim_end().ends_with(" by") {
            break;
        }
    }
    let sig = sig.split(":=").next().unwrap_or("").to_string();
    // Drop binders: `theorem x (a : T) {b : U} : P`.
    let mut depth = 0i32;
    let mut last_colon = None;
    for (i, c) in sig.char_indices() {
        match c {
            '(' | '{' | '[' | '⦃' => depth += 1,
            ')' | '}' | ']' | '⦄' => depth -= 1,
            ':' if depth == 0 => last_colon = Some(i),
            _ => {}
        }
    }
    match last_colon {
        Some(i) => sig[i + 1..].trim().to_string(),
        None => String::new(),
    }
}

fn is_counterexample(files: &[LeanFile], name: &str) -> bool {
    find_theorem(files, name).is_some_and(|t| t.4)
}

// ── the README theorem table ─────────────────────────────────────────────────

/// `(name, statement)` rows of the `## Theorems` table; a cell naming several
/// theorems gives one row each.
pub fn theorem_table(markdown: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut in_section = false;
    for line in markdown.lines() {
        let t = line.trim();
        if let Some(h) = t.strip_prefix("## ") {
            in_section = h.trim().eq_ignore_ascii_case("theorems");
            continue;
        }
        if !in_section || !t.starts_with('|') {
            continue;
        }
        let cells: Vec<String> = split_row(t);
        if cells.len() < 2 || cells[0].starts_with("---") || cells[0].eq_ignore_ascii_case("theorem") {
            continue;
        }
        let statement = cells[1].clone();
        for name in cells[0].split('`').skip(1).step_by(2) {
            let name = name.trim();
            if !name.is_empty() && name != "..." {
                out.push((name.to_string(), statement.clone()));
            }
        }
    }
    out
}

fn split_row(row: &str) -> Vec<String> {
    let inner = row.trim().trim_start_matches('|').trim_end_matches('|');
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek() == Some(&'|') {
            cur.push('|');
            chars.next();
        } else if c == '|' {
            cells.push(cur.trim().to_string());
            cur.clear();
        } else {
            cur.push(c);
        }
    }
    cells.push(cur.trim().to_string());
    cells
}

// ── Spec.lean ────────────────────────────────────────────────────────────────

/// Enum constructors from the extracted Lean: `inductive Role where` then
/// `| Owner : Role` lines.
fn enums(proofs: &Path) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(proofs.join("generated")) else { return out };
    for e in entries.flatten() {
        let Ok(text) = std::fs::read_to_string(e.path()) else { continue };
        let mut current: Option<(String, Vec<String>)> = None;
        for line in text.lines() {
            if let Some(r) = line.strip_prefix("inductive ")
                && let Some(name) = r.split_whitespace().next()
            {
                if let Some((n, cs)) = current.take() {
                    out.insert(n, cs);
                }
                current = Some((name.rsplit('.').next().unwrap_or(name).to_string(), Vec::new()));
            } else if let Some((_, cs)) = current.as_mut()
                && let Some(r) = line.strip_prefix("| ")
            {
                let ctor = r.split(':').next().unwrap_or("").trim();
                let plain = r.trim();
                // A constructor with arguments is not an enum case.
                if !ctor.is_empty() && !ctor.contains(' ') && !ctor.contains('(') {
                    cs.push(ctor.to_string());
                } else if !plain.is_empty() {
                    current = None;
                }
            } else if !line.starts_with(' ')
                && !line.is_empty()
                && let Some((n, cs)) = current.take()
            {
                out.insert(n, cs);
            }
        }
        if let Some((n, cs)) = current.take() {
            out.insert(n, cs);
        }
    }
    out.retain(|_, cs| !cs.is_empty());
    out
}

/// A two-argument `Bool` table over enums, evaluated arm by arm.
fn matrix_of(code: &str, enums: &BTreeMap<String, Vec<String>>) -> Option<Matrix> {
    let head = code.lines().next()?;
    let sig = head.split_once(':')?.1;
    let sig = sig.split(":=").next()?;
    let parts: Vec<&str> = sig.split('→').map(str::trim).collect();
    if parts.len() != 3 || parts[2] != "Bool" {
        return None;
    }
    let rows = enums.get(parts[0])?;
    let cols = enums.get(parts[1])?;
    let arms: Vec<(String, String, String)> = code
        .lines()
        .skip(1)
        .filter_map(|l| {
            let l = l.trim().strip_prefix('|')?;
            let (pat, val) = l.split_once("=>")?;
            let mut pats = pat.split(',').map(|p| p.trim().trim_start_matches('.').to_string());
            Some((pats.next()?, pats.next()?, val.trim().to_string()))
        })
        .collect();
    if arms.is_empty() {
        return None;
    }
    let cells = rows
        .iter()
        .map(|r| {
            cols.iter()
                .map(|c| {
                    arms.iter()
                        .find(|(pr, pc, _)| (pr == "_" || pr == r) && (pc == "_" || pc == c))
                        .map(|(_, _, v)| v.clone())
                        .unwrap_or_default()
                })
                .collect()
        })
        .collect();
    Some(Matrix { rows: rows.clone(), cols: cols.clone(), cells })
}

/// Fields of a `structure ... : Prop where`, with their docstrings.
fn fields_of(code: &str) -> Vec<Field> {
    let mut out = Vec::new();
    let mut doc: Vec<String> = Vec::new();
    let mut in_doc = false;
    let mut cur: Option<Field> = None;
    for line in code.lines().skip(1) {
        let t = line.trim();
        if in_doc {
            let (body, done) = match t.find("-/") {
                Some(j) => (&t[..j], true),
                None => (t, false),
            };
            doc.push(body.trim().to_string());
            in_doc = !done;
            continue;
        }
        if let Some(r) = t.strip_prefix("/--") {
            if let Some(f) = cur.take() {
                out.push(f);
            }
            match r.find("-/") {
                Some(j) => doc = vec![r[..j].trim().to_string()],
                None => {
                    doc = vec![r.trim().to_string()];
                    in_doc = true;
                }
            }
            continue;
        }
        if t.is_empty() || t.starts_with("--") {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let is_field = indent == 2
            && t.split_once(':').is_some_and(|(n, _)| {
                let n = n.trim();
                !n.is_empty() && n.chars().all(|c| c.is_alphanumeric() || c == '_')
            });
        if is_field {
            if let Some(f) = cur.take() {
                out.push(f);
            }
            let (name, prop) = t.split_once(':').unwrap_or((t, ""));
            cur = Some(Field {
                name: name.trim().to_string(),
                doc: doc.join(" ").split_whitespace().collect::<Vec<_>>().join(" "),
                prop: prop.trim().to_string(),
                mutants: Vec::new(),
            });
            doc.clear();
        } else if let Some(f) = cur.as_mut() {
            f.prop.push(' ');
            f.prop.push_str(t);
        }
    }
    if let Some(f) = cur.take() {
        out.push(f);
    }
    out
}

/// `Spec.lean` as an outline: section headers and the declarations under
/// them, each with its docstring and source.
pub fn read_spec(proofs: &Path) -> Option<Spec> {
    let path = proofs.join("Spec.lean");
    let text = std::fs::read_to_string(&path).ok()?;
    let enums = enums(proofs);
    let lines: Vec<&str> = text.lines().collect();
    let mut sections: Vec<SpecSection> = vec![SpecSection { heading: String::new(), items: Vec::new() }];
    let mut doc: Vec<String> = Vec::new();
    let mut in_doc = false;
    let mut in_module_doc = false;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim();
        if in_module_doc {
            in_module_doc = !t.contains("-/");
            i += 1;
            continue;
        }
        if in_doc {
            let (body, done) = match t.find("-/") {
                Some(j) => (&t[..j], true),
                None => (t, false),
            };
            doc.push(body.trim().to_string());
            in_doc = !done;
            i += 1;
            continue;
        }
        if let Some(r) = t.strip_prefix("/-!") {
            let body = r.split("-/").next().unwrap_or("").trim();
            if let Some(h) = body.strip_prefix('#') {
                let heading = h.trim_start_matches('#').trim().to_string();
                if sections.last().is_some_and(|s| s.items.is_empty() && s.heading.is_empty()) {
                    sections.last_mut().expect("one section").heading = heading;
                } else {
                    sections.push(SpecSection { heading, items: Vec::new() });
                }
            }
            in_module_doc = !r.contains("-/");
            i += 1;
            continue;
        }
        if let Some(r) = t.strip_prefix("/--") {
            match r.find("-/") {
                Some(j) => doc = vec![r[..j].trim().to_string()],
                None => {
                    doc = vec![r.trim().to_string()];
                    in_doc = true;
                }
            }
            i += 1;
            continue;
        }
        if let Some((kind, name)) = decl_head(line) {
            // The body runs to the next column-0 line that is not a match arm
            // or a continuation.
            let mut end = i + 1;
            while end < lines.len() {
                let l = lines[end];
                if !l.is_empty() && !l.starts_with(char::is_whitespace) && !l.starts_with('|') {
                    break;
                }
                end += 1;
            }
            while end > i + 1 && lines[end - 1].trim().is_empty() {
                end -= 1;
            }
            let body: Vec<&str> = lines[i..end].to_vec();
            let truncated = body.len() > CODE_LINES;
            let code = body.iter().take(CODE_LINES).cloned().collect::<Vec<_>>().join("\n");
            let full = body.join("\n");
            let fields = if kind == "structure" && lines[i].contains(": Prop") { fields_of(&full) } else { Vec::new() };
            let matrix = if kind == "def" { matrix_of(&full, &enums) } else { None };
            sections.last_mut().expect("one section").items.push(SpecItem {
                kind: kind.to_string(),
                name,
                line: i + 1,
                doc: doc.join(" ").split_whitespace().collect::<Vec<_>>().join(" "),
                code,
                truncated,
                fields,
                matrix,
            });
            doc.clear();
            i = end;
            continue;
        }
        if t.is_empty() {
            doc.clear();
        }
        i += 1;
    }
    sections.retain(|s| !s.items.is_empty());
    Some(Spec { file: "Spec.lean".into(), sections })
}

// ── the assumption ledger and the trusted edges ──────────────────────────────

fn ledger(repo: &Path) -> Vec<Assume> {
    let Ok(text) = std::fs::read_to_string(repo.join(LEDGER)) else { return Vec::new() };
    text.lines()
        .filter(|l| l.starts_with("| A"))
        .filter_map(|l| {
            let cells = split_row(l);
            (cells.len() >= 3).then(|| Assume { id: cells[0].clone(), text: cells[1].clone(), today: cells[2].clone() })
        })
        .collect()
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if !p.file_name().is_some_and(|n| n == "target") {
                rs_files(&p, out);
            }
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Lines where the app steps outside the kernel's guarantee on purpose.
fn edges(repo: &Path, kernel: Option<&Path>, server: Option<&Path>) -> Vec<Edge> {
    let mut out = Vec::new();
    let mut scan = |dir: &Path, needles: &[(&str, &str)]| {
        let mut files = Vec::new();
        rs_files(dir, &mut files);
        files.sort();
        for f in files {
            let Ok(text) = std::fs::read_to_string(&f) else { continue };
            for (i, line) in text.lines().enumerate() {
                for (needle, kind) in needles {
                    if line.contains(needle) {
                        out.push(Edge { kind: (*kind).into(), file: rel(repo, &f), line: i + 1, text: line.trim().to_string() });
                    }
                }
            }
        }
    };
    if let Some(k) = kernel {
        scan(k, &[("h5i-allow: privileged-field", "privileged-field")]);
    }
    if let Some(s) = server {
        scan(s, &[("h5i-allow: no-actor", "no-actor"), ("assume_authenticated", "assume_authenticated")]);
    }
    out
}

fn git_head(repo: &Path) -> Option<String> {
    let r = git2::Repository::discover(repo).ok()?;
    Some(r.head().ok()?.target()?.to_string())
}

/// Builds of this app recorded inside boxes: receipts whose command names
/// cargo and the app's server crate or directory.
pub fn box_builds(h5i_root: &Path, needles: &[String]) -> Vec<BoxBuild> {
    let mut out = Vec::new();
    for m in crate::env::list(h5i_root) {
        let recs = crate::receipt::list(&crate::env::env_dir(h5i_root, &m.agent, &m.slug)).unwrap_or_default();
        for r in recs {
            let Some(cmd) = r.cmd.as_deref() else { continue };
            if !cmd.contains("cargo") {
                continue;
            }
            let hit = needles.iter().any(|n| !n.is_empty() && (cmd.contains(n.as_str()) || r.cwd.as_deref().is_some_and(|c| c.contains(n.as_str()))));
            if hit {
                out.push(BoxBuild { env_id: m.id.clone(), at: r.timestamp.clone(), cmd: cmd.to_string(), git_tree: r.git_tree.clone() });
            }
        }
    }
    out.sort_by(|a, b| b.at.cmp(&a.at));
    out.truncate(BUILDS_CAP);
    out
}

/// What to look for in box receipts: the server crate's name and the app's
/// directory.
pub fn build_needles(proofs: &Path, server: Option<&Path>, repo: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(s) = server
        && let Ok(t) = std::fs::read_to_string(s.join("Cargo.toml"))
    {
        for l in t.lines() {
            if let Some(n) = l.strip_prefix("name") {
                let n = n.trim_start().trim_start_matches('=').trim().trim_matches('"');
                if !n.is_empty() {
                    out.push(n.to_string());
                    break;
                }
            }
        }
    }
    if let Some(parent) = proofs.parent() {
        out.push(rel(repo, parent));
    }
    out
}

// ── detail ───────────────────────────────────────────────────────────────────

/// Clause names of every `Prop` structure in the spec, for attributing a
/// failing proof to the invariant it was about.
fn clause_names(spec: Option<&Spec>) -> Vec<String> {
    spec.map(|s| s.sections.iter().flat_map(|sec| sec.items.iter().flat_map(|i| i.fields.iter().map(|f| f.name.clone()))).collect())
        .unwrap_or_default()
}

fn mentions(text: &str, word: &str) -> bool {
    let mut from = 0;
    while let Some(i) = text[from..].find(word) {
        let at = from + i;
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        let ok_before = before.is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        let ok_after = after.is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        if ok_before && ok_after {
            return true;
        }
        from = at + word.len();
    }
    false
}

/// The source of the declaration a failure sits in, from its head to a few
/// lines past the error.
fn failing_text(files: &[LeanFile], f: &Failure) -> String {
    let Some(file) = files.iter().find(|x| x.name == f.file) else { return String::new() };
    let lines: Vec<&str> = file.text.lines().collect();
    if f.line == 0 || f.line > lines.len() {
        return String::new();
    }
    let mut start = f.line - 1;
    while start > 0 && decl_head(lines[start]).is_none() {
        start -= 1;
    }
    let end = (f.line + 4).min(lines.len());
    lines[start..end].join("\n")
}

/// One proof project in full. `h5i_root` lets the trust tab list builds of
/// the app that ran inside this repository's boxes.
pub fn detail(repo: &Path, dir: &str, h5i_root: Option<&Path>) -> Option<Detail> {
    let proofs = repo.join(dir);
    if !proofs.join("lakefile.lean").is_file() || dir.contains("..") {
        return None;
    }
    let receipt = read_receipt(repo);
    let summary = summary(repo, &proofs, receipt.as_ref())?;
    let scope = read_scope(&proofs);
    let kernel = kernel_dir(&proofs, scope.as_ref());
    let server = server_dir(&proofs, scope.as_ref());
    let files = lean_sources(&proofs);
    let readme = readme_path(&proofs).map(|p| rel(repo, &p));
    let table = readme.as_ref().and_then(|p| std::fs::read_to_string(repo.join(p)).ok()).map(|t| theorem_table(&t)).unwrap_or_default();
    let spec = read_spec(&proofs);

    let mutants: Vec<&Mutant> = receipt.as_ref().map(|r| mutants_of(r, dir).collect()).unwrap_or_default();
    let axioms: Vec<&AxiomRec> = receipt.as_ref().map(|r| r.axioms.iter().filter(|a| a.dir == dir).collect()).unwrap_or_default();

    // Theorems: the README's rows, then counterexamples and gated theorems
    // the table does not name.
    let mut names: Vec<(String, String)> = table.clone();
    if let Some(s) = &scope {
        for c in &s.counterexample {
            if !names.iter().any(|(n, _)| n == &c.theorem) {
                names.push((c.theorem.clone(), String::new()));
            }
        }
    }
    for a in &axioms {
        let short = a.theorem.rsplit('.').next().unwrap_or(&a.theorem);
        if !names.iter().any(|(n, _)| n == short || n == &a.theorem) {
            names.push((short.to_string(), String::new()));
        }
    }
    let theorems: Vec<Theorem> = names
        .into_iter()
        .map(|(name, statement)| {
            let found = find_theorem(&files, &name);
            let short = name.rsplit('.').next().unwrap_or(&name).to_string();
            let qualified = found.as_ref().map(|f| f.3.clone());
            let suffix_hit = |q: &str| q == name || q.ends_with(&format!(".{short}")) || q == short;
            let axioms = axioms.iter().find(|a| suffix_hit(&a.theorem)).map(|a| (*a).clone());
            let expected_by = mutants.iter().filter(|m| m.expect.iter().any(|e| e == &short)).map(|m| m.name.clone()).collect();
            let caught = mutants
                .iter()
                .filter(|m| m.failed.iter().any(|f| f.decl.as_deref().is_some_and(suffix_hit)))
                .map(|m| m.name.clone())
                .collect();
            let bug = scope.as_ref().and_then(|s| s.counterexample.iter().find(|c| c.theorem == short).cloned());
            Theorem {
                counterexample: found.as_ref().is_some_and(|f| f.4) || bug.is_some(),
                name,
                qualified,
                file: found.as_ref().map(|f| f.0.clone()),
                line: found.as_ref().map(|f| f.1),
                doc: found.as_ref().map(|f| f.2.clone()).unwrap_or_default(),
                statement,
                bug,
                axioms,
                expected_by,
                caught,
            }
        })
        .collect();

    // Clause attribution: a mutant's failing proof text names the clause.
    let mut spec = spec;
    let clauses = clause_names(spec.as_ref());
    let mut hits: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for m in &mutants {
        for f in &m.failed {
            let text = failing_text(&files, f);
            for c in &clauses {
                if mentions(&text, c) {
                    let v = hits.entry(c.clone()).or_default();
                    if !v.contains(&m.name) {
                        v.push(m.name.clone());
                    }
                }
            }
        }
    }
    if let Some(s) = spec.as_mut() {
        for sec in &mut s.sections {
            for item in &mut sec.items {
                for f in &mut item.fields {
                    if let Some(v) = hits.get(&f.name) {
                        f.mutants = v.clone();
                    }
                }
            }
        }
    }

    let mutant_views: Vec<MutantView> = mutants
        .iter()
        .map(|m| {
            let failed: Vec<String> = m.failed.iter().filter_map(|f| f.decl.clone()).fold(Vec::new(), |mut v, d| {
                if !v.contains(&d) {
                    v.push(d);
                }
                v
            });
            let as_expected = if m.expect.is_empty() || m.verdict != "caught" {
                None
            } else {
                Some(m.expect.iter().any(|e| failed.iter().any(|d| d == e || d.ends_with(&format!(".{e}")))))
            };
            MutantView {
                name: m.name.clone(),
                bug: m.bug.clone(),
                expect: m.expect.clone(),
                verdict: m.verdict.clone(),
                stage: m.stage.clone(),
                failed,
                as_expected,
                secs: m.secs,
            }
        })
        .collect();

    let ledger = ledger(repo);
    let assumes = scope
        .as_ref()
        .map(|s| {
            s.assumes
                .iter()
                .map(|id| {
                    ledger.iter().find(|a| &a.id == id).cloned().unwrap_or(Assume { id: id.clone(), text: String::new(), today: String::new() })
                })
                .collect()
        })
        .unwrap_or_default();

    let rp = receipt.as_ref().and_then(|r| r.projects.iter().find(|p| p.dir == dir));
    let kernel_digest = kernel.as_deref().map(|k| digest_tree(k, "rs"));
    let proofs_digest = digest_tree(&proofs, "lean");
    let builds = h5i_root.map(|root| box_builds(root, &build_needles(&proofs, server.as_deref(), repo))).unwrap_or_default();
    let version = Version {
        receipt_head: receipt.as_ref().map(|r| r.git.head.clone()),
        receipt_dirty: receipt.as_ref().map(|r| r.git.dirty),
        current_head: git_head(repo),
        kernel_match: match (rp.and_then(|p| p.kernel_digest.as_ref()), &kernel_digest) {
            (Some(a), Some(b)) => Some(a == b),
            _ => None,
        },
        proofs_match: rp.map(|p| p.proofs_digest == proofs_digest),
        kernel_digest,
        proofs_digest,
        builds,
    };
    let trust = Trust {
        assumes,
        trusted_inputs: scope.as_ref().map(|s| s.trusted_input.clone()).unwrap_or_default(),
        out_of_scope: scope.as_ref().map(|s| s.out_of_scope.clone()).unwrap_or_default(),
        edges: edges(repo, kernel.as_deref(), server.as_deref()),
        version,
    };
    let steps = receipt
        .as_ref()
        .map(|r| r.steps.iter().filter(|s| s.project.as_deref().is_none_or(|p| p == dir)).cloned().collect())
        .unwrap_or_default();
    let evidence = Evidence {
        steps,
        counts: mutant_counts(mutants.iter().copied()),
        mutants: mutant_views,
        difftest: receipt.as_ref().and_then(|r| r.difftest.clone()).filter(|_| dir == "examples/app/docs/proofs"),
        summary: receipt.as_ref().map(|r| r.summary.clone()),
    };
    Some(Detail { summary, scope, readme, spec, theorems, trust, evidence })
}

// ── a checklist for the project store ────────────────────────────────────────

/// The proofs as checklist items: each theorem is "proven, application
/// unconfirmed" until the trusted edges below it are checked by hand.
pub fn checklist_markdown(repo: &Path, dir: &str) -> Option<(String, String)> {
    let d = detail(repo, dir, None)?;
    let app = if d.summary.app.is_empty() { dir.to_string() } else { d.summary.app.clone() };
    let mut md = format!("# Proofs of {app}: apply the guarantee\n\n");
    md.push_str(&format!(
        "Items come from `{dir}` (proofs digest `{}`). A theorem is about the extracted kernel; it says something about a running server only once the assumptions and the version below are confirmed.\n\n",
        &d.trust.version.proofs_digest[..12.min(d.trust.version.proofs_digest.len())]
    ));
    let assumes: Vec<&str> = d.trust.assumes.iter().map(|a| a.id.as_str()).collect();
    md.push_str("## Theorems: proven, application unconfirmed\n\n");
    for t in d.theorems.iter().filter(|t| !t.counterexample) {
        let what = if t.statement.is_empty() { t.doc.clone() } else { t.statement.clone() };
        let assume = if assumes.is_empty() { String::new() } else { format!(" Assumes {}.", assumes.join(", ")) };
        md.push_str(&format!("- `{}`: {what}{assume}\n", t.name));
    }
    if d.theorems.iter().any(|t| t.counterexample) {
        md.push_str("\n## Counterexamples: confirm the bug is absent from the build under test\n\n");
        for t in d.theorems.iter().filter(|t| t.counterexample) {
            let bug = t.bug.as_ref().map(|b| format!(" ({}{})", b.bug, if b.current { ", still present upstream" } else { "" })).unwrap_or_default();
            let what = if t.statement.is_empty() { t.doc.clone() } else { t.statement.clone() };
            md.push_str(&format!("- `{}`{bug}: {what}\n", t.name));
        }
    }
    md.push_str("\n## Trusted edges: confirm each on the running app\n\n");
    for a in &d.trust.assumes {
        let text = if a.text.is_empty() { "see the assumption ledger".to_string() } else { a.text.clone() };
        md.push_str(&format!("- {}: {text}\n", a.id));
    }
    for i in &d.trust.trusted_inputs {
        let from = if i.from.is_empty() { String::new() } else { format!(" from {}", i.from) };
        md.push_str(&format!("- shell input `{}`{from}: the theorems hold for any value; confirm the source\n", i.name));
    }
    for e in &d.trust.edges {
        md.push_str(&format!("- `{}:{}` {}: a route or field outside the kernel's check; confirm its own check\n", e.file, e.line, e.kind));
    }
    if !d.trust.out_of_scope.is_empty() {
        md.push_str("\n## Out of scope by design: cover by other means\n\n");
        for o in &d.trust.out_of_scope {
            let reason = if o.reason.is_empty() { String::new() } else { format!(": {}", o.reason) };
            md.push_str(&format!("- {}{reason}\n", o.item));
        }
    }
    md.push_str("\n## Version: the build under test is the proven code\n\n");
    md.push_str(&format!("- Proofs digest at import: `{}`\n", d.trust.version.proofs_digest));
    if let Some(k) = &d.trust.version.kernel_digest {
        md.push_str(&format!("- Kernel digest at import: `{k}`; confirm the server under test was built from it\n"));
    }
    match &d.trust.version.receipt_head {
        Some(h) => md.push_str(&format!("- Last `cargo app-verify` ran at commit `{h}`; confirm the extraction check passed there\n")),
        None => md.push_str("- No `cargo app-verify` receipt yet; run it and confirm the extraction check\n"),
    }
    Some((app, md))
}

/// `(repository root, proofs dir relative to it)` for a proofs directory
/// given on the command line.
pub fn locate(dir: &Path) -> Option<(PathBuf, String)> {
    let abs = dir.canonicalize().ok()?;
    let git = git2::Repository::discover(&abs).ok()?;
    let root = git.workdir()?.canonicalize().ok()?;
    let rel = abs.strip_prefix(&root).ok()?.to_string_lossy().replace('\\', "/");
    Some((root, rel))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("h5i-apps-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn theorem_table_splits_multi_name_cells() {
        let md = "## Theorems\n\n| Theorem | Statement |\n|---|---|\n| `a`, `b` | Both hold. |\n| `c` | Pipe \\| inside. |\n\n## Other\n| `d` | no |\n";
        let rows = theorem_table(md);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], ("a".into(), "Both hold.".into()));
        assert_eq!(rows[1].0, "b");
        assert_eq!(rows[2].1, "Pipe | inside.");
    }

    #[test]
    fn decl_heads_and_namespaces() {
        assert_eq!(decl_head("theorem foo (a : Nat) : a = a := rfl"), Some(("theorem", "foo".into())));
        assert_eq!(decl_head("@[simp] private def bar.baz : Nat := 1"), Some(("def", "bar.baz".into())));
        assert_eq!(decl_head("  theorem indented : True := trivial"), None);
        assert_eq!(decl_head("structure Inv (s : St) : Prop where"), Some(("structure", "Inv".into())));
        let files = vec![LeanFile {
            name: "T.lean".into(),
            text: "namespace k.T\n\n/-- Never fails. -/\ntheorem total (s : S) :\n    ∃ r, f s = .ok r := by\n  simp\n\n/-- Refutes. -/\ntheorem pre_broken : ¬ Safe old := by\n  decide\n\nend k.T\n".into(),
        }];
        let (file, line, doc, qualified, neg) = find_theorem(&files, "total").unwrap();
        assert_eq!((file.as_str(), line, neg), ("T.lean", 4, false));
        assert_eq!(doc, "Never fails.");
        assert_eq!(qualified, "k.T.total");
        assert!(find_theorem(&files, "pre_broken").unwrap().4);
        assert!(find_theorem(&files, "k.T.total").is_some());
    }

    #[test]
    fn spec_outline_fields_and_matrix() {
        let d = tmp("spec");
        std::fs::create_dir_all(d.join("generated")).unwrap();
        std::fs::write(d.join("lakefile.lean"), "").unwrap();
        std::fs::write(
            d.join("generated/K.lean"),
            "inductive Role where\n| Viewer : Role\n| Owner : Role\n\ninductive Action where\n| Read : Action\n| Write : Action\n\ninductive Write where\n| Put : Nat → Write\n",
        )
        .unwrap();
        std::fs::write(
            d.join("Spec.lean"),
            "import K\n/-! # The spec -/\n\n/-! ## Policy -/\n\ndef policy : Role → Action → Bool\n  | .Owner, _ => true\n  | .Viewer, .Read => true\n  | _, _ => false\n\n/-! ## Invariants -/\n\nstructure Inv (s : St) : Prop where\n  /-- Every project has an owner. -/\n  owned : ∀ p ∈ s.projects, 0 < owners s.members p\n  /-- Keys are unique,\n  across both lists. -/\n  keys : (s.projects.map (·.id)).Nodup ∧\n    (s.docs.map (·.id)).Nodup\n\ndef init : St := ⟨0, []⟩\n",
        )
        .unwrap();
        let spec = read_spec(&d).unwrap();
        assert_eq!(spec.sections.len(), 2);
        assert_eq!(spec.sections[0].heading, "Policy");
        let policy = &spec.sections[0].items[0];
        let m = policy.matrix.as_ref().expect("matrix");
        assert_eq!(m.rows, ["Viewer", "Owner"]);
        assert_eq!(m.cells[0], ["true", "false"]);
        assert_eq!(m.cells[1], ["true", "true"]);
        let inv = &spec.sections[1].items[0];
        assert_eq!(inv.fields.len(), 2);
        assert_eq!(inv.fields[0].doc, "Every project has an owner.");
        assert_eq!(inv.fields[1].doc, "Keys are unique, across both lists.");
        assert!(inv.fields[1].prop.contains("docs"));
        assert_eq!(spec.sections[1].items[1].name, "init");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn discovery_and_detail_without_receipt() {
        let repo = tmp("repo");
        let proofs = repo.join("examples/app/x/proofs");
        std::fs::create_dir_all(proofs.join("generated")).unwrap();
        std::fs::create_dir_all(repo.join("examples/app/x/kernel/src")).unwrap();
        std::fs::create_dir_all(repo.join("examples/app/x/server/src")).unwrap();
        std::fs::write(repo.join("examples/app/x/kernel/src/lib.rs"), "pub enum Command { A { role: u8 } } // h5i-allow: privileged-field\n").unwrap();
        std::fs::write(repo.join("examples/app/x/server/src/lib.rs"), "// h5i-allow: no-actor\nfn h() {}\n").unwrap();
        std::fs::write(repo.join("examples/app/x/server/Cargo.toml"), "[package]\nname = \"x-server\"\n").unwrap();
        std::fs::write(proofs.join("lakefile.lean"), "package x").unwrap();
        std::fs::write(proofs.join("Theorems.lean"), "namespace x_kernel.Theorems\n/-- Holds. -/\ntheorem total : True := trivial\ntheorem old_broken : ¬ False := by simp\nend x_kernel.Theorems\n").unwrap();
        std::fs::write(proofs.join("scope.toml"), "app = \"x\"\nassumes = [\"A1\"]\n[[counterexample]]\ntheorem = \"old_broken\"\nbug = \"issue #1\"\n[[out_of_scope]]\nitem = \"liveness\"\n").unwrap();
        std::fs::write(repo.join("examples/app/x/README.md"), "# x\n## Theorems\n| Theorem | Statement |\n|---|---|\n| `total` | Never fails. |\n").unwrap();
        // Build output must not be mistaken for a project.
        std::fs::create_dir_all(proofs.join(".lake/packages/dep")).unwrap();
        std::fs::write(proofs.join(".lake/packages/dep/lakefile.lean"), "").unwrap();
        std::fs::write(proofs.join(".lake/packages/dep/A.lean"), "").unwrap();

        let list = discover(&repo);
        assert_eq!(list.len(), 1);
        let s = &list[0];
        assert_eq!((s.dir.as_str(), s.app.as_str(), s.kind.as_str()), ("examples/app/x/proofs", "x", "server"));
        assert_eq!((s.theorems, s.counterexamples), (1, 1));
        assert!(s.flags.contains(&"no receipt".to_string()));

        let d = detail(&repo, "examples/app/x/proofs", None).unwrap();
        assert_eq!(d.theorems.len(), 2);
        assert_eq!(d.theorems[0].statement, "Never fails.");
        assert_eq!(d.theorems[0].qualified.as_deref(), Some("x_kernel.Theorems.total"));
        assert!(d.theorems[1].counterexample);
        assert_eq!(d.theorems[1].bug.as_ref().unwrap().bug, "issue #1");
        assert_eq!(d.trust.edges.len(), 2);
        assert_eq!(d.trust.assumes[0].id, "A1");
        assert_eq!(d.trust.version.kernel_match, None);
        assert_eq!(build_needles(&proofs, Some(&repo.join("examples/app/x/server")), &repo), ["x-server", "examples/app/x"]);
        let (app, md) = checklist_markdown(&repo, "examples/app/x/proofs").unwrap();
        assert_eq!(app, "x");
        assert!(md.contains("- `total`: Never fails. Assumes A1."));
        assert!(md.contains("old_broken"));
        assert!(md.contains("no-actor"));
        assert!(detail(&repo, "examples/app/x/../x/proofs", None).is_none());
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn receipt_matching_and_clause_attribution() {
        let repo = tmp("receipt");
        let proofs = repo.join("examples/app/docs/proofs");
        std::fs::create_dir_all(repo.join(".h5i/app-verify")).unwrap();
        std::fs::create_dir_all(&proofs).unwrap();
        std::fs::write(proofs.join("lakefile.lean"), "").unwrap();
        std::fs::write(proofs.join("Spec.lean"), "structure Inv (s : St) : Prop where\n  /-- Owned. -/\n  owned : True\n  /-- Fresh. -/\n  fresh : True\n").unwrap();
        std::fs::write(proofs.join("Invariants.lean"), "namespace d.T\ntheorem inv_preserved : True := by\n  have := h.owned\n  trivial\nend d.T\n").unwrap();
        let proofs_digest = digest_tree(&proofs, "lean");
        let receipt = serde_json::json!({
            "version": 1, "started": "2026-10-02T00:00:00Z", "git": {"head": "abc", "branch": "main", "dirty": false},
            "projects": [{"dir": "examples/app/docs/proofs", "app": "docs", "proofs_digest": proofs_digest, "lean": "pass", "extraction": "match"}],
            "axioms": [{"dir": "examples/app/docs/proofs", "theorem": "d.T.inv_preserved", "axioms": ["propext"], "verdict": "ok"}],
            "mutants": [
                {"suite": "docs", "name": "remove_last_owner", "bug": "last owner removable", "expect": ["inv_preserved"], "verdict": "caught", "stage": "lake",
                 "failed": [{"file": "Invariants.lean", "line": 3, "col": 2, "message": "unsolved goals", "decl": "d.T.inv_preserved"}]},
                {"suite": "docs", "name": "sneaky", "bug": "", "expect": ["authorized"], "verdict": "survived", "stage": "lake", "failed": []},
                {"suite": "apps", "name": "kellnr", "app": "examples/app/kellnr", "verdict": "caught", "failed": []}
            ]
        });
        std::fs::write(repo.join(RECEIPT), receipt.to_string()).unwrap();
        let list = discover(&repo);
        let r = list[0].receipt.as_ref().unwrap();
        assert!(r.proofs_match);
        assert_eq!((r.mutants.prepared, r.mutants.caught, r.mutants.survived), (2, 1, 1));
        assert_eq!(r.axioms.ok, 1);
        assert!(list[0].flags.contains(&"mutant survived".to_string()));
        let d = detail(&repo, "examples/app/docs/proofs", None).unwrap();
        let inv = &d.spec.as_ref().unwrap().sections[0].items[0];
        assert_eq!(inv.fields[0].mutants, ["remove_last_owner"]);
        assert!(inv.fields[1].mutants.is_empty());
        let t = d.theorems.iter().find(|t| t.name == "inv_preserved").unwrap();
        assert_eq!(t.caught, ["remove_last_owner"]);
        assert_eq!(t.expected_by, ["remove_last_owner"]);
        assert!(t.axioms.is_some());
        let mv = d.evidence.mutants.iter().find(|m| m.name == "remove_last_owner").unwrap();
        assert_eq!(mv.as_expected, Some(true));
        assert_eq!(d.evidence.mutants.len(), 2);
        let _ = std::fs::remove_dir_all(&repo);
    }
}
