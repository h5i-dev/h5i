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
    /// Framework-proven authorization (`docs/design/design-app-authz.md`).
    #[serde(default)]
    pub authz: Authz,
    /// Theorems that discharge a framework-defined obligation
    /// (`H5iAppLib.Obligation`): `check` requires each to have exactly the
    /// obligation's statement for this app (`crate::obligations`).
    #[serde(default)]
    pub obligations: Obligations,
    /// Optional temporal verification settings (`[verification]`).
    /// Disabled by default so Veil is never required unless opted in.
    #[serde(default)]
    pub verification: Verification,
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
    /// generated `Schema.lean` has its function extracted, and write that
    /// file (the `lean "..."` path of `schema!`) by running its test.
    #[serde(default)]
    pub schema: bool,
    /// Crates whose items are extracted with the kernel (`--include`).
    #[serde(default)]
    pub include: Vec<String>,
    /// Copy h5i-app-std's specs next to the extraction as `StdSpecs.lean`,
    /// importing it, and `AuthzSpecs.lean` when the kernel calls the
    /// authorization guard. For kernels that include `h5i_app_std`.
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
pub struct Authz {
    /// The kernel's guarded function: `guarded_transition`, which
    /// `h5i_app_std::authz_kernel!` defines as `guard_viewed` of the app's
    /// instances. `h5i app extract` writes `generated/Authz.lean`, which proves
    /// `<kernel>.<fn>.authorized`, `.kernel_apply_eq` and `.read_only_sound`
    /// by instantiating the framework's theorem; `h5i app check` gates them.
    #[serde(default)]
    pub guarded: Vec<String>,
    /// The app's own storage theorem (A4: the store commits what `apply`
    /// computes), when `extract` cannot generate one. Absent, the generated
    /// `Committed.*` theorems are required, and `extract` fails if it cannot
    /// write them. The same slot as `[obligations] storage`: `check` requires
    /// the theorem to state `H5iAppLib.Obligation.Storage` for the kernel.
    pub storage: Option<String>,
}

/// `[obligations]`: which theorem discharges which obligation. Each value is
/// a theorem name; `check` gates it like `[check] theorems` and also fails
/// unless its type is the obligation's, instantiated with this app's
/// extracted functions. A theorem listed only under `[check] theorems` is
/// gated for axioms and claims no obligation.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Obligations {
    /// `H5iAppLib.Obligation.ReadOnlySound` of the kernel's `read_only` and
    /// `transition`: required by `lint` when a `Kernel` overrides `read_only`.
    pub read_only: Option<String>,
    /// `H5iAppLib.Obligation.PanicFree` of the function the engine runs.
    pub panic_free: Option<String>,
    /// `H5iAppLib.Obligation.Authorized` of the kernel's `transition`, with
    /// the app's own `allowed` and `visible` (printed for review).
    pub authorized: Option<String>,
    /// `H5iAppLib.Obligation.Storage` for the kernel's `apply`, `sql_writes`
    /// and `decode`. Same slot as `[authz] storage`.
    pub storage: Option<String>,
    /// Opt in to `tenant-of-token`: the kernel function (`principal`, a
    /// path in the kernel crate) the server gives the authenticator to turn
    /// a token's tenant and user into a principal. `extract` extracts it with
    /// `authz_kernel!`'s `tenant_of`; `check` states
    /// `H5iAppLib.Obligation.TenantOfToken` for the two and proves it with
    /// the theorem below or by automation (else `open`). Needs `[authz]`.
    pub principal: Option<String>,
    /// `H5iAppLib.Obligation.TenantOfToken` (or `TenantOfTokenWith`) of
    /// `tenant_of` and `principal`. Optional: without it, automation.
    pub tenant_of_token: Option<String>,
}

/// One required Lean theorem in `[verification] temporal_theorems`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum TemporalTheoremSpec {
    Name(String),
    Detailed {
        name: String,
        /// Optional expected Lean proposition (`Prop`), verified via `Kernel.isDefEq`.
        #[serde(default)]
        statement: Option<String>,
        /// Verification scope (`implementation_level` or `model_level`).
        /// Defaults to `implementation_level` for Lean theorems.
        #[serde(default)]
        scope: Option<h5i_app_report::VerificationScope>,
    },
}

impl TemporalTheoremSpec {
    pub fn name(&self) -> &str {
        match self {
            Self::Name(n) => n,
            Self::Detailed { name, .. } => name,
        }
    }

    pub fn statement(&self) -> Option<&str> {
        match self {
            Self::Name(_) => None,
            Self::Detailed { statement, .. } => statement.as_deref(),
        }
    }

