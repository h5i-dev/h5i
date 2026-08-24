//! Reading receipts, from whichever of the three shapes the user has.
//!
//! An h5i receipt store is an append-only JSONL log (`<env>/receipt.jsonl`);
//! `h5i box export` freezes the same records into a bundle (`receipt.json`)
//! with the box's identity around them. This tool reads either, and only the
//! fields it needs: the model below is deliberately a *subset* of h5i's, with
//! every field optional, so a receipt written by a newer h5i than the one this
//! was built against still parses.
//!
//! Tolerance is not sloppiness. The log is appended to while a box runs, so its
//! last line can be a torn write; h5i's own reader skips a malformed tail
//! rather than failing the whole read, and so does this one. A malformed line
//! anywhere *else* is reported as a warning, because that is corruption rather
//! than a race.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// One destination a box's traffic was steered at, with the proxy's tallies.
#[derive(Debug, Clone, Deserialize)]
pub struct EgressHost {
    pub host: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub allowed: u64,
    #[serde(default)]
    pub denied: u64,
}

/// The network verdicts attached to one run. Host-observed: the box never
/// supplies this, which is what makes it worth acting on.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EgressSummary {
    /// Total refusals in this run, including any the per-destination list
    /// below no longer carries. A count with no matching host is reported
    /// rather than dropped.
    #[serde(default)]
    pub denied: u64,
    #[serde(default)]
    pub hosts: Vec<EgressHost>,
    /// h5i caps the per-record host list; when it clamps, the tail is gone and
    /// a reader must not mistake what is left for the whole picture.
    #[serde(default)]
    pub hosts_truncated: bool,
}

/// One observed execution inside a box — the subset this tool reads.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ExecRecord {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub timestamp: String,
    #[serde(default)]
    pub env_id: String,
    #[serde(default)]
    pub cmd: Option<String>,
    #[serde(default)]
    pub egress: Option<EgressSummary>,
}

impl ExecRecord {
    /// The short handle h5i prints for a run (`h5i box inspect --capture <id>`
    /// resolves any unique prefix), or `?` for a record that carries no id.
    pub fn short_id(&self) -> &str {
        if self.id.is_empty() {
            "?"
        } else {
            &self.id[..self.id.len().min(SHORT_ID)]
        }
    }
}

/// Characters of a record id shown in the report. Matches the prefix length
/// `h5i box inspect` accepts, so a printed handle can be pasted straight back.
const SHORT_ID: usize = 6;

/// What the receipts are evidence *for*. Every field is best-effort: a raw
/// `receipt.jsonl` carries none of it directly, so it is recovered from the
/// box's manifest when we found the log ourselves, and from the records' own
/// `env_id` otherwise.
#[derive(Debug, Clone, Default)]
pub struct BoxIdentity {
    pub env_id: Option<String>,
    pub profile: Option<String>,
    /// `container`, `microvm`, `supervised`, `process`, `workspace` — the tier
    /// the host could actually satisfy. It decides whether `h5i box allow` can
    /// reach this box at all; see [`crate::advise::AllowReach`].
    pub isolation_claim: Option<String>,
}

/// A parsed receipt source: the records, who they belong to, and anything that
/// was wrong with the file but not wrong enough to refuse to read it.
#[derive(Debug, Clone)]
pub struct Receipts {
    pub records: Vec<ExecRecord>,
    pub identity: BoxIdentity,
    /// Where this came from, for the report's header.
    pub origin: PathBuf,
    pub warnings: Vec<String>,
}

/// The machine-readable half of an `h5i box export` bundle.
#[derive(Debug, Deserialize)]
struct Bundle {
    #[serde(default)]
    env_id: Option<String>,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    isolation_claim: Option<String>,
    #[serde(default)]
    records: Vec<ExecRecord>,
}

/// The box manifest h5i keeps beside the log, read only for the two fields
/// that change what we can suggest.
#[derive(Debug, Deserialize)]
struct Manifest {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    isolation_claim: Option<String>,
}

