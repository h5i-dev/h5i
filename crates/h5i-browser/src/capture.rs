//! The message store: the bytes a receipt deliberately does not keep.
//!
//! The request log in [`crate::receipt`] records decisions, and it records them
//! in a form that is safe to paste into a bug report: it counts cookies rather
//! than naming them, and it holds no body at all. That is the right shape for an
//! audit trail and the wrong shape for a workbench, where the question is what
//! exactly was sent and what exactly came back, `Authorization` header included.
//!
//! So there are two artifacts rather than one, and the difference is deliberate.
//! The receipt is the account: always on, append-only, fail-closed, exportable.
//! This is the evidence: off unless a session was opened with a place to put it,
//! owner-only, bounded, and never included in an export unless someone names it.
//! Nothing here is ever written into a receipt, and no field of a receipt is
//! ever widened to hold a value that lives here.
//!
//! Two rules follow from being evidence rather than account.
//!
//! First, this store is best-effort and a failure here never fails a fetch. The
//! receipt's fail-closed rule exists because a request nobody recorded is a
//! request nobody can audit; a body nobody stored is a lesser loss than a page
//! that will not load because a disk filled, and the receipt still says the
//! request happened. Errors are counted and reported ([`Capture::errors`])
//! rather than raised.
//!
//! Second, an absent body is always a *named* state. A body that was too large,
//! that was a font nobody will read, or that arrived after the store filled up
//! is recorded as [`Body::Skipped`] with the reason, and a body kept in part is
//! [`Body::Stored`] with `truncated`. An empty string would say "the server sent
//! nothing", which is a different fact and one an agent would act on.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use h5i_error::H5iError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The largest body kept whole, per message.
///
/// Eight mebibytes admits any HTML document, any JSON API answer and most
/// bundles, and refuses the video someone left on an endpoint. Past it the head
/// is kept and the record says so, because the first bytes of an oversized
/// response are usually the whole answer to "what is this".
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// What one session's bodies may occupy, across every message.
///
/// A per-message cap alone does not bound a session: a loop over ten thousand
/// pages is ten thousand small bodies. This is the number that stops a workbench
/// from filling the disk a box lives on.
pub const MAX_STORE_BYTES: u64 = 512 * 1024 * 1024;

/// Media types a workbench never reads.
///
/// Fonts and audio and video are bytes an agent cannot ask a question about, and
/// they are the largest things a page loads. Images are deliberately *not* here:
/// an uploaded PNG that is really a shell is the whole point of a file-upload
/// problem, and a store that threw it away would be useless exactly when it
/// mattered.
fn is_skipped_type(content_type: Option<&str>) -> bool {
    let Some(kind) = content_type else {
        return false;
    };
    let kind = kind.trim().to_ascii_lowercase();
    kind.starts_with("font/")
        || kind.starts_with("audio/")
        || kind.starts_with("video/")
        || kind.starts_with("application/font")
}

/// Why a body is not in the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Skip {
    /// A media type from the skip list.
    Media,
    /// The session's store is full. Not "the body was too big": that one is
    /// truncated and kept.
    StoreFull,
    /// The engine never read it. A redirect hop's body is not read, and neither
    /// is the body of a response the same-origin policy refused, so there is
    /// nothing to store and that is a fact about the fetch rather than about
    /// this store.
    NotRead,
    /// The store itself failed on this one. Kept as a state rather than dropped
    /// so a gap in the evidence is visible in the evidence.
    Failed,
}

/// Where a message's body went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Body {
    /// There was no body. A GET's request body, or a 204.
    Empty,
    /// In the store, under `sha256`, which is also its file name.
    Stored {
        sha256: String,
        /// How many bytes are in the store.
        bytes: u64,
        /// How many bytes there were, when that is a larger number.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        of_bytes: Option<u64>,
        /// Set when only the head was kept.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        truncated: bool,
    },
    /// Not stored, and why.
    Skipped {
        reason: Skip,
        /// How large it was, when the engine had it in hand to measure.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes: Option<u64>,
    },
}

