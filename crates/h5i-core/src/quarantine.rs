//! Taking a tree from a machine we agreed might be compromised.
//!
//! A runner box's work comes home as a git bundle. Bundles are data, and this
//! one was written by the machine whose whole purpose is to be the one that
//! gets broken into — so nothing in it may touch the repository's object
//! database before it has been looked at (ROADMAP.md R9).
//!
//! **A ref namespace is not a quarantine.** Fetching into `refs/h5i/...` writes
//! every object into the *shared* store and only withholds reachability; the
//! objects are there, and a later `git cat-file` or a mis-scoped command finds
//! them. So the bundle is unpacked into a throwaway bare repository with its
//! own object database, checked there, and only the surviving tree is brought
//! across.
//!
//! The bundle is **thin** — it carries `base..tip` — so the quarantine is first
//! given the base commit from the repository we own, which is a trusted local
//! copy, and only then the untrusted bundle. That keeps the return trip
//! proportional to the work done rather than to the history.
//!
//! What comes out is a tree, never a commit: the history and authorship the
//! runner wrote are discarded, and the caller writes its own commit. The host
//! repository therefore only ever contains commits the host itself authored,
//! over objects a scan reached.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::H5iError;

/// The ref the worker puts its tip under inside the bundle.
const EXPORT_REF: &str = "refs/h5i/export-src";

/// Ceilings on what a bundle may contain.
///
/// Structural, not stylistic: each is a property that would make the *next*
/// step behave badly, and each is checked before that step runs.
const MAX_OBJECTS: usize = 500_000;
const MAX_BLOB_BYTES: u64 = 512 * 1024 * 1024;
const MAX_PATH_LEN: usize = 4096;
const MAX_TREE_ENTRIES: usize = 500_000;

/// What an inspection concluded.
///
/// An enum rather than a struct with a sentinel tree. The first version
/// returned `Oid::zero()` alongside a non-empty violation list and relied on
/// every caller checking the list first — a convention that holds exactly
/// until someone reorders two lines, and whose failure mode is writing a
/// commit over an empty tree. This makes that unrepresentable.
#[derive(Debug, Clone)]
pub enum Inspected {
    /// The tree is in the host repository's object database and may be
    /// committed.
    Accepted {
        tree: git2::Oid,
        /// Paths dropped because the policy calls them private. Not
        /// violations: the box was told it could have them, and the reviewer
        /// was never meant to see them.
        private_dropped: Vec<String>,
    },
    /// Nothing crossed. These are the reviewer's words for why.
    Refused { violations: Vec<String> },
}

