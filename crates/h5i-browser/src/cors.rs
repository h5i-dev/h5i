//! The Same-Origin Policy, and the cross-origin exception to it.

use url::Url;

/// A web origin: scheme, host, port. The unit the whole policy is written in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    scheme: String,
    host: String,
    port: u16,
}

impl Origin {
    /// The origin of a URL, or `None` for one that has none.
    ///
    /// `file:` and `data:` have no origin worth comparing. Every `file:` URL
    /// would otherwise be same-origin with every other, which is the historic
    /// hole that made local pages dangerous. `None` here means "opaque", and
    /// an opaque origin is same-origin with nothing, not with everything.
    pub fn of(url: &Url) -> Option<Origin> {
        let scheme = url.scheme();
        if !matches!(scheme, "http" | "https" | "ws" | "wss") {
            return None;
        }
        let host = url.host_str()?.to_ascii_lowercase();
        let port = url.port_or_known_default()?;
        Some(Origin {
            // A socket address is the same origin as its HTTP twin, the same
            // mapping `policy::normalize_origin` makes for the allowlist.
            scheme: match scheme {
                "ws" => "http".to_string(),
                "wss" => "https".to_string(),
                other => other.to_string(),
            },
            host,
            port,
        })
    }

    /// The serialisation that goes in an `Origin:` header.
    pub fn header(&self) -> String {
        let default = match self.scheme.as_str() {
            "https" => 443,
            _ => 80,
        };
        if self.port == default {
            format!("{}://{}", self.scheme, self.host)
        } else {
            format!("{}://{}:{}", self.scheme, self.host, self.port)
        }
    }
}

/// How a request treats the origin boundary. The `mode` of `fetch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Mode {
    /// Refuse to cross it at all.
    SameOrigin,
    /// Cross it by asking the server's permission. The default for `fetch`.
    #[default]
    Cors,
    /// Cross it without asking, and see nothing of the answer.
    NoCors,
}

impl Mode {
    pub fn parse(value: &str) -> Mode {
        match value.trim().to_ascii_lowercase().as_str() {
            "same-origin" => Mode::SameOrigin,
            "no-cors" => Mode::NoCors,
            _ => Mode::Cors,
        }
    }
}

/// Whether cookies ride along. The `credentials` of `fetch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Credentials {
    Omit,
    /// Cookies for a same-origin request only. The default, and the reason a
    /// cross-origin `fetch` does not carry a session by accident.
    #[default]
    SameOrigin,
    Include,
}

impl Credentials {
    pub fn parse(value: &str) -> Credentials {
        match value.trim().to_ascii_lowercase().as_str() {
            "omit" => Credentials::Omit,
            "include" => Credentials::Include,
            _ => Credentials::SameOrigin,
        }
    }
}

/// What the caller may see of a response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exposure {
    /// Same-origin: everything.
    Full,
    /// Cross-origin and allowed: body and status, headers filtered to the
    /// safelist plus whatever the server exposed.
    Filtered { expose: Vec<String>, all: bool },
    /// Cross-origin `no-cors`: status 0, no headers, no body. A page can tell
    /// the request happened and nothing else, which is what makes it safe to
    /// have sent at all.
    Opaque,
}

/// What to do about one request, decided before it moves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Send it. `origin_header` is `None` for a same-origin request, which is
    /// how a server tells the two apart.
    Send {
        origin_header: Option<String>,
        send_cookies: bool,
        /// Set when the request is not simple and the server must be asked
        /// first. Carries the method and headers the preflight declares.
        preflight: Option<Preflight>,
        /// Whether the response must name this origin back before the caller
        /// sees it.
        check_response: bool,
        exposure: Exposure,
    },
    /// Refused before the wire, with the reason a page should be told.
    Blocked(String),
}

/// What a preflight asks permission for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preflight {
    pub origin: String,
    pub method: String,
    pub headers: Vec<String>,
    pub credentials: bool,
}

