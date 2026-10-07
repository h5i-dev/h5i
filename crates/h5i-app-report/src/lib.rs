//! Shared, versioned data. Authored explanations cannot assert a proof result.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

pub const MODEL: &str = "h5i-app.ui.json";
pub const RUNS: &str = ".h5i/app/runs";
pub const MAX_FILE: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Model {
    pub version: u32,
    pub title: String,
    pub description: String,
    pub author: String,
    #[serde(default)]
    pub exclusions: Vec<String>,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Node {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(default)]
    pub sources: Vec<Source>,
    #[serde(default)]
    pub excludes: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// Repository-relative, never an arbitrary host path.
    pub path: String,
    #[serde(default)]
    pub anchor: String,
    /// Optional hash of the entire file when the explanation was reviewed.
    #[serde(default)]
    pub digest: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub kind: String,
    #[serde(default = "proof_view")]
    pub view: String,
}
fn proof_view() -> String {
    "proof".into()
}

impl Model {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err("Unsupported explanation version".into());
        }
        if self.nodes.len() > 200 || self.edges.len() > 600 {
            return Err("Explanation exceeds 200 nodes / 600 edges".into());
        }
        let mut ids = BTreeSet::new();
        for n in &self.nodes {
            if n.id.is_empty() || !ids.insert(&n.id) {
                return Err(format!("Duplicate or empty node ID: {}", n.id));
            }
            if ![
                "guarantee",
                "theorem",
                "specification",
                "assumption",
                "implementation",
                "counterexample",
                "boundary",
            ]
            .contains(&n.kind.as_str())
            {
                return Err(format!("Unknown node kind: {}", n.kind));
            }
            if n.sources.len() > 12 || n.sources.iter().any(|s| !safe_relative(&s.path)) {
                return Err(format!("Invalid sources for {}", n.id));
            }
        }
        for e in &self.edges {
            if !ids.contains(&e.from)
                || !ids.contains(&e.to)
                || e.kind.is_empty()
                || !["proof", "flow"].contains(&e.view.as_str())
            {
                return Err(format!("Invalid edge: {} → {}", e.from, e.to));
            }
        }
        Ok(())
    }
}

pub fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

/// Refuse escapes, including symlinks inside the repository.
pub fn contained(root: &Path, relative: &str) -> io::Result<PathBuf> {
    if !safe_relative(relative) {
        return Err(io::Error::other("invalid relative path"));
    }
    let root = root.canonicalize()?;
    let path = root.join(relative).canonicalize()?;
    if !path.starts_with(&root) {
        return Err(io::Error::other("path leaves repository"));
    }
    Ok(path)
}

