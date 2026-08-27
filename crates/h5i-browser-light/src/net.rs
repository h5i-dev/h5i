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

/// The one user agent: sent on the wire, and reported by `navigator.userAgent`.
///
/// Honest rather than imitative — it names this engine and does not claim to be
/// Chrome. The `Mozilla/5.0 (compatible; ...)` shape is kept because it is the
/// form content negotiation on real servers is written against, not because it
/// disguises anything.
///
/// Shared with the script realm deliberately. A page that branches on the user
/// agent server-side and again in script must see the same answer both times,
/// or it renders for one engine and scripts for another.
pub const USER_AGENT: &str = concat!(
    "Mozilla/5.0 (compatible; h5i-browser-light/",
    env!("CARGO_PKG_VERSION"),
    "; +https://github.com/h5i-dev/h5i)"
);

/// Matches `navigator.language`. A server that content-negotiates on this
/// should get the same answer the page's script would give.
pub const ACCEPT_LANGUAGE: &str = "en-US,en;q=0.9";

/// What this engine will take, by what asked for it.
///
/// Not cosmetic: crates.io answered **404** to a request with no `Accept` at
/// all, and the corpus recorded an empty page with no error. A server that
/// content-negotiates cannot serve a client that never says what it wants.
fn accept_for(initiator: Initiator) -> &'static str {
    match initiator {
        Initiator::Navigation | Initiator::Redirect => {
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"
        }
        Initiator::Subresource => "*/*",
    }
}


/// What a fetch produced. A denied or failed fetch is still an outcome, with
/// an empty body and a reason — never an absence.
#[derive(Debug, Clone)]
pub struct FetchOutcome {
    /// The receipt sequence number this request was recorded under.
    ///
    /// Carried out of the broker so a caller can say *which* receipt a page's
    /// action produced. `None` when the request never reached the point of
    /// being recorded, which is the same thing as there being no receipt to
    /// name.
    pub seq: Option<u64>,
    /// Response headers, in arrival order. Carried because `Response.headers`
    /// is how a page learns content type, pagination links and rate limits, and
    /// returning `null` from `headers.get` made every one of those look absent
    /// rather than unsupported.
    pub headers: Vec<(String, String)>,
    pub final_url: Url,
    pub body: Vec<u8>,
    pub status: Option<u16>,
    pub error: Option<String>,
    /// Set when this was a cross-origin `no-cors` request.
    ///
    /// The body and headers are already empty and the status is already zero,
    /// which is what an opaque response *is*. The flag exists so the caller can
    /// say so rather than presenting a page with an empty 0 that looks like a
    /// failure — a page checking `response.type === "opaque"` is doing the
    /// right thing and should get the right answer.
    pub opaque: bool,
}

impl FetchOutcome {
    /// An outcome that never reached the wire. Public because the script realm
    /// needs to answer a request it could not even start, rather than leaving
    /// the page's promise pending forever.
    pub fn refused(url: Url, error: String) -> Self {
        Self::failed(url, error)
    }

    fn failed(url: Url, error: String) -> Self {
        Self::failed_at(url, error, None)
    }

    fn failed_at(url: Url, error: String, seq: Option<u64>) -> Self {
        Self {
            seq,
            headers: Vec::new(),
            final_url: url,
            body: Vec::new(),
            status: None,
            error: Some(error),
            opaque: false,
        }
    }

    pub fn is_ok(&self) -> bool {
        self.error.is_none()
    }
}

/// Policy plus receipts plus a client, in that order of importance.
/// Addresses this broker has already checked, keyed by host.
///
/// The other half of [`Policy::check_address`], and the half that makes the
/// check mean something. Resolving a name to decide about it and then letting
/// the HTTP client resolve it again leaves a window between the two: the second
/// answer is what the bytes go to, and nothing checked it. That window is the
/// whole of DNS rebinding.
///
/// So the checked addresses are *pinned*. The client is built with this as its
/// resolver, and it answers from what the broker already decided about rather
/// than asking the network a second time. A name that is not in the map has not
/// been through the policy, and the answer is a refusal rather than a lookup —
/// fail closed, in the one place where failing open would mean connecting
/// somewhere nobody approved.
///
/// Lightpanda does this at curl's open-socket callback, which sees the resolved
/// `sockaddr` and can refuse it there. reqwest exposes no such hook; a pinning
/// resolver reaches the same place from the other side.
#[derive(Default)]
struct Pinned {
    by_host: std::sync::Mutex<std::collections::HashMap<String, Vec<std::net::SocketAddr>>>,
}

impl Pinned {
    /// Remember the addresses a host was approved for.
    fn set(&self, host: &str, addrs: Vec<std::net::SocketAddr>) {
        if let Ok(mut map) = self.by_host.lock() {
            map.insert(host.to_ascii_lowercase(), addrs);
        }
    }

    fn get(&self, host: &str) -> Option<Vec<std::net::SocketAddr>> {
        self.by_host.lock().ok()?.get(&host.to_ascii_lowercase()).cloned()
    }
}

impl reqwest::dns::Resolve for Pinned {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let found = self.get(name.as_str());
        Box::pin(std::future::ready(match found {
            Some(addrs) => {
                let iter: reqwest::dns::Addrs = Box::new(addrs.into_iter());
                Ok(iter)
            }
            // Not an error worth dressing up: every request this client makes
            // goes through the broker, which pins before it sends. Arriving
            // here means something reached the wire without a decision, and
            // connecting anyway is the one outcome that must not happen.
            None => Err(format!(
                "`{}` was not resolved through the policy, so this engine will not connect \
                 to it",
                name.as_str()
            )
            .into()),
        }))
    }
}

