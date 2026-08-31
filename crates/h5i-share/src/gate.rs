//! The HTTP gate: reading a share credential off a request, and making sure it
//! never travels any further.
//!
//! Two surfaces speak HTTP to a browser, the quick tunnel on the sharer's side
//! ([`crate::tunnel`]) and the loopback proxy on the joiner's side
//! ([`crate::join`]), and both need the same three things:
//!
//! 1. Find the credential. The first request carries it in the query string,
//!    because a URL is the only thing you can hand someone who has no h5i.
//!    Every request after that carries it in a cookie.
//! 2. Get it out of the address bar. A token in the URL leaks into `Referer`
//!    on every outbound link, into screen shares, and into whatever the shared
//!    app logs. So the first request is answered with a redirect that sets the
//!    cookie and points at the same page without the token.
//! 3. Never let it reach the box. The shared app is agent-written code we are
//!    deliberately exposing to someone; handing it the credential that
//!    authorizes the share would be handing it the share.
//!
//! Parsing is deliberately small and refuses rather than repairs. Everything
//! here arrives from the open internet in tunnel mode.

/// Where the credential travels between requests.
///
/// A *prefix*, not the whole name: the loopback proxy on the joining side
/// appends its port. Cookies are scoped to a host and ignore the port, so two
/// `h5i join` sessions on one machine would otherwise set the same cookie on
/// `127.0.0.1` and quietly log each other out, and every request the browser
/// made to any other local service would carry the token into that service's
/// logs. Naming it per port fixes the first outright and narrows the second.
pub const COOKIE: &str = "h5i_share";

/// Is this cookie name one of h5i's own?
///
/// The two names that exist are the bare [`COOKIE`] and `h5i_share_<port>`, and
/// both sanitisers were testing `starts_with("h5i_share")`, a different set. An
/// app cookie called `h5i_shared_theme` matched, so it was deleted from every
/// request on the way into the box and its `Set-Cookie` deleted from every
/// response on the way out. The app then loses its state or its login *only when
/// viewed through a share*, contradicting the contract stated above
/// [`split_cookie`].
///
/// One predicate, used in both directions and by the fuzz invariant, so the
/// three cannot drift.
pub fn is_share_cookie_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(COOKIE) else {
        return false;
    };
    if rest.is_empty() {
        return true;
    }
    // `h5i_share_<port>`, and a port is a nonzero `u16`. Anything else that
    // merely begins with these characters belongs to the app.
    rest.strip_prefix('_')
        .and_then(|p| p.parse::<u16>().ok())
        .is_some_and(|p| p != 0)
}

/// The cookie name a loopback proxy on `port` should use.
pub fn cookie_for_port(port: u16) -> String {
    format!("{COOKIE}_{port}")
}

/// How many names one box may teach this. A dev server has a handful; a box
/// that emits a new cookie name per response is not a dev server, and this is
/// the only structure here that a box could otherwise grow without limit.
const MAX_APP_COOKIES: usize = 64;

/// The cookie names the box has set for itself, learned from its own
/// `Set-Cookie` headers as its responses go past.
///
/// A cookie jar is scoped by host and ignores the port, so a proxy on
/// `127.0.0.1` receives every cookie every *other* local service on this machine
/// has set, and forwards them, because a share that logs the visitor out of the
/// app being demonstrated is a broken share. On a jar of this share's own that
/// rule costs nothing. On the shared `127.0.0.1` jar it means the joiner's own
/// `session=<secret>`, from their own local app, arrives at agent-written code
/// inside somebody else's box on the first request, and the joiner is the person
/// who did not choose that risk.
///
/// So on that jar, and only there, the rule is narrowed: a cookie goes upstream
/// only if the box on the other end set it.
///
/// What it costs: a cookie written by `document.cookie` never appears in a
/// `Set-Cookie` this proxy can see, so the box does not get those back. A real
/// loss of fidelity, said out loud at join time rather than discovered, and the
/// safe direction to be wrong in, since an unlearned name is dropped rather than
/// forwarded.
///
/// Name *and* value, not name alone. Two cookies may share a name in one jar
/// when their paths differ, and the browser then sends both segments in one
/// `Cookie` header. Matching on the name alone forwarded both, so a box that set
/// `sid` at some deep path, having guessed a name the joiner's own service uses
/// at `/`, was handed the joiner's `sid` beside its own on every request under
/// that path. Guessing is cheap. The value closes it, because the one thing a
/// box cannot do is set a cookie to a secret it has not been told.
#[derive(Default, Debug)]
pub struct AppCookies {
    /// `name=value`, exactly as the box set it and exactly as the browser will
    /// send it back.
    pairs: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl AppCookies {
    /// Remember a name the box set. Ours are never learned: they are stripped
    /// from the box's responses on the way out, so a box cannot talk its own
    /// name onto the list and get the share credential forwarded back to it.
    pub fn learn(&self, name: &str, value: &str) {
        if name.is_empty() || is_share_cookie_name(name) {
            return;
        }
        let pair = format!("{name}={value}");
        let Ok(mut pairs) = self.pairs.lock() else {
            return;
        };
        if pairs.len() >= MAX_APP_COOKIES && !pairs.contains(&pair) {
            return;
        }
        pairs.insert(pair);
    }

    /// Did the box set this cookie, to this value? Cookie names and values are
    /// both case-sensitive, so this is an exact match on both.
    ///
    /// A poisoned lock answers "no": the failure mode is an app that loses its
    /// cookies, not a jar that goes upstream unfiltered.
    pub fn knows(&self, name: &str, value: &str) -> bool {
        let pair = format!("{name}={value}");
        self.pairs
            .lock()
            .map(|p| p.contains(&pair))
            .unwrap_or(false)
    }
}
/// Where it travels on the first request, in the URL a human was sent.
pub const QUERY_PARAM: &str = "h5i";
/// Headers past this are refused. Generous for cookies, far below anything a
/// browser sends.
pub const MAX_HEAD: usize = 32 * 1024;

/// What the gate needs from a request line and its headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    /// The request target exactly as sent.
    pub target: String,
    /// The target with the share parameter removed. Where a browser should be
    /// sent once the cookie is set.
    pub clean_target: String,
    /// True when the credential came from the URL, which is what makes this
    /// request a redirect rather than a proxy.
    pub from_query: bool,
    /// The credential, from the query if present, otherwise from the cookie.
    pub token: Option<String>,
    /// A genuine upgrade: an `Upgrade` header *and* a `Connection` header
    /// that lists `upgrade`. Both are required, because this flag is what opts
    /// a connection out of the one-request rule, and a lone `Upgrade:` header
    /// is something any client can send on an ordinary request.
    pub upgrade: bool,
    /// How long this request's body is. `None` means there is none.
    pub content_length: Option<u64>,
    /// The body's length is not something this proxy can know in advance.
    pub chunked: bool,
    /// The client said `Expect: 100-continue` and is waiting for permission.
    pub expects_continue: bool,
    /// The browser is asking to register a service worker. See
    /// [`registers_a_service_worker`].
    pub service_worker: bool,
    /// The request carries an `Origin` that is not this share's own. See
    /// [`is_cross_origin`].
    pub cross_origin: bool,
}

/// Does this request come from a page that is not this share?
///
/// Two `h5i join` proxies on one machine are `127.0.0.1:A` and `127.0.0.1:B`:
/// different *origins* but the *same site*, so `SameSite=Lax` does not hold a
/// cookie back between them, and a page served by one share could make
/// credentialed requests to another colleague's box on the next port. CORS stops
/// the page reading the answers; it does not stop the side effects, and a demo
/// of somebody's agent-written app is exactly where such a request would come
/// from. The console's own gate (`h5i_core::server::authorize`) has had this
/// check since it was written; this one never got it.
///
/// Compared by host and port only, not by scheme: a tunnel share arrives with
/// `Origin: https://…trycloudflare.com` and `Host: …trycloudflare.com`, and
/// requiring the schemes to agree would refuse the app's own requests. An absent
/// `Origin` is allowed, being an ordinary navigation, which is how every visitor
/// arrives.
///
/// Every value of these headers is read, not the first, the same rule the
/// framing checks follow: a disagreement about *where a request came from* is
/// resolved against the request. `header()` was used here and `headers_named()`
/// everywhere else, so a head carrying `Sec-Fetch-Site: same-origin` followed by
/// anything at all was read as the share's own page. Found by the fuzzer at
/// round 9,931. No browser sends two, and `Sec-Fetch-Site` is a forbidden header
/// name so no page can add one; what makes it worth closing is that
/// `cloudflared` bridges HTTP/2 to HTTP/1.1 and a repeated h2 field arrives here
/// as two lines.
fn is_cross_origin(headers: &[&str], host: Option<&str>, carries_invite: bool) -> bool {
    // `Sec-Fetch-Site` first, because `Origin` is not sent at all on a
    // subresource GET, which is the shape the check was written for and did
    // not stop. `<img src="http://127.0.0.1:8899/api/reset">` on a page served
    // by the share next door puts the credential on the wire with no `Origin`
    // header anywhere, and it was served. Every browser that has service
    // workers has this header too, so the two arrived together.
    let sites = headers_named(headers, "sec-fetch-site");
    if !sites.is_empty() {
        // Foreign if *any* reading of the head says so. One value is the
        // ordinary case and answers for itself; two that disagree are a head
        // no browser produced.
        return sites.iter().any(|site| match site.trim() {
            // The share's own page, or a user-initiated load: typed, clicked
            // from a bookmark, or opened from the address bar.
            "same-origin" | "none" => false,
            // A navigation from *another site* is how every visitor arrives: the
            // invite is followed from a chat window, and refusing that refuses
            // everybody at the front door.
            //
            // But only the invite. `SameSite=Lax` is precisely the rule that attaches a
            // cookie to a cross-site top-level GET navigation, so once a visitor has the
            // share cookie this carve-out let any site on the internet that learns the
            // origin navigate them to `…/reset` or `…/delete?id=1` and have it
            // authorized: ordinary CSRF, wearing the exception written for the chat
            // window.
            //
            // The distinction the exception needs is not "is this a navigation" but "did
            // this request bring the capability with it". A link out of a chat window
            // carries the token in its query string; the attacker's link cannot, because
            // the token is the one thing they do not have.
            "cross-site" => !(is_a_navigation(headers) && carries_invite),
            // But a navigation from the same *site* is not that. Two `h5i join`
            // proxies on one machine are `127.0.0.1:A` and `127.0.0.1:B`, different
            // origins and the same site because a site does not include the port, and
            // `SameSite=Lax` constrains only cross-site requests, so a same-site POST
            // is not constrained at all. A form on share A targeting share B arrived
            // as `same-site` + `navigate` + `document`, which the carve-out let
            // through with B's cookie attached. The invite link is never same-site.
            _ => true,
        });
    }
    // Same rule for `Origin`, and the same reason: all of them, and foreign if
    // any of them is.
    headers_named(headers, "origin").iter().any(|origin| {
        // `null` is what a sandboxed context sends. It cannot be this share,
        // and a page that wants to demonstrate itself has no reason to send it.
        let Some(rest) = origin.split_once("://").map(|(_, r)| r) else {
            return true;
        };
        let origin_host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
        match host {
            Some(h) => !origin_host.eq_ignore_ascii_case(h.trim()),
            // No `Host` to compare against is itself a request no browser
            // makes.
            None => true,
        }
    })
}

/// A top-level navigation, as the fetch metadata describes it.
///
/// Both halves are required. `Sec-Fetch-Mode: navigate` alone is also sent for
/// an `<iframe>` and for a prefetch; pairing it with `Sec-Fetch-Dest: document`
/// narrows it to the address bar. An `<iframe>` of this share from another page
/// is `dest: iframe` and is refused, since a framed share is not something a
/// visitor can see the address of.
///
/// Both halves are also required to be *unambiguous*: this is the one thing that
/// opens the cross-site carve-out, so a head that says two things about its own
/// shape does not get to pick the generous one.
fn is_a_navigation(headers: &[&str]) -> bool {
    let all = |name: &str, want: &str| {
        let vs = headers_named(headers, name);
        vs.len() == 1 && vs[0].trim() == want
    };
    all("sec-fetch-mode", "navigate") && all("sec-fetch-dest", "document")
}

