//! Warm dependency caches.
//!
//! A cold box installs its dependencies from scratch, which is the difference
//! between a twenty second box and a four minute one. That gap is what makes
//! people give up and mount their host directory, so caching is a boundary
//! feature, not a convenience.
//!
//! The shape keeps the boundary intact:
//!
//! * One cache per project *and ecosystem*, keyed by a digest of that
//!   ecosystem's lockfiles. A different lockfile is a different cache, so a box
//!   never sees packages resolved for someone else's dependency set.
//! * Caches are mounted *read only* into a box. Every package manager falls
//!   back to fetching what it cannot find, so a read-only cache is a speed
//!   feature with no correctness cost.
//! * Writing to a cache happens only in [`refresh`], which runs the ecosystem's
//!   fetch step alone, in a throwaway box with no agent in it, with the cache
//!   bound writable at the same path the read-only mount later exposes.
//!
//! The result is that no mutable surface is ever shared between an agent box
//! and anything else.

use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::error::H5iError;

/// A package ecosystem h5i knows how to cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ecosystem {
    /// Name on the CLI (`cargo`, `npm`, `uv`, `go`).
    pub name: &'static str,
    /// Lockfiles that decide the cache key. Missing ones are skipped; a project
    /// with none of them has no cache for this ecosystem.
    pub lockfiles: &'static [&'static str],
    /// Where the package manager keeps its cache inside the box, as a `~`-path.
    /// This is the mount point the read-only grant targets.
    pub box_path: &'static str,
    /// The command `refresh` runs to populate the cache. It must fetch and not
    /// build, so nothing project-specific executes while the cache is writable.
    pub fetch: &'static [&'static str],
    /// Hosts the fetch step must reach. A refresh box gets these and nothing
    /// else: it is the one box that talks to a package registry, so its reach
    /// is spelled out here rather than inherited from a profile.
    pub registries: &'static [&'static str],
}

/// Every ecosystem, in a fixed order so listings are stable.
pub const ECOSYSTEMS: &[Ecosystem] = &[
    Ecosystem {
        name: "cargo",
        registries: &["static.crates.io", "index.crates.io", "crates.io"],
        lockfiles: &["Cargo.lock"],
        box_path: "~/.cargo/registry",
        fetch: &["cargo", "fetch", "--locked"],
    },
    Ecosystem {
        name: "npm",
        registries: &["registry.npmjs.org"],
        lockfiles: &["package-lock.json", "pnpm-lock.yaml", "yarn.lock"],
        box_path: "~/.npm",
        fetch: &["npm", "ci", "--ignore-scripts", "--no-audit", "--no-fund"],
    },
    Ecosystem {
        name: "uv",
        registries: &["pypi.org", "files.pythonhosted.org"],
        lockfiles: &["uv.lock", "requirements.txt"],
        box_path: "~/.cache/uv",
        fetch: &["uv", "sync", "--frozen", "--no-install-project"],
    },
    Ecosystem {
        name: "go",
        registries: &["proxy.golang.org", "sum.golang.org"],
        lockfiles: &["go.sum"],
        box_path: "~/go/pkg/mod",
        fetch: &["go", "mod", "download"],
    },
];

/// Look an ecosystem up by name.
pub fn ecosystem(name: &str) -> Option<&'static Ecosystem> {
    ECOSYSTEMS.iter().find(|e| e.name == name)
}

/// One cache on disk.
#[derive(Debug, Clone, Serialize)]
pub struct CacheEntry {
    pub ecosystem: String,
    /// Digest of the lockfile set this cache was populated for.
    pub key: String,
    pub path: PathBuf,
    /// Total bytes stored, best effort.
    pub bytes: u64,
    /// True when the project's current lockfiles still hash to `key`. A stale
    /// cache is still usable (the package manager fetches what is missing);
    /// it just no longer matches what the project asks for.
    pub current: bool,
}

/// Root of every cache for this repository.
pub fn cache_root(h5i_root: &Path) -> PathBuf {
    h5i_root.join("cache")
}

