//! Where a proof project's Lake packages live, and fetching them.
//!
//! Aeneas pulls in Mathlib, so a project's packages are gigabytes and its
//! first build fetches and unpacks all of them. Every project that requires
//! the same packages shares one copy: `proofs/.lake/packages` is a symlink to
//! `$XDG_CACHE_HOME/h5i/lake/<key>/packages`, the key a hash of the
//! `lean-toolchain` and the lakefile's `require`s. `H5I_LAKE_CACHE` names
//! another cache directory, or `off` for a copy in each project. A project
//! that already has a packages directory of its own keeps it; a symlink made
//! by hand is relinked to the cache (the link only, not what it points at).
//!
//! A fetch is done once a stamp next to the packages records it: a fetch the
//! network cut short is resumed by the next build instead of passed over, and
//! `--refetch` discards the packages and starts again.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::util::{self, Out};

const STAMP: &str = "h5i-packages-ready";
const LOCK: &str = "h5i-packages.lock";

/// The shared cache directory, or `None` for packages in each project.
pub fn cache_root() -> Option<PathBuf> {
    let abs = |p: PathBuf| p.is_absolute().then_some(p);
    match std::env::var_os("H5I_LAKE_CACHE") {
        Some(v) if v == "off" || v == "0" => return None,
        Some(v) if !v.is_empty() => return std::env::current_dir().ok().map(|d| d.join(v)),
        _ => {}
    }
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .and_then(abs)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")).and_then(abs))
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from).and_then(abs))?;
    Some(base.join("h5i/lake"))
}

/// A project's packages: where they are and whether they were fetched.
#[derive(Debug)]
pub struct Packages {
    /// `proofs/.lake/packages`, where Lake looks.
    pub link: PathBuf,
    /// Where the packages are: `link` itself, or what it points to.
    pub dir: PathBuf,
    /// In the shared cache, rather than the project's own.
    pub shared: bool,
    /// What the packages must match: the toolchain and the `require`s.
    fingerprint: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum State {
    Missing,
    /// Fetched in part, or for other `require`s.
    Incomplete,
    Ready,
}

impl Packages {
    pub fn locate(proofs: &Path, root: Option<&Path>) -> Result<Self> {
        let link = proofs.join(".lake/packages");
        let fingerprint = fingerprint(proofs)?;
        let wanted = root.map(|r| r.join(key(&fingerprint)).join("packages"));
        let meta = std::fs::symlink_metadata(&link).ok();
        let (dir, shared) = match meta {
            // With the cache, any symlink is relinked to it: one made by hand
            // may point at another checkout's packages, pinned to anything.
            // Without, a symlink that resolves is followed.
            Some(m) if m.file_type().is_symlink() => match (wanted, link.canonicalize()) {
                (Some(w), _) => (w, true),
                (None, Ok(t)) => (t, false),
                (None, Err(_)) => (link.clone(), false),
            },
            Some(_) => (link.clone(), false),
            None => match wanted {
                Some(w) => (w, true),
                None => (link.clone(), false),
            },
        };
        Ok(Packages { link, dir, shared, fingerprint })
    }

    /// Holds the stamp and the lock.
    fn home(&self) -> PathBuf {
        self.dir.parent().map_or_else(|| self.dir.clone(), Path::to_path_buf)
    }

    pub fn state(&self) -> State {
        match std::fs::read_to_string(self.home().join(STAMP)) {
            Ok(s) if s == self.fingerprint && self.dir.is_dir() => State::Ready,
            _ if self.dir.exists() => State::Incomplete,
            _ => State::Missing,
        }
    }

