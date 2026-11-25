//! The toolchain an h5i-app proof project is built with. Charon, Aeneas, the
//! Aeneas Lean library and Lean itself only work in matching versions, and the
//! committed extractions are what this exact set produces. `h5i app new`
//! writes these pins, `h5i app doctor` checks them.

/// The Aeneas commit: the `aeneas` binary and the Lean library both.
pub const AENEAS_REV: &str = "b86120db3183b0107eb5f2637b11c424cd06ef1c";

/// The Charon commit that Aeneas commit expects (its `charon-pin` file).
pub const CHARON_REV: &str = "4bd5a29f6e97ce2201ed35251afe5716e53b0a3a";

/// Lean, as `lean-toolchain` spells it. Aeneas's Lean library fixes it.
pub const LEAN_TOOLCHAIN: &str = "leanprover/lean4:v4.31.0";

/// Where the h5i-app Lean library (`H5iAppLib`) lives: a subdirectory of the
/// h5i repository, required from git at a tag.
pub const H5I_REPO: &str = "https://github.com/h5i-dev/h5i";
pub const LIB_SUBDIR: &str = "crates/h5i-app-core/proofs";

/// The first h5i release whose tag has `LIB_SUBDIR`. v0.4.8 and earlier
/// predate h5i-app's move into the repository.
pub const LIB_SINCE: &str = "v0.4.9";

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn proof_projects() -> Vec<PathBuf> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut out = Vec::new();
        fn walk(d: &Path, out: &mut Vec<PathBuf>) {
            let Ok(es) = std::fs::read_dir(d) else { return };
            for e in es.flatten() {
                let p = e.path();
                let n = e.file_name();
                if p.is_dir()
                    && !matches!(
                        n.to_string_lossy().as_ref(),
                        "target" | ".lake" | ".git" | "node_modules" | "h5i-app-veil" | "temporal"
                    )
                {
                    walk(&p, out);
                } else if n == "lean-toolchain" {
                    out.push(d.to_path_buf());
                }
            }
        }
        walk(&root.join("crates"), &mut out);
        walk(&root.join("examples/app"), &mut out);
        walk(&root.join("examples/embedded"), &mut out);
        // The repository-root package, which republishes the crates' proof
        // libraries for Reservoir.
        out.push(root);
        out
    }

    /// The root package (what Reservoir lists) builds every crate's proof
    /// project, so a new one is not left out of it.
    /// Optional temporal verification backends (like `h5i-app-veil`) are
    /// intentionally kept out of the root `lakefile.lean` so normal builds do
    /// not require Veil.
    #[test]
    fn root_package_has_every_crate_proof_project() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let lakefile = std::fs::read_to_string(root.join("lakefile.lean")).unwrap();
        for e in std::fs::read_dir(root.join("crates")).unwrap().flatten() {
            let name = e.file_name();
            if name == "h5i-app-veil" {
                continue;
            }
            if e.path().join("proofs/lakefile.lean").is_file() {
                let dir = format!("crates/{}/proofs", name.to_string_lossy());
                assert!(
                    lakefile.contains(&format!("srcDir := \"{dir}\"")),
                    "the root lakefile.lean has no library for {dir}"
                );
            }
        }
    }

    /// Every proof project in the repository uses the pinned toolchain, so
    /// what `h5i app new` writes is what the examples are proven with.
    #[test]
    fn repository_matches_the_pins() {
        let projects = proof_projects();
        assert!(projects.len() > 10, "found {projects:?}");
        for d in projects {
            let tc = std::fs::read_to_string(d.join("lean-toolchain")).unwrap();
            assert_eq!(tc.trim(), LEAN_TOOLCHAIN, "{}", d.display());
            let lakefile = std::fs::read_to_string(d.join("lakefile.lean")).unwrap();
            assert!(
                lakefile.contains(&format!("@ \"{AENEAS_REV}\"")),
                "{} pins another Aeneas",
                d.display()
            );
        }
    }
}
