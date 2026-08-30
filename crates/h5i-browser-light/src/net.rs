//! The broker: the only way bytes enter this engine.
//!
//! Every fetch — the page itself, every stylesheet, every image, every
//! redirect hop — goes through [`crate::broker::Broker::send`], which does the same three
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

/// What this engine will accept compressed, and can decode.
///
/// Kept beside the decoder deliberately: a header that advertises an encoding
/// `decode_capped` does not handle is a promise the engine cannot keep, and the
/// failure would be a page of binary rather than an error.
const ACCEPT_ENCODING: &str = "gzip, br, deflate";

/// What this engine will take, by what asked for it.
///
/// Not cosmetic: crates.io answered **404** to a request with no `Accept` at
/// all, and the corpus recorded an empty page with no error. A server that
/// content-negotiates cannot serve a client that never says what it wants.
fn accept_for(initiator: Initiator) -> &'static str {
    match initiator {
        // A frame fetch is a document fetch: it negotiates like a navigation,
        // because the server on the other end is serving a page.
        Initiator::Navigation | Initiator::Redirect | Initiator::Frame => {
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"
        }
        Initiator::Subresource => "*/*",
    }
}


/// What a fetch produced. A denied or failed fetch is still an outcome, with
/// an empty body and a reason — never an absence.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
    /// Carried beside the message rather than inside it when this crosses a
    /// process boundary: a page's images are megabytes, and base64 in JSON
    /// would pay a third again for the privilege of being unreadable.
    #[serde(skip)]
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

pub struct LocalBroker {
    /// A handle to itself, for the two operations that hand out something with
    /// a life of its own.
    ///
    /// A WebSocket's reader thread receipts every frame for as long as the
    /// connection is open, so it holds the broker rather than borrowing it —
    /// and [`crate::broker::Broker::open_socket`] takes `&self`, because a
    /// trait method that took `Arc<Self>` could not be called through
    /// `dyn Broker`. `Arc::new_cyclic` closes that: the broker is built already
    /// knowing the handle it will be reached through.
    me: std::sync::Weak<LocalBroker>,
    policy: Policy,
    sink: Arc<dyn Sink>,
    /// Every record, kept in memory as well as sent to the sink.
    ///
    /// The engine's own copy, and the only one a renderer can ask for. It is
    /// here rather than threaded in beside the sink because the record-keeper
    /// and the thing being asked "what did you record" have to be the same
    /// component, or the answer is somebody's report about it.
    log: Arc<crate::receipt::MemorySink>,
    /// Credentials the agent may use and may not read.
    ///
    /// **Read here and nowhere else.** This is the process that holds
    /// `H5I_SECRET_*`; the renderer's environment is scrubbed of them, so a
    /// compromised parser reads the values that were substituted into a field
    /// it was told to fill and no others.
    ///
    /// Read once, when the broker is built, so a later `setenv` cannot widen
    /// what the session can reach. See [`crate::secrets`] for why the namespace
    /// is narrower than `H5I_*`.
    secrets: crate::secrets::Secrets,
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
    /// What this page may still spend on the network.
    ///
    /// Per navigation, reset by the factory when the agent moves. See
    /// [`crate::budget`] for why the ceiling bounds a page rather than a
    /// session: a loop is untrusted code the engine cannot otherwise stop, and
    /// an agent navigating is the principal exercising its own authority.
    budget: crate::budget::Budget,
    /// Addresses already approved, and the client's only source of them.
    ///
    /// `None` when an egress proxy is configured: the proxy resolves the name
    /// itself and this engine never sees an address, so pinning one would be a
    /// claim it cannot support. The proxy is the enforcement point there, which
    /// is the same division of labour the socket client already follows.
    pinned: Option<Arc<Pinned>>,
}

impl LocalBroker {
    /// Build a broker.
    ///
    /// `proxy` is h5i's egress proxy (`H5I_EGRESS_PROXY`). It is not required —
    /// the engine is useful on a bare host — but inside a box it is how the
    /// sandbox's own allowlist stays in the path. Loopback bypasses it, because
    /// the dev server is not egress.
    pub fn new(
        policy: Policy,
        sink: Arc<dyn Sink>,
        proxy: Option<&str>,
    ) -> Result<Arc<Self>, H5iError> {
        Self::build(
            policy,
            sink,
            proxy,
            crate::budget::Limits::default(),
            crate::secrets::Secrets::from_env(),
        )
    }

