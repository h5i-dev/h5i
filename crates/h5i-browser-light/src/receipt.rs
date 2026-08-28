//! The request log, and the reason it is not merely a log.
//!
//! A record is written *before* the wire and again after it. The first write
//! is what makes the fail-closed claim true rather than aspirational: if the
//! sink refuses the decision record, [`crate::net::LocalBroker`] refuses the fetch,
//! so there is no path from "the engine made a request" to "nobody recorded
//! it". The second write carries the outcome (status, bytes, duration), which
//! is the part a human actually reads.
//!
//! Two phases rather than one record written at the end, because a single
//! trailing record cannot describe a request that hung, crashed the process,
//! or was still in flight when the page was screenshotted. The pair can.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use h5i_error::H5iError;
use serde::{Deserialize, Serialize};

/// Whether this record was written before the wire or after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// The decision: what was asked for, and whether policy permitted it.
    /// Written before any bytes move.
    Request,
    /// The outcome: status, size, duration, or the error that ended it.
    Response,
}

/// Why the engine asked for this URL. A proxy sees only the request; the
/// engine knows whether it was the page the user named, a stylesheet the
/// document pulled in, or a redirect hop, and that distinction is most of
/// what makes the record worth reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Initiator {
    /// The top-level document the caller asked to open.
    Navigation,
    /// A subresource the document referenced (stylesheet, image, font).
    Subresource,
    /// A frame's document, fetched to be flattened into the page (§B21).
    ///
    /// Its own name in the receipt rather than `subresource`, because an
    /// auditor asking "did this page pull in another *document*" is asking a
    /// different question from "did it load its stylesheet" — a frame is the
    /// one subresource whose content is someone else's whole page.
    Frame,
    /// A hop the server asked for via `Location`.
    Redirect,
}

/// The engine's clock, RFC3339 with microseconds.
///
/// Microseconds because a page's subresource fetches land inside the same
/// second, and a log an audit sorts by needs a total order within one verb.
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

/// One line of the request log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestRecord {
    pub seq: u64,
    /// When the engine wrote this row, RFC3339.
    ///
    /// **The engine's own claim**, not an observation. A reader outside the box
    /// has no way to check the box's clock, so this is what orders the engine's
    /// two logs against each other and what a host-side reader labels as a
    /// claim when it puts them beside rows h5i wrote itself.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub at: String,
    pub phase: Phase,
    pub initiator: Initiator,
    pub method: String,
    pub url: String,
    /// `true` when policy permitted the request. A denied request never
    /// reaches the wire, so it has a `Request` record and a `Response` record
    /// describing the refusal, and no bytes between them.
    pub allowed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denied_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// The body's size *after* decoding, which is what the page received.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// What actually crossed the wire, when that is a different number.
    ///
    /// Recorded separately rather than replacing `bytes`, because the two
    /// answer different questions and a log that reported one under the other's
    /// name would be wrong for whichever reader wanted the other. "How much did
    /// this cost the network" and "how much did the page get" diverge by a
    /// factor of three to five once compression is negotiated, and an export
    /// that conflated them would make a compressed fetch look like a smaller
    /// page rather than a cheaper one.
    ///
    /// `None` when the response was not encoded, so an uncompressed request
    /// records one number and not the same number twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire_bytes: Option<u64>,
    /// How the body was encoded on the wire, when it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_encoding: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// How many cookies this request carried. **A count, never a value.** The
    /// log is read by people and shipped in exports, and a credential in a
    /// receipt is a credential in a bug report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookies_sent: Option<usize>,
    /// How many the response stored, after the jar refused what it refuses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookies_stored: Option<usize>,
}

impl RequestRecord {
    /// The decision record, written before the wire.
    pub fn request(seq: u64, initiator: Initiator, method: &str, url: &str) -> Self {
        Self {
            seq,
            at: now_rfc3339(),
            phase: Phase::Request,
            initiator,
            method: method.to_string(),
            url: url.to_string(),
            allowed: true,
            denied_reason: None,
            status: None,
            bytes: None,
            wire_bytes: None,
            content_encoding: None,
            duration_ms: None,
            error: None,
            cookies_sent: None,
            cookies_stored: None,
        }
    }

