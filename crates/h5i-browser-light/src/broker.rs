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
///
/// A struct rather than eight arguments because it crosses a process boundary:
/// what the broker is told is exactly this, written down in one place, and a
/// caller cannot add a field the far side does not know how to police.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Fetch {
    pub url: Url,
    pub initiator: Initiator,
    pub method: String,
    /// Empty for a GET. Carried out of band on the wire, never in the JSON.
    #[serde(skip)]
    pub body: Vec<u8>,
    pub content_type: Option<String>,
    /// The document that asked, when a document did.
    ///
    /// Load-bearing rather than bookkeeping: without it the policy reads every
    /// subresource as the agent naming a URL, and a page from the open web
    /// reaches the box's dev server. See [`crate::policy::Policy::check_from`].
    pub document: Option<Url>,
    /// Set when a *page* asked, which is what subjects the answer to the
    /// same-origin policy. `None` is the agent exercising its own authority
    /// over a URL it named, which is a different question. See [`crate::cors`].
    pub cors: Option<CorsAsk>,
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
            document: None,
            cors: None,
        }
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
    /// The cursor to pass back as `since`.
    pub highest: Option<u64>,
}

/// What a page has spent, and what it was allowed.
///
/// A reading rather than a live handle. `budget()` used to hand back a
/// `&Budget` the caller could poll; across a process boundary there is nothing
/// to borrow, so the answer is a value taken at one moment and named as one.
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

    /// The requests this broker has decided about, in the order it recorded
    /// them.
    ///
    /// The renderer displays these; it does not own them. The authoritative copy
    /// is the broker's own sink and the file it writes, which is the distinction
    /// worth keeping in mind when the renderer prints a table: a compromised
    /// renderer holds the terminal and can print whatever it likes, but it
    /// cannot change what was written.
    fn records(&self) -> Vec<RequestRecord>;

    /// The highest sequence recorded so far, or `None` on an empty log. The
    /// mark a verb takes before it runs.
    fn high_water(&self) -> Option<u64> {
        self.records().iter().map(|r| r.seq).max()
    }

    /// The records after `mark`, which is what a reader polling for what
    /// changed actually wants.
    ///
    /// Separate from [`Self::records`] because this crosses a process boundary:
    /// answering "what happened since sequence 4000" by shipping four thousand
    /// records and throwing them away is the shape the `since` cursor exists to
    /// avoid, and it would grow with the session rather than with the answer.
    fn records_since(&self, mark: Option<u64>) -> Vec<RequestRecord> {
        self.records()
            .into_iter()
            .filter(|r| mark.is_none_or(|floor| r.seq > floor))
            .collect()
    }

    /// The whole log in three numbers.
    ///
    /// The counts a windowed read still has to report: "nothing was refused" is
    /// a claim about the session rather than about the window, and an agent
    /// that only ever asks for windows should still be able to make it. Three
    /// numbers rather than the log they are derived from, for the same reason
    /// [`Self::records_since`] exists.
    fn log_summary(&self) -> LogSummary {
        let records = self.records();
        LogSummary {
            total: records.len(),
            denied: records
                .iter()
                .filter(|r| r.phase == crate::receipt::Phase::Request && !r.allowed)
                .count(),
            // The highest sequence, not the last appended: numbers are taken
            // before the append and a socket's reader thread appends
            // concurrently with the page's own fetches, so append order and
            // sequence order can differ.
            highest: records.iter().map(|r| r.seq).max(),
        }
    }

    /// Sequence numbers recorded after `mark`, deduplicated and in order.
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

    /// The same, for an event stream.
    fn open_event_stream(
        &self,
        url: &Url,
        document: Option<&Url>,
    ) -> Result<Arc<dyn Channel>, String>;

    /// The credentials this session may substitute, by name.
    #[cfg(feature = "identity")]
    fn identity(&self) -> std::sync::Arc<crate::identity::Identity>;

    fn secret_names(&self) -> Vec<String>;

    /// Resolve `$H5I_SECRET_*` placeholders on the way into a field.
    ///
    /// The limit is worth stating where the operation is declared, because it is
    /// easy to oversell: substitution happens on the way *into* the page, so the
    /// renderer receives the value for the credential that was actually used.
    /// What the split protects is every credential that was not.
    fn substitute(&self, text: &str) -> Resolved;

    /// Put the placeholder back wherever a value appears in outgoing text.
    fn redact(&self, text: &str) -> String;

    /// The same, for every string in one reply.
    ///
    /// A batch rather than a loop over [`Self::redact`], because a reply is a
    /// tree with hundreds of strings in it and this crosses a process boundary:
    /// one round trip per string would make redaction the most expensive thing
    /// the control channel does. The default is the loop, for the
    /// implementation where a round trip is a function call.
    fn redact_all(&self, texts: &[String]) -> Vec<String> {
        texts.iter().map(|text| self.redact(text)).collect()
    }

    /// Whether this broker holds anything redaction would replace.
    ///
    /// So a caller can skip the traversal rather than walk a reply, copy every
    /// string out of it, and rebuild it unchanged. A snapshot reply is tens of
    /// kilobytes across thousands of fields, and a session with no credentials
    /// in it is the common case.
    ///
    /// Defaults to `true`, which is the safe answer: an implementation that has
    /// not thought about this gets the full pass rather than a silent skip. A
    /// wrong `false` here is a leaked credential, so it is only ever returned
    /// by a broker that can enumerate what it holds.
    fn has_redactions(&self) -> bool {
        true
    }

    // ── convenience over `send` ─────────────────────────────────────────────
    //
    // Kept as default methods rather than as free functions so the call sites
    // read the way they did before the trait existed, and so there is exactly
    // one place each of them turns into a `Fetch`.

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
