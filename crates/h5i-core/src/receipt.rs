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

/// What an ingress session was, as structured fact rather than as prose.
///
/// The one thing a reviewer must be able to read off a share record without
/// ambiguity is **whether a third party could read the traffic** — a Cloudflare
/// quick tunnel terminates TLS, and peer-to-peer does not. That was being
/// recovered by testing whether the rendered command string contained the
/// substring `tunnel`, and the command string contains the box's name: a
/// perfectly ordinary P2P share of a box called `tunnel`, `my-tunnel` or
/// `tunneling` was reported to the reviewer as Cloudflare-terminated. A wrong
/// security claim in the evidence artifact is worse than a missing one.
///
/// So it is a field. Prose is rendered from this; nothing parses the prose.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShareEvidence {
    /// `p2p` or `tunnel`, as `h5i-share` recorded it.
    pub transport: String,
    /// The port inside the box that was exposed.
    pub port: u16,
    /// Distinct peers admitted, including any past the receipt's record cap.
    pub peers: u64,
    /// How long the share ran, in seconds, from the monotonic clock.
    pub seconds: i64,
    /// Connections refused before a ticket was weighed at all.
    #[serde(default)]
    pub turned_away: u64,
}

impl ShareEvidence {
    /// Is a third party able to read this traffic in the clear?
    ///
    /// The question the export's warning is really asking. Decided on the
    /// recorded transport, and `true` for anything this h5i does not recognise
    /// — an unknown transport is not a promise of end-to-end encryption.
    pub fn third_party_can_read(&self) -> bool {
        self.transport != "p2p"
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
    /// sha256 of `policy.effective.json` — the kernel-tier enforced
    /// configuration, serialized at the apply seam inside
    /// `build_confined_command` (ROADMAP.md §P1). Absent for tiers that write
    /// no dump and for records from before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_digest: Option<String>,
    /// Other boxes **of this repository** materialized on this host whose
    /// effective Landlock grants
    /// overlap this box's — cross-box influence possible through the shared
    /// path each entry names (`env/<agent>/<slug> via <path>`). Empty is the
    /// strong answer: a clean check in both directions means neither box can
    /// influence the other through their granted filesystems. Scope
    /// is exactly the grant lists — binds, network, and host processes
    /// outside any box are not covered, and a listed overlap may be closed
    /// in practice by a bind (the private `/tmp` redirect notably); the
    /// check fails safe, never silent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fs_overlap: Vec<String>,
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
    /// What an ingress session was, when this record is one. Host observed:
    /// h5i owned both ends of the bridge and the box supplied none of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share: Option<ShareEvidence>,
    /// What the kernel saw, when the runtime-detection lane was watching
    /// (ROADMAP.md D10).
    ///
    /// A second observer of the same command, in its own lane. `source` above
    /// stays `host-env-run`, deliberately: the record is *about the command*,
    /// and this is a different observer of that command rather than a
    /// different record. Present even when the detector could not attach — the
    /// block then carries the reason — because a missing block and a quiet box
    /// would otherwise look identical, which is the confusion this lane exists
    /// to remove.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<h5i_bpf::RuntimeEvidence>,
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
    pub effective_digest: Option<String>,
    pub fs_overlap: Vec<String>,
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
    pub share: Option<ShareEvidence>,
    pub runtime: Option<h5i_bpf::RuntimeEvidence>,
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

/// Scrub the exemplars a detection carries.
///
/// The rules engine has already sanitised these for *terminal control
/// sequences* — that is about rendering. This is the other half: a secret that
/// happened to be in a path or an `argv[1]` must not reach `refs/h5i/objects`,
/// and the receipt's own pattern-based redaction is the thing that decides
/// what a secret looks like.
fn redact_runtime_evidence(
    mut r: h5i_bpf::RuntimeEvidence,
) -> h5i_bpf::RuntimeEvidence {
    for d in &mut r.detections {
        for e in &mut d.examples {
            *e = crate::secrets::redact_text(e);
        }
    }
    r
}

fn log_path(env_dir: &Path) -> PathBuf {
    env_dir.join(LOG_FILE)
}

fn raw_path(env_dir: &Path, id: &str) -> PathBuf {
    env_dir.join(RAW_DIR).join(format!("{id}.raw"))
}

/// Redact the decodable runs of a non-UTF-8 payload, preserving every other
/// byte exactly. Splitting on the invalid sequences is what keeps a credential
/// from hiding behind one stray byte.
fn redact_binary(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut rest = raw;
    loop {
        match std::str::from_utf8(rest) {
            Ok(text) => {
                out.extend_from_slice(crate::secrets::redact_text(text).as_bytes());
                return out;
            }
            Err(e) => {
                let good = e.valid_up_to();
                if good > 0 {
                    let text = std::str::from_utf8(&rest[..good]).unwrap_or_default();
                    out.extend_from_slice(crate::secrets::redact_text(text).as_bytes());
                }
                // Copy the invalid sequence through untouched and carry on.
                let skip = e.error_len().unwrap_or(rest.len() - good).max(1);
                let end = (good + skip).min(rest.len());
                out.extend_from_slice(&rest[good..end]);
                if end >= rest.len() {
                    return out;
                }
                rest = &rest[end..];
            }
        }
    }
}

/// Record ids are lowercase hex. Checking that before a handle becomes a path
/// keeps `../..` out of the join. No caller can reach it with hostile input
/// today — `env::inspect` is gated by `find` succeeding on the same handle —
/// but `raw_bytes` is `pub`, and a console route added later would inherit the
/// unvalidated join rather than this refusal.
///
/// Public because the id of a record read back off disk becomes a path in more
/// than one crate module (`export::copy_share_payloads` names the bundle file
/// after it), and every one of those joins needs the same gate. A record is
/// deserialized JSON, so its `id` is whatever the file says — `append` writes a
/// hex digest, but "what h5i wrote" is not what a reader is holding.
pub fn is_record_handle(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_hexdigit())
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
            // Redaction is UNCONDITIONAL. `scan_text` applies a placeholder
            // stoplist (`example`, `dummy`, `fake`, …) and skips the whole line
            // when it hits one, which is right for detection and fail-open for
            // publication: a box printing `example config: ghp_<real>` produced
            // no findings, so the credential was stored verbatim. `redact_line`
            // deliberately has no stoplist for exactly this reason, and there is
            // a regression test pinning that — gating the call on the detector
            // put the hole back one level up.
            //
            // So scan only to name the rules that fired, and always scrub.
            let findings = crate::secrets::scan_text(Path::new("<receipt>"), text);
            let mut ids: Vec<String> = findings.iter().map(|f| f.rule_id.to_string()).collect();
            ids.sort();
            ids.dedup();
            redactions = ids;
            redacted_holder = crate::secrets::redact_text(text).into_bytes();
            &redacted_holder[..]
        }
        // Binary payloads: the pattern scanner is line oriented and cannot run
        // here, but leaving them wholly untouched meant a box could defeat the
        // scrub by interleaving one invalid byte with a credential. Redact the
        // valid UTF-8 runs and leave the rest byte-for-byte, so the payload
        // stays faithful while known secret shapes still go.
        Err(_) => {
            redacted_holder = redact_binary(raw);
            &redacted_holder[..]
        }
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
        effective_digest: input.effective_digest,
        fs_overlap: input.fs_overlap,
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
        // Host observed, and every field is a number or one of two known
        // transport strings, so there is nothing here for the scrub to reach.
        share: input.share,
        // Kernel observed, but the exemplars inside it are *paths and command
        // lines a box chose*, so they get the same scrub as the command and
        // the payload. A credential passed as an argument reaches this lane
        // exactly as readily as it reaches the others.
        runtime: input.runtime.map(redact_runtime_evidence),
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
        // Only under an env directory that still exists. `append` creating
        // the whole tree is what let a share that outlived `h5i box rm`
        // recreate the box it had just erased — as a receipt log and a payload
        // under a path with no manifest, which every tool answers "no
        // environment named that" for and only `rm -rf` clears. Guarding one
        // caller left the next one armed.
        if let Some(env_dir) = parent.parent()
            && !env_dir.exists()
        {
            return Err(H5iError::Metadata(format!(
                "the box directory {} is gone, so there is nowhere to record this",
                env_dir.display()
            )));
        }
        std::fs::create_dir_all(parent)?;
    }
    // An identical payload is already on disk and rewriting it would only risk a
    // torn file for a concurrent reader.
    if !raw_file.exists() {
        // Written to a unique temp and renamed. `fs::write` truncates, so two
        // writers storing the same payload digest at once both truncated and
        // both rewrote, and a reader between them saw a short blob — measured
        // at up to 234 short reads per 1200 iterations in a transplant. Not
        // reachable today, because `run.lock` serialises the high-rate writer
        // and no two lanes produce identical bodies; it becomes reachable the
        // moment a second writer does. The payload is content-addressed, so
        // whichever rename wins is byte-identical to the other.
        let tmp = raw_file.with_extension(format!("raw.tmp.{}", std::process::id()));
        if let Err(e) = std::fs::write(&tmp, stored) {
            let _ = std::fs::remove_file(&tmp);
            return Err(H5iError::with_path(e, &tmp));
        }
        if let Err(e) = std::fs::rename(&tmp, &raw_file) {
            // The same cleanup as above, for the other half of the operation.
            // A rename can fail on its own — a read-only directory, a full
            // disk, EXDEV — and the round that added the cleanup put it only
            // on the write, so exactly the failures that leave a temp behind
            // most often were the ones that left it.
            let _ = std::fs::remove_file(&tmp);
            return Err(H5iError::with_path(e, &raw_file));
        }
    }

    let mut line = serde_json::to_string(&rec)?;
    line.push('\n');
    // Named here as well as on the write below. The comment there says this
    // "used to report `Permission denied` with no hint of which file refused",
    // and on a read-only directory the refusal happens at the `open` — so the
    // naming was attached to the one call that could not reach it.
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path(env_dir))
        .map_err(|e| H5iError::with_path(e, log_path(env_dir)))?;
    // One `write_all` of a line that already ends in a newline, so a short
    // write cannot leave a fragment the *next* append glues itself onto —
    // which `list()` would then drop silently, losing two receipts and saying
    // nothing. And named: this used to report `Permission denied` with no hint
    // of which file refused.
    f.write_all(line.as_bytes())
        .map_err(|e| H5iError::with_path(e, log_path(env_dir)))?;

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
    if let Ok(rec) = find(env_dir, id)
        && let Some(key) = blob_key(&rec.raw_oid)
    {
        let p = raw_path(env_dir, &key);
        if p.exists() {
            return Ok(std::fs::read(p)?);
        }
    }
    if !is_record_handle(id) {
        return Err(H5iError::Metadata(format!(
            "receipt handle '{id}' is not a hex record id (fail-closed)"
        )));
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
///
/// The key becomes a path component, and `raw_oid` is a field of a record
/// deserialized from `receipt.jsonl` — so it is whatever the file says, not
/// necessarily the digest `append` wrote. Two things follow, and neither was
/// being done:
///
/// * The prefix must be **validated as hex**, or `sha256:../../../../../x` is a
///   16-character traversal out of `receipts/` and `raw_bytes` reads a file of
///   the writer's choosing. `valid_handle` exists for exactly this and was
///   applied only on the fallback branch below it.
/// * The slice must be taken on a **character boundary**, or a multi-byte
///   `raw_oid` (`sha256:` + ten `é`) panics the process inside a `[..16]`.
fn blob_key(raw_oid: &str) -> Option<String> {
    let key = raw_oid.strip_prefix("sha256:")?.get(..ID_LEN)?;
    is_record_handle(key).then(|| key.to_string())
}

/// Human rendering of one record, for `h5i box inspect --capture <id>`.
///
/// Everything variable here is sanitised on the way out, because a receipt is
/// read as evidence and half of what it carries was written by the thing being
/// reviewed. `cmd` at the container tier comes from the box's own tee shim; the
/// browser lines are strings a *web page* produced. An escape sequence in any
/// of them rewrites the lines above it, so a box could show the reviewer
/// `exit : 0` and `egress : 0 denied` over a run that was neither. `export`'s
/// `report.md` has sanitised the same fields since it was written; this
/// renderer, which prints to a terminal — the surface where the sequences
/// actually execute — did not.
///
/// The payload goes through [`crate::redact::sanitize_block`] rather than the
/// single-line form: it is meant to have lines, and folding them together would
/// make the one command that shows a captured log useless. Colour sequences go
/// with the rest, which is the trade a document that has to be trustworthy
/// makes.
pub fn render(rec: &ExecRecord, raw: &[u8]) -> String {
    use crate::redact::sanitize_display as clean;
    let mut out = String::new();
    out.push_str(&format!(
        "── Receipt {} ({}) ──\n",
        clean(&rec.id),
        clean(&rec.env_id)
    ));
    if let Some(cmd) = &rec.cmd {
        out.push_str(&format!("  cmd      : {}\n", clean(cmd)));
    }
    out.push_str(&format!(
        "  exit     : {}{}\n",
        rec.exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".into()),
        if rec.timed_out { " (timed out)" } else { "" }
    ));
    if let Some(d) = &rec.policy_digest {
        // `crate::env::short`, not `&d[..12]`. The digest is a field of a
        // record deserialized from `receipt.jsonl`, so it is whatever the file
        // says — and a byte index into a multi-byte string aborts. Same slice,
        // same reasoning, as the one a manifest's `base_commit` needed.
        out.push_str(&format!(
            "  policy   : {}\n",
            clean(crate::env::short(d, 12))
        ));
    }
    out.push_str(&format!("  source   : {}\n", clean(&rec.source)));
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
        let verb = clean(b.verb.as_deref().unwrap_or("browser"));
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
                out.push_str(&format!("           · {}\n", clean(line)));
            }
        }
    }
    if let Some(rt) = &rec.runtime {
        out.push_str(&format!("  runtime  : {}\n", clean(&rt.summary())));
        // The detections, worst first, with their exemplars. Same argument as
        // the browser lines above: a count says something happened, the line
        // says what — and here the line is the difference between "a rule
        // fired" and "it read ~/.aws/credentials".
        for d in &rt.detections {
            out.push_str(&format!(
                "           [{}] {} — {} ×{}\n",
                d.severity.as_str(),
                clean(&d.rule),
                clean(&d.title),
                d.count
            ));
            for ex in &d.examples {
                out.push_str(&format!("             · {}\n", clean(ex)));
            }
            if d.examples_truncated {
                out.push_str("             · …\n");
            }
        }
    }
    if !rec.redactions.is_empty() {
        out.push_str(&format!("  redacted : {}\n", clean(&rec.redactions.join(", "))));
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
    out.push_str(&crate::redact::sanitize_block(&String::from_utf8_lossy(raw)));
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

    /// A record is a line of JSON read back off disk, so `raw_oid` is whatever
    /// the file says. Turning its first 16 characters into a path component
    /// without checking them made `sha256:../../../../../x` a traversal out of
    /// `receipts/`, and a multi-byte one panicked on the slice.
    #[test]
    fn a_forged_raw_oid_cannot_become_a_path_or_a_panic() {
        assert_eq!(blob_key(&format!("sha256:{}", "a".repeat(64))).as_deref(), Some("aaaaaaaaaaaaaaaa"));
        // Traversal: refused, not resolved.
        assert_eq!(blob_key("sha256:../../../../../x"), None);
        assert_eq!(blob_key("sha256:/etc/passwd0000000"), None);
        // Too short to be a key at all.
        assert_eq!(blob_key("sha256:abc"), None);
        // Not a digest.
        assert_eq!(blob_key("../../etc/passwd"), None);
        // A slice that would land mid-character: `None`, not an abort.
        assert_eq!(blob_key(&format!("sha256:{}", "é".repeat(10))), None);

        // End to end: a record whose oid was rewritten resolves nothing rather
        // than reading a file outside the store.
        let td = TempDir::new().unwrap();
        let rec = append(td.path(), input(), b"payload\n").unwrap();
        std::fs::write(td.path().join("secret.raw"), b"host secret\n").unwrap();
        let mut stored: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(log_path(td.path())).unwrap()).unwrap();
        stored["raw_oid"] = serde_json::Value::String("sha256:../../secret000000".into());
        std::fs::write(log_path(td.path()), format!("{stored}\n")).unwrap();
        // The id is still a valid handle, so the fallback branch runs and finds
        // no blob under it — which is the honest answer, not the host file.
        let out = raw_bytes(td.path(), &rec.id);
        assert!(
            out.as_ref().is_err_and(|e| e.to_string().contains("no stored payload")),
            "{out:?}"
        );
    }

    /// A receipt is read as evidence, and half of what it carries was written
    /// by the thing being reviewed. An escape sequence in the command — which
    /// the container tier takes from the box's own tee shim — rewrites the lines
    /// above it, so the box gets to choose what the reviewer's terminal says
    /// about its exit code and its egress.
    #[test]
    fn a_box_cannot_rewrite_the_reviewers_screen_through_a_receipt() {
        let td = TempDir::new().unwrap();
        let rec = append(
            td.path(),
            RecordInput {
                // Clear the screen, then print a reassuring line over the top.
                cmd: Some("rm -rf /\u{1b}[2J\u{1b}[H  cmd      : ls".into()),
                exit_code: Some(1),
                // Every other field `render` shows, including the one that is
                // *sliced*: `&d[..12]` on a multi-byte digest aborts, which is
                // the same defect a manifest's `base_commit` had.
                source: "tee-shim\u{1b}[1A\u{202e}".into(),
                policy_digest: Some("a日日日日日日".into()),
                browser: Some(BrowserEvidence {
                    verb: Some("click\u{1b}[1A".into()),
                    errors: vec!["boom\u{1b}[31m".into()],
                    ..Default::default()
                }),
                ..input()
            },
            b"line one\n\x1b[2Jline two\n",
        )
        .unwrap();

        let text = render(&rec, &raw_bytes(td.path(), &rec.id).unwrap());
        assert!(!text.contains('\u{1b}'), "no escape may survive: {text:?}");
        assert!(!text.contains('\u{202e}'), "nor a bidi override: {text:?}");
        // The payload is still a payload: its lines are intact.
        assert!(text.contains("line one\n"), "{text:?}");
        assert!(text.contains("line two"), "{text:?}");
        // And the record's own facts are still there to read.
        assert!(text.contains("exit     : 1"), "{text}");
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
        i.cmd =
            Some("curl -H 'Authorization: Bearer ghp_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789'".into());
        let rec = append(td.path(), i, b"").unwrap();
        assert!(!rec
            .cmd
            .unwrap()
            .contains("ghp_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789"));
    }

    #[test]
    fn oversized_payload_is_capped_and_says_so() {
        let td = TempDir::new().unwrap();
        let big = vec![b'x'; RAW_CAP + 1024];
        let rec = append(td.path(), input(), &big).unwrap();
        assert!(rec.raw_truncated);
        assert_eq!(rec.raw_size, RAW_CAP as u64);
    }

    /// The advisory case: a box prints a credential on a line that also carries
    /// a placeholder word. `scan_text` skips that line via its stoplist, so
    /// gating redaction on "did the scanner find anything" stored the token
    /// verbatim in `<env>/raw/<id>.raw` and in the published evidence.
    #[test]
    fn a_stoplist_word_cannot_smuggle_a_credential_into_the_raw_store() {
        let td = TempDir::new().unwrap();
        let token = format!("ghp_{}", "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8");
        for line in [
            format!("example config: {token}"),
            format!("dummy value {token}"),
            format!("# fake, do not use: {token}"),
            format!("${{PLACEHOLDER}} {token}"),
        ] {
            let rec = append(td.path(), input(), line.as_bytes()).unwrap();
            let stored = String::from_utf8(raw_bytes(td.path(), &rec.id).unwrap()).unwrap();
            assert!(!stored.contains(&token), "stored verbatim: {stored}");
        }
    }

    /// One stray non-UTF-8 byte used to skip redaction for the whole payload,
    /// so a box could smuggle a credential past the scrub by interleaving one.
    #[test]
    fn an_invalid_byte_does_not_disable_redaction_for_the_payload() {
        let td = TempDir::new().unwrap();
        let token = format!("ghp_{}", "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8");
        let mut raw: Vec<u8> = Vec::new();
        raw.extend_from_slice(b"before\xff\xfe binary\n");
        raw.extend_from_slice(format!("token={token}\n").as_bytes());
        let rec = append(td.path(), input(), &raw).unwrap();
        let stored = raw_bytes(td.path(), &rec.id).unwrap();
        assert!(
            !String::from_utf8_lossy(&stored).contains(&token),
            "credential survived behind an invalid byte"
        );
        // The undecodable bytes are still carried through untouched.
        assert!(stored.windows(2).any(|w| w == [0xff, 0xfe]));
    }

    /// Redaction is unconditional now, so a payload with nothing to scrub must
    /// still round-trip byte for byte.
    #[test]
    fn a_clean_payload_is_stored_unchanged() {
        let td = TempDir::new().unwrap();
        let body = b"line one\r\nline two\n";
        let rec = append(td.path(), input(), body).unwrap();
        assert_eq!(raw_bytes(td.path(), &rec.id).unwrap(), body);
        assert!(rec.redactions.is_empty());
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
    /// The block must survive a round trip through the log, and its exemplars
    /// must be scrubbed on the way in: a path or an `argv[1]` a box chose can
    /// carry a credential exactly as readily as a command line can.
    #[test]
    fn a_runtime_block_is_stored_redacted_and_read_back() {
        let dir = TempDir::new().unwrap();
        let mut input = input();
        input.runtime = Some(h5i_bpf::RuntimeEvidence {
            lane: h5i_bpf::evidence::LANE.into(),
            scope: "pidtree".into(),
            coverage: h5i_bpf::Coverage::Full,
            coverage_reason: None,
            events_seen: 12,
            events_lost: 0,
            events_filtered: 900,
            detections: vec![h5i_bpf::Detection {
                rule: "secret.read".into(),
                family: "secret".into(),
                severity: h5i_bpf::Severity::Alert,
                title: "opened a credential file".into(),
                count: 2,
                first_ns: 1,
                last_ns: 9,
                examples: vec![format!(
                    "open /h/.ssh/k --token=sk-ant-api03-{}",
                    "S3CRETVALUE".repeat(9)
                )],
                examples_truncated: false,
            }],
            unavailable: None,
        });
        let rec = append(dir.path(), input, b"out").unwrap();
        let stored = &rec.runtime.as_ref().unwrap().detections[0].examples[0];
        assert!(!stored.contains("S3CRETVALUE"), "{stored}");

        let back = find(dir.path(), &rec.id).unwrap();
        let rt = back.runtime.expect("the block must survive the log");
        assert_eq!(rt.events_seen, 12);
        assert_eq!(rt.detections.len(), 1);
        assert!(rt.observed());
    }

    /// A record written before this field existed must still read.
    #[test]
    fn a_record_without_a_runtime_block_still_reads() {
        let dir = TempDir::new().unwrap();
        let rec = append(dir.path(), input(), b"out").unwrap();
        assert!(rec.runtime.is_none());
        let text = std::fs::read_to_string(dir.path().join(LOG_FILE)).unwrap();
        assert!(!text.contains("runtime"), "{text}");
        assert!(find(dir.path(), &rec.id).unwrap().runtime.is_none());
    }

    /// Rendering must never let a box's exemplar rewrite the lines above it.
    #[test]
    fn rendering_a_runtime_block_neutralises_control_sequences() {
        let mut rec = append(TempDir::new().unwrap().path(), input(), b"").unwrap();
        rec.runtime = Some(h5i_bpf::RuntimeEvidence {
            lane: h5i_bpf::evidence::LANE.into(),
            scope: "pidtree".into(),
            coverage: h5i_bpf::Coverage::Full,
            coverage_reason: None,
            events_seen: 1,
            events_lost: 0,
            events_filtered: 0,
            detections: vec![h5i_bpf::Detection {
                rule: "kernel.bpf".into(),
                family: "kernel".into(),
                severity: h5i_bpf::Severity::Alert,
                title: "called bpf(2)".into(),
                count: 1,
                first_ns: 1,
                last_ns: 1,
                examples: vec!["bpf(cmd=5)\x1b[2J\x1b[Hexit     : 0".into()],
                examples_truncated: false,
            }],
            unavailable: None,
        });
        let text = render(&rec, b"");
        assert!(!text.contains('\x1b'), "{text}");
        assert!(text.contains("kernel.bpf"), "{text}");
    }

    /// An unwatched run's block must read as unwatched, never as clean.
    #[test]
    fn an_unavailable_runtime_block_renders_its_reason() {
        let mut rec = append(TempDir::new().unwrap().path(), input(), b"").unwrap();
        rec.runtime = Some(h5i_bpf::RuntimeEvidence::unavailable("missing CAP_BPF"));
        let text = render(&rec, b"");
        assert!(text.contains("not observed"), "{text}");
        assert!(text.contains("CAP_BPF"), "{text}");
    }

}