    pub fn denied(mut self, reason: &str) -> Self {
        self.allowed = false;
        self.denied_reason = Some(reason.to_string());
        self
    }

    /// The outcome record, written after the wire (or instead of it, when the
    /// request was refused).
    pub fn response(&self) -> Self {
        let mut next = self.clone();
        next.phase = Phase::Response;
        // Its own time, not the request's: the gap between them is how long the
        // fetch took, and copying the request's stamp would erase it.
        next.at = now_rfc3339();
        next
    }

    /// Render as the one-line form the CLI prints and a viewer pane shows.
    pub fn render(&self) -> String {
        if !self.allowed {
            let why = self.denied_reason.as_deref().unwrap_or("denied");
            return format!("DENIED {} {} — {why}", self.method, self.url);
        }
        match (self.status, self.error.as_deref()) {
            (_, Some(err)) => format!("ERROR  {} {} — {err}", self.method, self.url),
            (Some(status), None) => {
                let bytes = self.bytes.unwrap_or(0);
                let ms = self.duration_ms.unwrap_or(0);
                // Both numbers when they differ, because "184 KB" and "43 KB
                // on the wire" are two facts a reader wants and one line that
                // showed either alone would be answering the other's question.
                let size = match (self.wire_bytes, self.content_encoding.as_deref()) {
                    (Some(wire), Some(encoding)) => {
                        format!("{bytes} bytes, {wire} on the wire {encoding}")
                    }
                    (None, Some(encoding)) => format!("{bytes} bytes {encoding}"),
                    _ => format!("{bytes} bytes"),
                };
                format!("{status:>6} {} {} ({size}, {ms}ms)", self.method, self.url)
            }
            (None, None) => format!("       {} {}", self.method, self.url),
        }
    }
}

/// Somewhere records are durably written.
///
/// The trait returns a `Result` for exactly one reason: so a failure to record
/// can stop a fetch. An implementation that swallows its errors turns the
/// fail-closed guarantee back into a hope.
pub trait Sink: Send + Sync + 'static {
    fn append(&self, record: &RequestRecord) -> Result<(), H5iError>;
}

/// A JSON-lines file, one record per line.
pub struct JsonlSink {
    file: Mutex<File>,
}

impl JsonlSink {
    pub fn create(path: &Path) -> Result<Self, H5iError> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| H5iError::with_path(e, parent))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| H5iError::with_path(e, path))?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }
}

impl Sink for JsonlSink {
    fn append(&self, record: &RequestRecord) -> Result<(), H5iError> {
        let line = serde_json::to_string(record)?;
        let mut file = self
            .file
            .lock()
            .map_err(|_| H5iError::Internal("receipt sink lock was poisoned".to_string()))?;
        // Flush rather than buffer: a record that is only in our address space
        // has not recorded anything if the process dies mid-request.
        writeln!(file, "{line}").map_err(H5iError::Io)?;
        file.flush().map_err(H5iError::Io)?;
        Ok(())
    }
}

/// A sink that accepts everything and keeps nothing.
///
/// Not the same as having no sink. The broker always has one, and the
/// fail-closed rule is about what happens when a sink *refuses* — this one
/// never does. It is what a session without `--receipts` writes to: the
/// broker's own in-memory log is still kept, and still printed at the end.
pub struct NullSink;

impl Sink for NullSink {
    fn append(&self, _record: &RequestRecord) -> Result<(), H5iError> {
        Ok(())
    }
}

/// An in-memory sink, for tests and for `--dry-run` style inspection.
#[derive(Default)]
pub struct MemorySink {
    records: Mutex<Vec<RequestRecord>>,
}

