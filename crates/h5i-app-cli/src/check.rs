//! `h5i app check`: build the proofs and gate them. No `sorry` or
//! `native_decide` in hand-written Lean, no `axiom` declaration anywhere, and
//! every theorem of every hand-written module (plus the manifest's named
//! theorems) depends on Lean's three standard axioms and nothing else.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::manifest::Project;
use crate::util::{self, Out};

/// The axioms a gated theorem may use.
pub const STANDARD_AXIOMS: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];

/// `refetch` discards the Lake packages and fetches them again
/// ([`crate::packages`]). `extracted`: `proofs/generated/` was just written
/// by `extract` (as `prove` does), so an `[authz]` project need not
/// re-extract it to know it is fresh.
pub fn check(p: &Project, refetch: bool, extracted: bool, out: &Out) -> Result<()> {
    let mut record = crate::report::start(p, "check", p.manifest.check.targets.clone())?;
    let result = check_recorded(p, refetch, extracted, out, &mut record);
    if let Err(e) = &result {
        record.run.error = Some(format!("{e:#}"));
    }
    record.finish(if result.is_ok() { "passed" } else { "failed" })?;
    result
}

fn check_recorded(
    p: &Project,
    refetch: bool,
    extracted: bool,
    out: &Out,
    record: &mut h5i_app_report::Recorder,
) -> Result<()> {
    let proofs = p.proofs();
    if !proofs.join("lakefile.lean").is_file() && !proofs.join("lakefile.toml").is_file() {
        bail!("{} is not a Lake project", proofs.display());
    }
    // What this run requires, recorded before anything can fail, so the
    // web view never shows an older run's list as this one's.
    let authz = authz_theorems(p)?;
    let obligations = crate::obligations::of(p)?;
    let mut theorems = p.manifest.check.theorems.clone();
    theorems.extend(authz.iter().cloned());
    for o in &obligations {
        if let Some(t) = &o.theorem
            && !theorems.contains(t)
        {
            theorems.push(t.clone());
        }
    }
    record.run.required_theorems = theorems.clone();
    record.save()?;
    // `[authz]`: the gated theorems are only about the kernel if
    // `proofs/generated/` is what it extracts to. The gate checks names and
    // axioms, not statements, so a hand-edited `Authz.lean` (`theorem
    // ….authorized : True`), `AuthzSpecs.lean` or extracted kernel would
    // otherwise pass. Re-extract and compare, as `extract --check` does.
    if !authz.is_empty() && !extracted {
        crate::report::step(record, "extraction_fresh", out, |log| {
            crate::extract::check_fresh(p, log).context(
                "with `[authz]`, `check` re-extracts the kernel (Charon and Aeneas, see `h5i app doctor`) \
                 to confirm proofs/generated/ is what it extracts to; `h5i app prove` extracts and checks",
            )
        })?;
    }
    // Held through the build, so another `h5i app` cannot discard the
    // packages under it.
    let _packages = crate::report::step(record, "packages", out, |log| {
        crate::packages::prepare(&proofs, refetch, log)
    })?;
    let mut build = Command::new("lake");
    build
        .arg("build")
        .args(&p.manifest.check.targets)
        .current_dir(&proofs);
    crate::report::step(record, "lean_build", out, |log| util::run(&mut build, log))?;

    // `[authz]`: build the generated module itself, so the gate below never
    // reads an `Authz.olean` left from an earlier build (a lakefile whose
    // default roots miss it would otherwise pass on a stale one).
    if !authz.is_empty() {
        let mut build = Command::new("lake");
        build.args(["build", crate::extract::AUTHZ_MODULE]).current_dir(&proofs);
        crate::report::step(record, "authz_build", out, |log| {
            util::run(&mut build, log).with_context(|| {
                format!(
                    "building the generated `{m}` module failed. If lake says unknown target `{m}`, add \
                     `{m}` (with `AuthzSpecs` and `Schema`) to a `lean_lib`'s roots in {}/lakefile.lean",
                    proofs.display(),
                    m = crate::extract::AUTHZ_MODULE
                )
            })
        })?;
    }
    // The obligations' statements are elaborated in the gate against the
    // library's definitions, so build those (a project's own build need not
    // reach `H5iAppLib.Obligation`).
    if !obligations.is_empty() {
        let mut build = Command::new("lake");
        build.args(["build", OBLIGATION_MODULE]).current_dir(&proofs);
        crate::report::step(record, "obligation_library", out, |log| util::run(&mut build, log))?;
    }
    let mut problems = scan(&proofs)?;
    if !is_framework_project(p) {
        problems.extend(scan_extensions(&proofs)?);
        problems.extend(scan_generated_extensions(&proofs)?);
        problems.extend(crate::extract::stale_spec_copies(p)?);
    }
    problems.extend(crate::deps::framework_problems(p)?);
    let (mut built, unbuilt) = hand_written_modules(&proofs, &p.manifest.check.targets)?;
    record.run.unbuilt_modules = unbuilt.clone();
    for m in &unbuilt {
        out.note(&format!(
            "note: {m} is not built by `lake build`, so its theorems are not gated"
        ));
    }
    // `[authz]`: the generated module is gated like a hand-written one, and
    // each guarded function's theorem must exist.
    if !authz.is_empty() {
        built.push(crate::extract::AUTHZ_MODULE.to_string());
    }
    let theorems = &theorems;
    let checks: Vec<(String, String, String)> = obligations
        .iter()
        .filter_map(|o| o.theorem.as_ref().map(|t| (o.name.to_string(), t.clone(), o.statement.clone())))
        .collect();
    let mut results: Vec<h5i_app_report::ObligationResult> = Vec::new();
    // Every marker the gate and the automation print carries this number.
    let nonce = nonce();
    // The framework and the generated Lean: what statements are elaborated
    // in and what automation runs on, never a hand-written module.
    let clean = generated_modules(&proofs, p);
    let statement_errors = if checks.is_empty() {
        Default::default()
    } else {
        crate::report::step(record, "obligation_statements", out, |log| statements(&proofs, &clean, &checks, &nonce, log))?
    };
    if built.is_empty() && theorems.is_empty() {
        out.note("note: no hand-written modules built; nothing to gate");
    } else {
        let (issues, declarations, mut outcomes) = crate::report::step(record, "axiom_gate", out, |log| {
            gate(&proofs, &built, theorems, &checks, &nonce, log)
        })?;
        for g in &mut outcomes {
            if let Some(e) = statement_errors.get(&g.name) {
                g.status = "error".into();
                g.message = e.clone();
            }
        }
        problems.extend(issues);
        problems.extend(storage_problems(p, &declarations, out)?);
        for o in obligations.iter().filter(|o| o.theorem.is_some()) {
            let r = obligation_result(o, outcomes.iter().find(|g| g.name == o.name));
            if r.status == "failed" {
                problems.push(r.note.clone().unwrap_or_default());
            }
            results.push(r);
        }
        record.run.declarations = declarations;
    }
    // Obligations nobody named a theorem for: try the automation, in a Lean
    // run of its own whose failures only leave them open.
    let tries: Vec<&crate::obligations::Obligation> =
        obligations.iter().filter(|o| o.theorem.is_none() && o.auto.is_some()).collect();
    let mut proven = std::collections::BTreeMap::new();
    let tried = !tries.is_empty() && problems.is_empty();
    if tried {
        proven = crate::report::step(record, "obligation_automation", out, |log| automation(&proofs, &clean, &tries, &nonce, log))?;
    }
    for o in obligations.iter().filter(|o| o.theorem.is_none()) {
        let status = match proven.get(o.name) {
            Some(true) => "automation",
            Some(false) => "open",
            None if o.auto.is_some() && !tried => "not_run",
            None => "open",
        };
        results.push(h5i_app_report::ObligationResult {
            name: o.name.to_string(),
            statement: o.statement.clone(),
            status: status.into(),
            theorem: None,
            note: o.note.clone(),
        });
    }
    for r in &results {
        out.note(&format!(
            "obligation {}: {}{}",
            r.name,
            r.status,
            r.theorem.as_ref().map(|t| format!(" ({t})")).unwrap_or_default()
        ));
    }
    // The tenant: `tenant-of-token`'s status and the server's linkage to its
    // principal function (a source check), for apps with a kernel.
    if p.krate().is_some() {
        let status = results.iter().find(|r| r.name == crate::obligations::TENANT_OF_TOKEN).map(|r| r.status.as_str());
        let t = crate::tenant::link(p, status, &crate::tenant::linkage(p));
        if p.principal_fn().is_some() {
            out.note(&format!("tenant: {}: {}", t.label(), t.summary));
        }
        record.run.tenant = Some(t);
    }
    record.run.obligations = results;
    if !problems.is_empty() {
        bail!(
            "{}: proof gate failed:\n  {}",
            p.display(),
            problems.join("\n  ")
        );
    }
    // Only a passed gate establishes what the coverage line claims.
    if let Some(a) = crate::lint::authz_coverage(p) {
        record.run.authorization = a;
    }
    Ok(())
}

