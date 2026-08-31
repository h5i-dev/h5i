//! Host-side secrets broker (`docs/secrets-broker-design.md`).
//!
//! Resolves a profile's [`SecretGrant`]s from host-side sources at *run time*
//! (never at policy load) and materializes them for injection into the env's
//! child process. Capability-scoped, audited, redacted, and *fail-closed*:
//! a declared grant that cannot be resolved or delivered aborts the run rather
//! than running with the credential silently absent.
//!
//! The broker never writes a value to the policy, the manifest, or any git ref.
//! It records only the grant id, source, injection method, ttl, and a value
//! *fingerprint* (sha256 prefix). File-injected secrets are written `0600`
//! outside `$WORK` and unlinked when the [`Brokered`] guard drops.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::H5iError;
use crate::sandbox_policy::SecretGrant;

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

// Hand-written, value-free Debug. A derived one would print the secret values
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

/// Audit record for one delivered grant. Everything but the value.
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

/// Unlinks file-injected secrets when dropped, including on error/panic, so a
/// materialized secret never outlives the run.
struct TempFiles(Vec<PathBuf>);
impl Drop for TempFiles {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// `fp:<12 hex>` of a value under a per-repository key. Lets a reviewer confirm
/// "same token across runs" without ever seeing it. Public so `env secrets` can
/// fingerprint a dry-run resolution.
///
/// Keyed, not a bare digest. This lands in `GrantRecord::detail`, the env event
/// log, and `h5i box secrets` output, all of which are durable and reviewable
/// and may be mirrored through `refs/h5i/*`. An unsalted `sha256(value)` prefix
/// is 48 bits of an offline oracle: against a deploy password or a PIN-shaped
/// token, anyone holding the log can just enumerate candidates. HMAC under a
/// key that never leaves the repository gives the same "same token?" answer
/// with nothing to grind against.
pub fn fingerprint(key: &[u8], value: &str) -> String {
    let mac = hmac_sha256(key, value.as_bytes());
    let hex: String = mac.iter().take(6).map(|b| format!("{b:02x}")).collect();
    format!("fp:{hex}")
}

/// HMAC-SHA256 (RFC 2104). Hand-rolled against the `sha2` dependency already
/// present rather than adding a crate for sixteen lines.
fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let inner = Sha256::new().chain_update(ipad).chain_update(msg).finalize();
    Sha256::new().chain_update(opad).chain_update(inner).finalize().into()
}

/// Load (or mint) the per-repository fingerprint key at
/// `<h5i_root>/secrets-fp.key`, 32 bytes at 0600.
///
/// Per repository rather than per run, because the whole point of the
/// fingerprint is comparing one run against another. It is not a secret whose
/// loss is catastrophic, it only makes the fingerprints grindable again, but
/// it is written owner-only and never leaves the host.
pub fn fingerprint_key(h5i_root: &Path) -> Result<Vec<u8>, H5iError> {
    let path = h5i_root.join("secrets-fp.key");
    if let Ok(k) = std::fs::read(&path)
        && k.len() >= 32
    {
        return Ok(k);
    }
    let mut raw = [0u8; 32];
    getrandom::fill(&mut raw).map_err(|e| {
        H5iError::Metadata(format!(
            "no OS entropy for the secret fingerprint key ({e}) — refusing to fall back to an \
             unkeyed digest, which would be grindable offline (fail-closed)"
        ))
    })?;
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let _ = std::fs::remove_file(&path);
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| H5iError::with_path(e, &path))?;
        f.write_all(&raw).map_err(|e| H5iError::with_path(e, &path))?;
    }
    #[cfg(not(unix))]
    std::fs::write(&path, raw).map_err(|e| H5iError::with_path(e, &path))?;
    Ok(raw.to_vec())
}

/// Wall-clock timeout for a `command:` secret extractor.
const COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Max stdout captured from a `command:` extractor (1 MiB). A credential is
/// small; anything larger is a bug or an attempt to exhaust memory.
const COMMAND_OUTPUT_CAP: usize = 1024 * 1024;

/// Run `cmd` via `sh -c` with the production timeout + cap.
fn run_command_capped(cmd: &str, name: &str) -> Result<String, H5iError> {
    run_command_bounded(cmd, name, COMMAND_TIMEOUT, COMMAND_OUTPUT_CAP)
}

