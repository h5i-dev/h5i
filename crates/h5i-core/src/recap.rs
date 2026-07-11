//! Import Claude Code "Recap" / `away_summary` records into the context workspace.
//!
//! Claude Code periodically emits a JSONL record shaped like:
//!
//! ```json
//! {"type":"system","subtype":"away_summary",
//!  "content":"Goal: ... I did ... Next: ... (disable recaps in /config)",
//!  "uuid":"...","timestamp":"...","sessionId":"...","gitBranch":"..."}
//! ```
//!
//! `h5i context recap` scans the current project's session log, finds any
//! `away_summary` records not yet imported, and creates one `h5i context commit`
//! per recap. A set of imported UUIDs is kept at the root of the context tree in
//! `recaps.json` so repeated runs are idempotent.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ctx;
use crate::error::H5iError;
use crate::session_log;

/// File at the root of the context tree that tracks imported recap UUIDs.
pub const RECAPS_FILE: &str = "recaps.json";

/// The trailing marker Claude Code appends to every away_summary body.
const DISABLE_MARKER: &str = "(disable recaps in /config)";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recap {
    pub uuid: String,
    pub session_id: String,
    pub timestamp: String,
    pub git_branch: String,
    pub content: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct RecapsIndex {
    #[serde(default)]
    imported: BTreeSet<String>,
}

#[derive(Debug, Default, Clone)]
pub struct ImportOpts {
    /// Only import recaps with timestamp strictly after this cutoff.
    pub since: Option<DateTime<Utc>>,
    /// Explicit JSONL path to scan. If `None`, the latest session for this
    /// workdir is auto-discovered via `session_log::find_latest_session`.
    pub session_path: Option<PathBuf>,
    /// If true, report what would be imported without mutating the workspace.
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
pub struct ImportedRecap {
    pub recap: Recap,
    /// `true` when the recap was skipped (e.g. already imported).
    pub skipped: bool,
    pub reason: Option<String>,
}

/// Extract every `away_summary` system record from a Claude Code JSONL log.
pub fn parse_recaps_from_jsonl(path: &Path) -> Result<Vec<Recap>, H5iError> {
    let raw = fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) != Some("system") {
            continue;
        }
        if v.get("subtype").and_then(Value::as_str) != Some("away_summary") {
            continue;
        }
        let Some(content) = v.get("content").and_then(Value::as_str) else {
            continue;
        };
        let Some(uuid) = v.get("uuid").and_then(Value::as_str) else {
            continue;
        };
        out.push(Recap {
            uuid: uuid.to_string(),
            session_id: v
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            timestamp: v
                .get("timestamp")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            git_branch: v
                .get("gitBranch")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            content: content.to_string(),
        });
    }
    Ok(out)
}

/// Best-effort split of a recap body into `(summary, detail)`.
///
/// Recap bodies conventionally follow `"Goal: … <what was done>. Next: …"`.
/// We use `" Next:"` as the pivot: everything before becomes the one-line
/// summary, everything after becomes the detail. The trailing
/// `"(disable recaps in /config)"` marker is stripped from both halves.
pub fn split_summary_detail(content: &str) -> (String, String) {
    let cleaned = content.replace(DISABLE_MARKER, "").trim().to_string();
    let lower = cleaned.to_lowercase();
    if let Some(idx) = lower.find(" next:") {
        let summary = cleaned[..idx].trim().to_string();
        let detail = cleaned[idx + 1..].trim().to_string();
        return (summary, detail);
    }
    (cleaned, String::new())
}

/// Scan the current session log and import any new recaps as context commits.
pub fn import_recaps(workdir: &Path, opts: &ImportOpts) -> Result<Vec<ImportedRecap>, H5iError> {
    if !ctx::is_initialized(workdir) {
        return Err(H5iError::InvalidPath(
            "context workspace not initialized — run `h5i context init` first".into(),
        ));
    }

    let path = match &opts.session_path {
        Some(p) => p.clone(),
        None => session_log::find_latest_session(workdir).ok_or_else(|| {
            H5iError::InvalidPath(
                "no Claude Code session log found for this workdir".into(),
            )
        })?,
    };

    let mut recaps = parse_recaps_from_jsonl(&path)?;
    recaps.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

    if let Some(cutoff) = opts.since {
        recaps.retain(|r| {
            r.timestamp
                .parse::<DateTime<Utc>>()
                .map(|t| t > cutoff)
                .unwrap_or(true)
        });
    }

    let mut index = read_index(workdir);
    let mut results = Vec::with_capacity(recaps.len());

    for r in recaps {
        if index.imported.contains(&r.uuid) {
            results.push(ImportedRecap {
                recap: r,
                skipped: true,
                reason: Some("already imported".into()),
            });
            continue;
        }

        let (mut summary, detail) = split_summary_detail(&r.content);
        if summary.is_empty() {
            let short = r.uuid.get(..8).unwrap_or(&r.uuid);
            summary = format!("recap {short}");
        }

        let tagged_detail = if detail.is_empty() {
            format!("[recap-uuid: {}]\n[recap-session: {}]", r.uuid, r.session_id)
        } else {
            format!(
                "{detail}\n\n[recap-uuid: {}]\n[recap-session: {}]",
                r.uuid, r.session_id
            )
        };

        if !opts.dry_run {
            ctx::gcc_commit(workdir, &summary, &tagged_detail)?;
            index.imported.insert(r.uuid.clone());
        }

        results.push(ImportedRecap {
            recap: r,
            skipped: false,
            reason: None,
        });
    }

    if !opts.dry_run {
        write_index(workdir, &index)?;
    }

    Ok(results)
}

