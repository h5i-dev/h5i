//! Getting the code there, and proving it arrived intact.
//!
//! The unit is a **git bundle** rather than a tar (ROADMAP.md R7). A tar of a
//! working tree is a pile of bytes whose only identity is a hash we invented; a
//! bundle carries the commit, so the base the box was built from is a fact git
//! itself can check, and `git bundle verify` is a structural check nobody had
//! to write.
//!
//! Neither side pollutes a branch namespace to do it. The client points a
//! namespaced ref (`refs/h5i/bundle-src`) at the base commit for the length of
//! one `git bundle create` and removes it; the worker fetches that ref out of
//! the bundle into its own `h5i-base` branch. `git clone` cannot be used for
//! this: it only sees `refs/heads/*`, so cloning would mean creating a real
//! branch in the user's repository, which is a side effect on the thing we are
//! only supposed to be reading.
//!
//! **Bundles here carry full history.** `git bundle create` grew rev-list
//! arguments but not `--depth` (checked against git 2.43), so a shallow bundle
//! is not available and this module does not pretend otherwise. For a large
//! repository that is a real cost, and it is the first thing to revisit when
//! the transfer becomes the slow part.

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
            // beginning with `-` is read as an option, and `--upload-pack=<cmd>`
            // is an option that runs a command.
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

/// An empty box: a repository with no history, so that everything later in the
/// pipeline (a diff, an export) has a repository to work against.
pub fn materialize_empty(work: &Path) -> Result<(), SourceError> {
    std::fs::create_dir_all(work).map_err(|e| SourceError::io(work, e))?;
    git(work, &["init", "--quiet", "."])?;
    Ok(())
}

/// Run git in `dir`, returning stdout.
///
/// The three hardening rules the rest of this codebase's git shell-outs use
/// (`env::clone_argv`), applied here for the same reasons:
///
/// - `core.hooksPath` at an empty path, so a repository cannot run a hook. A
///   worker unpacking a bundle is the last place a repository-supplied script
///   should get to run.
/// - `protocol.ext.allow=never`, because `ext::` is a command rather than a
///   URL and the default refusal is configuration an operator may have
///   overridden.
/// - `--end-of-options` at every call site that passes a path, so a leading
///   `-` is a filename rather than a flag.
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
        // Everything later in the pipeline — a diff, an export — needs one.
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("empty");
        materialize_empty(&work).expect("init");
        assert!(work.join(".git").exists());
        assert!(git(&work, &["status", "--porcelain"]).is_ok());
    }
}