/// Run `cmd` via `sh -c` with a wall timeout and a stdout cap, returning the
/// trimmed stdout. Fail-closed: a non-zero exit, a timeout, or output past the
/// cap is an error, never a silently truncated/partial credential. The child
/// gets its own process group so a timeout reaps the whole tree.
fn run_command_bounded(
    cmd: &str,
    name: &str,
    timeout: std::time::Duration,
    cap: usize,
) -> Result<String, H5iError> {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = {
        let mut c = std::process::Command::new("sh");
        c.arg("-c")
            .arg(cmd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(unix)]
        unsafe {
            use std::os::unix::process::CommandExt;
            c.pre_exec(|| {
                // Own session so a timeout killpg reaps grandchildren too.
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        c.spawn().map_err(|e| {
            H5iError::Metadata(format!(
                "secret grant '{name}': command extractor failed to spawn: {e} (fail-closed)"
            ))
        })?
    };

    // Drain stdout on a thread so a child that fills the pipe can't deadlock
    // while we poll for exit/timeout. Keep reading to EOF (so the child never
    // blocks on a full pipe) but RETAIN only up to the cap, discarding the rest.
    // Bounded memory regardless of how much the child emits. Returns the
    // retained bytes plus whether the cap was exceeded.
    let mut stdout = child.stdout.take().expect("piped stdout");
    let reader = std::thread::spawn(move || {
        let mut buf: Vec<u8> = Vec::new();
        let mut over = false;
        let mut chunk = [0u8; 8192];
        loop {
            match stdout.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if buf.len() <= cap {
                        buf.extend_from_slice(&chunk[..n]);
                        if buf.len() > cap {
                            over = true;
                        }
                    }
                    // else: keep draining into the void so the child can finish.
                }
                Err(_) => break,
            }
        }
        (buf, over)
    });

    // Poll for exit until the deadline; on timeout, kill the whole group.
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {
                if started.elapsed() >= timeout {
                    #[cfg(unix)]
                    unsafe {
                        libc::kill(-(child.id() as i32), libc::SIGKILL);
                    }
                    #[cfg(not(unix))]
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = reader.join();
                    return Err(H5iError::Metadata(format!(
                        "secret grant '{name}': command extractor exceeded {}s (fail-closed)",
                        timeout.as_secs()
                    )));
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(e) => {
                let _ = reader.join();
                return Err(H5iError::Metadata(format!(
                    "secret grant '{name}': command extractor wait failed: {e} (fail-closed)"
                )));
            }
        }
    };
    let (buf, over) = reader.join().unwrap_or_default();
    if over {
        return Err(H5iError::Metadata(format!(
            "secret grant '{name}': command extractor produced more than {cap} bytes (fail-closed)"
        )));
    }
    if !status.success() {
        return Err(H5iError::Metadata(format!(
            "secret grant '{name}': command extractor exited {} (fail-closed)",
            status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into())
        )));
    }
    Ok(String::from_utf8_lossy(&buf)
        .trim_end_matches(['\n', '\r'])
        .to_string())
}

/// Read a `file:` secret source, bounded. See [`resolve_value`].
fn read_file_capped(path: &str, name: &str, cap: usize) -> Result<String, H5iError> {
    use std::io::Read;
    let f = std::fs::File::open(path).map_err(|e| {
        H5iError::Metadata(format!(
            "secret grant '{name}': cannot read source file '{path}': {e} (fail-closed)"
        ))
    })?;
    let mut buf = Vec::new();
    // `cap + 1`, so "exactly at the cap" is readable and one byte past it is
    // detectable rather than silently trimmed.
    f.take(cap as u64 + 1).read_to_end(&mut buf).map_err(|e| {
        H5iError::Metadata(format!(
            "secret grant '{name}': cannot read source file '{path}': {e} (fail-closed)"
        ))
    })?;
    if buf.len() > cap {
        return Err(H5iError::Metadata(format!(
            "secret grant '{name}': source file '{path}' is larger than {cap} bytes, which is \
             not a credential (fail-closed)"
        )));
    }
    String::from_utf8(buf).map_err(|_| {
        H5iError::Metadata(format!(
            "secret grant '{name}': source file '{path}' is not text (fail-closed)"
        ))
    })
}

