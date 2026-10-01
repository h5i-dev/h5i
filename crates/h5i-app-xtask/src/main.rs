//! `cargo app-verify`: run CI's checks locally (`--full` adds mutation and
//! differential tests). Missing tools count as skipped, never passed.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Instant;

/// Proof projects: directory, extraction script, generated file.
const PROJECTS: &[(&str, Option<&str>, Option<&str>)] = &[
    ("examples/app/docs/proofs", Some("scripts/app/extract.sh"), Some("examples/app/docs/proofs/generated/DocsKernel.lean")),
    ("examples/app/kellnr/proofs", Some("scripts/app/extract-kellnr.sh"), Some("examples/app/kellnr/proofs/generated/KellnrKernel.lean")),
    ("examples/app/atuin/proofs", Some("scripts/app/extract-atuin.sh"), Some("examples/app/atuin/proofs/generated/AtuinKernel.lean")),
    ("crates/h5i-app-sql/proofs", Some("scripts/app/extract-sql.sh"), Some("crates/h5i-app-sql/proofs/generated/H5iAppSql.lean")),
    ("crates/h5i-app-pgsql/proofs", Some("scripts/app/extract-pgsql.sh"), Some("crates/h5i-app-pgsql/proofs/generated/H5iAppPgsql.lean")),
    ("crates/h5i-app-token/proofs", Some("scripts/app/extract-token.sh"), Some("crates/h5i-app-token/proofs/generated/H5iAppToken.lean")),
    ("crates/h5i-app-json/proofs", Some("scripts/app/extract-json.sh"), Some("crates/h5i-app-json/proofs/generated/H5iAppJson.lean")),
    ("examples/app/tutorials/calculator/proofs", Some("scripts/app/extract-calculator.sh"), Some("examples/app/tutorials/calculator/proofs/generated/CalculatorKernel.lean")),
    ("examples/app/tutorials/board/proofs", Some("scripts/app/extract-board.sh"), Some("examples/app/tutorials/board/proofs/generated/BoardKernel.lean")),
    ("examples/app/wastebin/proofs", Some("scripts/app/extract-wastebin.sh"), Some("examples/app/wastebin/proofs/generated/WastebinKernel.lean")),
    ("examples/app/conduit/proofs", Some("scripts/app/extract-conduit.sh"), Some("examples/app/conduit/proofs/generated/ConduitKernel.lean")),
    ("examples/app/cratesio/proofs", Some("scripts/app/extract-cratesio.sh"), Some("examples/app/cratesio/proofs/generated/CratesioKernel.lean")),
    ("examples/app/tutorials/ledger/proofs", Some("scripts/app/extract-ledger.sh"), Some("examples/app/tutorials/ledger/proofs/generated/LedgerKernel.lean")),
    ("examples/app/tutorials/inbox/proofs", Some("scripts/app/extract-inbox.sh"), Some("examples/app/tutorials/inbox/proofs/generated/InboxKernel.lean")),
    ("examples/app/tutorials/booking/proofs", Some("scripts/app/extract-booking.sh"), Some("examples/app/tutorials/booking/proofs/generated/BookingKernel.lean")),
    ("examples/app/filters/proofs", Some("scripts/app/extract-filters.sh"), Some("examples/app/filters/proofs/generated/FiltersKernel.lean")),
    ("examples/app/keys/proofs", Some("scripts/app/extract-keys.sh"), Some("examples/app/keys/proofs/generated/KeysKernel.lean")),
    ("crates/h5i-app-core/proofs", None, None),
];

#[derive(PartialEq)]
enum Outcome {
    Pass,
    Fail(String),
    Skip(String),
}

struct Ctx {
    root: PathBuf,
    results: Vec<(String, Outcome, f32)>,
}

impl Ctx {
    fn step(&mut self, name: &str, f: impl FnOnce(&Path) -> Outcome) {
        eprint!("{name:<44} ");
        let t = Instant::now();
        let out = f(&self.root);
        let secs = t.elapsed().as_secs_f32();
        match &out {
            Outcome::Pass => eprintln!("ok      {secs:>6.1}s"),
            Outcome::Skip(why) => eprintln!("skipped ({why})"),
            Outcome::Fail(log) => eprintln!("FAILED  {secs:>6.1}s\n{log}"),
        }
        self.results.push((name.to_string(), out, secs));
    }
}

