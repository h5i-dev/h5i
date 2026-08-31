//! Git ref plumbing shared by the env state refs.
//!
//! `refs/h5i/env/meta` is an ordinary git ref holding a few JSONL files in its
//! tree, updated with compare-and-swap so concurrent envs can append without a
//! lock. The helpers here are the mechanical part of that: hash a payload,
//! read a blob out of a tree, write a tree, sign a commit, and move a ref only
//! if it has not moved under us.

use git2::{Repository, Signature};
use sha2::{Digest, Sha256};
use std::path::Path;

use crate::error::H5iError;

/// Lowercase hex SHA-256 of `bytes`. Used for content addressing and for the
/// policy digest that pins an env's confinement.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// One compare-and-swap ref update: create `refname` at `new_oid` when `tip`
/// is `None` (`force = false`, so a racing creator wins), otherwise move it to
/// `new_oid` only if it still points at `tip`. Both failure modes, a CAS
/// mismatch (the ref moved under us) and the loose-ref lock being held by a
/// concurrent writer, are retryable; the returned error says which.
pub fn cas_ref_update(
    repo: &Repository,
    refname: &str,
    tip: Option<git2::Oid>,
    new_oid: git2::Oid,
    message: &str,
) -> Result<(), git2::Error> {
    match tip {
        None => repo.reference(refname, new_oid, false, message).map(|_| ()),
        Some(old) => repo
            .reference_matching(refname, new_oid, true, old, message)
            .map(|_| ()),
    }
}

/// Randomized full-jitter sleep before CAS retry `attempt` (no-op before the
/// first attempt). The append loops have no cross-process coordination:
/// without jitter, concurrent writers retry in lockstep and can livelock on
/// the loose-ref lock until the whole attempt budget is spent. Capped at 50ms
/// so a fully spent 64-attempt budget waits ~1.5s on average.
pub fn cas_backoff(attempt: usize) {
    if attempt == 0 {
        return;
    }
    const BASE_US: u64 = 500; // 0.5 ms
    const CAP_US: u64 = 50_000; // 50 ms
    let ceiling = CAP_US.min(BASE_US << attempt.min(7));
    std::thread::sleep(std::time::Duration::from_micros(fastrand::u64(1..=ceiling)));
}

/// Render the last CAS failure for an attempts-exhausted error message, so a
/// held lock ("failed to lock") is distinguishable from a ref that kept
/// moving ("old reference value does not match").
pub fn cas_error_detail(last_err: &Option<git2::Error>) -> String {
    match last_err {
        Some(e) => format!(" (last git error: {e})"),
        None => String::new(),
    }
}

/// Read one path out of a tree as UTF-8, or `None` when it is absent or binary.
pub fn read_blob_from_tree(
    repo: &Repository,
    tree: Option<&git2::Tree>,
    path: &str,
) -> Option<String> {
    let entry = tree?.get_path(Path::new(path)).ok()?;
    let blob = repo.find_blob(entry.id()).ok()?;
    std::str::from_utf8(blob.content()).ok().map(str::to_owned)
}

/// Read one path out of a commit's tree as UTF-8.
pub fn read_file_from_commit(repo: &Repository, oid: git2::Oid, path: &str) -> Option<String> {
    let commit = repo.find_commit(oid).ok()?;
    let tree = commit.tree().ok()?;
    let entry = tree.get_path(Path::new(path)).ok()?;
    let blob = repo.find_blob(entry.id()).ok()?;
    std::str::from_utf8(blob.content()).ok().map(str::to_owned)
}

/// Write `files` into a tree layered over `base` (absent entries are inherited).
pub fn build_tree(
    repo: &Repository,
    base: Option<&git2::Tree>,
    files: &[(&str, &str)],
) -> Result<git2::Oid, H5iError> {
    let mut builder = repo.treebuilder(base)?;
    for (name, content) in files {
        let blob = repo.blob(content.as_bytes())?;
        builder.insert(name, blob, 0o100644)?;
    }
    Ok(builder.write()?)
}

/// The repo's committer identity, falling back to a fixed h5i identity so a
/// repository with no `user.email` can still record state.
pub fn signature(repo: &Repository) -> Result<Signature<'static>, H5iError> {
    repo.signature()
        .or_else(|_| Signature::now("h5i", "h5i@local"))
        .map_err(H5iError::Git)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_is_stable() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn cas_error_detail_distinguishes_empty() {
        assert_eq!(cas_error_detail(&None), "");
    }
}