pub fn read_text(path: &Path) -> io::Result<String> {
    let f = std::fs::File::open(path)?;
    if !f.metadata()?.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    let mut s = String::new();
    f.take(MAX_FILE + 1).read_to_string(&mut s)?;
    if s.len() as u64 > MAX_FILE {
        return Err(io::Error::other("file exceeds 4 MiB"));
    }
    Ok(s)
}

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Bounded traversal, without following symlinks. Errors are not an empty tree.
pub fn files(root: &Path) -> io::Result<Vec<PathBuf>> {
    fn visit(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) -> io::Result<()> {
        if depth > 30 {
            return Err(io::Error::other("directory depth exceeds 30"));
        }
        for e in std::fs::read_dir(dir)? {
            let e = e?;
            let name = e.file_name();
            if matches!(
                name.to_string_lossy().as_ref(),
                ".git" | ".h5i" | ".lake" | "target" | "node_modules" | "dist" | ".venv" | "vendor"
            ) {
                continue;
            }
            let ty = e.file_type()?;
            if ty.is_symlink() {
                continue;
            }
            if ty.is_dir() {
                visit(&e.path(), depth + 1, out)?;
            } else if ty.is_file() {
                out.push(e.path());
            }
            if out.len() > 30_000 {
                return Err(io::Error::other("file inventory exceeds 30000 entries"));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    visit(root, 0, &mut out)?;
    out.sort();
    Ok(out)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Inputs {
    pub head: Option<String>,
    pub dirty: bool,
    pub digest: String,
    pub files: BTreeMap<String, String>,
    pub errors: Vec<String>,
}

/// Conservative repository-wide input identity: no build or metadata command
/// is run by the console. External path packages remain an explicit limit.
pub fn inputs(root: &Path) -> Inputs {
    let head = git2::Repository::discover(root).ok().and_then(|r| {
        r.head()
            .ok()
            .and_then(|h| h.target().map(|id| id.to_string()))
    });
    let dirty = git2::Repository::discover(root)
        .ok()
        .and_then(|r| {
            let mut opts = git2::StatusOptions::new();
            opts.include_untracked(true).recurse_untracked_dirs(true);
            r.statuses(Some(&mut opts)).ok().map(|s| !s.is_empty())
        })
        .unwrap_or(true);
    let mut result = Inputs {
        head,
        dirty,
        ..Inputs::default()
    };
    match files(root) {
        Ok(files) => {
            for p in files {
                let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("");
                let name = p.file_name().unwrap_or_default().to_string_lossy();
                if !matches!(ext, "rs" | "lean" | "toml" | "lock" | "json")
                    && name != "lean-toolchain"
                {
                    continue;
                }
                // Authored prose and presentation do not make a measured proof stale.
                if name == MODEL || p.starts_with(root.join("web")) {
                    continue;
                }
                let rel = p
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                match read_text(&p) {
                    Ok(s) => {
                        result.files.insert(rel, digest(s.as_bytes()));
                    }
                    Err(e) => result.errors.push(format!("{rel}: {e}")),
                }
            }
        }
        Err(e) => result.errors.push(e.to_string()),
    }
    result.digest = digest(&serde_json::to_vec(&result.files).unwrap());
    result
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Declaration {
    pub name: String,
    pub kind: String,
    pub signature: String,
    #[serde(default)]
    pub definition: Option<String>,
    pub dependencies: Vec<String>,
    pub axioms: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Stage {
    pub name: String,
    pub status: String,
    pub seconds: f64,
    pub log: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Trial {
    pub name: String,
    pub generated: bool,
    pub file: String,
    pub edits: Vec<Edit>,
    pub status: String,
    pub stages: Vec<Stage>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Edit {
    pub before: String,
    pub after: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    pub version: u32,
    pub id: String,
    pub kind: String,
    pub started: u64,
    pub finished: Option<u64>,
    pub status: String,
    pub inputs: Inputs,
    pub inputs_changed: bool,
    pub targets: Vec<String>,
    pub toolchain: BTreeMap<String, String>,
    pub baseline: String,
    pub trials: Vec<Trial>,
    pub stages: Vec<Stage>,
    pub declarations: Vec<Declaration>,
    pub unbuilt_modules: Vec<String>,
    pub error: Option<String>,
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// A directory created exclusively gives concurrent runs independent IDs.
/// Drop preserves interrupted runs (also on ordinary error returns/panics).
pub struct Recorder {
    pub run: Run,
    dir: PathBuf,
    root: PathBuf,
}

impl Recorder {
    pub fn start(
        project: &Path,
        root: &Path,
        kind: &str,
        targets: Vec<String>,
    ) -> io::Result<Self> {
        let parent = project.join(RUNS);
        std::fs::create_dir_all(&parent)?;
        let dir = tempfile::Builder::new()
            .prefix(&format!("{}-{}-", now(), kind))
            .tempdir_in(&parent)?
            .keep();
        let id = dir.file_name().unwrap().to_string_lossy().into_owned();
        let mut toolchain = BTreeMap::new();
        toolchain.insert("h5i-app-report".into(), env!("CARGO_PKG_VERSION").into());
        let run = Run {
            version: 1,
            id,
            kind: kind.into(),
            started: now(),
            finished: None,
            status: "running".into(),
            inputs: inputs(root),
            inputs_changed: false,
            targets,
            toolchain,
            baseline: "not_run".into(),
            trials: Vec::new(),
            stages: Vec::new(),
            declarations: Vec::new(),
            unbuilt_modules: Vec::new(),
            error: None,
        };
        let r = Self {
            run,
            dir,
            root: root.into(),
        };
        r.save()?;
        Ok(r)
    }
    pub fn save(&self) -> io::Result<()> {
        let mut tmp = tempfile::NamedTempFile::new_in(&self.dir)?;
        serde_json::to_writer(&mut tmp, &self.run)?;
        tmp.flush()?;
        tmp.persist(self.dir.join("run.json"))
            .map_err(|e| e.error)?;
        Ok(())
    }
    pub fn finish(&mut self, status: &str) -> io::Result<()> {
        let current = inputs(&self.root);
        self.run.inputs_changed =
            current.digest != self.run.inputs.digest || current.errors != self.run.inputs.errors;
        self.run.status = status.into();
        self.run.finished = Some(now());
        self.save()
    }
}
impl Drop for Recorder {
    fn drop(&mut self) {
        if self.run.finished.is_none() {
            self.run.status = "interrupted".into();
            self.run.finished = Some(now());
            let _ = self.save();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_escape_and_unknown_evidence_claims() {
        for path in ["../key", "/etc/passwd", "a/../../b", "a\\b", ""] {
            assert!(!safe_relative(path));
        }
        assert!(serde_json::from_str::<Model>(r#"{"version":1,"title":"x","description":"","author":"agent","nodes":[],"edges":[],"verified":true}"#).is_err());
    }
    #[test]
    fn records_are_independent_and_errors_leave_evidence() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("kernel.rs"), "old").unwrap();
        let mut a = Recorder::start(d.path(), d.path(), "check", vec![]).unwrap();
        let b = Recorder::start(d.path(), d.path(), "check", vec![]).unwrap();
        assert_ne!(a.run.id, b.run.id);
        let path = b.dir.join("run.json");
        drop(b);
        let b: Run = serde_json::from_str(&read_text(&path).unwrap()).unwrap();
        assert_eq!(b.status, "interrupted");
        std::fs::write(d.path().join("kernel.rs"), "new").unwrap();
        a.finish("passed").unwrap();
        assert!(a.run.inputs_changed);
    }
}