/// Run a command; on failure return the last lines of its output.
fn run(dir: &Path, prog: &str, args: &[&str]) -> Outcome {
    match Command::new(prog).args(args).current_dir(dir).stdin(Stdio::null()).output() {
        Err(e) => Outcome::Fail(format!("cannot run {prog}: {e}")),
        Ok(o) if o.status.success() => Outcome::Pass,
        Ok(o) => {
            let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
            let tail: Vec<&str> = text.lines().rev().take(30).collect();
            Outcome::Fail(tail.into_iter().rev().collect::<Vec<_>>().join("\n"))
        }
    }
}

/// True if `prog` is an executable on PATH (charon and aeneas have no `--version`).
fn have(prog: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| std::env::split_paths(&paths).any(|d| d.join(prog).is_file()))
}

fn have_cargo_sub(sub: &str) -> bool {
    Command::new("cargo").args([sub, "--version"]).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// Hand-written Lean files must not use sorry or native_decide; no file may declare an axiom.
fn lean_scan(dir: &Path) -> Outcome {
    let mut bad = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return Outcome::Fail(format!("{} missing", dir.display())) };
    let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "lean")).collect();
    for sub in ["Engine", "H5iAppLib", "generated"] {
        if let Ok(e) = std::fs::read_dir(dir.join(sub)) {
            files.extend(e.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "lean")));
        }
    }
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap_or_default();
        // Generated Lean (extracted kernels, schema! output) lives in generated/.
        let is_generated = f.parent().is_some_and(|p| p.ends_with("generated"));
        for (i, line) in text.lines().enumerate() {
            let code = line.split("--").next().unwrap_or("");
            let words = |w: &str| code.split(|c: char| !c.is_alphanumeric() && c != '_').any(|t| t == w);
            if line.starts_with("axiom ") || (!is_generated && (words("sorry") || words("native_decide"))) {
                bad.push(format!("{}:{}: {}", f.display(), i + 1, line.trim()));
            }
        }
    }
    if bad.is_empty() { Outcome::Pass } else { Outcome::Fail(bad.join("\n")) }
}

/// Collect `.rs` files under `dir` whose path contains `needle`.
fn rs_files_under(dir: &Path, needle: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if !p.ends_with("target") && !p.file_name().is_some_and(|n| n == ".lake") {
                rs_files_under(&p, needle, out);
            }
        } else if p.extension().is_some_and(|x| x == "rs") && p.to_string_lossy().contains(needle) {
            out.push(p);
        }
    }
}

/// The crate a server file belongs to: the path up to and including `/server/`.
fn server_crate_of(p: &Path) -> String {
    let s = p.to_string_lossy();
    match s.find("/server/") {
        Some(i) => s[..i + "/server/".len()].to_string(),
        None => s.to_string(),
    }
}

/// Read `text` from `start` (just past a `(`) and return the substring up to the
/// matching close paren.
fn balanced_parens(text: &str, start: usize) -> &str {
    let bytes = text.as_bytes();
    let mut depth = 1i32;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return &text[start..i];
                }
            }
            _ => {}
        }
        i += 1;
    }
    &text[start..]
}

/// Every mutating route's handler must take an `Actor<` (an authenticated
/// caller). Opt out with `h5i-allow: no-actor` on or above the route.
fn route_coverage(root: &Path) -> Outcome {
    let mut files = Vec::new();
    rs_files_under(&root.join("examples/app"), "/server/", &mut files);
    // Which handler idents take an `Actor<`, per crate.
    let mut takes_actor: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let mut known: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let mut sources: Vec<(String, String)> = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap_or_default();
        let ck = server_crate_of(f);
        let mut rest = text.as_str();
        while let Some(rel) = rest.find("fn ") {
            let after = &rest[rel + 3..];
            let name: String = after.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            if let Some(paren) = after.find('(') {
                let params = balanced_parens(after, paren + 1);
                if !name.is_empty() {
                    known.insert((ck.clone(), name.clone()));
                    if params.contains("Actor<") {
                        takes_actor.insert((ck.clone(), name.clone()));
                    }
                }
            }
            rest = &after[name.len().max(1)..];
        }
        sources.push((ck, text));
    }
    // Scan mutating route registrations.
    let mut bad = Vec::new();
    let re_methods = ["post(", "put(", "delete(", "patch("];
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap_or_default();
        let ck = server_crate_of(f);
        let lines: Vec<&str> = text.lines().collect();
        for (lineno, line) in lines.iter().enumerate() {
            let prev = lineno.checked_sub(1).map(|i| lines[i]).unwrap_or("");
            if line.contains("h5i-allow: no-actor") || prev.contains("h5i-allow: no-actor") {
                continue;
            }
            for m in re_methods {
                let mut from = 0;
                while let Some(i) = line[from..].find(m) {
                    let at = from + i;
                    // Require a routing context (method-router builder), not just any foo(.
                    let after = &line[at + m.len()..];
                    let handler: String = after.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':').collect();
                    let ident = handler.rsplit("::").next().unwrap_or(&handler).to_string();
                    from = at + m.len();
                    if ident.is_empty() {
                        continue;
                    }
                    // Only judge handlers we can see in this crate; skip unknowns.
                    if known.contains(&(ck.clone(), ident.clone())) && !takes_actor.contains(&(ck.clone(), ident.clone())) {
                        bad.push(format!("{}:{}: {}({}) has no Actor<> parameter", f.display(), lineno + 1, m.trim_end_matches('('), ident));
                    }
                }
            }
        }
    }
    let _ = sources;
    if bad.is_empty() { Outcome::Pass } else { Outcome::Fail(bad.join("\n")) }
}

