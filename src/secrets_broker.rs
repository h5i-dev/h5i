//! Host-side secrets broker (`docs/secrets-broker-design.md`).
//!
//! Resolves a profile's [`SecretGrant`]s from host-side sources at **run time**
//! (never at policy load) and materializes them for injection into the env's
//! child process — capability-scoped, audited, redacted, and **fail-closed**:
//! a declared grant that cannot be resolved or delivered aborts the run rather
//! than running with the credential silently absent.
//!
//! The broker never writes a value to the policy, the manifest, or any git ref.
//! It records only the grant id, source, injection method, ttl, and a value
//! **fingerprint** (sha256 prefix). File-injected secrets are written `0600`
//! outside `$WORK` and unlinked when the [`Brokered`] guard drops.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::H5iError;
use crate::sandbox::SecretGrant;

/// The materialized result of brokering a set of grants: env vars to inject into
/// the child, the values to scrub from captured output, the audit records, and a
/// drop-guard that unlinks any file-injected secrets when the run ends.
pub struct Brokered {
    /// `(KEY, VALUE)` pairs applied to the child after the `env.pass` allowlist.
    /// For `inject=env` this is `(NAME, value)`; for `inject=file` it is
    /// `(NAME_FILE, path)`.
    pub env: Vec<(String, String)>,
    /// Exact secret values to redact from captured output (in addition to h5i's
    /// pattern-based secret scrub).
    pub redactions: Vec<String>,
    /// One audit record per delivered grant (no values).
    pub records: Vec<GrantRecord>,
    _temp: TempFiles,
}

// Hand-written, value-free Debug — a derived one would print the secret values
// held in `env`/`redactions`. Only counts and grant names are shown.
impl std::fmt::Debug for Brokered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Brokered")
            .field("grants", &self.records.iter().map(|r| &r.name).collect::<Vec<_>>())
            .field("env_vars", &self.env.iter().map(|(k, _)| k).collect::<Vec<_>>())
            .field("redaction_count", &self.redactions.len())
            .finish()
    }
}

/// Audit record for one delivered grant — everything but the value.
pub struct GrantRecord {
    pub name: String,
    pub source: String,
    pub inject: String,
    pub ttl: Option<String>,
    /// `sha256:<12 hex>` of the value, so reviewers can confirm "same token
    /// across runs" without ever seeing it.
    pub fingerprint: String,
}

impl GrantRecord {
    /// The `secret` event detail line (secret-free).
    pub fn detail(&self) -> String {
        let ttl = self.ttl.as_deref().map(|t| format!(" ttl={t}")).unwrap_or_default();
        format!(
            "grant={} source={} inject={}{} fp={}",
            self.name, self.source, self.inject, ttl, self.fingerprint
        )
    }
}