/// Is this request trying to register a service worker?
///
/// Browsers treat `http://127.0.0.1:<port>` as a *potentially trustworthy*
/// origin, so a page served over the joiner's loopback proxy is a secure context
/// and may call `navigator.serviceWorker.register()`. What it registers outlives
/// `h5i join`: after Ctrl-C the origin is still controlled, and if the joiner
/// later runs their own dev server on that port the worker intercepts every
/// fetch of their app.
///
/// The joiner is the person who did not choose this risk. A demo does not need a
/// service worker, so the registration request is refused by name; browsers send
/// `Service-Worker: script` on exactly that fetch and on nothing else. Any copy
/// of the header, for the reason `is_cross_origin` reads all of its own.
fn registers_a_service_worker(headers: &[&str]) -> bool {
    headers_named(headers, "service-worker")
        .iter()
        .any(|v| v.eq_ignore_ascii_case("script"))
}

/// Why a request was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// No credential at all, or one that no grant matches.
    NotAuthorized,
    /// The request was not something we are willing to parse.
    Malformed,
    /// A service worker registration, which would outlive the share.
    ServiceWorker,
    /// The request came from a page that is not this share.
    ForeignOrigin,
}

impl Refusal {
    pub fn status(self) -> (u16, &'static str) {
        match self {
            Refusal::NotAuthorized => (401, "Unauthorized"),
            Refusal::Malformed => (400, "Bad Request"),
            Refusal::ServiceWorker => (403, "Forbidden"),
            Refusal::ForeignOrigin => (403, "Forbidden"),
        }
    }

    /// One body for every refusal a peer can trigger.
    ///
    /// Deliberately the same text whether the ticket is unknown, expired or
    /// revoked. Whoever is probing a tunnel URL learns nothing from it; the
    /// sharer's terminal and the receipt get the real reason, which is where a
    /// distinction is worth something.
    pub fn body(self) -> &'static str {
        match self {
            Refusal::NotAuthorized => {
                "This h5i share needs a valid invite link. Ask whoever shared it for a new one."
            }
            Refusal::Malformed => "That is not a request this share can serve.",
            Refusal::ServiceWorker => {
                "This share will not register a service worker. One would keep control of \
                 this address after the share ends."
            }
            Refusal::ForeignOrigin => "That request came from another page, not from this share.",
        }
    }
}

/// Split a head into its request line and header lines. `None` for anything
/// that is not a well-formed HTTP/1 head.
fn lines(head: &str) -> Option<(&str, Vec<&str>)> {
    let mut it = head.split("\r\n");
    let request_line = it.next()?;
    Some((request_line, it.filter(|l| !l.is_empty()).collect()))
}

/// Find a header's value, case-insensitively.
fn header<'a>(headers: &[&'a str], name: &str) -> Option<&'a str> {
    headers.iter().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

/// Is this a credential we are willing to compare at all?
///
/// A share secret is fixed-width hex. Refusing anything else here means the
/// authorization path never sees a megabyte-long "token", and a cookie full of
/// junk is a miss rather than an argument about encodings.
fn plausible(token: &str) -> bool {
    !token.is_empty() && token.len() <= 128 && token.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Is this query-parameter name ours, as the box will read it?
///
/// Compared after percent-decoding, because that is what the thing on the other
/// end does. `%68%35%69` is `h5i` to every web framework that decodes its
/// parameter names, and a literal comparison passed it through untouched,
/// carrying whatever value it named into the app's logs and into `Referer` on
/// every link out of the page.
///
/// Decoded *by byte*, never by slicing the `&str`: a name may hold any UTF-8 the
/// client chose, and `&raw[i + 1..i + 3]` after a `%` lands inside a multi-byte
/// character for an input as ordinary as `%aé`, a panic on the request path
/// reachable by anyone who can send this share a URL.
///
/// The decoding is deliberately partial and exact: only a well-formed escape
/// turns into its byte, a malformed one stays as itself, and the comparison is
/// case-sensitive because query parameter names are. This is a *name
/// comparison*, not a URL parser.
fn is_share_param_name(raw: &str) -> bool {
    if raw == QUERY_PARAM {
        return true;
    }
    let b = raw.as_bytes();
    // Bounded before it decodes: a name is as long as the client made it, and
    // three characters cannot hide in more than nine bytes of escapes.
    if b.len() > QUERY_PARAM.len() * 3 {
        return false;
    }
    // Compared as it decodes, against no buffer at all. A target may carry
    // thousands of parameters and this runs on every one of them, for every
    // request from the open internet, so it allocates nothing and stops at the
    // first byte that is not ours, which for an app's own parameter is the
    // first byte.
    let want = QUERY_PARAM.as_bytes();
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let mut matched = 0usize;
    let mut i = 0;
    while i < b.len() {
        let (byte, step) = match (
            b[i],
            b.get(i + 1).copied().and_then(hex),
            b.get(i + 2).copied().and_then(hex),
        ) {
            (b'%', Some(hi), Some(lo)) => (hi * 16 + lo, 3),
            (c, _, _) => (c, 1),
        };
        if matched >= want.len() || byte != want[matched] {
            return false;
        }
        matched += 1;
        i += step;
    }
    matched == want.len()
}

/// Remove the share parameter from a request target, leaving the rest of the
/// query alone. The app's own parameters are its business.
///
/// Every occurrence, whatever the value. Narrowing this to values [`plausible`]
/// accepts is the obvious tidy-up and it is wrong: the box reads the query after
/// percent-decoding, so `?h5i=<secret>%20` is a value this module refuses as a
/// credential and the app resolves back into one. The removal is therefore a
/// superset of the read, and it has to stay that way.
///
/// The cost is an app that uses a parameter of this exact name losing it through
/// a share. Unlike the cookie side, where a *prefix* rule was swallowing names
/// like `h5i_shared_theme`, this is an exact match on the tool's own name.
///
/// The name is compared decoded, for the same reason the value is not narrowed:
/// `?%68%35%69=<secret>` is `h5i=<secret>` to every framework that decodes its
/// parameter names, and it was passed straight through.
fn strip_param(target: &str) -> (String, Option<String>) {
    let Some((path, query)) = target.split_once('?') else {
        return (target.to_string(), None);
    };
    let mut found = None;
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            if is_share_param_name(k) {
                found = Some(v.to_string());
                false
            } else {
                !pair.is_empty()
            }
        })
        .collect();
    let clean = if kept.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{}", kept.join("&"))
    };
    (clean, found)
}

/// Pull our cookie out of a `Cookie` header, and return the header as it should
/// go upstream: without *any* h5i share cookie.
///
/// Reading our own by exact name and dropping every cookie whose name starts
/// with [`COOKIE`] are two different rules on purpose, and the difference is a
/// credential leak.
///
/// Cookies ignore the port, so a browser sends every `127.0.0.1` cookie to every
/// `127.0.0.1` listener. Two `h5i join` sessions on one machine, the case
/// per-port naming was introduced for, therefore put *both* share credentials in
/// every request to either. Stripping only the exact name would leave one share
/// handing the other's credential to the agent-written code it is showing
/// somebody.
///
/// `app` narrows what is kept from "everything that is not ours" to "what the
/// box itself set", and is `Some` exactly when this proxy had to settle for a
/// cookie jar it shares with the rest of the machine. See [`AppCookies`].
fn split_cookie(value: &str, name: &str, app: Option<&AppCookies>) -> (Option<String>, String) {
    let mut ours = None;
    let kept: Vec<&str> = value
        .split(';')
        .filter(|c| {
            let c = c.trim();
            match c.split_once('=') {
                Some((k, v)) => {
                    let k = k.trim();
                    if k == name {
                        ours = Some(v.trim().to_string());
                    }
                    !is_share_cookie_name(k) && app.is_none_or(|a| a.knows(k, v.trim()))
                }
                // A segment with no `=` at all cannot carry a token, so this is not a
                // leak. It is the prefix rule being applied on one branch and not the
                // other, the exact shape of every "the check ran on two of the three paths"
                // defect this file has already had. Whatever is named like ours does not go
                // to the box, however it is spelled.
                //
                // The allowlist applies here too, and it can only ever say no: a
                // `Set-Cookie` has a value, so nothing the box set can arrive spelled like
                // this.
                None => !c.is_empty() && !is_share_cookie_name(c) && app.is_none(),
            }
        })
        .map(|c| c.trim())
        .collect();
    (ours, kept.join("; "))
}

/// Every value for a header name, in order. Used where a *second* copy is not
/// a curiosity but a disagreement between us and the box about what the request
/// says.
fn headers_named<'a>(headers: &[&'a str], name: &str) -> Vec<&'a str> {
    headers
        .iter()
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
        })
        .collect()
}

/// Does a comma-separated header list contain this token?
fn lists_token(value: &str, token: &str) -> bool {
    value
        .split(',')
        .any(|t| t.trim().eq_ignore_ascii_case(token))
}

/// Refuse a head whose headers are shaped in a way two parsers could read
/// differently.
///
/// Same reasoning as the CRLF check below and the same failure if skipped: the
/// gate decides where a request ends and the box decides separately, and any
/// construction they can disagree about walks a second request past the gate.
///
/// * *Obs-fold* (a header line starting with a space or tab) is a continuation
///   of the previous header to a server that implements RFC 7230's obsolete line
///   folding, and a header of its own to a parser splitting on CRLF.
///   `X-Pad: a\r\n Content-Length: 35` is no body to one and a 35-byte body to
///   the other.
/// * A space before the colon (`Content-Length : 35`) must be rejected by a
///   conforming server, and is a valid header to anything that trims the name.
fn headers_are_unambiguous(headers: &[&str]) -> bool {
    headers.iter().all(|l| {
        if l.starts_with(' ') || l.starts_with('\t') {
            return false;
        }
        match l.split_once(':') {
            Some((k, _)) => !k.is_empty() && k.bytes().all(is_tchar),
            None => false,
        }
    })
}

