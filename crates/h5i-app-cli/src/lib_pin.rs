//! The h5i Lean library (`h5i_app_lib`) a project requires, and whether it
//! has what this h5i builds against.
//!
//! A project pins the library in `proofs/lakefile.lean`
//! (`require h5i_app_lib from git "…h5i" @ "<rev>" / "crates/h5i-app-core/proofs"`,
//! resolved in `lake-manifest.json`). `h5i app` itself builds library modules
//! that appeared in later releases (`check` builds `H5iAppLib.Obligation`,
//! added in v0.5.4). A project pinned before them failed inside Lake with a
//! bare "no such file"; `doctor` and `check` now name the module, the pinned
//! revision and the fix, and `doctor --fix` moves the pin to this h5i's
//! release.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::util::{self, Out};

pub const LIB: &str = "h5i_app_lib";

/// Library modules `h5i app` builds or imports itself, and the release that
/// added each. Projects' own imports are their business.
pub const REQUIRED: &[(&str, &str)] = &[(crate::check::OBLIGATION_MODULE, "v0.5.4")];

/// How a project gets the library.
#[derive(Debug, PartialEq, Eq)]
pub struct LibPin {
    /// The revision `lakefile.lean` asks for (`@ "<rev>"`); `None` for a
    /// `require` of a local directory or one without a revision.
    pub requested: Option<String>,
    /// What Lake resolved, from `lake-manifest.json`: the input revision
    /// (`v0.5.0`) and the commit.
    pub input_rev: Option<String>,
    pub rev: Option<String>,
    /// A `require` of a local directory (an h5i checkout).
    pub from_path: bool,
    /// The library's Lean sources (`H5iAppLib/…`), if they are on disk.
    pub root: Option<PathBuf>,
}

impl LibPin {
    /// `v0.5.0 (d337c3cfff)`, or what is known of it.
    pub fn describe(&self) -> String {
        if self.from_path {
            return match &self.root {
                Some(r) => format!("a local checkout ({})", r.display()),
                None => "a local checkout".into(),
            };
        }
        let rev = self.input_rev.as_ref().or(self.requested.as_ref());
        match (rev, &self.rev) {
            (Some(r), Some(c)) if r != c => format!("{r} ({})", &c[..c.len().min(10)]),
            (Some(r), _) => r.clone(),
            (None, Some(c)) => c[..c.len().min(10)].to_string(),
            (None, None) => "an unknown revision".into(),
        }
    }

    /// The required modules the library does not have. Empty when its
    /// sources are not on disk yet (nothing to judge before the fetch).
    pub fn missing(&self) -> Vec<(&'static str, &'static str)> {
        let Some(root) = &self.root else { return Vec::new() };
        REQUIRED.iter().copied().filter(|(m, _)| !root.join(module_path(m)).is_file()).collect()
    }
}

/// `H5iAppLib.Obligation` -> `H5iAppLib/Obligation.lean`.
fn module_path(module: &str) -> PathBuf {
    let mut p: PathBuf = module.split('.').collect();
    p.set_extension("lean");
    p
}

/// The project's pin, or `None` if it does not require the library.
pub fn locate(proofs: &Path) -> Option<LibPin> {
    let lakefile = std::fs::read_to_string(proofs.join("lakefile.lean")).ok();
    let requested = lakefile.as_deref().and_then(requested_rev);
    let required = lakefile.as_deref().is_some_and(|t| require_span(t).is_some());
    let entry = std::fs::read_to_string(proofs.join("lake-manifest.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|m| {
            m.get("packages")?
                .as_array()?
                .iter()
                .find(|p| p.get("name").and_then(|n| n.as_str()) == Some(LIB))
                .cloned()
        });
    if !required && entry.is_none() {
        return None;
    }
    let s = |k: &str| entry.as_ref().and_then(|e| e.get(k)).and_then(|v| v.as_str()).map(str::to_string);
    let from_path = s("type").as_deref() == Some("path")
        || (entry.is_none() && lakefile.as_deref().and_then(require_span).is_some_and(|r| !r.contains(" from git")));
    let root = if from_path {
        s("dir").map(|d| proofs.join(d))
    } else {
        let sub = s("subDir").unwrap_or_else(|| crate::pins::LIB_SUBDIR.to_string());
        Some(proofs.join(".lake/packages").join(LIB).join(sub))
    }
    .filter(|r| r.is_dir());
    Some(LibPin { requested, input_rev: s("inputRev"), rev: s("rev"), from_path, root })
}

/// The text of the `require h5i_app_lib …` statement: from `require` to the
/// next blank line or the next top-level statement.
fn require_span(lakefile: &str) -> Option<&str> {
    let start = lakefile.find(&format!("require {LIB}"))?;
    let rest = &lakefile[start..];
    let mut end = rest.len();
    for (i, _) in rest.match_indices('\n') {
        let next = &rest[i + 1..];
        if next.trim_start_matches([' ', '\t']).starts_with('\n') || !next.starts_with([' ', '\t']) {
            end = i;
            break;
        }
    }
    Some(&rest[..end])
}

/// The quoted revision after `@` in the `require`.
fn requested_rev(lakefile: &str) -> Option<String> {
    let span = require_span(lakefile)?;
    let at = span.find('@')?;
    let q1 = at + span[at..].find('"')? + 1;
    let q2 = q1 + span[q1..].find('"')?;
    Some(span[q1..q2].to_string())
}