    pub fn scope(&self) -> h5i_app_report::VerificationScope {
        match self {
            Self::Name(_) => h5i_app_report::VerificationScope::ImplementationLevel,
            Self::Detailed { scope, .. } => {
                scope.unwrap_or(h5i_app_report::VerificationScope::ImplementationLevel)
            }
        }
    }
}

/// One structured Veil finite-state check executed via Veil's Lean API in the
/// trusted nonce-authenticated gate (`[verification] temporal_checks`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemporalVeilCheckSpec {
    /// Obligation name recorded in the report.
    pub name: String,
    /// Veil module name (must provide executable definitions via `#gen_executable` / `#model_check` / `#temporal_check`).
    pub module: String,
    /// `"safety"` (`Veil.ModelChecker.Concrete.findReachableThen`) or `"temporal"` (`Veil.LivenessMC.checkAllWithProgress`).
    #[serde(default = "default_veil_check_kind")]
    pub kind: String,
    /// Required when `kind == "temporal"`: the Veil `temporal [property]` name.
    #[serde(default)]
    pub property: Option<String>,
    /// Finite instantiation term for the Veil module (e.g. `"{ HeatMode := HeatMode_IndT }"`).
    pub instantiation: String,
    /// Optional theory term for the Veil module (defaults to `"{}"`).
    #[serde(default = "default_veil_theory")]
    pub theory: String,
}

fn default_veil_check_kind() -> String {
    "safety".into()
}