/// Where `eco`'s cache for lockfile digest `key` lives.
pub fn cache_dir(h5i_root: &Path, eco: &Ecosystem, key: &str) -> PathBuf {
    cache_root(h5i_root).join(eco.name).join(key)
}

/// Digest of `eco`'s lockfiles in `workdir`, or `None` when the project has
/// none of them (nothing to cache, so nothing is created).
///
/// The digest covers the file names as well as their contents, so adding a
/// second lockfile changes the key even if the first is untouched.
pub fn lock_key(workdir: &Path, eco: &Ecosystem) -> Option<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    // Streamed, not buffered. A lockfile lives in `$WORK`, which is the box's
    // own writable workspace, and this runs on every box run, so
    // `std::fs::read` was the box choosing how much memory the host allocates
    // to compute a *digest*. Feeding the same bytes in the same order gives the
    // same digest, so no existing cache key changes.
    let mut hasher = Sha256::new();
    let mut found = false;
    for name in eco.lockfiles {
        let path = workdir.join(name);
        let Ok(mut f) = std::fs::File::open(&path) else {
            continue;
        };
        found = true;
        hasher.update(name.as_bytes());
        hasher.update([0u8]);
        let mut chunk = [0u8; 64 * 1024];
        loop {
            match f.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => hasher.update(&chunk[..n]),
                Err(_) => break,
            }
        }
        hasher.update([0u8]);
    }
    found.then(|| format!("{:x}", hasher.finalize())[..16].to_string())
}

/// Every cache that exists for this repository, oldest ecosystem first.
pub fn list(h5i_root: &Path, workdir: &Path) -> Vec<CacheEntry> {
    let mut out = Vec::new();
    for eco in ECOSYSTEMS {
        let current_key = lock_key(workdir, eco);
        let eco_dir = cache_root(h5i_root).join(eco.name);
        let Ok(rd) = std::fs::read_dir(&eco_dir) else {
            continue;
        };
        for e in rd.flatten() {
            if !e.path().is_dir() {
                continue;
            }
            let key = e.file_name().to_string_lossy().into_owned();
            out.push(CacheEntry {
                ecosystem: eco.name.to_string(),
                current: current_key.as_deref() == Some(key.as_str()),
                bytes: dir_bytes(&e.path()),
                path: e.path(),
                key,
            });
        }
    }
    out
}

/// Remove one cache. `key` of `None` removes every cache for the ecosystem.
pub fn remove(h5i_root: &Path, eco: &Ecosystem, key: Option<&str>) -> Result<usize, H5iError> {
    let target = match key {
        Some(k) => cache_dir(h5i_root, eco, k),
        None => cache_root(h5i_root).join(eco.name),
    };
    if !target.exists() {
        return Ok(0);
    }
    let removed = if key.is_some() {
        1
    } else {
        std::fs::read_dir(&target)
            .map(|rd| rd.flatten().filter(|e| e.path().is_dir()).count())
            .unwrap_or(0)
    };
    std::fs::remove_dir_all(&target).map_err(|e| H5iError::with_path(e, &target))?;
    Ok(removed)
}

/// The caches a box should get, as `(host path, box path)` pairs.
///
/// Only caches whose key matches the project's current lockfiles are offered:
/// handing a box a cache resolved from a different dependency set would be a
/// silent, hard-to-explain source of wrong packages.
pub fn mounts_for(h5i_root: &Path, workdir: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    for eco in ECOSYSTEMS {
        let Some(key) = lock_key(workdir, eco) else {
            continue;
        };
        let dir = cache_dir(h5i_root, eco, &key);
        if dir.is_dir() {
            out.push((dir, eco.box_path.to_string()));
        }
    }
    out
}

/// Create (empty) the cache directory a refresh will populate, and return it.
pub fn prepare(h5i_root: &Path, eco: &Ecosystem, key: &str) -> Result<PathBuf, H5iError> {
    let dir = cache_dir(h5i_root, eco, key);
    std::fs::create_dir_all(&dir).map_err(|e| H5iError::with_path(e, &dir))?;
    Ok(dir)
}

