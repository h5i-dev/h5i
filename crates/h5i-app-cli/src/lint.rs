//! `h5i app lint`: source checks for the shapes the proofs assume but cannot
//! see.
//!
//! - Every mutating route of a server crate takes an `Actor` (an
//!   authenticated caller). Opt out with `h5i-allow: no-actor` on or above the
//!   route.
//! - The kernel's `Command` carries no client-set identity or privilege field:
//!   identity comes from the `Actor`. Opt out with `h5i-allow: privileged-field`.
//! - A `Kernel` impl that declares read-only commands (`fn read_only`) comes
//!   with a proof of `H5iAppLib.ReadOnly.Sound`: the engine skips locks and
//!   retries for them on the strength of it.
//! - An app that lists `[authz] guarded` functions runs one of them as its
//!   `Kernel::transition`: otherwise the framework's theorem is about code the
//!   server does not run.
//! - A report, never a failure: whether the proofs state a universal
//!   authorization theorem, or the framework proves one.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::manifest::Project;
use crate::util::Out;

pub fn lint(p: &Project, out: &Out) -> Result<()> {
    let mut bad = route_coverage(p);
    bad.extend(command_hygiene(p));
    bad.extend(read_only_proof(p));
    bad.extend(guarded_wiring(p));
    if let Some(note) = authz_coverage(p) {
        out.note(&note);
    }
    if !bad.is_empty() {
        bail!("{}: lint failed:\n  {}", p.display(), bad.join("\n  "));
    }
    Ok(())
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            let n = e.file_name();
            if !matches!(n.to_string_lossy().as_ref(), "target" | ".lake" | ".git" | "proofs") {
                rs_files(&p, out);
            }
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// The crate a file belongs to: its nearest ancestor with a Cargo.toml.
fn crate_of(f: &Path) -> Option<PathBuf> {
    f.ancestors().skip(1).find(|d| d.join("Cargo.toml").is_file()).map(Path::to_path_buf)
}

/// A crate that serves HTTP: one depending on axum or h5i-app's axum layer.
fn is_server(krate: &Path) -> bool {
    let text = std::fs::read_to_string(krate.join("Cargo.toml")).unwrap_or_default();
    text.lines().any(|l| {
        let l = l.trim_start();
        l.starts_with("axum") || l.starts_with("h5i-app-http") || (l.starts_with("h5i-app ") && l.contains("http"))
    })
}

fn rel(p: &Project, f: &Path) -> String {
    f.strip_prefix(&p.root).unwrap_or(f).display().to_string()
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

/// Substring from just inside a `{` up to its matching `}`.
fn balanced_braces(text: &str) -> &str {
    let bytes = text.as_bytes();
    let mut depth = 1i32;
    for (i, b) in bytes.iter().enumerate() {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &text[..i];
                }
            }
            _ => {}
        }
    }
    text
}

/// Every mutating route's handler takes an `Actor<`.
fn route_coverage(p: &Project) -> Vec<String> {
    let mut all = Vec::new();
    rs_files(&p.root, &mut all);
    let files: Vec<(PathBuf, PathBuf)> =
        all.into_iter().filter_map(|f| crate_of(&f).filter(|c| is_server(c)).map(|c| (f, c))).collect();
    // Which handler idents take an `Actor<`, per crate.
    let mut takes_actor = HashSet::new();
    let mut known = HashSet::new();
    for (f, ck) in &files {
        let text = std::fs::read_to_string(f).unwrap_or_default();
        let mut rest = text.as_str();
        while let Some(i) = rest.find("fn ") {
            let after = &rest[i + 3..];
            let name: String = after.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            if let Some(paren) = after.find('(')
                && !name.is_empty()
            {
                known.insert((ck.clone(), name.clone()));
                if balanced_parens(after, paren + 1).contains("Actor<") {
                    takes_actor.insert((ck.clone(), name.clone()));
                }
            }
            // `name` is a prefix of `after` made of whole characters; with
            // no name, step over one character, however wide.
            let step = if name.is_empty() { after.chars().next().map_or(after.len(), char::len_utf8) } else { name.len() };
            rest = &after[step..];
        }
    }
    let mut bad = Vec::new();
    for (f, ck) in &files {
        let text = std::fs::read_to_string(f).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        for (n, line) in lines.iter().enumerate() {
            let prev = n.checked_sub(1).map(|i| lines[i]).unwrap_or("");
            if line.contains("h5i-allow: no-actor") || prev.contains("h5i-allow: no-actor") {
                continue;
            }
            for m in ["post(", "put(", "delete(", "patch("] {
                let mut from = 0;
                while let Some(i) = line[from..].find(m) {
                    let at = from + i;
                    from = at + m.len();
                    let handler: String =
                        line[from..].chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':').collect();
                    let ident = handler.rsplit("::").next().unwrap_or(&handler).to_string();
                    let key = (ck.clone(), ident.clone());
                    // Only judge handlers visible in this crate.
                    if !ident.is_empty() && known.contains(&key) && !takes_actor.contains(&key) {
                        bad.push(format!(
                            "{}:{}: {}({ident}) has no Actor<> parameter (or mark it `h5i-allow: no-actor`)",
                            rel(p, f),
                            n + 1,
                            m.trim_end_matches('(')
                        ));
                    }
                }
            }
        }
    }
    bad
}

