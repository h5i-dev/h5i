//! The request log, and the reason it is not merely a log.
//!
//! A record is written *before* the wire and again after it. The first write
//! is what makes the fail-closed claim true rather than aspirational: if the
//! sink refuses the decision record, [`crate::net::Broker`] refuses the fetch,
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
    /// A hop the server asked for via `Location`.
    Redirect,
}

/// One line of the request log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestRecord {
    pub seq: u64,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl RequestRecord {
    /// The decision record, written before the wire.
    pub fn request(seq: u64, initiator: Initiator, method: &str, url: &str) -> Self {
        Self {
            seq,
            phase: Phase::Request,
            initiator,
            method: method.to_string(),
            url: url.to_string(),
            allowed: true,
            denied_reason: None,
            status: None,
            bytes: None,
            duration_ms: None,
            error: None,
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
                format!("{status:>6} {} {} ({bytes} bytes, {ms}ms)", self.method, self.url)
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
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| H5iError::with_path(e, parent))?;
            }
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