#[derive(Debug)]
pub enum LoadError {
    Io { path: PathBuf, err: std::io::Error },
    Empty(PathBuf),
    NotReceipts(PathBuf),
    NoStore,
    NoSuchBox { name: String, known: Vec<String> },
    AmbiguousBox { name: String, matches: Vec<String> },
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, err } => write!(f, "{}: {err}", path.display()),
            Self::Empty(p) => write!(f, "{}: no records in this receipt", p.display()),
            Self::NotReceipts(p) => write!(
                f,
                "{}: not an h5i receipt — expected an export bundle (receipt.json) \
                 or an append-only log (receipt.jsonl)",
                p.display()
            ),
            Self::NoStore => write!(
                f,
                "no h5i store here — run this from inside the repository whose boxes \
                 you want to read, or pass a receipt path"
            ),
            Self::NoSuchBox { name, known } if known.is_empty() => {
                write!(f, "no box named '{name}', and this store holds none")
            }
            Self::NoSuchBox { name, known } => {
                write!(
                    f,
                    "no box named '{name}' — this store holds: {}",
                    known.join(", ")
                )
            }
            Self::AmbiguousBox { name, matches } => write!(
                f,
                "'{name}' names more than one box ({}) — pass the full <agent>/<slug>",
                matches.join(", ")
            ),
        }
    }
}

impl std::error::Error for LoadError {}

/// Read a receipt from an explicit path: a bundle, a log, or the directory
/// holding either.
pub fn load_path(path: &Path) -> Result<Receipts, LoadError> {
    let file = if path.is_dir() {
        // An export bundle directory, or a box's env directory. Prefer the
        // bundle: it carries the identity the log does not.
        let bundle = path.join("receipt.json");
        let log = path.join("receipt.jsonl");
        if bundle.is_file() {
            bundle
        } else if log.is_file() {
            log
        } else {
            return Err(LoadError::NotReceipts(path.to_path_buf()));
        }
    } else {
        path.to_path_buf()
    };
    let text = std::fs::read_to_string(&file).map_err(|err| LoadError::Io {
        path: file.clone(),
        err,
    })?;
    let mut receipts = parse(&text, &file)?;
    // A log found next to a manifest can still say which box it is.
    if receipts.identity.profile.is_none()
        && let Some(dir) = file.parent()
        && let Some(m) = read_manifest(&dir.join("manifest.json"))
    {
        receipts.identity = m;
    }
    Ok(receipts)
}

/// Parse whichever of the two shapes this text is.
///
/// Decided by parsing, not by the file's name: `--out ./review` bundles get
/// renamed, and a log copied out of a store is routinely called `receipt.json`.
fn parse(text: &str, origin: &Path) -> Result<Receipts, LoadError> {
    let trimmed = text.trim_start();
    if trimmed.starts_with('{')
        && let Ok(bundle) = serde_json::from_str::<Bundle>(text)
    {
        // A single JSON object that parsed as a bundle. One line of JSONL is
        // also a single object, so require the `records` key to have been there
        // rather than defaulted — otherwise a one-run log reads as an empty
        // bundle and the tool cheerfully reports nothing.
        let has_records = serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|v| v.get("records").cloned())
            .is_some();
        if has_records {
            return Ok(Receipts {
                identity: BoxIdentity {
                    env_id: bundle.env_id,
                    profile: bundle.profile,
                    isolation_claim: bundle.isolation_claim,
                },
                records: bundle.records,
                origin: origin.to_path_buf(),
                warnings: Vec::new(),
            });
        }
    }
    parse_jsonl(text, origin)
}

/// Read the append-only log, tolerating the one torn line a concurrent append
/// can leave at the end.
fn parse_jsonl(text: &str, origin: &Path) -> Result<Receipts, LoadError> {
    let lines: Vec<&str> = text.lines().collect();
    let mut records = Vec::new();
    let mut warnings = Vec::new();
    let last = lines.len().saturating_sub(1);
    for (i, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<ExecRecord>(line) {
            Ok(r) => records.push(r),
            // A torn last line is a race with a running box. Only after a
            // record has parsed, though: a file whose *first* line is garbage
            // is not a receipt at all, and saying "no records" about it would
            // send the reader looking for runs instead of for the right file.
            Err(_) if i == last && !records.is_empty() => {}
            Err(e) => warnings.push(format!("line {}: unreadable record ({e})", i + 1)),
        }
    }
    if records.is_empty() {
        return Err(if warnings.is_empty() {
            LoadError::Empty(origin.to_path_buf())
        } else {
            LoadError::NotReceipts(origin.to_path_buf())
        });
    }
    let identity = BoxIdentity {
        env_id: records
            .iter()
            .map(|r| r.env_id.clone())
            .find(|e| !e.is_empty()),
        ..BoxIdentity::default()
    };
    Ok(Receipts {
        records,
        identity,
        origin: origin.to_path_buf(),
        warnings,
    })
}