/// Populate `eco`'s cache by running its fetch step in a box with no agent
/// in it, with that one cache writable and nothing else changed.
///
/// This is the only writer. An agent session never gets a cache read-write, so
/// a cache can never become a channel between boxes or a place one leaves
/// something for the next to pick up. The box is removed whether the fetch
/// worked or not: a failed refresh must not leave a box behind.
///
/// Returns the cache directory and the fetch's exit code. A non-zero exit is
/// reported rather than swallowed, because a half-populated cache that claims
/// success is worse than a cold one.
pub fn refresh(
    repo: &git2::Repository,
    h5i_root: &Path,
    workdir: &Path,
    eco: &Ecosystem,
) -> Result<(PathBuf, i32), H5iError> {
    let key = lock_key(workdir, eco).ok_or_else(|| {
        H5iError::Metadata(format!(
            "this project has no {} lockfile ({}), so there is nothing to cache",
            eco.name,
            eco.lockfiles.join(" or ")
        ))
    })?;
    let dir = prepare(h5i_root, eco, &key)?;
    let target = box_target(eco).ok_or_else(|| {
        H5iError::Metadata("cannot resolve HOME to place the cache in the box".to_string())
    })?;
    std::fs::create_dir_all(&target).map_err(|e| H5iError::with_path(e, &target))?;

    // A throwaway box on the `default` profile: build/test confinement, no
    // agent surface, no HOME state. Named for the ecosystem and key so a second
    // refresh of the same set collides loudly instead of racing.
    let slug = format!("cache-{}-{}", eco.name, &key[..8]);
    if crate::env::find(h5i_root, &slug).is_ok() {
        return Err(H5iError::Metadata(format!(
            "a refresh box for {} already exists ({slug}) — another refresh is running, or one              was interrupted: `h5i box rm {slug} --force` clears it",
            eco.name
        )));
    }
    // The refresh box is the one box that talks to a package registry, and no
    // built-in profile fits: `default` denies network (it is the build/test
    // profile) and the agent profiles grant a model API instead. So the project
    // declares the one it wants, and we refuse rather than create a box whose
    // fetch cannot possibly succeed.
    let profile_name = format!("cache-{}", eco.name);
    if crate::sandbox::load_profile(workdir, &profile_name, None).is_err() {
        return Err(H5iError::Metadata(format!(
        "`cache refresh` needs a profile that grants egress to {} and nothing else, and no \
         built-in does: `default` denies network (it is the build/test profile) and the agent \
         profiles grant a model API instead.\n  \
         Declare one in .h5i/env.toml and this becomes a one-liner:\n  \
         \n  \
           [profile.cache-{}]\n  \
           isolation = \"supervised\"\n  \
           net.egress = [{}]\n  \
         \n  \
         The cache directory is prepared at {} and the box would run `{}` with it writable \
         there.",
        eco.registries.join(", "),
        eco.name,
        eco.registries
            .iter()
            .map(|r| format!("\"{r}\""))
            .collect::<Vec<_>>()
            .join(", "),
        dir.display(),
            eco.fetch.join(" "),
        )));
    }

    let opts = crate::env::CreateOpts {
        profile: Some(profile_name),
        ..Default::default()
    };
    let mut m = crate::env::create(repo, h5i_root, workdir, "cache", &slug, opts)?;

    let argv: Vec<String> = eco.fetch.iter().map(|s| s.to_string()).collect();
    let outcome = crate::env::run_with_cache_write(repo, h5i_root, &mut m, &argv, (&dir, &target));
    let _ = crate::env::rm(repo, h5i_root, &m, true);
    Ok((dir, outcome?.exit_code.unwrap_or(1)))
}