/// Each `impl Kernel` body in the project's crates' `src/`, with its file and
/// line. Every impl in a file counts; a `#[cfg(test)]` module and a crate's
/// `tests/` do not, so a test's impl can neither satisfy nor fail a check
/// about what the server runs.
fn kernel_impls(p: &Project) -> Vec<(PathBuf, usize, String)> {
    let mut files = Vec::new();
    rs_files(&p.root, &mut files);
    files.sort();
    let mut out = Vec::new();
    for f in files {
        let Some(krate) = crate_of(&f) else { continue };
        if !f.starts_with(krate.join("src")) {
            continue;
        }
        let text = std::fs::read_to_string(&f).unwrap_or_default();
        let code = &text[..text.find("#[cfg(test)]").unwrap_or(text.len())];
        for marker in ["impl Kernel for", "impl h5i_app_core::Kernel for"] {
            for (at, _) in code.match_indices(marker) {
                let Some(brace) = code[at..].find('{') else { continue };
                let body = balanced_braces(&code[at + brace + 1..]).to_string();
                let line = code[..at].lines().count() + 1;
                out.push((f.clone(), line, body));
            }
        }
    }
    out
}

/// Whether some `impl Kernel` overrides `read_only`.
pub fn overrides_read_only(p: &Project) -> bool {
    kernel_impls(p).iter().any(|(_, _, body)| body.contains("fn read_only("))
}

/// `body` calls `name(` as a whole identifier (`k::name(` counts,
/// `my_name(` does not).
fn calls(body: &str, name: &str) -> bool {
    let pat = format!("{name}(");
    body.match_indices(&pat).any(|(i, _)| {
        body[..i].chars().next_back().is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
    })
}

/// Whether the kernel crate runs read-only commands through `guard_viewed`,
/// the guard that comes with a `ReadOnly.Sound` proof.
fn uses_guard_viewed(p: &Project) -> bool {
    let Some(krate) = p.krate() else { return false };
    let mut files = Vec::new();
    rs_files(&krate.join("src"), &mut files);
    files.iter().any(|f| {
        std::fs::read_to_string(f)
            .unwrap_or_default()
            .lines()
            .any(|l| l.split("//").next().unwrap_or("").contains("guard_viewed"))
    })
}

/// The body of `fn name(` inside an impl body.
fn fn_body<'a>(body: &'a str, name: &str) -> Option<&'a str> {
    let at = body.find(&format!("fn {name}("))?;
    let brace = body[at..].find('{')?;
    Some(balanced_braces(&body[at + brace + 1..]))
}

/// Every `impl Kernel` that overrides `read_only` has a `ReadOnly.Sound`
/// proof: a hand-written one, or the framework's for a kernel guarded with
/// `guard_viewed` (`h5i app check` then requires `<fn>.read_only_sound`).
fn read_only_proof(p: &Project) -> Vec<String> {
    let proven = crate::check::has_read_only_proof(p);
    // The framework's proof is about the view's `read_only`, so it counts only
    // for an impl that delegates to the kernel's.
    let generated = !p.manifest.authz.guarded.is_empty()
        && uses_guard_viewed(p)
        && p.generated().join(format!("{}.lean", crate::extract::AUTHZ_MODULE)).is_file();
    let mut bad = Vec::new();
    for (f, line, body) in kernel_impls(p) {
        let delegates = fn_body(&body, "read_only").is_some_and(|b| calls(b, "read_only"));
        if body.contains("fn read_only(") && !proven && !(generated && delegates) {
            bad.push(format!(
                "{}:{line}: Kernel::read_only is overridden, but no proof in {} states `H5iAppLib.ReadOnly.Sound`",
                rel(p, &f),
                p.proofs().display()
            ));
        }
    }
    bad
}