    /// The same, with the credentials named rather than read from the
    /// environment. For a caller that resolves them somewhere else, and for
    /// tests, which must not depend on what is exported around them.
    pub fn with_secrets(
        policy: Policy,
        sink: Arc<dyn Sink>,
        proxy: Option<&str>,
        secrets: crate::secrets::Secrets,
    ) -> Result<Arc<Self>, H5iError> {
        Self::build(policy, sink, proxy, crate::budget::Limits::default(), secrets)
    }

    /// The same, with the page ceiling the caller wants.
    ///
    /// Separate from [`Self::new`] because the limits have to be in place
    /// before the broker is shared: it is handed out as an `Arc` — a socket's
    /// reader thread holds one for the life of the connection — and there is no
    /// later moment at which it can be borrowed mutably.
    pub fn with_limits(
        policy: Policy,
        sink: Arc<dyn Sink>,
        proxy: Option<&str>,
        limits: crate::budget::Limits,
    ) -> Result<Arc<Self>, H5iError> {
        Self::build(policy, sink, proxy, limits, crate::secrets::Secrets::from_env())
    }

    fn build(
        policy: Policy,
        sink: Arc<dyn Sink>,
        proxy: Option<&str>,
        limits: crate::budget::Limits,
        secrets: crate::secrets::Secrets,
    ) -> Result<Arc<Self>, H5iError> {
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

        Ok(Arc::new_cyclic(|me| Self {
            me: me.clone(),
            policy,
            sink,
            log: Arc::new(crate::receipt::MemorySink::new()),
            secrets,
            client,
            budget: crate::budget::Budget::new(limits),
            pinned,
            seq: AtomicU64::new(0),
            jar: crate::cookies::Jar::new(),
            proxied,
        }))
    }

    /// This broker's own copy of the record, for the broker process's use.
    ///
    /// Not on [`crate::broker::Broker`], which offers `records()` — a reading,
    /// not the sink. Appending is not something a renderer gets to ask for:
    /// the whole claim is that the log is written by the component that made
    /// the decision.
    pub fn log(&self) -> &crate::receipt::MemorySink {
        &self.log
    }