/// Methods a cross-origin request may use without asking first.
const SIMPLE_METHODS: &[&str] = &["GET", "HEAD", "POST"];

/// Request headers a page may set cross-origin without asking first.
const SAFELISTED_REQUEST_HEADERS: &[&str] = &[
    "accept",
    "accept-language",
    "content-language",
    "content-type",
    "range",
];

/// `Content-Type` values that do not trigger a preflight.
///
/// `application/json` is deliberately *not* here, which surprises people and
/// is the point: a JSON POST is exactly the shape a CSRF attack takes, so the
/// spec makes it ask permission first.
const SAFELISTED_CONTENT_TYPES: &[&str] = &[
    "application/x-www-form-urlencoded",
    "multipart/form-data",
    "text/plain",
];

/// Response headers a cross-origin caller sees without the server exposing
/// them.
const SAFELISTED_RESPONSE_HEADERS: &[&str] = &[
    "cache-control",
    "content-language",
    "content-length",
    "content-type",
    "expires",
    "last-modified",
    "pragma",
];

/// Response headers no caller ever sees, however the exposure was decided.
///
/// The Fetch spec's forbidden response-header names. `Set-Cookie` is the reason
/// the list exists: `document.cookie` withholds `HttpOnly` cookies, and a
/// response header handed to script gives back exactly what that withholding
/// took away. Same-origin is not an exemption — a script on the page is who the
/// flag is being kept from.
const FORBIDDEN_RESPONSE_HEADERS: &[&str] = &["set-cookie", "set-cookie2"];

/// Whether a header may be set cross-origin without a preflight.
fn header_is_safelisted(name: &str, value: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    if !SAFELISTED_REQUEST_HEADERS.contains(&lower.as_str()) {
        return false;
    }
    if lower == "content-type" {
        // The parameters (`; charset=utf-8`) do not affect the decision.
        let essence = value
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        return SAFELISTED_CONTENT_TYPES.contains(&essence.as_str());
    }
    true
}

/// Who is asking, which is the question the whole policy turns on.
///
/// Three cases and not two, because "the agent named this URL" and "a page with
/// no origin of its own asked" are opposites that both look like the *absence*
/// of an origin. Collapsing them into one `Option` gives the second the
/// authority of the first, which is precisely backwards: a `file:` page is
/// same-origin with nothing and should be able to read nothing cross-origin,
/// while an agent typing a URL is exercising its own authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requester<'a> {
    /// The agent named this URL. Unrestricted.
    Agent,
    /// A document, with the origin it was served from.
    Document(&'a Origin),
    /// A document whose origin is opaque: `file:`, `data:`, or a request
    /// tainted by a cross-origin redirect. Same-origin with nothing.
    Opaque,
}

/// How a requester names itself in an `Origin:` header.
///
/// An opaque one sends the literal `null`, which is what the spec says and what
/// a server must allow explicitly (`Access-Control-Allow-Origin: null`), so it
/// cannot be granted by accident to a page that merely has an origin.
fn serialize(from: Option<&Origin>) -> String {
    match from {
        Some(origin) => origin.header(),
        None => "null".to_string(),
    }
}