/// What a script request needs the broker to know about its origin.
///
/// Only script requests carry one. A navigation has no document behind it, and
/// the absence of this is what says so.
struct CorsContext {
    /// `None` for a document with an opaque origin — a `file:` page, say —
    /// which is same-origin with nothing and so may read nothing cross-origin.
    document: Option<crate::cors::Origin>,
    headers: Vec<(String, String)>,
    mode: crate::cors::Mode,
    credentials: crate::cors::Credentials,
}

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
    /// Whether requests route through an egress proxy.
    ///
    /// Kept because the socket client has to know: it cannot use one — a raw
    /// `TcpStream` does not go through what `reqwest` was configured with — so
    /// it refuses any non-loopback address while one is set, rather than
    /// stepping around the allowlist that proxy enforces.
    proxied: bool,
    /// Addresses already approved, and the client's only source of them.
    ///
    /// `None` when an egress proxy is configured: the proxy resolves the name
    /// itself and this engine never sees an address, so pinning one would be a
    /// claim it cannot support. The proxy is the enforcement point there, which
    /// is the same division of labour the socket client already follows.
    pinned: Option<Arc<Pinned>>,
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
            .user_agent(USER_AGENT);

        let mut proxied = false;
        if let Some(proxy_url) = proxy.filter(|p| !p.trim().is_empty()) {
            proxied = true;
            let no_proxy = reqwest::NoProxy::from_string("localhost,127.0.0.1,::1");
            let proxy = reqwest::Proxy::all(proxy_url)
                .map_err(|e| {
                    H5iError::Metadata(format!("egress proxy `{proxy_url}` is not usable: {e}"))
                })?
                .no_proxy(no_proxy);
            builder = builder.proxy(proxy);
        }

        // With a proxy in the path the name is resolved at the far end and this
        // engine never sees an address, so there is nothing to pin and the
        // proxy is the enforcement point. Without one, every connection goes to
        // an address this broker has already decided about.
        let pinned = if proxied {
            None
        } else {
            let pinned = Arc::new(Pinned::default());
            builder = builder.dns_resolver(pinned.clone());
            Some(pinned)
        };

        let client = builder
            .build()
            .map_err(|e| H5iError::Metadata(format!("failed to build the http client: {e}")))?;

        Ok(Self {
            policy,
            sink,
            client,
            pinned,
            seq: AtomicU64::new(0),
            jar: crate::cookies::Jar::new(),
            proxied,
        })
    }

    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    /// Ask a server, before the real request, whether it will accept one.
    ///
    /// A request in its own right and treated as one: policy-checked and
    /// receipted like everything else, so it appears in the log rather than
    /// arriving from nowhere. A caller reading two requests where the page made
    /// one is reading the truth — the preflight really did happen, and it
    /// really did cost a round trip.
    ///
    /// Not cached. `Access-Control-Max-Age` would make this cheaper and is one
    /// more piece of state that can be wrong; when a corpus page makes the cost
    /// real it can be added, with the receipts showing what was reused.
    fn preflight(
        &self,
        url: &Url,
        ask: &crate::cors::Preflight,
        document: Option<&Url>,
    ) -> Result<(), String> {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);

        // The same allowlist as any other request. A preflight is a request to
        // the origin being asked about, so it is subject to the same grant.
        if let Some(reason) = self.policy.check_from(url, document).reason() {
            let record = RequestRecord::request(seq, Initiator::Subresource, "OPTIONS", url.as_str())
                .denied(reason);
            let _ = self.record_pair(&record);
            return Err(format!("the preflight was denied by policy: {reason}"));
        }
        if let Err(reason) = self.pin_addresses(url) {
            let record = RequestRecord::request(seq, Initiator::Subresource, "OPTIONS", url.as_str())
                .denied(&reason);
            let _ = self.record_pair(&record);
            return Err(format!("the preflight was denied by policy: {reason}"));
        }

        let record = RequestRecord::request(seq, Initiator::Subresource, "OPTIONS", url.as_str());
        if let Err(e) = self.sink.append(&record) {
            return Err(format!(
                "refusing to preflight: the receipt could not be written: {e}"
            ));
        }

        let started = Instant::now();
        let mut request = self
            .client
            .request(reqwest::Method::OPTIONS, url.clone())
            .header("origin", ask.origin.clone())
            .header("access-control-request-method", ask.method.clone());
        if !ask.headers.is_empty() {
            request = request.header("access-control-request-headers", ask.headers.join(", "));
        }
        // Never. A preflight is a question about whether a credentialed request
        // would be allowed, and sending the credential to ask would defeat the
        // asking.
        let response = request.send();
        let elapsed = started.elapsed().as_millis() as u64;

        let response = match response {
            Ok(response) => response,
            Err(e) => {
                let mut outcome = record.response();
                outcome.duration_ms = Some(elapsed);
                outcome.error = Some(e.to_string());
                let _ = self.sink.append(&outcome);
                return Err(format!("the preflight could not be sent: {e}"));
            }
        };

        let status = response.status();
        let header = |name: &str| -> Option<String> {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        let acao = header("access-control-allow-origin");
        let acac = header("access-control-allow-credentials");
        let methods = header("access-control-allow-methods");
        let headers = header("access-control-allow-headers");

        let mut outcome = record.response();
        outcome.status = Some(status.as_u16());
        outcome.duration_ms = Some(elapsed);
        let _ = self.sink.append(&outcome);

        if !status.is_success() {
            return Err(format!(
                "the preflight was answered with {}, so the request was not made.",
                status.as_u16()
            ));
        }

        crate::cors::check_preflight(
            ask,
            acao.as_deref(),
            acac.as_deref(),
            methods.as_deref(),
            headers.as_deref(),
        )
    }

    /// Resolve a URL's host, check every address it answers with, and pin the
    /// result for the connection that follows.
    ///
    /// `Ok(())` when there is nothing to do — a proxy in the path, or a URL
    /// with no host — so the caller has one branch rather than three.
    ///
    /// **Every** address is checked, not the first. A name that answers with
    /// one public address and one loopback address is refused: which one gets
    /// connected to is the client's choice among them, and approving a set
    /// while objecting to a member of it would leave the outcome to chance.
    fn pin_addresses(&self, url: &Url) -> Result<(), String> {
        let Some(pinned) = self.pinned.as_ref() else {
            // A proxy resolves the name at the far end; there is no address
            // here to check, and none to pin.
            return Ok(());
        };
        if !matches!(url.scheme(), "http" | "https") {
            return Ok(());
        }
        let Some(host) = url.host_str() else {
            return Ok(());
        };

        let port = url
            .port_or_known_default()
            .unwrap_or(if url.scheme() == "https" { 443 } else { 80 });
        // IPv6 arrives from `host_str` with its brackets, which the resolver
        // does not want.
        let bare = host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(host);

        use std::net::ToSocketAddrs;
        let resolved: Vec<std::net::SocketAddr> = match (bare, port).to_socket_addrs() {
            Ok(addrs) => addrs.collect(),
            Err(e) => return Err(format!("`{host}` could not be resolved: {e}")),
        };
        if resolved.is_empty() {
            return Err(format!("`{host}` resolved to no addresses"));
        }
        for addr in &resolved {
            if let Some(reason) = self.policy.check_address(url, addr.ip()).reason() {
                return Err(reason.to_string());
            }
        }
        pinned.set(host, resolved);
        Ok(())
    }

    /// The session's jar, for the things that may legitimately touch it:
    /// counting it, and clearing it. There is deliberately no accessor that
    /// returns a cookie's value.
    pub fn jar(&self) -> &crate::cookies::Jar {
        &self.jar
    }

    /// Fetch a URL, following redirects by hand and checking policy on each hop.
    ///
    /// No document, so the loopback rule treats this as the agent naming a URL.
    /// Anything a *page* reaches for — a subresource, a script `src`, a form's
    /// action — must use [`Self::fetch_from`] instead, or it is trusted like an
    /// instruction the agent typed.
    pub fn fetch(&self, url: &Url, initiator: Initiator) -> FetchOutcome {
        self.send(url, initiator, "GET", &[], None)
    }

    /// [`Self::fetch`] for something a *document* asked for, so the policy can
    /// tell a page reaching for loopback from the agent naming it. See
    /// [`Policy::check_from`].
    pub fn fetch_from(
        &self,
        url: &Url,
        initiator: Initiator,
        document: Option<&Url>,
    ) -> FetchOutcome {
        self.send_from(url, initiator, "GET", &[], None, document)
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
        self.send_from(url, initiator, method, body, content_type, None)
    }

    /// Send a request a *document* made, so the policy can reason about origin.
    ///
    /// The document is threaded through rather than stored on the broker because
    /// one broker serves a whole session and the page underneath it changes.
    #[allow(clippy::too_many_arguments)]
    pub fn send_from(
        &self,
        url: &Url,
        initiator: Initiator,
        method: &str,
        body: &[u8],
        content_type: Option<&str>,
        document: Option<&Url>,
    ) -> FetchOutcome {
        // No origin context: the agent asked for this by name, so there is no
        // document whose boundary could be crossed. See `cors::plan`.
        self.send_with_cors(url, initiator, method, body, content_type, document, None)
    }

    /// The same send, subject to the same-origin policy.
    ///
    /// Separate entry rather than a flag, because the two callers are asking
    /// different questions. A navigation is the *agent* exercising its own
    /// authority over a URL it named; a `fetch` is a *page* exercising an
    /// authority it has to be granted. Answering both through one door with no
    /// argument between them is how the second was going unasked.
    #[allow(clippy::too_many_arguments)]
    pub fn send_script(
        &self,
        url: &Url,
        method: &str,
        body: &[u8],
        content_type: Option<&str>,
        document: &Url,
        headers: &[(String, String)],
        mode: crate::cors::Mode,
        credentials: crate::cors::Credentials,
    ) -> FetchOutcome {
        let context = CorsContext {
            document: crate::cors::Origin::of(document),
            headers: headers.to_vec(),
            mode,
            credentials,
        };
        self.send_with_cors(
            url,
            Initiator::Subresource,
            method,
            body,
            content_type,
            Some(document),
            Some(&context),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn send_with_cors(
        &self,
        url: &Url,
        initiator: Initiator,
        method: &str,
        body: &[u8],
        content_type: Option<&str>,
        document: Option<&Url>,
        cors: Option<&CorsContext>,
    ) -> FetchOutcome {
        let mut current = url.clone();
        // What a caller may see of the answer. Recomputed per hop, because a
        // redirect can change the origin the answer comes from.
        let mut cors_exposure = crate::cors::Exposure::Full;
        // Set once a redirect has crossed an origin, which makes the request's
        // own origin opaque from there on: a server must not be able to launder
        // a cross-origin read by bouncing it somewhere that says `*`.
        let mut origin_tainted = false;
        // What originally asked, kept separate from the per-hop initiator: a
        // redirect chain that began as a navigation is still asking for a
        // document, and a subresource that redirects is still a subresource.
        let asked_as = initiator;
        let mut initiator = initiator;
        let mut method = method.to_ascii_uppercase();
        let mut body = body.to_vec();

        for hop in 0..=self.policy.max_redirects() {
            let seq = self.seq.fetch_add(1, Ordering::Relaxed);

            // 1. Policy. A denial is recorded as a pair like any other request,
            //    so the log shows what was attempted, not only what succeeded.
            let verdict = self.policy.check_from(&current, document);
            if let Some(reason) = verdict.reason() {
                let record = RequestRecord::request(seq, initiator, &method, current.as_str())
                    .denied(reason);
                if let Err(e) = self.record_pair(&record) {
                    return FetchOutcome::failed(current, format!("receipt sink refused: {e}"));
                }
                // A refused *redirect* is a different fact from a refused
                // request, and the difference is actionable. vitejs.dev moved to
                // vite.dev; the corpus reported only "origin is not in the
                // allowlist" and an agent had no way to learn the site had
                // moved. Following it automatically is not the fix — that would
                // let any server route us out of the allowlist — but saying so
                // is.
                let message = if hop > 0 {
                    format!(
                        "the site redirected to {current}, which is not allowed: {reason}. \
                         This engine will not follow a redirect out of the allowlist, because \
                         a server could then choose where we go; allow that host if you meant \
                         to follow it."
                    )
                } else {
                    format!("denied by policy: {reason}")
                };
                return FetchOutcome::failed_at(current, message, Some(seq));
            }

            // 1b. Where that name actually goes. The check above decided about
            //     a *name*; this decides about the address behind it and pins
            //     the answer, so the bytes cannot reach somewhere the decision
            //     never saw. Recorded as a denial like any other, because "the
            //     name was allowed and the address was not" is exactly what a
            //     reader of the log would want to find.
            if let Err(reason) = self.pin_addresses(&current) {
                let record = RequestRecord::request(seq, initiator, &method, current.as_str())
                    .denied(&reason);
                if let Err(e) = self.record_pair(&record) {
                    return FetchOutcome::failed(current, format!("receipt sink refused: {e}"));
                }
                return FetchOutcome::failed_at(
                    current,
                    format!("denied by policy: {reason}"),
                    Some(seq),
                );
            }

            // 1c. The same-origin policy. The checks above decided whether
            //     this engine may *connect*; this decides whether the document
            //     that asked may *read the answer*, which is a different
            //     question and was going unasked. See `crate::cors`.
            let mut cors_plan: Option<crate::cors::Plan> = None;
            if let Some(context) = cors {
                // Tainted by a cross-origin redirect, or a document with no
                // origin of its own: both are opaque, and an opaque requester
                // is same-origin with nothing. Distinct from `Agent`, which is
                // also origin-less and is the *opposite* case — see
                // `cors::Requester`.
                let requester = match (origin_tainted, context.document.as_ref()) {
                    (false, Some(origin)) => crate::cors::Requester::Document(origin),
                    _ => crate::cors::Requester::Opaque,
                };
                let plan = crate::cors::plan(
                    requester,
                    &current,
                    &method,
                    &context.headers,
                    context.mode,
                    context.credentials,
                );

                if let crate::cors::Plan::Blocked(why) = &plan {
                    let record =
                        RequestRecord::request(seq, initiator, &method, current.as_str())
                            .denied(why);
                    if let Err(e) = self.record_pair(&record) {
                        return FetchOutcome::failed(
                            current,
                            format!("receipt sink refused: {e}"),
                        );
                    }
                    return FetchOutcome::failed_at(
                        current,
                        format!("blocked by the same-origin policy: {why}"),
                        Some(seq),
                    );
                }

                // A request that is not simple asks permission first, and the
                // preflight is a request like any other: policy-checked and
                // receipted, so it appears in the log rather than arriving
                // from nowhere.
                if let crate::cors::Plan::Send {
                    preflight: Some(ask),
                    ..
                } = &plan
                    && let Err(why) = self.preflight(&current, ask, document)
                {
                    return FetchOutcome::failed_at(
                        current,
                        format!("blocked by the same-origin policy: {why}"),
                        Some(seq),
                    );
                }
                cors_plan = Some(plan);
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
            let mut request = self
                .client
                .request(verb, current.clone())
                .header(reqwest::header::ACCEPT, accept_for(asked_as))
                .header(reqwest::header::ACCEPT_LANGUAGE, ACCEPT_LANGUAGE);
            if !body.is_empty() {
                if let Some(kind) = content_type {
                    request = request.header(reqwest::header::CONTENT_TYPE, kind);
                }
                request = request.body(body.clone());
            }
            // Cookies, and whether this request may carry them at all.
            //
            // The gate is the whole point of the credentials mode: a
            // cross-origin `fetch` defaults to sending none, so a script on one
            // allowlisted origin cannot read another origin's pages *as the
            // logged-in user*. Before this the jar was attached to every
            // request unconditionally, which is what turned the missing
            // same-origin policy from an unauthenticated cross-origin read into
            // an authenticated one.
            let may_send_cookies = match &cors_plan {
                Some(crate::cors::Plan::Send { send_cookies, .. }) => *send_cookies,
                // No CORS context: the agent asked, and its own requests carry
                // its own session.
                _ => true,
            };
            let mut cookies_sent = 0;
            if may_send_cookies
                && let Some((header, count)) = self.jar.header_for(&current)
            {
                request = request.header(reqwest::header::COOKIE, header);
                cookies_sent = count;
            }
            // Cross-origin requests announce themselves, which is how a server
            // knows to answer the CORS question at all.
            if let Some(crate::cors::Plan::Send {
                origin_header: Some(origin),
                ..
            }) = &cors_plan
            {
                request = request.header("origin", origin.clone());
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
                    return FetchOutcome::failed_at(current, e.to_string(), Some(seq));
                }
            };

            let status = response.status();
            let headers: Vec<(String, String)> = response
                .headers()
                .iter()
                .filter_map(|(name, value)| {
                    value.to_str().ok().map(|v| (name.as_str().to_string(), v.to_string()))
                })
                .collect();

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
                        // A redirect that crosses an origin makes the request's
                        // own origin opaque from here on, so a server cannot
                        // launder a cross-origin read by bouncing it somewhere
                        // that answers `*`. Checked against the *previous* hop,
                        // because that is the boundary being crossed.
                        if cors.is_some()
                            && crate::cors::Origin::of(&next) != crate::cors::Origin::of(&current)
                        {
                            origin_tainted = true;
                        }
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

            // The answer to the question the `Origin` header asked. A response
            // that does not name this origin back is not handed to the caller —
            // which is the entire point, and the reason the body is not even
            // read below when this fails.
            if let Some(crate::cors::Plan::Send {
                check_response: true,
                send_cookies,
                ..
            }) = &cors_plan
            {
                let header = |name: &str| -> Option<&str> {
                    headers
                        .iter()
                        .find(|(key, _)| key.eq_ignore_ascii_case(name))
                        .map(|(_, value)| value.as_str())
                };
                let origin = match &cors_plan {
                    Some(crate::cors::Plan::Send {
                        origin_header: Some(origin),
                        ..
                    }) => origin.clone(),
                    _ => "null".to_string(),
                };
                if let Err(why) = crate::cors::check_response(
                    header("access-control-allow-origin"),
                    header("access-control-allow-credentials"),
                    &origin,
                    *send_cookies,
                ) {
                    let mut refused = record.response();
                    refused.status = Some(status.as_u16());
                    refused.duration_ms = Some(elapsed);
                    refused.cookies_sent = Some(cookies_sent);
                    refused.cookies_stored = Some(cookies_stored);
                    refused.error = Some(format!("blocked by the same-origin policy: {why}"));
                    let _ = self.sink.append(&refused);
                    return FetchOutcome::failed_at(
                        current,
                        format!("blocked by the same-origin policy: {why}"),
                        Some(seq),
                    );
                }
                // Allowed. What of the headers the caller may see is the
                // server's to widen, and `*` does not widen a credentialed
                // response.
                cors_exposure = crate::cors::exposure_from(
                    header("access-control-expose-headers"),
                    *send_cookies,
                );
            } else if let Some(crate::cors::Plan::Send {
                exposure: crate::cors::Exposure::Opaque,
                ..
            }) = &cors_plan
            {
                cors_exposure = crate::cors::Exposure::Opaque;
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
                    // What the caller may see of this. Same-origin sees
                    // everything; a cross-origin CORS response is filtered to
                    // the safelist plus whatever the server exposed; a no-cors
                    // response is opaque, which is the whole reason it was
                    // allowed to be sent without asking.
                    let exposure = cors_exposure.clone();
                    let opaque = matches!(exposure, crate::cors::Exposure::Opaque);
                    FetchOutcome {
                        seq: Some(seq),
                        headers: crate::cors::filter_headers(&headers, &exposure),
                        final_url: current,
                        body: if opaque { Vec::new() } else { body },
                        status: if opaque { Some(0) } else { Some(status.as_u16()) },
                        error: None,
                        opaque,
                    }
                }
                Err(e) => {
                    outcome_record.error = Some(e.to_string());
                    let _ = self.sink.append(&outcome_record);
                    FetchOutcome::failed_at(current, e.to_string(), Some(seq))
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

    /// Whether an egress proxy is in the path.
    ///
    /// Read by the socket client, which cannot go through one: a raw
    /// `TcpStream` bypasses whatever `reqwest` was configured with, and inside
    /// a box that proxy is how the sandbox's allowlist stays in the path.
    pub fn has_proxy(&self) -> bool {
        self.proxied
    }

    /// Authorise a long-lived connection and record the decision.
    ///
    /// The front half of [`Broker::send_from`] — policy, then the record,
    /// *then* the caller may dial — for a connection that has no single body to
    /// read and so cannot use the rest of that loop. Returns the sequence the
    /// handshake was recorded under.
    ///
    /// Same rule, same order: no receipt, no connection.
    pub fn authorise_socket(&self, url: &Url, document: Option<&Url>) -> Result<u64, String> {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);

        let verdict = self.policy.check_from(url, document);
        if let Some(reason) = verdict.reason() {
            let record =
                RequestRecord::request(seq, Initiator::Subresource, "WS-OPEN", url.as_str())
                    .denied(reason);
            if let Err(e) = self.record_pair(&record) {
                return Err(format!("receipt sink refused: {e}"));
            }
            return Err(format!("denied by policy: {reason}"));
        }

        // And where that name goes, on the same rule as every other request.
        if let Err(reason) = self.pin_addresses(url) {
            let record =
                RequestRecord::request(seq, Initiator::Subresource, "WS-OPEN", url.as_str())
                    .denied(&reason);
            if let Err(e) = self.record_pair(&record) {
                return Err(format!("receipt sink refused: {e}"));
            }
            return Err(format!("denied by policy: {reason}"));
        }

        let record = RequestRecord::request(seq, Initiator::Subresource, "WS-OPEN", url.as_str());
        if let Err(e) = self.sink.append(&record) {
            return Err(format!(
                "refusing to connect: the receipt could not be written: {e}"
            ));
        }
        Ok(seq)
    }

    /// Record one frame on an open connection.
    ///
    /// **Every frame, not just the handshake.** A socket open for ten minutes
    /// carrying four hundred messages could be honoured by receipting the
    /// handshake alone — and then this engine's central claim would quietly
    /// stop covering the bytes that followed it, which is the CONNECT-gate
    /// blindness it exists to remove.
    ///
    /// Written as an ordinary request/response pair with `WS-SEND`/`WS-RECV` as
    /// the method. That is not an HTTP verb and is hyphenated so it cannot be
    /// read as one, but "a thing that crossed the wire, this size, in this
    /// direction" is exactly what a [`RequestRecord`] holds — and reusing it
    /// means the console, `h5i box watch` and the export bundle all show socket
    /// traffic without being taught a new phase to skip.
    pub fn record_socket_frame(
        &self,
        url: &Url,
        direction: crate::wsclient::Direction,
        bytes: u64,
    ) -> Result<(), H5iError> {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let record = RequestRecord::request(
            seq,
            Initiator::Subresource,
            direction.as_method(),
            url.as_str(),
        );
        self.sink.append(&record)?;
        let mut outcome = record.response();
        outcome.bytes = Some(bytes);
        // No status. 101 is the WebSocket upgrade's, and stamping it on every
        // frame said "switching protocols" four hundred times on one
        // connection — and said it on event streams, which never switched
        // anything. A frame is not an exchange with a status of its own.
        self.sink.append(&outcome)
    }

    /// Authorise and begin an event stream, handing back the open response.
    ///
    /// The second exit from the receipt path, and the reason it exists:
    /// [`Broker::send_from`] reads a whole body before it returns and writes
    /// one response record with a final byte count. An event stream never
    /// completes, so it would hit the response cap or the client timeout and be
    /// reported as an error.
    ///
    /// The front half is identical — policy, then the decision record, *then*
    /// the wire — because that is the half the fail-closed rule lives in, and
    /// two copies of it would be two rules.
    pub fn open_event_stream(
        &self,
        url: &Url,
        document: Option<&Url>,
    ) -> Result<reqwest::blocking::Response, String> {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);

        let verdict = self.policy.check_from(url, document);
        if let Some(reason) = verdict.reason() {
            let record =
                RequestRecord::request(seq, Initiator::Subresource, "SSE-OPEN", url.as_str())
                    .denied(reason);
            if let Err(e) = self.record_pair(&record) {
                return Err(format!("receipt sink refused: {e}"));
            }
            return Err(format!("denied by policy: {reason}"));
        }

        // And where that name goes, on the same rule as every other request.
        if let Err(reason) = self.pin_addresses(url) {
            let record =
                RequestRecord::request(seq, Initiator::Subresource, "SSE-OPEN", url.as_str())
                    .denied(&reason);
            if let Err(e) = self.record_pair(&record) {
                return Err(format!("receipt sink refused: {e}"));
            }
            return Err(format!("denied by policy: {reason}"));
        }

        let record = RequestRecord::request(seq, Initiator::Subresource, "SSE-OPEN", url.as_str());
        if let Err(e) = self.sink.append(&record) {
            return Err(format!(
                "refusing to connect: the receipt could not be written: {e}"
            ));
        }

        let mut request = self
            .client
            .request(reqwest::Method::GET, url.clone())
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .header(reqwest::header::ACCEPT_LANGUAGE, ACCEPT_LANGUAGE)
            // The client carries a 30s timeout for ordinary requests, which is
            // exactly wrong for a stream that is *supposed* to stay open and
            // quiet. Cleared for this one request only.
            .timeout(Duration::from_secs(60 * 60));
        // `header_for` reports the value and how many cookies went with it;
        // only the value goes on the wire, and the count is what a receipt is
        // allowed to carry.
        if let Some((cookies, _count)) = self.jar.header_for(url) {
            request = request.header(reqwest::header::COOKIE, cookies);
        }

        match request.send() {
            Ok(response) => {
                // The response half, on success too. Every other path here
                // writes both phases, and a reader that pairs them — the
                // console's request/response linkage, `h5i box watch` — showed
                // an `SSE-OPEN` request that never completed for the life of
                // the session. It records that the connection was *established*;
                // what flows after it is receipted per event.
                let mut outcome = record.response();
                outcome.status = Some(response.status().as_u16());
                if let Err(e) = self.sink.append(&outcome) {
                    return Err(format!(
                        "refusing to stream: the receipt could not be written: {e}"
                    ));
                }
                Ok(response)
            }
            Err(error) => {
                let mut outcome = record.response();
                outcome.error = Some(error.to_string());
                let _ = self.sink.append(&outcome);
                Err(format!("could not open the event stream: {error}"))
            }
        }
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
    /// The document whose subresources these are.
    ///
    /// Load-bearing, not bookkeeping. Every image, stylesheet and font on the
    /// page arrives through here, and without an origin to attribute them to
    /// the policy read each one as the agent naming a URL — so
    /// `<img src="http://127.0.0.1:3000/…">` on a page from the open web reached
    /// the box's dev server, which is precisely what [`Policy::check_from`]
    /// exists to refuse. `None` only for a document with no origin of its own.
    document: Option<Url>,
}

impl BrokerNet {
    pub fn new(broker: Arc<Broker>, document: Option<Url>) -> Self {
        Self { broker, document }
    }
}

impl NetProvider for BrokerNet {
    fn fetch(&self, _doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        let outcome =
            self.broker
                .fetch_from(&request.url, Initiator::Subresource, self.document.as_ref());

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
        let net = BrokerNet::new(broker, Some(url("https://denied.test/")));

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

    /// A subresource is the *page* reaching for a URL, and it was being policed
    /// as though the agent had named one.
    ///
    /// `check_from` refuses a loopback request from a document that is not
    /// itself local — that is the rule that stops a page on the open web reading
    /// the box's dev server. Every non-script path into the broker passed
    /// `document: None`, which the same function documents as trusted, so
    /// `<img src="http://127.0.0.1:3000/…">` and `<script src=…>` on a page from
    /// the web went straight through the guard.
    #[test]
    fn a_page_from_the_web_cannot_reach_loopback_through_a_subresource() {
        let sink = Arc::new(MemorySink::new());
        let broker = broker_with(Policy::new().allow("docs.test"), sink.clone());
        let dev_server = url("http://127.0.0.1:9/src/main.rs");

        let outcome = broker.fetch_from(
            &dev_server,
            Initiator::Subresource,
            Some(&url("https://docs.test/page")),
        );
        assert!(!outcome.is_ok());
        assert_eq!(sink.denied_urls(), vec![dev_server.as_str()]);
        assert!(sink.fetched_urls().is_empty(), "nothing may reach the wire");

        // ...and the dev server's own page still talks to itself, which is the
        // whole reason loopback is reachable at all.
        let sink = Arc::new(MemorySink::new());
        let broker = broker_with(Policy::new(), sink.clone());
        let _ = broker.fetch_from(
            &dev_server,
            Initiator::Subresource,
            Some(&url("http://127.0.0.1:9/")),
        );
        assert_eq!(sink.denied_urls().len(), 0);
        assert_eq!(sink.fetched_urls(), vec![dev_server.as_str()]);
    }

    /// The Blitz adapter is where every image, stylesheet and font arrives, so
    /// it is the widest of those paths and carries the document explicitly.
    #[test]
    fn the_blitz_adapter_attributes_subresources_to_their_document() {
        let sink = Arc::new(MemorySink::new());
        let broker = broker_with(Policy::new().allow("docs.test"), sink.clone());
        let net = BrokerNet::new(broker, Some(url("https://docs.test/page")));

        let handler = Box::new(SpyHandler {
            called: Arc::new(AtomicBool::new(false)),
            body_len: Arc::new(AtomicU64::new(u64::MAX)),
        });
        net.fetch(0, Request::get(url("http://127.0.0.1:9/secret")), handler);

        assert_eq!(sink.denied_urls(), vec!["http://127.0.0.1:9/secret"]);
        assert!(sink.fetched_urls().is_empty());
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

    /// Serves a page that says who asked and echoes the cookie it saw, with
    /// whatever CORS headers the test wants.
    fn cors_server(
        allow: Option<&'static str>,
        allow_credentials: bool,
        hits: usize,
    ) -> (u16, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
        let record = seen.clone();
        std::thread::spawn(move || {
            for _ in 0..hits {
                let Ok((stream, _)) = listener.accept() else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                let _ = reader.read_line(&mut line);
                let method = line.split_whitespace().next().unwrap_or("").to_string();
                let mut origin = String::new();
                let mut cookie = String::new();
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0
                        || header.trim().is_empty()
                    {
                        break;
                    }
                    let lower = header.to_ascii_lowercase();
                    if let Some(rest) = lower.strip_prefix("origin:") {
                        origin = rest.trim().to_string();
                    }
                    if let Some(rest) = lower.strip_prefix("cookie:") {
                        cookie = rest.trim().to_string();
                    }
                }
                record
                    .lock()
                    .unwrap()
                    .push(format!("{method} origin={origin} cookie={cookie}"));

                let body = "SECRET-BODY";
                let mut head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\
                     X-Private: nope\r\nConnection: close\r\n",
                    body.len()
                );
                if let Some(allow) = allow {
                    head.push_str(&format!("Access-Control-Allow-Origin: {allow}\r\n"));
                    head.push_str("Access-Control-Allow-Methods: DELETE\r\n");
                    head.push_str("Access-Control-Allow-Headers: x-token\r\n");
                }
                if allow_credentials {
                    head.push_str("Access-Control-Allow-Credentials: true\r\n");
                }
                let mut stream = stream;
                let _ = write!(stream, "{head}\r\n{body}");
                let _ = stream.flush();
            }
        });
        (port, seen)
    }

    fn cors_broker() -> (Arc<Broker>, Arc<MemorySink>) {
        let sink = Arc::new(MemorySink::new());
        let broker = Arc::new(
            Broker::new(Policy::new(), sink.clone(), None).expect("broker"),
        );
        (broker, sink)
    }

    /// The hole. Loopback is reachable by default (it is the dev server), so
    /// two pages on it are two *origins* — different ports — that the allowlist
    /// both permits. Before the same-origin policy, a script on one could read
    /// the other's body.
    #[test]
    fn a_cross_origin_read_is_refused_unless_the_server_allows_it() {
        let (port, seen) = cors_server(None, false, 1);
        let (broker, _sink) = cors_broker();
        let document = Url::parse("http://127.0.0.1:1/page").unwrap();
        let target = Url::parse(&format!("http://127.0.0.1:{port}/secret")).unwrap();

        let outcome = broker.send_script(
            &target,
            "GET",
            &[],
            None,
            &document,
            &[],
            crate::cors::Mode::Cors,
            crate::cors::Credentials::default(),
        );

        assert!(
            outcome.error.is_some(),
            "a cross-origin body must not be handed over: {outcome:?}"
        );
        assert!(
            outcome.error.as_deref().unwrap().contains("same-origin policy"),
            "{outcome:?}"
        );
        assert!(
            outcome.body.is_empty(),
            "the body must not even be read: {outcome:?}"
        );
        // The request *was* made and announced itself, which is what lets a
        // server answer the question at all.
        let seen = seen.lock().unwrap();
        assert!(seen[0].contains("origin=http://127.0.0.1:1"), "{seen:?}");
    }

    /// And the other half: a server that names us back gets read.
    #[test]
    fn a_cross_origin_read_the_server_allows_goes_through() {
        let (port, _seen) = cors_server(Some("*"), false, 1);
        let (broker, _sink) = cors_broker();
        let document = Url::parse("http://127.0.0.1:1/page").unwrap();
        let target = Url::parse(&format!("http://127.0.0.1:{port}/open")).unwrap();

        let outcome = broker.send_script(
            &target,
            "GET",
            &[],
            None,
            &document,
            &[],
            crate::cors::Mode::Cors,
            crate::cors::Credentials::default(),
        );
        assert!(outcome.error.is_none(), "{outcome:?}");
        assert_eq!(String::from_utf8_lossy(&outcome.body), "SECRET-BODY");
        // Headers are filtered to the safelist: the server exposed nothing.
        assert!(
            !outcome.headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("x-private")),
            "an unexposed header leaked: {:?}",
            outcome.headers
        );
        assert!(
            outcome.headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("content-type")),
            "the safelist should still be visible: {:?}",
            outcome.headers
        );
    }

    /// The consequence of ROADMAP §B16's cookie work, and the reason this
    /// module was written before any further capability: with `Domain` cookies
    /// and no same-origin policy, a cross-origin read is an *authenticated*
    /// one. The default credentials mode is what stops it.
    #[test]
    fn a_cross_origin_request_does_not_carry_the_session_by_default() {
        let (port, seen) = cors_server(Some("*"), false, 1);
        let (broker, _sink) = cors_broker();
        let target = Url::parse(&format!("http://127.0.0.1:{port}/x")).unwrap();
        // A session cookie for the target origin, as a login would have left.
        broker.jar().store(&target, ["sid=s3cr3t; Path=/"]);

        let document = Url::parse("http://127.0.0.1:1/page").unwrap();
        let outcome = broker.send_script(
            &target,
            "GET",
            &[],
            None,
            &document,
            &[],
            crate::cors::Mode::Cors,
            crate::cors::Credentials::default(),
        );
        assert!(outcome.error.is_none(), "{outcome:?}");

        let seen = seen.lock().unwrap();
        assert!(
            seen[0].contains("cookie="),
            "the server should have been asked: {seen:?}"
        );
        assert!(
            !seen[0].contains("s3cr3t"),
            "a cross-origin fetch must not carry the session by default: {seen:?}"
        );
    }

    /// A same-origin request is unaffected by any of it, which is what keeps
    /// ordinary pages working.
    #[test]
    fn a_same_origin_request_still_carries_its_session_and_reads_everything() {
        let (port, seen) = cors_server(None, false, 1);
        let (broker, _sink) = cors_broker();
        let target = Url::parse(&format!("http://127.0.0.1:{port}/api")).unwrap();
        broker.jar().store(&target, ["sid=s3cr3t; Path=/"]);

        let document = Url::parse(&format!("http://127.0.0.1:{port}/page")).unwrap();
        let outcome = broker.send_script(
            &target,
            "GET",
            &[],
            None,
            &document,
            &[],
            crate::cors::Mode::Cors,
            crate::cors::Credentials::default(),
        );
        assert!(outcome.error.is_none(), "{outcome:?}");
        assert_eq!(String::from_utf8_lossy(&outcome.body), "SECRET-BODY");
        assert!(
            outcome.headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("x-private")),
            "same-origin sees every header: {:?}",
            outcome.headers
        );

        let seen = seen.lock().unwrap();
        assert!(seen[0].contains("s3cr3t"), "{seen:?}");
        // No `Origin` header at all: that is how a server tells a same-origin
        // request from a cross-origin one, and sending one would ask a
        // question that has no business being asked.
        assert!(
            !seen[0].contains("origin=http"),
            "same-origin must send no Origin header: {seen:?}"
        );
    }

    /// A non-simple request asks first, and the preflight is a real request:
    /// policy-checked and receipted, so it appears in the log rather than
    /// arriving from nowhere.
    #[test]
    fn a_non_simple_request_preflights_and_the_preflight_is_receipted() {
        // Two hits: the OPTIONS, then the DELETE.
        let (port, seen) = cors_server(Some("*"), false, 2);
        let (broker, sink) = cors_broker();
        let document = Url::parse("http://127.0.0.1:1/page").unwrap();
        let target = Url::parse(&format!("http://127.0.0.1:{port}/item")).unwrap();

        let outcome = broker.send_script(
            &target,
            "DELETE",
            &[],
            None,
            &document,
            &[("x-token".to_string(), "abc".to_string())],
            crate::cors::Mode::Cors,
            crate::cors::Credentials::default(),
        );
        assert!(outcome.error.is_none(), "{outcome:?}");

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "one preflight, one request: {seen:?}");
        assert!(seen[0].starts_with("OPTIONS"), "{seen:?}");
        assert!(seen[1].starts_with("DELETE"), "{seen:?}");

        // And both are in the log. A preflight that did not appear would be a
        // request this engine made and did not record.
        let options = sink
            .records()
            .into_iter()
            .filter(|r| r.method == "OPTIONS")
            .count();
        assert!(options >= 1, "the preflight must be receipted");
    }

    /// A server that refuses at preflight time refuses before the real
    /// request is made, which is the round trip a preflight buys.
    #[test]
    fn a_refused_preflight_stops_the_request_before_it_is_made() {
        let (port, seen) = cors_server(None, false, 1);
        let (broker, _sink) = cors_broker();
        let document = Url::parse("http://127.0.0.1:1/page").unwrap();
        let target = Url::parse(&format!("http://127.0.0.1:{port}/item")).unwrap();

        let outcome = broker.send_script(
            &target,
            "DELETE",
            &[],
            None,
            &document,
            &[],
            crate::cors::Mode::Cors,
            crate::cors::Credentials::default(),
        );
        assert!(outcome.error.is_some(), "{outcome:?}");

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "only the preflight was sent: {seen:?}");
        assert!(seen[0].starts_with("OPTIONS"), "{seen:?}");
    }

    /// A request the *agent* made is unrestricted, which is what keeps
    /// `navigate` and the read verbs working exactly as they did.
    #[test]
    fn an_agent_request_is_not_subject_to_the_same_origin_policy() {
        let (port, _seen) = cors_server(None, false, 1);
        let (broker, _sink) = cors_broker();
        let target = Url::parse(&format!("http://127.0.0.1:{port}/anything")).unwrap();

        let outcome = broker.fetch_from(&target, Initiator::Navigation, None);
        assert!(outcome.error.is_none(), "{outcome:?}");
        assert_eq!(String::from_utf8_lossy(&outcome.body), "SECRET-BODY");
    }

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
