//! `h5i app check`: build the proofs and gate them. No `sorry` or
//! `native_decide` in hand-written Lean, no `axiom` declaration anywhere, and
//! every theorem of every hand-written module (plus the manifest's named
//! theorems) depends on Lean's three standard axioms and nothing else.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Result, bail};

use crate::manifest::Project;
use crate::util::{self, Out};

/// The axioms a gated theorem may use.
pub const STANDARD_AXIOMS: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];

/// `refetch` discards the Lake packages and fetches them again
/// ([`crate::packages`]).
pub fn check(p: &Project, refetch: bool, out: &Out) -> Result<()> {
    let proofs = p.proofs();
    if !proofs.join("lakefile.lean").is_file() && !proofs.join("lakefile.toml").is_file() {
        bail!("{} is not a Lake project", proofs.display());
    }
    // Held through the build, so another `h5i app` cannot discard the
    // packages under it.
    let _packages = crate::packages::prepare(&proofs, refetch, out)?;
    let mut build = Command::new("lake");
    build
        .arg("build")
        .args(&p.manifest.check.targets)
        .current_dir(&proofs);
    util::run(&mut build, out)?;

    let mut problems = scan(&proofs)?;
    let (built, unbuilt) = hand_written_modules(&proofs, &p.manifest.check.targets)?;
    for m in &unbuilt {
        out.note(&format!(
            "note: {m} is not built by `lake build`, so its theorems are not gated"
        ));
    }
    let theorems = &p.manifest.check.theorems;
    if built.is_empty() && theorems.is_empty() {
        out.note("note: no hand-written modules built; nothing to gate");
    } else {
        problems.extend(gate(&proofs, &built, theorems, out)?);
    }
    if !problems.is_empty() {
        bail!(
            "{}: proof gate failed:\n  {}",
            p.display(),
            problems.join("\n  ")
        );
    }
    Ok(())
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
        let module = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join(".");
        modules.insert(module, rel);
    }
    let roots = build_roots(proofs, targets);
    // Follow imports from the roots, through hand-written modules only.
    let mut reached = std::collections::BTreeSet::new();
    let mut stack: Vec<String> = roots.into_iter().filter(|r| modules.contains_key(r)).collect();
    while let Some(m) = stack.pop() {
        if !reached.insert(m.clone()) {
            continue;
        }
        let text = std::fs::read_to_string(proofs.join(&modules[&m]).with_extension("lean")).unwrap_or_default();
        for i in imports(&text) {
            if modules.contains_key(&i) && !reached.contains(&i) {
                stack.push(i);
            }
        }
    }
    let (mut built, mut unbuilt) = (Vec::new(), Vec::new());
    for (module, rel) in &modules {
        let olean = proofs.join(".lake/build/lib/lean").join(rel).with_extension("olean");
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
        let rest = ["import ", "public import ", "meta import ", "public meta import "]
            .iter()
            .find_map(|k| l.strip_prefix(k));
        if let Some(rest) = rest {
            let rest = rest.split("--").next().unwrap_or("");
            out.extend(rest.split_whitespace().filter(|w| *w != "all").map(str::to_string));
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
        return libs.into_iter().filter(|l| l.default).flat_map(|l| l.roots).collect();
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
        let Ok(doc) = text.parse::<toml::Table>() else { return Vec::new() };
        let defaults: Vec<String> = doc
            .get("defaultTargets")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let libs = doc.get("lean_lib").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        return libs
            .iter()
            .filter_map(|l| {
                let name = l.get("name")?.as_str()?.to_string();
                let roots = l
                    .get("roots")
                    .and_then(|r| r.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                    .unwrap_or_else(|| vec![name.clone()]);
                Some(LeanLib { default: defaults.contains(&name), name, roots })
            })
            .collect();
    }
    let Ok(text) = std::fs::read_to_string(proofs.join("lakefile.lean")) else { return Vec::new() };
    let mut libs = Vec::new();
    let mut rest = text.as_str();
    while let Some(i) = rest.find("lean_lib ") {
        let line_start = rest[..i].rfind('\n').map_or(0, |n| n + 1);
        let default = rest[line_start..i].contains("default_target");
        let after = &rest[i + "lean_lib ".len()..];
        let name: String = after.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.').collect();
        // The declaration runs to the next top-level one.
        let end = ["\nlean_lib ", "\n@[", "\nlean_exe ", "\nrequire ", "\npackage ", "\ntarget "]
            .iter()
            .filter_map(|k| after.find(k))
            .min()
            .unwrap_or(after.len());
        let body = &after[..end];
        let roots = match body.find("roots").and_then(|r| body[r..].find("#[").map(|o| r + o + 2)) {
            Some(open) => {
                let close = body[open..].find(']').map_or(body.len(), |c| open + c);
                body[open..close].split(',').map(|r| r.trim().trim_start_matches('`').to_string()).filter(|r| !r.is_empty()).collect()
            }
            None => vec![name.clone()],
        };
        libs.push(LeanLib { name, roots, default });
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

/// The Lean program that imports the modules and reports every theorem whose
/// axioms go beyond the standard three.
fn gate_program(modules: &[String], theorems: &[String]) -> String {
    let mut s = String::from("import Lean\n");
    for m in modules {
        s.push_str(&format!("import {m}\n"));
    }
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
      out := out.push s!"{MARK} missing {{n}}"
  let mut bad := 0
  for n in todo do
    let axs ← collectAxioms n
    let extra := axs.filter (fun a => !std.contains a)
    unless extra.isEmpty do
      bad := bad + 1
      out := out.push s!"{MARK} bad {{n}} {{extra.toList}}"
  out := out.push s!"{MARK} done {{todo.size}} {{bad}}"
  logInfo (String.intercalate "\n" out.toList)
"#
    ));
    s
}

fn gate(proofs: &Path, modules: &[String], theorems: &[String], out: &Out) -> Result<Vec<String>> {
    let dir = proofs.join(".lake/h5i");
    std::fs::create_dir_all(&dir)?;
    let file = dir.join("Gate.lean");
    std::fs::write(&file, gate_program(modules, theorems))?;
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
    let mut problems = Vec::new();
    let mut done = None;
    for line in text.lines() {
        let Some(i) = line.find(MARK) else { continue };
        let mut words = line[i + MARK.len()..].trim().splitn(2, ' ');
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
    let Some(done) = done else {
        bail!("the axiom gate did not run:\n{}", text.trim_end());
    };
    let total = done.split(' ').next().unwrap_or("0");
    out.note(&format!(
        "gate: {total} theorems in {} modules checked against {}",
        modules.len(),
        STANDARD_AXIOMS.join(", ")
    ));
    Ok(problems)
}

#[cfg(test)]
mod tests {
    use super::*;

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
