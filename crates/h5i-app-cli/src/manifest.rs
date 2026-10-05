//! `h5i-app.toml`: what `h5i app` needs to know about one proof project.
//!
//! The file sits at the root of an application (or of a framework crate whose
//! code is extracted), next to the kernel crate and the `proofs/` Lake
//! project. Everything that used to be spelled out in a per-app script lives
//! here: the Charon start points, the theorems the gate must find, the mutants
//! the spec must catch.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

pub const FILE: &str = "h5i-app.toml";

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Rust to Lean. Absent for a proof project with no extracted code
    /// (the proof library itself).
    pub extract: Option<Extract>,
    #[serde(default)]
    pub proofs: Proofs,
    #[serde(default)]
    pub check: Check,
    #[serde(default)]
    pub mutate: Mutate,
    /// Bugs the proofs must reject: `[[mutant]]`.
    #[serde(default, rename = "mutant")]
    pub mutants: Vec<Mutant>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Extract {
    /// The crate to extract, relative to the manifest.
    #[serde(rename = "crate", default = "default_crate")]
    pub krate: String,
    /// Charon start points, as paths inside the crate (`transition`,
    /// `Foo::bar`). Empty extracts the whole crate.
    #[serde(default)]
    pub start_from: Vec<String>,
    /// Also start from everything `schema!` generates, so each lemma in the
    /// generated `Schema.lean` has its function extracted.
    #[serde(default)]
    pub schema: bool,
    /// Crates whose items are extracted with the kernel (`--include`).
    #[serde(default)]
    pub include: Vec<String>,
    /// Copy h5i-app-std's specs next to the extraction as `StdSpecs.lean`,
    /// importing it. For kernels that include `h5i_app_std`.
    #[serde(default)]
    pub std_specs: bool,
    /// Further arguments for `charon cargo`, before `--dest-file`.
    #[serde(default)]
    pub charon_args: Vec<String>,
}

