//! The cookie jar, and the four narrowings that make it safe to have one.
//!
//! Cookies are the first thing this engine holds that is worth stealing. Until
//! now a session had no memory at all, so there was nothing an injected page
//! could aim at; a jar changes that, and the defences have to arrive with it
//! rather than after it (ROADMAP §11 item 5.5).
//!
//! # Host-only, always
//!
//! **The `Domain` attribute is ignored.** A cookie is scoped to the exact host
//! that set it, and to nothing else.
//!
//! Honouring `Domain` correctly requires a public suffix list, because the rule
//! a browser actually enforces is "the domain must not be a public suffix" —
//! without it, a page on `evil.co.uk` may set `Domain=co.uk` and every later
//! request to `bank.co.uk` carries the cookie. There is no PSL in this
//! dependency tree, an embedded copy is a list that silently rots, and the
//! failure mode of getting it wrong is handing a credential to an attacker's
//! neighbour.
//!
//! The narrowing is real and worth stating: a site that logs you in at
//! `example.com` and serves the app from `www.example.com` will not stay logged
//! in here. That is a missing feature. Sending a session cookie to the wrong
//! origin would be a vulnerability, and between the two this engine takes the
//! missing feature.
//!
//! # In memory, never on disk
//!
//! The jar lives in the process and dies with it. Nothing here writes a
//! credential to a filesystem the box shares with anything, and "restart the
//! session" is a complete logout.
//!
//! # Never readable by the agent
//!
//! No verb returns a cookie value, and the request log records *how many*
//! cookies crossed rather than which. An agent driving this engine can be
//! logged in without ever being able to read the credential that makes it so,
//! which is the property that makes a stolen snapshot worth less than a stolen
//! jar.
//!
//! # Secure is enforced, not decorative
//!
//! A cookie set over https is never sent over http, so a downgrade cannot
//! collect it. `__Secure-` and `__Host-` prefixes are enforced at store time.

use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use url::Url;

/// One stored cookie, already scoped to the host that set it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cookie {
    name: String,
    value: String,
    /// The exact host this may be sent to. See the module docs on why there is
    /// no domain-matching here.
    host: String,
    path: String,
    /// `None` is a session cookie, which in this engine means "until the
    /// process exits" — the same thing, since nothing is persisted.
    expires: Option<SystemTime>,
    secure: bool,
    /// Withheld from `document.cookie`, as a browser withholds it.
    ///
    /// Parsed and dropped before this existed, which mattered the moment page
    /// script could read cookies: a session credential is almost always
    /// `HttpOnly`, and honouring the flag is what lets `document.cookie` exist
    /// at all without handing an agent the thing it must not be able to read.
    http_only: bool,
}

impl Cookie {
    fn is_expired(&self, now: SystemTime) -> bool {
        self.expires.is_some_and(|at| at <= now)
    }
}

/// What a request carried, for the record — counts, never values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CookieActivity {
    /// How many cookies this request sent.
    pub sent: usize,
    /// How many `Set-Cookie` headers the response asked to store, *after*
    /// refusals. A header this jar rejected is not counted as stored.
    pub stored: usize,
}

/// The session's cookies.
#[derive(Default)]
pub struct Jar {
    cookies: Mutex<Vec<Cookie>>,
}

impl Jar {
    pub fn new() -> Self {
        Self::default()
    }

    /// The `Cookie` header for this request, and how many it carries.
    ///
    /// `None` when nothing matches, so the caller sends no header at all
    /// rather than an empty one.
    pub fn header_for(&self, url: &Url) -> Option<(String, usize)> {
        self.header_for_at(url, SystemTime::now())
    }

    fn header_for_filtered(
        &self,
        url: &Url,
        keep: impl Fn(&Cookie) -> bool,
    ) -> Option<(String, usize)> {
        self.header_for_inner(url, SystemTime::now(), &keep)
    }

    fn header_for_at(&self, url: &Url, now: SystemTime) -> Option<(String, usize)> {
        self.header_for_inner(url, now, &|_| true)
    }

