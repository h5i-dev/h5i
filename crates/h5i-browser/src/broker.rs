//! The broker, as a named set of operations rather than a struct.

use std::sync::Arc;

use url::Url;

use crate::budget::{Limits, Spent};
use crate::cors::{Credentials, Mode};
use crate::receipt::{Initiator, RequestRecord};
use crate::secrets::Resolved;
use crate::wsclient::Event;

pub use crate::net::FetchOutcome;

/// One request, with everything the broker needs to decide about it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Fetch {
    pub url: Url,
    pub initiator: Initiator,
    pub method: String,
    /// Empty for a GET. Carried out of band on the wire, never in the JSON.
    #[serde(skip)]
    pub body: Vec<u8>,
    pub content_type: Option<String>,
    /// Headers the caller is setting. Everything is carried, `host`, `cookie`
    /// and `authorization` included, except the three the client computes
    /// (`content-length`, `transfer-encoding`, `connection`), which are refused
    /// and named in the receipt.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<(String, String)>,
    /// The document that asked, when one did: the loopback rule depends on it
    /// ([`crate::policy::Policy::check_from`]).
    pub document: Option<Url>,
    /// How many redirect hops to follow. `None` is the policy's limit; `Some(0)`
    /// hands back the first `Location` unfollowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_redirects: Option<usize>,
    /// Set when a *page* asked, which is what subjects the answer to the
    /// same-origin policy. `None` is the agent exercising its own authority
    /// over a URL it named, which is a different question. See [`crate::cors`].
    pub cors: Option<CorsAsk>,
    /// Add no `Cookie` from the jar. Set on a replay of a stored request, so
    /// `--unset header.cookie` really sends it without one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub without_jar: bool,
}

impl Fetch {
    /// A URL the agent named. No document, so the loopback rule treats this as
    /// an instruction rather than as something a page reached for.
    pub fn get(url: &Url, initiator: Initiator) -> Self {
        Self {
            url: url.clone(),
            initiator,
            method: "GET".to_string(),
            body: Vec::new(),
            content_type: None,
            headers: Vec::new(),
            max_redirects: None,
            document: None,
            cors: None,
            without_jar: false,
        }
    }

    /// The headers the caller is setting, in order.
    pub fn with_headers(mut self, headers: Vec<(String, String)>) -> Self {
        self.headers = headers;
        self
    }

    /// The document that asked for this, for the origin the policy reasons
    /// about.
    pub fn from_document(mut self, document: Option<&Url>) -> Self {
        self.document = document.cloned();
        self
    }

    pub fn with_body(mut self, method: &str, body: &[u8], content_type: Option<&str>) -> Self {
        self.method = method.to_string();
        self.body = body.to_vec();
        self.content_type = content_type.map(str::to_string);
        self
    }
}

/// What a page's own `fetch` brings with it, and nothing an agent's navigation
/// has.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CorsAsk {
    /// The document whose origin is asking.
    pub document: Url,
    pub headers: Vec<(String, String)>,
    pub mode: Mode,
    pub credentials: Credentials,
}

/// The whole request log, in the three numbers a windowed read still needs.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LogSummary {
    /// Every record, both phases, over the life of the session.
    pub total: usize,
    /// Requests the policy refused.
    pub denied: usize,
    /// The highest sequence recorded, over both phases.
    pub highest: Option<u64>,
    /// The cursor to pass back as `since`. It stops below anything still in
    /// flight, so a poll never skips a response half.
    pub cursor: Option<u64>,
}

/// A stored request, sent again with changes. Read, edited and sent in one
/// call, so the stored credentials stay in the broker.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Edited {
    /// The receipt sequence the replay was recorded under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// Each edit, as it was applied.
    pub applied: Vec<crate::edits::Applied>,
    /// One entry per send, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub samples: Vec<Timing>,
    /// The request as sent, for the record. Header *names* only: the values are
    /// in the store, which is the artifact that is allowed to hold them.
    pub sent: Sent,
    pub outcome: FetchOutcome,
}

/// A request handed to a session that never made it: another session's
/// message, sent under this session's identity.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Given {
    pub method: String,
    pub url: Url,
    /// Headers to carry. What a caller composes, and never a credential unless
    /// the caller put one here on purpose: see `h5i browser resend --as`.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub body: Vec<u8>,
}

