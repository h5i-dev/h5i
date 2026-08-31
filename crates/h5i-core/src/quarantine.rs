//! Taking a tree from a machine we agreed might be compromised.
//! A runner box's work comes home as a git bundle. Bundles are data, and this
//! one was written by the machine whose whole purpose is to be the one that
//! gets broken into, so nothing in it may touch the repository's object
//! database before it has been looked at (design-runner.md R9).
//! A ref namespace is not a quarantine. Fetching into `refs/h5i/...` writes
//! every object into the *shared* store and only withholds reachability; the
//! objects are there, and a later `git cat-file` finds them. So the bundle is
//! unpacked into a throwaway bare repository with its own object database,
//! checked there, and only the surviving tree is brought across.
//! The bundle is *thin*, carrying `base..tip`, so the quarantine is first given
//! the base commit from the repository we own and only then the untrusted
//! bundle. That keeps the return trip proportional to the work done rather than
//! to the history.
//! What comes out is a tree, never a commit: the history and authorship the
//! runner wrote are discarded, and the caller writes its own commit.

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
/// every caller checking the list first. A convention that holds exactly
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
/// `repo` is the host repository. `base_commit` is what the box was built from.
/// The quarantine is seeded with it so a thin bundle resolves.
pub fn import_tree(
    repo: &git2::Repository,
    bundle: &Path,
    base_commit: &str,
    expect_tree: &str,
    private_rels: &[String],
) -> Result<Inspected, H5iError> {
    // Both of these are interpolated into refspecs below, and both arrive from
    // somewhere else: `base_commit` from a manifest, `expect_tree` from the
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
    fetch(&qdir, &host_git, &format!("{base_commit}:refs/h5i/base"))?;

    // 3. The untrusted part.
    //
    //    `transfer.fsckObjects` is set below and is *not* what makes this safe:
    //    a bundle carrying a tree with a `..` entry, or a `.git` directory
    //    entry, is accepted by that check on git 2.43, and `git fsck --strict`
    //    afterwards reports exactly what the fetch let through. The gate is
    //    `inspect`, which walks the tree itself. The flag stays on because it
    //    costs nothing and catches malformed objects.
    fetch(
        &qdir,
        &bundle.to_string_lossy(),
        &format!("{EXPORT_REF}:refs/h5i/tip"),
    )?;

    let qrepo = git2::Repository::open(&qdir)?;
    let tip = qrepo.find_reference("refs/h5i/tip")?.peel_to_commit()?;

    // The worker described this tree before sending it, and this is where that
    // description is held to. A bundle whose contents differ from what was
    // announced is not a transfer problem, the digest already covered that,
    // it is a peer saying one thing and doing another.
    if tip.tree_id().to_string() != expect_tree {
        return Err(H5iError::Metadata(format!(
            "the runner said it was sending tree {expect_tree} and the bundle carries {}",
            tip.tree_id()
        )));
    }

    let base = qrepo.find_commit(git2::Oid::from_str(base_commit)?)?;

    // The tip has to be the base's descendant. Without this, a runner that
    // answered with an unrelated tree (another box's, or an empty one) would
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
    let walked = inspect(
        &qrepo,
        &tip_tree,
        &base_links,
        private_rels,
        &mut violations,
        &mut private_dropped,
    );

    // Violations first, error second. `TreeWalkResult::Abort` surfaces as a
    // libgit2 "callback returned -1", which would replace the reviewer's
    // sentence with an opaque one *and* skip the violation event the caller
    // files, so the three abort-shaped refusals (too many entries, an
    // over-long path, a path that escapes) would leave no durable mark. A
    // runner wanting to probe quietly would simply choose one of those.
    if !violations.is_empty() {
        for v in &mut violations {
            // A tree entry name may contain any byte but NUL and `/`, `ESC`
            // included, and these strings are printed and stored. Every other
            // peer-supplied string in this codebase is cleaned; this boundary
            // was the one that was not.
            *v = crate::redact::sanitize_display(v);
        }
        return Ok(Inspected::Refused { violations });
    }
    walked?;

    // 5. Only now does anything cross. A commit is made in the quarantine
    //    purely as a carrier (`git fetch` moves refs, not bare trees) and the
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

    // A unique name, and forced. Carrier commits are parentless, so a second
    // one is never a fast-forward: a fixed ref left behind by a crash, or by
    // two proposes at once, would make every later remote propose in this
    // repository fail with a non-fast-forward until someone deleted it by hand.
    let landing = format!("refs/h5i/quarantine-carry/{carrier}");
    fetch(
        &repo.path().to_path_buf(),
        &qdir.to_string_lossy(),
        &format!("+refs/h5i/carry:{landing}"),
    )?;
    // Read the tree, then drop the ref. In that order, and with the drop on
    // every path out including the failing one. Written the other way round the
    // comment claiming "removed on every path" was false whenever `peel`
    // failed, and a surviving landing ref keeps runner content reachable in the
    // host repository.
    let carried = repo
        .find_reference(&landing)
        .and_then(|r| r.peel_to_commit())
        .map(|c| c.tree_id());
    if let Ok(mut r) = repo.find_reference(&landing) {
        let _ = r.delete();
    }
    let tree = carried?;

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

        // An entry whose name is not UTF-8 is refused explicitly. It used to
        // fail closed only by accident: `unwrap_or("")` made the path equal to
        // its directory, which the empty-component check below happened to
        // catch. An accident that holds is still an accident.
        let Some(name) = entry.name() else {
            violations.push(format!(
                "a file name under {dir} that is not valid UTF-8"
            ));
            return git2::TreeWalkResult::Abort;
        };
        let path = format!("{dir}{name}");

        if path.len() > MAX_PATH_LEN {
            violations.push(format!("a path longer than {MAX_PATH_LEN} bytes: {}", &path[..64.min(path.len())]));
            return git2::TreeWalkResult::Abort;
        }

        // A `.git` anywhere below the root is a nested repository coming home
        // in a tree. The root's own is not in a tree at all, so any hit here is
        // one. This is the tree-shaped form of `scan_nested_git`, and it is
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
                // exactly this one. Otherwise the box added or moved a
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
                // The header, not the object. `find_blob(...).size()` reads the
                // *whole inflated blob* to tell you how big it is, so the
                // check for an oversized file was itself the memory bomb, and a
                // highly compressible multi-gigabyte blob fits easily inside
                // the two-gigabyte bundle cap. Worse, the old form was
                // `if let Ok(blob)` with no else: an allocation failure while
                // reading it recorded no violation and let the tree through.
                match repo.odb().and_then(|odb| odb.read_header(entry.id())) {
                    Ok((size, _)) if size as u64 > MAX_BLOB_BYTES => {
                        violations.push(format!(
                            "a file over {} MiB at {path}",
                            MAX_BLOB_BYTES / (1024 * 1024)
                        ));
                    }
                    Ok(_) => {}
                    // Fail closed. Every other ceiling in this file does, and
                    // "we could not measure it" is not "it is small".
                    Err(e) => violations.push(format!(
                        "a file at {path} whose size could not be read ({e})"
                    )),
                }
                // A symlink is a `120000` blob here rather than a filesystem
                // object, so the escape it could perform on a real filesystem
                // is not available to it in a tree. The host never checks
                // this tree out, it commits it, and whoever later checks the
                // branch out gets git's own `verify_path`. Not refused, and
                // not separately recorded either: it is a file like any other
                // at this layer.
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

/// `git fetch`, with the options that make a refspec mean what it appears to.
/// A refspec is *not* a limit on what a fetch writes. git follows tags by
/// default: any `refs/tags/*` on the source side whose target lands in the
/// downloaded set arrives too, under its own name. A compromised runner
/// crafting its own bundle therefore got an arbitrary `refs/tags/<name>` into
/// the quarantine and, on the carrier fetch, into the host repository, with a
/// runner-authored tag object carrying an attacker-chosen tagger and message.
/// That is exactly what this module's header promises cannot happen, and it
/// happened on the *success* path, silently.
/// `--no-write-fetch-head` for the smaller half of the same point.
fn fetch(dir: &PathBuf, from: &str, refspec: &str) -> Result<(), H5iError> {
    git(
        dir,
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-write-fetch-head",
            "--end-of-options",
            from,
            refspec,
        ],
    )
    .map(|_| ())
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

#[cfg(test)]
mod hostile_bundle_tests {
    use super::*;

    fn git_in(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A refspec is not a limit on what a fetch writes.
    /// The threat model says the runner may be compromised, so it does not have
    /// to use our bundle writer: it can craft any bundle bytes it likes. One
    /// carrying a `refs/tags/*` used to land that tag, and a runner-authored
    /// tag object with an attacker-chosen tagger and message, in the host
    /// repository, on the success path, silently.
    #[test]
    fn a_crafted_bundle_cannot_put_its_own_refs_or_objects_in_the_host() {
        let dir = tempfile::tempdir().expect("tempdir");

        let host = dir.path().join("host");
        std::fs::create_dir_all(&host).unwrap();
        git_in(&host, &["init", "--quiet", "."]);
        git_in(&host, &["config", "user.email", "t@example.com"]);
        git_in(&host, &["config", "user.name", "T"]);
        std::fs::write(host.join("a.txt"), b"one").unwrap();
        git_in(&host, &["add", "-A"]);
        git_in(&host, &["commit", "--quiet", "-m", "one"]);
        let base = git_in(&host, &["rev-parse", "HEAD"]);

        // The runner's copy, doing legitimate work and one illegitimate thing.
        let box_dir = dir.path().join("box");
        git_in(dir.path(), &["clone", "--quiet", &host.to_string_lossy(), "box"]);
        git_in(&box_dir, &["config", "user.email", "evil@runner"]);
        git_in(&box_dir, &["config", "user.name", "evil"]);
        std::fs::write(box_dir.join("b.txt"), b"work").unwrap();
        git_in(&box_dir, &["add", "-A"]);
        git_in(&box_dir, &["commit", "--quiet", "-m", "work"]);
        let tip = git_in(&box_dir, &["rev-parse", "HEAD"]);
        let tree = git_in(&box_dir, &["rev-parse", "HEAD^{tree}"]);
        git_in(
            &box_dir,
            &["tag", "-a", "v2.0.0", "-m", "signed-off by the release bot", &tip],
        );
        git_in(&box_dir, &["update-ref", EXPORT_REF, &tip]);

        // A bundle with a tag ref in it, which our own writer would never emit.
        let bundle = dir.path().join("evil.bundle");
        git_in(
            &box_dir,
            &[
                "bundle",
                "create",
                &bundle.to_string_lossy(),
                EXPORT_REF,
                "refs/tags/v2.0.0",
            ],
        );

        let repo = git2::Repository::open(&host).expect("open host");
        let accepted = import_tree(&repo, &bundle, &base, &tree, &[]).expect("import");
        assert!(
            matches!(accepted, Inspected::Accepted { .. }),
            "the work itself is legitimate and must still be accepted"
        );

        // And none of the runner's own naming survived into the host.
        let refs = git_in(&host, &["for-each-ref", "--format=%(refname)"]);
        assert!(
            !refs.contains("refs/tags/"),
            "a runner-chosen tag reached the host: {refs}"
        );
        assert!(
            !refs.contains("quarantine-carry"),
            "the carrier ref was left behind: {refs}"
        );
        assert!(
            repo.find_reference("refs/tags/v2.0.0").is_err(),
            "the runner-authored tag object is reachable in the host"
        );
    }

    /// A tree entry name may contain an escape sequence, and these strings are
    /// printed and stored.
    #[test]
    fn a_violation_carrying_terminal_escapes_is_cleaned_before_it_is_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let host = dir.path().join("host");
        std::fs::create_dir_all(&host).unwrap();
        git_in(&host, &["init", "--quiet", "."]);
        git_in(&host, &["config", "user.email", "t@example.com"]);
        git_in(&host, &["config", "user.name", "T"]);
        std::fs::write(host.join("a.txt"), b"one").unwrap();
        git_in(&host, &["add", "-A"]);
        git_in(&host, &["commit", "--quiet", "-m", "one"]);
        let base = git_in(&host, &["rev-parse", "HEAD"]);

        let box_dir = dir.path().join("box");
        git_in(dir.path(), &["clone", "--quiet", &host.to_string_lossy(), "box"]);
        git_in(&box_dir, &["config", "user.email", "e@e"]);
        git_in(&box_dir, &["config", "user.name", "e"]);
        // A gitlink the base did not have, under a name full of escapes.
        let name = "sub\u{1b}[2K\r\u{1b}[32mok  0 violations\u{1b}[0m";
        let fake = git_in(&box_dir, &["rev-parse", "HEAD"]);
        let entry = format!("160000 commit {fake}\t{name}\n");
        let mktree = std::process::Command::new("git")
            .args(["mktree"])
            .current_dir(&box_dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut c| {
                use std::io::Write;
                c.stdin.as_mut().unwrap().write_all(entry.as_bytes())?;
                c.wait_with_output()
            })
            .expect("mktree");
        let tree = String::from_utf8_lossy(&mktree.stdout).trim().to_string();
        let commit = git_in(
            &box_dir,
            &["commit-tree", &tree, "-p", &base, "-m", "hostile"],
        );
        git_in(&box_dir, &["update-ref", EXPORT_REF, &commit]);
        let bundle = dir.path().join("e.bundle");
        git_in(
            &box_dir,
            &["bundle", "create", &bundle.to_string_lossy(), EXPORT_REF],
        );

        let repo = git2::Repository::open(&host).expect("open");
        match import_tree(&repo, &bundle, &base, &tree, &[]).expect("import") {
            Inspected::Refused { violations } => {
                assert!(!violations.is_empty());
                for v in &violations {
                    assert!(
                        !v.contains('\u{1b}'),
                        "an escape survived into a printed violation: {v:?}"
                    );
                }
            }
            Inspected::Accepted { .. } => panic!("a gitlink the base lacked must be refused"),
        }
    }
}
