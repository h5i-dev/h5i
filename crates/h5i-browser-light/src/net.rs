//! The broker: the only way bytes enter this engine.
//!
//! Every fetch — the page itself, every stylesheet, every image, every
//! redirect hop — goes through [`Broker::fetch`], which does the same three
//! things in the same order: check policy, record the decision, then use the
//! wire. There is no second path, which is what lets the receipt be the
//! network rather than a report about it.
//!
//! # The invariant that is easy to get wrong
//!
//! Blitz hands us a [`NetHandler`] per request and counts that request as
//! pending until the handler is called. `paint_scene` refuses to paint while
//! any *critical* resource is pending. So a denial must **complete** the
//! request with an empty body, not drop the handler: dropping it leaves the
//! document permanently unpaintable, which reads as "the engine is broken"
//! rather than "the tracker was blocked". [`BrokerNet::fetch`] therefore has
//! exactly one exit, and there is a test pinning it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use blitz_traits::net::{Bytes, NetHandler, NetProvider, Request};
use h5i_error::H5iError;
use url::Url;

use crate::policy::Policy;
use crate::receipt::{Initiator, RequestRecord, Sink};

/// What a fetch produced. A denied or failed fetch is still an outcome, with
/// an empty body and a reason — never an absence.
#[derive(Debug, Clone)]
pub struct FetchOutcome {
    pub final_url: Url,
    pub body: Vec<u8>,
    pub status: Option<u16>,
    pub error: Option<String>,
}

impl FetchOutcome {
    fn failed(url: Url, error: String) -> Self {
        Self {
            final_url: url,
            body: Vec::new(),
            status: None,
            error: Some(error),
        }
    }

    pub fn is_ok(&self) -> bool {
        self.error.is_none()
    }
}

/// Policy plus receipts plus a client, in that order of importance.
pub struct Broker {
    policy: Policy,
    sink: Arc<dyn Sink>,
    client: reqwest::blocking::Client,
    seq: AtomicU64,
}

impl Broker {
    /// Build a broker.
    ///
    /// `proxy` is h5i's egress proxy (`H5I_EGRESS_PROXY`). It is not required —
    /// the engine is useful on a bare host — but inside a box it is how the
    /// sandbox's own allowlist stays in the path. Loopback bypasses it, because
    /// the dev server is not egress.
    pub fn new(policy: Policy, sink: Arc<dyn Sink>, proxy: Option<&str>) -> Result<Self, H5iError> {
        let mut builder = reqwest::blocking::Client::builder()
            // Redirects are followed by hand so each hop is a policy decision
            // and a receipt line. Letting the client follow them would hide
            // exactly the hops most worth seeing.
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("h5i-browser-light/", env!("CARGO_PKG_VERSION")));

        if let Some(proxy_url) = proxy.filter(|p| !p.trim().is_empty()) {
            let no_proxy = reqwest::NoProxy::from_string("localhost,127.0.0.1,::1");
            let proxy = reqwest::Proxy::all(proxy_url)
                .map_err(|e| {
                    H5iError::Metadata(format!("egress proxy `{proxy_url}` is not usable: {e}"))
                })?
                .no_proxy(no_proxy);
            builder = builder.proxy(proxy);
        }

        let client = builder
            .build()
            .map_err(|e| H5iError::Metadata(format!("failed to build the http client: {e}")))?;