/// A request written directly to the socket, without URL parsing.
pub struct RawRequest {
    /// The authority used for policy checks and dialing. Its path is ignored.
    pub url: Url,
    /// The method and target recorded in receipts. `wire` contains the sent values.
    pub method: String,
    pub target: String,
    /// The complete request bytes, written unchanged.
    pub wire: Vec<u8>,
    /// The body offset within `wire`.
    pub body_at: usize,
    /// Parsed headers for capture and receipts.
    pub headers: Vec<(String, String)>,
    /// Deliberately broken framing invariants recorded in the receipt.
    pub broke: Vec<String>,
}

/// Why a replay did not happen: a code to branch on, and a sentence.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SendError {
    /// `no-capture`, `no-such-request`, `unreplayable-body` or `bad-edit`.
    pub code: String,
    pub message: String,
}

impl SendError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

/// How many times to send it, and whether at once: in a row for a timing
/// test, together for a race.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Sends {
    /// How many. Always at least one.
    pub count: u32,
    /// Stop at the first redirect instead of following it.
    ///
    /// A browser follows; a test often wants the 302 itself. See
    /// [`Fetch::max_redirects`].
    #[serde(default)]
    pub no_follow: bool,
    /// Released together, from threads that meet at a barrier. A burst, not a
    /// single-packet attack.
    pub together: bool,
    /// A request-target to write unchanged. `None` uses the normal sender.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_target: Option<String>,
    /// A complete request to write unchanged. `None` uses the normal sender.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_request: Option<Vec<u8>>,
    /// Send once per target value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub each: Option<Each>,
    /// Preserve header-name casing via the raw sender.
    #[serde(default)]
    pub raw_headers: bool,
    /// Sends per second, at most. `None` sends as fast as the wire allows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<f64>,
}

impl Default for Sends {
    fn default() -> Self {
        Self {
            count: 1,
            together: false,
            no_follow: false,
            raw_target: None,
            raw_headers: false,
            each: None,
            raw_request: None,
            rate: None,
        }
    }
}

impl Sends {
    pub fn once() -> Self {
        Self::default()
    }
}

/// One send's timing and result.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Timing {
    pub seq: Option<u64>,
    pub status: Option<u16>,
    /// Enumeration value, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// To the response's headers: the server's decision.
    pub ttfb_ms: u64,
    /// To the body in hand: the decision plus the transfer.
    pub total_ms: u64,
    pub bytes: u64,
    /// Bounded body preview; capture retains the full body.
    #[serde(default)]
    pub body_preview: String,
    /// Whether [`Self::body_preview`] omits trailing bytes.
    #[serde(default)]
    pub body_truncated: bool,
}

pub const BODY_PREVIEW_BYTES: usize = 4096;

pub fn body_preview(body: &[u8]) -> String {
    String::from_utf8_lossy(&body[..body.len().min(BODY_PREVIEW_BYTES)]).into_owned()
}

/// One send of a walk: the edits that make it different, and what to call it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Step {
    /// What names this send in the samples. The value, when one target is
    /// walked; the whole combination, when several are.
    pub label: String,
    /// `target=value` edits, applied after the shared ones.
    pub set: Vec<String>,
}

/// A walk: one send per step, in order.
///
/// One shape for one target and for several, because the sender's job is the
/// same either way and two paths through it would drift.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Each {
    pub steps: Vec<Step>,
}

impl Each {
    /// The one-target form: `query.id` over a file of values.
    pub fn over(target: &str, values: Vec<String>) -> Self {
        Self {
            steps: values
                .into_iter()
                .map(|value| Step {
                    set: vec![format!("{target}={value}")],
                    label: value,
                })
                .collect(),
        }
    }
}

/// What went out, in the parts that are safe to hand back.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Sent {
    pub method: String,
    pub url: String,
    pub header_names: Vec<String>,
    pub body_bytes: u64,
}

/// What has been spent, and what was allowed, read at one moment.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Allowance {
    pub spent: Spent,
    pub limits: Limits,
}

/// A long-lived connection, from the side that does not own it.
pub trait Channel: Send + Sync {
    /// Send a text frame. `Err` for a stream that cannot be written to, which
    /// is what a server-sent event stream is.
    fn send(&self, text: &str) -> Result<(), String>;
    /// Everything that has arrived since the last drain.
    fn drain(&self) -> Vec<Event>;
    /// Stop. Idempotent, because the page may close a socket the peer already
    /// did.
    fn close(&self);
}