/// Decide what to do about one request.
pub fn plan(
    requester: Requester<'_>,
    target: &Url,
    method: &str,
    headers: &[(String, String)],
    mode: Mode,
    credentials: Credentials,
) -> Plan {
    let from = match requester {
        Requester::Agent => {
            // Unrestricted, cookies attached, which is what `navigate` and the
            // read verbs have always done.
            return Plan::Send {
                origin_header: None,
                send_cookies: true,
                preflight: None,
                check_response: false,
                exposure: Exposure::Full,
            };
        }
        Requester::Document(origin) => Some(origin),
        Requester::Opaque => None,
    };

    let to = Origin::of(target);
    // An opaque requester is same-origin with nothing, including itself.
    let same_origin = from.is_some() && to.as_ref() == from;

    if same_origin {
        return Plan::Send {
            origin_header: None,
            send_cookies: !matches!(credentials, Credentials::Omit),
            preflight: None,
            check_response: false,
            exposure: Exposure::Full,
        };
    }

    // From here down the request crosses an origin boundary.
    match mode {
        Mode::SameOrigin => Plan::Blocked(format!(
            "{target} is a different origin from {}, and this request asked for \
             `mode: \"same-origin\"`. Use `mode: \"cors\"` if the server allows it.",
            serialize(from)
        )),

        Mode::NoCors => {
            // Sendable, but the caller learns nothing. Credentials are the
            // same-origin default only, so a beacon does not carry a session.
            if !matches!(credentials, Credentials::Include) {
                Plan::Send {
                    origin_header: Some(serialize(from)),
                    send_cookies: false,
                    preflight: None,
                    check_response: false,
                    exposure: Exposure::Opaque,
                }
            } else {
                Plan::Blocked(
                    "`mode: \"no-cors\"` with `credentials: \"include\"` would send a \
                     credential to another origin and never be able to check that the \
                     server agreed, because an opaque response cannot be read."
                        .to_string(),
                )
            }
        }

        Mode::Cors => {
            let simple_method = SIMPLE_METHODS.contains(&method.to_ascii_uppercase().as_str());
            let unsafe_headers: Vec<String> = headers
                .iter()
                .filter(|(name, value)| !header_is_safelisted(name, value))
                .map(|(name, _)| name.trim().to_ascii_lowercase())
                .collect();

            let send_cookies = matches!(credentials, Credentials::Include);
            let preflight = (!simple_method || !unsafe_headers.is_empty()).then(|| {
                let mut headers = unsafe_headers.clone();
                headers.sort_unstable();
                headers.dedup();
                Preflight {
                    origin: serialize(from),
                    method: method.to_ascii_uppercase(),
                    headers,
                    credentials: send_cookies,
                }
            });

            Plan::Send {
                origin_header: Some(serialize(from)),
                send_cookies,
                preflight,
                check_response: true,
                exposure: Exposure::Filtered {
                    expose: Vec::new(),
                    all: false,
                },
            }
        }
    }
}

/// Whether a response permits this origin to read it.
///
/// `Err` carries the sentence a page should be told, which names what the
/// server would have to send. A CORS failure is otherwise the least debuggable
/// thing on the web.
pub fn check_response(
    acao: Option<&str>,
    acac: Option<&str>,
    origin: &str,
    credentialed: bool,
) -> Result<(), String> {
    let Some(acao) = acao.map(str::trim) else {
        return Err(format!(
            "the response has no `Access-Control-Allow-Origin` header, so {origin} may not \
             read it. The server would have to send `Access-Control-Allow-Origin: {origin}`."
        ));
    };

    if credentialed {
        // Two opt-ins, and the wildcard is refused rather than treated as one.
        // `*` with credentials is the misconfiguration this rule exists for:
        // a server that meant "anyone may read this" has not thought about
        // "anyone may read this *as the logged-in user*".
        if acao == "*" {
            return Err(format!(
                "the response allows any origin (`Access-Control-Allow-Origin: *`), which is \
                 not enough for a credentialed request: the server must name {origin} \
                 explicitly and send `Access-Control-Allow-Credentials: true`."
            ));
        }
        if !acac.map(str::trim).is_some_and(|v| v.eq_ignore_ascii_case("true")) {
            return Err(format!(
                "the response does not send `Access-Control-Allow-Credentials: true`, so a \
                 request from {origin} that carries cookies may not read it."
            ));
        }
    }

    if acao == "*" || acao.eq_ignore_ascii_case(origin) {
        Ok(())
    } else {
        Err(format!(
            "the response allows `{acao}`, which is not {origin}."
        ))
    }
}

