//! Storage layout validation and recovery helpers.
//!
//! h5i stores durable data in two places:
//! - a filesystem sidecar under the Git common directory: `.git/.h5i/`
//! - Git refs under `refs/h5i/*`
//!
//! This module keeps that layout versioned and gives `h5i doctor` a single
//! place to validate and repair common storage problems.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use git2::Repository;
use serde::Serialize;

use crate::error::H5iError;

pub const STORAGE_SCHEMA_VERSION: u32 = 1;
pub const STORAGE_VERSION_FILE: &str = "storage-version";

const REQUIRED_DIRS: &[&str] = &[
    "metadata",
    "crdt",
    "delta",
    "claims",
    "memory",
    "session_log",
];

const H5I_REFS: &[&str] = &[
    "refs/h5i/notes",
    "refs/h5i/context",
    "refs/h5i/ast",
    "refs/h5i/memory",
];

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DoctorSeverity {
    Ok,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorIssue {
    pub severity: DoctorSeverity,
    pub code: String,
    pub detail: String,
    pub repair: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub ok: bool,
    pub repaired: bool,
    pub h5i_root: PathBuf,
    pub schema_version: Option<u32>,
    pub issues: Vec<DoctorIssue>,
    pub export_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
struct ExportManifest {
    schema_version: u32,
    exported_at: String,
    h5i_root: PathBuf,
    refs: Vec<ExportedRef>,
}

#[derive(Debug, Clone, Serialize)]
struct ExportedRef {
    name: String,
    oid: String,
}

pub fn h5i_root_for_repo(repo: &Repository) -> Result<PathBuf, H5iError> {
    Ok(repo.commondir().join(".h5i"))
}

pub fn ensure_layout(h5i_root: &Path) -> Result<(), H5iError> {
    fs::create_dir_all(h5i_root).map_err(|e| H5iError::with_path(e, h5i_root))?;
    for dir in REQUIRED_DIRS {
        let path = h5i_root.join(dir);
        fs::create_dir_all(&path).map_err(|e| H5iError::with_path(e, path))?;
    }
    write_schema_version_if_missing(h5i_root)?;
    Ok(())
}

pub fn doctor(
    repo: &Repository,
    repair: bool,
    export_dir: Option<&Path>,
) -> Result<DoctorReport, H5iError> {
    let h5i_root = h5i_root_for_repo(repo)?;
    let mut issues = Vec::new();
    let mut repaired = false;

    if !h5i_root.exists() {
        if repair {
            ensure_layout(&h5i_root)?;
            repaired = true;
            issues.push(issue(
                DoctorSeverity::Ok,
                "repaired_sidecar",
                format!("created h5i sidecar directory: {}", h5i_root.display()),
                None::<String>,
            ));
        } else {
            issues.push(issue(
                DoctorSeverity::Error,
                "missing_sidecar",
                format!("h5i sidecar directory is missing: {}", h5i_root.display()),
                Some("create .git/.h5i and required subdirectories"),
            ));
        }
    }

    if h5i_root.exists() {
        for dir in REQUIRED_DIRS {
            let path = h5i_root.join(dir);
            if !path.is_dir() {
                if repair {
                    fs::create_dir_all(&path).map_err(|e| H5iError::with_path(e, &path))?;
                    repaired = true;
                    issues.push(issue(
                        DoctorSeverity::Ok,
                        "repaired_directory",
                        format!("created required storage directory: {}", path.display()),
                        None::<String>,
                    ));
                } else {
                    issues.push(issue(
                        DoctorSeverity::Error,
                        "missing_directory",
                        format!("required storage directory is missing: {}", path.display()),
                        Some(format!("create {}", path.display())),
                    ));
                }
            }
        }
    }

    let schema_version = read_schema_version(&h5i_root)?;
    match schema_version {
        Some(STORAGE_SCHEMA_VERSION) => {}
        Some(v) => issues.push(issue(
            DoctorSeverity::Error,
            "unsupported_schema",
            format!(
                "storage schema version {v} is newer than this h5i binary supports ({STORAGE_SCHEMA_VERSION})"
            ),
            None::<String>,
        )),
        None => {
            if repair && h5i_root.exists() {
                write_schema_version_if_missing(&h5i_root)?;
                repaired = true;
                issues.push(issue(
                    DoctorSeverity::Ok,
                    "repaired_schema_version",
                    "wrote missing storage schema version file".to_string(),
                    None::<String>,
                ));
            } else {
                issues.push(issue(
                    DoctorSeverity::Warning,
                    "missing_schema_version",
                    "storage schema version file is missing".to_string(),
                    Some("write storage-version with the current schema version"),
                ));
            }
        }
    }

    validate_refs(repo, &mut issues);
    validate_claim_files(&h5i_root, &mut issues)?;
    validate_pending_context(&h5i_root, &mut issues)?;

    let export_path = if let Some(dir) = export_dir {
        Some(export_storage(repo, &h5i_root, dir)?)
    } else {
        None
    };

    let ok = !issues
        .iter()
        .any(|issue| issue.severity == DoctorSeverity::Error);
    Ok(DoctorReport {
        ok,
        repaired,
        h5i_root,
        schema_version: read_schema_version_for_report(schema_version, repaired),
        issues,
        export_path,
    })
}

fn issue(
    severity: DoctorSeverity,
    code: impl Into<String>,
    detail: impl Into<String>,
    repair: Option<impl Into<String>>,
) -> DoctorIssue {
    DoctorIssue {
        severity,
        code: code.into(),
        detail: detail.into(),
        repair: repair.map(Into::into),
    }
}

fn read_schema_version(h5i_root: &Path) -> Result<Option<u32>, H5iError> {
    let path = h5i_root.join(STORAGE_VERSION_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path).map_err(|e| H5iError::with_path(e, &path))?;
    let parsed = raw.trim().parse::<u32>().map_err(|e| {
        H5iError::Metadata(format!(
            "invalid storage schema version in {}: {e}",
            path.display()
        ))
    })?;
    Ok(Some(parsed))
}

fn read_schema_version_for_report(previous: Option<u32>, repaired: bool) -> Option<u32> {
    if previous.is_some() {
        previous
    } else if repaired {
        Some(STORAGE_SCHEMA_VERSION)
    } else {
        None
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

fn validate_refs(repo: &Repository, issues: &mut Vec<DoctorIssue>) {
    for name in H5I_REFS {
        match repo.find_reference(name) {
            Ok(reference) => {
                if reference.target().is_none() {
                    issues.push(issue(
                        DoctorSeverity::Error,
                        "invalid_ref",
                        format!("{name} exists but does not point to a direct object"),
                        None::<String>,
                    ));
                }
            }
            Err(_) => issues.push(issue(
                DoctorSeverity::Warning,
                "missing_ref",
                format!("{name} is not present yet"),
                Some(format!("create it by using the corresponding h5i command")),
            )),
        }
    }
}

fn validate_claim_files(h5i_root: &Path, issues: &mut Vec<DoctorIssue>) -> Result<(), H5iError> {
    let dir = h5i_root.join("claims");
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(&dir).map_err(|e| H5iError::with_path(e, &dir))? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let raw = fs::read_to_string(&path).map_err(|e| H5iError::with_path(e, &path))?;
        if let Err(e) = serde_json::from_str::<crate::claims::Claim>(&raw) {
            issues.push(issue(
                DoctorSeverity::Error,
                "corrupt_claim",
                format!("claim file is not valid h5i claim JSON: {} ({e})", path.display()),
                Some("move or delete the corrupt claim file"),
            ));
        }
    }
    Ok(())
}

fn validate_pending_context(h5i_root: &Path, issues: &mut Vec<DoctorIssue>) -> Result<(), H5iError> {
    let path = h5i_root.join("pending_context.json");
    if !path.exists() {
        return Ok(());
    }
    let raw = fs::read_to_string(&path).map_err(|e| H5iError::with_path(e, &path))?;
    if let Err(e) = serde_json::from_str::<crate::metadata::PendingContext>(&raw) {
        issues.push(issue(
            DoctorSeverity::Error,
            "corrupt_pending_context",
            format!("pending_context.json is not valid: {e}"),
            Some("delete pending_context.json; the next hook run will recreate it"),
        ));
    }
    Ok(())
}

fn export_storage(repo: &Repository, h5i_root: &Path, dir: &Path) -> Result<PathBuf, H5iError> {
    let ts = Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let export_root = dir.join(format!("h5i-storage-{ts}"));
    fs::create_dir_all(&export_root).map_err(|e| H5iError::with_path(e, &export_root))?;

    let sidecar_dest = export_root.join("sidecar");
    if h5i_root.exists() {
        copy_dir_recursive(h5i_root, &sidecar_dest)?;
    } else {
        fs::create_dir_all(&sidecar_dest).map_err(|e| H5iError::with_path(e, &sidecar_dest))?;
    }

    let refs = H5I_REFS
        .iter()
        .filter_map(|name| {
            let reference = repo.find_reference(name).ok()?;
            let oid = reference.target()?;
            Some(ExportedRef {
                name: (*name).to_string(),
                oid: oid.to_string(),
            })
        })
        .collect();
    let manifest = ExportManifest {
        schema_version: STORAGE_SCHEMA_VERSION,
        exported_at: Utc::now().to_rfc3339(),
        h5i_root: h5i_root.to_path_buf(),
        refs,
    };
    let manifest_path = export_root.join("manifest.json");
    fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).map_err(H5iError::Serialization)?,
    )
    .map_err(|e| H5iError::with_path(e, manifest_path))?;

    Ok(export_root)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), H5iError> {
    fs::create_dir_all(dst).map_err(|e| H5iError::with_path(e, dst))?;
    for entry in fs::read_dir(src).map_err(|e| H5iError::with_path(e, src))? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else if file_type.is_file() {
            fs::copy(&src_path, &dst_path).map_err(|e| H5iError::with_path(e, dst_path))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn git_repo() -> (tempfile::TempDir, Repository) {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        (dir, repo)
    }

    #[test]
    fn doctor_reports_missing_sidecar_without_repair() {
        let (_dir, repo) = git_repo();

        let report = doctor(&repo, false, None).unwrap();

        assert!(!report.ok);
        assert!(report
            .issues
            .iter()
            .any(|i| i.code == "missing_sidecar" && i.severity == DoctorSeverity::Error));
    }

    #[test]
    fn doctor_repair_creates_layout_and_schema_version() {
        let (_dir, repo) = git_repo();

        let report = doctor(&repo, true, None).unwrap();

        assert!(report.ok);
        assert!(report.repaired);
        assert_eq!(report.schema_version, Some(STORAGE_SCHEMA_VERSION));
        for dir in REQUIRED_DIRS {
            assert!(report.h5i_root.join(dir).is_dir(), "{dir} should exist");
        }
        assert_eq!(
            fs::read_to_string(report.h5i_root.join(STORAGE_VERSION_FILE))
                .unwrap()
                .trim(),
            STORAGE_SCHEMA_VERSION.to_string()
        );
    }

    #[test]
    fn doctor_flags_corrupt_claim_json() {
        let (_dir, repo) = git_repo();
        let h5i_root = h5i_root_for_repo(&repo).unwrap();
        ensure_layout(&h5i_root).unwrap();
        let claims_dir = h5i_root.join("claims");
        fs::write(claims_dir.join("bad.json"), "{not valid json").unwrap();

        let report = doctor(&repo, false, None).unwrap();

        assert!(!report.ok);
        assert!(report.issues.iter().any(|i| i.code == "corrupt_claim"));
    }

    #[test]
    fn doctor_export_writes_manifest_and_sidecar_copy() {
        let (_dir, repo) = git_repo();
        let h5i_root = h5i_root_for_repo(&repo).unwrap();
        ensure_layout(&h5i_root).unwrap();
        fs::write(h5i_root.join("claims").join("note.txt"), "keep me").unwrap();
        let export_parent = tempdir().unwrap();

        let report = doctor(&repo, false, Some(export_parent.path())).unwrap();
        let export_path = report.export_path.expect("export path");

        assert!(export_path.join("manifest.json").is_file());
        assert!(export_path.join("sidecar").join("claims").join("note.txt").is_file());
    }
}