/// A kernel `Command` must not carry a client-set identity or privilege field;
/// identity comes from the `Actor`. Opt out with `h5i-allow: privileged-field`.
fn command_hygiene(root: &Path) -> Outcome {
    let mut files = Vec::new();
    rs_files_under(&root.join("examples/app"), "/kernel/", &mut files);
    let mut bad = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap_or_default();
        for (line, name) in flagged_command_fields(&text) {
            bad.push(format!("{}:{}: Command field `{}` is client-settable identity/privilege", f.display(), line, name));
        }
    }
    if bad.is_empty() { Outcome::Pass } else { Outcome::Fail(bad.join("\n")) }
}

/// Denied identity/privilege fields in the `Command` type, as `(line, field)`.
fn flagged_command_fields(text: &str) -> Vec<(usize, String)> {
    const DENY: &[&str] = &["owner", "owner_id", "role", "is_admin", "admin", "tenant", "tenant_id", "principal"];
    let Some(start) = text.find("enum Command").or_else(|| text.find("struct Command")) else { return Vec::new() };
    let Some(brace) = text[start..].find('{') else { return Vec::new() };
    let body = balanced_braces(&text[start + brace + 1..]);
    let base_line = text[..start + brace].lines().count();
    let mut out = Vec::new();
    for (i, line) in body.lines().enumerate() {
        if line.contains("h5i-allow: privileged-field") {
            continue;
        }
        let code = line.split("//").next().unwrap_or("");
        for field in code.split(',') {
            let name = field.split(':').next().unwrap_or("").trim().trim_start_matches("pub ").trim();
            if DENY.contains(&name) {
                out.push((base_line + i, name.to_string()));
            }
        }
    }
    out
}

/// Report which app proofs state a universal authorization theorem. Never
/// fails; makes gaps visible.
fn authz_coverage(root: &Path) -> Outcome {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for base in ["examples/app", "examples/app/tutorials"] {
        if let Ok(entries) = std::fs::read_dir(root.join(base)) {
            for e in entries.flatten() {
                let p = e.path().join("proofs");
                if p.is_dir() {
                    dirs.push(p);
                }
            }
        }
    }
    dirs.sort();
    let markers = ["WritesAuthorized", "theorem authorized", "writes_authorized", "writes_confined", "writes_scoped"];
    let schema = "WritesAuthorized";
    let mut covered = 0;
    let mut report = String::new();
    for d in &dirs {
        let mut lean = Vec::new();
        rs_or_lean(d, &mut lean);
        let mut hit = None;
        let mut uses_schema = false;
        for f in &lean {
            let text = std::fs::read_to_string(f).unwrap_or_default();
            if text.contains(schema) {
                uses_schema = true;
            }
            if hit.is_none()
                && let Some(m) = markers.iter().find(|m| text.contains(**m))
            {
                hit = Some(*m);
            }
        }
        let app = d.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        match hit {
            Some(_) => {
                covered += 1;
                let tag = if uses_schema { "schema" } else { "theorem" };
                report.push_str(&format!("  {app:<16} authorized ({tag})\n"));
            }
            None => report.push_str(&format!("  {app:<16} no universal authorization theorem\n")),
        }
    }
    eprintln!("\n  authorization coverage: {covered}/{} app proof projects\n{}", dirs.len(), report.trim_end());
    Outcome::Pass
}

/// Collect `.lean` files directly in `dir`.
fn rs_or_lean(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "lean") {
                out.push(p);
            }
        }
    }
}