/// `lakefile.lean` with the library's revision set to `rev`; `None` if the
/// `require` is not a git one with a revision.
pub fn set_requested_rev(lakefile: &str, rev: &str) -> Option<String> {
    let span = require_span(lakefile)?;
    let base = lakefile.find(span)?;
    let at = span.find('@')?;
    let q1 = at + span[at..].find('"')? + 1;
    let q2 = q1 + span[q1..].find('"')?;
    Some(format!("{}{rev}{}", &lakefile[..base + q1], &lakefile[base + q2..]))
}

/// What to do about missing modules, for `doctor` and `check`.
pub fn advice(pin: &LibPin, this_rev: &str, missing: &[(&str, &str)]) -> String {
    let mods = missing.iter().map(|(m, since)| format!("{m} (added in {since})")).collect::<Vec<_>>().join(", ");
    if pin.from_path {
        return format!(
            "the project requires the h5i Lean library from {}, which has no {mods}; this h5i ({this_rev}) builds \
             against it. Update that checkout, or require the library from git at {this_rev}",
            pin.describe()
        );
    }
    format!(
        "the project pins the h5i Lean library at {}, which has no {mods}; this h5i ({this_rev}) builds against \
         it. Run `h5i app doctor --fix` to pin the library at {this_rev} (it edits proofs/lakefile.lean and runs \
         `lake update {LIB}`; Aeneas, Mathlib and the rest stay as they are), then re-run `h5i app check`. Or use an \
         h5i release no newer than the pin",
        pin.describe()
    )
}

/// `doctor --fix`: pin the library at `rev` and let Lake resolve it.
pub fn repin(proofs: &Path, rev: &str, out: &Out) -> Result<()> {
    let file = proofs.join("lakefile.lean");
    let text = std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
    let Some(new) = set_requested_rev(&text, rev) else {
        bail!(
            "{} does not require {LIB} from git with a revision (`@ \"<rev>\"`); edit it by hand",
            file.display()
        );
    };
    if new != text {
        std::fs::write(&file, new).with_context(|| format!("writing {}", file.display()))?;
        out.note(&format!("{}: {LIB} now pinned at {rev}", file.display()));
    }
    // Lake resolves the new revision into lake-manifest.json and checks it
    // out, in the packages `check` uses: link them first and hold their lock
    // (`.lake/packages` may still point elsewhere, and another `h5i app` may
    // be building on them). The library is outside the shared packages' key
    // (crate::packages), so the other packages are not fetched again.
    let _packages = crate::packages::prepare(proofs, false, out)?;
    let mut update = Command::new("lake");
    update.args(["update", LIB]).current_dir(proofs);
    if !util::status(&mut update, out)? {
        bail!("`lake update {LIB}` failed in {}", proofs.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIT: &str = "import Lake\nopen Lake DSL\n\nrequire aeneas from git\n  \"https://github.com/AeneasVerif/aeneas\" @ \"b86120db\" / \"backends/lean\"\n\nrequire h5i_app_lib from git\n  \"https://github.com/h5i-dev/h5i\" @ \"v0.5.0\" / \"crates/h5i-app-core/proofs\"\n\npackage p\n";

    #[test]
    fn reads_and_sets_the_requested_revision() {
        assert_eq!(requested_rev(GIT).as_deref(), Some("v0.5.0"));
        let moved = set_requested_rev(GIT, "v0.5.6").unwrap();
        assert_eq!(requested_rev(&moved).as_deref(), Some("v0.5.6"));
        // Only the library's revision changes; Aeneas keeps its pin.
        assert!(moved.contains("@ \"b86120db\""));
        assert_eq!(moved.replace("v0.5.6", "v0.5.0"), GIT);
    }

    #[test]
    fn a_path_require_has_no_revision() {
        let t = "require h5i_app_lib from \"/x/h5i/crates/h5i-app-core/proofs\"\n\npackage p\n";
        assert_eq!(requested_rev(t), None);
        assert_eq!(set_requested_rev(t, "v0.5.6"), None);
    }

    #[test]
    fn missing_modules_are_found_on_disk() {
        let d = tempfile::tempdir().unwrap();
        let proofs = d.path();
        std::fs::write(proofs.join("lakefile.lean"), GIT).unwrap();
        std::fs::write(
            proofs.join("lake-manifest.json"),
            r#"{"version":"1.1.0","packages":[{"type":"git","name":"h5i_app_lib","rev":"d337c3cfff00","inputRev":"v0.5.0","subDir":"crates/h5i-app-core/proofs"}]}"#,
        )
        .unwrap();
        let lib = proofs.join(".lake/packages/h5i_app_lib/crates/h5i-app-core/proofs/H5iAppLib");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join("Basic.lean"), "").unwrap();
        let pin = locate(proofs).unwrap();
        assert_eq!(pin.describe(), "v0.5.0 (d337c3cfff)");
        assert_eq!(pin.missing(), vec![(crate::check::OBLIGATION_MODULE, "v0.5.4")]);
        let msg = advice(&pin, "v0.5.6", &pin.missing());
        assert!(msg.contains("v0.5.0") && msg.contains("H5iAppLib.Obligation") && msg.contains("doctor --fix"));
        std::fs::write(lib.join("Obligation.lean"), "").unwrap();
        assert!(locate(proofs).unwrap().missing().is_empty());
    }

    #[test]
    fn nothing_to_judge_before_the_fetch() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("lakefile.lean"), GIT).unwrap();
        let pin = locate(d.path()).unwrap();
        assert_eq!(pin.root, None);
        assert!(pin.missing().is_empty());
        assert_eq!(pin.describe(), "v0.5.0");
    }

    #[test]
    fn a_project_without_the_library() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("lakefile.lean"), "import Lake\npackage p\n").unwrap();
        assert_eq!(locate(d.path()), None);
    }
}