    /// Append to the sink, and to this broker's own copy.
    ///
    /// One method rather than two calls at thirty sites, and the order matters:
    /// the sink is the one that can refuse, and a refusal is what stops the
    /// request. The in-memory copy never fails, so writing it first cannot
    /// change whether a fetch happens.
    fn append(&self, record: &RequestRecord) -> Result<(), H5iError> {
        let _ = self.log.append(record);
        self.sink.append(record)
    }

    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    /// What this page has spent, and what it may. The live object, for the
    /// broker's own use; [`crate::broker::Broker::budget`] hands callers a
    /// reading of it instead.
    pub fn spending(&self) -> &crate::budget::Budget {
        &self.budget
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
        if let Err(e) = self.append(&record) {
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
                let _ = self.append(&outcome);
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
        let _ = self.append(&outcome);

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

    /// The session's jar, for the broker's own use.
    ///
    /// Not on [`crate::broker::Broker`], and that is the point of §B18.6's
    /// hardest case: this returns a live reference, which cannot cross a
    /// process boundary and, in one process, put the session in reach of the
    /// parsers. Callers get three operations instead — `document_cookie`,
    /// `store_cookie`, `keep_only_origin` — each of which enforces `HttpOnly`
    /// and origin scoping on the way through.
    pub fn jar(&self) -> &crate::cookies::Jar {
        &self.jar
    }

    /// The four public entry points are [`crate::broker::Broker`]'s, and they
    /// all arrive here: check policy, record the decision, then use the wire,
    /// following redirects by hand so every hop is a decision of its own.
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

            // 1d. What this page has left to spend. Every limit before this
            //     one is *per request* — a size cap, a redirect count, a
            //     timeout — and none of them bounds a page that makes many.
            //     A refusal here is recorded like any other, because "the page
            //     ran out of allowance" is exactly what a reader of the log
            //     needs to see rather than a request that silently stopped.
            if let Err(over) = self.budget.claim_request() {
                let record = RequestRecord::request(seq, initiator, &method, current.as_str())
                    .denied(&over.0);
                if let Err(e) = self.record_pair(&record) {
                    return FetchOutcome::failed(current, format!("receipt sink refused: {e}"));
                }
                return FetchOutcome::failed_at(current, over.0, Some(seq));
            }

            // 2. The decision record, before any bytes move. If this cannot be
            //    written, the fetch does not happen — this is the fail-closed
            //    guarantee, and it is why `Sink::append` returns a Result.
            let record = RequestRecord::request(seq, initiator, &method, current.as_str());
            if let Err(e) = self.append(&record) {
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
                .header(reqwest::header::ACCEPT_LANGUAGE, ACCEPT_LANGUAGE)
                .header(reqwest::header::ACCEPT_ENCODING, ACCEPT_ENCODING);
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
                    let _ = self.append(&outcome_record);
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
                let _ = self.append(&outcome_record);

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
                    let _ = self.append(&refused);
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

            // What crossed the wire, before `reqwest` decodes it. Read from
            // the response's own headers rather than measured, because the
            // decoding happens inside the client and the compressed bytes are
            // gone by the time a body is in hand.
            //
            // `Content-Length` is the *compressed* length when the body is
            // encoded, which is exactly the number wanted here. Absent under
            // chunked transfer, and absent is the honest answer: recording the
            // decoded size under `wire_bytes` would be a guess wearing a
            // measurement's name.
            let encoding = headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("content-encoding"))
                .map(|(_, value)| value.trim().to_ascii_lowercase())
                .filter(|value| !value.is_empty() && value != "identity");

            // Read compressed, then decode. Both sizes are *measured* rather
            // than read off a header, so they are right under chunked transfer
            // and cannot be lied about by a `Content-Length` that disagrees
            // with the body.
            let body = self.read_capped(response).and_then(|raw| {
                let wire = raw.len() as u64;
                match &encoding {
                    None => Ok((raw, wire, false)),
                    Some(encoding) => self
                        .decode_capped(&raw, encoding)
                        .map(|decoded| (decoded, wire, true)),
                }
            });

            let mut outcome_record = record.response();
            outcome_record.status = Some(status.as_u16());
            outcome_record.duration_ms = Some(elapsed);
            outcome_record.cookies_sent = Some(cookies_sent);
            outcome_record.cookies_stored = Some(cookies_stored);
            let body = match body {
                Ok((decoded, wire, was_encoded)) => {
                    if was_encoded {
                        outcome_record.wire_bytes = Some(wire);
                        outcome_record.content_encoding = encoding.clone();
                    }
                    // What this one cost, against the page's allowance. Both
                    // sizes, because a compressed response costs the wire
                    // little and the page's memory a great deal.
                    self.budget.record(
                        wire,
                        decoded.len() as u64,
                        std::time::Duration::from_millis(elapsed),
                    );
                    Ok(decoded)
                }
                Err(e) => {
                    // A failed read still cost the time it took.
                    self.budget
                        .record(0, 0, std::time::Duration::from_millis(elapsed));
                    Err(e)
                }
            };

            return match body {
                Ok(body) => {
                    outcome_record.bytes = Some(body.len() as u64);
                    let _ = self.append(&outcome_record);
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
                    let _ = self.append(&outcome_record);
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
    /// Decode a compressed body, under the same cap the raw read was under.
    ///
    /// **The cap is the point, not the decoding.** A response small enough to
    /// pass `read_capped` can decompress into something enormous — a few
    /// kilobytes of zeroes is gigabytes of zeroes — and a browser that decoded
    /// without a limit would let any allowed origin exhaust the box's memory
    /// with one response. The limit is the same
    /// [`Policy::max_response_bytes`] the wire read uses, applied to what comes
    /// out rather than only to what went in.
    ///
    /// An encoding this engine does not have is an error rather than a body
    /// passed through undecoded: handing compressed bytes to the HTML parser
    /// would render a page of binary, which is a wrong answer that looks like a
    /// broken site.
    fn decode_capped(&self, raw: &[u8], encoding: &str) -> Result<Vec<u8>, H5iError> {
        use std::io::Read;
        let cap = self.policy.max_response_bytes();

        // Only the last encoding is handled, which is all any server sends.
        // A stacked `gzip, br` is refused by name rather than half-decoded.
        let name = encoding.rsplit(',').next().unwrap_or(encoding).trim();
        let mut out = Vec::new();
        let read = match name {
            "gzip" | "x-gzip" => flate2::read::GzDecoder::new(raw)
                .take(cap + 1)
                .read_to_end(&mut out),
            // `deflate` is specified as zlib and sent as raw by enough servers
            // that a browser has to try both. The bare form is the fallback,
            // which is what every other engine does here.
            "deflate" => flate2::read::ZlibDecoder::new(raw)
                .take(cap + 1)
                .read_to_end(&mut out)
                .or_else(|_| {
                    out.clear();
                    flate2::read::DeflateDecoder::new(raw)
                        .take(cap + 1)
                        .read_to_end(&mut out)
                }),
            "br" => brotli::Decompressor::new(raw, 4096)
                .take(cap + 1)
                .read_to_end(&mut out),
            other => {
                return Err(H5iError::Metadata(format!(
                    "the response is `{other}`-encoded, which this engine cannot decode. It \
                     asked for {ACCEPT_ENCODING}."
                )))
            }
        };
        read.map_err(|e| {
            H5iError::Metadata(format!("the {name} response could not be decoded: {e}"))
        })?;

        if out.len() as u64 > cap {
            return Err(H5iError::Metadata(format!(
                "the response decompresses past the {cap} byte cap, so it was not read. A \
                 small response that expands without limit is how a page exhausts the \
                 memory of whatever is reading it."
            )));
        }
        Ok(out)
    }

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
    /// The front half of [`crate::broker::Broker::send_from`] — policy, then the record,
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
        if let Err(e) = self.append(&record) {
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
        self.append(&record)?;
        let mut outcome = record.response();
        outcome.bytes = Some(bytes);
        // No status. 101 is the WebSocket upgrade's, and stamping it on every
        // frame said "switching protocols" four hundred times on one
        // connection — and said it on event streams, which never switched
        // anything. A frame is not an exchange with a status of its own.
        self.append(&outcome)
    }

    /// Authorise and begin an event stream, handing back the open response.
    ///
    /// The half that touches the wire. [`crate::broker::Broker::open_event_stream`]
    /// is the operation callers reach for; this is what it is built on.
    ///
    /// The second exit from the receipt path, and the reason it exists:
    /// [`crate::broker::Broker::send_from`] reads a whole body before it returns and writes
    /// one response record with a final byte count. An event stream never
    /// completes, so it would hit the response cap or the client timeout and be
    /// reported as an error.
    ///
    /// The front half is identical — policy, then the decision record, *then*
    /// the wire — because that is the half the fail-closed rule lives in, and
    /// two copies of it would be two rules.
    pub fn begin_event_stream(
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
        if let Err(e) = self.append(&record) {
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
                if let Err(e) = self.append(&outcome) {
                    return Err(format!(
                        "refusing to stream: the receipt could not be written: {e}"
                    ));
                }
                Ok(response)
            }
            Err(error) => {
                let mut outcome = record.response();
                outcome.error = Some(error.to_string());
                let _ = self.append(&outcome);
                Err(format!("could not open the event stream: {error}"))
            }
        }
    }

    /// Write both phases for a request that never reaches the wire.
    fn record_pair(&self, record: &RequestRecord) -> Result<(), H5iError> {
        self.append(record)?;
        self.append(&record.response())
    }
}

impl crate::broker::Broker for LocalBroker {
    fn send(&self, fetch: &crate::broker::Fetch) -> FetchOutcome {
        let context = fetch.cors.as_ref().map(|ask| CorsContext {
            document: crate::cors::Origin::of(&ask.document),
            headers: ask.headers.clone(),
            mode: ask.mode,
            credentials: ask.credentials,
        });
        self.send_with_cors(
            &fetch.url,
            fetch.initiator,
            &fetch.method,
            &fetch.body,
            fetch.content_type.as_deref(),
            fetch.document.as_ref(),
            context.as_ref(),
        )
    }

    fn records(&self) -> Vec<RequestRecord> {
        self.log.records()
    }

    fn budget(&self) -> crate::broker::Allowance {
        crate::broker::Allowance {
            spent: self.budget.spent(),
            limits: self.budget.limits().clone(),
        }
    }

    fn reset_budget(&self) {
        self.budget.reset();
    }

    fn cookie_count(&self) -> usize {
        self.jar.len()
    }

    fn document_cookie(&self, url: &Url) -> String {
        self.jar.document_cookie(url)
    }

    fn store_cookie(&self, url: &Url, header: &str) -> usize {
        self.jar.store_from_script(url, header)
    }

    fn keep_only_origin(&self, origin: &Url) -> bool {
        self.jar.retain_origin(origin)
    }

    fn open_socket(
        &self,
        url: &Url,
        document: Option<&Url>,
    ) -> Result<Arc<dyn crate::broker::Channel>, String> {
        let me = self.me.upgrade().ok_or("the broker is no longer running")?;
        Ok(Arc::new(crate::wsclient::Socket::open(me, url, document)?))
    }

    fn open_event_stream(
        &self,
        url: &Url,
        document: Option<&Url>,
    ) -> Result<Arc<dyn crate::broker::Channel>, String> {
        let me = self.me.upgrade().ok_or("the broker is no longer running")?;
        Ok(Arc::new(crate::sse::EventStream::open(me, url, document)?))
    }

    fn secret_names(&self) -> Vec<String> {
        self.secrets.names().into_iter().map(str::to_string).collect()
    }

    fn substitute(&self, text: &str) -> crate::secrets::Resolved {
        self.secrets.substitute(text)
    }

    fn redact(&self, text: &str) -> String {
        self.secrets.redact(text)
    }
}

/// Adapts the broker to Blitz's [`NetProvider`].
pub struct BrokerNet {
    broker: Arc<dyn crate::broker::Broker>,
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
    pub fn new(broker: Arc<dyn crate::broker::Broker>, document: Option<Url>) -> Self {
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
    use crate::broker::Broker;
    use crate::receipt::{MemorySink, Phase};
    use std::sync::atomic::AtomicBool;

    fn broker_with(policy: Policy, sink: Arc<dyn Sink>) -> Arc<LocalBroker> {
        LocalBroker::new(policy, sink, None).expect("broker builds")
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
        let result = LocalBroker::new(Policy::new(), sink, Some("not a url"));
        assert!(result.is_err(), "a malformed proxy must fail loudly, early");
    }
}

#[cfg(test)]
mod cookie_wire_tests {
    use super::*;
    use crate::broker::Broker;
    use crate::receipt::MemorySink;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    /// A server that answers anything, forever, so a runaway page has
    /// somewhere to run away to.
    fn always_answers(hits: usize) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for _ in 0..hits {
                let Ok((stream, _)) = listener.accept() else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                let _ = reader.read_line(&mut line);
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0
                        || header.trim().is_empty()
                    {
                        break;
                    }
                }
                let body = "ok";
                let mut stream = stream;
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.flush();
            }
        });
        port
    }

