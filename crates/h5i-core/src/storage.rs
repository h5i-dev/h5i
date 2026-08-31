//! The on-disk sidecar layout.
//!
//! h5i keeps its durable state under the Git common directory (`.git/.h5i/`):
//! one directory per environment, holding that env's manifest, resolved
//! policy, worktree, receipts and spools. This module owns creating that root
//! and turning I/O failures on it into actionable errors.

use std::fs;
use std::path::{Path, PathBuf};

use git2::Repository;

use crate::error::H5iError;

pub const STORAGE_SCHEMA_VERSION: u32 = 1;
pub const STORAGE_VERSION_FILE: &str = "storage-version";

const REQUIRED_DIRS: &[&str] = &["env"];

pub fn h5i_root_for_repo(repo: &Repository) -> Result<PathBuf, H5iError> {
    Ok(repo.commondir().join(".h5i"))
}

pub fn ensure_layout(h5i_root: &Path) -> Result<(), H5iError> {
    fs::create_dir_all(h5i_root).map_err(|e| store_io_error(h5i_root, h5i_root, e))?;
    for dir in REQUIRED_DIRS {
        let path = h5i_root.join(dir);
        fs::create_dir_all(&path).map_err(|e| store_io_error(h5i_root, &path, e))?;
    }
    write_schema_version_if_missing(h5i_root)?;
    Ok(())
}

/// Best-effort owner-mismatch detail for an unwritable store (Unix only).
#[cfg(unix)]
fn store_owner_hint(h5i_root: &Path) -> String {
    use std::os::unix::fs::MetadataExt;
    let me = unsafe { libc::geteuid() };
    match fs::metadata(h5i_root) {
        Ok(m) if m.uid() != me => format!(" — store owned by uid {}, you are uid {}", m.uid(), me),
        _ => String::new(),
    }
}
#[cfg(not(unix))]
fn store_owner_hint(_h5i_root: &Path) -> String {
    String::new()
}

/// Turn an I/O error on the h5i data store into an actionable message when it's
/// a permission failure. A raw "Permission denied" deep in a sharded object
/// path, classically the store left root-owned by an earlier `sudo` run, is a
/// notorious head-scratcher; spell out the likely cause and the one-line repair.
/// Non-permission errors pass through with plain path context.
pub fn store_io_error(h5i_root: &Path, path: &Path, source: std::io::Error) -> H5iError {
    if source.kind() == std::io::ErrorKind::PermissionDenied {
        H5iError::Metadata(format!(
            "h5i data store not writable: {path}{owner}\n  \
             The store under {root} appears owned by another user — commonly from an earlier \
             `sudo`/root run.\n  \
             Repair:  sudo chown -R \"$(id -u):$(id -g)\" {root}\n  \
             (Inside an `h5i env` sandbox this is expected: the store is sealed and writes are \
             staged to the spool for host ingest.)",
            path = path.display(),
            owner = store_owner_hint(h5i_root),
            root = h5i_root.display(),
        ))
    } else {
        H5iError::with_path(source, path)
    }
}


fn write_schema_version_if_missing(h5i_root: &Path) -> Result<(), H5iError> {
    let path = h5i_root.join(STORAGE_VERSION_FILE);
    if !path.exists() {
        fs::write(&path, format!("{STORAGE_SCHEMA_VERSION}\n"))
            .map_err(|e| H5iError::with_path(e, path))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn ensure_layout_is_idempotent() {
        let td = TempDir::new().unwrap();
        let root = td.path().join(".h5i");
        ensure_layout(&root).unwrap();
        ensure_layout(&root).unwrap();
        assert!(root.join("env").is_dir());
        assert_eq!(
            fs::read_to_string(root.join(STORAGE_VERSION_FILE)).unwrap().trim(),
            STORAGE_SCHEMA_VERSION.to_string()
        );
    }
}
