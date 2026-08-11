//! The HTTP gate: reading a share credential off a request, and making sure it
//! never travels any further.
//!
//! Two surfaces speak HTTP to a browser — the quick tunnel on the sharer's side
//! ([`crate::tunnel`]) and the loopback proxy on the joiner's side
//! ([`crate::join`]) — and both need the same three things:
//!
//! 1. **Find the credential.** First request carries it in the query string,
//!    because a URL is the only thing you can hand someone who has no h5i.
//!    Every request after that carries it in a cookie.
//! 2. **Get it out of the address bar.** A token in the URL leaks into
//!    `Referer` on every outbound link, into screen shares, and into whatever
//!    the shared app logs. So the first request is answered with a redirect that
//!    sets the cookie and points at the same page without the token, and it is
//!    never proxied.
//! 3. **Never let it reach the box.** The shared app is agent-written code we
//!    are deliberately exposing to someone; handing it the credential that
//!    authorizes the share would be handing it the share. The cookie and the
//!    query parameter are stripped on the way upstream, and a test pins that.
//!
//! Parsing is deliberately small and refuses rather than repairs. Everything
//! here arrives from the open internet in tunnel mode.

/// Where the credential travels between requests.
///
/// A *prefix*, not the whole name: the loopback proxy on the joining side
/// appends its port. Cookies are scoped to a host and ignore the port, so two
/// `h5i join` sessions on one machine would otherwise set the same cookie on
/// `127.0.0.1` and quietly log each other out — and every request the browser
/// made to any other local service would carry the token into that service's
/// logs. Naming it per port fixes the first outright and narrows the second to
/// services that are looking for it.
pub const COOKIE: &str = "h5i_share";