/// `<kernel>.<fn>.authorized` and `.kernel_apply_eq` for each `[authz]
/// guarded` function, `.read_only_sound`, and the storage theorems (or
/// `[authz] storage`).
pub fn authz_theorems(p: &Project) -> Result<Vec<String>> {
    let guarded = &p.manifest.authz.guarded;
    if guarded.is_empty() {
        return Ok(Vec::new());
    }
    let ns = kernel_ns(p)?;
    let mut out: Vec<String> = guarded
        .iter()
        .flat_map(|g| [format!("{ns}.{g}.authorized"), format!("{ns}.{g}.kernel_apply_eq")])
        .collect();
    // `Authz<T>` declares the view's read-only commands, which the engine
    // runs without locks on the strength of `ReadOnly.Sound`.
    out.extend(guarded.iter().map(|g| format!("{ns}.{g}.read_only_sound")));
    // A4 for the app: the generated storage theorems, or the one the app
    // names. Never silently neither.
    match p.storage_theorem() {
        Some(t) => out.push(t.to_string()),
        None => out.extend(crate::extract::STORAGE_THEOREMS.iter().map(|t| format!("{ns}.{t}"))),
    }
    Ok(out)
}

/// `[authz] storage` names the app's own A4 theorem: it must be a theorem of
/// the kernel's namespace (not a library fact or a type). Its statement is
/// checked against `H5iAppLib.Obligation.Storage` with the other obligations,
/// and printed.
fn storage_problems(p: &Project, decls: &[h5i_app_report::Declaration], out: &Out) -> Result<Vec<String>> {
    let Some(name) = p.storage_theorem() else { return Ok(Vec::new()) };
    if p.manifest.authz.guarded.is_empty() {
        return Ok(Vec::new());
    }
    let ns = kernel_ns(p)?;
    let Some(d) = decls.iter().find(|d| d.name == name) else {
        return Ok(vec![format!("[authz] storage: {name} was not found among the gated declarations")]);
    };
    let mut bad = Vec::new();
    if d.kind != "theorem" {
        bad.push(format!("[authz] storage: {name} is a {}, not a theorem", d.kind));
    }
    if !name.starts_with(&format!("{ns}.")) {
        bad.push(format!("[authz] storage: {name} is not in the kernel's namespace `{ns}`"));
    }
    out.note(&format!("[authz] storage, to review: {name} : {}", d.signature));
    Ok(bad)
}

/// The kernel's Lean namespace: its crate's name, `-` as `_`.
pub(crate) fn kernel_ns(p: &Project) -> Result<String> {
    let Some(ex) = &p.manifest.extract else {
        bail!("{}: `[authz] guarded` needs an [extract] section", p.display());
    };
    let krate = p.root.join(&ex.krate);
    let meta = util::metadata(&krate, false)?;
    Ok(util::ident(&meta.package_at(&krate)?.name))
}

/// A hand-written proof states `ReadOnly.Sound` in code, not in a comment.
pub fn has_read_only_proof(p: &Project) -> bool {
    std::fs::read_dir(p.proofs()).into_iter().flatten().flatten().any(|e| {
        e.path().extension().is_some_and(|x| x == "lean")
            && lean_code(&std::fs::read_to_string(e.path()).unwrap_or_default()).contains("ReadOnly.Sound")
    })
}

/// Lean source without its comments (`--` to the end of a line, `/- … -/`).
pub fn lean_code(text: &str) -> String {
    static BLOCK: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?s)/-.*?-/").unwrap());
    BLOCK
        .replace_all(text, "")
        .lines()
        .map(|l| l.split("--").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

fn lean_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name();
        if p.is_dir() {
            if !matches!(name.to_string_lossy().as_ref(), ".lake" | "target" | ".git") {
                lean_files(&p, out);
            }
        } else if p.extension().is_some_and(|x| x == "lean") && name != "lakefile.lean" {
            out.push(p);
        }
    }
}

fn is_generated(proofs: &Path, f: &Path) -> bool {
    f.strip_prefix(proofs)
        .is_ok_and(|r| r.starts_with("generated"))
}

/// Textual checks. Generated Lean may hold `sorry` (Aeneas marks what it
/// cannot translate), which the axiom gate then rejects in any theorem that
/// reaches it; no file may declare an axiom.
pub fn scan(proofs: &Path) -> Result<Vec<String>> {
    let mut files = Vec::new();
    lean_files(proofs, &mut files);
    files.sort();
    let mut bad = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f)?;
        let generated = is_generated(proofs, &f);
        let rel = f.strip_prefix(proofs).unwrap_or(&f).display().to_string();
        for (i, line) in text.lines().enumerate() {
            let code = line.split("--").next().unwrap_or("");
            let word = |w: &str| {
                code.split(|c: char| !c.is_alphanumeric() && c != '_')
                    .any(|t| t == w)
            };
            if line.starts_with("axiom ") {
                bad.push(format!(
                    "{rel}:{}: declares an axiom: {}",
                    i + 1,
                    line.trim()
                ));
            } else if !generated && (word("sorry") || word("native_decide")) {
                bad.push(format!("{rel}:{}: {}", i + 1, line.trim()));
            }
        }
    }
    Ok(bad)
}