    /// The gap the budget fills. Every limit before it was *per request* — a
    /// size cap, a redirect count, a timeout — and none of them bounds a page
    /// that makes many. Recording a runaway is not the same as stopping one.
    #[test]
    fn a_page_that_keeps_asking_is_eventually_refused() {
        let port = always_answers(20);
        let sink = Arc::new(MemorySink::new());
        let broker = LocalBroker::with_limits(
            Policy::new(),
            sink.clone(),
            None,
            crate::budget::Limits {
                max_requests: 3,
                ..Default::default()
            },
        )
        .expect("broker");
        let url = Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();

        for at in 1..=3 {
            let outcome = broker.fetch_from(&url, Initiator::Subresource, None);
            assert!(outcome.error.is_none(), "request {at}: {outcome:?}");
        }
        let refused = broker.fetch_from(&url, Initiator::Subresource, None);
        let why = refused.error.expect("the fourth is over budget");
        assert!(why.contains("budget-exceeded"), "{why}");

        // And it is *recorded* as a denial, because "the page ran out of
        // allowance" is exactly what a reader of the log needs to see.
        let denied = sink
            .records()
            .into_iter()
            .filter(|r| !r.allowed)
            .filter(|r| {
                r.denied_reason
                    .as_deref()
                    .is_some_and(|why| why.contains("budget-exceeded"))
            })
            .count();
        assert!(denied >= 1, "the refusal must be in the log");
    }