        Ok(Self {
            policy,
            sink,
            client,
            seq: AtomicU64::new(0),
        })
    }

    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    /// Fetch a URL, following redirects by hand and checking policy on each hop.
    pub fn fetch(&self, url: &Url, initiator: Initiator) -> FetchOutcome {
        let mut current = url.clone();
        let mut initiator = initiator;

        for hop in 0..=self.policy.max_redirects() {
            let seq = self.seq.fetch_add(1, Ordering::Relaxed);

            // 1. Policy. A denial is recorded as a pair like any other request,
            //    so the log shows what was attempted, not only what succeeded.
            let verdict = self.policy.check(&current);
            if let Some(reason) = verdict.reason() {
                let record =
                    RequestRecord::request(seq, initiator, "GET", current.as_str()).denied(reason);
                if let Err(e) = self.record_pair(&record) {
                    return FetchOutcome::failed(current, format!("receipt sink refused: {e}"));
                }
                return FetchOutcome::failed(current, format!("denied by policy: {reason}"));
            }

            // 2. The decision record, before any bytes move. If this cannot be
            //    written, the fetch does not happen — this is the fail-closed
            //    guarantee, and it is why `Sink::append` returns a Result.
            let record = RequestRecord::request(seq, initiator, "GET", current.as_str());
            if let Err(e) = self.sink.append(&record) {
                return FetchOutcome::failed(
                    current,
                    format!("refusing to fetch: the receipt could not be written: {e}"),
                );
            }

            // 3. The wire.
            let started = Instant::now();
            let response = self.client.get(current.clone()).send();
            let elapsed = started.elapsed().as_millis() as u64;

            let response = match response {
                Ok(response) => response,
                Err(e) => {
                    let mut outcome_record = record.response();
                    outcome_record.duration_ms = Some(elapsed);
                    outcome_record.error = Some(e.to_string());
                    let _ = self.sink.append(&outcome_record);
                    return FetchOutcome::failed(current, e.to_string());
                }
            };

            let status = response.status();

            if status.is_redirection() {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|loc| current.join(loc).ok());

                let mut outcome_record = record.response();
                outcome_record.status = Some(status.as_u16());
                outcome_record.duration_ms = Some(elapsed);
                if location.is_none() {
                    outcome_record.error = Some("redirect without a usable Location".to_string());
                }
                let _ = self.sink.append(&outcome_record);

                match location {
                    Some(next) if hop < self.policy.max_redirects() => {
                        current = next;
                        initiator = Initiator::Redirect;
                        continue;
                    }
                    Some(_) => {
                        return FetchOutcome::failed(
                            current,
                            format!("too many redirects (limit {})", self.policy.max_redirects()),
                        );
                    }
                    None => {
                        return FetchOutcome::failed(
                            current,
                            "redirect without a usable Location".to_string(),
                        );
                    }
                }
            }

            let body = self.read_capped(response);
            let mut outcome_record = record.response();
            outcome_record.status = Some(status.as_u16());
            outcome_record.duration_ms = Some(elapsed);

            return match body {
                Ok(body) => {
                    outcome_record.bytes = Some(body.len() as u64);
                    let _ = self.sink.append(&outcome_record);
                    FetchOutcome {
                        final_url: current,
                        body,
                        status: Some(status.as_u16()),
                        error: None,
                    }
                }
                Err(e) => {
                    outcome_record.error = Some(e.to_string());
                    let _ = self.sink.append(&outcome_record);
                    FetchOutcome::failed(current, e.to_string())
                }
            };
        }

        FetchOutcome::failed(
            current,
            format!("too many redirects (limit {})", self.policy.max_redirects()),
        )
    }

    /// Read at most `max_response_bytes`, so one hostile response cannot
    /// become this process's memory ceiling.
    fn read_capped(&self, response: reqwest::blocking::Response) -> Result<Vec<u8>, H5iError> {
        use std::io::Read;

        let cap = self.policy.max_response_bytes();
        let mut buf = Vec::new();
        let mut reader = response.take(cap + 1);
        reader
            .read_to_end(&mut buf)
            .map_err(|e| H5iError::Metadata(format!("failed to read the response body: {e}")))?;

        if buf.len() as u64 > cap {
            return Err(H5iError::Metadata(format!(
                "response exceeds the {cap} byte cap"
            )));
        }
        Ok(buf)
    }

    /// Write both phases for a request that never reaches the wire.
    fn record_pair(&self, record: &RequestRecord) -> Result<(), H5iError> {
        self.sink.append(record)?;
        self.sink.append(&record.response())
    }
}

/// Adapts the broker to Blitz's [`NetProvider`].
pub struct BrokerNet {
    broker: Arc<Broker>,
}

impl BrokerNet {
    pub fn new(broker: Arc<Broker>) -> Self {
        Self { broker }
    }
}