/// Absolute path `eco`'s cache appears at inside a box, with `~` expanded.
fn box_target(eco: &Ecosystem) -> Option<PathBuf> {
    match eco.box_path.strip_prefix("~/") {
        Some(rest) => std::env::var("HOME")
            .ok()
            .filter(|h| !h.is_empty())
            .map(|h| PathBuf::from(h).join(rest)),
        None => Some(PathBuf::from(eco.box_path)),
    }
}

/// How deep the cache walk will go.
///
/// A real dependency tree is deep but finite. A symlink loop is not, and while
/// the growing path eventually trips `ENAMETOOLONG` and ends the recursion on
/// its own (measured, rather than assumed) that is hundreds of levels of
/// pointless `read_dir` first, and it is the filesystem's limit doing the job
/// rather than this function's.
const MAX_CACHE_DEPTH: usize = 64;

fn dir_bytes(path: &Path) -> u64 {
    dir_bytes_within(path, 0)
}

/// Total bytes under `path`, without following symlinks.
///
/// `DirEntry::metadata` follows them, and this walks a directory a package
/// manager populated: `npm` plants symlinks in `node_modules` as a matter of
/// course, and a package tarball may carry any link it likes. So a link pointing
/// anywhere else had `h5i box cache list` walking, and reporting the size of, a
/// tree outside the cache entirely; a link pointing at an ancestor had it
/// walking the same subtree over and over until the accumulating path tripped
/// `ENAMETOOLONG`.
///
/// `symlink_metadata` answers about the link rather than its target, which is
/// both the terminating choice and the accurate one: the bytes a link occupies
/// are the link's, and whatever it points at is either counted where it really
/// lives or is not this cache's.
fn dir_bytes_within(path: &Path, depth: usize) -> u64 {
    if depth >= MAX_CACHE_DEPTH {
        return 0;
    }
    let Ok(rd) = std::fs::read_dir(path) else {
        return 0;
    };
    let mut total = 0;
    for e in rd.flatten() {
        match std::fs::symlink_metadata(e.path()) {
            Ok(md) if md.file_type().is_symlink() => total += md.len(),
            Ok(md) if md.is_dir() => total += dir_bytes_within(&e.path(), depth + 1),
            Ok(md) => total += md.len(),
            Err(_) => {}
        }
    }
    total
}

