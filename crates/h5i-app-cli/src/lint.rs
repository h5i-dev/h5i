//! `h5i app lint`: source checks for the shapes the proofs assume but cannot
//! see.
//!
//! - Every mutating route of a server crate takes an `Actor` (an
//!   authenticated caller). Opt out with `h5i-allow: no-actor` on or above the
//!   route.
//! - The kernel's `Command` carries no client-set identity or privilege field:
//!   identity comes from the `Actor`. Opt out with `h5i-allow: privileged-field`.
//! - A report, never a failure: whether the proofs state a universal
//!   authorization theorem.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::manifest::Project;
use crate::util::Out;

pub fn lint(p: &Project, out: &Out) -> Result<()> {
    let mut bad = route_coverage(p);
    bad.extend(command_hygiene(p));
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
            rest = &after[name.len().max(1)..];
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