fn default_veil_theory() -> String {
    "{}".into()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Verification {
    /// Whether optional temporal verification is enabled for this project.
    /// Defaults to `false` (Veil is strictly optional).
    #[serde(default)]
    pub temporal: bool,
    /// Temporal verification backend (currently `"veil"`, an optional Lean/Veil Lake package).
    #[serde(default = "default_temporal_backend", alias = "temporal-backend")]
    pub temporal_backend: String,
    /// Directory containing optional temporal Lean/Veil proofs, relative to the
    /// manifest. Defaults to `"proofs/temporal"`.
    #[serde(default = "default_temporal_dir", alias = "temporal-dir")]
    pub temporal_dir: String,
    /// Lake targets to build and import in the trusted gate.
    #[serde(default, alias = "temporal-targets")]
    pub temporal_targets: Vec<String>,
    /// Required Veil finite-state model/temporal checks executed via Veil's Lean API.
    #[serde(default, alias = "temporal-checks")]
    pub temporal_checks: Vec<TemporalVeilCheckSpec>,
    /// Required Lean theorems in `temporal_dir` that must be actual theorems
    /// (`ci.isTheorem`), depend only on Lean's three standard axioms, and match
    /// `statement` via `Kernel.isDefEq` when specified.
    #[serde(default, alias = "temporal-theorems")]
    pub temporal_theorems: Vec<TemporalTheoremSpec>,
}

impl Default for Verification {
    fn default() -> Self {
        Verification {
            temporal: false,
            temporal_backend: default_temporal_backend(),
            temporal_dir: default_temporal_dir(),
            temporal_targets: Vec::new(),
            temporal_checks: Vec::new(),
            temporal_theorems: Vec::new(),
        }
    }
}

fn default_temporal_backend() -> String {
    "veil".into()
}

fn default_temporal_dir() -> String {
    "proofs/temporal".into()
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
    /// Only edit inside this function of `file`: `name` for a free function
    /// or method, or `Type::name` for a method of `impl Type` (or `impl Trait
    /// for Type`). Each `old` must then occur exactly once in that function,
    /// not in the whole file.
    pub function: Option<String>,
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
    /// Where the kernel's Cargo builds go, as `CARGO_TARGET_DIR` (which
    /// overrides the user's): set for a mutant's copy, so every build of it
    /// lands in its job slot's directory. `None` builds where Cargo would.
    pub cargo_target: Option<PathBuf>,
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
        // The theorems must be about the function `authz_kernel!` writes,
        // the instantiation `h5i_app_core::Authz<T>` runs, not another one.
        let g = &manifest.authz.guarded;
        if !g.is_empty() && g.as_slice() != ["guarded_transition"] {
            bail!(
                "{}: `[authz] guarded` must be [\"guarded_transition\"], the function `h5i_app_std::authz_kernel!` \
                 writes: the theorems are about it because `App` runs the same guard instantiation",
                file.display()
            );
        }
        let mut names = std::collections::HashSet::new();
        if let (Some(a), Some(b)) = (&manifest.authz.storage, &manifest.obligations.storage)
            && a != b
        {
            bail!(
                "{}: `[authz] storage` ({a}) and `[obligations] storage` ({b}) are the same slot; name one theorem",
                file.display()
            );
        }
        for m in &manifest.mutants {
            m.edits()?;
            if !names.insert(&m.name) {
                bail!("{}: two mutants are named `{}`", file.display(), m.name);
            }
        }
        let o = &manifest.obligations;
        if o.tenant_of_token.is_some() && o.principal.is_none() {
            bail!(
                "{}: `[obligations] tenant-of-token` needs `principal`, the kernel function the server builds principals \
                 with (e.g. `principal = \"principal\"`)",
                file.display()
            );
        }
        if let Some(f) = &o.principal {
            if g.is_empty() || manifest.extract.is_none() {
                bail!(
                    "{}: `[obligations] principal` needs `[extract]` and `[authz]`: the tenant it is proven against is \
                     `authz_kernel!`'s `tenant_of`, the function `AuthzKernel::tenant` calls",
                    file.display()
                );
            }
            let ok = !f.is_empty()
                && f.split("::").all(|s| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_'));
            if !ok || f == "tenant_of" {
                bail!(
                    "{}: `[obligations] principal` must name a function of the kernel crate by path (`principal`, \
                     `auth::principal`), not {f:?}",
                    file.display()
                );
            }
        }
        let backend = manifest.verification.temporal_backend.as_str();
        if backend != "veil" {
            bail!(
                "{}: `[verification] temporal_backend` must be \"veil\", got {backend:?}",
                file.display()
            );
        }
        Ok(Project { root, manifest, cargo_target: None })
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

    pub fn temporal_dir(&self) -> PathBuf {
        self.root.join(&self.manifest.verification.temporal_dir)
    }

    pub fn krate(&self) -> Option<PathBuf> {
        self.manifest
            .extract
            .as_ref()
            .map(|e| self.root.join(&e.krate))
    }

    /// The app's own storage theorem: `[authz] storage` or `[obligations] storage`.
    pub fn storage_theorem(&self) -> Option<&str> {
        self.manifest.authz.storage.as_deref().or(self.manifest.obligations.storage.as_deref())
    }

    /// `[obligations] principal`: the kernel's principal function, by Rust path.
    pub fn principal_fn(&self) -> Option<&str> {
        self.manifest.obligations.principal.as_deref()
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

    #[test]
    fn tenant_of_token_is_opt_in_and_needs_authz() {
        let load = |toml: &str| {
            let d = tempfile::tempdir().unwrap();
            std::fs::write(d.path().join(FILE), toml).unwrap();
            Project::load(&d.path().join(FILE)).map(|p| p.principal_fn().map(str::to_string))
        };
        let authz = "[extract]\n[authz]\nguarded = [\"guarded_transition\"]\n";
        assert_eq!(load(authz).unwrap(), None, "absent: not stated, nothing changes");
        assert_eq!(load(&format!("{authz}[obligations]\nprincipal = \"principal\"\n")).unwrap().as_deref(), Some("principal"));
        assert!(load(&format!("{authz}[obligations]\nprincipal = \"auth::principal\"\ntenant-of-token = \"k.t\"\n")).is_ok());
        assert!(load(&format!("{authz}[obligations]\ntenant-of-token = \"k.t\"\n")).is_err(), "a theorem needs its principal");
        assert!(load("[extract]\n[obligations]\nprincipal = \"principal\"\n").is_err(), "no [authz]: no tenant_of");
        for bad in ["k.principal", "", "tenant_of", "a::"] {
            assert!(load(&format!("{authz}[obligations]\nprincipal = \"{bad}\"\n")).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn verification_section_defaults_to_disabled_and_validates_backend() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(FILE), "[check]\n").unwrap();
        let p = Project::load(&d.path().join(FILE)).unwrap();
        assert!(!p.manifest.verification.temporal);
        assert_eq!(p.manifest.verification.temporal_backend, "veil");
        assert_eq!(p.manifest.verification.temporal_dir, "proofs/temporal");

        std::fs::write(
            d.path().join(FILE),
            "[verification]\ntemporal = true\ntemporal_backend = \"veil\"\n",
        )
        .unwrap();
        let p = Project::load(&d.path().join(FILE)).unwrap();
        assert!(p.manifest.verification.temporal);
        assert_eq!(p.manifest.verification.temporal_backend, "veil");

        std::fs::write(
            d.path().join(FILE),
            "[verification]\ntemporal = true\ntemporal_backend = \"unknown\"\n",
        )
        .unwrap();
        assert!(Project::load(&d.path().join(FILE)).is_err());
    }
}
