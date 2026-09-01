//! Getting the code there, and proving it arrived intact.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};
use thiserror::Error;

/// The ref a bundle carries its base commit under.
///
/// Under `refs/h5i/` because that namespace is already this product's, and
/// deliberately not under `refs/heads/` so that building a bundle never creates
/// a branch in the user's repository.
pub const BUNDLE_REF: &str = "refs/h5i/bundle-src";

/// The branch the worker fetches it into, inside the box's own clone.
pub const BASE_BRANCH: &str = "h5i-base";

#[derive(Debug, Error)]
pub enum SourceError {
    #[error("could not run `git`: {0}")]
    Spawn(#[source] std::io::Error),

    #[error("git {argv} failed:\n{stderr}")]
    Git { argv: String, stderr: String },

    /// libgit2, used wherever the repository's own configuration is hostile,
    /// which is anywhere inside a box's workspace.
    #[error("git object store: {0}")]
    Git2(#[from] git2::Error),

    /// The box's repository points somewhere outside the box. See
    /// [`open_box_repo`] for the two ways it can and why neither is allowed.
    #[error(
        "this box's {what} is {to}, which is outside the box — refusing to export a          repository that has been pointed somewhere else"
    )]
    Redirected { what: String, to: String },

    #[error("source I/O at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The bytes that arrived are not the bytes that were sent. Checked before
    /// any git command touches the file, so a corrupt or substituted bundle is
    /// never handed to a tool that would act on it.
    #[error(
        "the source that arrived does not match its digest — expected {expected}, \
         got {actual}. Nothing was unpacked."
    )]
    Digest { expected: String, actual: String },

    #[error("the source declared {declared} bytes and {actual} arrived")]
    Size { declared: u64, actual: u64 },
}

impl SourceError {
    fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// What a built bundle turned out to be.
#[derive(Debug, Clone)]
pub struct Bundle {
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
    pub base_commit: String,
}

/// Build a bundle of `base_commit` from `repo`.
///
/// The temporary ref is removed on every path out, including the failure ones:
/// a stray ref in someone's repository is exactly the kind of litter that gets
/// blamed on the tool six months later.
pub fn build_bundle(repo: &Path, base_commit: &str, out: &Path) -> Result<Bundle, SourceError> {
    let resolved = git(repo, &["rev-parse", "--verify", &format!("{base_commit}^{{commit}}")])?
        .trim()
        .to_string();

    git(repo, &["update-ref", BUNDLE_REF, &resolved])?;
    let built = (|| -> Result<(), SourceError> {
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| SourceError::io(parent, e))?;
        }
        git(
            repo,
            &[
                "bundle",
                "create",
                "--end-of-options",
                &out.to_string_lossy(),
                BUNDLE_REF,
            ],
        )?;
        Ok(())
    })();
    // Removed whether or not the bundle was built.
    let _ = git(repo, &["update-ref", "-d", BUNDLE_REF]);
    built?;

    let bytes = std::fs::metadata(out)
        .map_err(|e| SourceError::io(out, e))?
        .len();
    let sha256 = digest_file(out)?;

    Ok(Bundle {
        path: out.to_path_buf(),
        bytes,
        sha256,
        base_commit: resolved,
    })
}

/// Collect an incoming bundle, refusing anything that does not match what was
/// declared.
///
/// The receiver's own cap is applied as well as the declared size, because a
/// declared size is a claim: this is R5's rule at the one RPC that moves real
/// data.
pub struct Receiver {
    path: PathBuf,
    file: std::fs::File,
    hasher: Sha256,
    written: u64,
    declared: u64,
    cap: u64,
}

