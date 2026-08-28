//! The cookie jar, and the four narrowings that make it safe to have one.
//!
//! Cookies are the first thing this engine holds that is worth stealing. Until
//! now a session had no memory at all, so there was nothing an injected page
//! could aim at; a jar changes that, and the defences have to arrive with it
//! rather than after it (ROADMAP §11 item 5.5).
//!
//! # `Domain`, honoured over a public suffix list
//!
//! A cookie is host-only unless it says otherwise. When it does, the `Domain`
//! attribute is honoured under the rules a browser actually enforces, and the
//! load-bearing one needs a list: **the domain must not be a public suffix.**
//! Without that, a page on `evil.co.uk` sets `Domain=co.uk` and every later
//! request to `bank.co.uk` carries the cookie.
//!
//! This was refused for exactly that reason until the list arrived. The stated
//! cost was real and is now paid off: a site that logs you in at `example.com`
//! and serves the app from `www.example.com` stays logged in. Four rules stand
//! between that and the failure above, and a cookie must pass all of them:
//!
//! 1. **The domain must not be a public suffix.** `Domain=co.uk`,
//!    `Domain=com`, `Domain=github.io` are all refused.
//! 2. **The request host must be within it**, on a label boundary.
//!    `Domain=example.com` may not be set by `attackerexample.com`, which a
//!    plain suffix test would have allowed.
//! 3. **A host that is an IP address gets no `Domain` at all.** There is no
//!    domain tree above an address to widen into.
//! 4. **`__Host-` still forbids it outright**, which is the prefix's whole
//!    purpose: it is how a server says "this one is mine alone".
//!
//! The list is compiled in (the `psl` crate) rather than fetched, so nothing
//! here depends on the network to decide where a credential may go. It goes
//! stale between version bumps, and it does so safely: the list only grows, so
//! an out-of-date copy refuses suffixes it has not heard of rather than
//! accepting them.
//!
//! # SameSite, recorded and enforced where it can be
//!
//! Parsed and stored rather than dropped. `SameSite=None` without `Secure` is
//! refused at store time, which is the rule that stops a cross-site cookie
//! travelling in the clear. The cross-site *request* distinction itself is
//! computed on registrable domains — the same list, so `a.example.com` and
//! `b.example.com` are one site — rather than on bare host equality, which
//! would have called every subdomain a third party.
//!
//! # In memory unless h5i asks for a file, and it has to ask by name
//!
//! The jar lives in the process and dies with it. That was the whole story
//! until `h5i browser open --restore` needed a jar to inherit and there was
//! none to inherit — the flag copied a `cookies.json` that nothing wrote, so a
//! login could never outlive the session that performed it and the help text
//! said otherwise (ROADMAP §B19.6).
//!
//! So there is now exactly one way to put this on a disk: [`Jar::persist_to`],
//! called by the engine only when h5i passed `--cookie-jar`, with a path h5i
//! chose inside the session's own directory. Four properties keep that from
//! being a hole:
//!
//! * **Nothing defaults it on.** No path, no file, and `open`/`read` never pass
//!   one — a one-shot read is still a complete logout at exit.
//! * **The file is owner-only** (`0600` on Unix), written through a temporary
//!   file and renamed, so a reader never sees half a jar and a crash never
//!   leaves a truncated one.
//! * **It is written when it changes, not at shutdown.** A session that is
//!   SIGKILLed — which is how `close` and `service_stop` end one — would lose a
//!   shutdown hook entirely, and a jar that survives only a polite exit is a
//!   jar that is missing exactly when it is needed.
//! * **It stays unreadable to the agent.** No verb returns a cookie value, and
//!   this adds none. The file is for the *next session*, reached through
//!   `--restore`, which hands it back to another engine and never to a model.
//!
//! What is written is what the jar holds, and [`Jar::retain_origin`] means that
//! is one origin's cookies rather than a browsing history. That is a narrower
//! artifact than a browser's cookie store and it is the right size for what it
//! is for.
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

/// How a cookie says it may travel across sites.
///
/// Stored rather than dropped. Only one of these is enforceable here — `None`
/// requires `Secure`, checked at store time — but the value is what a
/// cross-site decision would be made from, and parsing an attribute only to
/// throw it away is how a jar ends up unable to answer a question it already
/// had the information for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SameSite {
    Strict,
    #[default]
    Lax,
    None,
}

