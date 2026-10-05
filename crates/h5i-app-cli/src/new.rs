//! `h5i app new`: a project that extracts, proves and mutates out of the box.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::pins;
use crate::util;

/// Where the project gets the h5i-app Lean library from.
pub enum Lib {
    /// `require h5i_app_lib from git "…h5i" @ "<rev>" / "crates/h5i-app-core/proofs"`.
    Git(String),
    /// A local h5i checkout, required by path (for working on h5i itself).
    Path(PathBuf),
}

pub fn new(dir: &Path, name: Option<&str>, lib: &Lib) -> Result<()> {
    if dir.exists() && std::fs::read_dir(dir)?.next().is_some() {
        bail!("{} exists and is not empty", dir.display());
    }
    let name = match name {
        Some(n) => n.to_string(),
        None => dir
            .file_name()
            .context("give the project a directory name")?
            .to_string_lossy()
            .into_owned(),
    };
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        || name.starts_with(|c: char| c.is_ascii_digit())
    {
        bail!(
            "`{name}` is not a usable crate name: letters, digits, `-` and `_`, not starting with a digit (pass --name)"
        );
    }
    let package = format!("{name}-kernel");
    let id = util::ident(&package);
    let module = util::lean_module(&id);
    let lean_pkg = format!("{}_proofs", util::ident(&name));

    std::fs::create_dir_all(dir.join("kernel/src"))?;
    std::fs::create_dir_all(dir.join("proofs/generated"))?;
    let require = match lib {
        Lib::Git(rev) => format!(
            "require h5i_app_lib from git\n  \"{}\" @ \"{rev}\" / \"{}\"",
            pins::H5I_REPO,
            pins::LIB_SUBDIR
        ),
        Lib::Path(h5i) => {
            let lib = h5i.join(pins::LIB_SUBDIR);
            let lib = lib
                .canonicalize()
                .with_context(|| format!("{} is not an h5i checkout", h5i.display()))?;
            format!("require h5i_app_lib from \"{}\"", lib.display())
        }
    };
    let files: Vec<(&str, String)> = vec![
        (
            "h5i-app.toml",
            fill(MANIFEST, &id, &module, &lean_pkg, &package, &require),
        ),
        ("Cargo.toml", WORKSPACE.to_string()),
        (".gitignore", GITIGNORE.to_string()),
        (
            "README.md",
            fill(README, &id, &module, &lean_pkg, &package, &require).replace("{{name}}", &name),
        ),
        (
            "kernel/Cargo.toml",
            fill(KERNEL_TOML, &id, &module, &lean_pkg, &package, &require),
        ),
        ("kernel/src/lib.rs", KERNEL_RS.to_string()),
        (
            "proofs/lean-toolchain",
            format!("{}\n", pins::LEAN_TOOLCHAIN),
        ),
        (
            "proofs/lakefile.lean",
            fill(LAKEFILE, &id, &module, &lean_pkg, &package, &require),
        ),
        (
            "proofs/Theorems.lean",
            fill(THEOREMS, &id, &module, &lean_pkg, &package, &require),
        ),
        ("proofs/generated/.gitkeep", String::new()),
    ];
    for (f, text) in files {
        std::fs::write(dir.join(f), text).with_context(|| format!("writing {f}"))?;
    }
    let shown = crate::manifest::display_path(&dir.canonicalize()?);
    eprintln!("created {shown}: kernel/ (Rust), proofs/ (Lean), h5i-app.toml");
    match lib {
        Lib::Git(rev) => eprintln!(
            "the proofs require h5i's Lean library from {} at `{rev}` (change it with --rev, or use \
             --h5i-path for a local checkout)\n",
            pins::H5I_REPO
        ),
        Lib::Path(p) => eprintln!("the proofs require h5i's Lean library from {}\n", p.display()),
    }
    eprintln!("next:");
    eprintln!("  cd {shown}");
    eprintln!("  h5i app doctor     # Charon, Aeneas and Lean at the pinned versions?");
    eprintln!("  h5i app prove      # extract kernel/ to Lean, build and gate the proofs");
    eprintln!("  h5i app mutate     # the bugs in h5i-app.toml must break a proof");
    eprintln!("  h5i app mutate --auto   # and so should generated ones");
    Ok(())
}