/// Whether a preflight's answer permits the request it was asked about.
pub fn check_preflight(
    ask: &Preflight,
    acao: Option<&str>,
    acac: Option<&str>,
    allow_methods: Option<&str>,
    allow_headers: Option<&str>,
) -> Result<(), String> {
    check_response(acao, acac, &ask.origin, ask.credentials)?;

    let listed = |header: Option<&str>| -> (bool, Vec<String>) {
        let Some(raw) = header else {
            return (false, Vec::new());
        };
        let mut wildcard = false;
        let values: Vec<String> = raw
            .split(',')
            .map(|piece| piece.trim().to_ascii_lowercase())
            .inspect(|piece| {
                if piece == "*" {
                    wildcard = true;
                }
            })
            .collect();
        (wildcard, values)
    };

    // A wildcard does not apply to a credentialed request, for the same reason
    // it does not on `Access-Control-Allow-Origin`.
    let (methods_any, methods) = listed(allow_methods);
    let method_ok = (methods_any && !ask.credentials)
        || methods.iter().any(|m| m.eq_ignore_ascii_case(&ask.method))
        // A simple method never needs to be listed.
        || SIMPLE_METHODS.contains(&ask.method.as_str());
    if !method_ok {
        return Err(format!(
            "the preflight does not allow `{}`. The server would have to send \
             `Access-Control-Allow-Methods: {}`.",
            ask.method, ask.method
        ));
    }

    let (headers_any, allowed) = listed(allow_headers);
    for wanted in &ask.headers {
        // Fetch carves `Authorization` out of the wildcard by name, and every
        // browser implements it: a server answering `*` has said "any header",
        // not "I agree to receive somebody's bearer token".
        let covered_by_wildcard =
            headers_any && !ask.credentials && wanted != "authorization";
        if covered_by_wildcard || allowed.iter().any(|a| a == wanted) {
            continue;
        }
        if wanted == "authorization" && headers_any {
            return Err(
                "the preflight allows `*`, and `*` does not cover `Authorization`: a \
                 server must name that header in `Access-Control-Allow-Headers` before a \
                 page may send it to another origin."
                    .to_string(),
            );
        }
        return Err(format!(
            "the preflight does not allow the `{wanted}` header. The server would \
             have to list it in `Access-Control-Allow-Headers`."
        ));
    }
    Ok(())
}

/// Read `Access-Control-Expose-Headers` into an exposure.
pub fn exposure_from(raw: Option<&str>, credentialed: bool) -> Exposure {
    let Some(raw) = raw else {
        return Exposure::Filtered {
            expose: Vec::new(),
            all: false,
        };
    };
    let mut all = false;
    let expose: Vec<String> = raw
        .split(',')
        .map(|piece| piece.trim().to_ascii_lowercase())
        .filter(|piece| {
            if piece == "*" {
                // Again not for a credentialed response.
                all = !credentialed;
                false
            } else {
                !piece.is_empty()
            }
        })
        .collect();
    Exposure::Filtered { expose, all }
}