    /// A fresh page is a fresh decision by the agent, so it gets a fresh
    /// allowance. The budget bounds untrusted page code, not the principal.
    #[test]
    fn navigating_gives_the_next_page_its_own_allowance() {
        let port = always_answers(20);
        let sink = Arc::new(MemorySink::new());
        let broker = LocalBroker::with_limits(
            Policy::new(),
            sink,
            None,
            crate::budget::Limits {
                max_requests: 2,
                ..Default::default()
            },
        )
        .expect("broker");
        let url = Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();

        for _ in 0..2 {
            assert!(broker.fetch_from(&url, Initiator::Subresource, None).error.is_none());
        }
        assert!(broker.fetch_from(&url, Initiator::Subresource, None).error.is_some());

        broker.reset_budget();
        assert!(
            broker.fetch_from(&url, Initiator::Subresource, None).error.is_none(),
            "a navigation restores the allowance"
        );
    }

    /// A server that compresses when asked, and reports what it was asked for.
    fn gzip_server(body: &'static [u8], hits: usize) -> (u16, Arc<std::sync::Mutex<Vec<String>>>) {
        use flate2::write::GzEncoder;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Arc<std::sync::Mutex<Vec<String>>> = Default::default();
        let record = seen.clone();
        std::thread::spawn(move || {
            for _ in 0..hits {
                let Ok((stream, _)) = listener.accept() else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                let _ = reader.read_line(&mut line);
                let mut accept = String::new();
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0
                        || header.trim().is_empty()
                    {
                        break;
                    }
                    let lower = header.to_ascii_lowercase();
                    if let Some(rest) = lower.strip_prefix("accept-encoding:") {
                        accept = rest.trim().to_string();
                    }
                }
                record.lock().unwrap().push(accept.clone());

                let mut encoder = GzEncoder::new(Vec::new(), flate2::Compression::default());
                encoder.write_all(body).unwrap();
                let payload = encoder.finish().unwrap();
                let mut stream = stream;
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\
                     Content-Encoding: gzip\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n",
                    payload.len()
                );
                let _ = stream.write_all(&payload);
                let _ = stream.flush();
            }
        });
        (port, seen)
    }

    /// The capability that was absent, and the measurement that goes with it.
    ///
    /// Both sizes are *measured* rather than read off a header, which is why
    /// this engine decodes its own bodies: `reqwest` will do it transparently
    /// and strips `Content-Encoding` and `Content-Length` on the way, so the
    /// number that says what the request actually cost is gone before anything
    /// can record it.
    #[test]
    fn a_compressed_response_is_decoded_and_both_sizes_are_recorded() {
        const BODY: &[u8] = b"<html><body>compressible filler compressible filler                               compressible filler compressible filler</body></html>";
        let (port, seen) = gzip_server(BODY, 1);
        let sink = Arc::new(MemorySink::new());
        let broker =
            LocalBroker::new(Policy::new(), sink.clone(), None).expect("broker");
        let url = Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();

        let outcome = broker.fetch_from(&url, Initiator::Navigation, None);
        assert!(outcome.error.is_none(), "{outcome:?}");
        assert_eq!(outcome.body, BODY, "the body must arrive decoded");

        // The engine asked for what it can decode, and nothing else.
        assert_eq!(seen.lock().unwrap()[0], "gzip, br, deflate");

        let response = sink
            .records()
            .into_iter()
            .find(|r| r.phase == crate::receipt::Phase::Response)
            .expect("a response record");
        assert_eq!(response.bytes, Some(BODY.len() as u64), "what the page got");
        let wire = response.wire_bytes.expect("what the wire carried");
        assert!(
            wire < BODY.len() as u64,
            "the compressed size should be smaller: {wire} vs {}",
            BODY.len()
        );
        assert_eq!(response.content_encoding.as_deref(), Some("gzip"));

        // And the line a person reads carries both, because "184 KB" and
        // "43 KB on the wire" answer different questions.
        let line = response.render();
        assert!(line.contains("on the wire"), "{line}");
        assert!(line.contains("gzip"), "{line}");
    }

    /// The reason the decoding is capped as well as the reading. A few
    /// kilobytes of zeroes is gigabytes of zeroes, and a browser that decoded
    /// without a limit would let any allowed origin exhaust the box's memory
    /// with one response.
    #[test]
    fn a_response_that_decompresses_past_the_cap_is_refused() {
        let sink = Arc::new(MemorySink::new());
        let broker = LocalBroker::new(
            Policy::new().set_max_response_bytes(64 * 1024),
            sink,
            None,
        )
        .expect("broker");

        // 4 MiB of zeroes compresses to a few kilobytes: small enough to pass
        // the wire cap, far past the decoded one.
        let bomb = vec![0u8; 4 * 1024 * 1024];
        let mut encoder =
            flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        encoder.write_all(&bomb).unwrap();
        let compressed = encoder.finish().unwrap();
        assert!(compressed.len() < 64 * 1024, "the bomb must pass the wire cap");

        let refused = broker.decode_capped(&compressed, "gzip");
        let why = refused.expect_err("a decompression bomb must be refused");
        assert!(
            why.to_string().contains("decompresses past"),
            "the refusal should name what happened: {why}"
        );
    }

    /// An encoding this engine cannot decode is an error, not a body passed
    /// through: handing compressed bytes to the HTML parser would render a page
    /// of binary, which is a wrong answer that looks like a broken site.
    #[test]
    fn an_encoding_this_engine_did_not_ask_for_is_an_error() {
        let sink = Arc::new(MemorySink::new());
        let broker = LocalBroker::new(Policy::new(), sink, None).expect("broker");
        let why = broker
            .decode_capped(b"whatever", "exotic-zip")
            .expect_err("an unknown encoding is an error");
        assert!(why.to_string().contains("exotic-zip"), "{why}");
    }

    #[test]
    fn every_encoding_this_engine_advertises_round_trips() {
        let sink = Arc::new(MemorySink::new());
        let broker = LocalBroker::new(Policy::new(), sink, None).expect("broker");
        let body = b"round trip me".repeat(50);

        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&body).unwrap();
        assert_eq!(broker.decode_capped(&gz.finish().unwrap(), "gzip").unwrap(), body);

        let mut zl = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        zl.write_all(&body).unwrap();
        assert_eq!(
            broker.decode_capped(&zl.finish().unwrap(), "deflate").unwrap(),
            body
        );

        // Raw deflate too: the spec says zlib and enough servers send bare that
        // a browser has to try both.
        let mut raw = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        raw.write_all(&body).unwrap();
        assert_eq!(
            broker.decode_capped(&raw.finish().unwrap(), "deflate").unwrap(),
            body
        );

        let mut br = Vec::new();
        {
            let mut writer = brotli::CompressorWriter::new(&mut br, 4096, 5, 22);
            writer.write_all(&body).unwrap();
        }
        assert_eq!(broker.decode_capped(&br, "br").unwrap(), body);
    }

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

    fn cors_broker() -> (Arc<LocalBroker>, Arc<MemorySink>) {
        let sink = Arc::new(MemorySink::new());
        let broker = LocalBroker::new(Policy::new(), sink.clone(), None).expect("broker");
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

    /// The consequence of roadmap-history.md §B16's cookie work, and the reason this
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
            let broker = LocalBroker::new(Policy::new(), Arc::new(MemorySink::new()), None).unwrap();
            let target = Url::parse(&format!("http://127.0.0.1:{port}/login")).unwrap();

            let outcome = broker.send_from(
                &target,
                Initiator::Navigation,
                "POST",
                b"password=hunter2",
                Some("application/x-www-form-urlencoded"),
                None,
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
        let broker = LocalBroker::new(Policy::new(), Arc::new(MemorySink::new()), None).unwrap();
        let target = Url::parse(&format!("http://127.0.0.1:{port}/login")).unwrap();

        broker.send_from(
            &target,
            Initiator::Navigation,
            "POST",
            b"password=hunter2",
            Some("application/x-www-form-urlencoded"),
            None,
        );

        let followed = server.join().unwrap();
        assert_eq!(followed, "POST body=password=hunter2", "got {followed:?}");
    }

    #[test]
    fn a_session_survives_between_requests_and_never_reaches_the_log() {
        let (port, server) = login_server();
        let sink = Arc::new(MemorySink::new());
        let broker = LocalBroker::new(Policy::new(), sink.clone(), None).unwrap();

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