/// A character RFC 7230 allows in a header field name.
///
/// Checked as a whole rather than by trimming the two whitespace characters
/// anyone thinks of. Rust's `str::trim` strips the entire Unicode whitespace
/// class (vertical tab, form feed, NEL, non-breaking space, the en/em quads,
/// ideographic space) so a name like `Content-Length\u{0c}` matched every lookup
/// in this module while being a malformed line to the box: the same disagreement
/// as a space before the colon, arriving by a door two characters wide.
pub fn is_tchar(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

/// Refuse a head whose line endings are not consistently CRLF.
///
/// A bare LF inside a header line is one header to a parser that splits on
/// CRLF and two headers to almost everything else, which is a disagreement
/// between this gate and the box about what the request actually said. The
/// classic way to walk a header past an inspecting proxy. There is no reason a
/// real client sends one, so this refuses rather than normalising: normalising
/// leaves two parsers and hopes they agree, refusing leaves one request.
fn crlf_is_consistent(head: &str) -> bool {
    let b = head.as_bytes();
    for (i, &c) in b.iter().enumerate() {
        if c == b'\n' && (i == 0 || b[i - 1] != b'\r') {
            return false;
        }
        if c == b'\r' && b.get(i + 1) != Some(&b'\n') {
            return false;
        }
    }
    true
}

/// Read what the gate needs. `None` when this is not an HTTP/1 request head, or
/// is one this proxy refuses to reason about.
pub fn parse(head: &str, cookie: &str) -> Option<Request> {
    if !crlf_is_consistent(head) {
        return None;
    }
    let (request_line, headers) = lines(head)?;
    if !headers_are_unambiguous(&headers) {
        return None;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let version = parts.next()?;
    if !version.starts_with("HTTP/") || parts.next().is_some() {
        return None;
    }
    // One `Host`, or none of this reasons about the right request. RFC 7230
    // §5.4 says a server must reject a message with more than one, and here it
    // is load bearing twice over: the origin check compares against it, and the
    // box picks its own from the same head, so two of them is this gate
    // deciding a request is the share's own while the box serves a different
    // virtual host. The framing checks below refuse duplicates for exactly this
    // reason; `Host` was the one that did not.
    if headers_named(&headers, "host").len() > 1 {
        return None;
    }
    let (clean_target, query_token) = strip_param(&target);
    // Every `Cookie` header, not just the first. A client is allowed to send
    // more than one, and reading only the first turned a legitimate visitor's
    // request into a `401` while the rewrite below stripped all of them anyway.
    // `None`: this half reads *our* cookie by exact name, and which of the
    // app's cookies may go upstream has no bearing on whether this visitor is
    // admitted.
    let cookie_token = headers_named(&headers, "cookie")
        .into_iter()
        .find_map(|c| split_cookie(c, cookie, None).0);
    // The query only wins when it carries something usable. `/?h5i` and `/?h5i=`
    // used to shadow a perfectly good cookie and produce a `401` on a page the
    // visitor was entitled to, and an app with its own parameter called `h5i`
    // does exactly that.
    //
    // Note the asymmetry, which is deliberate and is the safe direction:
    // `strip_param` removes *every* occurrence of the parameter and this accepts
    // only the values that could be a credential.
    let query_token = query_token.filter(|t| plausible(t));
    let from_query = query_token.is_some();
    let token = query_token.or(cookie_token).filter(|t| plausible(t));

    // Both halves required. `upgrade` is the flag that lets a connection out of
    // the one-request rule, and a lone `Upgrade:` header is something any
    // client can attach to an ordinary request that will never upgrade.
    let upgrade = header(&headers, "upgrade").is_some_and(|u| !u.trim().is_empty())
        && headers_named(&headers, "connection")
            .iter()
            .any(|c| lists_token(c, "upgrade"));

    // Framing has to be unambiguous, because the proxy forwards exactly one
    // request and needs to know where it ends. Two `Content-Length` headers, or
    // a `Content-Length` beside a `Transfer-Encoding`, are the two shapes that
    // let two parsers disagree about that.
    let lengths = headers_named(&headers, "content-length");
    // Every transfer coding, in order, with `identity` dropped: it is legal,
    // deprecated, and means "no encoding", so counting it as a coding would
    // refuse a perfectly ordinary request as ambiguous.
    let codings: Vec<String> = headers_named(&headers, "transfer-encoding")
        .iter()
        .flat_map(|v| v.split(','))
        .map(|t| t.trim().to_ascii_lowercase())
        .filter(|t| !t.is_empty() && t != "identity")
        .collect();
    // A transfer coding beside a length is ambiguous whatever the coding is,
    // and only `chunked` was being counted. `Transfer-Encoding: gzip` plus
    // `Content-Length: 4` therefore passed this gate and was forwarded with
    // both headers intact, while h5i sent exactly four bytes. An origin, for
    // which the transfer coding is authoritative, no longer agrees with this
    // proxy about where the request ends. That is precisely the disagreement
    // the duplicate-length check exists to remove.
    if lengths.len() > 1 || (!codings.is_empty() && !lengths.is_empty()) {
        return None;
    }
    // And a coding this proxy cannot follow is one it will not forward a body
    // under. `chunked` has to be final: `chunked, gzip` is invalid and means
    // the octets are not chunk-framed, so `forward_chunked` would be reading
    // compressed bytes as chunk headers.
    let chunked = codings.last().is_some_and(|c| c == "chunked");
    if !codings.is_empty() && !chunked {
        return None;
    }
    let content_length = match lengths.first() {
        Some(v) => {
            let v = v.trim();
            // ASCII digits and nothing else. Rust's `u64::from_str` accepts a
            // leading `+`, no HTTP server does, and a value one side reads as a
            // length and the other reads as absent is the same disagreement the
            // checks above exist to refuse.
            if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            Some(v.parse::<u64>().ok()?)
        }
        None => None,
    };

    let expects_continue = headers_named(&headers, "expect")
        .iter()
        .any(|v| lists_token(v, "100-continue"));

    Some(Request {
        method,
        target,
        clean_target,
        from_query,
        token,
        upgrade,
        content_length,
        expects_continue,
        service_worker: registers_a_service_worker(&headers),
        cross_origin: is_cross_origin(&headers, header(&headers, "host"), from_query),
        // Carried as its own flag rather than encoded in the length. As a
        // sentinel value it collided with a real `Content-Length` of
        // `u64::MAX`, and it was skipped entirely when the request also asked
        // to upgrade, which made "chunked bodies are refused" false for a
        // request that simply attached an `Upgrade` header.
        chunked,
    })
}

/// Is this somewhere on *this* origin?
///
/// The redirect's `Location` comes from the request target, so a target of
/// `//evil.test/` would send an authorized visitor to another site with this
/// share's origin in the `Referer`. Only an origin-relative path is ever
/// echoed back; anything else goes to the root.
pub fn safe_location(target: &str) -> String {
    let ok = target.starts_with('/') && !target.starts_with("//") && !target.starts_with("/\\");
    if ok {
        target.to_string()
    } else {
        "/".to_string()
    }
}

/// Headers that say who the visitor is, dropped before the box sees them.
///
/// Measured through a real quick tunnel, echoed back by a dev server inside a
/// box:
///
/// ```text
/// Cf-Connecting-Ip: 160.39.54.165
/// Cf-Ipcountry: US
/// X-Forwarded-For: 160.39.54.165
/// Cf-Ray: a29916242859de98-EWR
/// ```
///
/// The agent-written code being demonstrated was handed the visitor's public IP
/// address and their country, by a feature whose roadmap entry says nothing
/// inside the box learns it is being shared. Whoever clicked the link agreed to
/// look at a page, not to identify themselves to something running in somebody
/// else's sandbox.
///
/// `Host` and `X-Forwarded-Proto` are deliberately *not* here: a dev server
/// builds absolute URLs out of them, and a share that broke every link on the
/// page would not be used.
const HIDE_FROM_BOX: &[&str] = &[
    "cf-connecting-ip",
    "cf-connecting-ipv6",
    "cf-ipcountry",
    "cf-ray",
    "cf-visitor",
    "cf-warp-tag-id",
    "cf-ew-via",
    "cf-worker",
    "cdn-loop",
    "true-client-ip",
    "x-forwarded-for",
    "x-real-ip",
    // RFC 7239's own spelling, which carries `for=` and would be the obvious
    // way for any other front to say the same thing.
    "forwarded",
];

/// The head to send to the box: the share credential removed from both places it
/// could be, `Connection: close` forced, everything else byte-for-byte.
///
/// The `Cookie` header is rewritten rather than dropped, because the shared app
/// may have set cookies of its own and a share that silently logs the visitor
/// out of the app being demonstrated is a broken share. `app` narrows that to
/// the box's own cookies on a jar this proxy does not hold; see [`AppCookies`].
///
/// `Connection: close` is an authorization control, not a performance choice. A
/// connection is authorized once, when its first request arrives, which is
/// equivalent to authorizing every request only if a connection carries exactly
/// one. By default it does not: `cloudflared` pools connections to the origin
/// and reuses them for whatever request comes next, *from whatever visitor*, so
/// a request with no credential could ride in on a connection someone else's
/// credential opened. Browsers pool per origin the same way, putting the
/// identical hole on the joiner's proxy.
///
/// An upgrade is the exception and must be, since `Connection` is how an upgrade
/// is negotiated. That is safe for the same reason it is necessary: an upgraded
/// connection stops being an HTTP connection and is never returned to a pool.
pub fn rewrite_for_upstream(
    head: &str,
    req: &Request,
    cookie: &str,
    app: Option<&AppCookies>,
) -> String {
    let Some((request_line, _)) = lines(head) else {
        return head.to_string();
    };
    let mut out = String::with_capacity(head.len());
    // The target loses the share parameter; the method and version are copied.
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let _ = parts.next();
    let version = parts.next().unwrap_or("HTTP/1.1");
    out.push_str(&format!("{method} {} {version}\r\n", req.clean_target));

    for line in head.split("\r\n").skip(1) {
        if line.is_empty() {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else {
            out.push_str(line);
            out.push_str("\r\n");
            continue;
        };
        if k.trim().eq_ignore_ascii_case("cookie") {
            let (_, kept) = split_cookie(v.trim(), cookie, app);
            if !kept.is_empty() {
                out.push_str(&format!("Cookie: {kept}\r\n"));
            }
            continue;
        }
        if HIDE_FROM_BOX
            .iter()
            .any(|h| k.trim().eq_ignore_ascii_case(h))
        {
            continue;
        }
        if k.trim().eq_ignore_ascii_case("expect") && req.expects_continue {
            // Answered by the proxy already; leaving it would make the box send
            // a second, useless `100` behind the body it was gating.
            continue;
        }
        if k.trim().eq_ignore_ascii_case("connection") && !req.upgrade {
            // Replaced below, once, rather than passed through.
            continue;
        }
        // `Keep-Alive` only means anything alongside a `Connection` that keeps
        // the connection alive, and this one does not.
        if k.trim().eq_ignore_ascii_case("keep-alive") && !req.upgrade {
            continue;
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    if !req.upgrade {
        out.push_str("Connection: close\r\n");
    }
    out.push_str("\r\n");
    out
}

/// The redirect that gets the token out of the URL and into a cookie.
///
/// `SameSite=Lax` because a share is followed from a chat message, which is a
/// top-level navigation; `HttpOnly` because the page on the other end is
/// agent-written code and has no business reading the credential that admitted
/// its visitor.
pub fn set_cookie_redirect(cookie: &str, token: &str, location: &str, secure: bool) -> String {
    let attrs = if secure {
        "Path=/; HttpOnly; SameSite=Lax; Secure"
    } else {
        "Path=/; HttpOnly; SameSite=Lax"
    };
    format!(
        "HTTP/1.1 302 Found\r\n\
         Location: {location}\r\n\
         Set-Cookie: {cookie}={token}; {attrs}\r\n\
         Cache-Control: no-store\r\n\
         Content-Length: 0\r\n\
         Connection: close\r\n\r\n"
    )
}

/// A refusal, as bytes on the wire.
pub fn refusal_response(r: Refusal) -> String {
    let (code, reason) = r.status();
    let body = r.body();
    format!(
        "HTTP/1.1 {code} {reason}\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )
}

#[cfg(test)]
mod cookie_shape_tests {
    use super::*;

    #[test]
    fn a_share_cookie_with_no_value_is_still_not_the_box_s_business() {
        // Found by the fuzzer. `split_cookie` applied the "nothing named like
        // ours goes upstream" rule on the branch where a cookie has an `=` and
        // not on the branch where it does not. The same "the check ran on two
        // of the three paths" shape as several earlier defects here. It is not
        // a leak on its own (a segment with no `=` carries no value), but a
        // rule with a hole in it is a rule nobody can reason about.
        let (ours, kept) = split_cookie("a=1; h5i_share_8899; b=2", "h5i_share_8899", None);
        assert_eq!(ours, None);
        assert_eq!(kept, "a=1; b=2");

        // And the rule is about the *name*, not about the string appearing
        // anywhere: somebody else's cookie that merely contains ours is theirs.
        let (_, kept) = split_cookie("999h5i_share=x; y=2", "h5i_share", None);
        assert_eq!(kept, "999h5i_share=x; y=2");
    }

    /// An app cookie that merely *begins* like ours is the app's.
    ///
    /// Both sanitisers tested `starts_with("h5i_share")`, which is a larger set
    /// than the two names h5i actually owns. `h5i_shared_theme=dark` was
    /// therefore deleted from every request into the box and its `Set-Cookie`
    /// deleted from every response out of it, so the app lost its state, or
    /// its login, *only when someone was looking at it through a share*. That
    /// is the opposite of the promise made directly above `split_cookie`.
    #[test]
    fn a_cookie_that_only_looks_like_ours_belongs_to_the_app() {
        // Ours: the bare name and the per-port one.
        assert!(is_share_cookie_name("h5i_share"));
        assert!(is_share_cookie_name("h5i_share_8899"));
        assert!(is_share_cookie_name("h5i_share_1"));
        assert!(is_share_cookie_name("h5i_share_65535"));

        // Theirs.
        assert!(!is_share_cookie_name("h5i_shared_theme"));
        assert!(!is_share_cookie_name("h5i_shareable_session"));
        assert!(!is_share_cookie_name("h5i_share_"));
        assert!(!is_share_cookie_name("h5i_share_0"));
        assert!(!is_share_cookie_name("h5i_share_65536"));
        assert!(!is_share_cookie_name("h5i_share_abc"));
        assert!(!is_share_cookie_name("h5i_sharex"));
        assert!(!is_share_cookie_name("999h5i_share"));

        // And the request direction, end to end: the app's cookies go
        // upstream, both of ours do not.
        let (ours, kept) = split_cookie(
            "h5i_shared_theme=dark; h5i_share_8899=tok; h5i_shareable_session=s; h5i_share=t",
            "h5i_share_8899",
            None,
        );
        assert_eq!(ours.as_deref(), Some("tok"));
        assert_eq!(kept, "h5i_shared_theme=dark; h5i_shareable_session=s");
    }

    /// On a jar this share does not have to itself, a cookie belongs to the box
    /// only if the box set it.
    ///
    /// The joiner's browser sends every `127.0.0.1` cookie to every `127.0.0.1`
    /// listener, so on the fallback address the `Cookie` header arriving here is
    /// the whole machine's, not the app's. Forwarding it put the joiner's own
    /// `session=<secret>`, from their own local app on their own machine, into
    /// agent-written code inside somebody else's box, on the *first* request.
    #[test]
    fn on_a_shared_jar_only_the_boxs_own_cookies_go_upstream() {
        let jar = AppCookies::default();
        let sent = "session=joiners-own-secret; theme=dark; sid=9";

        // The first request. Nothing has been learned, so nothing is the
        // box's, so nothing goes.
        let (_, kept) = split_cookie(sent, "h5i_share_8899", Some(&jar));
        assert_eq!(kept, "", "a credential went upstream before the box spoke");

        // The box sets one of its own. Now that one, and only that one,
        // comes back to it.
        jar.learn("sid", "9");
        let (_, kept) = split_cookie(sent, "h5i_share_8899", Some(&jar));
        assert_eq!(kept, "sid=9");

        // And the same header on a jar of this share's own is untouched: there
        // is nobody else's cookie in it to tell apart.
        let (_, kept) = split_cookie(sent, "h5i_share_8899", None);
        assert_eq!(kept, sent);
    }

    /// One name, two cookies, and only the box's own goes back.
    ///
    /// Cookies sharing a name coexist in one jar when their paths differ, and
    /// the browser sends both segments in a single `Cookie` header. Matching on
    /// the name alone forwarded both, so a box that set `sid` at a deep path,
    /// having guessed a name the joiner's own local service uses at `/`, was
    /// handed the joiner's `sid` beside its own on every request under that
    /// path. The guess is cheap and the box is untrusted code by construction.
    ///
    /// The value closes it: the one thing a box cannot do is set a cookie to a
    /// secret nobody has told it.
    #[test]
    fn a_name_the_box_guessed_does_not_carry_the_joiners_cookie() {
        let jar = AppCookies::default();
        jar.learn("sid", "box-value");

        // What the browser sends under the deep path: the box's, and the
        // joiner's own from another service on this machine.
        let (_, kept) = split_cookie(
            "sid=box-value; sid=joiners-own-secret",
            "h5i_share_8899",
            Some(&jar),
        );
        assert_eq!(kept, "sid=box-value");
        assert!(
            !kept.contains("joiners-own-secret"),
            "a guessed name carried the joiner's cookie upstream: {kept}"
        );

        // Order is the browser's, not ours: most specific path first is usual,
        // but nothing may depend on it.
        let (_, kept) = split_cookie(
            "sid=joiners-own-secret; sid=box-value",
            "h5i_share_8899",
            Some(&jar),
        );
        assert_eq!(kept, "sid=box-value");

        // And a value the box has *stopped* setting is not grandfathered in
        // for having once shared a name with one it did.
        let (_, kept) = split_cookie("sid=something-else", "h5i_share_8899", Some(&jar));
        assert_eq!(kept, "");
    }

    /// A box cannot talk its way onto its own allowlist.
    ///
    /// The share credential is the one cookie in that jar the box must never
    /// get back, and the box chooses the contents of its own `Set-Cookie`
    /// headers. They are already deleted on the way out, but a name learned
    /// from one would have survived that and been forwarded on every later
    /// request, which is the leak this whole mechanism exists to close.
    #[test]
    fn the_box_cannot_learn_its_way_to_the_share_credential() {
        let jar = AppCookies::default();
        jar.learn("h5i_share", "tok");
        jar.learn("h5i_share_8899", "tok");
        assert!(!jar.knows("h5i_share", "tok"));
        assert!(!jar.knows("h5i_share_8899", "tok"));

        // Belt and braces: even if one were on the list, the name predicate
        // still refuses it.
        let (ours, kept) = split_cookie("h5i_share_8899=tok; a=1", "h5i_share_8899", Some(&jar));
        assert_eq!(ours.as_deref(), Some("tok"), "our own is still read");
        assert_eq!(kept, "");
    }

    /// Two shapes that must not slip past the narrowed rule.
    #[test]
    fn the_odd_shapes_are_dropped_on_a_shared_jar_too() {
        let jar = AppCookies::default();
        jar.learn("sid", "9");

        // A segment with no `=`. A `Set-Cookie` always has a value, so nothing
        // the box set can arrive spelled like this, and there is no name here
        // for the allowlist to match.
        let (_, kept) = split_cookie("nameless; sid=9", "h5i_share", Some(&jar));
        assert_eq!(kept, "sid=9");

        // Cookie names are case-sensitive, and so is this.
        let (_, kept) = split_cookie("SID=9", "h5i_share", Some(&jar));
        assert_eq!(kept, "");
    }

    /// The list is bounded, because the box chooses what goes on it.
    ///
    /// A dev server sets a handful of cookies. A box that emits a new name per
    /// response is not one, and this is the only structure on the joiner's
    /// machine that the far end could otherwise grow without limit.
    #[test]
    fn a_box_cannot_grow_the_list_without_limit() {
        let jar = AppCookies::default();
        for i in 0..MAX_APP_COOKIES * 4 {
            jar.learn(&format!("c{i}"), "v");
        }
        assert!(jar.knows("c0", "v"));
        assert!(!jar.knows(&format!("c{}", MAX_APP_COOKIES * 4 - 1), "v"));
        // Full is not frozen: a pair already on the list is still known, so a
        // busy app does not start losing the cookies it had.
        jar.learn("c0", "v");
        assert!(jar.knows("c0", "v"));
    }
}

#[cfg(test)]
mod origin_tests {
    use super::*;

    /// A request carrying the share cookie and nothing else. The state a
    /// visitor's browser is in after the invite redirect.
    fn req(extra: &str) -> Option<Request> {
        parse(
            &format!(
                "GET /a HTTP/1.1\r\nHost: 127.0.0.1:8899\r\nCookie: h5i_share=abc\r\n{extra}\r\n"
            ),
            "h5i_share",
        )
    }

    /// The invite itself: the token in the query string, which is the one
    /// thing a third-party page cannot put there.
    fn invite(extra: &str) -> Option<Request> {
        parse(
            &format!(
                "GET /?h5i={} HTTP/1.1\r\nHost: 127.0.0.1:8899\r\n{extra}\r\n",
                "a".repeat(64)
            ),
            "h5i_share",
        )
    }

    #[test]
    fn a_page_on_another_loopback_port_is_not_this_share() {
        // Two `h5i join` proxies on one machine are the same *site*, so
        // `SameSite=Lax` does not hold the cookie back between them: a page
        // served by one share could drive another colleague's box on the next
        // port, with the credential attached, and it reached it.
        let other = req("Origin: http://127.0.0.1:8900\r\n").expect("parses");
        assert!(other.cross_origin);

        // The share's own page is not foreign to itself.
        let mine = req("Origin: http://127.0.0.1:8899\r\n").expect("parses");
        assert!(!mine.cross_origin);

        // A navigation carries no `Origin`, and that is how every visitor
        // arrives. Refusing it would refuse the invite link itself.
        assert!(!req("").expect("parses").cross_origin);

        // `null` is a sandboxed context. It cannot be this share.
        assert!(req("Origin: null\r\n").expect("parses").cross_origin);
    }

    #[test]
    fn a_subresource_from_the_share_next_door_is_refused() {
        // The shape the `Origin` check missed entirely. Browsers send no
        // `Origin` on a subresource GET, so
        // `<img src="http://127.0.0.1:8899/api/reset">` on a page served by
        // another share put the credential on the wire with no `Origin`
        // anywhere, and was served. Routes like `/reset`, `/logout` and
        // `/delete?id=` are exactly what agent-written demo apps are full of.
        let img = req(
            "Sec-Fetch-Site: same-site\r\nSec-Fetch-Mode: no-cors\r\nSec-Fetch-Dest: image\r\n",
        )
        .expect("parses");
        assert!(img.cross_origin);

        // A cross-site subresource too, and a framed one. A share inside
        // somebody else's iframe is not something a visitor can see the
        // address of.
        let far =
            req("Sec-Fetch-Site: cross-site\r\nSec-Fetch-Mode: cors\r\nSec-Fetch-Dest: empty\r\n")
                .expect("parses");
        assert!(far.cross_origin);
        let framed = req(
            "Sec-Fetch-Site: cross-site\r\nSec-Fetch-Mode: navigate\r\nSec-Fetch-Dest: iframe\r\n",
        )
        .expect("parses");
        assert!(framed.cross_origin);

        // And the one the carve-out let through: a form on the share next door
        // targeting this one. Same site, because a site does not include the
        // port, so `SameSite=Lax` constrains nothing, and it arrives as a
        // navigation of a document, exactly the shape the invite link has.
        // What tells them apart is that the invite comes from another *site*.
        let neighbour = req(
            "Sec-Fetch-Site: same-site\r\nSec-Fetch-Mode: navigate\r\nSec-Fetch-Dest: document\r\n",
        )
        .expect("parses");
        assert!(
            neighbour.cross_origin,
            "a navigation from the share next door was treated as the invite link"
        );
    }

    /// A cross-site navigation with no invite in it is not the invite.
    ///
    /// `SameSite=Lax` is exactly the rule that puts a cookie on a cross-site
    /// top-level GET navigation, so once a visitor holds the share cookie, the
    /// carve-out written for "the link came from a chat window" admitted any site
    /// on the internet that had learned the origin: `<a
    /// href="http://127.0.0.1:8899/reset">`, or a meta refresh, and the state
    /// change happens with the credential attached. CORS never enters into it,
    /// since nothing needs to read the answer.
    ///
    /// The exception belongs to the request that *brings* the capability.
    #[test]
    fn a_cross_site_navigation_without_the_invite_is_refused() {
        let nav = "Sec-Fetch-Site: cross-site\r\nSec-Fetch-Mode: navigate\r\n\
                   Sec-Fetch-Dest: document\r\n";
        let attacker = req(nav).expect("parses");
        assert!(
            attacker.cross_origin,
            "a cookie-only cross-site navigation was treated as the invite link"
        );
        // The same request *with* the token is the visitor arriving.
        assert!(!invite(nav).expect("parses").cross_origin);

        // And a cross-site navigation carrying both (the visitor following
        // the link a second time, from the chat window, with the cookie
        // already set) is still the invite.
        let both = parse(
            &format!(
                "GET /?h5i={} HTTP/1.1\r\nHost: 127.0.0.1:8899\r\nCookie: h5i_share=abc\r\n{nav}\r\n",
                "a".repeat(64)
            ),
            "h5i_share",
        )
        .expect("parses");
        assert!(!both.cross_origin);
    }

    #[test]
    fn the_invite_link_still_works_from_wherever_it_was_sent() {
        // The other direction, and the one that breaks the feature if it is
        // wrong. The invite is followed from a chat window, which is
        // cross-site by construction. Refusing it would refuse every visitor
        // at the front door.
        let from_chat = invite(
            "Sec-Fetch-Site: cross-site\r\nSec-Fetch-Mode: navigate\r\nSec-Fetch-Dest: document\r\n",
        )
        .expect("parses");
        assert!(!from_chat.cross_origin);

        // Typed, or opened from a bookmark.
        let typed =
            req("Sec-Fetch-Site: none\r\nSec-Fetch-Mode: navigate\r\nSec-Fetch-Dest: document\r\n")
                .expect("parses");
        assert!(!typed.cross_origin);

        // And the share's own page fetching its own assets.
        let own =
            req("Sec-Fetch-Site: same-origin\r\nSec-Fetch-Mode: cors\r\nSec-Fetch-Dest: empty\r\n")
                .expect("parses");
        assert!(!own.cross_origin);

        // A client that sends no fetch metadata at all falls back to the
        // `Origin` rule, which is what `curl` and every older browser do.
        assert!(!req("").expect("parses").cross_origin);
        assert!(
            req("Origin: http://127.0.0.1:8900\r\n")
                .expect("parses")
                .cross_origin
        );
    }

    /// A head that says two things about where it came from is not read as the
    /// generous one.
    ///
    /// Found by the fuzzer at round 9,931. Every framing check in this file
    /// reads *every* copy of its header, because a second copy is a disagreement
    /// rather than a curiosity, and the origin check, the newest of them, read
    /// the first. `Sec-Fetch-Site: same-origin` followed by anything at all was
    /// therefore this share's own page.
    ///
    /// No browser sends two and no page can add one, `Sec-Fetch-Site` being a
    /// forbidden header name. What makes it worth closing is the hop in between:
    /// `cloudflared` bridges HTTP/2 to HTTP/1.1, and a field repeated in h2
    /// arrives at this listener as two lines.
    #[test]
    fn a_second_answer_about_where_a_request_came_from_is_not_the_generous_one() {
        let two = |a: &str, b: &str| {
            parse(
                &format!(
                    "GET /a HTTP/1.1\r\nHost: 127.0.0.1:8899\r\nCookie: h5i_share=abc\r\n\
                     {a}\r\n{b}\r\n\r\n"
                ),
                "h5i_share",
            )
            .expect("parses")
        };
        for (a, b) in [
            ("Sec-Fetch-Site: same-origin", "Sec-Fetch-Site: cross-site"),
            ("Sec-Fetch-Site: cross-site", "Sec-Fetch-Site: same-origin"),
            ("Sec-Fetch-Site: none", "Sec-Fetch-Site: same-site"),
            (
                "Origin: http://127.0.0.1:8899",
                "Origin: http://evil.example",
            ),
            (
                "Origin: http://evil.example",
                "Origin: http://127.0.0.1:8899",
            ),
        ] {
            assert!(
                two(a, b).cross_origin,
                "a head carrying two answers was read as the share's own: {a} / {b}"
            );
        }

        // And one answer, twice, is still that answer: a repeat is only a
        // disagreement when the values differ.
        assert!(!two("Sec-Fetch-Site: same-origin", "Sec-Fetch-Site: same-origin").cross_origin);

        // The carve-out closes the same way. `Sec-Fetch-Mode`/`-Dest` are what
        // let a cross-site request through when it carries the invite, so a
        // second value there must not open it either.
        let invited = |extra: &str| {
            parse(
                &format!(
                    "GET /?h5i={} HTTP/1.1\r\nHost: h\r\nSec-Fetch-Site: cross-site\r\n{extra}\r\n\r\n",
                    "a".repeat(64)
                ),
                "h5i_share",
            )
            .expect("parses")
        };
        assert!(
            !invited("Sec-Fetch-Mode: navigate\r\nSec-Fetch-Dest: document").cross_origin,
            "the invite link stopped working"
        );
        assert!(
            invited(
                "Sec-Fetch-Mode: navigate\r\nSec-Fetch-Mode: no-cors\r\nSec-Fetch-Dest: document"
            )
            .cross_origin,
            "two modes opened the carve-out"
        );
        assert!(
            invited(
                "Sec-Fetch-Mode: navigate\r\nSec-Fetch-Dest: document\r\nSec-Fetch-Dest: image"
            )
            .cross_origin,
            "two destinations opened the carve-out"
        );

        // A second `Service-Worker` is read the fail-closed way too.
        let sw = parse(
            "GET /sw.js HTTP/1.1\r\nHost: h\r\nCookie: h5i_share=abc\r\n\
             Service-Worker: no\r\nService-Worker: script\r\n\r\n",
            "h5i_share",
        )
        .expect("parses");
        assert!(sw.service_worker);
    }

    /// The shape the bridge above actually produces for `Origin`: one line, with
    /// the values joined by a comma.
    ///
    /// The test above pins two *lines*, which is what `cloudflared` hands over
    /// for `Sec-Fetch-Site`, `Sec-Fetch-Mode`, `Sec-Fetch-Dest` and
    /// `Service-Worker`, checked against a live quick tunnel with an HTTP/2
    /// client repeating each. `Origin` came through the same hop folded into
    /// `Origin: http://a.example, http://b.example`, so the one header the
    /// `headers_named` fix was reaching for is the one that never arrives as two
    /// lines.
    ///
    /// It falls the right way: an origin is a single value with no list form, so
    /// a comma in one is already a head no browser produced, and the host
    /// comparison cannot match a value carrying two of them.
    #[test]
    fn an_origin_the_bridge_folded_into_one_line_is_not_this_share() {
        for origin in [
            "http://127.0.0.1:8899, http://evil.example",
            "http://evil.example, http://127.0.0.1:8899",
            // Ours twice. Still not an origin.
            "http://127.0.0.1:8899, http://127.0.0.1:8899",
        ] {
            assert!(
                req(&format!("Origin: {origin}\r\n"))
                    .expect("parses")
                    .cross_origin,
                "a folded origin was read as the share's own: {origin}"
            );
        }

        // And the unfolded one still works, which is the whole visitor
        // population: a fold is a second answer, not an ordinary request.
        assert!(
            !req("Origin: http://127.0.0.1:8899\r\n")
                .expect("parses")
                .cross_origin
        );

        // `Sec-Fetch-Site` is read before `Origin` and answers for itself, so
        // an intermediary that folds *it* has to fall the same way. This hop
        // does not, but the value is compared whole and matches neither the
        // `same-origin` nor the `none` arm, which is the fail-closed default.
        assert!(
            req("Sec-Fetch-Site: same-origin, cross-site\r\n")
                .expect("parses")
                .cross_origin
        );
    }

    /// Two `Host` headers are a request this share will not reason about.
    ///
    /// The origin check compares against one of them and the box picks its own
    /// from the same head, so a head with two is this gate deciding a request
    /// belongs to the share while the box serves a different virtual host.
    /// RFC 7230 §5.4 requires rejecting it, and every other duplicate that
    /// decides something here is already refused.
    #[test]
    fn a_request_with_two_hosts_is_refused() {
        assert!(parse(
            "GET / HTTP/1.1\r\nHost: share.test\r\nHost: evil.example\r\nCookie: h5i_share=abc\r\n\r\n",
            COOKIE
        )
        .is_none());
        // One is fine, and so is none. An HTTP/1.0 client sends none, and the
        // origin rule already answers for that.
        assert!(parse("GET / HTTP/1.1\r\nHost: share.test\r\n\r\n", COOKIE).is_some());
        assert!(parse("GET / HTTP/1.0\r\n\r\n", COOKIE).is_some());
    }

    #[test]
    fn a_tunnel_share_is_not_foreign_to_itself() {
        // Cloudflare terminates TLS, so the app's own requests arrive with an
        // `https` origin and an unschemed `Host`. Comparing schemes would
        // refuse every request the shared page makes to itself.
        let head = "GET /a HTTP/1.1\r\nHost: odd-cat.trycloudflare.com\r\n\
                    Cookie: h5i_share=abc\r\nOrigin: https://odd-cat.trycloudflare.com\r\n\r\n";
        assert!(!parse(head, "h5i_share").expect("parses").cross_origin);

        let elsewhere = "GET /a HTTP/1.1\r\nHost: odd-cat.trycloudflare.com\r\n\
                         Cookie: h5i_share=abc\r\nOrigin: https://evil.example\r\n\r\n";
        assert!(parse(elsewhere, "h5i_share").expect("parses").cross_origin);
    }
}

#[cfg(test)]
mod service_worker_tests {
    use super::*;

    #[test]
    fn a_service_worker_registration_is_refused() {
        // What a registration leaves behind is the point: it outlives
        // `h5i join`, so after Ctrl-C the origin is still controlled, and if
        // the joiner later runs their own dev server on that port, the worker
        // intercepts every fetch of their app.
        let head = "GET /sw.js HTTP/1.1\r\nHost: t\r\nCookie: h5i_share=abc\r\n\
                    Service-Worker: script\r\n\r\n";
        let req = parse(head, "h5i_share").expect("parses");
        assert!(req.service_worker);

        // And an ordinary fetch of the same file is not refused: the header is
        // what browsers send on a registration and on nothing else, so this
        // does not stop an app serving its own script.
        let plain = "GET /sw.js HTTP/1.1\r\nHost: t\r\nCookie: h5i_share=abc\r\n\r\n";
        assert!(!parse(plain, "h5i_share").expect("parses").service_worker);

        assert_eq!(Refusal::ServiceWorker.status().0, 403);
    }
}

#[cfg(test)]
mod fuzz_tests {
    use super::*;
    use crate::fuzz::{request_head, rounds, Rng};

    /// The properties the request side owes everything downstream, checked
    /// against generated heads rather than against the ones somebody thought
    /// of. Every previous defect here was a case a person constructed; this is
    /// the other half.
    #[test]
    fn the_box_is_not_told_who_is_visiting() {
        // Measured through a real quick tunnel: the dev server inside the box
        // was handed `Cf-Connecting-Ip: 160.39.54.165`, `Cf-Ipcountry: US` and
        // an `X-Forwarded-For` with the same address. Whoever clicked the link
        // agreed to look at a page, not to identify themselves to
        // agent-written code in somebody else's sandbox, and the roadmap
        // entry for this feature says nothing inside the box learns it is
        // being shared.
        let head = "GET /app HTTP/1.1\r\n\
                    Host: odd-cat.trycloudflare.com\r\n\
                    Cf-Connecting-Ip: 203.0.113.7\r\n\
                    X-Forwarded-For: 203.0.113.7, 198.51.100.2\r\n\
                    True-Client-Ip: 203.0.113.7\r\n\
                    X-Real-IP: 203.0.113.7\r\n\
                    Forwarded: for=203.0.113.7;proto=https\r\n\
                    Cf-Ipcountry: US\r\n\
                    Cf-Ray: a29916242859de98-EWR\r\n\
                    Cf-Visitor: {\"scheme\":\"https\"}\r\n\
                    Cf-Warp-Tag-Id: c5646e5b\r\n\
                    Cf-Ew-Via: 15\r\n\
                    Cf-Worker: trycloudflare.com\r\n\
                    Cdn-Loop: cloudflare; loops=1\r\n\
                    X-Forwarded-Proto: https\r\n\
                    User-Agent: Mozilla/5.0\r\n\
                    Cookie: h5i_share=abc; sid=9\r\n\r\n";
        let req = parse(head, "h5i_share").expect("parses");
        let out = rewrite_for_upstream(head, &req, "h5i_share", None);

        // Nothing that names the visitor or where they are.
        assert!(!out.contains("203.0.113.7"), "{out}");
        assert!(!out.contains("198.51.100.2"), "{out}");
        for gone in [
            "Cf-Connecting-Ip",
            "X-Forwarded-For",
            "True-Client-Ip",
            "X-Real-IP",
            "Forwarded:",
            "Cf-Ipcountry",
            "Cf-Ray",
            "Cf-Visitor",
            "Cf-Warp-Tag-Id",
            "Cf-Ew-Via",
            "Cf-Worker",
            "Cdn-Loop",
        ] {
            assert!(!out.contains(gone), "{gone} still reached the box:\n{out}");
        }

        // And the app still works: the two headers a dev server builds URLs
        // from are untouched, as is everything the visitor's browser said
        // about itself.
        assert!(out.contains("Host: odd-cat.trycloudflare.com"), "{out}");
        assert!(out.contains("X-Forwarded-Proto: https"), "{out}");
        assert!(out.contains("User-Agent: Mozilla/5.0"), "{out}");
        assert!(out.contains("Cookie: sid=9"), "{out}");
    }

    #[test]
    fn a_forwarded_head_never_carries_the_credential_or_two_framings() {
        const COOKIE: &str = "h5i_share";
        let mut rng = Rng::new(0x5EED);
        // Counted, and asserted on at the end. A generator that stops
        // producing heads the parser will accept turns this whole test into an
        // expensive way of running `parse` and discarding the answer, which is
        // what it was: measured against the real parser, 1.88% of heads got in,
        // *none* of two million carried both framings, and about one per run
        // carried a share cookie. It passed twenty million heads and proved
        // almost nothing it claimed to.
        let mut parsed = 0usize;
        let mut with_cookie = 0usize;
        let mut with_framing = 0usize;
        // Counted before `parse` runs, because that is the only place the
        // both-framings shape exists: the parser's job is to refuse it.
        let mut both_framings = 0usize;
        let mut both_parsed = 0usize;
        // A box that has said nothing, kept for the whole run: it is never
        // written to, and one allocation is not worth doing per round.
        let nothing_learned = AppCookies::default();
        for i in 0..rounds() {
            let seed = rng.next();
            let mut one = Rng::new(seed);
            let head = request_head(&mut one);
            let ctx = || format!("round {i}, seed {seed:#x}, head {head:?}");

            // Matched on the header *name*, not on the text appearing
            // anywhere: the first version of this counted a header called
            // `007Content-Length` as a `Content-Length`, reported that a head
            // carrying both framings had been accepted, and the parser had
            // been right all along.
            let named = |want: &str| {
                head.split("\r\n").skip(1).any(|l| {
                    l.split_once(':')
                        .is_some_and(|(k, _)| k.eq_ignore_ascii_case(want))
                })
            };
            let offers_both = named("content-length")
                && head.split("\r\n").skip(1).any(|l| {
                    l.split_once(':').is_some_and(|(k, v)| {
                        k.eq_ignore_ascii_case("transfer-encoding")
                            && v.split(',')
                                .any(|t| t.trim().eq_ignore_ascii_case("chunked"))
                    })
                });
            if offers_both {
                both_framings += 1;
            }
            let Some(req) = parse(&head, COOKIE) else {
                continue;
            };
            if offers_both {
                both_parsed += 1;
            }
            parsed += 1;
            if req.chunked || req.content_length.is_some() {
                with_framing += 1;
            }
            if head.to_ascii_lowercase().contains("cookie:") && req.token.is_some() {
                with_cookie += 1;
            }

            // Not asserted here: that a parsed request never carries both
            // framings. `parse` returns `None` for exactly that combination,
            // so after a successful parse the conjunction is unreachable. For
            // any corpus, forever. It read as coverage of the crate's central
            // property and was a tautology. The property is real and is
            // checked where it can fail, on the *rejection* side, below.

            let out = rewrite_for_upstream(&head, &req, COOKIE, None);

            // The box must never see the credential that admitted its visitor.
            // Scoped to the headers and the request target, the two places this proxy
            // decides what to send, rather than to the whole head: a client that puts
            // the string in its own method or in a header of its own invention is
            // leaking a token it already holds to a box it already reached.
            //
            // By cookie *name*, not by substring of the line. A cookie called
            // `999h5i_share` merely contains the string and is somebody else's.
            for line in header_lines(&out) {
                if header_name_of(line) != "cookie" {
                    continue;
                }
                let value = line.split_once(':').map(|(_, v)| v).unwrap_or("");
                for c in value.split(';') {
                    let name = c.trim().split_once('=').map(|(k, _)| k).unwrap_or(c).trim();
                    assert!(
                        !is_share_cookie_name(name),
                        "a share cookie reached the box: {} -> {line:?}",
                        ctx()
                    );
                }
            }

            // The same head on a jar this share does not have to itself, where the
            // rule is narrower: nothing goes upstream but what the box set, and this
            // box has set nothing. So *no* cookie may survive, however the header is
            // spelled: quoted values, stray whitespace, a segment with no `=`, a second
            // `Cookie` header. That is the state of the very first request through a
            // join on `127.0.0.1`, the one that used to carry the joiner's own
            // `session=<secret>` into somebody else's box.
            //
            // Unfloored on purpose: it runs on every head that parses, and the
            // `with_cookie` floor below already guarantees a supply of real cookies.
            let narrowed = rewrite_for_upstream(&head, &req, COOKIE, Some(&nothing_learned));
            for line in header_lines(&narrowed) {
                assert!(
                    header_name_of(line) != "cookie",
                    "a cookie the box never set went upstream on a shared jar: {} -> {line:?}",
                    ctx()
                );
            }

            // And not in the URL either. A token in the target lands in the
            // app's own logs and in `Referer` on every link out of the page,
            // which is the whole reason an authorized query redirects instead
            // of proxying.
            let target = out.split_whitespace().nth(1).unwrap_or("");
            for (k, v) in query_params(target) {
                // The name as the *box* reads it. A framework decodes its
                // parameter names, so `%68%35%69` is `h5i` on the other end,
                // and an oracle that compares the raw text is agreeing with
                // the bug rather than checking for it.
                assert!(
                    !is_share_param_name(&k) || v.is_empty(),
                    "a share token reached the box in the URL: {} -> {target:?}",
                    ctx()
                );
            }

            // One line discipline, so the box's parser and ours cannot
            // disagree about where a header ends.
            assert!(
                crlf_is_clean(out.as_bytes()),
                "a bare CR or LF reached the box: {} -> {out:?}",
                ctx()
            );

            // And one framing on the way out, however many came in.
            let (lengths, chunked) = count_framing(&out);
            assert!(
                lengths <= 1 && !(chunked && lengths > 0),
                "two framings reached the box ({lengths} lengths, chunked={chunked}): {} -> {out:?}",
                ctx()
            );
        }

        // Floors, not exact numbers: the point is that the generator is still
        // reaching the code, and a test that silently stops doing so is worse than
        // no test because it reads as coverage.
        //
        // Well under what the generator achieves today (about 18%, 0.8% and 0.8%
        // of heads), so RNG variation cannot trip them but a collapse will. Before
        // this was measured the same three numbers were 1.9%, 0.01% and 0.008%:
        // the two-framings assertion had run on essentially nothing, and "the
        // credential never reaches the box" on about one input per run.
        let n = rounds();
        let counts =
            format!("parsed {parsed}, framed {with_framing}, cookied {with_cookie}, of {n}");

        // The correctness half, at any round count: whatever was generated,
        // nothing that carried both framings was accepted.
        assert_eq!(
            both_parsed, 0,
            "a head carrying both framings was accepted: {counts}"
        );

        // The reach half, did the generator actually offer the shapes this
        // test exists to check, only when enough rounds were asked for to
        // expect them. These are statements about a default-sized run: the
        // rarest shape here is two specific headers landing on one head, and
        // at `H5I_FUZZ_ROUNDS=200` the floors fired with messages blaming the
        // generator for something the operator had chosen. A floor that
        // reports the wrong cause is worse than no floor. It sends the next
        // person to read a generator that is working perfectly.
        if n < 1_000 {
            return;
        }
        assert!(
            parsed * 10 > n,
            "the generator has stopped producing heads the parser accepts: {counts}"
        );
        assert!(
            with_framing * 500 > n,
            "almost nothing declared a body length, so nothing exercised the framing \
             arithmetic: {counts}"
        );
        assert!(
            both_framings > 0,
            "no head carried both a length and a chunked encoding, so the one shape \
             two parsers disagree about was never offered: {counts}"
        );
        assert!(
            with_cookie * 500 > n,
            "almost nothing carried a credential, so the property this test is named for \
             was barely checked: {counts}"
        );
    }

    /// A request the gate would proxy came from this share, or brought the
    /// capability with it.
    ///
    /// The property `is_cross_origin` exists for, checked against generated
    /// heads rather than the handful somebody wrote down. It was the newest
    /// decision in this file and the only one the corpus never offered a single
    /// input to.
    ///
    /// The oracle is deliberately re-derived from the head rather than from
    /// `Request`: it reads *every* value of each header, where the gate reads
    /// the first, so a head that carries two answers is a disagreement this
    /// finds rather than a coin the gate flips.
    #[test]
    fn nothing_a_foreign_page_sent_is_proxied() {
        let mut rng = Rng::new(0xF0E1);
        let mut foreign = 0usize;
        let mut invited = 0usize;
        for i in 0..rounds() {
            let seed = rng.next();
            let mut one = Rng::new(seed);
            let head = request_head(&mut one);
            let Some(req) = parse(&head, COOKIE) else {
                continue;
            };
            let ctx = || format!("round {i}, seed {seed:#x}, head {head:?}");

            let values = |name: &str| -> Vec<String> {
                head.split("\r\n")
                    .skip(1)
                    .filter_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.trim()
                            .eq_ignore_ascii_case(name)
                            .then(|| v.trim().to_string())
                    })
                    .collect()
            };

            // What the browser said about where this came from, reading all of
            // it. `None` means it said nothing.
            let sites = values("sec-fetch-site");
            let origins = values("origin");
            let hosts = values("host");
            let target = head.split_whitespace().nth(1).unwrap_or("");
            let brought_the_invite = target
                .split_once('?')
                .map(|(_, q)| {
                    q.split('&').any(|p| {
                        let (k, v) = p.split_once('=').unwrap_or((p, ""));
                        is_share_param_name(k) && plausible(v)
                    })
                })
                .unwrap_or(false);
            if brought_the_invite {
                invited += 1;
            }

            // Is this, on any reading, a request from somewhere that is not
            // this share?
            let a_navigation = values("sec-fetch-mode").iter().any(|m| m == "navigate")
                && values("sec-fetch-dest").iter().any(|d| d == "document");
            let says_foreign = if !sites.is_empty() {
                sites.iter().any(|s| match s.as_str() {
                    "same-origin" | "none" => false,
                    "cross-site" => !(a_navigation && brought_the_invite),
                    _ => true,
                })
            } else {
                origins.iter().any(|o| match o.split_once("://") {
                    None => true,
                    Some((_, rest)) => {
                        let h = rest.split(['/', '?', '#']).next().unwrap_or(rest);
                        !hosts.iter().any(|want| h.eq_ignore_ascii_case(want.trim()))
                    }
                })
            };
            if says_foreign {
                foreign += 1;
                assert!(
                    req.cross_origin,
                    "a request from somewhere that is not this share was not flagged: {}",
                    ctx()
                );
                // And the decision made on it is a refusal, whatever else the
                // head says: this runs before the credential is weighed.
                assert!(
                    matches!(
                        crate::http_front::decide(&head, COOKIE, |_| true, false, None),
                        crate::http_front::Next::Respond(_, Some(Refusal::ForeignOrigin))
                            | crate::http_front::Next::Respond(_, Some(Refusal::ServiceWorker))
                    ),
                    "a foreign-origin request was not refused: {}",
                    ctx()
                );
            }

            // A service worker registration is refused before anything else,
            // for the same reason and by the same route.
            if values("service-worker")
                .iter()
                .any(|v| v.eq_ignore_ascii_case("script"))
            {
                assert!(req.service_worker, "a registration was not seen: {}", ctx());
            }
        }

        let n = rounds();
        if n < 1_000 {
            return;
        }
        // Floors, so a generator that stops offering these shapes says so
        // rather than reading as coverage. Both are well under what it reaches.
        assert!(
            foreign * 100 > n,
            "almost nothing arrived from a foreign origin, so the check this test is named \
             for was barely exercised: {foreign} of {n}"
        );
        assert!(
            invited * 2000 > n,
            "almost nothing carried an invite in its query, so the one carve-out in the \
             rule was never taken: {invited} of {n}"
        );
    }

    /// Wherever a browser is sent, it is somewhere on this origin.
    #[test]
    fn a_redirect_never_leaves_the_origin() {
        let mut rng = Rng::new(0xB0B);
        for i in 0..rounds() {
            let seed = rng.next();
            let mut one = Rng::new(seed);
            let head = request_head(&mut one);
            let Some(req) = parse(&head, "h5i_share") else {
                continue;
            };
            let loc = safe_location(&req.clean_target);
            let ctx = || format!("round {i}, seed {seed:#x}, target {:?}", req.clean_target);
            assert!(
                loc.starts_with('/'),
                "not origin-relative: {} -> {loc:?}",
                ctx()
            );
            assert!(
                !loc.starts_with("//") && !loc.starts_with("/\\"),
                "protocol-relative, so it leaves this origin: {} -> {loc:?}",
                ctx()
            );
            assert!(
                !loc.contains('\r') && !loc.contains('\n'),
                "a redirect target could split the response head: {} -> {loc:?}",
                ctx()
            );
        }
    }

    /// The header lines of a head, without the request line.
    fn header_lines(head: &str) -> impl Iterator<Item = &str> {
        head.split("\r\n").skip(1).filter(|l| !l.is_empty())
    }

    fn header_name_of(line: &str) -> String {
        line.split_once(':')
            .map(|(n, _)| n.trim().to_ascii_lowercase())
            .unwrap_or_default()
    }

    /// The query as a downstream app would split it.
    fn query_params(target: &str) -> Vec<(String, String)> {
        let Some((_, q)) = target.split_once('?') else {
            return Vec::new();
        };
        q.split('&')
            .filter(|p| !p.is_empty())
            .map(|p| match p.split_once('=') {
                Some((k, v)) => (k.to_string(), v.to_string()),
                None => (p.to_string(), String::new()),
            })
            .collect()
    }

    /// No bare CR and no bare LF anywhere.
    fn crlf_is_clean(b: &[u8]) -> bool {
        for (i, &c) in b.iter().enumerate() {
            if c == b'\n' && (i == 0 || b[i - 1] != b'\r') {
                return false;
            }
            if c == b'\r' && b.get(i + 1) != Some(&b'\n') {
                return false;
            }
        }
        true
    }

    /// How many `Content-Length` headers, and whether a chunked
    /// `Transfer-Encoding` is present, counted the naive way a downstream
    /// parser would.
    fn count_framing(head: &str) -> (usize, bool) {
        let mut lengths = 0;
        let mut chunked = false;
        for line in head.split("\r\n").skip(1) {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            let name = name.trim().to_ascii_lowercase();
            if name == "content-length" {
                lengths += 1;
            }
            if name == "transfer-encoding"
                && value
                    .split(',')
                    .any(|t| t.trim().eq_ignore_ascii_case("chunked"))
            {
                chunked = true;
            }
        }
        (lengths, chunked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Most tests do not care which cookie name is in play.
    fn parse_default(head: &str) -> Option<Request> {
        parse(head, COOKIE)
    }

    fn head(first: &str, extra: &[&str]) -> String {
        let mut s = format!("{first}\r\nHost: share.test\r\n");
        for e in extra {
            s.push_str(e);
            s.push_str("\r\n");
        }
        s.push_str("\r\n");
        s
    }

    #[test]
    fn the_first_request_carries_the_token_in_the_url() {
        let r = parse_default(&head("GET /?h5i=abc123 HTTP/1.1", &[])).expect("parse");
        assert_eq!(r.token.as_deref(), Some("abc123"));
        assert!(r.from_query);
        assert_eq!(r.clean_target, "/");
    }

    #[test]
    fn later_requests_carry_it_in_the_cookie() {
        let r = parse_default(&head(
            "GET /app.js HTTP/1.1",
            &["Cookie: h5i_share=abc123; theme=dark"],
        ))
        .expect("parse");
        assert_eq!(r.token.as_deref(), Some("abc123"));
        assert!(!r.from_query);
    }

    #[test]
    fn the_apps_own_query_and_cookies_survive_the_trip() {
        // A share that quietly rewrote the app's URLs or dropped its session
        // cookie would be demonstrating a different app than the one running.
        let raw = head(
            "GET /search?q=rust&h5i=abc123&page=2 HTTP/1.1",
            &["Cookie: sid=xyz; h5i_share=abc123; theme=dark"],
        );
        let r = parse_default(&raw).expect("parse");
        assert_eq!(r.clean_target, "/search?q=rust&page=2");
        let up = rewrite_for_upstream(&raw, &r, COOKIE, None);
        assert!(up.contains("GET /search?q=rust&page=2 HTTP/1.1"));
        assert!(up.contains("Cookie: sid=xyz; theme=dark"));
    }

    #[test]
    fn the_credential_never_reaches_the_box() {
        // The property this module exists for. The shared app is agent-written
        // code being shown to someone; it must not be handed the capability
        // that let them in.
        let raw = head(
            "GET /?h5i=SECRETTOKEN HTTP/1.1",
            &["Cookie: h5i_share=SECRETTOKEN"],
        );
        let r = parse_default(&raw).expect("parse");
        let up = rewrite_for_upstream(&raw, &r, COOKIE, None);
        assert!(
            !up.contains("SECRETTOKEN"),
            "credential leaked upstream: {up}"
        );
        assert!(!up.contains("h5i_share"));
        assert!(!up.contains("h5i="));
    }

    #[test]
    fn an_upgrade_needs_both_headers_not_just_one() {
        // The flag decides whether a connection may stay open for a second
        // request, so a lone `Upgrade:` header, which any client can attach to
        // an ordinary request that will never upgrade, must not set it.
        let lone = parse_default(&head(
            "GET / HTTP/1.1",
            &["Upgrade: h2c", "Connection: keep-alive"],
        ))
        .expect("parse");
        assert!(!lone.upgrade);

        let real = parse_default(&head(
            "GET /hmr HTTP/1.1",
            &["Upgrade: websocket", "Connection: keep-alive, Upgrade"],
        ))
        .expect("parse");
        assert!(real.upgrade);
    }

    #[test]
    fn a_bare_newline_inside_the_head_is_refused_rather_than_normalised() {
        // One header to a parser that splits on CRLF, two to almost everything
        // else, which is how a header walks past an inspecting proxy. There is
        // no reason a real client sends one.
        let smuggled = "GET / HTTP/1.1\r\nHost: x\r\nX-Pad: a\nConnection: keep-alive\r\n\r\n";
        assert!(parse_default(smuggled).is_none());
        assert!(parse_default("GET / HTTP/1.1\rHost: x\r\n\r\n").is_none());
    }

    #[test]
    fn framing_that_two_parsers_could_read_differently_is_refused() {
        assert!(parse_default(&head(
            "POST / HTTP/1.1",
            &["Content-Length: 5", "Content-Length: 6"]
        ))
        .is_none());
        assert!(parse_default(&head(
            "POST / HTTP/1.1",
            &["Content-Length: 5", "Transfer-Encoding: chunked"]
        ))
        .is_none());
        assert!(parse_default(&head("POST / HTTP/1.1", &["Content-Length: nonsense"])).is_none());

        // Any transfer coding beside a length, not only `chunked`. Only
        // `chunked` was counted, so this passed the gate and was forwarded
        // with *both* headers while h5i sent exactly four bytes. An origin,
        // for which a transfer coding is authoritative, then disagrees with
        // this proxy about where the request ended, which is the whole shape
        // the duplicate-length check exists to remove.
        assert!(parse_default(&head(
            "POST / HTTP/1.1",
            &["Transfer-Encoding: gzip", "Content-Length: 4"]
        ))
        .is_none());
        assert!(parse_default(&head(
            "POST / HTTP/1.1",
            &["Transfer-Encoding: gzip, chunked", "Content-Length: 4"]
        ))
        .is_none());

        // A coding this proxy cannot follow, with no length at all: there is
        // no way to know where the body ends, so it is not forwarded.
        assert!(parse_default(&head("POST / HTTP/1.1", &["Transfer-Encoding: gzip"])).is_none());
        // `chunked` has to be final. `chunked, gzip` is invalid, and it means
        // the octets on the wire are not chunk-framed. Reading them as chunk
        // headers is reading compressed data as framing.
        assert!(parse_default(&head(
            "POST / HTTP/1.1",
            &["Transfer-Encoding: chunked, gzip"]
        ))
        .is_none());

        // And the two that must still work: `chunked` last, and `identity`,
        // which is legal, deprecated, and means "no encoding".
        let r = parse_default(&head(
            "POST / HTTP/1.1",
            &["Transfer-Encoding: gzip, chunked"],
        ))
        .expect("gzip then chunked is a body this proxy can find the end of");
        assert!(r.chunked);
        let r = parse_default(&head(
            "POST / HTTP/1.1",
            &["Transfer-Encoding: identity", "Content-Length: 4"],
        ))
        .expect("identity is not a coding");
        assert_eq!(r.content_length, Some(4));
        assert!(!r.chunked);
    }

    #[test]
    fn a_body_this_proxy_cannot_measure_is_flagged_rather_than_forwarded() {
        let r = parse_default(&head("POST / HTTP/1.1", &["Transfer-Encoding: chunked"]))
            .expect("parse");
        assert!(r.chunked);
        let plain =
            parse_default(&head("POST / HTTP/1.1", &["Content-Length: 12"])).expect("parse");
        assert_eq!(plain.content_length, Some(12));
        assert!(!plain.chunked);
    }

    #[test]
    fn the_redirect_can_only_point_back_at_this_share() {
        // A `Location` of `//evil.test/` is resolved by browsers as another
        // site, which would turn an authorized share into a phishing hop with
        // this origin in the `Referer`.
        assert_eq!(safe_location("/dash?tab=2"), "/dash?tab=2");
        assert_eq!(safe_location("//evil.test/"), "/");
        assert_eq!(safe_location("/\\evil.test/"), "/");
        assert_eq!(safe_location("https://evil.test/"), "/");
        assert_eq!(safe_location(""), "/");
    }

    #[test]
    fn one_connection_carries_one_request_so_one_check_covers_it() {
        // The hole this closes: `cloudflared` pools connections to the origin
        // and reuses them for the next request from *any* visitor, and browsers
        // pool per origin the same way. Gating the first request on a
        // connection is only equivalent to gating every request if the
        // connection cannot carry a second one.
        let raw = head(
            "GET / HTTP/1.1",
            &[
                "Cookie: h5i_share=abc123",
                "Connection: keep-alive",
                "Keep-Alive: timeout=60",
            ],
        );
        let r = parse_default(&raw).expect("parse");
        let up = rewrite_for_upstream(&raw, &r, COOKIE, None);
        assert!(up.contains("Connection: close"), "{up}");
        assert!(!up.to_lowercase().contains("keep-alive"), "{up}");
    }

    #[test]
    fn an_upgrade_keeps_its_connection_header_because_that_is_how_it_upgrades() {
        // And it is safe for the same reason it is necessary: an upgraded
        // connection stops being an HTTP connection and never goes back into
        // anybody's pool.
        let raw = head(
            "GET /hmr HTTP/1.1",
            &[
                "Cookie: h5i_share=abc123",
                "Connection: Upgrade",
                "Upgrade: websocket",
            ],
        );
        let r = parse_default(&raw).expect("parse");
        let up = rewrite_for_upstream(&raw, &r, COOKIE, None);
        assert!(up.contains("Connection: Upgrade"), "{up}");
        assert!(!up.contains("Connection: close"), "{up}");
    }

    #[test]
    fn a_cookie_header_with_nothing_left_in_it_is_dropped_entirely() {
        let raw = head("GET / HTTP/1.1", &["Cookie: h5i_share=abc123"]);
        let r = parse_default(&raw).expect("parse");
        let up = rewrite_for_upstream(&raw, &r, COOKIE, None);
        assert!(!up.to_lowercase().contains("cookie:"), "{up}");
    }

    #[test]
    fn the_header_name_is_matched_however_the_client_capitalised_it() {
        let raw = head("GET / HTTP/1.1", &["COOKIE: h5i_share=abc123"]);
        let r = parse_default(&raw).expect("parse");
        assert_eq!(r.token.as_deref(), Some("abc123"));
        assert!(!rewrite_for_upstream(&raw, &r, COOKIE, None).contains("abc123"));
    }

    #[test]
    fn an_upgrade_is_recognised_so_hot_reload_can_be_proxied() {
        let r = parse_default(&head(
            "GET /_next/webpack-hmr HTTP/1.1",
            &[
                "Upgrade: websocket",
                "Connection: Upgrade",
                "Cookie: h5i_share=abc",
            ],
        ))
        .expect("parse");
        assert!(r.upgrade);
        assert_eq!(r.token.as_deref(), Some("abc"));
    }

    #[test]
    fn a_token_that_could_not_be_one_is_not_carried_forward() {
        // Keeps absurd input away from the authorization path entirely.
        for bad in ["", &"a".repeat(200), "abc/../../etc", "a b"] {
            let r = parse_default(&head(
                "GET / HTTP/1.1",
                &[&format!("Cookie: h5i_share={bad}")],
            ))
            .expect("parse");
            assert_eq!(r.token, None, "accepted a token of {:?}", bad);
        }
    }

    #[test]
    fn the_query_wins_over_a_stale_cookie() {
        // Following a fresh invite link must replace an old grant's cookie,
        // or a revoked peer with a stale cookie could never be re-admitted.
        let r = parse_default(&head(
            "GET /?h5i=fresh HTTP/1.1",
            &["Cookie: h5i_share=stale"],
        ))
        .expect("parse");
        assert_eq!(r.token.as_deref(), Some("fresh"));
        assert!(r.from_query);
    }

    #[test]
    fn something_that_is_not_http_is_refused_rather_than_guessed_at() {
        assert!(parse_default("").is_none());
        assert!(parse_default("GET\r\n\r\n").is_none());
        assert!(parse_default("GET / HTTP/1.1 extra\r\n\r\n").is_none());
        assert!(parse_default("\x16\x03\x01 this is a TLS hello\r\n\r\n").is_none());
    }

    #[test]
    fn the_redirect_sets_the_cookie_and_drops_the_token_from_the_url() {
        let r = set_cookie_redirect(COOKIE, "abc123", "/dashboard", true);
        assert!(r.starts_with("HTTP/1.1 302 "));
        assert!(r.contains("Location: /dashboard"));
        assert!(r.contains("Set-Cookie: h5i_share=abc123; Path=/; HttpOnly; SameSite=Lax; Secure"));
        // Loopback is not https, and a Secure cookie there is a cookie some
        // browsers will refuse to store.
        assert!(!set_cookie_redirect(COOKIE, "abc123", "/", false).contains("Secure"));
    }

    #[test]
    fn one_shares_credential_is_never_handed_to_another_shares_box() {
        // The leak per-port naming introduced. Cookies ignore the port, so a
        // browser sends every 127.0.0.1 cookie to every 127.0.0.1 listener,
        // and stripping only this front's name left the *other* share's
        // credential in the head going to agent-written code.
        let a = cookie_for_port(43821);
        let b = cookie_for_port(43822);
        let raw = head(
            "GET / HTTP/1.1",
            &[&format!("Cookie: {a}=mine1111; {b}=theirs2222; sid=9")],
        );
        let r = parse(&raw, &a).expect("parse");
        assert_eq!(r.token.as_deref(), Some("mine1111"));
        let up = rewrite_for_upstream(&raw, &r, &a, None);
        assert!(!up.contains("mine1111"), "{up}");
        assert!(
            !up.contains("theirs2222"),
            "another share's credential leaked: {up}"
        );
        // The app's own cookies still survive, which is the whole reason this
        // is a filter rather than a `Cookie` header that gets dropped.
        assert!(up.contains("sid=9"), "{up}");
    }

    #[test]
    fn a_header_name_with_exotic_whitespace_in_it_is_refused() {
        // `str::trim` strips the whole Unicode whitespace class, so
        // `Content-Length\u{0c}` used to match every lookup here while being a
        // malformed line to the box, which is a smuggled request, by a door
        // two characters wide.
        for pad in [
            "\u{0b}", "\u{0c}", "\u{85}", "\u{a0}", "\u{2000}", "\u{3000}",
        ] {
            let raw = head(
                "POST / HTTP/1.1",
                &[&format!("Content-Length{pad}: 5"), "Cookie: h5i_share=abc"],
            );
            assert!(
                parse_default(&raw).is_none(),
                "accepted a name padded with {pad:?}"
            );
        }
        assert!(parse_default(&head("POST / HTTP/1.1", &["Content-Length: 5"])).is_some());
    }

    #[test]
    fn an_expect_header_is_noticed_and_not_passed_on() {
        let raw = head(
            "POST / HTTP/1.1",
            &[
                "Expect: 100-continue",
                "Content-Length: 5",
                "Cookie: h5i_share=abc",
            ],
        );
        let r = parse_default(&raw).expect("parse");
        assert!(r.expects_continue);
        let up = rewrite_for_upstream(&raw, &r, COOKIE, None);
        assert!(!up.to_lowercase().contains("expect:"), "{up}");
        assert!(up.contains("Content-Length: 5"), "{up}");
    }

    #[test]
    fn a_token_in_a_second_cookie_header_is_still_found() {
        let raw = head(
            "GET / HTTP/1.1",
            &["Cookie: sid=9", "Cookie: h5i_share=abc123"],
        );
        let r = parse_default(&raw).expect("parse");
        assert_eq!(r.token.as_deref(), Some("abc123"));
        assert!(!rewrite_for_upstream(&raw, &r, COOKIE, None).contains("abc123"));
    }

    #[test]
    fn headers_two_parsers_would_fold_differently_are_refused() {
        // Obs-fold: `X-Pad: a` continued onto the next line is one header to a
        // server that implements folding and two to anything splitting on CRLF,
        // no body to one, a 35-byte body to the other.
        assert!(
            parse_default("GET / HTTP/1.1\r\nX-Pad: a\r\n Content-Length: 35\r\n\r\n").is_none()
        );
        assert!(
            parse_default("GET / HTTP/1.1\r\nX-Pad: a\r\n\tContent-Length: 35\r\n\r\n").is_none()
        );
        // A space before the colon: a conforming server must reject it, and
        // anything that trims the name reads it as a real header.
        assert!(parse_default(&head("POST / HTTP/1.1", &["Content-Length : 35"])).is_none());
        // A `+` is a length to Rust's parser and to nothing else.
        assert!(parse_default(&head("POST / HTTP/1.1", &["Content-Length: +35"])).is_none());
        assert!(parse_default(&head("POST / HTTP/1.1", &["Content-Length: 35 "])).is_some());
    }

    #[test]
    fn a_chunked_body_is_refused_even_when_the_request_asks_to_upgrade() {
        // Attaching `Upgrade` used to skip the chunked check entirely, so the
        // box got a head saying `Transfer-Encoding: chunked` and then waited
        // for chunks that would never arrive. Holding a slot for free.
        let r = parse_default(&head(
            "POST / HTTP/1.1",
            &[
                "Transfer-Encoding: chunked",
                "Upgrade: websocket",
                "Connection: Upgrade",
            ],
        ))
        .expect("parse");
        assert!(r.chunked);
        // And a real length of u64::MAX is a length, not a sentinel.
        let big = parse_default(&head(
            "POST / HTTP/1.1",
            &["Content-Length: 18446744073709551615"],
        ))
        .expect("parse");
        assert!(!big.chunked);
        assert_eq!(big.content_length, Some(u64::MAX));
    }

    #[test]
    fn an_unusable_query_parameter_does_not_shadow_a_good_cookie() {
        // `/?h5i` and `/?h5i=` used to produce a 401 on a page the visitor was
        // entitled to, and an app with its own `h5i` parameter did it too.
        for target in ["/?h5i", "/?h5i=", "/?h5i=not%20a%20token"] {
            let raw = head(
                &format!("GET {target} HTTP/1.1"),
                &["Cookie: h5i_share=goodtoken"],
            );
            let r = parse_default(&raw).expect("parse");
            assert_eq!(r.token.as_deref(), Some("goodtoken"), "target {target}");
            assert!(!r.from_query, "target {target}");
        }
    }

    /// The parameter is removed in the spelling the *box* will read, not only in
    /// the one h5i writes.
    ///
    /// `?%68%35%69=<secret>` is `h5i=<secret>` to every web framework that
    /// decodes its parameter names, and a literal comparison forwarded it
    /// untouched, so the credential reached the app's logs and rode out on
    /// `Referer` with every link on the page. The narrowing that would have
    /// caught it on the *value* side does not exist for the opposite reason:
    /// `?h5i=<secret>%20` is not a credential to this parser and is one to the
    /// app, so the removal stays a superset of the read.
    #[test]
    fn the_parameter_is_stripped_however_it_is_spelled() {
        let real = "a".repeat(64);
        for name in ["h5i", "%68%35%69", "h%35i", "%685i", "h5%69"] {
            let raw = head(&format!("GET /x?{name}={real}&b=1 HTTP/1.1"), &[]);
            let r = parse_default(&raw).expect("parse");
            assert!(
                !r.clean_target.contains(&real),
                "a credential stayed in the target as `{name}`: {}",
                r.clean_target
            );
            let up = rewrite_for_upstream(&raw, &r, COOKIE, None);
            assert!(
                !up.contains(&real),
                "a credential reached the box as `{name}`: {up}"
            );
            // And the app's own parameter beside it is untouched.
            assert!(up.contains("b=1"), "{up}");
        }

        // Only ours. A name that merely contains an escape, or decodes to
        // something else, belongs to the app. Query parameter names are
        // case-sensitive, and nothing here lowercases them.
        for name in ["H5I", "%48%35%49", "h5ix", "xh5i", "%25h5i", "h%255i"] {
            let raw = head(&format!("GET /x?{name}=v HTTP/1.1"), &[]);
            let r = parse_default(&raw).expect("parse");
            assert!(
                r.clean_target.contains(name),
                "the app's own `{name}` was deleted: {}",
                r.clean_target
            );
        }
    }

    /// A percent escape at the end of a name, and a multi-byte character after
    /// one, do not panic.
    ///
    /// The decode is byte-wise for this reason: `&raw[i + 1..i + 3]` after a
    /// `%` lands inside a multi-byte character for an input as ordinary as
    /// `%aé`, which is a panic on the request path reachable by anybody who can
    /// send this share a URL.
    #[test]
    fn a_half_written_escape_in_a_parameter_name_is_not_a_crash() {
        for name in [
            "%",
            "%6",
            "%zz",
            "%aé",
            "é%",
            "%68%3",
            "%%%%%%%%%",
            "%68%35%69%",
        ] {
            let raw = head(&format!("GET /x?{name}=v HTTP/1.1"), &[]);
            assert!(parse_default(&raw).is_some(), "name {name:?}");
            assert!(!is_share_param_name(name) || name == "h5i", "name {name:?}");
        }
        // And the bound is on the encoded length, so a long name is refused
        // before it is walked.
        assert!(!is_share_param_name(&"%68".repeat(100)));
    }

    #[test]
    fn a_loopback_proxy_names_its_cookie_after_its_port() {
        // Cookies ignore the port, so two `h5i join` sessions on one machine
        // would set the same cookie on 127.0.0.1 and log each other out.
        let a = cookie_for_port(43821);
        let b = cookie_for_port(43822);
        assert_ne!(a, b);
        let raw = head(
            "GET / HTTP/1.1",
            &[&format!("Cookie: {a}=mine; {b}=theirs")],
        );
        assert_eq!(
            parse(&raw, &a).expect("parse").token.as_deref(),
            Some("mine")
        );
        assert_eq!(
            parse(&raw, &b).expect("parse").token.as_deref(),
            Some("theirs")
        );
        // And each strips only its own on the way to the box.
        let r = parse(&raw, &a).expect("parse");
        let up = rewrite_for_upstream(&raw, &r, &a, None);
        assert!(!up.contains("mine"), "{up}");
        assert!(!up.contains("theirs"), "{up}");
    }

    #[test]
    fn every_refusal_looks_the_same_from_the_outside() {
        // Unknown, expired and revoked are three different problems for the
        // sharer and one single answer to whoever is knocking.
        let body = refusal_response(Refusal::NotAuthorized);
        assert!(body.contains("401 Unauthorized"));
        assert!(!body.to_lowercase().contains("expired"));
        assert!(!body.to_lowercase().contains("revoked"));
    }
}