/// One stored cookie, already scoped to the host that set it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cookie {
    name: String,
    value: String,
    /// The host that set it, lowercased.
    ///
    /// With `host_only` set this is the only host it may be sent to. Otherwise
    /// it is the domain the cookie widened to, and any host at or below it
    /// matches — see [`domain_matches`].
    host: String,
    /// Whether `Domain` was absent, which is the default and the narrow case.
    ///
    /// Kept as its own flag rather than inferred from `host`, because
    /// `Domain=example.com` set *by* `example.com` is not the same cookie as
    /// one set without the attribute: the first is sent to `www.example.com`
    /// and the second is not. RFC 6265 §5.3 draws the line here and a jar that
    /// inferred it would send credentials one hop wider than the server asked.
    host_only: bool,
    same_site: SameSite,
    path: String,
    /// `None` is a session cookie: it lives until the process exits.
    ///
    /// It used to be true that this was "the same thing" as a browser's rule,
    /// because nothing was persisted. With [`Jar::persist_to`] it is not: a
    /// session cookie *is* written to the jar file, on purpose, because
    /// `--restore` exists to carry a login forward and the cookie that says
    /// "signed in" is usually this one. See [`StoredCookie::expires`].
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

/// Who is setting a cookie. `HttpOnly` exists to tell these two apart, so the
/// jar has to know which it is talking to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Setter {
    /// A `Set-Cookie` header on a response.
    Wire,
    /// `document.cookie = …` from page script.
    Script,
}

/// The session's cookies.
#[derive(Default)]
pub struct Jar {
    cookies: Mutex<Vec<Cookie>>,
    /// Where the jar is mirrored, when h5i named a file. See the module header.
    file: Mutex<Option<std::path::PathBuf>>,
}

/// One cookie as it appears in the file.
///
/// A mirror of [`Cookie`] rather than a derive on it, so the on-disk shape is a
/// deliberate thing that changes when someone edits *this* struct. A derive
/// would make the format a side effect of a field rename, and this file is read
/// back by a later process that may be a different build.
#[derive(serde::Serialize, serde::Deserialize)]
struct StoredCookie {
    name: String,
    value: String,
    host: String,
    host_only: bool,
    /// `"strict"`, `"lax"` or `"none"`.
    same_site: String,
    path: String,
    /// Unix seconds. Absent is a session cookie.
    ///
    /// Persisted rather than dropped, which is a deliberate difference from a
    /// browser's disk jar. A browser discards session cookies on exit because
    /// the user is closing a window; this file exists so `--restore` can carry
    /// a login forward, and the cookie that says "you are signed in" is very
    /// often exactly this one. Dropping it would make the feature not work
    /// while looking like it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires: Option<u64>,
    secure: bool,
    http_only: bool,
}

/// The file itself.
#[derive(serde::Serialize, serde::Deserialize)]
struct JarFile {
    /// Bumped when the shape changes. A file from the future is refused rather
    /// than half-read: a jar read wrongly is a login that half-works, which is
    /// worse to debug than one that plainly did not.
    version: u32,
    cookies: Vec<StoredCookie>,
}

/// The only version this build writes and the only one it reads.
const JAR_VERSION: u32 = 1;

impl Jar {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mirror this jar to `path` from now on, and read whatever is there.
    ///
    /// Called by the engine only when h5i passed `--cookie-jar`. Returns how
    /// many cookies were loaded, or an error describing a file that could not
    /// be read as a jar — never silence, because a `--restore` that silently
    /// loaded nothing is the defect this whole feature exists to fix.
    ///
    /// A missing file is not an error: it is the first run.
    pub fn persist_to(&self, path: &std::path::Path) -> Result<usize, String> {
        let loaded = match std::fs::read_to_string(path) {
            Ok(text) => self.load_json(&text)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(e) => return Err(format!("could not read `{}`: {e}", path.display())),
        };
        if let Ok(mut slot) = self.file.lock() {
            *slot = Some(path.to_path_buf());
        }
        // Write once now, so the file exists from the moment the session does.
        // `--restore` asks "did this session leave a jar", and a session that
        // ended before storing anything should answer "yes, an empty one"
        // rather than look like a session that never had persistence on.
        self.flush();
        Ok(loaded)
    }