/// The cookie name a loopback proxy on `port` should use.
pub fn cookie_for_port(port: u16) -> String {
    format!("{COOKIE}_{port}")
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
    /// The target with the share parameter removed — where a browser should be
    /// sent once the cookie is set.
    pub clean_target: String,
    /// True when the credential came from the URL, which is what makes this
    /// request a redirect rather than a proxy.
    pub from_query: bool,
    /// The credential, from the query if present, otherwise from the cookie.
    pub token: Option<String>,
    /// A genuine upgrade: an `Upgrade` header **and** a `Connection` header
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
/// Two `h5i join` proxies on one machine are `127.0.0.1:A` and `127.0.0.1:B`.
/// Those are different *origins* but the **same site**, so `SameSite=Lax` does
/// not hold a cookie back between them: a page served by one share could make
/// credentialed requests to another colleague's box on the next port, and they
/// reached it. CORS stops the page reading the answers; it does not stop the
/// side effects, and a demo of somebody's agent-written app is exactly the
/// place a request like that would come from.
///
/// The console's own gate (`h5i_core::server::authorize`) has had this check
/// since it was written, for the same reason. This one never got it.
///
/// Compared by host and port only, not by scheme: a tunnel share arrives with
/// `Origin: https://…trycloudflare.com` and `Host: …trycloudflare.com`, and
/// requiring the schemes to agree would refuse the app's own requests. An
/// absent `Origin` is allowed — that is an ordinary navigation, which is how
/// every visitor arrives.
fn is_cross_origin(headers: &[&str], host: Option<&str>) -> bool {
    let Some(origin) = header(headers, "origin") else {
        return false;
    };
    // `null` is what a sandboxed context sends. It cannot be this share, and a
    // page that wants to demonstrate itself has no reason to send it.
    let Some(rest) = origin.split_once("://").map(|(_, r)| r) else {
        return true;
    };
    let origin_host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    match host {
        Some(h) => !origin_host.eq_ignore_ascii_case(h.trim()),
        // No `Host` to compare against is itself a request no browser makes.
        None => true,
    }
}

/// Is this request trying to register a service worker?
///
/// Browsers treat `http://127.0.0.1:<port>` as a *potentially trustworthy*
/// origin, so a page served over the joiner's loopback proxy is a secure
/// context and may call `navigator.serviceWorker.register()`. What it registers
/// **outlives `h5i join`**: after Ctrl-C the origin is still controlled, and if
/// the joiner later runs their own dev server on that port — 3000, 5173, 8080 —
/// the worker intercepts every fetch of their app.
///
/// The joiner is the person who did not choose this risk. A demo does not need
/// a service worker, so the registration request is refused by name; browsers
/// send `Service-Worker: script` on exactly that fetch and on nothing else.
fn registers_a_service_worker(headers: &[&str]) -> bool {
    header(headers, "service-worker").is_some_and(|v| v.eq_ignore_ascii_case("script"))
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

/// Remove the share parameter from a request target, leaving the rest of the
/// query alone. The app's own parameters are its business.
fn strip_param(target: &str) -> (String, Option<String>) {
    let Some((path, query)) = target.split_once('?') else {
        return (target.to_string(), None);
    };
    let mut found = None;
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            if k == QUERY_PARAM {
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
/// go upstream — which is to say, without **any** h5i share cookie.
///
/// Reading our own by exact name and dropping every cookie whose name starts
/// with [`COOKIE`] are two different rules on purpose, and the difference is a
/// credential leak.
///
/// Cookies ignore the port, so a browser sends every `127.0.0.1` cookie to
/// every `127.0.0.1` listener. Two `h5i join` sessions on one machine — the
/// case per-port naming was introduced for — therefore put *both* share
/// credentials in every request to either. Stripping only the exact name would
/// leave one share handing the other's credential to the agent-written code it
/// is showing somebody, which is the one thing this module exists to prevent.
fn split_cookie(value: &str, name: &str) -> (Option<String>, String) {
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
                    !k.starts_with(COOKIE)
                }
                // A segment with no `=` at all. It cannot carry a token —
                // there is nothing after an equals sign to carry it — so this
                // is not a leak. It is the prefix rule being applied on one
                // branch and not the other, which is the exact shape of every
                // "the check ran on two of the three paths" defect this file
                // has already had. Whatever is named like ours does not go to
                // the box, however it is spelled.
                None => !c.is_empty() && !c.starts_with(COOKIE),
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
/// Same reasoning as the CRLF check below and the same failure if it is
/// skipped: the gate decides where a request ends and the box decides
/// separately, and any construction they can disagree about is a construction
/// that walks a second request past the gate.
///
/// * **Obs-fold** (a header line starting with a space or tab) is a
///   continuation of the previous header to a server that implements RFC 7230's
///   obsolete line folding, and a header of its own to a parser splitting on
///   CRLF. `X-Pad: a\r\n Content-Length: 35` is no body to one and a 35-byte
///   body to the other.
/// * **A space before the colon** (`Content-Length : 35`) must be rejected by a
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
/// class — vertical tab, form feed, NEL, non-breaking space, the en/em quads,
/// ideographic space — so a name like `Content-Length\u{0c}` matched every
/// lookup in this module while being a malformed line to the box. That is the
/// same disagreement as a space before the colon, arriving by a door two
/// characters wide.
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
/// between this gate and the box about what the request actually said — the
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
    let (clean_target, query_token) = strip_param(&target);
    // Every `Cookie` header, not just the first. A client is allowed to send
    // more than one, and reading only the first turned a legitimate visitor's
    // request into a `401` while the rewrite below stripped all of them anyway.
    let cookie_token = headers_named(&headers, "cookie")
        .into_iter()
        .find_map(|c| split_cookie(c, cookie).0);
    // The query only wins when it carries something usable. `/?h5i` and `/?h5i=`
    // used to shadow a perfectly good cookie and produce a `401` on a page the
    // visitor was entitled to — and an app with its own parameter called `h5i`
    // does exactly that.
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
    // Only `chunked` frames a body. `identity` is legal, deprecated, and means
    // "no encoding" — the response side learned this a round ago and this side
    // did not, so `Transfer-Encoding: identity` was refused as ambiguous.
    let chunked = headers_named(&headers, "transfer-encoding")
        .iter()
        .any(|v| lists_token(v, "chunked"));
    if lengths.len() > 1 || (chunked && !lengths.is_empty()) {
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
        cross_origin: is_cross_origin(&headers, header(&headers, "host")),
        // Carried as its own flag rather than encoded in the length. As a
        // sentinel value it collided with a real `Content-Length` of
        // `u64::MAX`, and it was skipped entirely when the request also asked
        // to upgrade — which made "chunked bodies are refused" false for a
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

/// The head to send to the box: the share credential removed from both places
/// it could be, `Connection: close` forced, everything else byte-for-byte.
///
/// The `Cookie` header is rewritten rather than dropped, because the shared app
/// may well have set cookies of its own and a share that silently logs the
/// visitor out of the app being demonstrated is a broken share.
///
/// **`Connection: close` is an authorization control, not a performance
/// choice.** A connection is authorized once, when its first request arrives.
/// That is only equivalent to authorizing every request if a connection carries
/// exactly one — and by default it does not. `cloudflared` keeps a pool of
/// connections to the origin and reuses them for whatever request comes next,
/// *from whatever visitor*, so a request with no credential could ride in on a
/// connection someone else's credential opened. Browsers pool per origin the
/// same way, which puts the identical hole on the joiner's proxy. Forcing the
/// connection closed after one response collapses the difference: one
/// connection, one request, one check.
///
/// An upgrade is the exception and must be, because `Connection` is how an
/// upgrade is negotiated. That is safe for the same reason it is necessary: an
/// upgraded connection stops being an HTTP connection and is never returned to
/// anybody's pool.
pub fn rewrite_for_upstream(head: &str, req: &Request, cookie: &str) -> String {
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
            let (_, kept) = split_cookie(v.trim(), cookie);
            if !kept.is_empty() {
                out.push_str(&format!("Cookie: {kept}\r\n"));
            }
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
        // not on the branch where it does not — the same "the check ran on two
        // of the three paths" shape as several earlier defects here. It is not
        // a leak on its own (a segment with no `=` carries no value), but a
        // rule with a hole in it is a rule nobody can reason about.
        let (ours, kept) = split_cookie("a=1; h5i_share_8899; b=2", "h5i_share_8899");
        assert_eq!(ours, None);
        assert_eq!(kept, "a=1; b=2");

        // And the rule is about the *name*, not about the string appearing
        // anywhere: somebody else's cookie that merely contains ours is theirs.
        let (_, kept) = split_cookie("999h5i_share=x; y=2", "h5i_share");
        assert_eq!(kept, "999h5i_share=x; y=2");
    }
}

#[cfg(test)]
mod origin_tests {
    use super::*;

    fn req(extra: &str) -> Option<Request> {
        parse(
            &format!(
                "GET /a HTTP/1.1\r\nHost: 127.0.0.1:8899\r\nCookie: h5i_share=abc\r\n{extra}\r\n"
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
        // arrives — refusing it would refuse the invite link itself.
        assert!(!req("").expect("parses").cross_origin);

        // `null` is a sandboxed context. It cannot be this share.
        assert!(req("Origin: null\r\n").expect("parses").cross_origin);
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
        // `h5i join`, so after Ctrl-C the origin is still controlled — and if
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
    fn a_forwarded_head_never_carries_the_credential_or_two_framings() {
        const COOKIE: &str = "h5i_share";
        let mut rng = Rng::new(0x5EED);
        // Counted, and asserted on at the end. A generator that stops
        // producing heads the parser will accept turns this whole test into an
        // expensive way of running `parse` and discarding the answer — which is
        // what it was: measured against the real parser, 1.88% of heads got in,
        // *none* of two million carried both framings, and about one per run
        // carried a share cookie. It passed twenty million heads and proved
        // almost nothing it claimed to.
        let mut parsed = 0usize;
        let mut with_cookie = 0usize;
        let mut with_framing = 0usize;
        for i in 0..rounds() {
            let seed = rng.next();
            let mut one = Rng::new(seed);
            let head = request_head(&mut one);
            let ctx = || format!("round {i}, seed {seed:#x}, head {head:?}");

            let Some(req) = parse(&head, COOKIE) else {
                continue;
            };
            parsed += 1;
            if req.chunked || req.content_length.is_some() {
                with_framing += 1;
            }
            if head.to_ascii_lowercase().contains("cookie:") && req.token.is_some() {
                with_cookie += 1;
            }

            // A request cannot be both framings at once. Downstream picks one
            // and reads exactly that many bytes, so an input that sets both is
            // an input two parsers can disagree about.
            assert!(
                !(req.chunked && req.content_length.is_some()),
                "both framings survived parse: {}",
                ctx()
            );

            let out = rewrite_for_upstream(&head, &req, COOKIE);

            // The box must never see the credential that admitted its
            // visitor. Scoped to the headers and the request target — the two
            // places this proxy decides what to send — rather than to the whole
            // head: a client that puts the string in its own method or in a
            // header of its own invention is leaking a token it already holds
            // to a box it already reached, which is not a property this code
            // can or should enforce.
            // By cookie *name*, not by substring of the line. A cookie called
            // `999h5i_share` merely contains the string and is somebody else's
            // cookie; ours are the ones named `h5i_share` or `h5i_share_<port>`.
            for line in header_lines(&out) {
                if header_name_of(line) != "cookie" {
                    continue;
                }
                let value = line.split_once(':').map(|(_, v)| v).unwrap_or("");
                for c in value.split(';') {
                    let name = c.trim().split_once('=').map(|(k, _)| k).unwrap_or(c).trim();
                    assert!(
                        !name.starts_with(COOKIE),
                        "a share cookie reached the box: {} -> {line:?}",
                        ctx()
                    );
                }
            }

            // And not in the URL either. A token in the target lands in the
            // app's own logs and in `Referer` on every link out of the page,
            // which is the whole reason an authorized query redirects instead
            // of proxying.
            let target = out.split_whitespace().nth(1).unwrap_or("");
            for (k, v) in query_params(target) {
                assert!(
                    k != QUERY_PARAM || v.is_empty(),
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
        // reaching the code, and a test that silently stops doing so is worse
        // than no test because it reads as coverage.
        // Floors, well under what the generator achieves today (about 18%,
        // 0.8% and 0.8% of heads), so RNG variation cannot trip them but a
        // collapse will. Before this was measured the same three numbers were
        // 1.9%, 0.01% and 0.008%: the two-framings assertion had run on
        // essentially nothing, and "the credential never reaches the box" —
        // the property in the test's own name — on about one input per run.
        let n = rounds();
        let counts =
            format!("parsed {parsed}, framed {with_framing}, cookied {with_cookie}, of {n}");
        assert!(
            parsed * 10 > n,
            "the generator has stopped producing heads the parser accepts: {counts}"
        );
        assert!(
            with_framing * 500 > n,
            "almost nothing declared a body length, so the two-framings assertion had \
             nothing to work on: {counts}"
        );
        assert!(
            with_cookie * 500 > n,
            "almost nothing carried a credential, so the property this test is named for \
             was barely checked: {counts}"
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
        let up = rewrite_for_upstream(&raw, &r, COOKIE);
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
        let up = rewrite_for_upstream(&raw, &r, COOKIE);
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
        // request, so a lone `Upgrade:` header — which any client can attach to
        // an ordinary request that will never upgrade — must not set it.
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
        // else — which is how a header walks past an inspecting proxy. There is
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
        let up = rewrite_for_upstream(&raw, &r, COOKIE);
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
        let up = rewrite_for_upstream(&raw, &r, COOKIE);
        assert!(up.contains("Connection: Upgrade"), "{up}");
        assert!(!up.contains("Connection: close"), "{up}");
    }

    #[test]
    fn a_cookie_header_with_nothing_left_in_it_is_dropped_entirely() {
        let raw = head("GET / HTTP/1.1", &["Cookie: h5i_share=abc123"]);
        let r = parse_default(&raw).expect("parse");
        let up = rewrite_for_upstream(&raw, &r, COOKIE);
        assert!(!up.to_lowercase().contains("cookie:"), "{up}");
    }

    #[test]
    fn the_header_name_is_matched_however_the_client_capitalised_it() {
        let raw = head("GET / HTTP/1.1", &["COOKIE: h5i_share=abc123"]);
        let r = parse_default(&raw).expect("parse");
        assert_eq!(r.token.as_deref(), Some("abc123"));
        assert!(!rewrite_for_upstream(&raw, &r, COOKIE).contains("abc123"));
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
        // browser sends every 127.0.0.1 cookie to every 127.0.0.1 listener —
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
        let up = rewrite_for_upstream(&raw, &r, &a);
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
        // malformed line to the box — which is a smuggled request, by a door
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
        let up = rewrite_for_upstream(&raw, &r, COOKIE);
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
        assert!(!rewrite_for_upstream(&raw, &r, COOKIE).contains("abc123"));
    }

    #[test]
    fn headers_two_parsers_would_fold_differently_are_refused() {
        // Obs-fold: `X-Pad: a` continued onto the next line is one header to a
        // server that implements folding and two to anything splitting on CRLF
        // — no body to one, a 35-byte body to the other.
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
        // for chunks that would never arrive — holding a slot for free.
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
        // entitled to — and an app with its own `h5i` parameter did it too.
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
        let up = rewrite_for_upstream(&raw, &r, &a);
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