/// Drop the response headers this caller may not see.
pub fn filter_headers(headers: &[(String, String)], exposure: &Exposure) -> Vec<(String, String)> {
    // Applied before the exposure decision, not inside one arm of it: `Full`
    // returned every header verbatim, and a server naming `set-cookie` in
    // `Access-Control-Expose-Headers` got past the cross-origin arm too.
    let visible = headers.iter().filter(|(name, _)| {
        !FORBIDDEN_RESPONSE_HEADERS.contains(&name.to_ascii_lowercase().as_str())
    });
    match exposure {
        Exposure::Full => visible.cloned().collect(),
        Exposure::Opaque => Vec::new(),
        Exposure::Filtered { expose, all } => visible
            .filter(|(name, _)| {
                let lower = name.to_ascii_lowercase();
                SAFELISTED_RESPONSE_HEADERS.contains(&lower.as_str())
                    || *all
                    || expose.contains(&lower)
            })
            .cloned()
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).expect("test url")
    }

    fn origin(text: &str) -> Origin {
        Origin::of(&url(text)).expect("test origin")
    }

    #[test]
    fn an_origin_is_scheme_host_and_port() {
        assert_eq!(origin("https://a.example/x"), origin("https://a.example/y"));
        assert_ne!(origin("https://a.example/"), origin("http://a.example/"));
        assert_ne!(origin("https://a.example/"), origin("https://b.example/"));
        assert_ne!(
            origin("https://a.example/"),
            origin("https://a.example:8443/")
        );
        // The default port is not part of the serialisation.
        assert_eq!(origin("https://a.example:443/").header(), "https://a.example");
        assert_eq!(
            origin("http://a.example:8080/").header(),
            "http://a.example:8080"
        );
    }

    /// A `file:` URL has no origin, and that is what makes local pages safe:
    /// treating every `file:` as one origin would make any downloaded page
    /// same-origin with every other file on the disk.
    #[test]
    fn schemes_without_an_origin_get_none() {
        assert!(Origin::of(&url("file:///tmp/a.html")).is_none());
        assert!(Origin::of(&url("data:text/html,hi")).is_none());
    }

    #[test]
    fn same_origin_is_unrestricted() {
        let from = origin("https://a.example/");
        let plan = plan(
            Requester::Document(&from),
            &url("https://a.example/api"),
            "GET",
            &[],
            Mode::Cors,
            Credentials::default(),
        );
        match plan {
            Plan::Send {
                origin_header,
                send_cookies,
                preflight,
                check_response,
                exposure,
            } => {
                assert!(origin_header.is_none(), "same-origin sends no Origin");
                assert!(send_cookies);
                assert!(preflight.is_none());
                assert!(!check_response);
                assert_eq!(exposure, Exposure::Full);
            }
            other => panic!("{other:?}"),
        }
    }

    /// The hole this module was written for. Two allowlisted origins, a script
    /// on one, a `fetch` of the other: the allowlist says the engine may
    /// connect, and until now nothing said whether the *document* may read it.
    #[test]
    fn a_cross_origin_read_must_be_checked_against_the_response() {
        let from = origin("https://a.example/");
        let plan = plan(
            Requester::Document(&from),
            &url("https://b.example/secret"),
            "GET",
            &[],
            Mode::Cors,
            Credentials::default(),
        );
        match plan {
            Plan::Send {
                origin_header,
                send_cookies,
                check_response,
                ..
            } => {
                assert_eq!(origin_header.as_deref(), Some("https://a.example"));
                assert!(check_response, "the response must name us back");
                assert!(
                    !send_cookies,
                    "the default credentials mode does not send a session cross-origin"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_response_that_does_not_name_us_is_refused() {
        let deny = check_response(None, None, "https://a.example", false);
        assert!(deny.unwrap_err().contains("no `Access-Control-Allow-Origin`"));

        assert!(check_response(Some("*"), None, "https://a.example", false).is_ok());
        assert!(
            check_response(Some("https://a.example"), None, "https://a.example", false).is_ok()
        );
        let wrong = check_response(Some("https://c.example"), None, "https://a.example", false);
        assert!(wrong.unwrap_err().contains("is not https://a.example"));
    }

    /// The misconfiguration the credentialed rule exists to catch: a server
    /// that said "anyone may read this" has not said "anyone may read this as
    /// the logged-in user".
    #[test]
    fn a_wildcard_is_not_enough_for_a_credentialed_read() {
        let refused = check_response(Some("*"), Some("true"), "https://a.example", true);
        assert!(refused.unwrap_err().contains("not enough for a credentialed"));

        let no_acac = check_response(Some("https://a.example"), None, "https://a.example", true);
        assert!(no_acac
            .unwrap_err()
            .contains("Access-Control-Allow-Credentials"));

        assert!(check_response(
            Some("https://a.example"),
            Some("true"),
            "https://a.example",
            true
        )
        .is_ok());
    }

    #[test]
    fn a_simple_request_does_not_preflight_and_a_json_post_does() {
        let from = origin("https://a.example/");
        let simple = plan(
            Requester::Document(&from),
            &url("https://b.example/x"),
            "POST",
            &[(
                "content-type".into(),
                "application/x-www-form-urlencoded".into(),
            )],
            Mode::Cors,
            Credentials::default(),
        );
        assert!(matches!(simple, Plan::Send { preflight: None, .. }));

        // `application/json` is not safelisted, and that is the point: a JSON
        // POST is the shape a CSRF attack takes.
        let json = plan(
            Requester::Document(&from),
            &url("https://b.example/x"),
            "POST",
            &[("content-type".into(), "application/json".into())],
            Mode::Cors,
            Credentials::default(),
        );
        match json {
            Plan::Send {
                preflight: Some(ask),
                ..
            } => {
                assert_eq!(ask.method, "POST");
                assert_eq!(ask.headers, vec!["content-type".to_string()]);
            }
            other => panic!("a JSON POST must preflight: {other:?}"),
        }

        // A non-simple method preflights whatever the headers say.
        let delete = plan(
            Requester::Document(&from),
            &url("https://b.example/x"),
            "DELETE",
            &[],
            Mode::Cors,
            Credentials::default(),
        );
        assert!(matches!(delete, Plan::Send { preflight: Some(_), .. }));
    }

    #[test]
    fn a_preflight_answer_must_allow_the_method_and_the_headers() {
        let ask = Preflight {
            origin: "https://a.example".into(),
            method: "DELETE".into(),
            headers: vec!["x-token".into()],
            credentials: false,
        };
        assert!(check_preflight(
            &ask,
            Some("https://a.example"),
            None,
            Some("DELETE, PATCH"),
            Some("X-Token"),
        )
        .is_ok());

        let no_method = check_preflight(
            &ask,
            Some("https://a.example"),
            None,
            Some("PATCH"),
            Some("X-Token"),
        );
        assert!(no_method.unwrap_err().contains("does not allow `DELETE`"));

        let no_header =
            check_preflight(&ask, Some("https://a.example"), None, Some("DELETE"), None);
        assert!(no_header.unwrap_err().contains("`x-token` header"));
    }

    /// A wildcard in a preflight answer does not apply to a credentialed
    /// request, for the same reason it does not on the origin header.
    #[test]
    fn a_wildcard_preflight_does_not_cover_a_credentialed_request() {
        let ask = Preflight {
            origin: "https://a.example".into(),
            method: "DELETE".into(),
            headers: vec!["x-token".into()],
            credentials: true,
        };
        let refused = check_preflight(
            &ask,
            Some("https://a.example"),
            Some("true"),
            Some("*"),
            Some("*"),
        );
        assert!(refused.is_err(), "a wildcard must not cover credentials");
    }

    /// Fetch carves `Authorization` out of `Allow-Headers: *`, because a server
    /// answering `*` has said "any header", not "send me a bearer token".
    #[test]
    fn a_wildcard_preflight_does_not_cover_authorization() {
        let asking_for = |header: &str| Preflight {
            origin: "https://a.example".into(),
            method: "GET".into(),
            headers: vec![header.into()],
            credentials: false,
        };
        // Any other header the wildcard does cover.
        assert!(
            check_preflight(
                &asking_for("x-token"),
                Some("*"),
                None,
                Some("*"),
                Some("*")
            )
            .is_ok(),
            "`*` covers an ordinary header"
        );
        let refused = check_preflight(
            &asking_for("authorization"),
            Some("*"),
            None,
            Some("*"),
            Some("*"),
        )
        .expect_err("`*` does not cover Authorization");
        assert!(refused.contains("does not cover"), "{refused}");

        // Named explicitly, it goes through: that is the server agreeing.
        assert!(
            check_preflight(
                &asking_for("authorization"),
                Some("*"),
                None,
                Some("*"),
                Some("authorization, x-token")
            )
            .is_ok(),
            "a server that names it has agreed to it"
        );
    }

    #[test]
    fn no_cors_is_sendable_and_unreadable() {
        let from = origin("https://a.example/");
        let plan = plan(
            Requester::Document(&from),
            &url("https://b.example/beacon"),
            "POST",
            &[],
            Mode::NoCors,
            Credentials::default(),
        );
        match plan {
            Plan::Send {
                exposure,
                send_cookies,
                ..
            } => {
                assert_eq!(exposure, Exposure::Opaque);
                assert!(!send_cookies, "a beacon must not carry a session");
            }
            other => panic!("{other:?}"),
        }
    }

    /// An opaque response cannot be checked, so a credential sent with one
    /// could never be shown to have been permitted.
    #[test]
    fn no_cors_with_credentials_is_refused_outright() {
        let from = origin("https://a.example/");
        let refused = plan(
            Requester::Document(&from),
            &url("https://b.example/x"),
            "GET",
            &[],
            Mode::NoCors,
            Credentials::Include,
        );
        assert!(matches!(refused, Plan::Blocked(_)));
    }

    #[test]
    fn same_origin_mode_refuses_to_cross_at_all() {
        let from = origin("https://a.example/");
        let refused = plan(
            Requester::Document(&from),
            &url("https://b.example/x"),
            "GET",
            &[],
            Mode::SameOrigin,
            Credentials::default(),
        );
        match refused {
            Plan::Blocked(why) => assert!(why.contains("same-origin")),
            other => panic!("{other:?}"),
        }
    }

    /// A request the *agent* made has no document behind it, so there is no
    /// boundary to cross. This is what keeps `navigate` and the read verbs
    /// working exactly as they did.
    #[test]
    fn a_request_with_no_document_is_unrestricted() {
        let plan = plan(
            Requester::Agent,
            &url("https://b.example/x"),
            "GET",
            &[],
            Mode::Cors,
            Credentials::default(),
        );
        assert!(matches!(
            plan,
            Plan::Send {
                exposure: Exposure::Full,
                send_cookies: true,
                check_response: false,
                ..
            }
        ));
    }

    #[test]
    fn response_headers_are_filtered_to_the_safelist_plus_what_was_exposed() {
        let headers = vec![
            ("content-type".to_string(), "application/json".to_string()),
            ("x-request-id".to_string(), "abc".to_string()),
            ("set-cookie".to_string(), "sid=secret".to_string()),
        ];

        let default = exposure_from(None, false);
        let seen = filter_headers(&headers, &default);
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert_eq!(seen[0].0, "content-type");

        let exposed = exposure_from(Some("X-Request-Id"), false);
        let seen = filter_headers(&headers, &exposed);
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert!(
            !seen.iter().any(|(name, _)| name == "set-cookie"),
            "a credential must never be exposed by name: {seen:?}"
        );

        // Same-origin sees everything but the credential, opaque sees nothing.
        // `Set-Cookie` is forbidden to script whatever the exposure: the whole
        // point of `HttpOnly` is that the page cannot read it, and handing it
        // back through `response.headers` returns what was withheld.
        let full = filter_headers(&headers, &Exposure::Full);
        assert_eq!(full.len(), 2, "{full:?}");
        assert!(!full.iter().any(|(name, _)| name == "set-cookie"), "{full:?}");
        assert!(filter_headers(&headers, &Exposure::Opaque).is_empty());

        // Nor may a server expose it by name, or sweep it up with `*`.
        for exposure in [exposure_from(Some("Set-Cookie"), false), exposure_from(Some("*"), false)] {
            let seen = filter_headers(&headers, &exposure);
            assert!(!seen.iter().any(|(name, _)| name == "set-cookie"), "{seen:?}");
        }
    }

    #[test]
    fn a_wildcard_exposure_does_not_apply_to_a_credentialed_response() {
        let credentialed = exposure_from(Some("*"), true);
        let headers = vec![("x-secret".to_string(), "v".to_string())];
        assert!(
            filter_headers(&headers, &credentialed).is_empty(),
            "`*` must not expose headers on a credentialed response"
        );

        let anonymous = exposure_from(Some("*"), false);
        assert_eq!(filter_headers(&headers, &anonymous).len(), 1);
    }
}