/// `text` with comments and string literals blanked out, line breaks kept,
/// so a line number in the result is the line number in `text`.
pub fn blank_comments_and_strings(text: &str) -> String {
    let b: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let blank = |c: char| if c == '\n' { '\n' } else { ' ' };
    let mut i = 0;
    while i < b.len() {
        if b[i] == '-' && b.get(i + 1) == Some(&'-') {
            while i < b.len() && b[i] != '\n' {
                out.push(' ');
                i += 1;
            }
        } else if b[i] == '/' && b.get(i + 1) == Some(&'-') {
            // Block comments nest.
            let mut depth = 0;
            while i < b.len() {
                if b[i] == '/' && b.get(i + 1) == Some(&'-') {
                    depth += 1;
                    out.push_str("  ");
                    i += 2;
                } else if b[i] == '-' && b.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    out.push_str("  ");
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    out.push(blank(b[i]));
                    i += 1;
                }
            }
        } else if b[i] == '"' {
            out.push('"');
            i += 1;
            while i < b.len() && b[i] != '"' {
                if b[i] == '\\' && i + 1 < b.len() {
                    out.push(' ');
                    i += 1;
                }
                out.push(blank(b[i]));
                i += 1;
            }
            if i < b.len() {
                out.push('"');
                i += 1;
            }
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// Whether `p` is one of the framework's own crates (`crates/h5i-app-*`
/// next to this CLI), whose Lean libraries define tactics, attributes and
/// commands (`H5iAppLib`, `AuthzSpecs`). Every other project's hand-written
/// modules go through [`scan_extensions`].
pub fn is_framework_project(p: &Project) -> bool {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let (Ok(crates), Ok(root)) = (crates.canonicalize(), p.root.canonicalize()) else { return false };
    root.parent() == Some(crates.as_path()) && root.file_name().is_some_and(|n| n.to_string_lossy().starts_with("h5i-app-"))
}

/// Constructs a hand-written module may not use, because they run code at
/// import or change how later Lean text elaborates (`check`'s gate program
/// and the obligation statements included): `initialize`, `implemented_by`,
/// `extern`, `unsafe`, term syntax (`notation`, `infix`, `prefix`, ...), and
/// `macro_rules`, `syntax`, `macro`, `elab`, `elab_rules` outside the `tactic`
/// category. Tactic macros only build proofs the kernel checks, and are
/// allowed. Comments and strings are ignored.
pub fn scan_extensions(proofs: &Path) -> Result<Vec<String>> {
    let mut files = Vec::new();
    lean_files(proofs, &mut files);
    files.sort();
    let mut bad = Vec::new();
    for f in files {
        if is_generated(proofs, &f) {
            continue;
        }
        let rel = f.strip_prefix(proofs).unwrap_or(&f).display().to_string();
        bad.extend(extension_problems(&rel, &std::fs::read_to_string(&f)?));
    }
    Ok(bad)
}

/// [`scan_extensions`] for `proofs/generated/`: the extraction, the schema's
/// Lean and `Authz.lean`, which the obligation statements are elaborated
/// against. The two spec copies define the framework's commands and are
/// compared with their source instead (`extract::stale_spec_copies`).
pub fn scan_generated_extensions(proofs: &Path) -> Result<Vec<String>> {
    let mut files = Vec::new();
    lean_files(&proofs.join("generated"), &mut files);
    files.sort();
    let mut bad = Vec::new();
    for f in files {
        let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if name == "StdSpecs.lean" || name == "AuthzSpecs.lean" {
            continue;
        }
        let rel = f.strip_prefix(proofs).unwrap_or(&f).display().to_string();
        bad.extend(extension_problems(&rel, &std::fs::read_to_string(&f)?));
    }
    Ok(bad)
}

/// [`scan_extensions`] on one file's text; `rel` names it in messages.
pub fn extension_problems(rel: &str, text: &str) -> Vec<String> {
    const ANYWHERE: [&str; 5] = ["initialize", "builtin_initialize", "implemented_by", "extern", "unsafe"];
    const TERM_SYNTAX: [&str; 8] = ["notation", "notation3", "infix", "infixl", "infixr", "prefix", "postfix", "binder_predicate"];
    const MACROS: [&str; 5] = ["macro_rules", "syntax", "macro", "elab", "elab_rules"];
    let code = blank_comments_and_strings(text);
    let lines: Vec<&str> = code.lines().collect();
    let mut bad = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let n = i + 1;
        let mut words = line.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '\'')).filter(|w| !w.is_empty());
        if let Some(w) = words.find(|w| ANYWHERE.contains(w)) {
            bad.push(format!("{rel}:{n}: `{w}` is not allowed in a hand-written module (it runs code or replaces it): {}", line.trim()));
            continue;
        }
        // A command starts in column 0; its keyword comes after attributes
        // and modifiers.
        if !line.starts_with(|c: char| !c.is_whitespace()) {
            continue;
        }
        let mut rest = line.trim_start();
        // `open Lean in macro_rules …`, `set_option … in notation …`.
        while (rest.starts_with("open ") || rest.starts_with("set_option ")) && rest.contains(" in ") {
            rest = rest.split_once(" in ").map_or("", |(_, r)| r).trim_start();
        }
        while rest.starts_with("@[") {
            match rest.find(']') {
                Some(e) => rest = rest[e + 1..].trim_start(),
                None => break,
            }
        }
        let mut cw = rest.split_whitespace();
        let mut kw = cw.next().unwrap_or("");
        while matches!(kw, "local" | "scoped" | "private" | "protected") {
            kw = cw.next().unwrap_or("");
        }
        // `macro:max`, `syntax:arg`, `elab (name := x)`.
        let kw = kw.split([':', '(']).next().unwrap_or("");
        if TERM_SYNTAX.contains(&kw) {
            bad.push(format!(
                "{rel}:{n}: `{kw}` is not allowed in a hand-written module: term syntax changes how later Lean, the \
                 obligation statements included, elaborates"
            ));
            continue;
        }
        if !MACROS.contains(&kw) {
            continue;
        }
        // The whole command: up to the next line starting in column 0.
        let mut cmd = rest.to_string();
        for l in lines.iter().skip(i + 1) {
            if l.starts_with(|c: char| !c.is_whitespace()) {
                break;
            }
            cmd.push('\n');
            cmd.push_str(l);
        }
        let tactic = if kw == "macro_rules" {
            // Every quotation, pattern and result, is a tactic one.
            let quotes: Vec<&str> = cmd.split("`(").skip(1).collect();
            !quotes.is_empty() && quotes.iter().all(|q| q.starts_with("tactic|"))
        } else {
            // `macro "run" : tactic => …`, `syntax "x" : tactic`, `elab_rules : tactic`.
            let head = cmd.split("=>").next().unwrap_or(&cmd);
            head.rsplit(':').next().is_some_and(|c| c.split_whitespace().next() == Some("tactic"))
        };
        if !tactic {
            bad.push(format!(
                "{rel}:{n}: `{kw}` outside the `tactic` category is not allowed in a hand-written module: it changes \
                 how later Lean, the obligation statements included, elaborates (tactic macros are allowed)"
            ));
        }
    }
    bad
}

/// The hand-written modules, split into those this `lake build` built and
/// the rest. Built means in the import closure of the roots it builds (the
/// default `lean_lib`s, or the requested targets) and compiled: an `.olean`
/// left over from another target or an old root is not evidence.
fn hand_written_modules(proofs: &Path, targets: &[String]) -> Result<(Vec<String>, Vec<String>)> {
    let mut files = Vec::new();
    lean_files(proofs, &mut files);
    files.sort();
    let mut modules = std::collections::BTreeMap::new();
    for f in files {
        if is_generated(proofs, &f) {
            continue;
        }
        let rel = f.strip_prefix(proofs)?.with_extension("");
        let module = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(".");
        modules.insert(module, rel);
    }
    let roots = build_roots(proofs, targets);
    // Follow imports from the roots, through hand-written modules only.
    let mut reached = std::collections::BTreeSet::new();
    let mut stack: Vec<String> = roots
        .into_iter()
        .filter(|r| modules.contains_key(r))
        .collect();
    while let Some(m) = stack.pop() {
        if !reached.insert(m.clone()) {
            continue;
        }
        let text = std::fs::read_to_string(proofs.join(&modules[&m]).with_extension("lean"))
            .unwrap_or_default();
        for i in imports(&text) {
            if modules.contains_key(&i) && !reached.contains(&i) {
                stack.push(i);
            }
        }
    }
    let (mut built, mut unbuilt) = (Vec::new(), Vec::new());
    for (module, rel) in &modules {
        let olean = proofs
            .join(".lake/build/lib/lean")
            .join(rel)
            .with_extension("olean");
        if reached.contains(module) && olean.is_file() {
            built.push(module.clone())
        } else {
            unbuilt.push(module.clone())
        }
    }
    Ok((built, unbuilt))
}

/// The modules a file imports.
fn imports(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let l = line.trim_start();
        let rest = [
            "import ",
            "public import ",
            "meta import ",
            "public meta import ",
        ]
        .iter()
        .find_map(|k| l.strip_prefix(k));
        if let Some(rest) = rest {
            let rest = rest.split("--").next().unwrap_or("");
            out.extend(
                rest.split_whitespace()
                    .filter(|w| *w != "all")
                    .map(str::to_string),
            );
        }
    }
    out
}