fn read_manifest(path: &Path) -> Option<BoxIdentity> {
    let text = std::fs::read_to_string(path).ok()?;
    let m: Manifest = serde_json::from_str(&text).ok()?;
    Some(BoxIdentity {
        env_id: m.id,
        profile: m.profile,
        isolation_claim: m.isolation_claim,
    })
}

/// Read the live log of a named box, found the way h5i finds it: the store is
/// under the repository's Git common directory, one directory per box at
/// `env/<agent>/<slug>`.
///
/// `name` is a slug (`mybox`) or the full id (`claude/mybox`, `env/claude/mybox`).
pub fn load_box(name: &str, root: Option<&Path>, start: &Path) -> Result<Receipts, LoadError> {
    let store = match root {
        Some(r) => r.to_path_buf(),
        None => find_store(start).ok_or(LoadError::NoStore)?,
    };
    let env_root = store.join("env");
    let (agent, slug) = split_name(name);
    let mut matches: Vec<(String, PathBuf)> = Vec::new();
    let mut known: Vec<String> = Vec::new();
    for agent_dir in read_dir_sorted(&env_root) {
        let agent_name = file_name(&agent_dir);
        for box_dir in read_dir_sorted(&agent_dir) {
            let box_name = file_name(&box_dir);
            let id = format!("{agent_name}/{box_name}");
            known.push(id.clone());
            let hit = box_name == slug && agent.is_none_or(|a| a == agent_name);
            if hit {
                matches.push((id, box_dir));
            }
        }
    }
    match matches.len() {
        0 => Err(LoadError::NoSuchBox {
            name: name.to_string(),
            known,
        }),
        1 => {
            let (_, dir) = matches.remove(0);
            let log = dir.join("receipt.jsonl");
            if !log.is_file() {
                // The box exists and has simply never run.
                return Err(LoadError::Empty(log));
            }
            load_path(&log)
        }
        _ => Err(LoadError::AmbiguousBox {
            name: name.to_string(),
            matches: matches.into_iter().map(|(id, _)| id).collect(),
        }),
    }
}

/// `mybox` / `claude/mybox` / `env/claude/mybox` → `(agent, slug)`.
fn split_name(name: &str) -> (Option<&str>, &str) {
    let parts: Vec<&str> = name.trim_matches('/').split('/').collect();
    match parts.as_slice() {
        [slug] => (None, slug),
        [agent, slug] => (Some(agent), slug),
        // `env/<agent>/<slug>`, as the receipt's own `env_id` spells it.
        [_, agent, slug] => (Some(agent), slug),
        _ => (None, name),
    }
}