/// With `[authz] guarded`, some `Kernel` impl runs a guarded function as its
/// `transition` and commits with the kernel's `apply`: the theorem is about
/// that pair (`h5i_authz` proves the policy checks against that `apply`).
fn guarded_wiring(p: &Project) -> Vec<String> {
    let guarded = &p.manifest.authz.guarded;
    if guarded.is_empty() {
        return Vec::new();
    }
    let impls = kernel_impls(p);
    let wired = |body: &str| {
        fn_body(body, "transition").is_some_and(|t| guarded.iter().any(|g| calls(t, g)))
            && fn_body(body, "apply").is_some_and(|a| calls(a, "apply"))
    };
    if impls.is_empty() {
        return vec![format!(
            "`[authz] guarded` names {}, but no crate's `src/` has an `impl Kernel`",
            guarded.join(", ")
        )];
    }
    impls
        .iter()
        .filter(|(_, _, body)| !wired(body))
        .map(|(f, line, _)| {
            format!(
                "{}:{line}: this `Kernel` impl does not call {} from `transition` and the kernel's `apply` \
                 from `apply`: with `[authz]`, every impl must, or the framework's theorem is about \
                 code the server does not run",
                rel(p, f),
                guarded.join(" or ")
            )
        })
        .collect()
}

/// The kernel's `Command` carries no client-set identity or privilege field.
fn command_hygiene(p: &Project) -> Vec<String> {
    let Some(krate) = p.krate() else { return Vec::new() };
    let mut files = Vec::new();
    rs_files(&krate.join("src"), &mut files);
    files.sort();
    let mut bad = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap_or_default();
        for (line, name) in flagged_command_fields(&text) {
            bad.push(format!(
                "{}:{line}: Command field `{name}` is client-settable identity or privilege (or mark it \
                 `h5i-allow: privileged-field`)",
                rel(p, f)
            ));
        }
    }
    bad
}