    fn header_for_inner(
        &self,
        url: &Url,
        now: SystemTime,
        keep: &dyn Fn(&Cookie) -> bool,
    ) -> Option<(String, usize)> {
        let host = url.host_str()?.to_ascii_lowercase();
        let secure_channel = is_secure(url);
        // The request's own path, not the default-path derivation below: that
        // one exists only to give a cookie a Path when the attribute is absent,
        // and using it here made `/admin` fail to match a cookie set at
        // `Path=/admin` because the derivation had already stripped it to `/`.
        let request_path = match url.path() {
            "" => "/".to_string(),
            path => path.to_string(),
        };

        let mut jar = self.cookies.lock().ok()?;
        // Expired cookies are dropped on the way past rather than swept
        // separately: a cookie the server deleted must not linger for a later
        // request, and this is the only place every request passes through.
        jar.retain(|c| !c.is_expired(now));

        let mut matched: Vec<&Cookie> = jar
            .iter()
            .filter(|c| c.host == host)
            .filter(|c| !c.secure || secure_channel)
            .filter(|c| path_matches(&request_path, &c.path))
            .filter(|c| keep(c))
            .collect();

        if matched.is_empty() {
            return None;
        }

        // RFC 6265 §5.4: longer paths first. Servers that read only the first
        // occurrence of a name then get the more specific one.
        matched.sort_by(|a, b| b.path.len().cmp(&a.path.len()).then(a.name.cmp(&b.name)));

        let count = matched.len();
        let header = matched
            .iter()
            .map(|c| format!("{}={}", c.name, c.value))
            .collect::<Vec<_>>()
            .join("; ");
        Some((header, count))
    }