fn default_crate() -> String {
    "kernel".into()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proofs {
    /// The Lake project, relative to the manifest. Extracted Lean goes to its
    /// `generated/` directory.
    #[serde(default = "default_proofs")]
    pub dir: String,
}

impl Default for Proofs {
    fn default() -> Self {
        Proofs {
            dir: default_proofs(),
        }
    }
}

fn default_proofs() -> String {
    "proofs".into()
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    /// Lake targets to build. Empty builds the default targets.
    #[serde(default)]
    pub targets: Vec<String>,
    /// Theorems that must exist and use only the standard axioms, wherever
    /// they are declared. Every theorem in a hand-written module is gated
    /// anyway; this list catches one that was renamed or deleted, and covers
    /// theorems that live in a library.
    #[serde(default)]
    pub theorems: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mutate {
    /// Lake targets a mutant must break. Empty builds the default targets.
    #[serde(default)]
    pub targets: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mutant {
    pub name: String,
    /// The file to edit, relative to the manifest. Defaults to the extracted
    /// crate's `src/lib.rs`.
    pub file: Option<String>,
    /// One replacement: `old` must occur exactly once.
    pub old: Option<String>,
    pub new: Option<String>,
    /// Several replacements, applied in order.
    #[serde(default)]
    pub edits: Vec<Edit>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edit {
    pub old: String,
    pub new: String,
}

impl Mutant {
    pub fn edits(&self) -> Result<Vec<Edit>> {
        let mut out = Vec::new();
        match (&self.old, &self.new) {
            (Some(old), Some(new)) => out.push(Edit {
                old: old.clone(),
                new: new.clone(),
            }),
            (None, None) => {}
            _ => bail!("mutant `{}`: `old` and `new` go together", self.name),
        }
        out.extend(self.edits.iter().cloned());
        if out.is_empty() {
            bail!(
                "mutant `{}` changes nothing: give `old`/`new` or `edits`",
                self.name
            );
        }
        Ok(out)
    }
}

/// A loaded manifest and the directory it sits in.
#[derive(Debug)]
pub struct Project {
    pub root: PathBuf,
    pub manifest: Manifest,
}

impl Project {
    pub fn load(file: &Path) -> Result<Project> {
        let text =
            std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
        let manifest: Manifest =
            toml::from_str(&text).with_context(|| format!("parsing {}", file.display()))?;
        let root = file.parent().map(Path::to_path_buf).unwrap_or_default();
        let root = if root.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            root
        };
        let root = root
            .canonicalize()
            .with_context(|| format!("resolving {}", root.display()))?;
        let mut names = std::collections::HashSet::new();
        for m in &manifest.mutants {
            m.edits()?;
            if !names.insert(&m.name) {
                bail!("{}: two mutants are named `{}`", file.display(), m.name);
            }
        }
        Ok(Project { root, manifest })
    }

    /// The project at `path`: a manifest file, or a directory at or below one.
    pub fn find(path: Option<&Path>) -> Result<Project> {
        let start = match path {
            Some(p) => p.to_path_buf(),
            None => std::env::current_dir()?,
        };
        if start.is_file() {
            return Project::load(&start);
        }
        if let Some(f) = locate(&start) {
            return Project::load(&f);
        }
        let start = start
            .canonicalize()
            .with_context(|| format!("{} does not exist", start.display()))?;
        bail!(
            "no {FILE} in {} or above it. `h5i app new <dir>` starts a project; an existing one needs a {FILE} \
             at its root",
            start.display()
        )
    }

    /// Every project under `dir`, sorted by path.
    pub fn discover(dir: &Path) -> Result<Vec<Project>> {
        let mut files = Vec::new();
        walk(dir, &mut files);
        files.sort();
        files.iter().map(|f| Project::load(f)).collect()
    }

    pub fn proofs(&self) -> PathBuf {
        self.root.join(&self.manifest.proofs.dir)
    }

    pub fn generated(&self) -> PathBuf {
        self.proofs().join("generated")
    }

    pub fn krate(&self) -> Option<PathBuf> {
        self.manifest
            .extract
            .as_ref()
            .map(|e| self.root.join(&e.krate))
    }

    /// The project's path for messages: relative to the current directory when it can be.
    pub fn display(&self) -> String {
        display_path(&self.root)
    }
}

/// The manifest in `dir` or the nearest directory above it.
pub fn locate(dir: &Path) -> Option<PathBuf> {
    let dir = dir.canonicalize().ok()?;
    dir.ancestors().map(|d| d.join(FILE)).find(|f| f.is_file())
}

pub fn display_path(p: &Path) -> String {
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|c| c.canonicalize().ok());
    match cwd.as_deref().and_then(|c| p.strip_prefix(c).ok()) {
        Some(r) if r.as_os_str().is_empty() => p
            .file_name()
            .map_or(".".into(), |n| n.to_string_lossy().into_owned()),
        Some(r) => r.display().to_string(),
        None => p.display().to_string(),
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name();
        let name = name.to_string_lossy();
        if p.is_dir() {
            if !matches!(name.as_ref(), "target" | ".lake" | ".git" | "node_modules") {
                walk(&p, out);
            }
        } else if name == FILE {
            out.push(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_manifest() {
        let m: Manifest = toml::from_str(
            r#"
            [extract]
            crate = "kernel"
            start-from = ["transition"]
            schema = true
            include = ["h5i_app_sql"]

            [check]
            theorems = ["k.Theorems.inv"]

            [[mutant]]
            name = "a"
            old = "<="
            new = "<"

            [[mutant]]
            name = "b"
            edits = [{ old = "x", new = "y" }, { old = "p", new = "q" }]
            "#,
        )
        .unwrap();
        let e = m.extract.unwrap();
        assert_eq!(e.krate, "kernel");
        assert!(e.schema && !e.std_specs);
        assert_eq!(m.proofs.dir, "proofs");
        assert_eq!(m.mutants[0].edits().unwrap().len(), 1);
        assert_eq!(m.mutants[1].edits().unwrap().len(), 2);
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(toml::from_str::<Manifest>("[extract]\nstart_from = []").is_err());
    }

    #[test]
    fn a_mutant_must_change_something() {
        let m: Manifest = toml::from_str("[[mutant]]\nname = \"a\"\nold = \"x\"").unwrap();
        assert!(m.mutants[0].edits().is_err());
    }
}