impl MemorySink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn records(&self) -> Vec<RequestRecord> {
        self.records.lock().map(|r| r.clone()).unwrap_or_default()
    }

    /// The highest sequence number written so far, or `None` on an empty log.
    ///
    /// The mark a verb takes before it runs, so the receipts written while it
    /// ran can be identified afterwards. The highest rather than the count:
    /// numbers are taken before the append, so append order and sequence order
    /// can differ and a count would drift.
    pub fn high_water(&self) -> Option<u64> {
        self.records()
            .iter()
            .map(|r| r.seq)
            .max()
    }

    /// Sequence numbers written after `mark`, deduplicated and in order.
    ///
    /// A request and its response share a sequence number, so the pair collapses
    /// to one entry: what a reader wants is "which fetches", not "how many rows".
    pub fn since(&self, mark: Option<u64>) -> Vec<u64> {
        let mut seen: Vec<u64> = self
            .records()
            .iter()
            .map(|r| r.seq)
            .filter(|seq| mark.is_none_or(|floor| *seq > floor))
            .collect();
        seen.sort_unstable();
        seen.dedup();
        seen
    }

    /// Just the URLs that actually reached the wire, in order.
    pub fn fetched_urls(&self) -> Vec<String> {
        self.records()
            .into_iter()
            .filter(|r| r.phase == Phase::Request && r.allowed)
            .map(|r| r.url)
            .collect()
    }

    pub fn denied_urls(&self) -> Vec<String> {
        self.records()
            .into_iter()
            .filter(|r| r.phase == Phase::Request && !r.allowed)
            .map(|r| r.url)
            .collect()
    }
}

impl Sink for MemorySink {
    fn append(&self, record: &RequestRecord) -> Result<(), H5iError> {
        self.records
            .lock()
            .map_err(|_| H5iError::Internal("receipt sink lock was poisoned".to_string()))?
            .push(record.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn a_request_record_becomes_a_response_record_without_losing_identity() {
        let req = RequestRecord::request(7, Initiator::Navigation, "GET", "https://example.com/");
        let mut resp = req.response();
        resp.status = Some(200);
        resp.bytes = Some(1234);

        assert_eq!(resp.seq, 7, "the pair must be joinable by seq");
        assert_eq!(resp.url, req.url);
        assert_eq!(req.phase, Phase::Request);
        assert_eq!(resp.phase, Phase::Response);
    }

    #[test]
    fn denial_is_carried_on_the_record_not_inferred_from_a_missing_status() {
        let rec = RequestRecord::request(1, Initiator::Subresource, "GET", "https://tracker.test/p")
            .denied("origin `https://tracker.test` is not in the allowlist");
        assert!(!rec.allowed);
        assert!(rec.render().starts_with("DENIED"));
        assert!(rec.render().contains("not in the allowlist"));
    }

    #[test]
    fn jsonl_sink_writes_one_parseable_line_per_record() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("requests.jsonl");
        let sink = JsonlSink::create(&path).expect("sink creates its parent directory");

        let req = RequestRecord::request(1, Initiator::Navigation, "GET", "https://example.com/");
        sink.append(&req).expect("append request");
        let mut resp = req.response();
        resp.status = Some(200);
        sink.append(&resp).expect("append response");

        let mut contents = String::new();
        File::open(&path)
            .expect("open log")
            .read_to_string(&mut contents)
            .expect("read log");

        let lines: Vec<_> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        for line in lines {
            let parsed: RequestRecord = serde_json::from_str(line).expect("each line is a record");
            assert_eq!(parsed.seq, 1);
        }
    }

    #[test]
    fn memory_sink_separates_what_was_fetched_from_what_was_refused() {
        let sink = MemorySink::new();
        sink.append(&RequestRecord::request(
            1,
            Initiator::Navigation,
            "GET",
            "https://example.com/",
        ))
        .unwrap();
        sink.append(
            &RequestRecord::request(2, Initiator::Subresource, "GET", "https://tracker.test/p")
                .denied("nope"),
        )
        .unwrap();

        assert_eq!(sink.fetched_urls(), vec!["https://example.com/"]);
        assert_eq!(sink.denied_urls(), vec!["https://tracker.test/p"]);
    }
}

// ── the agent's own verbs ───────────────────────────────────────────────────

/// One verb an agent asked the resident session for.
///
/// A *separate* record from [`RequestRecord`], and a separate lane, because
/// they answer different questions with different evidence. A request row is
/// what crossed the wire; this is what the agent asked for. Correlating the two
/// — which click caused which fetch — is not attempted here: the link has to be
/// stamped by whoever knows it, and inferring it from adjacency in a file would
/// be inventing evidence.
///
/// Written by the engine, inside the box, so this lane is **box-claimed**. h5i
/// sits on no socket between an agent and this engine, because there is none:
/// the engine *is* the browser. Anything reading these rows should weigh them
/// as the box's own account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionRecord {
    pub seq: u64,
    /// When the engine wrote this row, RFC3339. The engine's own claim, like
    /// [`RequestRecord::at`].
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub at: String,
    /// `request` before the verb runs, `result` after it.
    pub phase: String,
    pub verb: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Receipt sequence numbers written **while this verb ran**.
    ///
    /// The differentiator, and the field a reviewer joins against the request
    /// log's own numbering: not "a request happened somewhere in this session"
    /// but "this click is the verb the page was under when these fetches went
    /// out".
    ///
    /// Deliberately a *window*, and named as one. The engine could only claim
    /// strict causation for the one path that dispatches a script event and
    /// gets a list back; every other verb that moves the page — `navigate`, a
    /// click that follows an href, `submit`, a `wait_for` that lets a pending
    /// load finish — produces fetches it never enumerates. A window covers all
    /// of them, and its one weakness is stated rather than hidden: the page
    /// thread owns the session, but a viewer's own traffic can land inside the
    /// window and be attributed to a verb that did not ask for it.
    ///
    /// The earlier version of this field read the reply's `requests` key, which
    /// the `requests` verb uses for the rows it *returns*. The one verb that
    /// causes nothing was therefore recorded as having caused everything it
    /// read, and the verbs that actually fetch recorded nothing at all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requests: Vec<u64>,
}