/// Human size, for the listing.
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn cargo() -> &'static Ecosystem {
        ecosystem("cargo").unwrap()
    }

    #[test]
    fn a_project_with_no_lockfile_has_no_key() {
        let td = TempDir::new().unwrap();
        assert!(lock_key(td.path(), cargo()).is_none());
    }

    #[test]
    fn the_key_follows_the_lockfile_contents() {
        let td = TempDir::new().unwrap();
        std::fs::write(td.path().join("Cargo.lock"), "one").unwrap();
        let a = lock_key(td.path(), cargo()).unwrap();
        std::fs::write(td.path().join("Cargo.lock"), "two").unwrap();
        let b = lock_key(td.path(), cargo()).unwrap();
        assert_ne!(a, b, "a different lockfile is a different cache");

        std::fs::write(td.path().join("Cargo.lock"), "one").unwrap();
        assert_eq!(lock_key(td.path(), cargo()).unwrap(), a, "and it is stable");
    }

    #[test]
    fn a_cache_is_offered_only_when_its_key_matches() {
        let root = TempDir::new().unwrap();
        let work = TempDir::new().unwrap();
        std::fs::write(work.path().join("Cargo.lock"), "deps-v1").unwrap();
        let key = lock_key(work.path(), cargo()).unwrap();
        prepare(root.path(), cargo(), &key).unwrap();

        let mounts = mounts_for(root.path(), work.path());
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].1, "~/.cargo/registry");

        // Change the lockfile: the old cache is still on disk but must not be
        // handed to a box resolved against different dependencies.
        std::fs::write(work.path().join("Cargo.lock"), "deps-v2").unwrap();
        assert!(mounts_for(root.path(), work.path()).is_empty());
    }

    #[test]
    fn listing_marks_stale_caches() {
        let root = TempDir::new().unwrap();
        let work = TempDir::new().unwrap();
        std::fs::write(work.path().join("Cargo.lock"), "deps-v1").unwrap();
        let key = lock_key(work.path(), cargo()).unwrap();
        let dir = prepare(root.path(), cargo(), &key).unwrap();
        std::fs::write(dir.join("blob"), vec![0u8; 2048]).unwrap();

        let entries = list(root.path(), work.path());
        assert_eq!(entries.len(), 1);
        assert!(entries[0].current);
        assert!(entries[0].bytes >= 2048);

        std::fs::write(work.path().join("Cargo.lock"), "deps-v2").unwrap();
        assert!(!list(root.path(), work.path())[0].current);
    }

    #[test]
    fn remove_takes_one_cache_or_the_whole_ecosystem() {
        let root = TempDir::new().unwrap();
        let work = TempDir::new().unwrap();
        prepare(root.path(), cargo(), "aaaa").unwrap();
        prepare(root.path(), cargo(), "bbbb").unwrap();

        assert_eq!(remove(root.path(), cargo(), Some("aaaa")).unwrap(), 1);
        assert_eq!(list(root.path(), work.path()).len(), 1);
        assert_eq!(remove(root.path(), cargo(), None).unwrap(), 1);
        assert!(list(root.path(), work.path()).is_empty());
        // Removing what is not there is not an error.
        assert_eq!(remove(root.path(), cargo(), None).unwrap(), 0);
    }

    /// `DirEntry::metadata` follows symlinks, and this walks a directory a
    /// package manager populated: `npm` plants links in `node_modules` as a
    /// matter of course. A link out of the cache had the walk leave it entirely
    /// and report somebody else's bytes; a link back into it had the walk repeat
    /// the same subtree until the path grew too long for the filesystem.
    #[test]
    #[cfg(unix)]
    fn a_symlink_loop_in_a_cache_does_not_take_the_process_down() {
        let root = TempDir::new().unwrap();
        let work = TempDir::new().unwrap();
        std::fs::write(work.path().join("Cargo.lock"), "deps").unwrap();
        let key = lock_key(work.path(), cargo()).unwrap();
        let dir = prepare(root.path(), cargo(), &key).unwrap();

        std::fs::write(dir.join("blob"), vec![0u8; 2048]).unwrap();
        std::fs::create_dir(dir.join("pkg")).unwrap();
        // The loop: a link back to the cache root from inside it.
        std::os::unix::fs::symlink(&dir, dir.join("pkg").join("self")).unwrap();
        // ...and a link out of it entirely, which used to be walked and counted.
        std::os::unix::fs::symlink(work.path(), dir.join("elsewhere")).unwrap();

        let entries = list(root.path(), work.path());
        assert_eq!(entries.len(), 1);
        assert!(entries[0].bytes >= 2048, "the real bytes are still counted");
        assert!(
            entries[0].bytes < 1024 * 1024,
            "the walk left the cache: {} bytes",
            entries[0].bytes
        );
    }

    /// The digest is streamed rather than buffered, and the value it produces
    /// has to be the one it always produced or every warm cache silently
    /// invalidates.
    #[test]
    fn the_lock_digest_is_unchanged_by_being_streamed() {
        use sha2::{Digest, Sha256};
        let work = TempDir::new().unwrap();
        std::fs::write(work.path().join("Cargo.lock"), "deps-v1").unwrap();

        // What the buffered version hashed: `name\0contents\0`.
        let mut buf = Vec::new();
        buf.extend_from_slice(b"Cargo.lock");
        buf.push(0);
        buf.extend_from_slice(b"deps-v1");
        buf.push(0);
        let want = format!("{:x}", Sha256::digest(&buf))[..16].to_string();

        assert_eq!(lock_key(work.path(), cargo()).as_deref(), Some(want.as_str()));
    }

    #[test]
    fn sizes_render_readably() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KiB");
    }
}