/// `preferred` (this binary's release tag) if the h5i repository has it,
/// otherwise `main`: a build made between releases reports a version whose tag
/// does not exist yet, or predates the Lean library. Offline, keep `preferred`
/// and say so; Lake will need the network to fetch it anyway.
pub fn default_rev(preferred: &str) -> String {
    let out = std::process::Command::new("git")
        .args(["ls-remote", "--exit-code", "--tags", pins::H5I_REPO])
        .arg(format!("refs/tags/{preferred}"))
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() && has_lib(preferred) => preferred.to_string(),
        Ok(o) if o.status.success() || o.status.code() == Some(2) => {
            eprintln!("note: h5i {preferred} has no h5i-app Lean library to require; using `main` (pin one with --rev)");
            "main".to_string()
        }
        _ => {
            eprintln!("warning: could not reach {} to check for tag {preferred}; requiring it anyway", pins::H5I_REPO);
            preferred.to_string()
        }
    }
}

/// Whether release `tag` ships the Lean library. Releases before
/// `LIB_SINCE` predate h5i-app's move into the h5i repository.
fn has_lib(tag: &str) -> bool {
    let v = |s: &str| -> Option<(u64, u64, u64)> {
        let mut it = s.trim_start_matches('v').split('.').map(|p| p.parse::<u64>().ok());
        Some((it.next()??, it.next()??, it.next()??))
    };
    match (v(tag), v(pins::LIB_SINCE)) {
        (Some(t), Some(s)) => t >= s,
        _ => true,
    }
}

fn fill(t: &str, id: &str, module: &str, lean_pkg: &str, package: &str, require: &str) -> String {
    t.replace("{{id}}", id)
        .replace("{{module}}", module)
        .replace("{{lean_pkg}}", lean_pkg)
        .replace("{{package}}", package)
        .replace("{{require}}", require)
        .replace("{{aeneas}}", pins::AENEAS_REV)
}

const WORKSPACE: &str = r#"[workspace]
resolver = "3"
members = ["kernel"]
"#;

const GITIGNORE: &str = "target/\nproofs/.lake/\n";

const KERNEL_TOML: &str = r#"[package]
name = "{{package}}"
edition = "2024"
version = "0.1.0"
publish = false

# Extracted to Lean by `h5i app extract`. Keep it to the Rust that Charon and
# Aeneas translate, and its dependencies to crates that are extracted with it
# (h5i-app-std, h5i-app-sql, h5i-app-schema), never I/O.
[dependencies]
"#;

const KERNEL_RS: &str = r#"//! The kernel: every decision the application makes, written in the subset of
//! Rust that Charon and Aeneas translate to Lean (no `?`, no closures, no
//! `String`; loops over indices). `h5i app extract` turns it into
//! `proofs/generated/`, and the theorems in `proofs/` are about this code.

/// The whole state: a counter that must never pass its limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State {
    pub count: u64,
    pub limit: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Add one, unless the counter is at its limit.
    Incr,
    /// Back to zero.
    Reset,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    AtLimit,
}

/// One step: the next state, or why the command is refused.
pub fn transition(s: &State, cmd: &Command) -> Result<State, Error> {
    match cmd {
        Command::Incr => {
            if s.count >= s.limit {
                Err(Error::AtLimit)
            } else {
                Ok(State { count: s.count + 1, limit: s.limit })
            }
        }
        Command::Reset => Ok(State { count: 0, limit: s.limit }),
    }
}
"#;

const MANIFEST: &str = r#"# Read by `h5i app`: extract, check, prove, mutate, doctor.

[extract]
crate = "kernel"
# Charon starts here and extracts what it reaches.
start-from = ["transition"]

[check]
# Must exist and use only Lean's standard axioms. Every theorem in a
# hand-written module under proofs/ is gated anyway; naming the main ones
# here catches a rename.
theorems = ["{{id}}.inv_preserved", "{{id}}.incr_below_limit", "{{id}}.reset_zero"]