    /// Merge a jar file's contents into this jar, dropping what has expired.
    fn load_json(&self, text: &str) -> Result<usize, String> {
        if text.trim().is_empty() {
            return Ok(0);
        }
        let parsed: JarFile =
            serde_json::from_str(text).map_err(|e| format!("this is not a jar file: {e}"))?;
        if parsed.version != JAR_VERSION {
            return Err(format!(
                "jar file version {} was written by a different build of this engine; \
                 this one reads version {JAR_VERSION}",
                parsed.version
            ));
        }
        let now = SystemTime::now();
        let Ok(mut jar) = self.cookies.lock() else {
            return Err("the cookie jar is poisoned".to_string());
        };
        let mut loaded = 0;
        for stored in parsed.cookies {
            let cookie = Cookie {
                name: stored.name,
                value: stored.value,
                host: stored.host.to_ascii_lowercase(),
                host_only: stored.host_only,
                same_site: match stored.same_site.as_str() {
                    "strict" => SameSite::Strict,
                    "none" => SameSite::None,
                    // Anything unrecognised reads as the default a server that
                    // said nothing would have got. Narrower than `None`, so an
                    // unknown value cannot widen a cookie's reach.
                    _ => SameSite::Lax,
                },
                path: stored.path,
                expires: stored
                    .expires
                    .map(|secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs)),
                secure: stored.secure,
                http_only: stored.http_only,
            };
            if cookie.is_expired(now) {
                continue;
            }
            // Same identity rule the store path uses, so a reload does not
            // duplicate a cookie the jar already holds.
            jar.retain(|held| {
                !(held.name == cookie.name
                    && held.host == cookie.host
                    && held.path == cookie.path)
            });
            jar.push(cookie);
            loaded += 1;
        }
        Ok(loaded)
    }

    /// The jar as the file holds it. Public for the tests that check the shape.
    fn to_json(&self) -> String {
        let now = SystemTime::now();
        let cookies = match self.cookies.lock() {
            Ok(jar) => jar
                .iter()
                .filter(|c| !c.is_expired(now))
                .map(|c| StoredCookie {
                    name: c.name.clone(),
                    value: c.value.clone(),
                    host: c.host.clone(),
                    host_only: c.host_only,
                    same_site: match c.same_site {
                        SameSite::Strict => "strict",
                        SameSite::Lax => "lax",
                        SameSite::None => "none",
                    }
                    .to_string(),
                    path: c.path.clone(),
                    expires: c.expires.and_then(|at| {
                        at.duration_since(SystemTime::UNIX_EPOCH)
                            .ok()
                            .map(|d| d.as_secs())
                    }),
                    secure: c.secure,
                    http_only: c.http_only,
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        serde_json::to_string_pretty(&JarFile {
            version: JAR_VERSION,
            cookies,
        })
        .unwrap_or_else(|_| String::from("{}"))
    }

    /// Write the jar out, if h5i named a file.
    ///
    /// Silent on failure and deliberately so: a jar that cannot be mirrored is
    /// a session that will not be restorable, which is a loss of convenience,
    /// and turning it into a failed navigation would trade the page an agent
    /// asked for against a file nobody has asked for yet. The session's own
    /// receipts are the fail-closed record; this is not one.
    fn flush(&self) {
        let Ok(slot) = self.file.lock() else { return };
        let Some(path) = slot.as_ref() else { return };
        let text = self.to_json();
        // Temp-then-rename, so a reader never sees half a jar and a crash
        // mid-write never leaves a truncated one where a whole one was.
        let temporary = path.with_extension("json.tmp");
        if std::fs::write(&temporary, text.as_bytes()).is_err() {
            return;
        }
        restrict_to_owner(&temporary);
        let _ = std::fs::rename(&temporary, path);
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
            .filter(|c| domain_matches(&host, c))
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
        self.store_at(url, headers, SystemTime::now(), Setter::Wire)
    }

    /// Store a cookie **page script** set, through `document.cookie`.
    ///
    /// Deliberately not [`Self::store`], which is what `write_cookie` called.
    /// The wire and the script are not the same authority, and `HttpOnly` is the
    /// whole statement of that difference — so a browser enforces two rules here
    /// that the response path has no reason to:
    ///
    /// * **Script may not overwrite an `HttpOnly` cookie.** Replacement goes
    ///   through the same identity match as deletion, so `document.cookie =
    ///   "sid=attacker"` replaced the server's `HttpOnly` session cookie and the
    ///   jar then sent the attacker's value on the wire. Script could not *read*
    ///   the credential and could substitute one, which is session fixation —
    ///   and `document.cookie = "sid=; Max-Age=0"` was a logout the server never
    ///   asked for. Honouring the flag on reads and not on writes is honouring
    ///   half of it.
    /// * **Script may not *set* `HttpOnly`.** RFC 6265 §8.6 says a
    ///   set-cookie-string carrying that attribute from script is ignored
    ///   entirely, which is the rule that stops script planting a cookie it can
    ///   then hide behind.
    pub fn store_from_script(&self, url: &Url, header: &str) -> usize {
        self.store_at(url, [header], SystemTime::now(), Setter::Script)
    }

    fn store_at<'a>(
        &self,
        url: &Url,
        headers: impl IntoIterator<Item = &'a str>,
        now: SystemTime,
        setter: Setter,
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

            if setter == Setter::Script {
                // Ignored whole, per RFC 6265 §8.6.
                if cookie.http_only {
                    continue;
                }
                // And it may not stand on one the wire set.
                let shadows_http_only =
                    jar.iter().any(|c| c.http_only && same_cookie(c, &cookie));
                if shadows_http_only {
                    continue;
                }
            }

            // Replacing by identity is what makes deletion work: a server
            // clears a cookie by re-sending it with an expiry in the past, so
            // the removal has to happen through the same door as the write.
            jar.retain(|c| !same_cookie(c, &cookie));

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
        // Dropped before the write: `flush` takes the same lock to serialise.
        drop(jar);
        if stored > 0 {
            self.flush();
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
        // A logout has to reach the file too, or `--restore` would hand the
        // next session a credential this one deliberately threw away.
        self.flush();
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
        let dropped = before != jar.len();
        drop(jar);
        if dropped {
            self.flush();
        }
        dropped
    }
}

/// Make a file readable only by its owner.
///
/// The jar holds session credentials, so the default umask is not good enough:
/// on a box whose `/tmp` is shared with the host (the `agent` profile does
/// exactly this) a world-readable jar is a login anything on the machine can
/// pick up. Best-effort on Unix and a no-op elsewhere, because Windows has no
/// mode bits to set and pretending otherwise would be the more misleading of
/// the two behaviours.
fn restrict_to_owner(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Loopback over http is still a first-party channel, and the dev server is
/// the whole reason this engine reaches loopback at all. Treating it as
/// insecure would make `Secure` cookies untestable against it.
fn is_secure(url: &Url) -> bool {
    if url.scheme() == "https" {
        return true;
    }
    // `[::1]` with the brackets, because that is what `host_str` returns for an
    // IPv6 literal — the bare `::1` this listed could never match, so a dev
    // server on IPv6 loopback was the one first-party channel the rule above
    // did not cover.
    matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1" | "[::1]"))
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

/// Whether two cookies are the same one, for replacement and deletion.
///
/// RFC 6265 §5.3 identifies a cookie by name, domain and path — and *domain*
/// here includes whether it was host-only, because `Domain=example.com` set by
/// `example.com` and a bare cookie set by the same host are two different
/// cookies that a browser stores side by side. Comparing only the scope string
/// would have let a widened cookie silently delete the narrow one, which is a
/// logout the server never asked for.
fn same_cookie(a: &Cookie, b: &Cookie) -> bool {
    a.name == b.name && a.host == b.host && a.host_only == b.host_only && a.path == b.path
}

/// Whether a stored cookie may be sent to this host.
///
/// Host-only cookies match exactly. The rest match the domain they were scoped
/// to and anything below it, **on a label boundary** — the check that makes
/// `attackerexample.com` fail to match a cookie scoped to `example.com`, which
/// a bare `ends_with` would have let through.
fn domain_matches(request_host: &str, cookie: &Cookie) -> bool {
    if cookie.host_only {
        return request_host == cookie.host;
    }
    if request_host == cookie.host {
        return true;
    }
    request_host
        .strip_suffix(&cookie.host)
        .is_some_and(|prefix| prefix.ends_with('.'))
}

/// The registrable domain for a host: one label above its public suffix.
///
/// `www.example.co.uk` is `example.co.uk`; `example.co.uk` is itself; `co.uk`
/// is nothing, because a public suffix is not a domain anyone registers.
/// `None` for an IP address or an unknown shape, which every caller here treats
/// as "refuse to widen".
fn registrable_domain(host: &str) -> Option<String> {
    // The `psl` crate answers over bytes and returns the suffix; the domain is
    // the suffix plus one more label, which it computes for us.
    let domain = psl::domain(host.as_bytes())?;
    let text = std::str::from_utf8(domain.as_bytes()).ok()?;
    Some(text.to_ascii_lowercase())
}

/// Whether a host is a public suffix in its own right (`com`, `co.uk`,
/// `github.io`), which is the one thing a `Domain` may never be.
fn is_public_suffix(host: &str) -> bool {
    // A host with no registrable domain is either a suffix itself or a shape
    // the list does not describe. Both must refuse to widen, so both answer
    // true here — the conservative direction.
    match registrable_domain(host) {
        Some(domain) => domain != host,
        None => true,
    }
}

/// What host a cookie should be scoped to, given the `Domain` it asked for.
///
/// `None` refuses the cookie outright. The four rules in the module docs live
/// here, in the order that makes each one cheap.
fn scope_for(host: &str, requested: Option<&str>) -> Option<(String, bool)> {
    let Some(requested) = requested else {
        // No `Domain`: host-only, which is both the default and the narrow
        // case.
        return Some((host.to_string(), true));
    };

    // A leading dot is legal and historically meaningless (RFC 6265 §5.2.3).
    let wanted = requested
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase();
    if wanted.is_empty() {
        return Some((host.to_string(), true));
    }

    // Rule 3: an address has no domain tree above it to widen into. Checked
    // before the list, which does not describe addresses.
    if host.parse::<std::net::IpAddr>().is_ok() || wanted.parse::<std::net::IpAddr>().is_ok() {
        return (host == wanted).then(|| (host.to_string(), true));
    }

    // Rule 1: never a public suffix. This is the whole reason the list is here.
    if is_public_suffix(&wanted) {
        return None;
    }

    // Rule 2: the setter must be at or below what it is asking for, on a label
    // boundary. `attackerexample.com` asking for `example.com` fails here.
    let within = host == wanted
        || host
            .strip_suffix(&wanted)
            .is_some_and(|prefix| prefix.ends_with('.'));
    if !within {
        return None;
    }

    Some((wanted, false))
}

/// Whether two hosts are the same *site*, which is not the same question as
/// whether they are the same host.
///
/// Computed on registrable domains, so `app.example.com` and `api.example.com`
/// are one site. Host equality would have called them third parties to each
/// other, which is how a same-site rule ends up breaking the ordinary case it
/// was never aimed at.
#[allow(dead_code)]
fn same_site(a: &str, b: &str) -> bool {
    match (registrable_domain(a), registrable_domain(b)) {
        (Some(x), Some(y)) => x == y,
        // No registrable domain on either side (an address, say): fall back to
        // the strict answer rather than guessing generously.
        _ => a == b,
    }
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
    let mut domain: Option<String> = None;
    let mut same_site = SameSite::default();

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
            // Honoured now, over a public suffix list. Read and dropped before
            // that list existed, because the alternative to checking it is
            // sending a session cookie to an attacker's neighbour.
            "domain" if !val.is_empty() => domain = Some(val),
            "samesite" => {
                same_site = match val.to_ascii_lowercase().as_str() {
                    "strict" => SameSite::Strict,
                    "none" => SameSite::None,
                    // Anything unrecognised is the default rather than a
                    // refusal: an attribute a server spelled wrong should not
                    // silently widen the cookie.
                    _ => SameSite::Lax,
                }
            }
            _ => {}
        }
    }

    // Max-Age wins over Expires (RFC 6265 §5.2.2), and a non-positive one is
    // an immediate deletion.
    //
    // `checked_add`, because `SystemTime + Duration` **panics** on overflow and
    // the addend here is a number off the wire. `Max-Age=9223372036854775807`
    // in a `Set-Cookie` aborted the engine — any page the box's browser is
    // pointed at could end the session with one response header, which is a
    // page deciding whether the agent driving it keeps running.
    //
    // An expiry too far out to represent is stored as a session cookie rather
    // than clamped to some arbitrary date. In this jar the two are the same
    // thing: nothing is persisted, so "until the process exits" is already the
    // longest life a cookie can have (see the module docs).
    let expires = match max_age {
        Some(seconds) if seconds > 0 => now.checked_add(Duration::from_secs(seconds as u64)),
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
    // `__Host-` forbids `Domain` outright, which is the prefix's whole point:
    // it is how a server says this cookie is its host's alone and may not be
    // widened to a sibling.
    if name.starts_with("__Host-") && (!secure || path != "/" || domain.is_some()) {
        return None;
    }

    // `SameSite=None` is a cookie asking to travel cross-site, and the spec
    // requires it to be `Secure` for that. Refused rather than silently
    // downgraded: a server that asked for the wide behaviour should not get the
    // narrow one and no indication.
    if same_site == SameSite::None && !secure {
        return None;
    }

    // Where this cookie may go. `None` is a refusal — a public suffix, or a
    // host asking to widen into a domain it is not under.
    let (scope, host_only) = scope_for(host, domain.as_deref())?;

    Some(Cookie {
        name: name.to_string(),
        value,
        host: scope,
        host_only,
        same_site,
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
    fn a_cookie_without_domain_never_reaches_another_host_even_a_sibling() {
        // The default, and still the narrow one: no `Domain` means host-only,
        // so a sibling gets nothing.
        let jar = Jar::new();
        jar.store(&url("https://a.example.com/"), ["sid=abc"]);

        assert!(jar.header_for(&url("https://b.example.com/")).is_none());
        assert!(jar.header_for(&url("https://example.com/")).is_none());
        assert!(
            jar.header_for(&url("https://a.example.com/")).is_some(),
            "the host that set it still gets it"
        );
    }

    /// The narrowing this jar was built around, now paid off. A site that logs
    /// you in at `example.com` and serves the app from `www.example.com` used
    /// to log you straight back out.
    #[test]
    fn a_domain_cookie_reaches_subdomains_of_what_it_asked_for() {
        let jar = Jar::new();
        jar.store(
            &url("https://example.com/"),
            ["sid=abc; Domain=example.com"],
        );

        for host in [
            "https://example.com/",
            "https://www.example.com/",
            "https://deep.nested.example.com/",
        ] {
            assert!(
                jar.header_for(&url(host)).is_some(),
                "{host} is within the domain it was scoped to"
            );
        }
        assert!(
            jar.header_for(&url("https://other.com/")).is_none(),
            "and nothing outside it"
        );
    }

    /// The whole reason the list is here. Without it this cookie is stored and
    /// every later request to any `.co.uk` carries it.
    #[test]
    fn a_domain_that_is_a_public_suffix_is_refused() {
        let jar = Jar::new();
        for attempt in [
            ("https://evil.co.uk/", "sid=abc; Domain=co.uk"),
            ("https://evil.com/", "sid=abc; Domain=com"),
            ("https://evil.github.io/", "sid=abc; Domain=github.io"),
        ] {
            assert_eq!(
                jar.store(&url(attempt.0), [attempt.1]),
                0,
                "{} must not be storable",
                attempt.1
            );
        }
        assert!(jar.header_for(&url("https://bank.co.uk/")).is_none());
        assert!(jar.header_for(&url("https://victim.github.io/")).is_none());
    }

    /// The label-boundary check, which a bare suffix test would have failed.
    /// `attackerexample.com` *ends with* `example.com`.
    #[test]
    fn a_host_may_not_widen_into_a_domain_it_merely_ends_with() {
        let jar = Jar::new();
        assert_eq!(
            jar.store(
                &url("https://attackerexample.com/"),
                ["sid=abc; Domain=example.com"]
            ),
            0,
            "a suffix match is not a domain match"
        );
        assert!(jar.header_for(&url("https://example.com/")).is_none());
    }

    /// And the same boundary on the way out: a cookie scoped to `example.com`
    /// must not be sent to `notexample.com`.
    #[test]
    fn a_domain_cookie_is_not_sent_to_a_host_that_merely_ends_with_it() {
        let jar = Jar::new();
        jar.store(
            &url("https://example.com/"),
            ["sid=abc; Domain=example.com"],
        );
        assert!(jar.header_for(&url("https://notexample.com/")).is_none());
    }

    /// `__Host-` says "mine alone", so it may not be widened at all. This is
    /// the prefix doing the one job it exists for.
    #[test]
    fn host_prefixed_cookies_refuse_a_domain_outright() {
        let jar = Jar::new();
        assert_eq!(
            jar.store(
                &url("https://example.com/"),
                ["__Host-s=1; Secure; Path=/; Domain=example.com"]
            ),
            0
        );
        // Without the attribute the same cookie is fine.
        assert_eq!(
            jar.store(&url("https://example.com/"), ["__Host-s=1; Secure; Path=/"]),
            1
        );
    }

    /// There is no domain tree above an address to widen into.
    #[test]
    fn an_ip_address_host_gets_no_domain_widening() {
        let jar = Jar::new();
        assert_eq!(
            jar.store(&url("https://127.0.0.1/"), ["sid=abc; Domain=0.0.1"]),
            0
        );
        // Host-only still works, which is what a dev server needs.
        assert_eq!(jar.store(&url("https://127.0.0.1/"), ["sid=abc"]), 1);
        assert!(jar.header_for(&url("https://127.0.0.1/")).is_some());
    }

    /// A cookie asking to travel cross-site has to be `Secure` for it. Refused
    /// rather than quietly downgraded to the narrow behaviour.
    #[test]
    fn same_site_none_without_secure_is_refused() {
        let jar = Jar::new();
        assert_eq!(
            jar.store(&url("https://example.com/"), ["sid=abc; SameSite=None"]),
            0
        );
        assert_eq!(
            jar.store(
                &url("https://example.com/"),
                ["sid=abc; SameSite=None; Secure"]
            ),
            1
        );
    }

    /// A widened cookie and a host-only one of the same name are two cookies,
    /// as they are in a browser. Folding them together would let one delete
    /// the other — a logout the server never asked for.
    #[test]
    fn a_domain_cookie_does_not_replace_the_host_only_one_beside_it() {
        let jar = Jar::new();
        jar.store(&url("https://example.com/"), ["sid=narrow"]);
        jar.store(
            &url("https://example.com/"),
            ["sid=wide; Domain=example.com"],
        );

        let (header, count) = jar.header_for(&url("https://example.com/")).unwrap();
        assert_eq!(count, 2, "both are stored: {header}");
        assert!(header.contains("sid=narrow"), "{header}");
        assert!(header.contains("sid=wide"), "{header}");

        // And only the wide one reaches a subdomain.
        let (sub, count) = jar.header_for(&url("https://www.example.com/")).unwrap();
        assert_eq!(count, 1, "{sub}");
        assert!(sub.contains("sid=wide"), "{sub}");
    }

    /// Registrable-domain arithmetic, which both the `Domain` gate and the
    /// same-site decision rest on.
    #[test]
    fn registrable_domains_are_computed_over_the_list_not_by_counting_dots() {
        assert_eq!(
            registrable_domain("www.example.co.uk").as_deref(),
            Some("example.co.uk"),
            "a two-label suffix is one suffix"
        );
        assert_eq!(
            registrable_domain("example.com").as_deref(),
            Some("example.com")
        );
        assert!(is_public_suffix("co.uk"));
        assert!(is_public_suffix("com"));
        assert!(!is_public_suffix("example.com"));

        // Same site is computed here, not on host equality, or every subdomain
        // would be a third party to its own siblings.
        assert!(same_site("app.example.com", "api.example.com"));
        assert!(!same_site("example.com", "example.org"));
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
            Setter::Wire,
        );
        assert_eq!(jar.len(), 0, "an already-expired cookie is not kept");

        jar.store_at(
            &url("https://a.example/"),
            ["new=1; Expires=Sat, 01 Jan 2050 00:00:00 GMT"],
            now,
            Setter::Wire,
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
            Setter::Wire,
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

    /// A `Max-Age` off the wire is a number the page chose, and
    /// `SystemTime + Duration` panics on overflow. One response header ended
    /// the engine — and with it whatever the agent driving it was doing.
    #[test]
    fn an_absurd_max_age_does_not_end_the_session() {
        let jar = Jar::new();
        let u = url("https://a.example/");
        assert_eq!(jar.store(&u, [format!("s=1; Max-Age={}", i64::MAX).as_str()]), 1);
        // Too far out to represent, so it is held as a session cookie — which
        // in an in-memory jar is the same lifetime, and is still sent.
        let (header, _) = jar.header_for(&u).expect("still sent");
        assert_eq!(header, "s=1");

        // The neighbouring shapes stay as they were.
        assert_eq!(jar.store(&u, ["t=1; Max-Age=-1"]), 1);
        assert!(jar.header_for(&u).expect("s survives").0.contains("s=1"));
        assert_eq!(jar.store(&u, ["u=1; Max-Age=not-a-number"]), 1);
    }

    #[test]
    fn ipv6_loopback_is_a_first_party_channel_like_the_others() {
        // `host_str` serialises an IPv6 literal with its brackets, so the bare
        // `::1` in the list matched nothing and a `Secure` cookie set by a dev
        // server on `[::1]` was never sent back to it.
        let jar = Jar::new();
        jar.store(&url("http://[::1]:3000/"), ["sid=abc; Secure"]);
        assert!(jar.header_for(&url("http://[::1]:3000/")).is_some());
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

    /// `HttpOnly` is a statement about *script*, and it was being honoured in
    /// one direction only. `write_cookie` called `Jar::store` — the same door a
    /// `Set-Cookie` header comes through — and replacement there goes by
    /// name/host/path identity, so page script could substitute the server's
    /// session credential without ever being able to read it. That is session
    /// fixation, and the delete form is a logout the server never asked for.
    #[test]
    fn script_cannot_overwrite_or_clear_an_http_only_cookie() {
        let jar = Jar::new();
        let u = url("https://app.example/");
        jar.store(&u, ["sid=server-value; HttpOnly", "theme=dark"]);

        // Substitution is refused, and the wire still carries the real one.
        assert_eq!(jar.store_from_script(&u, "sid=attacker"), 0);
        let (wire, _) = jar.header_for(&u).expect("sent");
        assert!(wire.contains("sid=server-value"), "{wire}");
        assert!(!wire.contains("attacker"), "{wire}");

        // So is deletion.
        assert_eq!(jar.store_from_script(&u, "sid=; Max-Age=0"), 0);
        assert!(jar.header_for(&u).expect("still there").0.contains("sid=server-value"));

        // A cookie script is allowed to own is still script's to change.
        assert_eq!(jar.store_from_script(&u, "theme=light"), 1);
        assert!(jar.document_cookie(&u).contains("theme=light"));

        // And script may not *set* `HttpOnly` — RFC 6265 §8.6 ignores the whole
        // set-cookie-string, which is what stops it planting a cookie it can
        // then hide behind.
        assert_eq!(jar.store_from_script(&u, "planted=1; HttpOnly"), 0);
        assert!(!jar.header_for(&u).expect("sent").0.contains("planted"));

        // The wire is unaffected by all of this: a server may still replace its
        // own `HttpOnly` cookie, which is how a session is rotated.
        assert_eq!(jar.store(&u, ["sid=rotated; HttpOnly"]), 1);
        assert!(jar.header_for(&u).expect("sent").0.contains("sid=rotated"));
    }

    #[test]
    fn document_cookie_is_empty_when_everything_is_http_only() {
        let jar = Jar::new();
        jar.store(&url("https://app.example/"), ["sid=secret; HttpOnly"]);
        assert_eq!(jar.document_cookie(&url("https://app.example/")), "");
    }
}

/// The jar as a file: what `--restore` reads and what `--cookie-jar` writes.
///
/// Its own module because the questions are different from the ones above.
/// Those are about what a cookie *means*; these are about what survives a
/// process boundary, which is where a jar can quietly undo a rule it was
/// written beside.
#[cfg(test)]
mod persistence_tests {
    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).expect("test url")
    }

    // --- persistence (ROADMAP §B19.6, item 8) -----------------------------

    #[test]
    fn a_login_survives_into_the_next_session() {
        // The whole feature, in one assertion. Before this, `--restore` copied
        // a file nothing wrote, so the second jar came up empty and the flag
        // was a silent no-op.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cookies.json");
        let site = url("https://app.example/");

        let first = Jar::new();
        first.persist_to(&path).expect("first jar");
        first.store(&site, ["sid=s3cr3t; HttpOnly"]);
        assert!(path.exists(), "storing a cookie writes the jar");

        let second = Jar::new();
        let loaded = second.persist_to(&path).expect("second jar");
        assert_eq!(loaded, 1, "the next session inherits the login");
        let (header, count) = second.header_for(&site).expect("the cookie is sent");
        assert_eq!(count, 1);
        assert_eq!(header, "sid=s3cr3t");
    }

    #[test]
    fn http_only_survives_the_round_trip_and_stays_hidden_from_script() {
        // If `HttpOnly` were lost on the way through the file, a restored
        // session would hand page script the credential the original refused
        // it — persistence quietly undoing the flag it was written beside.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cookies.json");
        let site = url("https://app.example/");

        let first = Jar::new();
        first.persist_to(&path).expect("first");
        first.store(&site, ["sid=s3cr3t; HttpOnly", "theme=dark"]);

        let second = Jar::new();
        second.persist_to(&path).expect("second");
        assert_eq!(second.document_cookie(&site), "theme=dark");
    }

    #[test]
    fn an_expired_cookie_is_not_restored() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cookies.json");
        let site = url("https://app.example/");

        let first = Jar::new();
        first.persist_to(&path).expect("first");
        first.store(&site, ["gone=1; Max-Age=1", "stays=1"]);
        // Rewrite the file with the first cookie already in the past, which is
        // what a jar read back tomorrow looks like.
        let text = std::fs::read_to_string(&path).expect("written");
        let text = text.replace(
            &format!("\"expires\": {}", now_secs() + 1),
            "\"expires\": 1000",
        );
        std::fs::write(&path, text).expect("rewritten");

        let second = Jar::new();
        second.persist_to(&path).expect("second");
        let (header, _) = second.header_for(&site).expect("something survives");
        assert!(!header.contains("gone"), "expired cookie restored: {header}");
        assert!(header.contains("stays"), "{header}");
    }

    #[test]
    fn a_logout_reaches_the_file() {
        // Otherwise `--restore` would hand the next session a credential this
        // one deliberately threw away, which is worse than not persisting.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cookies.json");
        let site = url("https://app.example/");

        let jar = Jar::new();
        jar.persist_to(&path).expect("jar");
        jar.store(&site, ["sid=s3cr3t"]);
        jar.clear();

        let next = Jar::new();
        assert_eq!(next.persist_to(&path).expect("next"), 0);
    }

    #[test]
    fn nothing_is_written_unless_a_file_was_named() {
        // The default, and the reason this is safe to have: `open` and `read`
        // pass no path, so a one-shot fetch is still a complete logout.
        let dir = tempfile::tempdir().expect("tempdir");
        let jar = Jar::new();
        jar.store(&url("https://app.example/"), ["sid=s3cr3t"]);
        assert_eq!(
            std::fs::read_dir(dir.path()).expect("readable").count(),
            0,
            "a jar with no file must leave nothing behind"
        );
    }

    #[test]
    fn a_file_that_is_not_a_jar_is_a_failure_rather_than_an_empty_start() {
        // Starting silently logged-out is exactly the defect the feature
        // removes, so a corrupt file must not look like a fresh session.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cookies.json");
        std::fs::write(&path, "not json at all").expect("write");
        assert!(Jar::new().persist_to(&path).is_err());

        std::fs::write(&path, r#"{"version": 99, "cookies": []}"#).expect("write");
        let refused = Jar::new().persist_to(&path).unwrap_err();
        assert!(refused.contains("version"), "{refused}");
    }

    #[test]
    fn a_missing_file_is_a_first_run_and_creates_one() {
        // `--restore` asks "did this session leave a jar", so a session that
        // stored nothing must leave an empty jar rather than no jar.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cookies.json");
        assert_eq!(Jar::new().persist_to(&path).expect("first run"), 0);
        assert!(path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn the_jar_is_readable_only_by_its_owner() {
        // It holds session credentials, and the `agent` profile shares /tmp
        // with the host, so the umask is not good enough.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cookies.json");
        let jar = Jar::new();
        jar.persist_to(&path).expect("jar");
        jar.store(&url("https://app.example/"), ["sid=s3cr3t"]);
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(mode & 0o077, 0, "group/other can read the jar: {mode:o}");
    }

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_secs()
    }
}