/// Denied identity/privilege fields in the `Command` type, as `(line, field)`.
pub fn flagged_command_fields(text: &str) -> Vec<(usize, String)> {
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

/// Whether the proofs of a kernel with commands state a universal
/// authorization theorem. `None` for a project that has no `Command`.
pub fn authz_coverage(p: &Project) -> Option<String> {
    let krate = p.krate()?;
    let guarded = &p.manifest.authz.guarded;
    if !guarded.is_empty() {
        let viewed = uses_guard_viewed(p);
        let thms: Vec<String> = guarded.iter().map(|g| format!("{g}.authorized")).collect();
        return Some(format!(
            "authorization: framework-proven ({}; writes and disclosed rows{})",
            thms.join(", "),
            if viewed { ", read-only noninterference" } else { "" }
        ));
    }
    let mut files = Vec::new();
    rs_files(&krate.join("src"), &mut files);
    let has_commands = files.iter().any(|f| {
        let t = std::fs::read_to_string(f).unwrap_or_default();
        t.contains("enum Command") || t.contains("struct Command")
    });
    if !has_commands {
        return None;
    }
    let markers = ["WritesAuthorized", "theorem authorized", "writes_authorized", "writes_confined", "writes_scoped"];
    let (mut hit, mut schema, mut reads) = (false, false, false);
    if let Ok(entries) = std::fs::read_dir(p.proofs()) {
        for e in entries.flatten() {
            if e.path().extension().is_none_or(|x| x != "lean") {
                continue;
            }
            let t = std::fs::read_to_string(e.path()).unwrap_or_default();
            schema |= t.contains("WritesAuthorized");
            reads |= t.contains("ReadsAuthorized");
            hit |= markers.iter().any(|m| t.contains(m));
        }
    }
    Some(if hit {
        let tag = if schema { "schema" } else { "theorem" };
        format!("authorization: proven ({tag}{})", if reads { ", reads too" } else { "" })
    } else {
        "authorization: no universal authorization theorem (state one with H5iAppLib.Authz)".to_string()
    })
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

    #[test]
    fn scanning_survives_wide_characters_and_a_trailing_fn() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("h5i-app.toml"), "").unwrap();
        let server = d.path().join("server");
        std::fs::create_dir_all(server.join("src")).unwrap();
        std::fs::write(server.join("Cargo.toml"), "[package]\nname = \"s\"\n[dependencies]\naxum = \"0.8\"\n").unwrap();
        std::fs::write(server.join("src/main.rs"), "// every fn — one\n// fn é\nfn ").unwrap();
        let p = Project::load(&d.path().join("h5i-app.toml")).unwrap();
        assert!(route_coverage(&p).is_empty());
    }

    #[test]
    fn read_only_needs_a_proof() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("h5i-app.toml"), "").unwrap();
        let server = d.path().join("server");
        std::fs::create_dir_all(server.join("src")).unwrap();
        std::fs::create_dir_all(d.path().join("proofs")).unwrap();
        std::fs::write(server.join("Cargo.toml"), "[package]\nname = \"s\"\n[dependencies]\naxum = \"0.8\"\n").unwrap();
        std::fs::write(server.join("src/lib.rs"), "impl Kernel for K {\n    fn read_only(c: &C) -> bool { k::read_only(c) }\n}\n").unwrap();
        let p = Project::load(&d.path().join("h5i-app.toml")).unwrap();
        assert_eq!(read_only_proof(&p).len(), 1);
        std::fs::write(d.path().join("proofs/ReadOnly.lean"), "-- ReadOnly.Sound, some day\n").unwrap();
        assert_eq!(read_only_proof(&p).len(), 1, "a comment is not a proof");
        std::fs::write(d.path().join("proofs/ReadOnly.lean"), "theorem ro : H5iAppLib.ReadOnly.Sound k t w := k_sound\n").unwrap();
        assert!(read_only_proof(&p).is_empty());
    }

    #[test]
    fn guarded_kernels_must_be_wired() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("h5i-app.toml"), "[authz]\nguarded = [\"guarded_transition\"]\n").unwrap();
        let server = d.path().join("server");
        std::fs::create_dir_all(server.join("src")).unwrap();
        std::fs::create_dir_all(server.join("tests")).unwrap();
        std::fs::write(server.join("Cargo.toml"), "[package]\nname = \"s\"\n[dependencies]\naxum = \"0.8\"\n").unwrap();
        let lib = server.join("src/lib.rs");
        let kernel = |t: &str, a: &str| {
            format!("impl Kernel for K {{\n    fn transition(a: &P, s: &S, c: &C) -> R {{\n        {t}(a, s, c)\n    }}\n    fn apply(s: &S, ws: &W) -> S {{\n        {a}(s, ws)\n    }}\n}}\n")
        };
        let p = Project::load(&d.path().join("h5i-app.toml")).unwrap();
        std::fs::write(&lib, kernel("k::transition", "k::apply")).unwrap();
        assert_eq!(guarded_wiring(&p).len(), 1, "unguarded transition");
        std::fs::write(&lib, kernel("k::my_guarded_transition", "k::apply")).unwrap();
        assert_eq!(guarded_wiring(&p).len(), 1, "a longer name is not the guarded function");
        std::fs::write(&lib, kernel("k::guarded_transition", "k::snapshot_of")).unwrap();
        assert_eq!(guarded_wiring(&p).len(), 1, "commits with something else than apply");
        std::fs::write(&lib, kernel("k::guarded_transition", "k::apply")).unwrap();
        assert!(guarded_wiring(&p).is_empty());
        // A wired impl elsewhere does not excuse an unwired one, in any order.
        let two = format!("{}{}", kernel("k::guarded_transition", "k::apply"), kernel("k::transition", "k::apply"));
        std::fs::write(&lib, two).unwrap();
        assert_eq!(guarded_wiring(&p).len(), 1, "second impl unwired");
        std::fs::write(&lib, kernel("k::transition", "k::apply")).unwrap();
        std::fs::write(server.join("tests/decoy.rs"), kernel("k::guarded_transition", "k::apply")).unwrap();
        assert_eq!(guarded_wiring(&p).len(), 1, "a wired impl in tests/ is not the server's");
        let with_test = format!("{}#[cfg(test)]\nmod t {{\n{}}}\n", kernel("k::guarded_transition", "k::apply"), kernel("k::transition", "k::apply"));
        std::fs::write(&lib, with_test).unwrap();
        assert!(guarded_wiring(&p).is_empty(), "an impl in a test module is not the server's");
    }

    #[test]
    fn calls_matches_whole_identifiers() {
        assert!(calls("k::apply(s, ws)", "apply"));
        assert!(calls("apply(s, ws)", "apply"));
        assert!(!calls("k::reapply(s, ws)", "apply"));
        assert!(!calls("apply_one(s)", "apply"));
    }

    #[test]
    fn route_without_actor_is_flagged() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("h5i-app.toml"), "").unwrap();
        let server = d.path().join("server");
        std::fs::create_dir_all(server.join("src")).unwrap();
        std::fs::write(server.join("Cargo.toml"), "[package]\nname = \"s\"\n[dependencies]\naxum = \"0.8\"\n").unwrap();
        std::fs::write(
            server.join("src/main.rs"),
            "fn ok(a: Actor<K>) {}\nfn open() {}\nfn r() { route(\"/a\", post(ok)); route(\"/b\", post(open)); }\n",
        )
        .unwrap();
        let p = Project::load(&d.path().join("h5i-app.toml")).unwrap();
        let bad = route_coverage(&p);
        assert_eq!(bad.len(), 1, "{bad:?}");
        assert!(bad[0].contains("post(open)"));
    }
}