/// The one way bytes enter this engine.
pub trait Broker: Send + Sync {
    /// Check policy, record the decision, use the wire. In that order, and
    /// with no second path. Every other fetch entry point on this trait is a
    /// convenience over this one.
    fn send(&self, fetch: &Fetch) -> FetchOutcome;

    /// The same send, with `while_waiting` run while the answer is in flight.
    fn send_while(&self, fetch: &Fetch, while_waiting: &mut dyn FnMut()) -> FetchOutcome {
        let _ = while_waiting;
        self.send(fetch)
    }

    /// Send a stored request again, with the caller's changes. A policy
    /// refusal is an outcome with a receipt, not an `Err`.
    fn send_edited(
        &self,
        _from: u64,
        _edits: &[crate::edits::Edit],
        _create: bool,
        _plan: Sends,
    ) -> Result<Edited, SendError> {
        Err(SendError::new(
            "no-capture",
            "this session keeps no message store, so it has no stored request to send \
             again. A session `h5i browser open` or `proxy` started keeps every message",
        ))
    }

    /// Send a request this session did not make, under this session's
    /// cookies and policy.
    fn send_given(
        &self,
        _request: Given,
        _edits: &[crate::edits::Edit],
        _create: bool,
        _plan: Sends,
    ) -> Result<Edited, SendError> {
        Err(SendError::new(
            "not-supported",
            "this broker cannot send a composed request",
        ))
    }

    /// Send exact request bytes after the usual policy, budget, and receipt checks.
    fn send_raw(&self, _request: &RawRequest) -> FetchOutcome {
        FetchOutcome::failed(
            _request.url.clone(),
            "this broker cannot send a raw request".to_string(),
        )
    }

    /// The message store's health, including anything it dropped.
    fn capture(&self) -> Option<crate::capture::Health> {
        None
    }

    /// The requests this broker has decided about, in the order it recorded
    /// them.
    fn records(&self) -> Vec<RequestRecord>;

    /// The highest sequence recorded so far, or `None` on an empty log. The
    /// mark a verb takes before it runs.
    fn high_water(&self) -> Option<u64> {
        self.records().iter().map(|r| r.seq).max()
    }

    /// The records after `mark`.
    fn records_since(&self, mark: Option<u64>) -> Vec<RequestRecord> {
        self.records()
            .into_iter()
            .filter(|r| mark.is_none_or(|floor| r.seq > floor))
            .collect()
    }

    /// The whole log in three numbers, for a windowed read to report.
    fn log_summary(&self) -> LogSummary {
        let records = self.records();
        // The lowest sequence whose pair is not finished. Everything below it
        // is complete and safe to move a cursor past.
        let mut answered: std::collections::HashSet<u64> = std::collections::HashSet::new();
        for record in &records {
            if record.phase == crate::receipt::Phase::Response {
                answered.insert(record.seq);
            }
        }
        let in_flight = records
            .iter()
            .filter(|r| r.phase == crate::receipt::Phase::Request)
            .map(|r| r.seq)
            .filter(|seq| !answered.contains(seq))
            .min();
        let highest = records.iter().map(|r| r.seq).max();
        let cursor = match in_flight {
            None => highest,
            // Stop below the first request still waiting for its answer.
            Some(0) => None,
            Some(seq) => Some(seq - 1),
        };
        LogSummary {
            cursor,
            total: records.len(),
            denied: records
                .iter()
                .filter(|r| r.phase == crate::receipt::Phase::Request && !r.allowed)
                .count(),
            // The highest sequence, not the last appended: numbers are taken
            // before the append and a socket's reader thread appends
            // concurrently with the page's own fetches, so append order and
            // sequence order can differ.
            highest,
        }
    }

    /// Sequence numbers recorded after `mark`, deduplicated and in order.
    /// The receipts after `mark` that a verb caused: [`Self::since`], less
    /// whatever arrived beside it rather than because of it.
    fn caused_since(&self, mark: Option<u64>) -> Vec<u64> {
        self.since(mark)
    }

