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
pub const COOKIE: &str = "h5i_share";
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
    /// A WebSocket (or other) upgrade. Proxied like anything else once
    /// authorized — hot reload is a WebSocket, and a share where the page
    /// loads but never updates is not a share of a dev server.
    pub upgrade: bool,
}

/// Why a request was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// No credential at all, or one that no grant matches.
    NotAuthorized,
    /// The request was not something we are willing to parse.
    Malformed,
}

impl Refusal {
    pub fn status(self) -> (u16, &'static str) {
        match self {
            Refusal::NotAuthorized => (401, "Unauthorized"),
            Refusal::Malformed => (400, "Bad Request"),
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
    !token.is_empty()
        && token.len() <= 128
        && token.bytes().all(|b| b.is_ascii_alphanumeric())
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
/// go upstream — which is to say, without it.
fn split_cookie(value: &str) -> (Option<String>, String) {
    let mut ours = None;
    let kept: Vec<&str> = value
        .split(';')
        .filter(|c| {
            let c = c.trim();
            match c.split_once('=') {
                Some((k, v)) if k.trim() == COOKIE => {
                    ours = Some(v.trim().to_string());
                    false
                }
                _ => !c.is_empty(),
            }
        })
        .map(|c| c.trim())
        .collect();
    (ours, kept.join("; "))
}

/// Read what the gate needs. `None` when this is not an HTTP/1 request head.
pub fn parse(head: &str) -> Option<Request> {
    let (request_line, headers) = lines(head)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let version = parts.next()?;
    if !version.starts_with("HTTP/") || parts.next().is_some() {
        return None;
    }
    let (clean_target, query_token) = strip_param(&target);
    let cookie_token = header(&headers, "cookie").and_then(|c| split_cookie(c).0);
    let from_query = query_token.is_some();
    let token = query_token
        .or(cookie_token)
        .filter(|t| plausible(t));
    let upgrade = header(&headers, "upgrade").is_some_and(|u| !u.trim().is_empty());
    Some(Request {
        method,
        target,
        clean_target,
        from_query,
        token,
        upgrade,
    })
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
pub fn rewrite_for_upstream(head: &str, req: &Request) -> String {
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
            let (_, kept) = split_cookie(v.trim());
            if !kept.is_empty() {
                out.push_str(&format!("Cookie: {kept}\r\n"));
            }
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
pub fn set_cookie_redirect(token: &str, location: &str, secure: bool) -> String {
    let attrs = if secure {
        "Path=/; HttpOnly; SameSite=Lax; Secure"
    } else {
        "Path=/; HttpOnly; SameSite=Lax"
    };
    format!(
        "HTTP/1.1 302 Found\r\n\
         Location: {location}\r\n\
         Set-Cookie: {COOKIE}={token}; {attrs}\r\n\
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
mod tests {
    use super::*;

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
        let r = parse(&head("GET /?h5i=abc123 HTTP/1.1", &[])).expect("parse");
        assert_eq!(r.token.as_deref(), Some("abc123"));
        assert!(r.from_query);
        assert_eq!(r.clean_target, "/");
    }

    #[test]
    fn later_requests_carry_it_in_the_cookie() {
        let r = parse(&head(
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
        let r = parse(&raw).expect("parse");
        assert_eq!(r.clean_target, "/search?q=rust&page=2");
        let up = rewrite_for_upstream(&raw, &r);
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
        let r = parse(&raw).expect("parse");
        let up = rewrite_for_upstream(&raw, &r);
        assert!(!up.contains("SECRETTOKEN"), "credential leaked upstream: {up}");
        assert!(!up.contains("h5i_share"));
        assert!(!up.contains("h5i="));
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
            &["Cookie: h5i_share=abc123", "Connection: keep-alive", "Keep-Alive: timeout=60"],
        );
        let r = parse(&raw).expect("parse");
        let up = rewrite_for_upstream(&raw, &r);
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
            &["Cookie: h5i_share=abc123", "Connection: Upgrade", "Upgrade: websocket"],
        );
        let r = parse(&raw).expect("parse");
        let up = rewrite_for_upstream(&raw, &r);
        assert!(up.contains("Connection: Upgrade"), "{up}");
        assert!(!up.contains("Connection: close"), "{up}");
    }

    #[test]
    fn a_cookie_header_with_nothing_left_in_it_is_dropped_entirely() {
        let raw = head("GET / HTTP/1.1", &["Cookie: h5i_share=abc123"]);
        let r = parse(&raw).expect("parse");
        let up = rewrite_for_upstream(&raw, &r);
        assert!(!up.to_lowercase().contains("cookie:"), "{up}");
    }

    #[test]
    fn the_header_name_is_matched_however_the_client_capitalised_it() {
        let raw = head("GET / HTTP/1.1", &["COOKIE: h5i_share=abc123"]);
        let r = parse(&raw).expect("parse");
        assert_eq!(r.token.as_deref(), Some("abc123"));
        assert!(!rewrite_for_upstream(&raw, &r).contains("abc123"));
    }

    #[test]
    fn an_upgrade_is_recognised_so_hot_reload_can_be_proxied() {
        let r = parse(&head(
            "GET /_next/webpack-hmr HTTP/1.1",
            &["Upgrade: websocket", "Connection: Upgrade", "Cookie: h5i_share=abc"],
        ))
        .expect("parse");
        assert!(r.upgrade);
        assert_eq!(r.token.as_deref(), Some("abc"));
    }

    #[test]
    fn a_token_that_could_not_be_one_is_not_carried_forward() {
        // Keeps absurd input away from the authorization path entirely.
        for bad in ["", &"a".repeat(200), "abc/../../etc", "a b"] {
            let r = parse(&head("GET / HTTP/1.1", &[&format!("Cookie: h5i_share={bad}")]))
                .expect("parse");
            assert_eq!(r.token, None, "accepted a token of {:?}", bad);
        }
    }

    #[test]
    fn the_query_wins_over_a_stale_cookie() {
        // Following a fresh invite link must replace an old grant's cookie,
        // or a revoked peer with a stale cookie could never be re-admitted.
        let r = parse(&head("GET /?h5i=fresh HTTP/1.1", &["Cookie: h5i_share=stale"]))
            .expect("parse");
        assert_eq!(r.token.as_deref(), Some("fresh"));
        assert!(r.from_query);
    }

    #[test]
    fn something_that_is_not_http_is_refused_rather_than_guessed_at() {
        assert!(parse("").is_none());
        assert!(parse("GET\r\n\r\n").is_none());
        assert!(parse("GET / HTTP/1.1 extra\r\n\r\n").is_none());
        assert!(parse("\x16\x03\x01 this is a TLS hello\r\n\r\n").is_none());
    }

    #[test]
    fn the_redirect_sets_the_cookie_and_drops_the_token_from_the_url() {
        let r = set_cookie_redirect("abc123", "/dashboard", true);
        assert!(r.starts_with("HTTP/1.1 302 "));
        assert!(r.contains("Location: /dashboard"));
        assert!(r.contains("Set-Cookie: h5i_share=abc123; Path=/; HttpOnly; SameSite=Lax; Secure"));
        // Loopback is not https, and a Secure cookie there is a cookie some
        // browsers will refuse to store.
        assert!(!set_cookie_redirect("abc123", "/", false).contains("Secure"));
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