/// How a verb went, for [`ActionLog::finish`].
///
/// A struct rather than five more parameters: the two `Option<String>`s next to
/// each other were a call site nobody could read without counting.
#[derive(Debug, Default, Clone)]
pub struct ActionOutcome {
    pub ok: bool,
    pub url: Option<String>,
    pub error: Option<String>,
    /// Receipt sequence numbers this verb caused.
    pub requests: Vec<u64>,
}

/// Where the resident session records what it was asked to do.
pub struct ActionLog {
    file: Mutex<File>,
    seq: AtomicU64,
}

impl ActionLog {
    pub fn create(path: &Path) -> Result<Self, H5iError> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| H5iError::with_path(e, parent))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| H5iError::with_path(e, path))?;
        Ok(Self {
            file: Mutex::new(file),
            seq: AtomicU64::new(0),
        })
    }

    /// Record that a verb is about to run, and return its sequence number.
    ///
    /// Before, not after, and the failure is propagated: **no record, no
    /// action**, the same rule the request log enforces for fetches. Recording
    /// afterwards would make a full disk into an agent that acts invisibly,
    /// which is precisely the silent under-reporting this log exists to end.
    ///
    /// Worth being exact about what that buys, since the lane is box-claimed:
    /// it is a guarantee against *accident* — a bad path, a full disk, a
    /// permission the box does not have. It is not a guarantee against a box
    /// that has decided to lie, and nothing written inside the box could be.
    pub fn begin(&self, verb: &str, target: Option<&str>) -> Result<u64, H5iError> {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        self.write(&ActionRecord {
            seq,
            at: now_rfc3339(),
            phase: "request".to_string(),
            verb: verb.to_string(),
            target: target.map(str::to_string),
            ok: None,
            url: None,
            error: None,
            requests: Vec::new(),
        })?;
        Ok(seq)
    }

    /// Record how it went. Best-effort: the verb has already happened, so
    /// refusing anything now would only hide the outcome of something real.
    pub fn finish(&self, seq: u64, verb: &str, target: Option<&str>, outcome: ActionOutcome) {
        let _ = self.write(&ActionRecord {
            seq,
            at: now_rfc3339(),
            phase: "result".to_string(),
            verb: verb.to_string(),
            target: target.map(str::to_string),
            ok: Some(outcome.ok),
            url: outcome.url,
            error: outcome.error,
            requests: outcome.requests,
        });
    }

    /// An [`ActionLog`] whose every write fails, for the one test that has to
    /// prove "no record, no action" holds at the *verb*, not merely at startup.
    ///
    /// A read-only handle rather than a clever filesystem: unlinking the file
    /// does not break writes through an fd that is already open, which is how
    /// the first attempt at that test passed while proving nothing.
    #[cfg(test)]
    pub(crate) fn unwritable_for_test(path: &Path) -> Result<Self, H5iError> {
        std::fs::write(path, "").map_err(|e| H5iError::with_path(e, path))?;
        let file = OpenOptions::new()
            .read(true)
            .open(path)
            .map_err(|e| H5iError::with_path(e, path))?;
        Ok(Self {
            file: Mutex::new(file),
            seq: AtomicU64::new(0),
        })
    }

    fn write(&self, record: &ActionRecord) -> Result<(), H5iError> {
        let line = serde_json::to_string(record)?;
        let mut file = self
            .file
            .lock()
            .map_err(|_| H5iError::Internal("action log lock was poisoned".to_string()))?;
        writeln!(file, "{line}").map_err(H5iError::Io)?;
        file.flush().map_err(H5iError::Io)?;
        Ok(())
    }
}

