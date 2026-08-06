//! Execution receipts: the honest record of what actually ran in a box.
//!
//! One append-only JSONL log per environment (`<env>/receipt.jsonl`) plus the
//! raw payload of each record under `<env>/receipts/<id>.raw`. A record is
//! generated from observation — the command, its exit code, its resource
//! accounting, the egress verdicts the proxy handed down — never from the
//! agent's account of itself.
//!
//! Two properties the design depends on:
//!
//! * **Append only.** [`append`] opens the log with `O_APPEND` and never
//!   rewrites an existing line. A reader tolerates a malformed tail line
//!   (a torn concurrent write) instead of failing the whole read.
//! * **Redacted at the boundary.** Secrets are scrubbed from the command and
//!   from the raw payload *before* either is written, and the scrub is
//!   recorded by rule id, never by value.
//!
//! The store is host side today. Section 5.7 of the roadmap moves the writer
//! to an inherited fd owned by a host collector so an in-box agent cannot
//! rewrite what it already reported; the record shape here is what that
//! collector will carry.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::H5iError;
use crate::refstore::sha256_hex;
use crate::sandbox_policy::EgressSummary;

/// Log of records, relative to an env directory.
const LOG_FILE: &str = "receipt.jsonl";
/// Directory of raw payloads, relative to an env directory.
const RAW_DIR: &str = "receipts";
/// Largest raw payload stored per record. Beyond this the head is kept and the
/// truncation is stated in the record, so a reader is never silently given a
/// partial view.
const RAW_CAP: usize = 4 * 1024 * 1024;
/// Length of the short handle used on the CLI.
const ID_LEN: usize = 16;

/// What the page said back, for a run that drove the browser.
///
/// The agent's own account of a UI check is the least trustworthy part of an
/// export: "I clicked Submit and it worked" is a sentence, not evidence. This is
/// the observed half — the verb that ran, and the console messages, page errors
/// and failed requests that followed it.
///
/// **Lane.** This is box-claimed, like `tee-shim`: the numbers come out of the
/// browser inside the box. What makes it useful anyway is that h5i decides
/// *when* to collect it (right after the browser command, in the same policy)
/// rather than the agent choosing what to report. An agent can still close the
/// browser to stop the record — and a run with a browser verb and no evidence
/// block is itself visible.
///
/// **Only what is new.** The browser's buffers accumulate for the life of a
/// session, so every field here is the slice since the previous drain, tracked
/// by a host-side cursor outside the box's write grants. Repeating the whole
/// buffer on every record would bury the one error that just appeared.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserEvidence {
    /// The agent-browser verb this run invoked (`open`, `click`, `snapshot`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verb: Option<String>,
    /// Console messages at `error`/`warning` level, newest last. Ordinary
    /// `log`/`info` chatter is not evidence and is not carried.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub console: Vec<String>,
    /// Uncaught exceptions and page errors.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
    /// Requests that failed: HTTP >= 400, or no status at all (blocked, refused,
    /// or denied by the egress allowlist — which is the interesting case in a
    /// box).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_requests: Vec<String>,
    /// Entries dropped by the per-record cap, so a flood is never silently
    /// rendered as a clean page.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// Set when the drain could not reach a browser (no daemon, or the run
    /// closed it). Distinguishes "nothing happened" from "nothing was looked
    /// at", which a reviewer must be able to tell apart.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unavailable: bool,
}

impl BrowserEvidence {
    /// Did the page complain at all? Drives the one-line summary.
    pub fn is_clean(&self) -> bool {
        self.console.is_empty() && self.errors.is_empty() && self.failed_requests.is_empty()
    }
}