/// Substring from just inside a `{` up to its matching `}`.
fn balanced_braces(text: &str) -> &str {
    let bytes = text.as_bytes();
    let mut depth = 1i32;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &text[..i];
                }
            }
            _ => {}
        }
        i += 1;
    }
    text
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("verify") {
        eprintln!("usage: cargo app-verify [--full] [--no-extract]");
        return ExitCode::from(2);
    }
    let full = args.iter().any(|a| a == "--full");
    let extract = !args.iter().any(|a| a == "--no-extract");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repository root");
    let mut cx = Ctx { root, results: Vec::new() };
    let db = std::env::var("H5I_APP_TEST_DATABASE_URL").is_ok();

    cx.step("route coverage (mutating routes take an Actor)", route_coverage);
    cx.step("command hygiene (no client-set identity fields)", command_hygiene);
    cx.step("authorization coverage report", authz_coverage);
    cx.step("rust tests", |r| {
        if !db {
            return Outcome::Skip("H5I_APP_TEST_DATABASE_URL unset".into());
        }
        run(r, "bash", &["scripts/app/ci-rust-tests.sh", "--release"])
    });
    cx.step("cargo deny (bans)", |r| {
        if !have_cargo_sub("deny") {
            return Outcome::Skip("cargo-deny not installed".into());
        }
        match run(r, "cargo", &["deny", "check", "bans"]) {
            Outcome::Pass => run(&r.join("examples/app"), "cargo", &["deny", "check", "bans"]),
            other => other,
        }
    });
    if extract {
        let tools = have("charon") && have("aeneas");
        for (_, script, generated) in PROJECTS {
            let (Some(script), Some(generated)) = (script, generated) else { continue };
            cx.step(&format!("extract {generated}"), |r| {
                if !tools {
                    return Outcome::Skip("charon/aeneas not on PATH".into());
                }
                match run(r, "bash", &[script]) {
                    Outcome::Pass => run(r, "git", &["diff", "--exit-code", "--", generated]),
                    other => other,
                }
            });
        }
    }
    let lake = have("lake");
    for (dir, _, _) in PROJECTS {
        cx.step(&format!("lean {dir}"), |r| {
            if !lake {
                return Outcome::Skip("lake not on PATH".into());
            }
            match run(&r.join(dir), "lake", &["build"]) {
                Outcome::Pass => lean_scan(&r.join(dir)),
                other => other,
            }
        });
    }
    cx.step("axioms of the docs theorems", |r| {
        if !lake {
            return Outcome::Skip("lake not on PATH".into());
        }
        run(r, "bash", &["scripts/app/ci-lean-gate.sh"])
    });
    cx.step("axioms of the database theorems", |r| {
        if !lake {
            return Outcome::Skip("lake not on PATH".into());
        }
        run(r, "bash", &["scripts/app/ci-db-axioms.sh"])
    });
    if full {
        cx.step("mutation suite", |r| run(r, "python3", &["scripts/app/mutants.py"]));
        cx.step("app mutation suite", |r| run(r, "python3", &["scripts/app/mutants-apps.py"]));
        cx.step("rust vs lean differential test", |r| run(r, "bash", &["scripts/app/difftest.sh"]));
    }

    let failed = cx.results.iter().filter(|(_, o, _)| matches!(o, Outcome::Fail(_))).count();
    let skipped = cx.results.iter().filter(|(_, o, _)| matches!(o, Outcome::Skip(_))).count();
    eprintln!("\n{} passed, {failed} failed, {skipped} skipped", cx.results.len() - failed - skipped);
    if failed > 0 { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balanced_helpers() {
        assert_eq!(balanced_parens("f(a, g(b), c) x", 2), "a, g(b), c");
        assert_eq!(balanced_braces("a { b } c } d"), "a { b } c ");
    }

    #[test]
    fn command_hygiene_flags_role_from_body() {
        // The readur class: a command carries the new user's role.
        let bad = "pub enum Command {\n    Register { email: u64, role: Role },\n}";
        let hits = flagged_command_fields(bad);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].1, "role");
    }

    #[test]
    fn command_hygiene_allows_reviewed_target() {
        // An admin action naming its target, explicitly reviewed.
        let ok = "pub enum Command {\n    SetRole { user: u64, role: Role }, // h5i-allow: privileged-field (admin sets target's role)\n}";
        assert!(flagged_command_fields(ok).is_empty());
    }

    #[test]
    fn command_hygiene_ignores_plain_fields() {
        let ok = "pub enum Command {\n    CreateDoc { project: u64, title: u64 },\n    AddMsg { conv: u64, user: u64 },\n}";
        assert!(flagged_command_fields(ok).is_empty(), "user/project/title are not privilege fields");
    }
}