    /// Take the `Set-Cookie` headers from a response. Returns how many were
    /// actually stored, which is not how many arrived.
    pub fn store<'a>(&self, url: &Url, headers: impl IntoIterator<Item = &'a str>) -> usize {
        self.store_at(url, headers, SystemTime::now())
    }

    fn store_at<'a>(
        &self,
        url: &Url,
        headers: impl IntoIterator<Item = &'a str>,
        now: SystemTime,
    ) -> usize {
        let Some(host) = url.host_str().map(|h| h.to_ascii_lowercase()) else {
            return 0;
        };
        let Ok(mut jar) = self.cookies.lock() else {
            return 0;
        };

        let mut stored = 0;
        for header in headers {
            let Some(cookie) = parse_set_cookie(header, &host, url, now) else {
                continue;
            };

            // Replacing by identity is what makes deletion work: a server
            // clears a cookie by re-sending it with an expiry in the past, so
            // the removal has to happen through the same door as the write.
            jar.retain(|c| !(c.name == cookie.name && c.host == cookie.host && c.path == cookie.path));

            if cookie.is_expired(now) {
                // Stored as a deletion, which is a real outcome and is counted
                // as one: "the server cleared your session" is a thing a
                // reviewer wants to see in the record.
                stored += 1;
                continue;
            }
            jar.push(cookie);
            stored += 1;
        }
        stored
    }

    /// How many cookies are held. For `doctor` and for tests — never the values.
    pub fn len(&self) -> usize {
        self.cookies.lock().map(|j| j.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The `document.cookie` value for this document: the non-`HttpOnly`
    /// cookies that match it, in the same form a browser exposes.
    ///
    /// Separate from [`Self::header_for`] because they answer different
    /// questions. That one is what crosses the wire; this is what *page script*
    /// may see, and the difference is the whole reason `HttpOnly` exists.
    pub fn document_cookie(&self, url: &Url) -> String {
        let Some((header, _)) = self.header_for_filtered(url, |c| !c.http_only) else {
            return String::new();
        };
        header
    }

    /// Forget everything. A complete logout, and what a session reset means.
    pub fn clear(&self) {
        if let Ok(mut jar) = self.cookies.lock() {
            jar.clear();
        }
    }

    /// Drop everything when a navigation leaves the origin that set it.
    ///
    /// The box replaces three of Chromium's four process-model reasons and not
    /// the fourth: it protects the host from the box, and says nothing about two
    /// origins sharing one address space. That did not matter until this engine
    /// held cookies *and* ran script. It cannot be fixed without a process
    /// split, so it is bounded instead — at any moment the jar holds only
    /// cookies for the origin currently loaded, so a foreign origin's script
    /// never runs alongside someone else's live session.
    ///
    /// The cost is real and belongs next to the guarantee: a login does not
    /// survive a redirect through another origin, so OAuth-style flows that
    /// bounce via an identity provider will not stay signed in.
    ///
    /// Returns whether anything was dropped, so a caller can say so rather than
    /// leaving an agent to discover it by being logged out.
    pub fn retain_origin(&self, origin: &Url) -> bool {
        let Some(host) = origin.host_str().map(|h| h.to_ascii_lowercase()) else {
            return false;
        };
        let Ok(mut jar) = self.cookies.lock() else {
            return false;
        };
        let before = jar.len();
        jar.retain(|cookie| cookie.host == host);
        before != jar.len()
    }
}

/// Loopback over http is still a first-party channel, and the dev server is
/// the whole reason this engine reaches loopback at all. Treating it as
/// insecure would make `Secure` cookies untestable against it.
fn is_secure(url: &Url) -> bool {
    if url.scheme() == "https" {
        return true;
    }
    matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"))
}

/// RFC 6265 §5.1.4 default-path: the request path with its last segment
/// removed. Used **only** to fill in a missing `Path` attribute.
fn default_path(path: &str) -> String {
    if path.is_empty() || !path.starts_with('/') {
        return "/".to_string();
    }
    match path.rfind('/') {
        Some(0) => "/".to_string(),
        Some(cut) => path[..cut].to_string(),
        None => "/".to_string(),
    }
}

/// RFC 6265 §5.1.4 path-match.
fn path_matches(request_path: &str, cookie_path: &str) -> bool {
    if request_path == cookie_path {
        return true;
    }
    if !request_path.starts_with(cookie_path) {
        return false;
    }
    cookie_path.ends_with('/') || request_path.as_bytes().get(cookie_path.len()) == Some(&b'/')
}

/// Parse one `Set-Cookie` value, or refuse it.
///
/// Refusals are silent by design at this layer — a malformed or disallowed
/// cookie is not an error the page gets to raise — but they are visible in the
/// count the caller records, which is where a reviewer would notice.
fn parse_set_cookie(header: &str, host: &str, url: &Url, now: SystemTime) -> Option<Cookie> {
    let mut parts = header.split(';');
    let pair = parts.next()?.trim();
    let (name, value) = pair.split_once('=')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let value = value.trim().trim_matches('"').to_string();

    let mut path: Option<String> = None;
    let mut secure = false;
    let mut http_only = false;
    let mut max_age: Option<i64> = None;
    let mut expires: Option<SystemTime> = None;

    for attribute in parts {
        let attribute = attribute.trim();
        let (key, val) = match attribute.split_once('=') {
            Some((k, v)) => (k.trim().to_ascii_lowercase(), v.trim().to_string()),
            None => (attribute.to_ascii_lowercase(), String::new()),
        };
        match key.as_str() {
            "secure" => secure = true,
            "httponly" => http_only = true,
            "path" if val.starts_with('/') => path = Some(val),
            "max-age" => max_age = val.parse::<i64>().ok(),
            "expires" => expires = httpdate::parse_http_date(&val).ok(),
            // `Domain` is read and deliberately dropped; see the module docs.
            // `SameSite` is a no-op because this engine makes one navigation at
            // a time on the agent's behalf, so there is no third-party context
            // to be lax about. `HttpOnly` used to be one too, on the reasoning
            // that there was no script to hide a cookie from. There is now.
            _ => {}
        }
    }

    // Max-Age wins over Expires (RFC 6265 §5.2.2), and a non-positive one is
    // an immediate deletion.
    let expires = match max_age {
        Some(seconds) if seconds > 0 => Some(now + Duration::from_secs(seconds as u64)),
        Some(_) => Some(now),
        None => expires,
    };

    let path = path.unwrap_or_else(|| default_path(url.path()));

    // Cookie name prefixes, enforced rather than parsed and ignored. These are
    // the one part of the spec that exists purely to stop a weaker channel from
    // overwriting a stronger one's cookie, so honouring them is free integrity.
    if name.starts_with("__Secure-") && !secure {
        return None;
    }
    if name.starts_with("__Host-") && (!secure || path != "/") {
        return None;
    }

    Some(Cookie {
        name: name.to_string(),
        value,
        host: host.to_string(),
        path,
        expires,
        secure,
        http_only,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).expect("test url")
    }

    #[test]
    fn a_cookie_comes_back_to_the_host_that_set_it() {
        let jar = Jar::new();
        assert_eq!(jar.store(&url("https://a.example/"), ["sid=abc"]), 1);

        let (header, count) = jar.header_for(&url("https://a.example/page")).expect("sent");
        assert_eq!(header, "sid=abc");
        assert_eq!(count, 1);
    }

    #[test]
    fn a_cookie_never_reaches_another_host_even_a_sibling() {
        // The narrowing this jar is built around. `Domain=.example` would make
        // a browser send this to both; here it goes nowhere but the setter.
        let jar = Jar::new();
        jar.store(&url("https://a.example/"), ["sid=abc; Domain=.example"]);

        assert!(jar.header_for(&url("https://b.example/")).is_none());
        assert!(jar.header_for(&url("https://example/")).is_none());
        assert!(
            jar.header_for(&url("https://a.example/")).is_some(),
            "the host that set it still gets it"
        );
    }

    #[test]
    fn a_secure_cookie_does_not_survive_a_downgrade() {
        let jar = Jar::new();
        jar.store(&url("https://a.example/"), ["sid=abc; Secure"]);
        assert!(jar.header_for(&url("http://a.example/")).is_none());
        assert!(jar.header_for(&url("https://a.example/")).is_some());
    }

    #[test]
    fn paths_are_matched_rather_than_prefixed() {
        let jar = Jar::new();
        jar.store(&url("https://a.example/"), ["s=1; Path=/admin"]);

        assert!(jar.header_for(&url("https://a.example/admin")).is_some());
        assert!(jar.header_for(&url("https://a.example/admin/x")).is_some());
        // The classic off-by-one: /administrator is not under /admin.
        assert!(jar.header_for(&url("https://a.example/administrator")).is_none());
        assert!(jar.header_for(&url("https://a.example/other")).is_none());
    }

    #[test]
    fn a_server_can_delete_a_cookie_which_is_how_logging_out_works() {
        let jar = Jar::new();
        jar.store(&url("https://a.example/"), ["sid=abc"]);
        assert_eq!(jar.len(), 1);

        let stored = jar.store(&url("https://a.example/"), ["sid=; Max-Age=0"]);
        assert_eq!(stored, 1, "a deletion is an outcome worth recording");
        assert_eq!(jar.len(), 0);
        assert!(jar.header_for(&url("https://a.example/")).is_none());
    }

    #[test]
    fn an_expiry_in_the_past_deletes_and_one_in_the_future_does_not() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let jar = Jar::new();

        jar.store_at(
            &url("https://a.example/"),
            ["old=1; Expires=Thu, 01 Jan 2015 00:00:00 GMT"],
            now,
        );
        assert_eq!(jar.len(), 0, "an already-expired cookie is not kept");

        jar.store_at(
            &url("https://a.example/"),
            ["new=1; Expires=Sat, 01 Jan 2050 00:00:00 GMT"],
            now,
        );
        assert!(jar.header_for_at(&url("https://a.example/"), now).is_some());
    }

    #[test]
    fn max_age_wins_over_expires_when_both_are_given() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let jar = Jar::new();
        // Expires says keep it; Max-Age says drop it. The spec says Max-Age.
        jar.store_at(
            &url("https://a.example/"),
            ["s=1; Max-Age=0; Expires=Sat, 01 Jan 2050 00:00:00 GMT"],
            now,
        );
        assert_eq!(jar.len(), 0);
    }

    #[test]
    fn a_cookie_is_replaced_rather_than_duplicated() {
        let jar = Jar::new();
        jar.store(&url("https://a.example/"), ["sid=one"]);
        jar.store(&url("https://a.example/"), ["sid=two"]);

        assert_eq!(jar.len(), 1);
        let (header, _) = jar.header_for(&url("https://a.example/")).unwrap();
        assert_eq!(header, "sid=two");
    }

    #[test]
    fn prefixed_names_are_enforced_not_merely_parsed() {
        let jar = Jar::new();
        // __Secure- without Secure, and __Host- with a path: both refused.
        assert_eq!(jar.store(&url("https://a.example/"), ["__Secure-s=1"]), 0);
        assert_eq!(
            jar.store(&url("https://a.example/"), ["__Host-s=1; Secure; Path=/admin"]),
            0
        );
        assert_eq!(
            jar.store(&url("https://a.example/"), ["__Host-s=1; Secure; Path=/"]),
            1
        );
    }

    #[test]
    fn several_cookies_are_sent_most_specific_path_first() {
        let jar = Jar::new();
        jar.store(&url("https://a.example/"), ["broad=1; Path=/"]);
        jar.store(&url("https://a.example/admin/x"), ["narrow=1; Path=/admin"]);

        let (header, count) = jar.header_for(&url("https://a.example/admin/x")).unwrap();
        assert_eq!(count, 2);
        assert!(
            header.starts_with("narrow=1"),
            "longer path first, per RFC 6265 §5.4: {header}"
        );
    }

    #[test]
    fn nonsense_is_dropped_rather_than_stored() {
        let jar = Jar::new();
        assert_eq!(jar.store(&url("https://a.example/"), ["", "=novalue", "novalue"]), 0);
        assert_eq!(jar.len(), 0);
    }

    #[test]
    fn loopback_counts_as_a_secure_channel_because_the_dev_server_is_first_party() {
        let jar = Jar::new();
        jar.store(&url("http://localhost:3000/"), ["sid=abc; Secure"]);
        assert!(jar.header_for(&url("http://localhost:3000/")).is_some());
    }

    #[test]
    fn leaving_an_origin_drops_its_session() {
        // The site-isolation bound: with script and cookies in one address
        // space, the jar must never hold one origin's credential while another
        // origin's script is running.
        let jar = Jar::new();
        jar.store(&url("https://bank.example/"), ["sid=secret"]);
        assert_eq!(jar.len(), 1);

        assert!(
            jar.retain_origin(&url("https://evil.example/page")),
            "navigating away drops it, and says that it did"
        );
        assert!(jar.is_empty());
        assert!(jar.header_for(&url("https://bank.example/")).is_none());
    }

    #[test]
    fn staying_on_an_origin_keeps_the_session() {
        let jar = Jar::new();
        jar.store(&url("https://bank.example/"), ["sid=secret"]);

        assert!(
            !jar.retain_origin(&url("https://bank.example/account")),
            "a same-origin navigation drops nothing"
        );
        assert!(jar.header_for(&url("https://bank.example/account")).is_some());
    }

    #[test]
    fn clearing_is_a_complete_logout() {
        let jar = Jar::new();
        jar.store(&url("https://a.example/"), ["sid=abc"]);
        jar.clear();
        assert!(jar.is_empty());
        assert!(jar.header_for(&url("https://a.example/")).is_none());
    }
}