/// One observed execution inside an environment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecRecord {
    /// Short, stable handle for the CLI (`h5i box inspect --capture <id>`).
    pub id: String,
    /// RFC3339 UTC, microsecond precision, lexically sortable.
    pub timestamp: String,
    /// The environment this is evidence for, e.g. `env/claude/fix-auth`.
    pub env_id: String,
    /// sha256 of the resolved policy in force — what was actually enforced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_digest: Option<String>,
    /// Which lane observed this: `host-env-run`, `tee-shim`, `shell-egress`.
    /// Host-observed and box-claimed lanes stay distinguishable forever.
    pub source: String,
    /// The command, secret-redacted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// True when the wall-clock limit killed the process group.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub timed_out: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rss_kb: Option<u64>,
    /// The HEAD tree the run was taken against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_tree: Option<String>,
    /// Files this record is about, repo-relative.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// Network verdicts from the allowlist proxy (container tier). Host
    /// observed: the box never supplies this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress: Option<EgressSummary>,
    /// What the browser observed, when this run drove one. See
    /// [`BrowserEvidence`] for the lane and the cursor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<BrowserEvidence>,
    /// Secret rules that fired while redacting, by rule id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redactions: Vec<String>,
    /// Full content address of the stored raw payload.
    pub raw_oid: String,
    pub raw_size: u64,
    pub raw_lines: usize,
    /// True when the payload exceeded [`RAW_CAP`] and only the head was kept.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub raw_truncated: bool,
}

/// The observed facts a caller hands to [`append`]. Everything derived from
/// the payload (id, digest, sizes, redaction rules) is computed here rather
/// than trusted from the caller.
#[derive(Debug, Clone, Default)]
pub struct RecordInput {
    pub env_id: String,
    pub policy_digest: Option<String>,
    pub source: String,
    pub cmd: Option<String>,
    pub cwd: Option<String>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub wall_ms: Option<u64>,
    pub cpu_ms: Option<u64>,
    pub max_rss_kb: Option<u64>,
    pub git_tree: Option<String>,
    pub files: Vec<String>,
    pub egress: Option<EgressSummary>,
    pub browser: Option<BrowserEvidence>,
}

/// Scrub every string a page supplied. A console line is a place a token turns
/// up routinely — a fetch that logs the request it just made, a framework
/// dumping config on error — and the whole point of the receipt is that it can
/// be handed to a reviewer.
fn redact_browser_evidence(mut b: BrowserEvidence) -> BrowserEvidence {
    let scrub = |v: &mut Vec<String>| {
        for s in v.iter_mut() {
            *s = crate::secrets::redact_text(s);
        }
    };
    scrub(&mut b.console);
    scrub(&mut b.errors);
    scrub(&mut b.failed_requests);
    b
}

fn log_path(env_dir: &Path) -> PathBuf {
    env_dir.join(LOG_FILE)
}

fn raw_path(env_dir: &Path, id: &str) -> PathBuf {
    env_dir.join(RAW_DIR).join(format!("{id}.raw"))
}

/// RFC3339 UTC with microsecond precision — lexically sortable, so the log's
/// file order and its time order agree.
fn now_ts() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

/// Monotonic tiebreaker for [`run_id`]. Two runs can share a microsecond stamp
/// *and* every other input (`true` twice in a tight loop), so the id needs one
/// component that cannot repeat within a process.
static RUN_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The handle for one *run*.
///
/// This used to be `sha256(payload)[..16]` — the content address of the output
/// and nothing else. That made the id collide across genuinely different runs:
/// `true` and `false` both produce no output, so both got the same id, as did
/// `echo hello` and `printf hello`. Lookup is first-match-wins, so the second
/// run of a colliding pair became unreachable through `inspect`, and `compare`
/// showed the wrong command and exit code for a box's latest run.
///
/// So the id now covers what distinguishes a run — when it happened, where, what
/// was executed and how it ended — with the payload digest still folded in, plus
/// the process-local sequence number. `raw_oid` remains the content address, so
/// nothing about payload deduplication changes.
fn run_id(timestamp: &str, input: &RecordInput, digest: &str) -> String {
    let seq = RUN_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // NUL separators: no field can contain one, so no two distinct field sets
    // can flatten to the same string.
    let material = format!(
        "{timestamp}\0{}\0{}\0{}\0{}\0{}\0{}\0{seq}\0{digest}\0{}",
        input.env_id,
        input.policy_digest.as_deref().unwrap_or(""),
        input.source,
        input.cmd.as_deref().unwrap_or(""),
        input.exit_code.map(|c| c.to_string()).unwrap_or_default(),
        input.timed_out,
        std::process::id(),
    );
    sha256_hex(material.as_bytes())[..ID_LEN].to_string()
}