impl Receiver {
    pub fn new(path: PathBuf, declared: u64, cap: u64) -> Result<Self, SourceError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| SourceError::io(parent, e))?;
        }
        let file = std::fs::File::create(&path).map_err(|e| SourceError::io(&path, e))?;
        Ok(Self {
            path,
            file,
            hasher: Sha256::new(),
            written: 0,
            declared,
            cap,
        })
    }

    /// Take one chunk. Fails the moment the total exceeds either bound, rather
    /// than at the end: the point of a cap is not to write the bytes.
    pub fn chunk(&mut self, bytes: &[u8]) -> Result<(), SourceError> {
        let next = self.written.saturating_add(bytes.len() as u64);
        let limit = self.declared.min(self.cap);
        if next > limit {
            return Err(SourceError::Size {
                declared: limit,
                actual: next,
            });
        }
        self.file
            .write_all(bytes)
            .map_err(|e| SourceError::io(&self.path, e))?;
        self.hasher.update(bytes);
        self.written = next;
        Ok(())
    }

    /// Check what arrived against what was promised.
    pub fn finish(mut self, expected_sha256: &str) -> Result<PathBuf, SourceError> {
        self.file
            .flush()
            .map_err(|e| SourceError::io(&self.path, e))?;
        if self.written != self.declared {
            return Err(SourceError::Size {
                declared: self.declared,
                actual: self.written,
            });
        }
        let actual = hex(&self.hasher.finalize());
        if !actual.eq_ignore_ascii_case(expected_sha256) {
            // The half-verified file must not be left where something could
            // later mistake it for a checked one.
            let _ = std::fs::remove_file(&self.path);
            return Err(SourceError::Digest {
                expected: expected_sha256.to_string(),
                actual,
            });
        }
        Ok(self.path)
    }

    pub fn bytes_written(&self) -> u64 {
        self.written
    }
}

/// Turn a verified bundle into a working tree.
///
/// `git init` plus `git fetch` rather than `git clone`, because the bundle's
/// ref is not a branch (see the module note). Hooks are pointed at an empty
/// directory for the length of every command: the bundle is data from another
/// machine, and no part of unpacking data should be able to run a script.
pub fn materialize(bundle: &Path, work: &Path, base_commit: &str) -> Result<(), SourceError> {
    std::fs::create_dir_all(work).map_err(|e| SourceError::io(work, e))?;

    git(work, &["init", "--quiet", "."])?;
    git(
        work,
        &[
            "fetch",
            "--quiet",
            // Before any string this process did not choose. Without it a path
            // beginning with `-` is read as an option, and
            // `--upload-pack=<cmd>` is an option that runs a command.
            "--end-of-options",
            &bundle.to_string_lossy(),
            &format!("{BUNDLE_REF}:refs/heads/{BASE_BRANCH}"),
        ],
    )?;
    git(work, &["checkout", "--quiet", BASE_BRANCH])?;

    // The commit the box claims to be based on is the commit it actually has.
    // Without this the whole "the bundle is the base identity" argument rests
    // on the transfer alone.
    let head = git(work, &["rev-parse", "HEAD"])?.trim().to_string();
    if !head.eq_ignore_ascii_case(base_commit) {
        return Err(SourceError::Git {
            argv: "rev-parse HEAD".into(),
            stderr: format!(
                "the bundle checked out {head}, and the request said the base was {base_commit}"
            ),
        });
    }
    Ok(())
}

/// Commit whatever the box has now, and bundle it for the trip home.
pub fn export_bundle(work: &Path, base_commit: &str, out: &Path) -> Result<Exported, SourceError> {
    let repo = open_box_repo(work)?;
    let base_oid = git2::Oid::from_str(base_commit).map_err(SourceError::Git2)?;

    let head = repo
        .head()
        .and_then(|h| h.peel_to_commit())
        .map_err(SourceError::Git2)?;

    // Everything, including files the agent never told git about. An export
    // that silently dropped untracked work would be the worst kind of quiet.
    let mut index = repo.index().map_err(SourceError::Git2)?;
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .map_err(SourceError::Git2)?;
    // `add_all` does not notice a tracked file that was deleted; without this,
    // a box that removed a file exports a tree that still has it.
    index
        .update_all(["*"].iter(), None)
        .map_err(SourceError::Git2)?;
    let staged_tree = index.write_tree().map_err(SourceError::Git2)?;

    let tip = if staged_tree == head.tree_id() {
        head.id()
    } else {
        let tree = repo.find_tree(staged_tree).map_err(SourceError::Git2)?;
        // A carrier, not a record: the receiving side takes the tree and
        // authors its own commit (R9), so this signature never reaches the
        // host repository and this message is never read by anyone.
        let sig = git2::Signature::now("h5i runner", "runner@h5i.invalid")
            .map_err(SourceError::Git2)?;
        repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            "h5i: box state for export",
            &tree,
            &[&head],
        )
        .map_err(SourceError::Git2)?
    };
    let tip_tree = repo
        .find_commit(tip)
        .map_err(SourceError::Git2)?
        .tree_id();

    write_thin_bundle(&repo, base_oid, tip, out)?;

    let bytes = std::fs::metadata(out)
        .map_err(|e| SourceError::io(out, e))?
        .len();
    Ok(Exported {
        tip_commit: tip.to_string(),
        tip_tree: tip_tree.to_string(),
        has_changes: staged_tree != head.tree_id() || tip != base_oid,
        bytes,
        sha256: digest_file(out)?,
    })
}

