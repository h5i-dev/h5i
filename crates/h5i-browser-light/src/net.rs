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
    /// The session's cookies. Attached here rather than by the HTTP client so
    /// that sending one is a decision this broker makes and records, like every
    /// other thing it does with the wire. `reqwest`'s own cookie store would
    /// have done the matching for us and taken that with it.
    jar: crate::cookies::Jar,
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
            jar: crate::cookies::Jar::new(),
        })
    }

    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    /// The session's jar, for the things that may legitimately touch it:
    /// counting it, and clearing it. There is deliberately no accessor that
    /// returns a cookie's value.
    pub fn jar(&self) -> &crate::cookies::Jar {
        &self.jar
    }

    /// Fetch a URL, following redirects by hand and checking policy on each hop.
    pub fn fetch(&self, url: &Url, initiator: Initiator) -> FetchOutcome {
        self.send(url, initiator, "GET", &[], None)
    }

    /// Send a request that may carry a body — what a form submission needs.
    ///
    /// One function rather than a second path beside [`Self::fetch`], because
    /// every guarantee this broker makes lives in that loop: the policy check,
    /// the record before the wire, the hand-followed redirects. A POST that
    /// took a shortcut around it would be the one request in the engine with no
    /// receipt, which is precisely the hole the whole design exists to close.
    ///
    /// The redirect rule is the browser's, and it is a security rule rather
    /// than a convenience: a 301/302/303 turns a POST into a GET and drops the
    /// body, so a form's credentials are not replayed to wherever a server
    /// points next. 307/308 preserve the method, and each hop is still checked
    /// against the allowlist like any other.
    pub fn send(
        &self,
        url: &Url,
        initiator: Initiator,
        method: &str,
        body: &[u8],
        content_type: Option<&str>,
    ) -> FetchOutcome {
        let mut current = url.clone();
        let mut initiator = initiator;
        let mut method = method.to_ascii_uppercase();
        let mut body = body.to_vec();

        for hop in 0..=self.policy.max_redirects() {
            let seq = self.seq.fetch_add(1, Ordering::Relaxed);

            // 1. Policy. A denial is recorded as a pair like any other request,
            //    so the log shows what was attempted, not only what succeeded.
            let verdict = self.policy.check(&current);
            if let Some(reason) = verdict.reason() {
                let record = RequestRecord::request(seq, initiator, &method, current.as_str())
                    .denied(reason);
                if let Err(e) = self.record_pair(&record) {
                    return FetchOutcome::failed(current, format!("receipt sink refused: {e}"));
                }
                return FetchOutcome::failed(current, format!("denied by policy: {reason}"));
            }

            // 2. The decision record, before any bytes move. If this cannot be
            //    written, the fetch does not happen — this is the fail-closed
            //    guarantee, and it is why `Sink::append` returns a Result.
            let record = RequestRecord::request(seq, initiator, &method, current.as_str());
            if let Err(e) = self.sink.append(&record) {
                return FetchOutcome::failed(
                    current,
                    format!("refusing to fetch: the receipt could not be written: {e}"),
                );
            }

            // 3. The wire. Cookies are attached here, after the policy check
            //    and after the record: a request that policy refuses must never
            //    have carried a credential anywhere, not even into a log line.
            let started = Instant::now();
            let verb = reqwest::Method::from_bytes(method.as_bytes())
                .unwrap_or(reqwest::Method::GET);
            let mut request = self.client.request(verb, current.clone());
            if !body.is_empty() {
                if let Some(kind) = content_type {
                    request = request.header(reqwest::header::CONTENT_TYPE, kind);
                }
                request = request.body(body.clone());
            }
            let mut cookies_sent = 0;
            if let Some((header, count)) = self.jar.header_for(&current) {
                request = request.header(reqwest::header::COOKIE, header);
                cookies_sent = count;
            }
            let response = request.send();
            let elapsed = started.elapsed().as_millis() as u64;

            let response = match response {
                Ok(response) => response,
                Err(e) => {
                    let mut outcome_record = record.response();
                    outcome_record.duration_ms = Some(elapsed);
                    outcome_record.cookies_sent = Some(cookies_sent);
                    outcome_record.error = Some(e.to_string());
                    let _ = self.sink.append(&outcome_record);
                    return FetchOutcome::failed(current, e.to_string());
                }
            };

            let status = response.status();

            // Before the redirect branch, deliberately: a login flow sets its
            // session cookie on the 302 itself, so a jar that only looked at
            // final responses would never see the thing it exists to hold.
            let cookies_stored = self.jar.store(
                &current,
                response
                    .headers()
                    .get_all(reqwest::header::SET_COOKIE)
                    .iter()
                    .filter_map(|v| v.to_str().ok()),
            );

            if status.is_redirection() {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|loc| current.join(loc).ok());

                let mut outcome_record = record.response();
                outcome_record.status = Some(status.as_u16());
                outcome_record.duration_ms = Some(elapsed);
                outcome_record.cookies_sent = Some(cookies_sent);
                outcome_record.cookies_stored = Some(cookies_stored);
                if location.is_none() {
                    outcome_record.error = Some("redirect without a usable Location".to_string());
                }
                let _ = self.sink.append(&outcome_record);

                match location {
                    Some(next) if hop < self.policy.max_redirects() => {
                        current = next;
                        initiator = Initiator::Redirect;
                        // 303 always, and 301/302 by universal practice, turn
                        // the follow-up into a bodyless GET. Carrying a form
                        // body onward would replay a password to whatever the
                        // server named next.
                        if matches!(status.as_u16(), 301..=303) {
                            method = "GET".to_string();
                            body.clear();
                        }
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
            outcome_record.cookies_sent = Some(cookies_sent);
            outcome_record.cookies_stored = Some(cookies_stored);

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

#[cfg(test)]
mod cookie_wire_tests {
    use super::*;
    use crate::receipt::MemorySink;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    /// A server that sets a cookie, then reports what came back.
    fn login_server() -> (u16, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            for _ in 0..2 {
                let Ok((stream, _)) = listener.accept() else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut sent_cookie = String::new();
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
                        break;
                    }
                    if let Some(rest) = header.to_ascii_lowercase().strip_prefix("cookie:") {
                        sent_cookie = rest.trim().to_string();
                    }
                }
                let mut stream = stream;
                if path == "/login" {
                    let body = "<html><body>ok</body></html>";
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nSet-Cookie: sid=s3cr3t-value; Path=/\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    ).unwrap();
                } else {
                    let body = format!("<html><body>saw:{sent_cookie}</body></html>");
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    ).unwrap();
                }
                let _ = stream.flush();
            }
        });
        (port, handle)
    }

    /// Redirects a POST once, then reports what the follow-up looked like.
    fn redirecting_server(status: u16) -> (u16, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let mut second = String::new();
            for hop in 0..2 {
                let Ok((stream, _)) = listener.accept() else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let method = line.split_whitespace().next().unwrap_or("").to_string();
                let mut length = 0usize;
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0u8; length];
                if length > 0 {
                    use std::io::Read;
                    let _ = reader.read_exact(&mut body);
                }
                let mut stream = stream;
                if hop == 0 {
                    write!(
                        stream,
                        "HTTP/1.1 {status} Moved
Location: /after
Content-Length: 0
Connection: close

"
                    ).unwrap();
                } else {
                    second = format!("{method} body={}", String::from_utf8_lossy(&body));
                    let page = "<html><body>done</body></html>";
                    write!(
                        stream,
                        "HTTP/1.1 200 OK
Content-Length: {}
Connection: close

{page}",
                        page.len()
                    ).unwrap();
                }
                let _ = stream.flush();
            }
            second
        });
        (port, handle)
    }

    #[test]
    fn a_redirected_post_does_not_replay_its_body_to_the_next_host() {
        // The browser rule, and it is a security rule: 301/302/303 turn the
        // follow-up into a bodyless GET, so a password typed into one form is
        // not re-sent to wherever that server points next.
        for status in [301u16, 302, 303] {
            let (port, server) = redirecting_server(status);
            let broker = Broker::new(Policy::new(), Arc::new(MemorySink::new()), None).unwrap();
            let target = Url::parse(&format!("http://127.0.0.1:{port}/login")).unwrap();

            let outcome = broker.send(
                &target,
                Initiator::Navigation,
                "POST",
                b"password=hunter2",
                Some("application/x-www-form-urlencoded"),
            );
            assert!(outcome.is_ok(), "{status}: {:?}", outcome.error);

            let followed = server.join().unwrap();
            assert_eq!(
                followed, "GET body=",
                "{status} must downgrade to a bodyless GET, got {followed:?}"
            );
        }
    }

    #[test]
    fn a_307_keeps_the_method_because_the_server_asked_for_that_explicitly() {
        let (port, server) = redirecting_server(307);
        let broker = Broker::new(Policy::new(), Arc::new(MemorySink::new()), None).unwrap();
        let target = Url::parse(&format!("http://127.0.0.1:{port}/login")).unwrap();

        broker.send(
            &target,
            Initiator::Navigation,
            "POST",
            b"password=hunter2",
            Some("application/x-www-form-urlencoded"),
        );

        let followed = server.join().unwrap();
        assert_eq!(followed, "POST body=password=hunter2", "got {followed:?}");
    }

    #[test]
    fn a_session_survives_between_requests_and_never_reaches_the_log() {
        let (port, server) = login_server();
        let sink = Arc::new(MemorySink::new());
        let broker = Broker::new(Policy::new(), sink.clone(), None).unwrap();

        // Loopback is reachable without an allowlist entry, which is what lets
        // this test exercise the wire without inventing a policy.
        let login = Url::parse(&format!("http://127.0.0.1:{port}/login")).unwrap();
        let outcome = broker.fetch(&login, Initiator::Navigation);
        assert!(outcome.is_ok(), "login failed: {:?}", outcome.error);
        assert_eq!(broker.jar().len(), 1, "the session cookie was kept");

        let page = Url::parse(&format!("http://127.0.0.1:{port}/app")).unwrap();
        let outcome = broker.fetch(&page, Initiator::Navigation);
        let body = String::from_utf8_lossy(&outcome.body).to_string();
        assert!(
            body.contains("saw:sid=s3cr3t-value"),
            "the second request carried the session: {body}"
        );

        // The receipt says how many, and nothing more. A credential in a
        // request log is a credential in every export that log ends up in.
        let log = serde_json::to_string(&sink.records()).unwrap();
        assert!(!log.contains("s3cr3t-value"), "a value reached the log:\n{log}");
        assert!(log.contains("cookies_sent"), "the count is recorded:\n{log}");

        let sent: Vec<usize> = sink
            .records()
            .into_iter()
            .filter_map(|r| r.cookies_sent)
            .collect();
        assert_eq!(sent, vec![0, 1], "none on login, one on the page after it");

        let _ = server.join();
    }
}