/// Resolve a grant's value from its host-side source. Pure w.r.t. the filesystem
/// and process env (both injectable in tests). Fail-closed on missing/empty.
///
/// `allow_command` gates the `command:` extractor, which executes arbitrary code
/// on the host, outside the sandbox (Codex). It is off unless the env's
/// pinned, tamper-evident policy opts in (`allow_command_extractors = true`), so
/// a credential source can never be turned into a host-code-exec channel without
/// an explicit, digested grant.
pub fn resolve_value(grant: &SecretGrant, allow_command: bool) -> Result<String, H5iError> {
    let source = grant.source_or_default();
    let value = if let Some(var) = source.strip_prefix("env:") {
        std::env::var(var).map_err(|_| {
            H5iError::Metadata(format!(
                "secret grant '{}': host env var '{var}' is not set (fail-closed)",
                grant.name
            ))
        })?
    } else if let Some(path) = source.strip_prefix("file:") {
        // Capped, like the `command:` extractor beside it and for the same
        // reason: the source is repo-supplied policy, a credential is small, and
        // `read_to_string` on `file:/dev/zero` is an unbounded allocation on the
        // host at box-create time. Fail-closed past the cap rather than
        // truncating. Half a credential is not a credential.
        read_file_capped(path, &grant.name, COMMAND_OUTPUT_CAP)?
            .trim_end_matches(['\n', '\r'])
            .to_string()
    } else if let Some(cmd) = source.strip_prefix("command:") {
        if !allow_command {
            return Err(H5iError::Metadata(format!(
                "secret grant '{}': a command: extractor runs host-side code outside the \
                 sandbox and is refused unless the profile sets \
                 `allow_command_extractors = true` (fail-closed)",
                grant.name
            )));
        }
        // Bounded even when opted-in: a trusted-but-buggy extractor (or a
        // credential helper waiting on a TTY prompt) must not hang env
        // create/run forever or balloon memory. Wall timeout + stdout cap,
        // fail-closed on either.
        run_command_capped(cmd, &grant.name)?
    } else {
        return Err(H5iError::Metadata(format!(
            "secret grant '{}': unsupported source '{source}' (use env:, file:, or command:)",
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
    allow_command: bool,
    fp_key: &[u8],
) -> Result<Brokered, H5iError> {
    let mut env = Vec::new();
    let mut redactions = Vec::new();
    let mut records = Vec::new();
    let mut temp = Vec::new();

    for g in grants {
        let value = resolve_value(g, allow_command)?;
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
            fingerprint: fingerprint(fp_key, &value),
        });
        redactions.push(value);
    }

    Ok(Brokered { env, redactions, records, _temp: TempFiles(temp) })
}

/// Write a secret to `secret_dir/<name>` with mode `0600` (dir `0700`).
///
/// Fail-closed against a pre-planted path. `inject=file` is only permitted on
/// the workspace tier, which applies no kernel confinement, so the box, or any
/// same-uid process, can create `<env>/secrets/<name>` before the run. Without
/// `O_NOFOLLOW|O_EXCL` the open would follow a symlink and write the plaintext
/// credential to its target, and `mode()` is ignored for a file that already
/// exists, so a pre-created 0644 file would keep those permissions. `TempFiles`
/// would then unlink only the link, leaving the secret behind.
fn write_secret_file(secret_dir: &Path, name: &str, value: &str) -> Result<PathBuf, H5iError> {
    std::fs::create_dir_all(secret_dir).map_err(|e| H5iError::with_path(e, secret_dir))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // The symlink check comes *first*. `set_permissions` follows links, so
        // running it before the check chmods whatever the link points at. A
        // write to an attacker-chosen path taken on the way to deciding we
        // would not write to it.
        if std::fs::symlink_metadata(secret_dir)
            .map_err(|e| H5iError::with_path(e, secret_dir))?
            .file_type()
            .is_symlink()
        {
            return Err(H5iError::Metadata(format!(
                "secrets: '{}' is a symlink — refusing to materialize a credential through it \
                 (fail-closed)",
                secret_dir.display()
            )));
        }
        // Propagated, not swallowed: a directory left at the umask default is
        // a readable secret store, which is the thing this function exists to
        // prevent.
        std::fs::set_permissions(secret_dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| H5iError::with_path(e, secret_dir))?;
    }
    let path = secret_dir.join(name);
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        // A leftover from an earlier run is ours to clear; `create_new` then
        // guarantees we created this file, so its mode was never wider than
        // 0600 and it is not a link to somewhere else.
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(H5iError::with_path(e, &path)),
        }
        let mut f = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
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

    /// `inject=file` runs on the workspace tier, which confines nothing, so the
    /// destination can be pre-planted. Writing through a symlink would put the
    /// plaintext credential wherever the link pointed and leave it there after
    /// teardown (TempFiles unlinks only the link).
    #[test]
    #[cfg(unix)]
    fn a_planted_symlink_cannot_capture_a_brokered_secret() {
        let td = tempfile::tempdir().unwrap();
        let secrets = td.path().join("secrets");
        let stolen = td.path().join("stolen");
        std::fs::create_dir_all(&secrets).unwrap();
        std::os::unix::fs::symlink(&stolen, secrets.join("DEPLOY_KEY")).unwrap();

        let err = write_secret_file(&secrets, "DEPLOY_KEY", "s3cret").map(|_| ());
        // Either refused, or replaced with a fresh 0600 regular file. Never
        // written through to the link's target.
        assert!(
            !stolen.exists(),
            "credential was written through the planted symlink"
        );
        if err.is_ok() {
            let md = std::fs::symlink_metadata(secrets.join("DEPLOY_KEY")).unwrap();
            assert!(md.file_type().is_file());
        }
    }

    /// A pre-created world-readable file must not keep its mode: `mode()` on
    /// `OpenOptions` is ignored when the file already exists.
    #[test]
    #[cfg(unix)]
    fn a_pre_created_loose_mode_file_is_replaced_not_reused() {
        use std::os::unix::fs::PermissionsExt;
        let td = tempfile::tempdir().unwrap();
        let secrets = td.path().join("secrets");
        std::fs::create_dir_all(&secrets).unwrap();
        let victim = secrets.join("TOKEN");
        std::fs::write(&victim, "").unwrap();
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o644)).unwrap();

        let p = write_secret_file(&secrets, "TOKEN", "s3cret").unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "secret file must be 0600, got {mode:o}");
        let dmode = std::fs::metadata(&secrets).unwrap().permissions().mode() & 0o777;
        assert_eq!(dmode, 0o700, "secret dir must be 0700, got {dmode:o}");
    }

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
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::set_var("H5I_TEST_TOKEN_A", "s3cr3t-A");
        }
        let g = grant("TOK", Some("env:H5I_TEST_TOKEN_A"), Some("env"));
        assert_eq!(resolve_value(&g, false).unwrap(), "s3cr3t-A");
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::remove_var("H5I_TEST_TOKEN_A");
        }
    }

    #[test]
    fn default_source_is_namespaced_env_var() {
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::set_var("H5I_SECRET_GITHUB_TOKEN", "ghp_xyz");
        }
        let g = grant("GITHUB_TOKEN", None, None);
        assert_eq!(g.source_or_default(), "env:H5I_SECRET_GITHUB_TOKEN");
        assert_eq!(resolve_value(&g, false).unwrap(), "ghp_xyz");
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::remove_var("H5I_SECRET_GITHUB_TOKEN");
        }
    }

    #[test]
    fn missing_source_fails_closed() {
        let g = grant("NOPE", Some("env:H5I_DEFINITELY_UNSET_VAR_XYZ"), Some("env"));
        assert!(resolve_value(&g, false).is_err());
    }

    #[test]
    fn empty_value_fails_closed() {
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::set_var("H5I_TEST_EMPTY", "");
        }
        let g = grant("E", Some("env:H5I_TEST_EMPTY"), Some("env"));
        assert!(resolve_value(&g, false).is_err());
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::remove_var("H5I_TEST_EMPTY");
        }
    }

    #[test]
    fn file_source_trims_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("tok");
        std::fs::write(&p, "value-from-file\n").unwrap();
        let g = grant("T", Some(&format!("file:{}", p.display())), Some("env"));
        assert_eq!(resolve_value(&g, false).unwrap(), "value-from-file");
    }

    #[test]
    fn env_inject_brokers_value_and_records_no_value() {
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::set_var("H5I_TEST_TOKEN_B", "tok-B");
        }
        let g = grant("API_KEY", Some("env:H5I_TEST_TOKEN_B"), Some("env"));
        let dir = tempfile::tempdir().unwrap();
        let b = broker(&[g], &dir.path().join("secrets"), false, false, b"test-key").unwrap();
        assert_eq!(b.env, vec![("API_KEY".to_string(), "tok-B".to_string())]);
        assert_eq!(b.redactions, vec!["tok-B".to_string()]);
        assert_eq!(b.records.len(), 1);
        let detail = b.records[0].detail();
        assert!(detail.contains("grant=API_KEY"));
        assert!(detail.contains("inject=env"));
        assert!(detail.starts_with("grant=API_KEY"));
        assert!(!detail.contains("tok-B"), "value must never appear in the record");
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::remove_var("H5I_TEST_TOKEN_B");
        }
    }

    #[test]
    fn file_inject_writes_0600_and_points_env_at_it() {
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::set_var("H5I_TEST_TOKEN_C", "file-tok-C");
        }
        let g = grant("CERT", Some("env:H5I_TEST_TOKEN_C"), Some("file"));
        let dir = tempfile::tempdir().unwrap();
        let sdir = dir.path().join("secrets");
        let b = broker(&[g], &sdir, true, false, b"test-key").unwrap();
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
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::remove_var("H5I_TEST_TOKEN_C");
        }
    }

    #[test]
    fn file_inject_refused_off_workspace_tier() {
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::set_var("H5I_TEST_TOKEN_D", "x");
        }
        let g = grant("T", Some("env:H5I_TEST_TOKEN_D"), Some("file"));
        let dir = tempfile::tempdir().unwrap();
        let err = broker(&[g], &dir.path().join("secrets"), false, false, b"test-key").unwrap_err();
        assert!(format!("{err}").contains("inject=env"));
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::remove_var("H5I_TEST_TOKEN_D");
        }
    }

    #[test]
    fn multiple_grants_all_brokered_independently() {
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::set_var("H5I_TEST_M1", "val-one");
            std::env::set_var("H5I_TEST_M2", "val-two");
        }
        let grants = vec![
            grant("TOK_A", Some("env:H5I_TEST_M1"), Some("env")),
            grant("TOK_B", Some("env:H5I_TEST_M2"), Some("env")),
        ];
        let dir = tempfile::tempdir().unwrap();
        let b = broker(&grants, &dir.path().join("secrets"), false, false, b"test-key").unwrap();
        assert_eq!(b.env.len(), 2);
        assert!(b.env.contains(&("TOK_A".into(), "val-one".into())));
        assert!(b.env.contains(&("TOK_B".into(), "val-two".into())));
        // Both values are scrubbed; both grants are audited; no value in records.
        assert_eq!(b.redactions.len(), 2);
        assert_eq!(b.records.len(), 2);
        assert!(b.records.iter().all(|r| !r.detail().contains("val-")));
        // Distinct fingerprints for distinct values.
        assert_ne!(b.records[0].fingerprint, b.records[1].fingerprint);
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::remove_var("H5I_TEST_M1");
            std::env::remove_var("H5I_TEST_M2");
        }
    }

    #[test]
    fn one_missing_grant_fails_the_whole_broker() {
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::set_var("H5I_TEST_PRESENT", "here");
        }
        // First grant resolves; second is absent → the whole call fails closed
        // (an env must not run with a partial credential set).
        let grants = vec![
            grant("OK", Some("env:H5I_TEST_PRESENT"), Some("env")),
            grant("MISSING", Some("env:H5I_TEST_ABSENT_ZZZ"), Some("env")),
        ];
        let dir = tempfile::tempdir().unwrap();
        assert!(broker(&grants, &dir.path().join("secrets"), false, false, b"test-key").is_err());
        // Safety: single-threaded test; no other thread reads the environment.
        unsafe {
            std::env::remove_var("H5I_TEST_PRESENT");
        }
    }

    #[test]
    fn fingerprint_is_stable_and_value_free() {
        let k = b"per-repo-key";
        let fp = fingerprint(k, "hello");
        assert!(fp.starts_with("fp:"));
        assert_eq!(fp.len(), "fp:".len() + 12);
        assert_eq!(fp, fingerprint(k, "hello"), "stable across runs");
        assert_ne!(fp, fingerprint(k, "world"));
        assert!(!fp.contains("hello"));
    }

    /// The fingerprint reaches durable, reviewable records, so it must not be a
    /// bare digest anyone holding the log can grind offline against a
    /// low-entropy credential. Under a different key the same value must
    /// fingerprint differently.
    #[test]
    fn fingerprints_are_keyed_not_a_plain_digest() {
        let a = fingerprint(b"key-a", "deploy-password");
        let b = fingerprint(b"key-b", "deploy-password");
        assert_ne!(a, b, "the fingerprint must depend on the repository key");

        // And it is not sha256(value) truncated, which is what was grindable.
        let mut h = Sha256::new();
        h.update(b"deploy-password");
        let plain = format!("{:x}", h.finalize());
        assert!(!a.contains(&plain[..12]), "still a bare digest");
    }

    /// RFC 2104 test vector, so the hand-rolled HMAC is pinned to the standard.
    #[test]
    fn hmac_matches_the_rfc_vector() {
        let mac = hmac_sha256(&[0x0b; 20], b"Hi There");
        let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn the_fingerprint_key_is_per_repo_and_owner_only() {
        let td = tempfile::tempdir().unwrap();
        let k1 = fingerprint_key(td.path()).unwrap();
        assert_eq!(k1.len(), 32);
        // Stable across calls, so fingerprints compare across runs.
        assert_eq!(k1, fingerprint_key(td.path()).unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = std::fs::metadata(td.path().join("secrets-fp.key")).unwrap();
            assert_eq!(m.permissions().mode() & 0o777, 0o600);
        }
        // A different repository gets a different key.
        let other = tempfile::tempdir().unwrap();
        assert_ne!(k1, fingerprint_key(other.path()).unwrap());
    }

    #[test]
    fn command_extractor_refused_unless_allowed() {
        let g = grant("TOK", Some("command:printf secret-from-cmd"), Some("env"));
        // Gated off by default: a command: source must not run.
        let err = resolve_value(&g, false).unwrap_err();
        assert!(format!("{err}").contains("allow_command_extractors"));
    }

    #[test]
    fn command_extractor_runs_when_allowed() {
        let g = grant("TOK", Some("command:printf secret-from-cmd"), Some("env"));
        // Opted in: the command runs host-side and its stdout is the value.
        assert_eq!(resolve_value(&g, true).unwrap(), "secret-from-cmd");
    }

    #[test]
    fn command_extractor_nonzero_exit_fails_closed() {
        let g = grant("TOK", Some("command:exit 3"), Some("env"));
        assert!(resolve_value(&g, true).is_err());
    }

    #[test]
    fn command_extractor_under_limits_ok() {
        let v = run_command_bounded(
            "printf hi",
            "T",
            std::time::Duration::from_secs(5),
            1024,
        )
        .unwrap();
        assert_eq!(v, "hi");
    }

    #[test]
    fn command_extractor_times_out_fail_closed() {
        // A hanging extractor must not block forever. Killed at the deadline.
        let err = run_command_bounded(
            "sleep 30",
            "T",
            std::time::Duration::from_millis(300),
            1024,
        )
        .unwrap_err();
        assert!(format!("{err}").contains("exceeded"), "{err}");
    }

    #[test]
    fn command_extractor_caps_output_fail_closed() {
        // Far more than the cap: the reader drains to EOF (no deadlock) but
        // retains only the cap and reports it exceeded.
        let err = run_command_bounded(
            "yes aaaaaaaaaa | head -c 200000",
            "T",
            std::time::Duration::from_secs(10),
            1024,
        )
        .unwrap_err();
        assert!(format!("{err}").contains("more than"), "{err}");
    }
}