fn read_index(workdir: &Path) -> RecapsIndex {
    ctx::read_ctx_file(workdir, RECAPS_FILE)
        .and_then(|s| serde_json::from_str::<RecapsIndex>(&s).ok())
        .unwrap_or_default()
}

fn write_index(workdir: &Path, idx: &RecapsIndex) -> Result<(), H5iError> {
    let s = serde_json::to_string_pretty(idx)
        .map_err(|e| H5iError::InvalidPath(format!("recap index serialize: {e}")))?;
    ctx::write_ctx_file(workdir, RECAPS_FILE, &s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp_jsonl(lines: &[&str]) -> PathBuf {
        let mut p = std::env::temp_dir();
        let n: u64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
        p.push(format!("h5i-recap-{n}.jsonl"));
        let mut f = fs::File::create(&p).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
        p
    }

    #[test]
    fn split_extracts_goal_and_next() {
        let body = "Goal: simplify the README. I rewrote it. \
                    Next: your review. (disable recaps in /config)";
        let (s, d) = split_summary_detail(body);
        assert_eq!(s, "Goal: simplify the README. I rewrote it.");
        assert!(d.starts_with("Next: your review."));
        assert!(!d.contains("disable recaps"));
    }

    #[test]
    fn split_without_next_returns_full_body_as_summary() {
        let body = "Goal: something. (disable recaps in /config)";
        let (s, d) = split_summary_detail(body);
        assert_eq!(s, "Goal: something.");
        assert!(d.is_empty());
    }

    #[test]
    fn parse_keeps_only_away_summary_records() {
        let path = write_temp_jsonl(&[
            r#"{"type":"user","message":{"content":[]},"uuid":"u1"}"#,
            r#"{"type":"system","subtype":"hook","content":"x","uuid":"u2"}"#,
            r#"{"type":"system","subtype":"away_summary","content":"Goal: a. Next: b.","uuid":"u3","timestamp":"2026-04-23T12:00:00Z","sessionId":"s1","gitBranch":"main"}"#,
            r#"not json"#,
            r#"{"type":"system","subtype":"away_summary","content":"Goal: c.","uuid":"u4","timestamp":"2026-04-23T12:01:00Z","sessionId":"s1"}"#,
        ]);

        let recaps = parse_recaps_from_jsonl(&path).unwrap();
        fs::remove_file(&path).ok();

        assert_eq!(recaps.len(), 2);
        assert_eq!(recaps[0].uuid, "u3");
        assert_eq!(recaps[0].git_branch, "main");
        assert_eq!(recaps[1].uuid, "u4");
    }

    #[test]
    fn import_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let workdir = tmp.path();

        // Init a bare git repo so ctx can attach refs to it.
        git2::Repository::init(workdir).unwrap();
        ctx::init(workdir, "test").unwrap();

        let log = write_temp_jsonl(&[
            r#"{"type":"system","subtype":"away_summary","content":"Goal: one. Next: two.","uuid":"u-alpha","timestamp":"2026-04-23T12:00:00Z","sessionId":"s1","gitBranch":"main"}"#,
            r#"{"type":"system","subtype":"away_summary","content":"Goal: three. Next: four.","uuid":"u-beta","timestamp":"2026-04-23T12:01:00Z","sessionId":"s1","gitBranch":"main"}"#,
        ]);

        let opts = ImportOpts {
            session_path: Some(log.clone()),
            ..Default::default()
        };

        let first = import_recaps(workdir, &opts).unwrap();
        assert_eq!(first.iter().filter(|r| !r.skipped).count(), 2);

        let second = import_recaps(workdir, &opts).unwrap();
        assert_eq!(second.iter().filter(|r| !r.skipped).count(), 0);
        assert_eq!(second.iter().filter(|r| r.skipped).count(), 2);

        fs::remove_file(&log).ok();
    }

    #[test]
    fn dry_run_does_not_persist() {
        let tmp = tempfile::tempdir().unwrap();
        let workdir = tmp.path();
        git2::Repository::init(workdir).unwrap();
        ctx::init(workdir, "test").unwrap();

        let log = write_temp_jsonl(&[
            r#"{"type":"system","subtype":"away_summary","content":"Goal: x. Next: y.","uuid":"u-dry","timestamp":"2026-04-23T12:00:00Z","sessionId":"s1","gitBranch":"main"}"#,
        ]);

        let opts = ImportOpts {
            session_path: Some(log.clone()),
            dry_run: true,
            ..Default::default()
        };

        let r1 = import_recaps(workdir, &opts).unwrap();
        assert_eq!(r1.iter().filter(|r| !r.skipped).count(), 1);

        // A second non-dry-run should still import everything.
        let opts2 = ImportOpts {
            session_path: Some(log.clone()),
            ..Default::default()
        };
        let r2 = import_recaps(workdir, &opts2).unwrap();
        assert_eq!(r2.iter().filter(|r| !r.skipped).count(), 1);

        fs::remove_file(&log).ok();
    }
}