# Bugs the proofs must reject (`h5i app mutate`). Each `old` must occur
# exactly once in kernel/src/lib.rs (or in `file`).
[[mutant]]
name = "off_by_one_limit"
old = "if s.count >= s.limit {"
new = "if s.count > s.limit {"

[[mutant]]
name = "reset_keeps_count"
old = "Command::Reset => Ok(State { count: 0, limit: s.limit }),"
new = "Command::Reset => Ok(State { count: s.count, limit: s.limit }),"
"#;

const LAKEFILE: &str = r#"import Lake
open Lake DSL

-- Pinned to the Aeneas commit `h5i app` extracts with (`h5i app doctor`).
require aeneas from git
  "https://github.com/AeneasVerif/aeneas" @ "{{aeneas}}" / "backends/lean"

-- h5i-app's lemmas and tactics for extracted kernels.
{{require}}

package {{lean_pkg}}

-- The extracted kernel (`h5i app extract`). Do not edit.
lean_lib Generated where
  srcDir := "generated"
  roots := #[`{{module}}]

-- Hand-written specs and proofs.
@[default_target] lean_lib Proofs where
  roots := #[`Theorems]
"#;

const THEOREMS: &str = r#"import {{module}}
import H5iAppLib
open Aeneas Aeneas.Std Result Aeneas.Std.WP H5iAppLib

-- `step*` specs for every `==` and `clone` the kernel derives.
h5i_derive_all

namespace {{id}}

/-- The invariant: the counter never passes its limit. -/
def Inv (s : State) : Prop := s.count.val ≤ s.limit.val

/-- No command panics or overflows, and every one keeps the invariant. -/
theorem inv_preserved (s : State) (c : Command) (h : Inv s) :
    transition s c ⦃ r => ∀ s', r = .Ok s' → Inv s' ⦄ := by
  unfold transition Inv at *
  cases c <;> step*

/-- Below the limit, `Incr` succeeds and adds one. The invariant alone would
also hold for a kernel that refuses everything. -/
theorem incr_below_limit (s : State) (h : s.count.val < s.limit.val) :
    transition s .Incr ⦃ r => ∃ s', r = .Ok s' ∧ s'.count.val = s.count.val + 1 ⦄ := by
  unfold transition
  step*

/-- `Reset` always succeeds and starts again from zero. -/
theorem reset_zero (s : State) :
    transition s .Reset ⦃ r => ∃ s', r = .Ok s' ∧ s'.count.val = 0 ⦄ := by
  unfold transition
  step*

end {{id}}
"#;

const README: &str = r#"# {{name}}

An h5i-app kernel and its Lean proofs.

| Path | Contents |
|---|---|
| `kernel/src/lib.rs` | the application logic the proofs are about |
| `proofs/Theorems.lean` | the spec and its proofs |
| `proofs/generated/` | Lean extracted from `kernel/` by `h5i app extract`; never edit |
| `h5i-app.toml` | what `h5i app` extracts, gates and mutates |

```
h5i app doctor         # Charon, Aeneas and Lean at the pinned versions
h5i app prove          # extract, then build and gate the proofs
h5i app mutate         # the bugs listed in h5i-app.toml must break a proof
h5i app mutate --auto  # so should generated ones; a survivor is a gap in the spec
```

`h5i app check` rejects `sorry`, `native_decide` and `axiom`, and any theorem
in a hand-written module that depends on an axiom beyond `propext`,
`Classical.choice` and `Quot.sound`.

The tactics and lemmas for the proofs (`h5i_invert`, `h5i_arith`,
`h5i_search_any`, ...) are listed in H5iAppLib's README:
`crates/h5i-app-core/proofs/README.md` in the h5i repository.
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn releases_before_the_move_have_no_library() {
        assert!(!has_lib("v0.4.8"));
        assert!(has_lib("v0.4.9"));
        assert!(has_lib("v0.10.0"));
        assert!(has_lib("main"));
    }
}