/// Unpack, inspect, and bring across.
///
/// `repo` is the host repository. `base_commit` is what the box was built from
/// — the quarantine is seeded with it so a thin bundle resolves.
pub fn import_tree(
    repo: &git2::Repository,
    bundle: &Path,
    base_commit: &str,
    expect_tree: &str,
    private_rels: &[String],
) -> Result<Inspected, H5iError> {
    // Both of these are interpolated into refspecs below, and both arrive from
    // somewhere else — `base_commit` from a manifest, `expect_tree` from the
    // runner itself. The manifest's is validated on import and the runner's is
    // validated on receipt, so this is the third check rather than the first.
    // It is here anyway because this is the module whose entire job is to not
    // rely on someone else having been careful: a `:` in either of them is a
    // refspec, not an object id.
    for (what, value) in [("base commit", base_commit), ("expected tree", expect_tree)] {
        let ok = (7..=64).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_hexdigit());
        if !ok {
            return Err(H5iError::Metadata(format!(
                "{what} is not an object id, so it will not be used to build a refspec"
            )));
        }
    }
    let scratch = tempfile::tempdir()
        .map_err(|e| H5iError::Metadata(format!("could not make a quarantine: {e}")))?;
    let qdir = scratch.path().join("q");

    // 1. A repository of its own. `--bare` because nothing is ever checked out
    //    of it: a working tree would be a place for a hostile path to land on
    //    a real filesystem, which is the whole class this avoids.
    git(&scratch.path().to_path_buf(), &["init", "--quiet", "--bare", &qdir.to_string_lossy()])?;

    // 2. The base, from the repository we own. Trusted, local, and what makes
    //    the thin bundle resolvable.
    let host_git = repo.path().to_string_lossy().to_string();
    git(
        &qdir,
        &[
            "fetch",
            "--quiet",
            "--end-of-options",
            &host_git,
            &format!("{base_commit}:refs/h5i/base"),
        ],
    )?;

    // 3. The untrusted part, with git's own structural checking on.
    git(
        &qdir,
        &[
            "fetch",
            "--quiet",
            "--end-of-options",
            &bundle.to_string_lossy(),
            &format!("{EXPORT_REF}:refs/h5i/tip"),
        ],
    )?;

    let qrepo = git2::Repository::open(&qdir)?;
    let tip = qrepo.find_reference("refs/h5i/tip")?.peel_to_commit()?;

    // The worker described this tree before sending it, and this is where that
    // description is held to. A bundle whose contents differ from what was
    // announced is not a transfer problem — the digest already covered that —
    // it is a peer saying one thing and doing another.
    if tip.tree_id().to_string() != expect_tree {
        return Err(H5iError::Metadata(format!(
            "the runner said it was sending tree {expect_tree} and the bundle carries {}",
            tip.tree_id()
        )));
    }

    let base = qrepo.find_commit(git2::Oid::from_str(base_commit)?)?;

    // The tip has to be the base's descendant. Without this, a runner that
    // answered with an unrelated tree — another box's, or an empty one — would
    // have it written onto this box's branch as a mediated commit. The human
    // reviewing the diff would still catch it, but a check that costs one graph
    // walk should not be left to a human's attention.
    if tip.id() != base.id() && !qrepo.graph_descendant_of(tip.id(), base.id())? {
        return Err(H5iError::Metadata(format!(
            "the runner returned work that does not descend from this box's base \
             ({base_commit}) — it is not this box's history"
        )));
    }

    let mut violations = Vec::new();
    let mut private_dropped = Vec::new();

    // 4. The structural checks, before anything is copied anywhere.
    let base_links = gitlinks_of(&base.tree()?)?;
    let tip_tree = tip.tree()?;
    inspect(&qrepo, &tip_tree, &base_links, private_rels, &mut violations, &mut private_dropped)?;

    if !violations.is_empty() {
        return Ok(Inspected::Refused { violations });
    }

    // 5. Only now does anything cross. A commit is made in the quarantine
    //    purely as a carrier — `git fetch` moves refs, not bare trees — and the
    //    caller throws it away and writes its own.
    let filtered = if private_dropped.is_empty() {
        tip.tree_id()
    } else {
        filtered_tree(&qrepo, &tip_tree, private_rels)?
    };
    let sig = git2::Signature::now("h5i", "h5i@localhost")?;
    let carrier = {
        let tree = qrepo.find_tree(filtered)?;
        qrepo.commit(None, &sig, &sig, "carrier", &tree, &[])?
    };
    qrepo.reference("refs/h5i/carry", carrier, true, "carry")?;

    git(
        &repo.path().to_path_buf(),
        &[
            "fetch",
            "--quiet",
            "--end-of-options",
            &qdir.to_string_lossy(),
            "refs/h5i/carry:refs/h5i/quarantine-carry",
        ],
    )?;
    let carried = repo
        .find_reference("refs/h5i/quarantine-carry")?
        .peel_to_commit()?;
    let tree = carried.tree_id();
    // The carrier's own commit object is now unreferenced and collectable; the
    // tree and its blobs are what was wanted and what stays.
    if let Ok(mut r) = repo.find_reference("refs/h5i/quarantine-carry") {
        r.delete()?;
    }

    Ok(Inspected::Accepted {
        tree,
        private_dropped,
    })
}

/// Walk the tree, refusing what must not come across.
fn inspect(
    repo: &git2::Repository,
    tree: &git2::Tree,
    base_links: &HashMap<String, git2::Oid>,
    private_rels: &[String],
    violations: &mut Vec<String>,
    private_dropped: &mut Vec<String>,
) -> Result<(), H5iError> {
    let mut entries = 0usize;
    let mut objects = 0usize;

    tree.walk(git2::TreeWalkMode::PreOrder, |dir, entry| {
        entries += 1;
        objects += 1;
        if entries > MAX_TREE_ENTRIES || objects > MAX_OBJECTS {
            violations.push(format!(
                "the exported tree has more than {MAX_TREE_ENTRIES} entries"
            ));
            return git2::TreeWalkResult::Abort;
        }

        let name = entry.name().unwrap_or("");
        let path = format!("{dir}{name}");

        if path.len() > MAX_PATH_LEN {
            violations.push(format!("a path longer than {MAX_PATH_LEN} bytes: {}", &path[..64.min(path.len())]));
            return git2::TreeWalkResult::Abort;
        }

        // A `.git` anywhere below the root is a nested repository coming home
        // in a tree. The root's own is not in a tree at all, so any hit here is
        // one — this is the tree-shaped form of `scan_nested_git`, and it is
        // total where a filesystem walk is a best effort.
        if name.eq_ignore_ascii_case(".git") {
            violations.push(format!("a nested git repository at {path}"));
            return git2::TreeWalkResult::Skip;
        }

        // A path component that climbs, or an absolute one. Git will not
        // normally produce these, which is exactly why a peer that does is
        // worth refusing loudly.
        if path.split('/').any(|c| c == ".." || c.is_empty()) || path.starts_with('/') {
            violations.push(format!("a path that does not stay inside the tree: {path}"));
            return git2::TreeWalkResult::Abort;
        }

        match entry.kind() {
            Some(git2::ObjectType::Commit) => {
                // A gitlink: a submodule pointer. Allowed only if the base had
                // exactly this one — otherwise the box added or moved a
                // submodule, which is a change to what the repository *is*
                // rather than to what it contains.
                match base_links.get(&path) {
                    Some(oid) if *oid == entry.id() => {}
                    _ => violations.push(format!(
                        "a submodule pointer the base did not have, at {path}"
                    )),
                }
            }
            Some(git2::ObjectType::Blob) => {
                if let Ok(blob) = repo.find_blob(entry.id())
                    && blob.size() as u64 > MAX_BLOB_BYTES
                {
                    violations.push(format!(
                        "a file over {} MiB at {path}",
                        MAX_BLOB_BYTES / (1024 * 1024)
                    ));
                }
                // A symlink is a `120000` blob here rather than a filesystem
                // object, so the escape it could perform on a real filesystem
                // is not available to it in a tree. It is recorded for the
                // reviewer and not refused.
                if is_under_private(&path, private_rels) {
                    private_dropped.push(path.clone());
                }
            }
            _ => {
                if is_under_private(&path, private_rels) {
                    private_dropped.push(path.clone());
                }
            }
        }
        git2::TreeWalkResult::Ok
    })?;
    Ok(())
}