impl NetProvider for BrokerNet {
    fn fetch(&self, _doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        let outcome = self.broker.fetch(&request.url, Initiator::Subresource);

        // The single exit. A denied or failed request completes with an empty
        // body: Blitz counts the resource as resolved and paints, having
        // loaded nothing. Returning early here — however tempting for a
        // request we refused — leaves the document pending forever and blank.
        handler.bytes(outcome.final_url.to_string(), Bytes::from(outcome.body));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::receipt::{MemorySink, Phase};
    use std::sync::atomic::AtomicBool;

    fn broker_with(policy: Policy, sink: Arc<dyn Sink>) -> Arc<Broker> {
        Arc::new(Broker::new(policy, sink, None).expect("broker builds"))
    }

    fn url(s: &str) -> Url {
        Url::parse(s).expect("test url")
    }

    /// A sink that refuses everything, to prove the fail-closed path.
    struct RefusingSink;
    impl Sink for RefusingSink {
        fn append(&self, _record: &RequestRecord) -> Result<(), H5iError> {
            Err(H5iError::Internal("disk is on fire".to_string()))
        }
    }

    /// Records whether Blitz's handler was completed.
    struct SpyHandler {
        called: Arc<AtomicBool>,
        body_len: Arc<AtomicU64>,
    }
    impl NetHandler for SpyHandler {
        fn bytes(self: Box<Self>, _resolved_url: String, bytes: Bytes) {
            self.body_len.store(bytes.len() as u64, Ordering::SeqCst);
            self.called.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn a_denied_request_never_reaches_the_wire_and_is_recorded_as_a_pair() {
        let sink = Arc::new(MemorySink::new());
        // Empty policy: nothing remote is allowed, so this cannot escape even
        // if the test host has a network.
        let broker = broker_with(Policy::new(), sink.clone());

        let outcome = broker.fetch(&url("https://tracker.test/pixel.gif"), Initiator::Subresource);

        assert!(!outcome.is_ok());
        assert!(outcome.body.is_empty());
        assert!(outcome.error.unwrap().contains("denied by policy"));

        let records = sink.records();
        assert_eq!(records.len(), 2, "a denial is still a request/response pair");
        assert!(records.iter().all(|r| !r.allowed));
        assert_eq!(records[0].phase, Phase::Request);
        assert_eq!(records[1].phase, Phase::Response);
        assert!(sink.fetched_urls().is_empty());
        assert_eq!(sink.denied_urls(), vec!["https://tracker.test/pixel.gif"]);
    }

    #[test]
    fn no_receipt_means_no_fetch() {
        // The fail-closed claim, stated as a test: a sink that cannot record
        // the decision must stop the request, not be ignored.
        let broker = broker_with(Policy::new().allow("example.com"), Arc::new(RefusingSink));

        let outcome = broker.fetch(&url("https://example.com/"), Initiator::Navigation);

        assert!(!outcome.is_ok());
        let error = outcome.error.unwrap();
        assert!(
            error.contains("receipt could not be written"),
            "the refusal must name its cause, got: {error}"
        );
    }

    #[test]
    fn blitz_handler_is_always_completed_even_when_the_request_is_denied() {
        // If this regresses, `paint_scene` silently stops painting: the
        // document keeps a pending critical resource forever and every
        // screenshot comes back blank. See the module docs.
        let sink = Arc::new(MemorySink::new());
        let broker = broker_with(Policy::new(), sink);
        let net = BrokerNet::new(broker);

        let called = Arc::new(AtomicBool::new(false));
        let body_len = Arc::new(AtomicU64::new(u64::MAX));
        let handler = Box::new(SpyHandler {
            called: called.clone(),
            body_len: body_len.clone(),
        });

        net.fetch(
            0,
            Request::get(url("https://denied.test/style.css")),
            handler,
        );

        assert!(
            called.load(Ordering::SeqCst),
            "a denied request must still complete its handler"
        );
        assert_eq!(
            body_len.load(Ordering::SeqCst),
            0,
            "completing a denial must hand back no bytes"
        );
    }

    #[test]
    fn loopback_is_reachable_without_an_allowlist_entry() {
        // Not a network test: it asserts the policy decision the broker makes
        // before dialling, using a port nothing is listening on.
        let sink = Arc::new(MemorySink::new());
        let broker = broker_with(Policy::new(), sink.clone());

        let outcome = broker.fetch(&url("http://127.0.0.1:9/"), Initiator::Navigation);

        // The connection fails (nothing is listening), but it was *attempted*,
        // which is the difference from a policy denial.
        assert!(!outcome.is_ok());
        assert_eq!(sink.denied_urls().len(), 0);
        assert_eq!(sink.fetched_urls(), vec!["http://127.0.0.1:9/"]);
    }

    #[test]
    fn a_bad_proxy_url_is_refused_at_construction_not_at_first_fetch() {
        let sink = Arc::new(MemorySink::new());
        let result = Broker::new(Policy::new(), sink, Some("not a url"));
        assert!(result.is_err(), "a malformed proxy must fail loudly, early");
    }
}
