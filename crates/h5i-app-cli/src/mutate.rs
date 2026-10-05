//! `h5i app mutate`: put a bug in the kernel, re-extract it, rebuild the
//! proofs. A mutant is caught when the proofs stop building; one that survives
//! is a behaviour the spec does not pin down.
//!
//! Each mutant runs in its own copy of the project: the kernel and the path
//! crates it depends on, laid out as in the original so relative paths still
//! resolve, under a fresh Cargo workspace. The copy's Lake project shares the
//! original's packages (read-only), so a mutant rebuilds only the kernel's
//! extraction and the proofs above it.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use quote::ToTokens;
use syn::visit::Visit;

use crate::manifest::{Edit, Project};
use crate::util::{self, Out};

#[derive(Clone, Debug)]
pub struct Mutant {
    pub name: String,
    /// What the change is, for generated mutants: "`a <= b` -> `a < b`".
    pub what: Option<String>,
    /// Relative to the project root.
    pub file: PathBuf,
    pub change: Change,
}

#[derive(Clone, Debug)]
pub enum Change {
    /// Declared in the manifest: each `old` occurs exactly once.
    Replace(Vec<Edit>),
    /// Generated: replace bytes `start..end`, which must read `was`.
    Splice {
        start: usize,
        end: usize,
        was: String,
        new: String,
    },
}

impl Mutant {
    pub fn apply(&self, src: &str) -> Result<String> {
        match &self.change {
            Change::Replace(edits) => {
                let mut s = src.to_string();
                for e in edits {
                    let n = s.matches(&e.old).count();
                    if n != 1 {
                        bail!("pattern occurs {n} times, not once: {:?}", trunc(&e.old));
                    }
                    s = s.replacen(&e.old, &e.new, 1);
                }
                Ok(s)
            }
            Change::Splice {
                start,
                end,
                was,
                new,
            } => {
                if src.get(*start..*end) != Some(was.as_str()) {
                    bail!("source changed under the mutant");
                }
                Ok(format!("{}{}{}", &src[..*start], new, &src[*end..]))
            }
        }
    }
}

fn trunc(s: &str) -> String {
    if s.chars().count() > 60 {
        s.chars().take(60).collect::<String>() + "…"
    } else {
        s.to_string()
    }
}

/// The manifest's `[[mutant]]`s.
pub fn declared(p: &Project) -> Result<Vec<Mutant>> {
    let default_file = p
        .manifest
        .extract
        .as_ref()
        .map(|e| PathBuf::from(&e.krate).join("src/lib.rs"));
    p.manifest
        .mutants
        .iter()
        .map(|m| {
            let file = match (&m.file, &default_file) {
                (Some(f), _) => PathBuf::from(f),
                (None, Some(d)) => d.clone(),
                (None, None) => bail!(
                    "mutant `{}` needs `file`: the project extracts no crate",
                    m.name
                ),
            };
            Ok(Mutant {
                name: m.name.clone(),
                what: None,
                file,
                change: Change::Replace(m.edits()?),
            })
        })
        .collect()
}

/// Mutants generated from the kernel's syntax: comparison boundaries
/// (`<` and `<=`), equality flipped, `&&` and `||` swapped, a `!` dropped, an
/// `if` condition forced to `true` or `false`. Test code is skipped.
pub fn generated(p: &Project) -> Result<Vec<Mutant>> {
    let Some(krate) = p.krate() else {
        bail!("{}: no [extract] crate to mutate", p.display())
    };
    let mut files = Vec::new();
    rs_files(&krate.join("src"), &mut files);
    files.sort();
    let mut out = Vec::new();
    for f in files {
        let src = std::fs::read_to_string(&f)?;
        let ast = syn::parse_file(&src).with_context(|| format!("parsing {}", f.display()))?;
        let rel = f.strip_prefix(&p.root)?.to_path_buf();
        let mut v = Finder {
            src: &src,
            lines: line_starts(&src),
            func: String::new(),
            found: Vec::new(),
        };
        v.visit_file(&ast);
        let short = f.strip_prefix(&krate).unwrap_or(&f).display().to_string();
        let mut names = std::collections::HashMap::new();
        for (fun, line, start, end, was, new) in v.found {
            let kind = kind(&was, &new);
            let base = format!("{fun}@{short}:{line}:{kind}");
            let n = names.entry(base.clone()).or_insert(0);
            *n += 1;
            let name = if *n == 1 { base } else { format!("{base}#{n}") };
            let what = if new.is_empty() {
                format!("drop `{was}`")
            } else {
                format!("`{was}` -> `{new}`")
            };
            out.push(Mutant {
                name,
                what: Some(what),
                file: rel.clone(),
                change: Change::Splice {
                    start,
                    end,
                    was,
                    new,
                },
            });
        }
    }
    Ok(out)
}

