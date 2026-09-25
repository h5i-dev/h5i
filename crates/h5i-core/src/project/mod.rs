//! Projects: the durable half of an engagement (docs/design/design-project.md).
//!
//! A browser session is disposable, and its findings used to go with it. A
//! project keeps what was concluded, the evidence copies it stands on, the
//! checklists it was asked to cover and the reports issued from it. Nothing
//! here is removed when a session is.

pub mod checklist;
pub mod evidence;
pub mod finding;
pub mod glossary;
pub mod note;
pub mod render;
pub mod report;

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Everything this module refuses, in words meant for the person who typed it.
#[derive(Debug)]
pub struct ProjectError(pub String);

impl std::fmt::Display for ProjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ProjectError {}

impl From<std::io::Error> for ProjectError {
    fn from(e: std::io::Error) -> Self {
        ProjectError(e.to_string())
    }
}

impl From<serde_json::Error> for ProjectError {
    fn from(e: serde_json::Error) -> Self {
        ProjectError(e.to_string())
    }
}

impl From<toml::de::Error> for ProjectError {
    fn from(e: toml::de::Error) -> Self {
        ProjectError(e.to_string())
    }
}

impl From<crate::error::H5iError> for ProjectError {
    fn from(e: crate::error::H5iError) -> Self {
        ProjectError(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, ProjectError>;

macro_rules! bail {
    ($($arg:tt)*) => { return Err($crate::project::ProjectError(format!($($arg)*))) };
}
pub(crate) use bail;

/// The longest one free-text field may be. Report prose lives in the draft
/// file, not here, so this bounds a finding's fields rather than a document.
pub const MAX_TEXT_BYTES: usize = 32 * 1024;

/// How much of one log a read folds.
pub const MAX_LOG_BYTES: u64 = 32 * 1024 * 1024;

/// `$H5I_PROJECT_HOME`, else `$XDG_DATA_HOME/h5i/projects`, else
/// `~/.local/share/h5i/projects`. Data rather than config: losing it loses work.
pub fn root() -> Result<PathBuf> {
    if let Some(explicit) = std::env::var_os("H5I_PROJECT_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(explicit));
    }
    if let Some(data) = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from)
        && data.is_absolute()
    {
        return Ok(data.join("h5i").join("projects"));
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| ProjectError("no $HOME, so there is nowhere to keep projects: set $H5I_PROJECT_HOME".into()))?;
    Ok(PathBuf::from(home).join(".local").join("share").join("h5i").join("projects"))
}

/// A project name that is safe to join onto a path: the scope file's rule, so
/// one `--project` names both.
pub fn validated(name: &str) -> Result<&str> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !ok {
        bail!(
            "`{name}` is not a usable project name. Use ASCII letters, digits, `-`, `_` or `.`, \
             up to 64 characters, not starting with a dot."
        );
    }
    Ok(name)
}

/// What `project.json` says.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub name: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// What is being assessed, in whatever form is useful: origins, a product.
    #[serde(default)]
    pub targets: Vec<String>,
    pub created: String,
}

/// One project on disk.
#[derive(Debug, Clone)]
pub struct Project {
    pub meta: Meta,
    pub dir: PathBuf,
}

impl Project {
    /// Create a project. Refuses one that exists: two `init`s agreeing on a
    /// name would otherwise be one project with two authors' descriptions.
    pub fn init(root: &Path, name: &str, title: &str, description: &str, targets: Vec<String>) -> Result<Project> {
        let name = validated(name)?;
        let dir = root.join(name);
        if dir.join("project.json").exists() {
            bail!("project `{name}` already exists at {}", dir.display());
        }
        private_dir(&dir)?;
        let meta = Meta {
            name: name.to_string(),
            title: if title.is_empty() { name.to_string() } else { bounded("title", title)? },
            description: bounded("description", description)?,
            targets,
            created: now(),
        };
        write_private(&dir.join("project.json"), serde_json::to_string_pretty(&meta)?.as_bytes())?;
        Ok(Project { meta, dir })
    }

    pub fn open(root: &Path, name: &str) -> Result<Project> {
        let name = validated(name)?;
        let dir = root.join(name);
        let path = dir.join("project.json");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                bail!("no project `{name}`: create it with `h5i project init {name}`")
            }
            Err(e) => bail!("cannot read {}: {e}", path.display()),
        };
        let meta: Meta = serde_json::from_str(&text)
            .map_err(|e| ProjectError(format!("{} is not readable: {e}", path.display())))?;
        Ok(Project { meta, dir })
    }

    /// Open, creating on first use. A session opened with `--project acme`
    /// can be promoted from without a separate `init`.
    pub fn open_or_init(root: &Path, name: &str) -> Result<Project> {
        match Project::open(root, name) {
            Ok(p) => Ok(p),
            Err(_) if !root.join(validated(name)?).join("project.json").exists() => {
                Project::init(root, name, "", "", Vec::new())
            }
            Err(e) => Err(e),
        }
    }

    pub fn save_meta(&self) -> Result<()> {
        write_private(&self.dir.join("project.json"), serde_json::to_string_pretty(&self.meta)?.as_bytes())
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.dir.join(rel)
    }
}

/// Every project under `root`, by name.
pub fn list(root: &Path) -> Vec<Project> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if validated(name).is_err() {
            continue;
        }
        if let Ok(p) = Project::open(root, name) {
            out.push(p);
        }
    }
    out.sort_by(|a, b| a.meta.name.cmp(&b.meta.name));
    out
}

