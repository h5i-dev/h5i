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

pub fn check(p: &Project, out: &Out) -> Result<()> {
    let proofs = p.proofs();
    if !proofs.join("lakefile.lean").is_file() && !proofs.join("lakefile.toml").is_file() {
        bail!("{} is not a Lake project", proofs.display());
    }
    fetch_packages(&proofs, out);
    let mut build = Command::new("lake");
    build
        .arg("build")
        .args(&p.manifest.check.targets)
        .current_dir(&proofs);
    util::run(&mut build, out)?;

    let mut problems = scan(&proofs)?;
    let (built, unbuilt) = hand_written_modules(&proofs)?;
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

/// A fresh project has no packages yet: fetch Mathlib's build cache, which
/// Aeneas needs, rather than building Mathlib from source. Failure is not
/// fatal; `lake build` will resolve and build what is missing.
fn fetch_packages(proofs: &Path, out: &Out) {
    if proofs.join(".lake/packages").exists() {
        return;
    }
    out.note("fetching Lean packages and the Mathlib cache (first build only)");
    let ok = util::status(
        Command::new("lake")
            .args(["exe", "cache", "get"])
            .current_dir(proofs),
        out,
    );
    if !matches!(ok, Ok(true)) {
        out.note("warning: `lake exe cache get` failed; Mathlib will be built from source");
    }
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

/// The hand-written modules, split into those `lake build` produced an
/// `.olean` for and those it did not.
fn hand_written_modules(proofs: &Path) -> Result<(Vec<String>, Vec<String>)> {
    let mut files = Vec::new();
    lean_files(proofs, &mut files);
    files.sort();
    let (mut built, mut unbuilt) = (Vec::new(), Vec::new());
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
        let olean = proofs
            .join(".lake/build/lib/lean")
            .join(&rel)
            .with_extension("olean");
        if olean.is_file() {
            built.push(module)
        } else {
            unbuilt.push(module)
        }
    }
    Ok((built, unbuilt))
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