/// A short tag for a generated change, used in its name.
fn kind(was: &str, new: &str) -> &'static str {
    match (was, new) {
        (_, "true") => "if-true",
        (_, "false") => "if-false",
        ("!", "") => "drop-not",
        ("<" | "<=" | ">" | ">=", _) => "boundary",
        ("==" | "!=", _) => "negate",
        ("&&" | "||", _) => "and-or",
        _ => "edit",
    }
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

fn line_starts(src: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(src.match_indices('\n').map(|(i, _)| i + 1))
        .collect()
}

struct Finder<'a> {
    src: &'a str,
    lines: Vec<usize>,
    func: String,
    /// (function, line, start, end, was, new)
    found: Vec<(String, usize, usize, usize, String, String)>,
}

impl Finder<'_> {
    /// Byte offset of a proc-macro2 location (1-based line, column in chars).
    fn offset(&self, lc: proc_macro2::LineColumn) -> Option<usize> {
        let start = *self.lines.get(lc.line.checked_sub(1)?)?;
        let line = &self.src[start..];
        let col = line
            .char_indices()
            .nth(lc.column)
            .map(|(i, _)| i)
            .unwrap_or(line.len());
        Some(start + col)
    }

    fn range(&self, t: &impl ToTokens) -> Option<(usize, usize, usize)> {
        let ts: Vec<_> = t.to_token_stream().into_iter().collect();
        let (first, last) = (ts.first()?, ts.last()?);
        let (s, e) = (first.span().start(), last.span().end());
        Some((s.line, self.offset(s)?, self.offset(e)?))
    }

    fn push(&mut self, t: &impl ToTokens, new: &str) {
        let Some((line, start, end)) = self.range(t) else {
            return;
        };
        let was = self.src[start..end].to_string();
        self.found
            .push((self.func.clone(), line, start, end, was, new.to_string()));
    }
}

fn is_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        let s = a.to_token_stream().to_string().replace(' ', "");
        s == "#[test]" || s.starts_with("#[cfg(test") || s.contains("cfg(test)")
    })
}