/// The root modules `lake build <targets>` compiles: a target that names a
/// `lean_lib` stands for its roots, any other for the module of that name;
/// no targets means the default `lean_lib`s.
fn build_roots(proofs: &Path, targets: &[String]) -> Vec<String> {
    let libs = lean_libs(proofs);
    if targets.is_empty() {
        return libs
            .into_iter()
            .filter(|l| l.default)
            .flat_map(|l| l.roots)
            .collect();
    }
    targets
        .iter()
        .flat_map(|t| match libs.iter().find(|l| &l.name == t) {
            Some(l) => l.roots.clone(),
            None => vec![t.clone()],
        })
        .collect()
}

struct LeanLib {
    name: String,
    roots: Vec<String>,
    default: bool,
}

/// The `lean_lib`s of a `lakefile.lean` (or `lakefile.toml`).
fn lean_libs(proofs: &Path) -> Vec<LeanLib> {
    if let Ok(text) = std::fs::read_to_string(proofs.join("lakefile.toml")) {
        let Ok(doc) = text.parse::<toml::Table>() else {
            return Vec::new();
        };
        let defaults: Vec<String> = doc
            .get("defaultTargets")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let libs = doc
            .get("lean_lib")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        return libs
            .iter()
            .filter_map(|l| {
                let name = l.get("name")?.as_str()?.to_string();
                let roots = l
                    .get("roots")
                    .and_then(|r| r.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_else(|| vec![name.clone()]);
                Some(LeanLib {
                    default: defaults.contains(&name),
                    name,
                    roots,
                })
            })
            .collect();
    }
    let Ok(text) = std::fs::read_to_string(proofs.join("lakefile.lean")) else {
        return Vec::new();
    };
    let mut libs = Vec::new();
    let mut rest = text.as_str();
    while let Some(i) = rest.find("lean_lib ") {
        let line_start = rest[..i].rfind('\n').map_or(0, |n| n + 1);
        let default = rest[line_start..i].contains("default_target");
        let after = &rest[i + "lean_lib ".len()..];
        let name: String = after
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
            .collect();
        // The declaration runs to the next top-level one.
        let end = [
            "\nlean_lib ",
            "\n@[",
            "\nlean_exe ",
            "\nrequire ",
            "\npackage ",
            "\ntarget ",
        ]
        .iter()
        .filter_map(|k| after.find(k))
        .min()
        .unwrap_or(after.len());
        let body = &after[..end];
        let roots = match body
            .find("roots")
            .and_then(|r| body[r..].find("#[").map(|o| r + o + 2))
        {
            Some(open) => {
                let close = body[open..].find(']').map_or(body.len(), |c| open + c);
                body[open..close]
                    .split(',')
                    .map(|r| r.trim().trim_start_matches('`').to_string())
                    .filter(|r| !r.is_empty())
                    .collect()
            }
            None => vec![name.clone()],
        };
        libs.push(LeanLib {
            name,
            roots,
            default,
        });
        rest = &after[end..];
    }
    libs
}

fn lean_name(n: &str) -> String {
    // Names with characters Lean's backtick syntax does not take (`select_sound'`)
    // need «» around each such component.
    n.split('.')
        .map(|c| {
            if c.chars().all(|ch| ch.is_alphanumeric() || ch == '_') {
                c.to_string()
            } else {
                format!("«{c}»")
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

const MARK: &str = "H5I-GATE";

/// The library module defining the obligations.
pub const OBLIGATION_MODULE: &str = "H5iAppLib.Obligation";

/// `s` as a Lean string literal.
fn lean_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// What the gate found for one obligation's theorem.
#[derive(Debug, serde::Deserialize)]
struct GateObligation {
    name: String,
    /// `ok`, `mismatch`, `undetermined`, `missing` or `error`.
    status: String,
    #[serde(default)]
    expected: String,
    #[serde(default)]
    stated: String,
    #[serde(default)]
    message: String,
}

/// The gate's verdict on an obligation with a theorem, as recorded.
fn obligation_result(o: &crate::obligations::Obligation, g: Option<&GateObligation>) -> h5i_app_report::ObligationResult {
    let thm = o.theorem.clone().unwrap_or_default();
    let (status, note) = match g {
        Some(g) if g.status == "ok" => {
            // The resolved statement, so the app's choices (`_`) are reviewed.
            let shown = if o.statement.contains(" _") { format!("resolved to: {}", g.expected) } else { String::new() };
            let note = [o.note.clone().unwrap_or_default(), shown].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("; ");
            ("proven", (!note.is_empty()).then_some(note))
        }
        Some(g) if g.status == "mismatch" => (
            "failed",
            Some(format!(
                "obligation `{}`: {thm} does not state it. Expected (H5iAppLib.Obligation, for this app):\n      {}\n    \
                 {thm} states:\n      {}",
                o.name, g.expected, g.stated
            )),
        ),
        Some(g) if g.status == "undetermined" => (
            "failed",
            Some(format!(
                "obligation `{}`: {thm} leaves the app's part of the statement undetermined: {}",
                o.name, g.expected
            )),
        ),
        Some(g) if g.status == "missing" => {
            ("failed", Some(format!("obligation `{}`: theorem {thm} does not exist", o.name)))
        }
        Some(g) => (
            "failed",
            Some(format!("obligation `{}`: the statement `{}` did not elaborate: {}", o.name, o.statement, g.message)),
        ),
        None => ("failed", Some(format!("obligation `{}`: the gate did not report on {thm}", o.name))),
    };
    h5i_app_report::ObligationResult {
        name: o.name.to_string(),
        statement: o.statement.clone(),
        status: status.into(),
        theorem: o.theorem.clone(),
        note,
    }
}

/// Modules of `proofs/generated/` this build compiled, and `Authz` for `[authz]`.
pub(crate) fn generated_modules(proofs: &Path, p: &Project) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(proofs.join("generated")) else { return out };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            (path.extension()? == "lean").then(|| path.file_stem().unwrap().to_string_lossy().into_owned())
        })
        .collect();
    names.sort();
    for m in names {
        let olean = proofs.join(".lake/build/lib/lean").join(format!("{m}.olean"));
        if olean.is_file() && (m != crate::extract::AUTHZ_MODULE || !p.manifest.authz.guarded.is_empty()) {
            out.push(m);
        }
    }
    out
}

/// Try each obligation's `auto` proof in `Auto.lean`. A failed tactic is an
/// error in that file only; the obligation is proven by automation only if
/// its theorem exists and uses the standard axioms (no `sorryAx` from error
/// recovery). `modules` are the generated ones: like the statements, the
/// automation never sees a hand-written module, and its markers carry the
/// run's nonce. Returns, per obligation, whether it was proven.
pub(crate) fn automation(
    proofs: &Path,
    modules: &[String],
    tries: &[&crate::obligations::Obligation],
    nonce: &str,
    out: &Out,
) -> Result<std::collections::BTreeMap<&'static str, bool>> {
    let mut s = String::from("import Lean\n");
    for m in modules.iter().map(String::as_str).chain([OBLIGATION_MODULE]) {
        s.push_str(&format!("import {m}\n"));
    }
    s.push_str("open Aeneas Aeneas.Std Result\nset_option maxHeartbeats 1000000\n\nnamespace H5iAuto\n");
    let ident = |n: &str| n.replace('-', "_");
    for o in tries {
        s.push_str(&format!("\ntheorem {} : {} :=\n  {}\n", ident(o.name), o.statement, o.auto.as_deref().unwrap_or("sorry")));
    }
    let names = tries.iter().map(|o| format!("`H5iAuto.{}", ident(o.name))).collect::<Vec<_>>().join(", ");
    let std = STANDARD_AXIOMS.iter().map(|a| format!("`{a}")).collect::<Vec<_>>().join(", ");
    s.push_str(&format!(
        "\nend H5iAuto\n\nopen Lean Elab Command in\n#eval show CommandElabM Unit from do\n  let std : List Name := [{std}]\n  \
         for n in ([{names}] : List Name) do\n    if (← getEnv).contains n then\n      let axs ← collectAxioms n\n      \
         let ok := axs.all (fun a => std.contains a)\n      IO.println s!\"H5I-AUTO-{nonce} {{n}} {{if ok then \"proven\" else \"open\"}}\"\n    \
         else\n      IO.println s!\"H5I-AUTO-{nonce} {{n}} open\"\n"
    ));
    let dir = proofs.join(".lake/h5i");
    std::fs::create_dir_all(&dir)?;
    let file = dir.join("Auto.lean");
    std::fs::write(&file, s)?;
    out.note(&format!("$ lake env lean {}", file.display()));
    let text = run_with_timeout(Command::new("lake").args(["env", "lean"]).arg(&file).current_dir(proofs), AUTO_SECONDS)?;
    out.note(&text);
    let mut result = std::collections::BTreeMap::new();
    for o in tries {
        let want = format!("H5I-AUTO-{nonce} H5iAuto.{} proven", ident(o.name));
        result.insert(o.name, text.lines().any(|l| l.trim() == want));
    }
    Ok(result)
}

/// How long the obligation automation may run before it is stopped and
/// everything it tried is left open.
const AUTO_SECONDS: u64 = 900;

/// Run `cmd`, killing it after `secs`; its stdout and stderr either way.
fn run_with_timeout(cmd: &mut Command, secs: u64) -> Result<String> {
    use std::io::Read;
    let mut child = cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn()?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let o = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let e = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    let mut timed_out = false;
    while child.try_wait()?.is_none() {
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            timed_out = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let _ = child.wait();
    let mut text = format!("{}{}", o.join().unwrap_or_default(), e.join().unwrap_or_default());
    if timed_out {
        text.push_str(&format!("\n(stopped after {secs} s)\n"));
    }
    Ok(text)
}

/// The Lean program that imports the modules and reports every theorem whose
/// axioms go beyond the standard three, and checks each obligation's theorem
/// (`checks`: name, theorem, statement) against the statement.
fn gate_program(modules: &[String], theorems: &[String], checks: &[(String, String, String)], nonce: &str) -> String {
    let mut s = String::from("import Lean\n");
    // The statements, elaborated by `statements_program` without any
    // hand-written module; imported first, as finished declarations.
    if !checks.is_empty() {
        s.push_str(&format!("import {STATEMENTS_MODULE}\n"));
    }
    for m in modules {
        s.push_str(&format!("import {m}\n"));
    }
    let obls = checks
        .iter()
        .enumerate()
        .map(|(i, (n, t, _))| format!("({}, `{}, `{}.s{i})", lean_str(n), lean_name(t), STATEMENT_NS))
        .collect::<Vec<_>>()
        .join(",\n    ");
    let mark = format!("{MARK}-{nonce}");
    let catalog = format!("H5I-CATALOG-{nonce}");
    let obligation = format!("H5I-OBLIGATION-{nonce}");
    let mods = modules
        .iter()
        .map(|m| format!("`{}", lean_name(m)))
        .collect::<Vec<_>>()
        .join(", ");
    let thms = theorems
        .iter()
        .map(|t| format!("`{}", lean_name(t)))
        .collect::<Vec<_>>()
        .join(", ");
    let std = STANDARD_AXIOMS
        .iter()
        .map(|a| format!("`{a}"))
        .collect::<Vec<_>>()
        .join(", ");
    s.push_str(&format!(
        r#"
open Lean Elab Command in
#eval show CommandElabM Unit from do
  let env ← getEnv
  let mods : List Name := [{mods}]
  let named : List Name := [{thms}]
  let std : List Name := [{std}]
  let mut todo : Array Name := #[]
  for (n, ci) in env.constants.map₁.toList do
    if let .thmInfo _ := ci then
      if let some idx := env.getModuleIdxFor? n then
        if mods.contains (env.header.moduleNames[idx.toNat]!) && !n.isInternal then
          todo := todo.push n
  let mut out : Array String := #[]
  for n in named do
    if env.contains n then
      unless todo.contains n do todo := todo.push n
    else
      out := out.push s!"{mark} missing {{n}}"
  let mut bad := 0
  for n in todo do
    let axs ← collectAxioms n
    let extra := axs.filter (fun a => !std.contains a)
    unless extra.isEmpty do
      bad := bad + 1
      out := out.push s!"{mark} bad {{n}} {{extra.toList}}"
  -- This catalog comes from the elaborated environment, including implicit
  -- binders and definitions. It is not a source-text approximation.
  for (n, ci) in env.constants.map₁.toList do
    let localDecl := (env.getModuleIdxFor? n).any fun idx => mods.contains (env.header.moduleNames[idx.toNat]!)
    if (localDecl || named.contains n) && !n.isInternal then
      let kind := if ci.isTheorem then "theorem" else "definition"
      let signature ← liftTermElabM do
        withOptions (fun o => o.setBool `pp.explicit true |>.setBool `pp.fullNames true) do
          return (← Meta.ppExpr ci.type).pretty
      let body ← liftTermElabM do
        match ci.value? with
        | some v => if ci.isTheorem then pure Json.null else return toJson (← Meta.ppExpr v).pretty
        | none => pure Json.null
      let axs ← collectAxioms n
      let deps := ci.getUsedConstantsAsSet.toList.map Name.toString
      let row := Json.mkObj [("name", toJson n.toString), ("kind", toJson kind),
        ("signature", toJson signature), ("definition", body),
        ("dependencies", toJson deps), ("axioms", toJson (axs.toList.map Name.toString))]
      liftIO <| IO.println ("{catalog} " ++ row.compress)
  -- Each obligation's theorem states exactly the obligation, instantiated
  -- for this app. The statement is a declaration elaborated without any
  -- hand-written module (`{STATEMENTS_MODULE}`), its holes (the app's part)
  -- abstracted as binders. They are filled by unification here, and the
  -- verdict is the kernel's: the filled statement and the theorem's type are
  -- definitionally equal (`Kernel.isDefEq`).
  let obls : List (String × Name × Name) := [
    {obls}]
  for (slot, n, sn) in obls do
    let row ← liftTermElabM do
      let obj := fun (st : String) (fs : List (String × Json)) =>
        Json.mkObj ([("name", toJson slot), ("status", toJson st)] ++ fs)
      let ppS : Expr → Term.TermElabM String := fun e => do return (← Meta.ppExpr e).pretty
      match env.find? sn, env.find? n with
      | none, _ => return obj "error" [("message", toJson "the statement did not elaborate")]
      | _, none => return obj "missing" []
      | some sci, some ci =>
        try
          let v := sci.value!.instantiateLevelParams sci.levelParams (← sci.levelParams.mapM fun _ => Meta.mkFreshLevelMVar)
          let (_, _, expected) ← Meta.lambdaMetaTelescope v
          if ← Meta.isDefEq expected ci.type then
            let e ← instantiateMVars expected
            if e.hasMVar then
              return obj "undetermined" [("expected", toJson (← ppS e))]
            match Kernel.isDefEq env {{}} e ci.type with
            | .ok true => return obj "ok" [("expected", toJson (← ppS e))]
            | _ => return obj "mismatch" [("expected", toJson (← ppS e)), ("stated", toJson (← ppS ci.type))]
          else
            return obj "mismatch" [("expected", toJson (← ppS (← instantiateMVars expected))),
              ("stated", toJson (← ppS ci.type))]
        catch ex => return obj "error" [("message", toJson (← ex.toMessageData.toString))]
    liftIO <| IO.println ("{obligation} " ++ row.compress)
  out := out.push s!"{mark} done {{todo.size}} {{bad}}"
  logInfo (String.intercalate "\n" out.toList)
"#
    ));
    s
}

fn gate(
    proofs: &Path,
    modules: &[String],
    theorems: &[String],
    checks: &[(String, String, String)],
    nonce: &str,
    out: &Out,
) -> Result<(Vec<String>, Vec<h5i_app_report::Declaration>, Vec<GateObligation>)> {
    let dir = proofs.join(".lake/h5i");
    std::fs::create_dir_all(&dir)?;
    let file = dir.join("Gate.lean");
    std::fs::write(&file, gate_program(modules, theorems, checks, nonce))?;
    out.note(&format!("$ lake env lean {}", file.display()));
    let o = Command::new("lake")
        .args(["env", "lean"])
        .arg(&file)
        .current_dir(proofs)
        .output()?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    let (problems, declarations, obligations, total) = parse_gate(&text, nonce, o.status.success())?;
    out.note(&format!(
        "gate: {total} theorems in {} modules checked against {}",
        modules.len(),
        STANDARD_AXIOMS.join(", ")
    ));
    Ok((problems, declarations, obligations))
}

/// What the gate printed, read only from lines carrying this run's `nonce`:
/// a module the gate imports could print markers of its own, but not one
/// with a number chosen after it was built. Fails unless the process exited
/// successfully and printed its `done` line.
type GateOutput = (Vec<String>, Vec<h5i_app_report::Declaration>, Vec<GateObligation>, String);

fn parse_gate(text: &str, nonce: &str, success: bool) -> Result<GateOutput> {
    let mark = format!("{MARK}-{nonce}");
    let catalog = format!("H5I-CATALOG-{nonce} ");
    let obligation = format!("H5I-OBLIGATION-{nonce} ");
    let mut problems = Vec::new();
    let mut declarations = Vec::new();
    let mut obligations = Vec::new();
    let mut done = None;
    for line in text.lines() {
        if let Some((_, json)) = line.split_once(&catalog) {
            declarations.push(serde_json::from_str(json)?);
            continue;
        }
        if let Some((_, json)) = line.split_once(&obligation) {
            obligations.push(serde_json::from_str(json)?);
            continue;
        }
        let Some(i) = line.find(&mark) else { continue };
        let mut words = line[i + mark.len()..].trim().splitn(2, ' ');
        match (words.next(), words.next()) {
            (Some("missing"), Some(n)) => {
                problems.push(format!("theorem {n} named in h5i-app.toml does not exist"))
            }
            (Some("bad"), Some(rest)) => {
                let (n, axs) = rest.split_once(' ').unwrap_or((rest, ""));
                problems.push(format!("{n} depends on {axs}"));
            }
            (Some("done"), Some(rest)) => done = Some(rest.to_string()),
            _ => {}
        }
    }
    if !success {
        bail!("the axiom gate failed:\n{}", text.trim_end());
    }
    let Some(done) = done else {
        bail!("the axiom gate did not run (no `done` line of this run):\n{}", text.trim_end());
    };
    let total = done.split(' ').next().unwrap_or("0").to_string();
    Ok((problems, declarations, obligations, total))
}

/// The module holding the obligation statements, elaborated for the gate.
pub const STATEMENTS_MODULE: &str = "H5iGateStatements";
/// The namespace of its declarations: `s<i>` for the gate's `i`th check.
const STATEMENT_NS: &str = "H5iGateStatement";

/// A number no module the gate imports can know when it was built.
pub(crate) fn nonce() -> String {
    use std::hash::{BuildHasher, Hasher};
    let a = std::collections::hash_map::RandomState::new().build_hasher().finish();
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
    h.write_u32(std::process::id());
    format!("{a:016x}{:016x}", h.finish())
}

/// The Lean program that elaborates each obligation statement (`checks`:
/// name, theorem, statement) and adds it as `H5iGateStatement.s<i>`, a
/// definition whose binders are the statement's holes. `modules` are the
/// framework (`H5iAppLib.Obligation`) and the generated modules, never a
/// hand-written one, so no app macro, notation or definition changes what a
/// statement means.
fn statements_program(modules: &[String], checks: &[(String, String, String)], nonce: &str) -> String {
    let mut s = String::from("import Lean\n");
    for m in modules {
        s.push_str(&format!("import {m}\n"));
    }
    let srcs = checks
        .iter()
        .enumerate()
        .map(|(i, (n, _, st))| format!("({}, `{STATEMENT_NS}.s{i}, {})", lean_str(n), lean_str(st)))
        .collect::<Vec<_>>()
        .join(",\n    ");
    s.push_str(&format!(
        r#"
open Lean Elab Command Meta in
#eval show CommandElabM Unit from do
  let srcs : List (String × Name × String) := [
    {srcs}]
  for (slot, n, src) in srcs do
    match Parser.runParserCategory (← getEnv) `term src with
    | .error e => liftIO <| IO.println s!"H5I-STATEMENT-{nonce} {{slot}} error {{e}}"
    | .ok stx =>
      try
        liftTermElabM do
          let e ← Term.withoutErrToSorry (Term.elabType stx)
          Term.synthesizeSyntheticMVarsNoPostponing
          let r ← abstractMVars (← instantiateMVars e)
          let ty ← instantiateMVars (← inferType r.expr)
          addDecl <| .defnDecl (mkDefinitionValEx n r.paramNames.toList ty r.expr .abbrev .safe [n])
        liftIO <| IO.println s!"H5I-STATEMENT-{nonce} {{slot}} ok"
      catch ex => liftIO <| IO.println s!"H5I-STATEMENT-{nonce} {{slot}} error {{(← ex.toMessageData.toString)}}"
"#
    ));
    s
}

/// Elaborate the statements (`statements_program`) and compile them to
/// `.lake/build/lib/lean/H5iGateStatements.olean`, where the gate imports
/// them. Returns, per obligation name, the elaboration error if any.
fn statements(
    proofs: &Path,
    modules: &[String],
    checks: &[(String, String, String)],
    nonce: &str,
    out: &Out,
) -> Result<std::collections::BTreeMap<String, String>> {
    let dir = proofs.join(".lake/h5i/statements");
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(format!("{STATEMENTS_MODULE}.lean"));
    let imports: Vec<String> = std::iter::once(OBLIGATION_MODULE.to_string()).chain(modules.iter().cloned()).collect();
    std::fs::write(&file, statements_program(&imports, checks, nonce))?;
    let olean = proofs.join(".lake/build/lib/lean").join(format!("{STATEMENTS_MODULE}.olean"));
    std::fs::create_dir_all(olean.parent().unwrap())?;
    let _ = std::fs::remove_file(&olean);
    let mut cmd = Command::new("lake");
    cmd.args(["env", "lean", "-R"]).arg(&dir).arg("-o").arg(&olean).arg(&file).current_dir(proofs);
    out.note(&format!("$ {}", util::show(&cmd)));
    let o = cmd.output()?;
    let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    out.note(&text);
    if !o.status.success() {
        bail!("elaborating the obligation statements failed:\n{}", text.trim_end());
    }
    let tag = format!("H5I-STATEMENT-{nonce} ");
    let mut errors = std::collections::BTreeMap::new();
    let mut seen = 0;
    for line in text.lines() {
        let Some((_, rest)) = line.split_once(&tag) else { continue };
        seen += 1;
        let (slot, verdict) = rest.split_once(' ').unwrap_or((rest, ""));
        if let Some(e) = verdict.strip_prefix("error ") {
            errors.insert(slot.to_string(), e.to_string());
        }
    }
    if seen != checks.len() {
        bail!("elaborating the obligation statements: {seen} of {} reported:\n{}", checks.len(), text.trim_end());
    }
    Ok(errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires the pinned Lean toolchain; run explicitly for catalog changes"]
    fn catalog_uses_real_elaborated_hypotheses_and_dependencies() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("Fixture.lean"), "namespace Fixture\ndef Valid (n : Nat) := n > 0\ntheorem safe {n : Nat} (given : Valid n) : n > 0 := given\nend Fixture\n").unwrap();
        let compiled = Command::new("lean")
            .arg(format!("+{}", crate::pins::LEAN_TOOLCHAIN))
            .args(["-o", "Fixture.olean", "Fixture.lean"])
            .current_dir(d.path())
            .output()
            .unwrap();
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stdout)
        );
        let program = gate_program(&["Fixture".into()], &["Fixture.safe".into()], &[], "n0");
        let path = d.path().join("Catalog.lean");
        std::fs::write(&path, program).unwrap();
        let o = Command::new("lean")
            .arg(format!("+{}", crate::pins::LEAN_TOOLCHAIN))
            .arg(&path)
            .env("LEAN_PATH", d.path())
            .current_dir(d.path())
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&o.stdout);
        assert!(
            o.status.success(),
            "{text}\n{}",
            String::from_utf8_lossy(&o.stderr)
        );
        let rows: Vec<h5i_app_report::Declaration> = text
            .lines()
            .filter_map(|l| {
                l.split_once("H5I-CATALOG-n0 ")
                    .map(|(_, row)| serde_json::from_str(row).unwrap())
            })
            .collect();
        let d = rows
            .iter()
            .find(|d| d.name == "Fixture.safe")
            .unwrap_or_else(|| panic!("catalog row absent: {text}"));
        assert_eq!(d.name, "Fixture.safe");
        assert!(d.signature.contains("Fixture.Valid"), "{}", d.signature);
        assert!(d.dependencies.iter().any(|d| d == "Fixture.Valid"));
        assert!(d.definition.is_none());
        assert!(
            rows.iter()
                .any(|d| d.name == "Fixture.Valid" && d.definition.is_some())
        );
        assert!(text.contains("H5I-GATE-n0 done 1 0"));
    }

    #[test]
    #[ignore = "requires the pinned Lean toolchain; run explicitly for gate changes"]
    fn an_obligation_needs_exactly_its_statement() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("Fixture.lean"),
            "namespace Fixture\n\
             def f (n : Nat) : Nat := n + 1\n\
             def Obl (g : Nat → Nat) (P : Nat → Prop) : Prop := ∀ n, P (g n)\n\
             theorem good : Obl f (fun m => 0 < m) := fun n => Nat.succ_pos n\n\
             theorem unfolded : ∀ n, 0 < f n := fun n => Nat.succ_pos n\n\
             theorem weaker : Obl f (fun _ => True) := fun _ => trivial\n\
             def g (n : Nat) : Nat := n + 2\n\
             theorem other_fn : Obl g (fun m => 0 < m) := fun n => Nat.succ_pos (n + 1)\n\
             end Fixture\n",
        )
        .unwrap();
        let compiled = Command::new("lean")
            .arg(format!("+{}", crate::pins::LEAN_TOOLCHAIN))
            .args(["-o", "Fixture.olean", "Fixture.lean"])
            .current_dir(d.path())
            .output()
            .unwrap();
        assert!(compiled.status.success(), "{}", String::from_utf8_lossy(&compiled.stdout));
        // An app module that rewrites the statement's syntax to `True`, and
        // a theorem of `True` named for it.
        std::fs::write(
            d.path().join("Evil.lean"),
            "import Fixture\nopen Lean in\nmacro_rules\n  | `(Fixture.Obl $_f $_p) => `(True)\n\
             namespace Fixture\ntheorem evil : Fixture.Obl Fixture.f (fun m => 0 < m) := trivial\nend Fixture\n",
        )
        .unwrap();
        // An app module that prints the gate's markers when imported, and exits.
        std::fs::write(
            d.path().join("Forge.lean"),
            "import Fixture\ninitialize do\n  IO.println \"H5I-OBLIGATION {\\\"name\\\":\\\"good\\\",\\\"status\\\":\\\"ok\\\"}\"\n\
             \x20 IO.println \"H5I-GATE done 0 0\"\n  IO.Process.exit 0\n",
        )
        .unwrap();
        let lean = |args: &[&str]| {
            Command::new("lean")
                .arg(format!("+{}", crate::pins::LEAN_TOOLCHAIN))
                .args(args)
                .env("LEAN_PATH", d.path())
                .current_dir(d.path())
                .output()
                .unwrap()
        };
        for m in ["Evil", "Forge"] {
            let o = lean(&["-o", &format!("{m}.olean"), &format!("{m}.lean")]);
            assert!(o.status.success(), "{m}: {}", String::from_utf8_lossy(&o.stdout));
        }
        let stmt = "Fixture.Obl Fixture.f (fun m => 0 < m)";
        let checks: Vec<(String, String, String)> = [
            ("good", "Fixture.good", stmt),
            ("unfolded", "Fixture.unfolded", stmt),
            ("weaker", "Fixture.weaker", stmt),
            ("other_fn", "Fixture.other_fn", stmt),
            ("hole", "Fixture.weaker", "Fixture.Obl Fixture.f _"),
            ("missing", "Fixture.nothing", stmt),
            ("rewritten", "Fixture.evil", stmt),
            ("bad_statement", "Fixture.good", "Fixture.nope"),
        ]
        .iter()
        .map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string()))
        .collect();
        // The statements, without the app's modules.
        let nonce = nonce();
        std::fs::create_dir_all(d.path().join("st")).unwrap();
        std::fs::write(d.path().join("st/H5iGateStatements.lean"), statements_program(&["Fixture".into()], &checks, &nonce)).unwrap();
        let o = lean(&["-R", "st", "-o", "H5iGateStatements.olean", "st/H5iGateStatements.lean"]);
        let text = String::from_utf8_lossy(&o.stdout);
        assert!(o.status.success(), "{text}");
        assert_eq!(text.matches(&format!("H5I-STATEMENT-{nonce} ")).count(), checks.len(), "{text}");
        assert!(text.contains(&format!("H5I-STATEMENT-{nonce} bad_statement error")), "{text}");
        // The gate, with them.
        std::fs::write(d.path().join("Gate.lean"), gate_program(&["Fixture".into(), "Evil".into()], &[], &checks, &nonce)).unwrap();
        let o = lean(&["Gate.lean"]);
        let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        let (_, _, outcomes, _) = parse_gate(&text, &nonce, o.status.success()).unwrap_or_else(|e| panic!("{e:#}"));
        let got: std::collections::BTreeMap<String, String> = outcomes.into_iter().map(|g| (g.name, g.status)).collect();
        assert_eq!(got["good"], "ok");
        assert_eq!(got["unfolded"], "ok", "the statement unfolded is the same statement");
        assert_eq!(got["weaker"], "mismatch", "a weaker postcondition is another statement");
        // Definitional equality: a function with the same body would pass, and the
        // theorem would then be about the same function.
        assert_eq!(got["other_fn"], "mismatch", "the same property of another function");
        assert_eq!(got["hole"], "ok", "the app's part resolves to what the theorem chose");
        assert_eq!(got["missing"], "missing");
        assert_eq!(got["rewritten"], "mismatch", "an app macro does not change the statement");
        assert_eq!(got["bad_statement"], "error");
        // A module that prints markers at import: none carries the nonce.
        std::fs::write(d.path().join("Gate2.lean"), gate_program(&["Fixture".into(), "Forge".into()], &[], &checks, &nonce)).unwrap();
        let o = lean(&["Gate2.lean"]);
        let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        assert!(text.contains("H5I-GATE done 0 0"), "the forged line was printed: {text}");
        let e = parse_gate(&text, &nonce, o.status.success()).unwrap_err();
        assert!(format!("{e:#}").contains("did not run"), "{e:#}");
    }

    #[test]
    fn forged_markers_without_the_nonce_are_not_read() {
        let forged = "H5I-OBLIGATION {\"name\":\"panic-free\",\"status\":\"ok\"}\nH5I-GATE done 0 0\n";
        assert!(parse_gate(forged, "abc", true).is_err());
        let real = format!("{forged}H5I-OBLIGATION-abc {{\"name\":\"x\",\"status\":\"mismatch\"}}\nH5I-GATE-abc done 3 0\n");
        let (problems, _, obligations, total) = parse_gate(&real, "abc", true).unwrap();
        assert!(problems.is_empty());
        assert_eq!((obligations.len(), obligations[0].status.as_str(), total.as_str()), (1, "mismatch", "3"));
        assert!(parse_gate(&real, "abc", false).is_err(), "a failed process is a failed gate");
    }

    #[test]
    fn hand_written_modules_may_not_change_elaboration_or_run_code() {
        let rejected = [
            ("initialize do\n  IO.println \"H5I-GATE done 0 0\"\n", "initialize"),
            ("builtin_initialize foo : IO.Ref Nat ← IO.mkRef 0\n", "builtin_initialize"),
            ("@[implemented_by g] def f : Nat := 0\n", "implemented_by"),
            ("@[extern \"c_f\"] opaque f : Nat\n", "extern"),
            ("unsafe def f : Nat := 0\n", "unsafe"),
            ("open Lean in\nmacro_rules\n  | `(H5iAppLib.Obligation.PanicFree $_f) => `(True)\n", "macro_rules"),
            ("open Lean in macro_rules | `(Foo $x) => `(True)\n", "macro_rules"),
            ("notation \"PF\" => True\n", "notation"),
            ("local notation \"PF\" => True\n", "notation"),
            ("scoped infixl:65 \" +++ \" => Nat.add\n", "infixl"),
            ("prefix:max \"√\" => Nat.sqrt\n", "prefix"),
            ("postfix:max \"!\" => Nat.succ\n", "postfix"),
            ("syntax \"pf\" : term\n", "syntax"),
            ("macro \"pf\" : term => `(True)\n", "macro"),
            ("elab \"pf\" : term => return Lean.mkConst ``True\n", "elab"),
            ("elab_rules : command\n  | `(pf) => pure ()\n", "elab_rules"),
            ("@[simp] macro:max \"pf\" : term => `(True)\n", "macro"),
        ];
        for (src, what) in rejected {
            let bad = extension_problems("A.lean", src);
            assert!(bad.len() == 1 && bad[0].contains(&format!("`{what}`")) && bad[0].starts_with("A.lean:"), "{src}: {bad:?}");
        }
        // Tactic macros, as the examples and h5i-app-pgsql use them, and the
        // constructs in comments or strings, are fine.
        let allowed = [
            "macro \"run\" : tactic => `(tactic| (\n    simp only [x]\n    step*))\n",
            "elab \"bpush\" : tactic => do\n  pure ()\n",
            "macro \"emit\" x:term : tactic => `(tactic| exact $x)\n",
            "syntax \"mytac\" : tactic\nmacro_rules\n  | `(tactic| mytac) => `(tactic| simp)\n",
            "elab_rules : tactic\n  | `(tactic| mytac) => pure ()\n",
            "-- initialize, notation, unsafe in a comment\n/- macro_rules | `(x) => `(True)\n  /- nested -/ -/\ntheorem t : True := trivial\n",
            "def s : String := \"initialize notation unsafe extern\"\n",
            "theorem select_sound' : True := trivial -- unsafe\n",
        ];
        for src in allowed {
            assert_eq!(extension_problems("A.lean", src), Vec::<String>::new(), "{src}");
        }
        // Line numbers survive the blanking of a multi-line comment.
        let bad = extension_problems("B.lean", "/- a\nb\n-/\n\nnotation \"x\" => True\n");
        assert!(bad[0].starts_with("B.lean:5:"), "{bad:?}");
    }

    #[test]
    fn the_examples_hand_written_proofs_pass_the_extension_scan() {
        let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/app");
        for p in crate::manifest::Project::discover(&examples).unwrap() {
            assert!(!is_framework_project(&p), "{}", p.display());
            assert_eq!(scan_extensions(&p.proofs()).unwrap(), Vec::<String>::new(), "{}", p.display());
        }
        // h5i-app-pgsql is a framework project, but its tactic macros are
        // exactly what the rule allows.
        let pgsql = Path::new(env!("CARGO_MANIFEST_DIR")).join("../h5i-app-pgsql/proofs");
        for f in ["Render.lean", "Sound.lean"] {
            let text = std::fs::read_to_string(pgsql.join(f)).unwrap();
            assert_eq!(extension_problems(f, &text), Vec::<String>::new());
        }
        let core = Path::new(env!("CARGO_MANIFEST_DIR")).join("../h5i-app-core/h5i-app.toml");
        assert!(is_framework_project(&crate::manifest::Project::load(&core).unwrap()));
    }

    #[test]
    fn only_modules_the_build_reaches_are_gated() {
        let d = tempfile::tempdir().unwrap();
        let w = |f: &str, t: &str| {
            let p = d.path().join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, t).unwrap();
        };
        w(
            "lakefile.lean",
            "lean_lib Generated where\n  srcDir := \"generated\"\n  roots := #[`K]\n\n@[default_target] lean_lib Proofs where\n  roots := #[`Theorems]\n\nlean_exe difftest where\n  root := `DiffTest\n",
        );
        w("generated/K.lean", "");
        w("Theorems.lean", "import K\nimport Spec\n");
        w("Spec.lean", "public import Lib.A -- comment\n");
        w("Lib/A.lean", "");
        w("DiffTest.lean", "import Theorems\n");
        w("Old.lean", "");
        // Every module has an .olean, as after other builds.
        for m in ["Theorems", "Spec", "Lib/A", "DiffTest", "Old"] {
            w(&format!(".lake/build/lib/lean/{m}.olean"), "");
        }
        let (built, unbuilt) = hand_written_modules(d.path(), &[]).unwrap();
        assert_eq!(built, ["Lib.A", "Spec", "Theorems"]);
        assert_eq!(unbuilt, ["DiffTest", "Old"]);
        let (built, _) = hand_written_modules(d.path(), &["Spec".into()]).unwrap();
        assert_eq!(built, ["Lib.A", "Spec"]);
    }

    #[test]
    fn primes_are_quoted() {
        assert_eq!(
            lean_name("a.Sound.select_sound'"),
            "a.Sound.«select_sound'»"
        );
        assert_eq!(lean_name("a.b"), "a.b");
    }

    #[test]
    fn comments_are_not_proofs() {
        let t = "-- ReadOnly.Sound later\n/- ReadOnly.Sound\n  here -/\ntheorem x : True := trivial\n";
        assert!(!lean_code(t).contains("ReadOnly.Sound"));
        assert!(lean_code("theorem ro : H5iAppLib.ReadOnly.Sound r t w := h -- done").contains("ReadOnly.Sound"));
    }

    #[test]
    fn scan_flags_sorry_outside_generated_only() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("generated")).unwrap();
        std::fs::write(
            d.path().join("generated/G.lean"),
            "theorem x : True := sorry\n",
        )
        .unwrap();
        std::fs::write(
            d.path().join("A.lean"),
            "-- sorry in a comment\ntheorem y : True := by sorry\n",
        )
        .unwrap();
        std::fs::write(d.path().join("B.lean"), "axiom z : False\n").unwrap();
        let bad = scan(d.path()).unwrap();
        assert_eq!(bad.len(), 2, "{bad:?}");
        assert!(bad[0].starts_with("A.lean:2:"));
        assert!(bad[1].starts_with("B.lean:1:"));
    }
}