/// Unlinks file-injected secrets when dropped — including on error/panic, so a
/// materialized secret never outlives the run.
struct TempFiles(Vec<PathBuf>);
impl Drop for TempFiles {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

fn fingerprint(value: &str) -> String {
    let mut h = Sha256::new();
    h.update(value.as_bytes());
    format!("sha256:{:x}", h.finalize())[..19].to_string() // "sha256:" + 12 hex
}

/// Resolve a grant's value from its host-side source. Pure w.r.t. the filesystem
/// and process env (both injectable in tests). Fail-closed on missing/empty.
pub fn resolve_value(grant: &SecretGrant) -> Result<String, H5iError> {
    let source = grant.source_or_default();
    let value = if let Some(var) = source.strip_prefix("env:") {
        std::env::var(var).map_err(|_| {
            H5iError::Metadata(format!(
                "secret grant '{}': host env var '{var}' is not set (fail-closed)",
                grant.name
            ))
        })?
    } else if let Some(path) = source.strip_prefix("file:") {
        std::fs::read_to_string(path)
            .map_err(|e| {
                H5iError::Metadata(format!(
                    "secret grant '{}': cannot read source file '{path}': {e} (fail-closed)",
                    grant.name
                ))
            })?
            .trim_end_matches(['\n', '\r'])
            .to_string()
    } else {
        return Err(H5iError::Metadata(format!(
            "secret grant '{}': unsupported source '{source}' (use env: or file:)",
            grant.name
        )));
    };
    if value.is_empty() {
        return Err(H5iError::Metadata(format!(
            "secret grant '{}': source '{source}' resolved to an empty value (fail-closed)",
            grant.name
        )));
    }
    Ok(value)
}

/// Resolve + materialize all `grants`. `secret_dir` is where `inject=file`
/// secrets are written (`0600`, created `0700`); `is_workspace` gates file
/// injection (see [`SecretGrant::inject_or_default`]). Returns a guard that
/// unlinks the files when dropped. Fail-closed throughout.
pub fn broker(
    grants: &[SecretGrant],
    secret_dir: &Path,
    is_workspace: bool,
) -> Result<Brokered, H5iError> {
    let mut env = Vec::new();
    let mut redactions = Vec::new();
    let mut records = Vec::new();
    let mut temp = Vec::new();

    for g in grants {
        let value = resolve_value(g)?;
        let inject = g.inject_or_default();
        match inject {
            "env" => {
                env.push((g.name.clone(), value.clone()));
            }
            "file" => {
                if !is_workspace {
                    return Err(H5iError::Metadata(format!(
                        "secret grant '{}': inject=file is supported only on the workspace \
                         tier in this build (the file needs a Landlock grant on process / a \
                         bind-mount on container) — use inject=env (fail-closed)",
                        g.name
                    )));
                }
                let path = write_secret_file(secret_dir, &g.name, &value)?;
                env.push((format!("{}_FILE", g.name), path.display().to_string()));
                temp.push(path);
            }
            other => {
                return Err(H5iError::Metadata(format!(
                    "secret grant '{}': unknown inject '{other}'",
                    g.name
                )))
            }
        }
        records.push(GrantRecord {
            name: g.name.clone(),
            source: g.source_or_default(),
            inject: inject.to_string(),
            ttl: g.ttl.clone(),
            fingerprint: fingerprint(&value),
        });
        redactions.push(value);
    }

    Ok(Brokered { env, redactions, records, _temp: TempFiles(temp) })
}

/// Write a secret to `secret_dir/<name>` with mode `0600` (dir `0700`).
fn write_secret_file(secret_dir: &Path, name: &str, value: &str) -> Result<PathBuf, H5iError> {
    std::fs::create_dir_all(secret_dir).map_err(|e| H5iError::with_path(e, secret_dir))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(secret_dir, std::fs::Permissions::from_mode(0o700));
    }
    let path = secret_dir.join(name);
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| H5iError::with_path(e, &path))?;
        f.write_all(value.as_bytes()).map_err(|e| H5iError::with_path(e, &path))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, value).map_err(|e| H5iError::with_path(e, &path))?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(name: &str, source: Option<&str>, inject: Option<&str>) -> SecretGrant {
        SecretGrant {
            name: name.into(),
            source: source.map(String::from),
            inject: inject.map(String::from),
            ttl: None,
        }
    }

    #[test]
    fn resolves_env_source() {
        // SAFETY: single-threaded test; unique var name avoids cross-test races.
        std::env::set_var("H5I_TEST_TOKEN_A", "s3cr3t-A");
        let g = grant("TOK", Some("env:H5I_TEST_TOKEN_A"), Some("env"));
        assert_eq!(resolve_value(&g).unwrap(), "s3cr3t-A");
        std::env::remove_var("H5I_TEST_TOKEN_A");
    }

    #[test]
    fn default_source_is_namespaced_env_var() {
        std::env::set_var("H5I_SECRET_GITHUB_TOKEN", "ghp_xyz");
        let g = grant("GITHUB_TOKEN", None, None);
        assert_eq!(g.source_or_default(), "env:H5I_SECRET_GITHUB_TOKEN");
        assert_eq!(resolve_value(&g).unwrap(), "ghp_xyz");
        std::env::remove_var("H5I_SECRET_GITHUB_TOKEN");
    }

    #[test]
    fn missing_source_fails_closed() {
        let g = grant("NOPE", Some("env:H5I_DEFINITELY_UNSET_VAR_XYZ"), Some("env"));
        assert!(resolve_value(&g).is_err());
    }

    #[test]
    fn empty_value_fails_closed() {
        std::env::set_var("H5I_TEST_EMPTY", "");
        let g = grant("E", Some("env:H5I_TEST_EMPTY"), Some("env"));
        assert!(resolve_value(&g).is_err());
        std::env::remove_var("H5I_TEST_EMPTY");
    }

    #[test]
    fn file_source_trims_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("tok");
        std::fs::write(&p, "value-from-file\n").unwrap();
        let g = grant("T", Some(&format!("file:{}", p.display())), Some("env"));
        assert_eq!(resolve_value(&g).unwrap(), "value-from-file");
    }

    #[test]
    fn env_inject_brokers_value_and_records_no_value() {
        std::env::set_var("H5I_TEST_TOKEN_B", "tok-B");
        let g = grant("API_KEY", Some("env:H5I_TEST_TOKEN_B"), Some("env"));
        let dir = tempfile::tempdir().unwrap();
        let b = broker(&[g], &dir.path().join("secrets"), false).unwrap();
        assert_eq!(b.env, vec![("API_KEY".to_string(), "tok-B".to_string())]);
        assert_eq!(b.redactions, vec!["tok-B".to_string()]);
        assert_eq!(b.records.len(), 1);
        let detail = b.records[0].detail();
        assert!(detail.contains("grant=API_KEY"));
        assert!(detail.contains("inject=env"));
        assert!(detail.starts_with("grant=API_KEY"));
        assert!(!detail.contains("tok-B"), "value must never appear in the record");
        std::env::remove_var("H5I_TEST_TOKEN_B");
    }

    #[test]
    fn file_inject_writes_0600_and_points_env_at_it() {
        std::env::set_var("H5I_TEST_TOKEN_C", "file-tok-C");
        let g = grant("CERT", Some("env:H5I_TEST_TOKEN_C"), Some("file"));
        let dir = tempfile::tempdir().unwrap();
        let sdir = dir.path().join("secrets");
        let b = broker(&[g], &sdir, true).unwrap();
        // Injected as NAME_FILE → path.
        assert_eq!(b.env.len(), 1);
        assert_eq!(b.env[0].0, "CERT_FILE");
        let path = std::path::Path::new(&b.env[0].1);
        assert!(path.exists());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "file-tok-C");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Drop unlinks it.
        let p2 = path.to_path_buf();
        drop(b);
        assert!(!p2.exists(), "file-injected secret must be unlinked on drop");
        std::env::remove_var("H5I_TEST_TOKEN_C");
    }

    #[test]
    fn file_inject_refused_off_workspace_tier() {
        std::env::set_var("H5I_TEST_TOKEN_D", "x");
        let g = grant("T", Some("env:H5I_TEST_TOKEN_D"), Some("file"));
        let dir = tempfile::tempdir().unwrap();
        let err = broker(&[g], &dir.path().join("secrets"), false).unwrap_err();
        assert!(format!("{err}").contains("inject=env"));
        std::env::remove_var("H5I_TEST_TOKEN_D");
    }

    #[test]
    fn fingerprint_is_stable_and_value_free() {
        let fp = fingerprint("hello");
        assert!(fp.starts_with("sha256:"));
        assert_eq!(fp.len(), "sha256:".len() + 12);
        assert_eq!(fp, fingerprint("hello"));
        assert_ne!(fp, fingerprint("world"));
    }
}