impl<'ast> Visit<'ast> for Finder<'_> {
    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        if !is_test(&m.attrs) {
            syn::visit::visit_item_mod(self, m);
        }
    }

    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        if is_test(&f.attrs) {
            return;
        }
        let saved = std::mem::replace(&mut self.func, f.sig.ident.to_string());
        syn::visit::visit_item_fn(self, f);
        self.func = saved;
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        if is_test(&f.attrs) {
            return;
        }
        let saved = std::mem::replace(&mut self.func, f.sig.ident.to_string());
        syn::visit::visit_impl_item_fn(self, f);
        self.func = saved;
    }

    fn visit_expr_binary(&mut self, e: &'ast syn::ExprBinary) {
        use syn::BinOp::*;
        let new = match e.op {
            Lt(_) => Some("<="),
            Le(_) => Some("<"),
            Gt(_) => Some(">="),
            Ge(_) => Some(">"),
            Eq(_) => Some("!="),
            Ne(_) => Some("=="),
            And(_) => Some("||"),
            Or(_) => Some("&&"),
            _ => None,
        };
        if let (Some(new), false) = (new, self.func.is_empty()) {
            self.push(&e.op, new);
        }
        syn::visit::visit_expr_binary(self, e);
    }

    fn visit_expr_unary(&mut self, e: &'ast syn::ExprUnary) {
        if matches!(e.op, syn::UnOp::Not(_)) && !self.func.is_empty() {
            // `!x` becomes `x`: splice the operator out.
            if let Some((line, start, _)) = self.range(&e.op) {
                self.found.push((
                    self.func.clone(),
                    line,
                    start,
                    start + 1,
                    "!".into(),
                    String::new(),
                ));
            }
        }
        syn::visit::visit_expr_unary(self, e);
    }

    fn visit_expr_if(&mut self, e: &'ast syn::ExprIf) {
        if !matches!(*e.cond, syn::Expr::Let(_)) && !self.func.is_empty() {
            let cond = &*e.cond;
            if !matches!(cond, syn::Expr::Lit(_)) {
                self.push(cond, "true");
                self.push(cond, "false");
            }
        }
        syn::visit::visit_expr_if(self, e);
    }

    // Macro bodies are tokens, not syntax: `schema!` and friends are not
    // mutated.
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The proofs no longer build.
    Caught,
    /// Everything builds: the spec does not see this bug.
    Survived,
    /// The baseline built.
    Ok,
    /// The edit did not apply, or the mutated Rust or its extraction failed.
    Invalid(&'static str),
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Verdict::Caught => write!(f, "caught"),
            Verdict::Survived => write!(f, "SURVIVED"),
            Verdict::Ok => write!(f, "ok"),
            Verdict::Invalid(why) => write!(f, "invalid ({why})"),
        }
    }
}

/// A copy of the project to mutate.
struct Sandbox {
    _tmp: tempfile::TempDir,
    project: Project,
    log: PathBuf,
}

/// Lay out a copy of `p` in a temporary directory: the kernel's path crates
/// at their relative places under a fresh workspace, the manifest and the
/// proofs, with the Lake packages and path requires pointing at the original.
fn sandbox(p: &Project, name: &str) -> Result<Sandbox> {
    let krate = p.krate().context("no [extract] crate")?;
    let meta = util::metadata(&krate, true)?;
    let kernel = meta.package_at(&krate)?;
    let locals = meta.local_closure(kernel);
    let dirs: Vec<PathBuf> = locals
        .iter()
        .map(|q| q.manifest_path.parent().unwrap().canonicalize())
        .collect::<std::io::Result<_>>()?;
    let proofs = p.proofs().canonicalize()?;
    let mut anchor = p.root.clone();
    for d in dirs.iter().chain([&proofs]) {
        while !d.starts_with(&anchor) {
            anchor = anchor.parent().context("no common ancestor")?.to_path_buf();
        }
    }
    let safe: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .take(40)
        .collect();
    let tmp = tempfile::Builder::new()
        .prefix(&format!("h5i-app-mutant-{safe}-"))
        .tempdir()?;
    let at = |orig: &Path| tmp.path().join(orig.strip_prefix(&anchor).unwrap());

    let mut members = Vec::new();
    let mut ws = WorkspaceTables::default();
    for d in &dirs {
        util::copy_tree(d, &at(d))?;
        strip_dev_dependencies(&at(d).join("Cargo.toml"))?;
        members.push(d.strip_prefix(&anchor)?.to_string_lossy().into_owned());
        ws.absorb(d, &anchor, &dirs)?;
    }
    let lock = meta.workspace_root.join("Cargo.lock");
    if lock.is_file() {
        std::fs::copy(&lock, tmp.path().join("Cargo.lock"))?;
    }
    std::fs::write(tmp.path().join("Cargo.toml"), ws.render(&members)?)?;

    std::fs::create_dir_all(at(&p.root))?;
    std::fs::copy(
        p.root.join(crate::manifest::FILE),
        at(&p.root).join(crate::manifest::FILE),
    )?;
    let copy = at(&proofs);
    if !copy.exists() {
        util::copy_tree(&proofs, &copy)?;
    }
    let packages = proofs.join(".lake/packages");
    if !packages.exists() {
        bail!(
            "{} has no Lake packages yet; run `h5i app check` once first",
            proofs.display()
        );
    }
    std::fs::create_dir_all(copy.join(".lake"))?;
    util::symlink(&packages.canonicalize()?, &copy.join(".lake/packages"))?;
    absolute_path_requires(&proofs, &copy)?;

    let project = Project::load(&at(&p.root).join(crate::manifest::FILE))?;
    let log = tmp.path().join("log.txt");
    Ok(Sandbox {
        _tmp: tmp,
        project,
        log,
    })
}