/// The project store is the host's. A box is not granted it, and a write that
/// landed in the box's own `/tmp` would vanish with the box while looking kept.
pub fn refuse_in_box() -> Result<()> {
    if crate::env::in_env_box() {
        bail!(
            "projects live on the host and this is a box. Keep writing session findings here \
             (`h5i websec finding create`), then promote them from the host with \
             `h5i project finding promote --all --session <name>`."
        );
    }
    Ok(())
}

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Refuse text that is too long rather than storing it truncated.
pub fn bounded(what: &str, text: &str) -> Result<String> {
    if text.len() > MAX_TEXT_BYTES {
        bail!("the {what} is {} bytes, and {MAX_TEXT_BYTES} is the most one holds", text.len());
    }
    Ok(text.to_string())
}

/// `F-3`, `f3`, `3` all name finding 3 when `prefix` is `F`.
pub fn normalise_id(prefix: &str, id: &str) -> Result<String> {
    let trimmed = id.trim();
    let bare = trimmed
        .strip_prefix(prefix)
        .or_else(|| trimmed.strip_prefix(&prefix.to_ascii_lowercase()))
        .map(|rest| rest.strip_prefix('-').unwrap_or(rest))
        .unwrap_or(trimmed);
    match bare.parse::<u64>() {
        Ok(n) if n > 0 => Ok(format!("{prefix}-{n}")),
        _ => bail!("`{id}` is not a {prefix} id: try `{prefix}-3` or `3`"),
    }
}

/// The next id after the highest one in `ids`.
pub fn next_id<'a>(prefix: &str, ids: impl Iterator<Item = &'a str>) -> String {
    let highest = ids
        .filter_map(|id| id.strip_prefix(prefix)?.strip_prefix('-')?.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    format!("{prefix}-{}", highest + 1)
}

/// `create_dir_all`, owner-only.
pub fn private_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir)?;
    Ok(())
}

fn open_private(path: &Path, append: bool) -> Result<File> {
    if let Some(parent) = path.parent() {
        private_dir(parent)?;
    }
    let mut options = OpenOptions::new();
    options.create(true);
    if append {
        options.append(true);
    } else {
        options.write(true).truncate(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

/// Replace a file through a sibling, so a reader never sees half of one.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        let mut file = open_private(&tmp, false)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Append one JSON line. One `write_all`, so two writers interleave whole
/// entries rather than bytes.
pub fn append_line<T: Serialize>(path: &Path, entry: &T) -> Result<()> {
    let mut line = serde_json::to_string(entry)?;
    line.push('\n');
    open_private(path, true)?.write_all(line.as_bytes())?;
    Ok(())
}

/// Every entry of a JSON-lines log, oldest first. A torn line is skipped: half
/// a claim is not a smaller claim.
pub fn read_lines<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    if text.len() as u64 > MAX_LOG_BYTES {
        bail!("{} is larger than {MAX_LOG_BYTES} bytes, which is more than one read folds", path.display());
    }
    Ok(text.lines().filter_map(|line| serde_json::from_str(line).ok()).collect())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

/// A short summary of a project, for lists.
#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub name: String,
    pub title: String,
    pub created: String,
    pub findings: usize,
    pub open_findings: usize,
    pub by_severity: Vec<(String, usize)>,
    pub notes: usize,
    pub evidence: usize,
    pub checklists: usize,
    pub reports: usize,
    pub updated: Option<String>,
}

pub fn summary(project: &Project) -> Summary {
    let findings = finding::read(project).unwrap_or_default();
    let notes = note::read(project).unwrap_or_default();
    let evidence = evidence::list(project);
    let mut updated: Option<String> = None;
    for at in findings.iter().map(|f| &f.updated).chain(notes.iter().map(|n| &n.updated)) {
        if updated.as_deref().is_none_or(|u| u < at.as_str()) {
            updated = Some(at.clone());
        }
    }
    Summary {
        name: project.meta.name.clone(),
        title: project.meta.title.clone(),
        created: project.meta.created.clone(),
        open_findings: findings.iter().filter(|f| f.is_open()).count(),
        by_severity: finding::severity_counts(&findings),
        findings: findings.len(),
        notes: notes.iter().filter(|n| !n.archived).count(),
        evidence: evidence.len(),
        checklists: checklist::list(project).len(),
        reports: report::issued(project).len(),
        updated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_read_with_or_without_their_prefix() {
        assert_eq!(normalise_id("F", "F-3").unwrap(), "F-3");
        assert_eq!(normalise_id("F", "f3").unwrap(), "F-3");
        assert_eq!(normalise_id("F", "3").unwrap(), "F-3");
        for bad in ["F-", "F-0", "x", "3x", ""] {
            assert!(normalise_id("F", bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_next_id_follows_the_highest() {
        assert_eq!(next_id("E", ["E-1", "E-7", "junk"].into_iter()), "E-8");
        assert_eq!(next_id("E", std::iter::empty()), "E-1");
    }

    #[test]
    fn init_then_open_round_trips_and_a_second_init_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Project::init(tmp.path(), "acme", "ACME web", "", vec!["https://acme.test".into()]).unwrap();
        assert_eq!(Project::open(tmp.path(), "acme").unwrap().meta.title, p.meta.title);
        assert!(Project::init(tmp.path(), "acme", "", "", vec![]).is_err());
        assert!(Project::open(tmp.path(), "../etc").is_err());
        assert_eq!(list(tmp.path()).len(), 1);
    }
}