/// Record one observed execution and store its raw payload.
///
/// The payload and the command are secret-redacted before anything is written.
/// Returns the record as stored, whose `id` is the handle the CLI shows.
pub fn append(env_dir: &Path, input: RecordInput, raw: &[u8]) -> Result<ExecRecord, H5iError> {
    // Redact first: hashing, sizing and storing all happen on the scrubbed
    // bytes, so a credential can never reach the stored payload.
    let mut redactions: Vec<String> = Vec::new();
    let redacted_holder;
    let raw: &[u8] = match std::str::from_utf8(raw) {
        Ok(text) => {
            let findings = crate::secrets::scan_text(Path::new("<receipt>"), text);
            if findings.is_empty() {
                raw
            } else {
                let mut ids: Vec<String> = findings.iter().map(|f| f.rule_id.to_string()).collect();
                ids.sort();
                ids.dedup();
                redactions = ids;
                redacted_holder = crate::secrets::redact_text(text).into_bytes();
                &redacted_holder[..]
            }
        }
        // Binary payloads are left as-is: the secret scanner is line oriented.
        Err(_) => raw,
    };

    let raw_truncated = raw.len() > RAW_CAP;
    let stored: &[u8] = if raw_truncated { &raw[..RAW_CAP] } else { raw };

    let digest = sha256_hex(stored);
    let timestamp = now_ts();
    let id = run_id(&timestamp, &input, &digest);

    let rec = ExecRecord {
        id: id.clone(),
        timestamp,
        env_id: input.env_id,
        policy_digest: input.policy_digest,
        source: input.source,
        cmd: input.cmd.as_deref().map(crate::secrets::redact_text),
        cwd: input.cwd,
        exit_code: input.exit_code,
        timed_out: input.timed_out,
        wall_ms: input.wall_ms,
        cpu_ms: input.cpu_ms,
        max_rss_kb: input.max_rss_kb,
        git_tree: input.git_tree,
        files: input.files,
        egress: input.egress,
        // Box-claimed strings from a page the box just visited, so they go
        // through the same scrub as everything else before they are stored.
        browser: input.browser.map(redact_browser_evidence),
        redactions,
        raw_oid: format!("sha256:{digest}"),
        raw_size: stored.len() as u64,
        raw_lines: bytecount_lines(stored),
        raw_truncated,
    };

    // The blob stays content addressed — keyed by the payload digest, NOT by the
    // record id, which now identifies the run instead. Two runs that printed the
    // same bytes still share one stored payload; they simply no longer share a
    // handle. The key keeps the same shape as the ids written before this split,
    // so payloads stored by older versions remain readable.
    let raw_file = raw_path(env_dir, &digest[..ID_LEN]);
    if let Some(parent) = raw_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // An identical payload is already on disk and rewriting it would only risk a
    // torn file for a concurrent reader.
    if !raw_file.exists() {
        std::fs::write(&raw_file, stored)?;
    }

    let mut line = serde_json::to_string(&rec)?;
    line.push('\n');
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path(env_dir))?;
    f.write_all(line.as_bytes())?;

    Ok(rec)
}

/// Every record for this env, oldest first.
///
/// A malformed trailing line (a torn concurrent append) is skipped rather than
/// failing the read: a partially written record must not hide the complete
/// ones before it.
pub fn list(env_dir: &Path) -> Result<Vec<ExecRecord>, H5iError> {
    let path = log_path(env_dir);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)?;
    Ok(text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<ExecRecord>(l).ok())
        .collect())
}