/// Dev-dependencies are never built here, but Cargo would still resolve them,
/// and they may point at crates the copy does not have.
fn strip_dev_dependencies(manifest: &Path) -> Result<()> {
    let text = std::fs::read_to_string(manifest)?;
    let mut doc: toml::Table = toml::from_str(&text)?;
    if doc.remove("dev-dependencies").is_some() {
        std::fs::write(manifest, toml::to_string(&doc)?)?;
    }
    Ok(())
}

/// What the copied crates inherit with `workspace = true`, gathered from the
/// workspaces they came from.
#[derive(Default)]
struct WorkspaceTables {
    package: toml::Table,
    dependencies: toml::Table,
    lints: toml::Table,
    seen: Vec<PathBuf>,
}

impl WorkspaceTables {
    fn absorb(&mut self, crate_dir: &Path, anchor: &Path, copied: &[PathBuf]) -> Result<()> {
        let Some(root) = crate_dir
            .ancestors()
            .map(|a| a.join("Cargo.toml"))
            .find(|m| {
                std::fs::read_to_string(m)
                    .ok()
                    .and_then(|t| t.parse::<toml::Table>().ok())
                    .is_some_and(|t| t.contains_key("workspace"))
            })
        else {
            return Ok(());
        };
        if self.seen.contains(&root) {
            return Ok(());
        }
        self.seen.push(root.clone());
        let doc: toml::Table = std::fs::read_to_string(&root)?.parse()?;
        let ws_dir = root.parent().unwrap();
        let Some(ws) = doc.get("workspace").and_then(|w| w.as_table()) else {
            return Ok(());
        };
        let merge = |into: &mut toml::Table, key: &str| {
            if let Some(t) = ws.get(key).and_then(|v| v.as_table()) {
                for (k, v) in t {
                    into.entry(k.clone()).or_insert(v.clone());
                }
            }
        };
        merge(&mut self.package, "package");
        merge(&mut self.lints, "lints");
        if let Some(deps) = ws.get("dependencies").and_then(|v| v.as_table()) {
            for (k, v) in deps {
                let mut v = v.clone();
                if let Some(path) = v.get("path").and_then(|p| p.as_str()) {
                    let target = ws_dir
                        .join(path)
                        .canonicalize()
                        .unwrap_or_else(|_| ws_dir.join(path));
                    if !copied.contains(&target) {
                        continue;
                    }
                    let rel = target.strip_prefix(anchor)?.to_string_lossy().into_owned();
                    v.as_table_mut()
                        .unwrap()
                        .insert("path".into(), toml::Value::String(rel));
                }
                self.dependencies.entry(k.clone()).or_insert(v);
            }
        }
        Ok(())
    }

    fn render(&self, members: &[String]) -> Result<String> {
        let mut ws = toml::Table::new();
        ws.insert("resolver".into(), "3".into());
        ws.insert(
            "members".into(),
            toml::Value::Array(members.iter().map(|m| m.clone().into()).collect()),
        );
        for (k, t) in [
            ("package", &self.package),
            ("dependencies", &self.dependencies),
            ("lints", &self.lints),
        ] {
            if !t.is_empty() {
                ws.insert(k.into(), toml::Value::Table(t.clone()));
            }
        }
        let mut doc = toml::Table::new();
        doc.insert("workspace".into(), toml::Value::Table(ws));
        Ok(toml::to_string(&doc)?)
    }
}