/// Open the box's repository, and refuse one that is not where it claims.
fn open_box_repo(work: &Path) -> Result<git2::Repository, SourceError> {
    let repo = git2::Repository::open(work).map_err(SourceError::Git2)?;
    let want = work
        .canonicalize()
        .map_err(|e| SourceError::io(work, e))?;

    let inside = |p: Option<&Path>, what: &str| -> Result<(), SourceError> {
        let Some(p) = p else {
            return Err(SourceError::Redirected {
                what: what.to_string(),
                to: "nowhere".into(),
            });
        };
        let got = p.canonicalize().map_err(|e| SourceError::io(p, e))?;
        if got.starts_with(&want) {
            Ok(())
        } else {
            Err(SourceError::Redirected {
                what: what.to_string(),
                to: got.display().to_string(),
            })
        }
    };

    inside(Some(repo.path()), "git directory")?;
    inside(repo.workdir(), "working directory")?;
    Ok(repo)
}

/// Write a v2 git bundle of `base..tip` by hand.
///
/// `git bundle create` would be shorter and would mean running the CLI in a
/// repository whose configuration the box controls. See [`export_bundle`]. The
/// format is small enough to write directly:
///
/// ```text
/// # v2 git bundle
/// -<prerequisite sha>
/// <tip sha> <ref>
/// <blank line>
/// <packfile>
/// ```
///
/// The prerequisite line is what makes it thin: the reader must already have
/// that commit.
fn write_thin_bundle(
    repo: &git2::Repository,
    base: git2::Oid,
    tip: git2::Oid,
    out: &Path,
) -> Result<(), SourceError> {
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| SourceError::io(parent, e))?;
    }

    // The objects reachable from the tip and not from the base. The same set
    // `git bundle create base..tip` would pack.
    let mut walk = repo.revwalk().map_err(SourceError::Git2)?;
    walk.push(tip).map_err(SourceError::Git2)?;
    if repo.find_commit(base).is_ok() {
        walk.hide(base).map_err(SourceError::Git2)?;
    }

    let mut builder = repo.packbuilder().map_err(SourceError::Git2)?;
    builder.insert_walk(&mut walk).map_err(SourceError::Git2)?;

    let mut file = std::fs::File::create(out).map_err(|e| SourceError::io(out, e))?;
    let mut header = String::from("# v2 git bundle\n");
    // A prerequisite the reader must already have. Omitted when the tip *is*
    // the base, where there is nothing to require and nothing to send.
    if base != tip {
        header.push_str(&format!("-{base}\n"));
    }
    header.push_str(&format!("{tip} {BUNDLE_TIP_REF}\n\n"));
    file.write_all(header.as_bytes())
        .map_err(|e| SourceError::io(out, e))?;

    // Streamed, not buffered. Collecting the pack into a `Vec` first meant a
    // box that filled its own disk OOM-killed the worker instead of being
    // refused. The client's size check runs on the far side, after the
    // allocation has already happened here.
    let mut wrote = Err(None);
    builder
        .foreach(|chunk| {
            if wrote.is_err() && wrote.as_ref().err().is_some_and(|e: &Option<_>| e.is_some()) {
                return false;
            }
            match file.write_all(chunk) {
                Ok(()) => {
                    wrote = Ok(());
                    true
                }
                Err(e) => {
                    wrote = Err(Some(e));
                    false
                }
            }
        })
        .map_err(SourceError::Git2)?;
    if let Err(Some(e)) = wrote {
        return Err(SourceError::io(out, e));
    }
    file.sync_all().map_err(|e| SourceError::io(out, e))?;
    Ok(())
}

/// The ref an export bundle carries its tip under. Must match what the
/// receiving side fetches.
pub const BUNDLE_TIP_REF: &str = crate::proto::EXPORT_REF;

/// What an export turned out to be.
#[derive(Debug, Clone)]
pub struct Exported {
    pub tip_commit: String,
    pub tip_tree: String,
    pub has_changes: bool,
    pub bytes: u64,
    pub sha256: String,
}