#[cfg(test)]
mod action_log_tests {
    use super::*;

    #[test]
    fn a_verb_is_recorded_before_it_runs_and_again_after() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("browser-actions.jsonl");
        let log = ActionLog::create(&path).expect("creates, making the directory");

        let seq = log.begin("click", Some("@e1")).expect("records");
        log.finish(
            seq,
            "click",
            Some("@e1"),
            ActionOutcome {
                ok: true,
                url: Some("https://example.com/".to_string()),
                requests: vec![4, 5],
                ..Default::default()
            },
        );

        let lines: Vec<ActionRecord> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).expect("each line is a record"))
            .collect();

        assert_eq!(lines.len(), 2, "a pair, like the request log");
        assert_eq!(lines[0].phase, "request");
        assert_eq!(lines[0].ok, None, "the first line cannot know the outcome");
        assert_eq!(lines[1].phase, "result");
        assert_eq!(lines[1].ok, Some(true));
        assert_eq!(lines[1].seq, seq, "the pair shares a sequence number");
    }

    #[test]
    fn a_failed_verb_is_recorded_as_fully_as_a_successful_one() {
        // The rows that matter most for a reviewer are the ones where the
        // agent did not get what it asked for.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("a.jsonl");
        let log = ActionLog::create(&path).expect("creates");

        let seq = log.begin("navigate", Some("https://denied.test/")).unwrap();
        log.finish(
            seq,
            "navigate",
            Some("https://denied.test/"),
            ActionOutcome {
                ok: false,
                error: Some("denied by policy".to_string()),
                ..Default::default()
            },
        );

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("denied by policy"), "{text}");
        assert!(text.contains("\"ok\":false"), "{text}");
    }

    #[test]
    fn an_unwritable_log_refuses_rather_than_recording_nothing() {
        // No record, no action — the same rule the request log enforces for
        // fetches. A directory where the file should be is the cheapest way to
        // make the open fail without depending on permissions.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("occupied");
        std::fs::create_dir(&path).unwrap();
        assert!(
            ActionLog::create(&path).is_err(),
            "a session that cannot record must fail at startup"
        );
    }

    #[test]
    fn sequence_numbers_do_not_repeat() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = ActionLog::create(&dir.path().join("a.jsonl")).unwrap();
        let seqs: Vec<u64> = (0..5).map(|_| log.begin("scroll", None).unwrap()).collect();
        assert_eq!(seqs, vec![0, 1, 2, 3, 4]);
    }
}