    fn since(&self, mark: Option<u64>) -> Vec<u64> {
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

    /// What this page has spent, and what it may.
    fn budget(&self) -> Allowance;

    /// A navigation is starting, so the page's allowance starts again. See
    /// [`crate::budget`] for why the ceiling bounds a page rather than a
    /// session.
    fn reset_budget(&self);

    /// How many cookies the session holds. A count, never a value.
    fn cookie_count(&self) -> usize;

    /// What `document.cookie` may see: the non-`HttpOnly` subset for this URL.
    fn document_cookie(&self, url: &Url) -> String;

    /// Store a cookie script set, subject to the same rules the wire path
    /// applies, and to one more, since script may not set `HttpOnly`.
    /// Returns how many were stored.
    fn store_cookie(&self, url: &Url, header: &str) -> usize;

    /// Leaving an origin drops every other origin's cookies. `true` when
    /// something was dropped, which the page reports as a note.
    fn keep_only_origin(&self, origin: &Url) -> bool;

    /// Authorise, record, and dial a WebSocket.
    fn open_socket(&self, url: &Url, document: Option<&Url>) -> Result<Arc<dyn Channel>, String>;

    /// Authorise, record, and make one gRPC call over HTTP/2. The request body
    /// arrives framed and the response comes back framed; the protobuf meaning
    /// is the plugin's, out of the engine.
    fn grpc_call(&self, ask: crate::net::GrpcAsk) -> crate::net::GrpcCallOutcome;

    /// The same, for an event stream.
    fn open_event_stream(
        &self,
        url: &Url,
        document: Option<&Url>,
    ) -> Result<Arc<dyn Channel>, String>;

    /// The credentials this session may substitute, by name.
    fn secret_names(&self) -> Vec<String>;

    /// Resolve `$H5I_SECRET_*` placeholders.
    fn substitute(&self, text: &str) -> Resolved;

    /// Put the placeholder back wherever a value appears in outgoing text.
    fn redact(&self, text: &str) -> String;

    /// The same, for every string in one reply.
    fn redact_all(&self, texts: &[String]) -> Vec<String> {
        texts.iter().map(|text| self.redact(text)).collect()
    }

    /// Whether this broker holds anything redaction would replace. Defaults to
    /// `true`: a wrong `false` is a leaked credential.
    fn has_redactions(&self) -> bool {
        true
    }

    // ── convenience over `send` ─────────────────────────────────────────────

    /// Fetch a URL the agent named.
    fn fetch(&self, url: &Url, initiator: Initiator) -> FetchOutcome {
        self.send(&Fetch::get(url, initiator))
    }

    /// The same fetch, with speculative work run while it is in flight. See
    /// [`Self::send_while`] for what may go in the closure.
    fn fetch_while(
        &self,
        url: &Url,
        initiator: Initiator,
        while_waiting: &mut dyn FnMut(),
    ) -> FetchOutcome {
        self.send_while(&Fetch::get(url, initiator), while_waiting)
    }

    /// Fetch something a *document* asked for.
    fn fetch_from(&self, url: &Url, initiator: Initiator, document: Option<&Url>) -> FetchOutcome {
        self.send(&Fetch::get(url, initiator).from_document(document))
    }

    /// Send a request that may carry a body. What a form submission needs.
    fn send_from(
        &self,
        url: &Url,
        initiator: Initiator,
        method: &str,
        body: &[u8],
        content_type: Option<&str>,
        document: Option<&Url>,
    ) -> FetchOutcome {
        self.send(
            &Fetch::get(url, initiator)
                .with_body(method, body, content_type)
                .from_document(document),
        )
    }

    /// The same send, subject to the same-origin policy: a *page* exercising an
    /// authority it has to be granted.
    #[allow(clippy::too_many_arguments)]
    fn send_script(
        &self,
        url: &Url,
        method: &str,
        body: &[u8],
        content_type: Option<&str>,
        document: &Url,
        headers: &[(String, String)],
        mode: Mode,
        credentials: Credentials,
    ) -> FetchOutcome {
        let mut fetch = Fetch::get(url, Initiator::Subresource)
            .with_body(method, body, content_type)
            .from_document(Some(document));
        fetch.cors = Some(CorsAsk {
            document: document.clone(),
            headers: headers.to_vec(),
            mode,
            credentials,
        });
        self.send(&fetch)
    }
}