/// An empty box: a repository with no history, so that everything later in the
/// pipeline (a diff, an export) has a repository to work against.
pub fn materialize_empty(work: &Path) -> Result<(), SourceError> {
    std::fs::create_dir_all(work).map_err(|e| SourceError::io(work, e))?;
    git(work, &["init", "--quiet", "."])?;
    Ok(())
}

/// Run git in `dir`, returning stdout.
fn git(dir: &Path, args: &[&str]) -> Result<String, SourceError> {
    // These are *git* options and must precede the subcommand: `git fetch -c x`
    // is an unknown switch, not a configuration override. Putting them here
    // rather than at each call site is also what stops one of them being
    // forgotten at the one call site that needed it.
    let out = Command::new("git")
        .arg("-c")
        .arg("core.hooksPath=/dev/null")
        .arg("-c")
        .arg("protocol.ext.allow=never")
        // Untrusted-object discipline, applied in the direction we currently
        // trust so that it is already here when a later milestone reverses it
        // (R9 fetches the other way, from a machine we have agreed may be
        // compromised).
        .arg("-c")
        .arg("transfer.fsckObjects=true")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(SourceError::Spawn)?;

    if !out.status.success() {
        return Err(SourceError::Git {
            argv: args.join(" "),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn digest_file(path: &Path) -> Result<String, SourceError> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|e| SourceError::io(path, e))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|e| SourceError::io(path, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small repository with two commits.
    fn repo() -> (tempfile::TempDir, PathBuf, String) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("r");
        std::fs::create_dir_all(&path).unwrap();
        git(&path, &["init", "--quiet", "."]).unwrap();
        git(&path, &["config", "user.email", "t@example.com"]).unwrap();
        git(&path, &["config", "user.name", "Test"]).unwrap();
        std::fs::write(path.join("a.txt"), b"one").unwrap();
        git(&path, &["add", "-A"]).unwrap();
        git(&path, &["commit", "--quiet", "-m", "one"]).unwrap();
        std::fs::write(path.join("b.txt"), b"two").unwrap();
        git(&path, &["add", "-A"]).unwrap();
        git(&path, &["commit", "--quiet", "-m", "two"]).unwrap();
        let head = git(&path, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        (dir, path, head)
    }

    #[test]
    fn a_bundle_round_trips_to_the_same_commit() {
        // The whole reason the unit is a bundle: the base is a fact git checks,
        // not a hash we invented.
        let (dir, repo_path, head) = repo();
        let bundle_path = dir.path().join("src.bundle");
        let bundle = build_bundle(&repo_path, &head, &bundle_path).expect("build");

        assert_eq!(bundle.base_commit, head);
        assert!(bundle.bytes > 0);
        assert_eq!(bundle.sha256.len(), 64);

        let work = dir.path().join("work");
        materialize(&bundle_path, &work, &head).expect("materialize");

        assert_eq!(
            git(&work, &["rev-parse", "HEAD"]).unwrap().trim(),
            head,
            "the box is on the commit the request named"
        );
        assert_eq!(std::fs::read_to_string(work.join("a.txt")).unwrap(), "one");
        assert_eq!(std::fs::read_to_string(work.join("b.txt")).unwrap(), "two");
    }

    #[test]
    fn building_a_bundle_leaves_no_ref_behind() {
        // A stray ref in someone's repository is exactly the litter that gets
        // blamed on the tool six months later.
        let (dir, repo_path, head) = repo();
        let before = git(&repo_path, &["for-each-ref"]).unwrap();
        build_bundle(&repo_path, &head, &dir.path().join("s.bundle")).expect("build");
        let after = git(&repo_path, &["for-each-ref"]).unwrap();
        assert_eq!(before, after, "the temporary ref must not survive");
    }

    #[test]
    fn building_a_bundle_creates_no_branch() {
        // `git clone` would need `refs/heads/*`, which is why this fetches
        // instead: creating a branch is a side effect on a repository we are
        // only supposed to be reading.
        let (dir, repo_path, head) = repo();
        let branches_before = git(&repo_path, &["branch", "--list"]).unwrap();
        build_bundle(&repo_path, &head, &dir.path().join("s.bundle")).expect("build");
        assert_eq!(
            git(&repo_path, &["branch", "--list"]).unwrap(),
            branches_before
        );
    }

    #[test]
    fn a_bundle_for_an_older_commit_carries_only_that_history() {
        let (dir, repo_path, _head) = repo();
        let first = git(&repo_path, &["rev-parse", "HEAD~1"])
            .unwrap()
            .trim()
            .to_string();
        let bundle_path = dir.path().join("old.bundle");
        build_bundle(&repo_path, &first, &bundle_path).expect("build");

        let work = dir.path().join("work-old");
        materialize(&bundle_path, &work, &first).expect("materialize");
        assert!(work.join("a.txt").exists());
        assert!(
            !work.join("b.txt").exists(),
            "the later commit is not in a bundle of the earlier one"
        );
    }

    #[test]
    fn a_commit_that_does_not_exist_fails_before_anything_is_written() {
        let (dir, repo_path, _) = repo();
        let out = dir.path().join("nope.bundle");
        assert!(build_bundle(&repo_path, &"0".repeat(40), &out).is_err());
        assert!(!out.exists(), "no bundle for a commit that is not there");
    }

    #[test]
    fn a_receiver_verifies_the_digest_before_anything_unpacks_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("in.bundle");
        let payload = b"some bundle bytes";
        let good = {
            let mut h = Sha256::new();
            h.update(payload);
            hex(&h.finalize())
        };

        let mut rx = Receiver::new(path.clone(), payload.len() as u64, 1024).unwrap();
        rx.chunk(payload).unwrap();
        assert_eq!(rx.finish(&good).unwrap(), path);
        assert!(path.exists());

        // And a digest that does not match takes the file with it, so nothing
        // can later mistake a half-verified file for a checked one.
        let mut rx = Receiver::new(path.clone(), payload.len() as u64, 1024).unwrap();
        rx.chunk(payload).unwrap();
        match rx.finish(&"f".repeat(64)) {
            Err(SourceError::Digest { .. }) => {}
            other => panic!("expected Digest, got {other:?}"),
        }
        assert!(!path.exists(), "a failed transfer leaves nothing behind");
    }

    #[test]
    fn a_receiver_stops_at_the_smaller_of_the_two_bounds() {
        // The declared size is a claim, and the receiver's cap is a budget.
        let dir = tempfile::tempdir().unwrap();

        // Over the declared size.
        let mut rx = Receiver::new(dir.path().join("a"), 4, 1024).unwrap();
        assert!(matches!(
            rx.chunk(b"more than four"),
            Err(SourceError::Size { .. })
        ));

        // Under the declared size but over the receiver's cap.
        let mut rx = Receiver::new(dir.path().join("b"), 1_000_000, 8).unwrap();
        assert!(matches!(
            rx.chunk(b"nine bytes"),
            Err(SourceError::Size { .. })
        ));

        // And a transfer that stops early is not silently accepted.
        let mut rx = Receiver::new(dir.path().join("c"), 100, 1024).unwrap();
        rx.chunk(b"only ten b").unwrap();
        assert!(matches!(rx.finish(&"0".repeat(64)), Err(SourceError::Size { .. })));
    }

    #[test]
    fn a_corrupt_bundle_is_refused_by_git_rather_than_half_unpacked() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.bundle");
        std::fs::write(&bad, b"this is not a git bundle at all").unwrap();
        let work = dir.path().join("work");
        assert!(materialize(&bad, &work, &"a".repeat(40)).is_err());
    }

    #[test]
    fn a_bundle_whose_commit_is_not_the_one_requested_is_refused() {
        // Otherwise "the bundle is the base identity" would rest on the
        // transfer alone, and a transfer only proves the bytes did not change.
        let (dir, repo_path, head) = repo();
        let bundle_path = dir.path().join("s.bundle");
        build_bundle(&repo_path, &head, &bundle_path).expect("build");

        let work = dir.path().join("work");
        let wrong = "0".repeat(40);
        match materialize(&bundle_path, &work, &wrong) {
            Err(SourceError::Git { stderr, .. }) => {
                assert!(stderr.contains(&head), "the message names what it found");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_source_is_still_a_repository() {
        // Everything later in the pipeline (a diff, an export) needs one.
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("empty");
        materialize_empty(&work).expect("init");
        assert!(work.join(".git").exists());
        assert!(git(&work, &["status", "--porcelain"]).is_ok());
    }
}

#[cfg(test)]
mod config_execution_tests {
    use super::*;

    /// The box owns its own `.git/config`, so anything that reads it is a place
    /// the box can run a command on the runner.
    ///
    /// Not hypothetical: the git CLI executes `core.fsmonitor` and
    /// `filter.<name>.clean` from repository config, and the three hardening
    /// flags this module used to rely on cover none of them. Both were
    /// demonstrated running as the runner user before this test existed.
    #[test]
    fn staging_a_hostile_repository_runs_none_of_its_configured_commands() {
        let dir = tempfile::tempdir().expect("tempdir");
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let marker_a = dir.path().join("FSMONITOR_RAN");
        let marker_b = dir.path().join("FILTER_RAN");

        // A repository shaped exactly as a box would leave it.
        let cli = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&work)
                .output()
                .expect("git");
            assert!(out.status.success(), "git {args:?}");
        };
        cli(&["init", "--quiet", "."]);
        cli(&["config", "user.email", "t@example.com"]);
        cli(&["config", "user.name", "T"]);
        std::fs::write(work.join("a.txt"), b"one").unwrap();
        cli(&["add", "-A"]);
        cli(&["commit", "--quiet", "-m", "one"]);
        let base = String::from_utf8(
            std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&work)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();

        // The box now writes its escape into its own repository.
        cli(&[
            "config",
            "core.fsmonitor",
            &format!("touch {}; echo", marker_a.display()),
        ]);
        cli(&[
            "config",
            "filter.evil.clean",
            &format!("touch {}; cat", marker_b.display()),
        ]);
        std::fs::write(work.join(".gitattributes"), b"* filter=evil\n").unwrap();
        std::fs::write(work.join("b.txt"), b"work the agent did").unwrap();

        // And the worker exports it.
        let out = dir.path().join("export.bundle");
        let exported = export_bundle(&work, &base, &out).expect("export");
        assert!(exported.has_changes);

        assert!(
            !marker_a.exists(),
            "core.fsmonitor from the box's config ran on the runner host"
        );
        assert!(
            !marker_b.exists(),
            "filter.evil.clean from the box's config ran on the runner host"
        );

        // And the work still made it out, which is the other half of correct.
        // Read the way the receiving side reads it: a bare repository seeded
        // with the base (the bundle is thin) and then the bundle itself.
        let check = tempfile::tempdir().unwrap();
        let q = check.path().join("q");
        std::fs::create_dir_all(&q).unwrap();
        let g = |dir: &std::path::Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .expect("git");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        g(&q, &["init", "--quiet", "--bare", "."]);
        g(&q, &["fetch", "--end-of-options", &work.to_string_lossy(),
                &format!("{base}:refs/h5i/base")]);
        g(&q, &["fetch", "--end-of-options", &out.to_string_lossy(),
                &format!("{}:refs/h5i/tip", crate::proto::EXPORT_REF)]);

        let listed = std::process::Command::new("git")
            .args(["ls-tree", "-r", "--name-only", "refs/h5i/tip"])
            .current_dir(&q)
            .output()
            .expect("git");
        let names = String::from_utf8_lossy(&listed.stdout);
        assert!(names.contains("b.txt"), "the work is in the bundle: {names}");
        assert!(names.contains("a.txt"), "and so is what was there before");

        let tip = std::process::Command::new("git")
            .args(["rev-parse", "refs/h5i/tip"])
            .current_dir(&q)
            .output()
            .expect("git");
        assert_eq!(
            String::from_utf8_lossy(&tip.stdout).trim(),
            exported.tip_commit,
            "and the bundle's tip is the one the worker described"
        );
    }
}

#[cfg(test)]
mod bundle_edge_cases {
    use super::*;

    fn repo_with(n: usize) -> (tempfile::TempDir, PathBuf, Vec<String>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("r");
        std::fs::create_dir_all(&path).unwrap();
        git(&path, &["init", "--quiet", "."]).unwrap();
        git(&path, &["config", "user.email", "t@example.com"]).unwrap();
        git(&path, &["config", "user.name", "T"]).unwrap();
        let mut commits = Vec::new();
        for i in 0..n {
            std::fs::write(path.join(format!("f{i}.txt")), format!("v{i}")).unwrap();
            git(&path, &["add", "-A"]).unwrap();
            git(&path, &["commit", "--quiet", "-m", &format!("c{i}")]).unwrap();
            commits.push(git(&path, &["rev-parse", "HEAD"]).unwrap().trim().to_string());
        }
        (dir, path, commits)
    }

    /// Read a bundle back the way the receiving side does.
    fn readable(bundle: &Path, base: &Path, base_commit: &str) -> Result<String, String> {
        let q = bundle.parent().unwrap().join(format!(
            "q{}",
            bundle.file_name().unwrap().to_string_lossy()
        ));
        std::fs::create_dir_all(&q).map_err(|e| e.to_string())?;
        let run = |dir: &Path, args: &[&str]| -> Result<String, String> {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .map_err(|e| e.to_string())?;
            if !out.status.success() {
                return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
            }
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        };
        run(&q, &["init", "--quiet", "--bare", "."])?;
        run(&q, &["fetch", "--end-of-options", &base.to_string_lossy(),
                  &format!("{base_commit}:refs/h5i/base")])?;
        run(&q, &["fetch", "--no-tags", "--end-of-options", &bundle.to_string_lossy(),
                  &format!("{}:refs/h5i/tip", crate::proto::EXPORT_REF)])?;
        run(&q, &["rev-parse", "refs/h5i/tip"])
    }

    /// A box that changed nothing still produces a bundle the other side reads.
    ///
    /// This is the `base == tip` case, where the prerequisite line is omitted
    /// and the pack is empty. A bundle format that only worked when there was
    /// something to send would fail exactly on the "the agent did nothing"
    /// path, which is the one a user is least likely to expect an error on.
    #[test]
    fn a_box_that_changed_nothing_still_bundles_and_reads_back() {
        let (dir, work, commits) = repo_with(1);
        let base = commits[0].clone();
        let out = dir.path().join("empty.bundle");

        let exported = export_bundle(&work, &base, &out).expect("export");
        assert!(!exported.has_changes, "nothing was done in this box");
        assert_eq!(exported.tip_commit, base);

        assert_eq!(
            readable(&out, &work, &base).expect("the other side must read it"),
            base
        );
    }

    /// The ordinary case, and the one that proves the prerequisite line works.
    #[test]
    fn a_thin_bundle_needs_the_base_and_carries_only_the_new_work() {
        let (dir, work, commits) = repo_with(3);
        let base = commits[0].clone();
        std::fs::write(work.join("new.txt"), b"agent work").unwrap();

        let out = dir.path().join("thin.bundle");
        let exported = export_bundle(&work, &base, &out).expect("export");
        assert!(exported.has_changes);

        assert_eq!(
            readable(&out, &work, &base).expect("reads"),
            exported.tip_commit
        );

        // And it really is thin: a repository without the base cannot read it.
        let bare = dir.path().join("nobase");
        std::fs::create_dir_all(&bare).unwrap();
        let init = std::process::Command::new("git")
            .args(["init", "--quiet", "--bare", "."])
            .current_dir(&bare)
            .output()
            .unwrap();
        assert!(init.status.success());
        let fetched = std::process::Command::new("git")
            .args([
                "fetch",
                "--end-of-options",
                &out.to_string_lossy(),
                &format!("{}:refs/h5i/tip", crate::proto::EXPORT_REF),
            ])
            .current_dir(&bare)
            .output()
            .unwrap();
        assert!(
            !fetched.status.success(),
            "a thin bundle must not resolve without its prerequisite"
        );
    }

    /// A merge commit has two parents, and the pack has to carry both sides.
    #[test]
    fn a_tip_with_two_parents_bundles_completely() {
        let (dir, work, commits) = repo_with(1);
        let base = commits[0].clone();
        git(&work, &["checkout", "--quiet", "-b", "side"]).unwrap();
        std::fs::write(work.join("side.txt"), b"side").unwrap();
        git(&work, &["add", "-A"]).unwrap();
        git(&work, &["commit", "--quiet", "-m", "side"]).unwrap();
        git(&work, &["checkout", "--quiet", "-"]).unwrap();
        std::fs::write(work.join("main.txt"), b"main").unwrap();
        git(&work, &["add", "-A"]).unwrap();
        git(&work, &["commit", "--quiet", "-m", "main"]).unwrap();
        git(&work, &["merge", "--quiet", "--no-ff", "-m", "merge", "side"]).unwrap();

        let out = dir.path().join("merge.bundle");
        let exported = export_bundle(&work, &base, &out).expect("export");
        let tip = readable(&out, &work, &base).expect("reads");
        assert_eq!(tip, exported.tip_commit);
    }

    /// Deleting a tracked file is work, and an export that dropped it would be
    /// a silently wrong patch rather than a failure.
    #[test]
    fn a_deletion_survives_the_export() {
        let (dir, work, commits) = repo_with(2);
        let base = commits[1].clone();
        std::fs::remove_file(work.join("f0.txt")).unwrap();

        let out = dir.path().join("del.bundle");
        let exported = export_bundle(&work, &base, &out).expect("export");
        assert!(exported.has_changes);

        let q = dir.path().join("qdel");
        std::fs::create_dir_all(&q).unwrap();
        let run = |args: &[&str]| {
            let o = std::process::Command::new("git")
                .args(args)
                .current_dir(&q)
                .output()
                .unwrap();
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        };
        run(&["init", "--quiet", "--bare", "."]);
        run(&[
            "fetch",
            "--end-of-options",
            &work.to_string_lossy(),
            &format!("{base}:refs/h5i/base"),
        ]);
        run(&[
            "fetch",
            "--no-tags",
            "--end-of-options",
            &out.to_string_lossy(),
            &format!("{}:refs/h5i/tip", crate::proto::EXPORT_REF),
        ]);
        let listed = std::process::Command::new("git")
            .args(["ls-tree", "-r", "--name-only", "refs/h5i/tip"])
            .current_dir(&q)
            .output()
            .unwrap();
        let names = String::from_utf8_lossy(&listed.stdout);
        assert!(!names.contains("f0.txt"), "the deletion was lost: {names}");
        assert!(names.contains("f1.txt"));
    }
}

#[cfg(test)]
mod redirection_tests {
    use super::*;

    fn a_repo(at: &Path) -> String {
        std::fs::create_dir_all(at).unwrap();
        git(at, &["init", "--quiet", "."]).unwrap();
        git(at, &["config", "user.email", "t@example.com"]).unwrap();
        git(at, &["config", "user.name", "T"]).unwrap();
        std::fs::write(at.join("mine.txt"), b"mine").unwrap();
        git(at, &["add", "-A"]).unwrap();
        git(at, &["commit", "--quiet", "-m", "one"]).unwrap();
        git(at, &["rev-parse", "HEAD"]).unwrap().trim().to_string()
    }

    /// Moving off the git CLI stopped the box *executing* commands through its
    /// own config. It did not stop the box *redirecting* the export, and
    /// libgit2 honours `core.worktree` too, so a box could stage another box's
    /// workspace into its own bundle and have it shipped to the owner.
    #[test]
    fn a_box_cannot_point_its_export_at_another_boxs_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let victim = dir.path().join("victim/work");
        a_repo(&victim);
        std::fs::write(victim.join("SECRET.txt"), b"another box's secret").unwrap();

        let work = dir.path().join("attacker/work");
        let base = a_repo(&work);
        git(&work, &["config", "core.worktree", &victim.to_string_lossy()]).unwrap();

        let err = export_bundle(&work, &base, &dir.path().join("e.bundle"))
            .expect_err("an export must not stage a path outside the box");
        assert!(
            matches!(err, SourceError::Redirected { .. }),
            "got {err:?}"
        );
    }

    /// A `.git` file pointing elsewhere makes this repository be a different
    /// repository, so the export's carrier commit lands in it and moves its
    /// HEAD.
    #[test]
    fn a_box_cannot_make_its_git_directory_be_someone_elses() {
        let dir = tempfile::tempdir().expect("tempdir");
        let other = dir.path().join("other");
        a_repo(&other);

        let work = dir.path().join("work");
        let base = a_repo(&work);
        std::fs::remove_dir_all(work.join(".git")).unwrap();
        std::fs::write(
            work.join(".git"),
            format!("gitdir: {}\n", other.join(".git").display()),
        )
        .unwrap();

        let err = export_bundle(&work, &base, &dir.path().join("e.bundle"))
            .expect_err("an export must not run in another repository");
        assert!(
            matches!(err, SourceError::Redirected { .. }),
            "got {err:?}"
        );
    }

    /// And an ordinary box still exports.
    #[test]
    fn an_honest_box_is_unaffected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let work = dir.path().join("work");
        let base = a_repo(&work);
        std::fs::write(work.join("new.txt"), b"work").unwrap();
        let e = export_bundle(&work, &base, &dir.path().join("ok.bundle")).expect("exports");
        assert!(e.has_changes);
    }
}