/// One request, as it went to the wire.
///
/// The headers are the ones the client actually built, after the engine added
/// its own and the jar added the cookie, which is the only set worth storing: a
/// record of what a caller asked for is a record of an intention, and a replay
/// that reproduces an intention rather than a request reproduces nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredRequest {
    pub seq: u64,
    pub at: String,
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Body,
}

/// One response, as the engine received it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredResponse {
    pub seq: u64,
    pub at: String,
    /// The URL this hop was sent to, so a stored redirect chain reads without
    /// having to be joined against the request file.
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    pub headers: Vec<(String, String)>,
    /// How the body was encoded on the wire, when it was. The stored body is
    /// always the decoded one, which is what the page received and what a diff
    /// or a match should read; this says what it arrived as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_encoding: Option<String>,
    /// What crossed the wire, when that is a different number from the body's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire_bytes: Option<u64>,
    pub body: Body,
}

/// One response, as the engine has it in hand.
///
/// A struct rather than seven arguments, for the reason [`crate::broker::Fetch`]
/// is one: what gets written down is exactly this, named in one place, and a
/// caller cannot pass the status of one hop with the headers of another by
/// getting the order wrong.
#[derive(Debug, Clone)]
pub struct Response<'a> {
    pub seq: u64,
    /// The URL this hop was sent to.
    pub url: &'a str,
    pub status: Option<u16>,
    pub headers: Vec<(String, String)>,
    /// How the body was encoded on the wire, when it was.
    pub content_encoding: Option<String>,
    /// What crossed the wire, when that is a different number.
    pub wire_bytes: Option<u64>,
    pub body: Received<'a>,
}

/// What the engine has to offer the store for a response body.
#[derive(Debug, Clone, Copy)]
pub enum Received<'a> {
    /// The decoded body.
    Bytes(&'a [u8]),
    /// There was a response, and its body was never read. See [`Skip::NotRead`].
    NotRead,
}

/// The engine's clock, matching the receipt's to the microsecond.
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

/// What a store has done, in the three numbers that say whether to trust it.
///
/// Counts, in the receipt's spirit: how much is here, and whether any of it is
/// missing. `errors` is the one that matters. A store that failed to write a
/// message is a store with a hole in it, and a hole nobody reports is worse
/// than no store at all, because an agent reading the messages it *does* hold
/// would conclude the missing request never happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Health {
    /// Message files written, both phases.
    pub messages: u64,
    /// Bytes of body held.
    pub bytes: u64,
    /// Messages this store could not write.
    pub errors: u64,
}

/// A session's stored messages.
pub struct Capture {
    dir: PathBuf,
    bodies: PathBuf,
    /// Bytes this session has put in `bodies`, which is what
    /// [`MAX_STORE_BYTES`] bounds.
    used: AtomicU64,
    /// Hashes already on disk, so the fiftieth identical response costs a hash
    /// and no bytes. Also what keeps `used` honest: a body stored twice is
    /// counted once, because it occupies one file.
    seen: Mutex<HashSet<String>>,
    /// How many messages this store failed to write. Reported rather than
    /// raised; see the module documentation.
    errors: AtomicU64,
    /// How many it wrote, so a reader can tell an empty store from a broken one.
    messages: AtomicU64,
}

