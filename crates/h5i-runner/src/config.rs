//! Where a paired runner is remembered.
//!
//! Host-scoped, never in the repo (design-runner.md R6). `.h5i/env.toml` is
//! checked in and describes what a box may do; which machines *this* developer
//! can reach is a fact about this machine, and it lives beside the user egress
//! allowlist, under the user config dir, for the same reason: a box must never
//! be able to read or widen it.
//!
//! One directory per runner:
//!
//! ```text
//! ~/.config/h5i/runners/<name>/
//!   runner.toml       the record below
//!   id_ed25519        the pair key, 0600. This key and no other
//!   id_ed25519.pub
//!   known_hosts       the one host key pinned at pair time
//!   capabilities.json the last PROBE, cached for error messages
//!   ctl.sock          OpenSSH's multiplexing socket, when it is running
//! ```

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::proto::Capabilities;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("no user config directory — set $XDG_CONFIG_HOME or $HOME")]
    NoConfigDir,

    #[error(
        "`{0}` is not a usable runner name — use letters, digits, `-` and `_`, \
         starting with a letter or digit"
    )]
    BadName(String),

    #[error("no runner named `{0}` — `h5i runner list` shows the ones you have paired")]
    Unknown(String),

    #[error("a runner named `{0}` is already paired — unpair it first, or pick another name")]
    Exists(String),

    #[error("runner config at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("runner record at {path} does not parse: {source}")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("could not write a runner record: {0}")]
    TomlWrite(#[from] toml::ser::Error),
}

impl ConfigError {
    fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// What pairing recorded about one runner.
///
/// Everything here is static or slow-moving. Anything that drifts between one
/// minute and the next belongs to a `PROBE` and lives in `capabilities.json`,
/// which is a cache and is labelled as one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerRecord {
    /// The display name. A label for humans and command lines; never identity.
    pub name: String,
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,

    /// The identity: SHA-256 of the host key pinned below
    /// ([`crate::identity`]). This is what a manifest records and what a
    /// receipt names.
    pub runner_id: String,
    /// The same hash in OpenSSH's spelling, stored so `runner list` can show a
    /// user something they can compare against the machine itself.
    pub fingerprint: String,

    /// Absolute path to `h5i` on the runner, as the forced command names it.
    pub worker_path: String,

    pub paired_at: DateTime<Utc>,
}

impl RunnerRecord {
    /// Where this runner's directory is.
    pub fn dir(&self) -> Result<PathBuf, ConfigError> {
        runner_dir(&self.name)
    }

    /// The dedicated pair key.
    pub fn identity_path(&self) -> Result<PathBuf, ConfigError> {
        Ok(self.dir()?.join("id_ed25519"))
    }

    pub fn public_key_path(&self) -> Result<PathBuf, ConfigError> {
        Ok(self.dir()?.join("id_ed25519.pub"))
    }

    /// This runner's own pinned `known_hosts`.
    pub fn known_hosts_path(&self) -> Result<PathBuf, ConfigError> {
        Ok(self.dir()?.join("known_hosts"))
    }

    /// OpenSSH's multiplexing socket.
    ///
    /// `None` when the path would be too long for a unix socket: the limit is
    /// about a hundred bytes, a deep `$XDG_CONFIG_HOME` can exceed it, and
    /// OpenSSH's failure when it does is obscure. Losing multiplexing costs
    /// latency; guessing costs an unexplainable error on someone's machine.
    pub fn control_path(&self) -> Option<PathBuf> {
        let p = self.dir().ok()?.join("ctl.sock");
        (p.as_os_str().len() <= 92).then_some(p)
    }

    fn capabilities_path(&self) -> Result<PathBuf, ConfigError> {
        Ok(self.dir()?.join("capabilities.json"))
    }

    /// The last capability report, if one was ever stored.
    ///
    /// A *cache*, and treated as one everywhere it is read: `create` checks
    /// against it for a good error message, and the worker refusing at create
    /// time is the enforcement (design-runner.md R7).
    pub fn cached_capabilities(&self) -> Option<Capabilities> {
        let path = self.capabilities_path().ok()?;
        let text = std::fs::read_to_string(path).ok()?;
        let caps: Capabilities = serde_json::from_str(&text).ok()?;
        // Re-validated on the way *out* as well as on the way in: a file on
        // disk is not more trustworthy than the wire that filled it, and this
        // one is written by whatever the peer said.
        caps.sanitized().ok()
    }

