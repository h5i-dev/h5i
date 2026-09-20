//! Engagement scope: what the target's owner authorised, as opposed to what
//! this machine permits.
//!
//! Lives under `~/.config/h5i/projects/`, which `default_fs_deny` already
//! refuses to every box: a scope an agent can edit from inside the box it
//! confines is not a scope. The engine never reads it. The host resolves it and
//! passes the rules as arguments, which is also the only thing that works for a
//! boxed session.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// One project's scope, as read off disk.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    /// Origins this engagement covers. Takes everything `--allow` takes.
    #[serde(default)]
    pub allow: Vec<String>,
    /// Origins refused even when something else granted them.
    #[serde(default)]
    pub deny: Vec<String>,
    /// Paths refused on every host. `*` matches any run of characters.
    #[serde(default)]
    pub deny_paths: Vec<String>,
}

/// A scope and where it came from.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub scope: Scope,
    pub path: PathBuf,
    pub digest: String,
}

/// `$XDG_CONFIG_HOME/h5i` or `~/.config/h5i`.
///
/// The same derivation as the runner directory and the user egress allowlist:
/// these move together if the location ever changes.
fn config_root() -> anyhow::Result<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .ok_or_else(|| {
            anyhow::anyhow!("no user config directory — set $XDG_CONFIG_HOME or $HOME")
        })?;
    Ok(base.join("h5i"))
}

/// Where every project's scope lives.
pub fn projects_dir() -> anyhow::Result<PathBuf> {
    Ok(config_root()?.join("projects"))
}

/// A project name that is safe to join onto a path. Refused rather than
/// sanitised: a name quietly rewritten resolves a scope nobody asked for.
fn validated(name: &str) -> anyhow::Result<&str> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !name.starts_with('.');
    if !ok {
        anyhow::bail!(
            "`{name}` is not a usable project name. Use ASCII letters, digits, `-`, `_` or `.`, \
             up to 64 characters, not starting with a dot."
        );
    }
    Ok(name)
}

/// The scope file for a project, whether or not it exists.
pub fn path_for(name: &str) -> anyhow::Result<PathBuf> {
    Ok(projects_dir()?.join(format!("{}.toml", validated(name)?)))
}

/// Read a project's scope, or `None` when it has no scope file. A project
/// without one is allowed: `--project` is also just a label.
pub fn resolve(name: &str) -> anyhow::Result<Option<Resolved>> {
    let path = path_for(name)?;
    if !path.exists() {
        return Ok(None);
    }
    Some(read(&path)).transpose()
}

/// Read one scope file.
pub fn read(path: &Path) -> anyhow::Result<Resolved> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("cannot read the scope at {}: {e}", path.display()))?;
    let scope: Scope = toml::from_str(&text)
        .map_err(|e| anyhow::anyhow!("{} is not a usable scope: {e}", path.display()))?;
    let digest = digest(&scope);
    Ok(Resolved {
        scope,
        path: path.to_path_buf(),
        digest,
    })
}

/// Sorted, so reordering a file without changing what it permits does not
/// change the digest.
fn digest(scope: &Scope) -> String {
    use sha2::{Digest, Sha256};
    let canon = |values: &[String]| {
        let mut v: Vec<&str> = values.iter().map(String::as_str).collect();
        v.sort_unstable();
        v.dedup();
        v.join(",")
    };
    let material = format!(
        "scope/1\nallow={}\ndeny={}\ndeny_paths={}\n",
        canon(&scope.allow),
        canon(&scope.deny),
        canon(&scope.deny_paths)
    );
    format!("sha256:{:x}", Sha256::digest(material.as_bytes()))
}

impl Resolved {
    /// The engine arguments this scope becomes. `allow` joins the origin grant
    /// because naming a project is the same act as typing `--allow`.
    pub fn engine_args(&self) -> Vec<String> {
        let mut argv = Vec::new();
        for origin in &self.scope.allow {
            argv.push("--allow".to_string());
            argv.push(origin.clone());
        }
        for origin in &self.scope.deny {
            argv.push("--deny".to_string());
            argv.push(origin.clone());
        }
        for path in &self.scope.deny_paths {
            argv.push("--deny-path".to_string());
            argv.push(path.clone());
        }
        argv
    }

    /// One line for `open`'s banner.
    pub fn summary(&self) -> String {
        format!(
            "{} allowed, {} denied, {} path rules",
            self.scope.allow.len(),
            self.scope.deny.len(),
            self.scope.deny_paths.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(toml: &str) -> Scope {
        toml::from_str(toml).expect("parses")
    }

    #[test]
    fn an_unknown_key_is_an_error_rather_than_ignored_configuration() {
        let err = toml::from_str::<Scope>("alow = [\"acme.com\"]").unwrap_err();
        assert!(err.to_string().contains("alow"), "{err}");
    }

    #[test]
    fn reordering_a_file_does_not_change_its_digest() {
        let a = scope("allow = [\"a.com\", \"b.com\"]");
        let b = scope("allow = [\"b.com\", \"a.com\"]");
        assert_eq!(digest(&a), digest(&b));
    }

    #[test]
    fn a_rule_that_changes_what_is_permitted_changes_the_digest() {
        let a = scope("allow = [\"a.com\"]");
        let b = scope("allow = [\"a.com\"]\ndeny_paths = [\"/admin/*\"]");
        assert_ne!(digest(&a), digest(&b));
    }

    #[test]
    fn deny_rules_reach_the_engine_as_deny_rules_and_not_as_grants() {
        let resolved = Resolved {
            scope: scope("allow = [\"*.acme.com\"]\ndeny = [\"blog.acme.com\"]"),
            path: PathBuf::from("/nowhere"),
            digest: String::new(),
        };
        assert_eq!(
            resolved.engine_args(),
            vec!["--allow", "*.acme.com", "--deny", "blog.acme.com"]
        );
    }

    #[test]
    fn a_project_name_that_would_leave_its_directory_is_refused() {
        for bad in ["../secrets", "a/b", "", ".hidden"] {
            assert!(validated(bad).is_err(), "{bad} should be refused");
        }
        assert!(validated("acme-bb").is_ok());
    }
}