/// Shortest handle prefix `find` will resolve. Shorter is refused rather than
/// silently rendering whichever record happens to match first.
const MIN_HANDLE: usize = 4;

/// One record by its handle: the full id, or a unique prefix of at least
/// [`MIN_HANDLE`] characters. An ambiguous prefix is an error, never a guess.
pub fn find(env_dir: &Path, handle: &str) -> Result<ExecRecord, H5iError> {
    let all = list(env_dir)?;
    if let Some(exact) = all.iter().find(|r| r.id == handle) {
        return Ok(exact.clone());
    }
    if handle.len() < MIN_HANDLE {
        return Err(H5iError::Metadata(format!(
            "capture handle '{handle}' is too short — give at least {MIN_HANDLE} characters"
        )));
    }
    let mut hits = all.into_iter().filter(|r| r.id.starts_with(handle));
    match (hits.next(), hits.next()) {
        (Some(one), None) => Ok(one),
        (Some(a), Some(b)) => Err(H5iError::Metadata(format!(
            "capture handle '{handle}' is ambiguous — matches {} and {} (give more characters)",
            a.id, b.id
        ))),
        _ => Err(H5iError::Metadata(format!(
            "no object matches '{handle}' in this environment"
        ))),
    }
}

/// The stored raw payload for a record, by run handle.
///
/// Two steps, because a run id and a payload key are no longer the same string:
/// resolve the record, then read the blob its `raw_oid` names. Records written
/// before that split used the payload digest as their id, so a direct hit on the
/// handle is tried as a fallback and keeps those readable.
pub fn raw_bytes(env_dir: &Path, id: &str) -> Result<Vec<u8>, H5iError> {
    if let Ok(rec) = find(env_dir, id) {
        if let Some(key) = blob_key(&rec.raw_oid) {
            let p = raw_path(env_dir, &key);
            if p.exists() {
                return Ok(std::fs::read(p)?);
            }
        }
    }
    let p = raw_path(env_dir, id);
    if !p.exists() {
        return Err(H5iError::Metadata(format!(
            "receipt {id} has no stored payload"
        )));
    }
    Ok(std::fs::read(p)?)
}

/// Blob filename for a `sha256:<hex>` object id.
fn blob_key(raw_oid: &str) -> Option<String> {
    let hex = raw_oid.strip_prefix("sha256:")?;
    (hex.len() >= ID_LEN).then(|| hex[..ID_LEN].to_string())
}