/// Walk up for a Git repository and return its `.h5i` store, following the
/// `.git` file a linked worktree has instead of a directory.
fn find_store(start: &Path) -> Option<PathBuf> {
    let mut dir = std::fs::canonicalize(start).unwrap_or_else(|_| start.to_path_buf());
    loop {
        let dot_git = dir.join(".git");
        if dot_git.is_dir() {
            let store = dot_git.join(".h5i");
            if store.is_dir() {
                return Some(store);
            }
        } else if dot_git.is_file()
            && let Some(common) = common_dir_of(&dot_git)
        {
            let store = common.join(".h5i");
            if store.is_dir() {
                return Some(store);
            }
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Resolve a linked worktree's `.git` file to the repository's common dir,
/// which is where the store lives for every worktree of the same repository.
fn common_dir_of(dot_git_file: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(dot_git_file).ok()?;
    let rest = text.trim().strip_prefix("gitdir:")?.trim();
    let git_dir = dot_git_file.parent()?.join(rest);
    let commondir = git_dir.join("commondir");
    let resolved = match std::fs::read_to_string(&commondir) {
        Ok(c) => git_dir.join(c.trim()),
        Err(_) => git_dir,
    };
    std::fs::canonicalize(&resolved).ok().or(Some(resolved))
}

fn read_dir_sorted(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    out.sort();
    out
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bundle_carries_the_box_identity() {
        let text = r#"{"env_id":"env/claude/fix","agent":"claude","profile":"review",
            "isolation_claim":"container","records":[{"id":"a3f1c2ff","timestamp":"t"}]}"#;
        let r = parse(text, Path::new("receipt.json")).unwrap();
        assert_eq!(r.records.len(), 1);
        assert_eq!(r.identity.profile.as_deref(), Some("review"));
        assert_eq!(r.identity.isolation_claim.as_deref(), Some("container"));
    }

    #[test]
    fn a_log_is_read_line_by_line_and_names_its_env() {
        let text = "{\"id\":\"a\",\"env_id\":\"env/claude/fix\"}\n{\"id\":\"b\",\"env_id\":\"env/claude/fix\"}\n";
        let r = parse(text, Path::new("receipt.jsonl")).unwrap();
        assert_eq!(r.records.len(), 2);
        assert_eq!(r.identity.env_id.as_deref(), Some("env/claude/fix"));
        assert!(r.warnings.is_empty());
    }

    /// A single-record log is one JSON object — the shape a bundle also has.
    /// Without the `records` check it parsed as an empty bundle and the run
    /// that was refused vanished from the report.
    #[test]
    fn a_one_line_log_is_not_mistaken_for_an_empty_bundle() {
        let text =
            r#"{"id":"a3f1c2","egress":{"denied":1,"hosts":[{"host":"h","port":443,"denied":1}]}}"#;
        let r = parse(text, Path::new("receipt.json")).unwrap();
        assert_eq!(r.records.len(), 1);
    }

    #[test]
    fn a_torn_tail_line_is_tolerated_but_a_torn_middle_is_reported() {
        let ok = "{\"id\":\"a\"}\n{\"id\":\"b\"}\n{\"id\":\"c\"";
        let r = parse(ok, Path::new("l.jsonl")).unwrap();
        assert_eq!(r.records.len(), 2);
        assert!(r.warnings.is_empty());

        let bad = "{\"id\":\"a\"}\n{oops\n{\"id\":\"c\"}\n";
        let r = parse(bad, Path::new("l.jsonl")).unwrap();
        assert_eq!(r.records.len(), 2);
        assert_eq!(r.warnings.len(), 1);
        assert!(r.warnings[0].contains("line 2"));
    }

    #[test]
    fn something_that_is_not_a_receipt_is_refused() {
        assert!(matches!(
            parse("just some prose\n", Path::new("x")),
            Err(LoadError::NotReceipts(_))
        ));
    }

    #[test]
    fn a_record_from_a_newer_h5i_still_parses() {
        let text = r#"{"id":"a","some_field_from_the_future":{"deep":[1,2]},"egress":{"allowed":1,"denied":2,"hosts":[{"host":"x","port":443,"denied":2,"why":"new"}]}}"#;
        let r = parse(text, Path::new("l.jsonl")).unwrap();
        assert_eq!(r.records[0].egress.as_ref().unwrap().denied, 2);
    }

    #[test]
    fn box_names_split_into_agent_and_slug() {
        assert_eq!(split_name("mybox"), (None, "mybox"));
        assert_eq!(split_name("claude/mybox"), (Some("claude"), "mybox"));
        assert_eq!(split_name("env/claude/mybox"), (Some("claude"), "mybox"));
    }

    #[test]
    fn short_ids_are_prefixes_and_survive_a_missing_id() {
        let r = ExecRecord {
            id: "a3f1c2deadbeef".into(),
            ..ExecRecord::default()
        };
        assert_eq!(r.short_id(), "a3f1c2");
        assert_eq!(ExecRecord::default().short_id(), "?");
    }
}