impl Capture {
    /// Open a store at `dir`, creating it.
    ///
    /// h5i chooses the path and the engine writes the bytes, the same division
    /// the cookie jar and the request log already follow: it is what keeps the
    /// location of a session's evidence a decision h5i makes rather than one the
    /// engine's caller can point anywhere.
    pub fn open(dir: &Path) -> Result<Self, H5iError> {
        let bodies = dir.join("bodies");
        std::fs::create_dir_all(&bodies).map_err(|e| H5iError::with_path(e, &bodies))?;
        owner_only_dir(dir);
        owner_only_dir(&bodies);
        // Reopening a store, which a `--restore` does, inherits what is there
        // rather than starting the session's allowance again. The alternative is
        // a store that grows without bound across restarts while reporting that
        // it is within its cap.
        let mut used = 0u64;
        let mut seen = HashSet::new();
        if let Ok(entries) = std::fs::read_dir(&bodies) {
            for entry in entries.flatten() {
                let Ok(meta) = entry.metadata() else { continue };
                if !meta.is_file() {
                    continue;
                }
                used = used.saturating_add(meta.len());
                if let Some(name) = entry.file_name().to_str() {
                    seen.insert(name.to_string());
                }
            }
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            bodies,
            used: AtomicU64::new(used),
            seen: Mutex::new(seen),
            errors: AtomicU64::new(0),
            messages: AtomicU64::new(0),
        })
    }

    /// Where this store lives.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// How many messages could not be written.
    pub fn errors(&self) -> u64 {
        self.errors.load(Ordering::Relaxed)
    }

    /// How many bytes of body this store holds.
    pub fn used(&self) -> u64 {
        self.used.load(Ordering::Relaxed)
    }

    /// What this store has done, for whoever is asking whether to trust it.
    pub fn health(&self) -> Health {
        Health {
            messages: self.messages.load(Ordering::Relaxed),
            bytes: self.used(),
            errors: self.errors(),
        }
    }

    /// Store a request, as built, just before it goes to the wire.
    pub fn request(
        &self,
        seq: u64,
        method: &str,
        url: &str,
        headers: Vec<(String, String)>,
        body: &[u8],
        content_type: Option<&str>,
    ) {
        let stored = StoredRequest {
            seq,
            at: now_rfc3339(),
            method: method.to_string(),
            url: url.to_string(),
            headers,
            body: self.store_body(Received::Bytes(body), content_type),
        };
        self.write(seq, "request", &stored);
    }

    /// Store a response, as received.
    pub fn response(&self, response: Response<'_>) {
        let content_type = response
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            .map(|(_, value)| value.clone());
        let stored = StoredResponse {
            seq: response.seq,
            at: now_rfc3339(),
            url: response.url.to_string(),
            status: response.status,
            headers: response.headers,
            content_encoding: response.content_encoding,
            wire_bytes: response.wire_bytes,
            body: self.store_body(response.body, content_type.as_deref()),
        };
        self.write(stored.seq, "response", &stored);
    }

    /// Read one stored request back.
    pub fn read_request(&self, seq: u64) -> Result<StoredRequest, H5iError> {
        self.read(seq, "request")
    }

    /// Read one stored response back.
    pub fn read_response(&self, seq: u64) -> Result<StoredResponse, H5iError> {
        self.read(seq, "response")
    }

    /// Read a body out of the store, by the hash a [`Body::Stored`] names.
    pub fn read_body(&self, sha256: &str) -> Result<Vec<u8>, H5iError> {
        let path = self.body_path(sha256)?;
        std::fs::read(&path).map_err(|e| H5iError::with_path(e, &path))
    }

    /// The file a hash names, refusing anything that is not one.
    ///
    /// The hash reaches this from a JSON file, and a JSON file in a session
    /// directory is not a trusted input: a `..` in that field would otherwise
    /// name a path outside the store. Hex and length are the whole check because
    /// a name that is 64 hex characters cannot traverse anywhere.
    fn body_path(&self, sha256: &str) -> Result<PathBuf, H5iError> {
        if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(H5iError::Internal(format!(
                "{sha256:?} is not a body hash, so there is nothing in the store to read"
            )));
        }
        Ok(self.bodies.join(sha256))
    }

    /// Put a body in the store, and say what became of it.
    fn store_body(&self, body: Received<'_>, content_type: Option<&str>) -> Body {
        let bytes = match body {
            Received::NotRead => {
                return Body::Skipped {
                    reason: Skip::NotRead,
                    bytes: None,
                };
            }
            Received::Bytes(bytes) => bytes,
        };
        if bytes.is_empty() {
            return Body::Empty;
        }
        let full = bytes.len() as u64;
        if is_skipped_type(content_type) {
            return Body::Skipped {
                reason: Skip::Media,
                bytes: Some(full),
            };
        }
        let truncated = bytes.len() > MAX_BODY_BYTES;
        let kept = if truncated {
            &bytes[..MAX_BODY_BYTES]
        } else {
            bytes
        };

        let sha256 = hex(&Sha256::digest(kept));
        let path = self.bodies.join(&sha256);

        // Already stored, by this session or by the one that left this
        // directory behind. Content addressing makes that check free, and it is
        // the common case: a loop that replays one request a hundred times
        // usually gets one of two answers back.
        {
            let mut seen = match self.seen.lock() {
                Ok(seen) => seen,
                Err(poisoned) => poisoned.into_inner(),
            };
            if seen.contains(&sha256) {
                return Body::Stored {
                    sha256,
                    bytes: kept.len() as u64,
                    of_bytes: truncated.then_some(full),
                    truncated,
                };
            }
            // The allowance is checked under the same lock that claims the
            // hash, so two threads storing two different bodies at once cannot
            // both decide there is room for the last of it.
            let used = self.used.load(Ordering::Relaxed);
            if used.saturating_add(kept.len() as u64) > MAX_STORE_BYTES {
                // Refused rather than evicted. Eviction wants a pin, so that a
                // body a finding rests on is not the one thrown away to make
                // room for a font; until `websec pin` exists there is nothing to
                // consult, and silently dropping the oldest evidence is worse
                // than refusing the newest and saying so.
                return Body::Skipped {
                    reason: Skip::StoreFull,
                    bytes: Some(full),
                };
            }
            if let Err(e) = write_owner_only(&path, kept) {
                self.errors.fetch_add(1, Ordering::Relaxed);
                let _ = e;
                return Body::Skipped {
                    reason: Skip::Failed,
                    bytes: Some(full),
                };
            }
            self.used.fetch_add(kept.len() as u64, Ordering::Relaxed);
            seen.insert(sha256.clone());
        }

        Body::Stored {
            sha256,
            bytes: kept.len() as u64,
            of_bytes: truncated.then_some(full),
            truncated,
        }
    }

    /// Write one message file. Best-effort, and counted when it fails.
    fn write<T: Serialize>(&self, seq: u64, phase: &str, message: &T) {
        let path = self.dir.join(format!("{seq}.{phase}.json"));
        let wrote = serde_json::to_vec(message)
            .map_err(H5iError::from)
            .and_then(|bytes| write_owner_only(&path, &bytes));
        match wrote {
            Ok(()) => self.messages.fetch_add(1, Ordering::Relaxed),
            Err(_) => self.errors.fetch_add(1, Ordering::Relaxed),
        };
    }

    fn read<T: for<'de> Deserialize<'de>>(&self, seq: u64, phase: &str) -> Result<T, H5iError> {
        let path = self.dir.join(format!("{seq}.{phase}.json"));
        let bytes = std::fs::read(&path).map_err(|e| H5iError::with_path(e, &path))?;
        Ok(serde_json::from_slice(&bytes)?)
    }
}