/// Human rendering of one record, for `h5i box inspect --capture <id>`.
pub fn render(rec: &ExecRecord, raw: &[u8]) -> String {
    let mut out = String::new();
    out.push_str(&format!("── Receipt {} ({}) ──\n", rec.id, rec.env_id));
    if let Some(cmd) = &rec.cmd {
        out.push_str(&format!("  cmd      : {cmd}\n"));
    }
    out.push_str(&format!(
        "  exit     : {}{}\n",
        rec.exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".into()),
        if rec.timed_out { " (timed out)" } else { "" }
    ));
    if let Some(d) = &rec.policy_digest {
        out.push_str(&format!("  policy   : {}\n", &d[..12.min(d.len())]));
    }
    out.push_str(&format!("  source   : {}\n", rec.source));
    if let (Some(w), Some(c)) = (rec.wall_ms, rec.cpu_ms) {
        let rss = rec
            .max_rss_kb
            .map(|kb| format!(", rss {}MiB", kb / 1024))
            .unwrap_or_default();
        out.push_str(&format!("  cost     : wall {w}ms, cpu {c}ms{rss}\n"));
    }
    if let Some(eg) = &rec.egress {
        out.push_str(&format!(
            "  egress   : {} allowed, {} denied\n",
            eg.allowed, eg.denied
        ));
    }
    if let Some(b) = &rec.browser {
        let verb = b.verb.as_deref().unwrap_or("browser");
        if b.unavailable {
            out.push_str(&format!("  browser  : {verb} (no browser to observe)\n"));
        } else if b.is_clean() {
            out.push_str(&format!("  browser  : {verb}, page clean\n"));
        } else {
            out.push_str(&format!(
                "  browser  : {verb} — {} console, {} page error(s), {} failed request(s){}\n",
                b.console.len(),
                b.errors.len(),
                b.failed_requests.len(),
                if b.truncated { ", capped" } else { "" }
            ));
            // The detail is the point: a count tells a reviewer something
            // happened, the line tells them what.
            for line in b
                .errors
                .iter()
                .chain(b.console.iter())
                .chain(b.failed_requests.iter())
            {
                out.push_str(&format!("           · {line}\n"));
            }
        }
    }
    if !rec.redactions.is_empty() {
        out.push_str(&format!("  redacted : {}\n", rec.redactions.join(", ")));
    }
    out.push_str(&format!(
        "  raw      : {} bytes, {} lines{}\n",
        rec.raw_size,
        rec.raw_lines,
        if rec.raw_truncated {
            " (truncated at cap)"
        } else {
            ""
        }
    ));
    out.push('\n');
    out.push_str(&String::from_utf8_lossy(raw));
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn bytecount_lines(bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        return 0;
    }
    let n = bytes.iter().filter(|b| **b == b'\n').count();
    if bytes.last() == Some(&b'\n') {
        n
    } else {
        n + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn input() -> RecordInput {
        RecordInput {
            env_id: "env/tester/demo".into(),
            source: "host-env-run".into(),
            cmd: Some("echo hi".into()),
            exit_code: Some(0),
            ..Default::default()
        }
    }

    #[test]
    fn runs_that_share_an_output_still_get_distinct_ids() {
        let td = TempDir::new().unwrap();

        // The pair that used to collide: no output at all, different exit code.
        let ok = append(
            td.path(),
            RecordInput {
                cmd: Some("/usr/bin/true".into()),
                exit_code: Some(0),
                ..input()
            },
            b"",
        )
        .unwrap();
        let bad = append(
            td.path(),
            RecordInput {
                cmd: Some("/usr/bin/false".into()),
                exit_code: Some(1),
                ..input()
            },
            b"",
        )
        .unwrap();
        assert_ne!(ok.id, bad.id, "distinct runs must not share a handle");

        // Byte-identical runs of the *same* command are still distinct events.
        let a = append(td.path(), input(), b"hello\n").unwrap();
        let b = append(td.path(), input(), b"hello\n").unwrap();
        assert_ne!(a.id, b.id);

        // ...while the payload stays content addressed: one blob, one oid.
        assert_eq!(a.raw_oid, b.raw_oid);
        assert_eq!(
            std::fs::read_dir(td.path().join(RAW_DIR)).unwrap().count(),
            2,
            "two payloads stored: the empty one and `hello`"
        );

        // Every handle resolves to its own record, and to the right bytes.
        for rec in [&ok, &bad, &a, &b] {
            assert_eq!(find(td.path(), &rec.id).unwrap().id, rec.id);
        }
        assert_eq!(find(td.path(), &bad.id).unwrap().exit_code, Some(1));
        assert_eq!(raw_bytes(td.path(), &a.id).unwrap(), b"hello\n");
        assert_eq!(raw_bytes(td.path(), &ok.id).unwrap(), b"");
    }

    #[test]
    fn payloads_written_under_the_old_id_keyed_layout_still_read_back() {
        // Pre-split receipts used the payload digest as the record id, so the
        // blob sits at `<id>.raw`. Those envs must not lose their evidence.
        let td = TempDir::new().unwrap();
        let raw = b"legacy payload\n";
        let rec = append(td.path(), input(), raw).unwrap();

        // Rewind the record to the old shape: id == payload digest prefix,
        // which is also where `append` already put the blob.
        let legacy_id = rec.raw_oid.strip_prefix("sha256:").unwrap()[..ID_LEN].to_string();
        let mut stored: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(log_path(td.path())).unwrap()).unwrap();
        stored["id"] = serde_json::Value::String(legacy_id.clone());
        std::fs::write(log_path(td.path()), format!("{stored}\n")).unwrap();

        assert_eq!(raw_bytes(td.path(), &legacy_id).unwrap(), raw);
    }

    #[test]
    fn append_then_read_back() {
        let td = TempDir::new().unwrap();
        let rec = append(td.path(), input(), b"hello\nworld\n").unwrap();
        assert_eq!(rec.raw_lines, 2);
        assert_eq!(rec.raw_size, 12);
        assert!(rec.raw_oid.starts_with("sha256:"));

        let all = list(td.path()).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, rec.id);
        assert_eq!(raw_bytes(td.path(), &rec.id).unwrap(), b"hello\nworld\n");
    }

    #[test]
    fn appends_accumulate_in_order() {
        let td = TempDir::new().unwrap();
        append(td.path(), input(), b"one").unwrap();
        append(td.path(), input(), b"two").unwrap();
        let all = list(td.path()).unwrap();
        assert_eq!(all.len(), 2);
        assert!(all[0].timestamp <= all[1].timestamp);
    }

    #[test]
    fn secrets_never_reach_the_stored_payload() {
        let td = TempDir::new().unwrap();
        let raw = "export TOKEN=ghp_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789\n";
        let rec = append(td.path(), input(), raw.as_bytes()).unwrap();
        assert!(!rec.redactions.is_empty(), "a secret rule must fire");
        let stored = String::from_utf8(raw_bytes(td.path(), &rec.id).unwrap()).unwrap();
        assert!(
            !stored.contains("ghp_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789"),
            "stored payload still carries the credential: {stored}"
        );
    }

    #[test]
    fn command_is_redacted_before_it_is_recorded() {
        let td = TempDir::new().unwrap();
        let mut i = input();
        i.cmd = Some("curl -H 'Authorization: Bearer ghp_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789'".into());
        let rec = append(td.path(), i, b"").unwrap();
        assert!(!rec.cmd.unwrap().contains("ghp_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789"));
    }

    #[test]
    fn oversized_payload_is_capped_and_says_so() {
        let td = TempDir::new().unwrap();
        let big = vec![b'x'; RAW_CAP + 1024];
        let rec = append(td.path(), input(), &big).unwrap();
        assert!(rec.raw_truncated);
        assert_eq!(rec.raw_size, RAW_CAP as u64);
    }

    #[test]
    fn a_torn_tail_line_does_not_hide_complete_records() {
        let td = TempDir::new().unwrap();
        append(td.path(), input(), b"one").unwrap();
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(log_path(td.path()))
            .unwrap();
        f.write_all(b"{\"id\":\"trunc").unwrap();
        let all = list(td.path()).unwrap();
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn missing_id_is_an_error_not_a_panic() {
        let td = TempDir::new().unwrap();
        assert!(find(td.path(), "deadbeef").is_err());
        assert!(raw_bytes(td.path(), "deadbeef").is_err());
    }

    #[test]
    fn resolves_a_unique_prefix_and_refuses_a_short_one() {
        let td = TempDir::new().unwrap();
        let rec = append(td.path(), input(), b"prefix test").unwrap();
        let by_prefix = find(td.path(), &rec.id[..8]).unwrap();
        assert_eq!(by_prefix.id, rec.id);

        let too_short = find(td.path(), &rec.id[..2]).unwrap_err().to_string();
        assert!(too_short.contains("too short"), "{too_short}");

        let unknown = find(td.path(), "zzzzzzzz").unwrap_err().to_string();
        assert!(unknown.contains("no object matches"), "{unknown}");
    }
}