    /// Point `link` at `dir`, if that is where the packages are to be.
    /// Only the link is replaced; what it pointed at is left alone.
    fn link(&self, out: &Out) -> Result<()> {
        if !self.shared {
            // A symlink to packages that are gone: Lake would fail on it.
            if std::fs::symlink_metadata(&self.link).is_ok() && !self.link.exists() {
                std::fs::remove_file(&self.link)?;
            }
            return Ok(());
        }
        std::fs::create_dir_all(&self.dir)?;
        if std::fs::read_link(&self.link).is_ok_and(|t| t == self.dir) {
            return Ok(());
        }
        if let Ok(old) = std::fs::read_link(&self.link) {
            out.note(&format!("relinking {} from {} to the shared packages", self.link.display(), old.display()));
            std::fs::remove_file(&self.link)?;
        }
        std::fs::create_dir_all(self.link.parent().unwrap())?;
        util::symlink(&self.dir, &self.link)
            .with_context(|| format!("linking {} to {}", self.link.display(), self.dir.display()))
    }
}

/// Make the packages ready to build with, fetching them if they are not.
/// The lock it returns keeps another `h5i app` from fetching into or
/// discarding them while it is held: shared once they are ready, exclusive
/// while this call fetches (and so through the build that follows, which
/// builds the dependencies).
pub fn prepare(proofs: &Path, refetch: bool, out: &Out) -> Result<File> {
    prepare_with(proofs, cache_root().as_deref(), refetch, out, |dir| {
        util::status(Command::new("lake").args(["exe", "cache", "get"]).current_dir(dir), out)
    })
}

fn prepare_with(
    proofs: &Path,
    root: Option<&Path>,
    refetch: bool,
    out: &Out,
    fetch: impl FnOnce(&Path) -> Result<bool>,
) -> Result<File> {
    if refetch {
        discard(proofs, root, out)?;
    }
    let p = Packages::locate(proofs, root)?;
    let home = p.home();
    std::fs::create_dir_all(&home)?;
    let lock = File::create(home.join(LOCK))?;
    lock.lock()?;
    // Before `link`, which creates the directory.
    let state = p.state();
    p.link(out)?;
    match state {
        State::Ready => {
            lock.unlock()?;
            lock.lock_shared()?;
            return Ok(lock);
        }
        State::Incomplete => out.note(&format!(
            "resuming an unfinished fetch of the Lean packages into {}",
            p.dir.display()
        )),
        State::Missing => out.note(&format!(
            "fetching the Lean packages and the Mathlib cache into {} (once{})",
            p.dir.display(),
            if p.shared { " for every project with these requires" } else { "" }
        )),
    }
    let _ = std::fs::remove_file(home.join(STAMP));
    if !fetch(proofs)? {
        bail!(
            "fetching the Lean packages failed (network?). Run again to resume, or pass --refetch to discard {} \
             and start over",
            p.dir.display()
        );
    }
    std::fs::write(home.join(STAMP), &p.fingerprint)?;
    Ok(lock)
}

/// `--refetch`: drop the project's packages. Shared ones go too, as they are
/// what is broken; packages linked by hand from elsewhere are only unlinked.
fn discard(proofs: &Path, root: Option<&Path>, out: &Out) -> Result<()> {
    let p = Packages::locate(proofs, root)?;
    let is_link = std::fs::symlink_metadata(&p.link).is_ok_and(|m| m.file_type().is_symlink());
    let home = p.home();
    if (p.shared || !is_link) && (p.dir.exists() || home.join(STAMP).exists()) {
        let lock = File::create(home.join(LOCK))?;
        lock.lock()?;
        out.note(&format!("removing {}", p.dir.display()));
        if p.dir.exists() {
            std::fs::remove_dir_all(&p.dir).with_context(|| format!("removing {}", p.dir.display()))?;
        }
        let _ = std::fs::remove_file(home.join(STAMP));
    }
    if is_link {
        std::fs::remove_file(&p.link)?;
    }
    Ok(())
}

/// The cache directory name: the toolchain, readable, and a hash of the rest.
fn key(fingerprint: &str) -> String {
    let toolchain = fingerprint.lines().next().unwrap_or("");
    let name: String = toolchain
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' { c } else { '-' })
        .collect();
    // FNV-1a: stable across Rust releases, unlike `DefaultHasher`.
    let h = fingerprint
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    format!("{name}-{h:016x}")
}

/// The toolchain, then each `require` of a git or Reservoir package with
/// whitespace collapsed. A `require` of a local directory is left out: Lake
/// clones nothing for it, so a project that requires h5i's Lean library from a
/// checkout shares packages with the library itself.
fn fingerprint(proofs: &Path) -> Result<String> {
    let toolchain = std::fs::read_to_string(proofs.join("lean-toolchain")).unwrap_or_default();
    let mut lines = vec![toolchain.trim().to_string()];
    if let Ok(text) = std::fs::read_to_string(proofs.join("lakefile.lean")) {
        lines.extend(lean_requires(&text).into_iter().filter(|r| !is_path_require(r)));
    } else if let Ok(text) = std::fs::read_to_string(proofs.join("lakefile.toml")) {
        let doc: toml::Table = text.parse().context("parsing lakefile.toml")?;
        for r in doc.get("require").and_then(|r| r.as_array()).into_iter().flatten() {
            if r.get("path").is_some() {
                continue;
            }
            lines.push(toml::to_string(r)?.split_whitespace().collect::<Vec<_>>().join(" "));
        }
    }
    Ok(lines.join("\n"))
}

/// The `require` statements of a `lakefile.lean`, each on one line. A
/// statement runs until the next line that starts in the first column.
fn lean_requires(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    for line in text.lines() {
        let code = line.split("--").next().unwrap_or("");
        if code.trim().is_empty() {
            continue;
        }
        if !code.starts_with(char::is_whitespace) {
            out.extend(cur.take());
            if code.starts_with("require ") {
                cur = Some(String::new());
            }
        }
        if let Some(c) = &mut cur {
            c.push(' ');
            c.push_str(code);
        }
    }
    out.extend(cur);
    out.into_iter().map(|r| r.split_whitespace().collect::<Vec<_>>().join(" ")).collect()
}

/// `require x from "../dir"`, as opposed to `from git` or a Reservoir name.
fn is_path_require(req: &str) -> bool {
    req.contains(" from \"")
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAKEFILE: &str = r#"import Lake
open Lake DSL

-- Pinned.
require aeneas from git
  "https://github.com/AeneasVerif/aeneas" @ "b86120db" / "backends/lean"

require h5i_app_lib from "LIB"

package NAME

lean_lib Proofs where
  roots := #[`Theorems]
"#;

    fn project(dir: &Path, name: &str, lib: &str) -> PathBuf {
        let proofs = dir.join(name).join("proofs");
        std::fs::create_dir_all(&proofs).unwrap();
        let _ = std::fs::create_dir_all(proofs.join(lib));
        std::fs::write(proofs.join("lean-toolchain"), "leanprover/lean4:v4.31.0\n").unwrap();
        std::fs::write(proofs.join("lakefile.lean"), LAKEFILE.replace("LIB", lib).replace("NAME", name)).unwrap();
        proofs
    }

    #[test]
    fn requires_are_whole_statements() {
        let r = lean_requires(&LAKEFILE.replace("LIB", "../lib"));
        assert_eq!(
            r,
            [
                r#"require aeneas from git "https://github.com/AeneasVerif/aeneas" @ "b86120db" / "backends/lean""#,
                r#"require h5i_app_lib from "../lib""#,
            ]
        );
    }

    #[test]
    fn same_requires_share_a_key() {
        let t = tempfile::tempdir().unwrap();
        let a = project(t.path(), "a", "../../lib");
        let b = project(&t.path().join("deep"), "b", "../../../lib");
        let c = project(t.path(), "c", "../../other");
        let k = |p: &Path| key(&fingerprint(p).unwrap());
        assert_eq!(k(&a), k(&b), "the package name and path depth do not matter");
        assert_eq!(k(&a), k(&c), "nor a local require, for which Lake clones nothing");
        std::fs::write(c.join("lakefile.lean"), LAKEFILE.replace("b86120db", "0123abcd")).unwrap();
        assert_ne!(k(&a), k(&c), "another Aeneas is other packages");
        assert!(k(&a).starts_with("leanprover-lean4-v4.31.0-"), "{}", k(&a));
        std::fs::write(a.join("lean-toolchain"), "leanprover/lean4:v4.32.0\n").unwrap();
        assert_ne!(k(&a), k(&b));
    }

    #[test]
    fn a_failed_fetch_is_resumed_and_refetch_starts_over() {
        let t = tempfile::tempdir().unwrap();
        let cache = t.path().join("cache");
        let a = project(t.path(), "a", "../../lib");
        let out = Out::Log(t.path().join("log"));
        let dir = Packages::locate(&a, Some(&cache)).unwrap().dir;

        let err = prepare_with(&a, Some(&cache), false, &out, |_| {
            std::fs::create_dir_all(dir.join("mathlib")).unwrap();
            Ok(false)
        });
        assert!(err.is_err());
        let p = Packages::locate(&a, Some(&cache)).unwrap();
        assert!(p.shared);
        assert_eq!(std::fs::read_link(&p.link).unwrap(), p.dir);
        assert_eq!(p.state(), State::Incomplete);

        let mut fetched = false;
        drop(prepare_with(&a, Some(&cache), false, &out, |_| {
            fetched = true;
            Ok(true)
        }).unwrap());
        assert!(fetched, "the unfinished fetch runs again");
        assert_eq!(p.state(), State::Ready);
        assert!(p.dir.join("mathlib").exists());

        // Ready, and another project with the same requires shares it.
        let b = project(t.path(), "b", "../../lib");
        drop(prepare_with(&b, Some(&cache), false, &out, |_| panic!("already fetched")).unwrap());
        assert_eq!(Packages::locate(&b, Some(&cache)).unwrap().dir, p.dir);

        let mut fetched = false;
        drop(prepare_with(&a, Some(&cache), true, &out, |_| {
            fetched = true;
            Ok(true)
        }).unwrap());
        assert!(fetched);
        assert!(!p.dir.join("mathlib").exists(), "--refetch starts from nothing");
        assert_eq!(p.state(), State::Ready);
    }

    #[test]
    fn own_packages_are_kept() {
        let t = tempfile::tempdir().unwrap();
        let cache = t.path().join("cache");
        let out = Out::Log(t.path().join("log"));
        let a = project(t.path(), "a", "../../lib");
        std::fs::create_dir_all(a.join(".lake/packages/mathlib")).unwrap();
        // Fetched before stamps existed: fetch again (cheap, resumes).
        let mut fetched = false;
        drop(prepare_with(&a, Some(&cache), false, &out, |_| {
            fetched = true;
            Ok(true)
        }).unwrap());
        assert!(fetched);
        let p = Packages::locate(&a, Some(&cache)).unwrap();
        assert!(!p.shared);
        assert_eq!(p.state(), State::Ready);
        assert!(a.join(".lake").join(STAMP).is_file());

        // Linked by hand to another project's: relinked to the cache, the
        // other project's packages untouched. Without the cache, followed.
        let b = project(t.path(), "b", "../../lib");
        std::fs::create_dir_all(b.join(".lake")).unwrap();
        util::symlink(&a.join(".lake/packages"), &b.join(".lake/packages")).unwrap();
        drop(prepare_with(&b, None, false, &out, |_| panic!("already fetched")).unwrap());
        drop(prepare_with(&b, Some(&cache), false, &out, |_| Ok(true)).unwrap());
        let pb = Packages::locate(&b, Some(&cache)).unwrap();
        assert!(pb.shared);
        assert_eq!(std::fs::read_link(&pb.link).unwrap(), pb.dir);
        assert!(a.join(".lake/packages/mathlib").exists(), "the other project's packages stay");
    }

    #[test]
    fn off_keeps_packages_in_the_project() {
        let t = tempfile::tempdir().unwrap();
        let a = project(t.path(), "a", "../../lib");
        let out = Out::Log(t.path().join("log"));
        drop(prepare_with(&a, None, false, &out, |_| Ok(true)).unwrap());
        let p = Packages::locate(&a, None).unwrap();
        assert!(!p.shared);
        assert_eq!(p.dir, a.join(".lake/packages"));
    }
}