#[cfg(test)]
mod http_only_tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).expect("test url")
    }

    #[test]
    fn http_only_cookies_cross_the_wire_but_never_reach_script() {
        // The distinction `document.cookie` rests on. A session credential is
        // almost always HttpOnly, so honouring the flag is what lets page
        // script read cookies at all without handing an agent — which can read
        // whatever script writes into the DOM — the thing it must not have.
        let jar = Jar::new();
        jar.store(
            &url("https://app.example/"),
            ["sid=secret; HttpOnly", "theme=dark"],
        );

        let (wire, count) = jar.header_for(&url("https://app.example/")).expect("sent");
        assert!(wire.contains("sid=secret"), "the wire carries both: {wire}");
        assert!(wire.contains("theme=dark"), "{wire}");
        assert_eq!(count, 2);

        let visible = jar.document_cookie(&url("https://app.example/"));
        assert!(!visible.contains("secret"), "script must not see it: {visible}");
        assert!(visible.contains("theme=dark"), "but does see the rest: {visible}");
    }

    #[test]
    fn document_cookie_is_empty_when_everything_is_http_only() {
        let jar = Jar::new();
        jar.store(&url("https://app.example/"), ["sid=secret; HttpOnly"]);
        assert_eq!(jar.document_cookie(&url("https://app.example/")), "");
    }
}