    pub fn store_capabilities(&self, caps: &Capabilities) -> Result<(), ConfigError> {
        let path = self.capabilities_path()?;
        let json = serde_json::to_string_pretty(caps).unwrap_or_else(|_| "{}".into());
        write_private(&path, json.as_bytes())
    }
}

/// `$XDG_CONFIG_HOME/h5i` or `~/.config/h5i`.
///
/// The same derivation as the user egress allowlist, deliberately: these are
/// the two pieces of host-scoped state a box must never reach, and they should
/// move together if that location ever changes.
fn config_root() -> Result<PathBuf, ConfigError> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .ok_or(ConfigError::NoConfigDir)?;
    Ok(base.join("h5i"))
}

/// Where every paired runner lives.
pub fn runners_root() -> Result<PathBuf, ConfigError> {
    Ok(config_root()?.join("runners"))
}

/// One runner's directory, name-checked.
pub fn runner_dir(name: &str) -> Result<PathBuf, ConfigError> {
    Ok(runners_root()?.join(validated_name(name)?))
}

/// Names become directory names, so this is a path-safety check before it is a
/// style rule: no separators, no `..`, no leading dot, nothing empty.
pub fn validated_name(name: &str) -> Result<&str, ConfigError> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(name)
    } else {
        Err(ConfigError::BadName(name.to_string()))
    }
}

/// Read one runner's record.
pub fn load(name: &str) -> Result<RunnerRecord, ConfigError> {
    let path = runner_dir(name)?.join("runner.toml");
    let text = std::fs::read_to_string(&path).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            ConfigError::Unknown(name.to_string())
        } else {
            ConfigError::io(&path, source)
        }
    })?;
    toml::from_str(&text).map_err(|source| ConfigError::Toml { path, source })
}

/// Every paired runner, by name.
pub fn list() -> Result<Vec<RunnerRecord>, ConfigError> {
    let root = runners_root()?;
    let entries = match std::fs::read_dir(&root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(ConfigError::io(&root, e)),
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        // A directory we cannot read as a record is skipped rather than fatal:
        // one broken runner must not take `runner list` down with it. That is
        // the same lesson `sweep_invalid_worktree_registrations` records, where
        // one stale registration broke create for the whole repo.
        if let Ok(record) = load(&name) {
            out.push(record);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Write a record, creating the runner's directory.
///
/// Refuses to overwrite an existing runner: re-pairing a name that already
/// points at a machine would silently move every box bound to it.
pub fn save_new(record: &RunnerRecord) -> Result<PathBuf, ConfigError> {
    let dir = runner_dir(&record.name)?;
    if dir.join("runner.toml").exists() {
        return Err(ConfigError::Exists(record.name.clone()));
    }
    create_private_dir(&dir)?;
    save(record)?;
    Ok(dir)
}

/// Overwrite an existing record.
pub fn save(record: &RunnerRecord) -> Result<(), ConfigError> {
    let dir = runner_dir(&record.name)?;
    create_private_dir(&dir)?;
    let text = toml::to_string_pretty(record)?;
    write_private(&dir.join("runner.toml"), text.as_bytes())
}

/// Forget a runner: the record, the key, the pin, the cache.
pub fn remove(name: &str) -> Result<PathBuf, ConfigError> {
    let dir = runner_dir(name)?;
    if !dir.exists() {
        return Err(ConfigError::Unknown(name.to_string()));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| ConfigError::io(&dir, e))?;
    Ok(dir)
}

/// Write a file only this user can read.
///
/// The pair key lives in one of these. Written to a temporary file and renamed,
/// so a reader never sees a half-written record, and created with the narrow
/// mode from the start rather than widened-then-narrowed. A key that is
/// world-readable for a millisecond is a key that was world-readable.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<(), ConfigError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ConfigError::io(parent, e))?;
    }
    let tmp = path.with_extension("tmp");

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| ConfigError::io(&tmp, e))?;
        f.write_all(bytes).map_err(|e| ConfigError::io(&tmp, e))?;
        f.sync_all().map_err(|e| ConfigError::io(&tmp, e))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&tmp, bytes).map_err(|e| ConfigError::io(&tmp, e))?;
    }

    std::fs::rename(&tmp, path).map_err(|e| ConfigError::io(path, e))?;
    Ok(())
}