/// A Lake `require … from "../relative"` would point inside the copy, where
/// that package is not: point it at the original, already built.
fn absolute_path_requires(orig: &Path, copy: &Path) -> Result<()> {
    let manifest = orig.join("lake-manifest.json");
    let Ok(text) = std::fs::read_to_string(&manifest) else {
        return Ok(());
    };
    let json: serde_json::Value = serde_json::from_str(&text)?;
    let mut dirs = Vec::new();
    for pkg in json
        .get("packages")
        .and_then(|p| p.as_array())
        .into_iter()
        .flatten()
    {
        if pkg.get("type").and_then(|t| t.as_str()) == Some("path")
            && let Some(dir) = pkg.get("dir").and_then(|d| d.as_str())
            && Path::new(dir).is_relative()
        {
            let abs = orig
                .join(dir)
                .canonicalize()
                .with_context(|| format!("Lake path package {dir}"))?;
            dirs.push((dir.to_string(), abs.to_string_lossy().into_owned()));
        }
    }
    for f in ["lakefile.lean", "lakefile.toml", "lake-manifest.json"] {
        let path = copy.join(f);
        let Ok(mut text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (rel, abs) in &dirs {
            text = text.replace(&format!("\"{rel}\""), &format!("\"{abs}\""));
        }
        std::fs::write(&path, text)?;
    }
    Ok(())
}

fn evaluate(
    p: &Project,
    m: Option<&Mutant>,
    targets: &[String],
    keep: bool,
) -> Result<(Verdict, Option<PathBuf>)> {
    let name = m.map_or("baseline", |m| m.name.as_str());
    let sb = sandbox(p, name)?;
    let out = Out::Log(sb.log.clone());
    let verdict = (|| -> Result<Verdict> {
        if let Some(m) = m {
            let file = sb.project.root.join(&m.file);
            let src = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            match m.apply(&src) {
                Ok(s) => std::fs::write(&file, s)?,
                Err(e) => {
                    out.note(&format!("{e:#}"));
                    return Ok(Verdict::Invalid("edit"));
                }
            }
        }
        let krate = sb.project.krate().unwrap();
        if !util::status(
            Command::new("cargo")
                .args(["check", "-q"])
                .current_dir(&krate),
            &out,
        )? {
            return Ok(Verdict::Invalid("rust"));
        }
        if let Err(e) = crate::extract::extract(&sb.project, &out) {
            out.note(&format!("{e:#}"));
            return Ok(Verdict::Invalid("extraction"));
        }
        let built = util::status(
            Command::new("lake")
                .arg("build")
                .args(targets)
                .current_dir(sb.project.proofs()),
            &out,
        )?;
        Ok(match (m.is_some(), built) {
            (false, true) => Verdict::Ok,
            (false, false) => Verdict::Invalid("baseline does not build"),
            (true, true) => Verdict::Survived,
            (true, false) => Verdict::Caught,
        })
    })()?;
    let kept = if keep { Some(sb._tmp.keep()) } else { None };
    Ok((verdict, kept))
}

pub struct Options {
    pub jobs: usize,
    pub keep: bool,
    pub baseline: bool,
}

/// One mutant's outcome.
#[derive(Debug)]
pub struct Outcome {
    pub name: String,
    pub verdict: Verdict,
    /// Generated rather than declared in the manifest.
    pub generated: bool,
}

/// Run `mutants` against `p`, printing one line each.
pub fn run(p: &Project, mutants: Vec<Mutant>, opts: &Options) -> Result<Vec<Outcome>> {
    let targets = &p.manifest.mutate.targets;
    if opts.baseline {
        eprint!("{:<48} ", "baseline (the unmodified kernel, copied)");
        let t = Instant::now();
        let (v, kept) = evaluate(p, None, targets, opts.keep)?;
        eprintln!("{v}  {:.0}s{}", t.elapsed().as_secs_f32(), kept_note(&kept));
        if v != Verdict::Ok {
            bail!(
                "the unmodified project does not build in a copy, so no mutant would mean anything{}",
                kept_note(&kept)
            );
        }
    }
    let total = mutants.len();
    let queue = Arc::new(Mutex::new(
        mutants.into_iter().enumerate().collect::<VecDeque<_>>(),
    ));
    let results = Arc::new(Mutex::new(Vec::new()));
    std::thread::scope(|s| {
        for _ in 0..opts.jobs.max(1) {
            let (queue, results) = (queue.clone(), results.clone());
            s.spawn(move || {
                loop {
                    let Some((i, m)) = queue.lock().unwrap().pop_front() else {
                        break;
                    };
                    let t = Instant::now();
                    let (v, kept) = match evaluate(p, Some(&m), targets, opts.keep) {
                        Ok(r) => r,
                        Err(e) => {
                            eprintln!("{:<48} error: {e:#}", m.name);
                            (Verdict::Invalid("error"), None)
                        }
                    };
                    eprintln!(
                        "[{}/{total}] {:<40} {v}  {:.0}s{}",
                        i + 1,
                        m.name,
                        t.elapsed().as_secs_f32(),
                        kept_note(&kept)
                    );
                    let o = Outcome {
                        name: m.name.clone(),
                        verdict: v,
                        generated: m.what.is_some(),
                    };
                    results.lock().unwrap().push((i, o));
                }
            });
        }
    });
    let mut r = Arc::try_unwrap(results).unwrap().into_inner().unwrap();
    r.sort_by_key(|(i, _)| *i);
    Ok(r.into_iter().map(|(_, o)| o).collect())
}

fn kept_note(kept: &Option<PathBuf>) -> String {
    kept.as_ref()
        .map(|k| format!("  ({})", k.display()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(src: &str) -> Vec<(String, String, String)> {
        let ast = syn::parse_file(src).unwrap();
        let mut v = Finder {
            src,
            lines: line_starts(src),
            func: String::new(),
            found: Vec::new(),
        };
        v.visit_file(&ast);
        v.found
            .into_iter()
            .map(|(f, _, _, _, was, new)| (f, was, new))
            .collect()
    }

    #[test]
    fn finds_operators_and_conditions() {
        let src = "fn f(a: u64, b: u64) -> bool {\n    if a <= b && !(a == 0) { true } else { false }\n}\n#[cfg(test)]\nmod t { fn g() -> bool { 1 < 2 } }\n";
        let got = found(src);
        let want = [
            ("f", "a <= b && !(a == 0)", "true"),
            ("f", "a <= b && !(a == 0)", "false"),
            ("f", "&&", "||"),
            ("f", "<=", "<"),
            ("f", "!", ""),
            ("f", "==", "!="),
        ];
        let got: Vec<(&str, &str, &str)> = got
            .iter()
            .map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str()))
            .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn splices_apply_by_offset() {
        let src = "fn f(a: u64) -> bool { a >= 1 }";
        let ast = syn::parse_file(src).unwrap();
        let mut v = Finder {
            src,
            lines: line_starts(src),
            func: String::new(),
            found: Vec::new(),
        };
        v.visit_file(&ast);
        let (_, _, start, end, was, new) = v.found[0].clone();
        let m = Mutant {
            name: "x".into(),
            what: None,
            file: "a.rs".into(),
            change: Change::Splice {
                start,
                end,
                was,
                new,
            },
        };
        assert_eq!(m.apply(src).unwrap(), "fn f(a: u64) -> bool { a > 1 }");
    }

    #[test]
    fn replacements_must_match_once() {
        let m = Mutant {
            name: "x".into(),
            what: None,
            file: "a.rs".into(),
            change: Change::Replace(vec![Edit {
                old: "a".into(),
                new: "b".into(),
            }]),
        };
        assert!(m.apply("aa").is_err());
        assert_eq!(m.apply("xa").unwrap(), "xb");
    }
}