/// Lowercase hex, for a hash that is also a file name.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// Write a file readable only by its owner.
///
/// The same reasoning as the request log's, one step further along. This holds
/// session cookies and `Authorization` headers in full, and a boxed session's
/// directory can be under a `/tmp` the `agent` profile shares with the host.
fn write_owner_only(path: &Path, bytes: &[u8]) -> Result<(), H5iError> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|e| H5iError::with_path(e, path))?;
    file.write_all(bytes).map_err(H5iError::Io)?;
    file.flush().map_err(H5iError::Io)?;
    Ok(())
}

/// Narrow a directory to its owner. Best-effort: a filesystem without modes is
/// not a reason to refuse to capture.
fn owner_only_dir(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Capture) {
        let dir = tempfile::tempdir().expect("tempdir");
        let capture = Capture::open(&dir.path().join("messages")).expect("store opens");
        (dir, capture)
    }

    #[test]
    fn a_request_and_its_response_round_trip() {
        let (_dir, capture) = store();
        capture.request(
            7,
            "POST",
            "https://example.test/login",
            vec![("cookie".to_string(), "session=abc".to_string())],
            b"user=admin",
            Some("application/x-www-form-urlencoded"),
        );
        capture.response(Response {
            seq: 7,
            url: "https://example.test/login",
            status: Some(302),
            headers: vec![("location".to_string(), "/home".to_string())],
            content_encoding: None,
            wire_bytes: None,
            body: Received::Bytes(b"see other"),
        });

        let request = capture.read_request(7).expect("request reads back");
        assert_eq!(request.method, "POST");
        // The store holds what a receipt refuses to: the credential itself.
        assert_eq!(request.headers[0].1, "session=abc");
        let Body::Stored { sha256, bytes, .. } = &request.body else {
            panic!("a body was stored: {:?}", request.body);
        };
        assert_eq!(*bytes, 10);
        assert_eq!(capture.read_body(sha256).expect("body reads"), b"user=admin");

        let response = capture.read_response(7).expect("response reads back");
        assert_eq!(response.status, Some(302));
        assert_eq!(capture.errors(), 0);
    }

    #[test]
    fn an_empty_body_is_empty_and_not_a_stored_nothing() {
        let (_dir, capture) = store();
        capture.request(1, "GET", "https://example.test/", Vec::new(), b"", None);
        assert_eq!(capture.read_request(1).expect("reads").body, Body::Empty);
    }

    #[test]
    fn a_body_never_read_says_so() {
        let (_dir, capture) = store();
        capture.response(Response {
            seq: 2,
            url: "https://example.test/",
            status: Some(301),
            headers: Vec::new(),
            content_encoding: None,
            wire_bytes: None,
            body: Received::NotRead,
        });
        assert_eq!(
            capture.read_response(2).expect("reads").body,
            Body::Skipped {
                reason: Skip::NotRead,
                bytes: None,
            }
        );
    }

    #[test]
    fn media_is_named_rather_than_kept() {
        let (_dir, capture) = store();
        capture.response(Response {
            seq: 3,
            url: "https://example.test/f.woff2",
            status: Some(200),
            headers: vec![("content-type".to_string(), "font/woff2".to_string())],
            content_encoding: None,
            wire_bytes: None,
            body: Received::Bytes(b"not really a font"),
        });
        assert_eq!(
            capture.read_response(3).expect("reads").body,
            Body::Skipped {
                reason: Skip::Media,
                bytes: Some(17),
            }
        );
        assert_eq!(capture.used(), 0);
    }

    #[test]
    fn one_body_stored_twice_occupies_one_file() {
        let (_dir, capture) = store();
        for seq in 0..5 {
            capture.response(Response {
                seq,
                url: "https://example.test/",
                status: Some(200),
                headers: Vec::new(),
                content_encoding: None,
                wire_bytes: None,
                body: Received::Bytes(b"the same answer"),
            });
        }
        assert_eq!(capture.used(), 15);
        let first = capture.read_response(0).expect("reads").body;
        let last = capture.read_response(4).expect("reads").body;
        assert_eq!(first, last);
    }

    #[test]
    fn a_reopened_store_inherits_what_it_already_holds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("messages");
        let capture = Capture::open(&path).expect("opens");
        capture.response(Response {
            seq: 1,
            url: "https://example.test/",
            status: Some(200),
            headers: Vec::new(),
            content_encoding: None,
            wire_bytes: None,
            body: Received::Bytes(b"body"),
        });
        drop(capture);

        let reopened = Capture::open(&path).expect("reopens");
        assert_eq!(reopened.used(), 4, "the earlier session's bytes still count");
    }

    #[test]
    fn a_hash_that_is_not_one_names_nothing() {
        let (_dir, capture) = store();
        let escaped = capture.read_body("../../../etc/passwd");
        assert!(escaped.is_err(), "a body name cannot leave the store");
    }

    #[test]
    fn an_oversized_body_keeps_its_head_and_says_so() {
        let (_dir, capture) = store();
        let big = vec![b'a'; MAX_BODY_BYTES + 100];
        capture.response(Response {
            seq: 9,
            url: "https://example.test/big",
            status: Some(200),
            headers: Vec::new(),
            content_encoding: None,
            wire_bytes: None,
            body: Received::Bytes(&big),
        });
        let Body::Stored {
            bytes,
            of_bytes,
            truncated,
            ..
        } = capture.read_response(9).expect("reads").body
        else {
            panic!("the head was kept");
        };
        assert!(truncated);
        assert_eq!(bytes, MAX_BODY_BYTES as u64);
        assert_eq!(of_bytes, Some(MAX_BODY_BYTES as u64 + 100));
    }
}