/// Create a directory that is owner-only from the moment it exists.
/// `create_dir_all` then `chmod` leaves a window at `0777 & ~umask`, and under
/// a permissive umask that window is a place to pre-create a symlink where the
/// pair key is about to be written. The same argument `write_private` already
/// makes about files, applied to the directory holding it.
/// Every ancestor h5i creates gets the same mode, because the *names* of the
/// paired runners are worth as little to leak as their keys.
fn create_private_dir(dir: &Path) -> Result<(), ConfigError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| ConfigError::io(dir, e))?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir).map_err(|e| ConfigError::io(dir, e))?;
    // `recursive` does not re-apply the mode to directories that already
    // existed, so an earlier run's wider directory is narrowed here.
    restrict_dir(dir)
}

/// Make a runner's directory owner-only.
fn restrict_dir(dir: &Path) -> Result<(), ConfigError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(dir, perms).map_err(|e| ConfigError::io(dir, e))?;
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Point the config root at a temporary directory for one test.
    ///
    /// `$XDG_CONFIG_HOME` is process-wide, so these tests must not run beside
    /// each other. They share one lock rather than one directory, which keeps
    /// each test's state its own.
    struct Sandbox {
        _dir: tempfile::TempDir,
        _guard: std::sync::MutexGuard<'static, ()>,
        prev_xdg: Option<std::ffi::OsString>,
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    impl Sandbox {
        fn new() -> Self {
            let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let dir = tempfile::tempdir().expect("tempdir");
            let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
            // SAFETY: the lock above serialises every test that touches it.
            unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
            Self {
                _dir: dir,
                _guard: guard,
                prev_xdg,
            }
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            unsafe {
                match &self.prev_xdg {
                    Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                    None => std::env::remove_var("XDG_CONFIG_HOME"),
                }
            }
        }
    }

    fn record(name: &str) -> RunnerRecord {
        RunnerRecord {
            name: name.to_string(),
            host: "pi.local".into(),
            user: Some("h5i".into()),
            port: None,
            runner_id: "a".repeat(64),
            fingerprint: "SHA256:abcdef".into(),
            worker_path: "/usr/local/bin/h5i".into(),
            paired_at: Utc::now(),
        }
    }

    #[test]
    fn a_record_survives_the_round_trip() {
        let _s = Sandbox::new();
        let r = record("pi5");
        save_new(&r).expect("save");
        let back = load("pi5").expect("load");
        assert_eq!(back.name, "pi5");
        assert_eq!(back.host, "pi.local");
        assert_eq!(back.runner_id, r.runner_id);
        assert_eq!(back.user.as_deref(), Some("h5i"));
    }

    #[test]
    fn pairing_over_an_existing_runner_is_refused() {
        // Re-pairing a name that already points at a machine would silently
        // move every box bound to it.
        let _s = Sandbox::new();
        save_new(&record("pi5")).expect("first");
        assert!(matches!(
            save_new(&record("pi5")),
            Err(ConfigError::Exists(_))
        ));
    }

    #[test]
    fn an_unknown_runner_says_so() {
        let _s = Sandbox::new();
        assert!(matches!(load("nope"), Err(ConfigError::Unknown(_))));
        assert!(matches!(remove("nope"), Err(ConfigError::Unknown(_))));
    }

    #[test]
    fn a_name_cannot_escape_the_runners_directory() {
        // These become directory names, so this is path safety before it is
        // style.
        for bad in [
            "", "..", "../evil", "a/b", "a\\b", ".hidden", "-leading", "a b", "a:b",
            &"x".repeat(65),
        ] {
            assert!(
                validated_name(bad).is_err(),
                "`{bad}` should not be a runner name"
            );
        }
        for good in ["pi5", "lab-box", "vm_2", "a", "A1"] {
            assert!(validated_name(good).is_ok(), "`{good}` should be allowed");
        }
    }

    #[test]
    fn listing_skips_a_broken_runner_instead_of_failing() {
        // One stale registration must not take the whole verb down; that is the
        // lesson `sweep_invalid_worktree_registrations` was written for.
        let _s = Sandbox::new();
        save_new(&record("good")).expect("save");
        let junk = runners_root().unwrap().join("broken");
        std::fs::create_dir_all(&junk).unwrap();
        std::fs::write(junk.join("runner.toml"), "this is not toml {{{").unwrap();

        let all = list().expect("list");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "good");
    }

    #[test]
    fn listing_an_unpaired_machine_is_empty_not_an_error() {
        let _s = Sandbox::new();
        assert!(list().expect("list").is_empty());
    }

    #[test]
    fn removing_forgets_the_key_and_the_pin_as_well_as_the_record() {
        let _s = Sandbox::new();
        let r = record("pi5");
        save_new(&r).expect("save");
        let key = r.identity_path().unwrap();
        write_private(&key, b"PRIVATE KEY").unwrap();
        assert!(key.exists());

        remove("pi5").expect("remove");
        assert!(!key.exists(), "the pair key must go with the record");
        assert!(matches!(load("pi5"), Err(ConfigError::Unknown(_))));
    }

    #[cfg(unix)]
    #[test]
    fn a_private_file_is_never_readable_by_anyone_else() {
        use std::os::unix::fs::PermissionsExt;
        let _s = Sandbox::new();
        save_new(&record("pi5")).expect("save");
        let key = runner_dir("pi5").unwrap().join("id_ed25519");
        write_private(&key, b"PRIVATE KEY").unwrap();

        let mode = std::fs::metadata(&key).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the pair key is owner-only");

        let dir_mode = std::fs::metadata(runner_dir("pi5").unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700, "and so is the directory holding it");
    }

    #[test]
    fn writing_leaves_no_temporary_file_behind() {
        let _s = Sandbox::new();
        save_new(&record("pi5")).expect("save");
        let dir = runner_dir("pi5").unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "rename must replace, not accumulate");
    }

    #[test]
    fn a_capability_cache_round_trips_and_is_revalidated_on_the_way_out() {
        let _s = Sandbox::new();
        let r = record("pi5");
        save_new(&r).expect("save");
        assert!(r.cached_capabilities().is_none(), "nothing cached yet");

        let caps = Capabilities {
            arch: "aarch64".into(),
            os: "linux".into(),
            memory_mb: 512,
            workspace_mb: 4096,
            isolation: vec!["process".into()],
            container: false,
            kvm: false,
            persistent_boxes: true,
            own_egress: true,
            notes: vec![],
        };
        r.store_capabilities(&caps).expect("store");
        let back = r.cached_capabilities().expect("cached");
        assert_eq!(back.memory_mb, 512);
        assert_eq!(back.isolation, vec!["process"]);

        // A file on disk is not more trustworthy than the wire that filled it.
        std::fs::write(
            r.capabilities_path().unwrap(),
            r#"{"arch":"x","os":"windows","memory_mb":1,"workspace_mb":1,
                "isolation":[],"container":false,"kvm":false,
                "persistent_boxes":true,"own_egress":true}"#,
        )
        .unwrap();
        assert!(
            r.cached_capabilities().is_none(),
            "a tampered cache is refused, not returned"
        );
    }

    #[test]
    fn a_control_path_that_would_be_too_long_for_a_socket_is_declined() {
        // Losing multiplexing costs latency. Guessing costs an obscure OpenSSH
        // error on someone else's machine.
        let mut r = record("pi5");
        r.name = "a".repeat(60);
        let _s = Sandbox::new();
        // Whatever the temp dir's depth, the rule is the same: either a usable
        // path, or None.
        if let Some(p) = r.control_path() {
            assert!(p.as_os_str().len() <= 92);
        }
    }
}