/// Rebuild the tree without the paths the policy calls private.
fn filtered_tree(
    repo: &git2::Repository,
    tree: &git2::Tree,
    private_rels: &[String],
) -> Result<git2::Oid, H5iError> {
    // An index is the shortest correct way to rewrite a tree: it flattens the
    // whole thing, drops what should go, and writes the nesting back.
    let mut index = git2::Index::new()?;
    index.read_tree(tree)?;
    let doomed: Vec<String> = index
        .iter()
        .filter_map(|e| String::from_utf8(e.path.clone()).ok())
        .filter(|p| is_under_private(p, private_rels))
        .collect();
    for path in doomed {
        index.remove_path(Path::new(&path))?;
    }
    index.set_version(2)?;
    Ok(index.write_tree_to(repo)?)
}

fn gitlinks_of(tree: &git2::Tree) -> Result<HashMap<String, git2::Oid>, H5iError> {
    let mut out = HashMap::new();
    tree.walk(git2::TreeWalkMode::PreOrder, |dir, entry| {
        if entry.kind() == Some(git2::ObjectType::Commit) {
            out.insert(format!("{dir}{}", entry.name().unwrap_or("")), entry.id());
        }
        git2::TreeWalkResult::Ok
    })?;
    Ok(out)
}

/// Is this path inside one of the policy's private paths? Component-wise, so
/// `secretsauce/` is not caught by a private path of `secret`.
fn is_under_private(path: &str, private_rels: &[String]) -> bool {
    private_rels.iter().any(|rel| {
        let rel = rel.trim_matches('/');
        !rel.is_empty()
            && (path == rel || path.starts_with(&format!("{rel}/")))
    })
}

/// Run git in `dir`, with the same three hardening rules the rest of this
/// codebase's git shell-outs use.
fn git(dir: &PathBuf, args: &[&str]) -> Result<String, H5iError> {
    let out = Command::new("git")
        .arg("-c")
        .arg("core.hooksPath=/dev/null")
        .arg("-c")
        .arg("protocol.ext.allow=never")
        // The untrusted-object check, in the direction it was written for: this
        // is the fetch that reads a machine we agreed might be compromised.
        .arg("-c")
        .arg("transfer.fsckObjects=true")
        .arg("-c")
        .arg("fetch.fsckObjects=true")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| H5iError::Metadata(format!("could not run git: {e}")))?;
    if !out.status.success() {
        return Err(H5iError::Metadata(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_private_path_is_matched_by_component_not_by_prefix() {
        // `secretsauce/` must not be caught by a private path of `secret`, or a
        // reviewer silently loses files nobody meant to hide.
        let rels = vec!["secret".to_string(), "a/b".to_string()];
        assert!(is_under_private("secret", &rels));
        assert!(is_under_private("secret/key.pem", &rels));
        assert!(is_under_private("a/b/c.txt", &rels));
        assert!(!is_under_private("secretsauce/x", &rels));
        assert!(!is_under_private("a/bc", &rels));
        assert!(!is_under_private("other", &rels));
    }

    #[test]
    fn an_empty_private_rel_matches_nothing() {
        // An empty entry would otherwise match the whole tree and silently
        // export nothing at all.
        let rels = vec![String::new(), "/".to_string()];
        assert!(!is_under_private("anything", &rels));
        assert!(!is_under_private("", &rels));
    }
}
