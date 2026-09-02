//! What the page is allowed to reach.

use std::collections::BTreeSet;

use url::Url;

/// The answer to "may this request happen", carrying the reason when it is no.
///
/// The reason is not decoration: it is what the receipt records and what the
/// caller shows a human, so it has to name the origin that was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny(String),
}

impl Verdict {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Verdict::Allow)
    }

    /// The reason text for a denial, or `None` when allowed.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Verdict::Allow => None,
            Verdict::Deny(why) => Some(why),
        }
    }
}

/// An origin allowlist plus the limits that keep one page from becoming an
/// unbounded amount of work.
#[derive(Debug, Clone)]
pub struct Policy {
    origins: BTreeSet<String>,
    /// Subdomain wildcards (`*.example.com`), stored as `scheme://suffix[:port]`
    /// so they carry the same scheme and port constraints as an exact origin.
    ///
    /// h5i's own `net.egress` accepts `*.host`, and a box's egress list is handed
    /// to this engine verbatim, so treating the entry as a literal hostname made
    /// every wildcard grant match nothing. Storing only the bare host was the
    /// other half of that mistake: it silently dropped the scheme, so a wildcard
    /// permitted plaintext http on any port while the exact spelling of the same
    /// grant refused it.
    wildcards: BTreeSet<String>,
    allow_loopback: bool,
    /// Every remote origin is granted.
    any_remote: bool,
    max_redirects: usize,
    max_response_bytes: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            origins: BTreeSet::new(),
            wildcards: BTreeSet::new(),
            // Loopback is the agent's own dev server. It never appears in an
            // egress allowlist and is the whole point of a dev loop, so it is
            // opt-out rather than opt-in. Matching the sandbox's own handling.
            allow_loopback: true,
            any_remote: false,
            max_redirects: 5,
            max_response_bytes: 8 * 1024 * 1024,
        }
    }
}

impl Policy {
    /// A policy that permits nothing but loopback.
    pub fn new() -> Self {
        Self::default()
    }

    /// Grant an origin. Accepts a bare host (`example.com`), a scheme-qualified
    /// origin (`https://example.com`), or a full URL, because every one of
    /// those is a thing a person types and none of them should mean "denied,
    /// silently".
    pub fn allow(mut self, origin: &str) -> Self {
        let trimmed = origin.trim();
        // `*.host` and `.host` are h5i's spellings for "this host and its
        // subdomains". Record the suffix; `check` matches it against the host
        // rather than against the whole origin string.
        // The wildcard marker sits on the *host*, which may follow a scheme
        // (`http://*.host:8080`) or stand alone (`*.host`), so strip the scheme
        // first and put it back afterwards.
        let (scheme, authority) = match trimmed.split_once("://") {
            Some((scheme, rest)) => (Some(scheme), rest),
            None => (None, trimmed),
        };
        if let Some(rest) = authority
            .strip_prefix("*.")
            .or_else(|| authority.strip_prefix('.'))
        {
            let rebuilt = match scheme {
                Some(scheme) => format!("{scheme}://{rest}"),
                None => rest.to_string(),
            };
            // Normalised exactly like an exact entry, so a bare `*.host` means
            // https like a bare `host` does, and `*.host:8443` keeps its port.
            if let Some(origin) = normalize_origin(&rebuilt) {
                self.wildcards.insert(origin);
            }
            return self;
        }
        if let Some(normalized) = normalize_origin(trimmed) {
            self.origins.insert(normalized);
        }
        self
    }

    pub fn allow_all_of<I, S>(mut self, origins: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for origin in origins {
            self = self.allow(origin.as_ref());
        }
        self
    }

    pub fn set_allow_loopback(mut self, allow: bool) -> Self {
        self.allow_loopback = allow;
        self
    }

    /// Grant every remote origin. See the `any_remote` field for what this does
    /// *not* grant, which is the part that matters.
    pub fn set_any_remote(mut self, allow: bool) -> Self {
        self.any_remote = allow;
        self
    }

    /// Whether this policy is in the instrument mode, so a caller can say so.
    ///
    /// Read by the doctor line and by `open`'s placement reporting. A mode this
    /// wide that does not announce itself is the kind of quiet difference
    /// between a measurement and the thing measured that §B19 is about.
    pub fn allows_any_remote(&self) -> bool {
        self.any_remote
    }

    pub fn set_max_redirects(mut self, max: usize) -> Self {
        self.max_redirects = max;
        self
    }

    pub fn set_max_response_bytes(mut self, max: u64) -> Self {
        self.max_response_bytes = max;
        self
    }

    pub fn max_redirects(&self) -> usize {
        self.max_redirects
    }

    pub fn max_response_bytes(&self) -> u64 {
        self.max_response_bytes
    }

    /// The granted origins, for the doctor output and for a receipt header
    /// that says what this run was permitted to reach.
    pub fn origins(&self) -> impl Iterator<Item = &str> {
        self.origins.iter().map(String::as_str)
    }

    /// The one decision point: every request and every redirect hop comes through here.
    pub fn check_from(&self, url: &Url, document: Option<&Url>) -> Verdict {
        if self.allow_loopback && url.host_str().is_some_and(is_loopback) {
            let document_is_local = match document {
                None => true,
                Some(origin) => {
                    origin.host_str().is_some_and(is_loopback) || origin.scheme() == "file"
                }
            };
            if !document_is_local {
                return Verdict::Deny(format!(
                    "a page served from {} may not reach loopback ({}): only a page the \
                     dev server itself served may talk to it",
                    document.map(|d| d.origin().ascii_serialization()).unwrap_or_default(),
                    url.host_str().unwrap_or_default(),
                ));
            }
        }
        self.check(url)
    }

    /// Check the *address* a host actually resolved to.
    pub fn check_address(&self, url: &Url, addr: std::net::IpAddr) -> Verdict {
        if !is_internal_address(addr) {
            return Verdict::Allow;
        }
        let host = url.host_str().unwrap_or_default();
        // A name that already declared itself local, and a policy that permits
        // local names at all.
        if self.allow_loopback && is_loopback(host) {
            return Verdict::Allow;
        }
        // An address written directly into the URL was itself what the allowlist
        // decided about, so it is not a rebinding: the caller asked for this
        // address by name.
        //
        // `any_remote` deliberately does not answer here. The instrument mode grants
        // the open web, and an RFC 1918 literal is not the open web; letting the
        // blanket grant reach one would turn a measurement flag into a
        // private-network flag. An explicit `--allow http://10.0.0.1:8080` still
        // works.
        if host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(host)
            .parse::<std::net::IpAddr>()
            .is_ok()
        {
            return self.listed(url);
        }
        Verdict::Deny(format!(
            "`{host}` resolved to {addr}, which is an internal address. A public name \
             pointing into private space is how an allowlist is walked around, so the \
             request is refused rather than made to somewhere the receipt would not \
             describe."
        ))
    }

    pub fn check(&self, url: &Url) -> Verdict {
        match url.scheme() {
            // `data:` is inline in the document that already passed policy;
            // it reaches no network, so refusing it would only break pages
            // without protecting anything.
            "data" => return Verdict::Allow,
            "http" | "https" => {}
            // A WebSocket address is checked by exactly the same rules as its
            // http twin: same host, same allowlist, same loopback exemption.
            // The scheme differs; what is being decided does not, and giving
            // `ws://` its own path would be an allowlist a reader has to know
            // about separately.
            "ws" | "wss" => {}
            other => {
                return Verdict::Deny(format!(
                    "scheme `{other}` is not fetchable by this engine (only http, https, ws, \
                     wss, data)"
                ));
            }
        }

        let Some(host) = url.host_str() else {
            return Verdict::Deny("request has no host".to_string());
        };

        if self.allow_loopback && is_loopback(host) {
            return Verdict::Allow;
        }

        // The instrument mode. Deliberately *after* the scheme check, so it
        // grants origins and not schemes: `file:` is still refused here, and a
        // name that resolves into private space is still refused by
        // `check_address`, which this does not touch.
        if self.any_remote {
            return Verdict::Allow;
        }

        self.listed(url)
    }

    /// The allowlist proper: exact origins and wildcards, and nothing else.
    ///
    /// Split out of [`Self::check`] so [`Self::check_address`] can ask the
    /// narrower question. `check` is "may this run reach it", which the
    /// instrument mode answers yes to; this is "did somebody name it", which
    /// only a grant answers.
    fn listed(&self, url: &Url) -> Verdict {
        let Some(host) = url.host_str() else {
            return Verdict::Deny("request has no host".to_string());
        };
        let Some(origin) = normalize_origin(url.as_str()) else {
            return Verdict::Deny(format!("could not derive an origin from `{url}`"));
        };

        if self.origins.contains(&origin) {
            return Verdict::Allow;
        }

        // A wildcard grants the host and anything under it, but only on the
        // scheme and port it was granted for, so it is compared as an origin
        // with the host part relaxed, not as a bare hostname.
        let host_lower = host.to_ascii_lowercase();
        if self.wildcards.iter().any(|w| {
            let Some((scheme, rest)) = w.split_once("://") else {
                return false;
            };
            // Split the granted authority back into host and optional port.
            let (w_host, w_port) = match rest.rsplit_once(':') {
                Some((h, p)) if p.bytes().all(|b| b.is_ascii_digit()) => (h, Some(p)),
                _ => (rest, None),
            };
            let host_matches =
                host_lower == w_host || host_lower.ends_with(&format!(".{w_host}"));
            let scheme_matches = url.scheme() == scheme;
            let port_matches = match w_port {
                Some(p) => url.port().map(|actual| actual.to_string()) == Some(p.to_string()),
                None => url.port().is_none(),
            };
            host_matches && scheme_matches && port_matches
        }) {
            return Verdict::Allow;
        }

        // Name the origin, not just "denied": this string is what a human
        // reads when a page came back empty, and it is the retry hint.
        Verdict::Deny(format!("origin `{origin}` is not in the allowlist"))
    }
}

/// Whether an address belongs to a space only something on this machine or
/// this network should be able to reach.
///
/// Loopback, the unspecified address, link-local (which carries the cloud
/// metadata endpoint at `169.254.169.254`), and the RFC 1918 private ranges.
/// IPv6 unique-local and its IPv4-mapped forms are included, or the same
/// address reached by a different spelling would answer differently.
pub fn is_internal_address(addr: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match addr {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                // 100.64.0.0/10, carrier-grade NAT, which `std` does not name.
                || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))
        }
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_internal_address(IpAddr::V4(mapped));
            }
            v6.is_loopback()
                || v6.is_unspecified()
                // fc00::/7 unique-local and fe80::/10 link-local, neither of
                // which `std` exposes on stable.
                || (v6.octets()[0] & 0xfe) == 0xfc
                || (v6.octets()[0] == 0xfe && (v6.octets()[1] & 0xc0) == 0x80)
        }
    }
}

pub(crate) fn is_loopback(host: &str) -> bool {
    // `localhost` and its subdomains resolve to loopback by convention.
    if host == "localhost" || host.ends_with(".localhost") {
        return true;
    }

    // Everything else must be an *address*, parsed, not string-matched. A
    // literal `127.` prefix looks like the 127.0.0.0/8 loopback block but also
    // matches the perfectly public DNS name `127.evil.com`, which would hand an
    // allowlist bypass to any page that references it. And IPv6 loopback
    // arrives from `host_str` as `[::1]` *with* the brackets, so a bare `::1`
    // comparison never fires. Parsing settles both.
    let literal = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    match literal.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => v4.octets()[0] == 127, // 127.0.0.0/8
        Ok(std::net::IpAddr::V6(v6)) => v6.is_loopback(),       // ::1
        Err(_) => false,
    }
}

/// Reduce anything origin-shaped to `scheme://host[:port]`.
///
/// A bare host is read as https, because that is what someone granting
/// `example.com` means, and silently reading it as http would grant less
/// than they asked for while looking like it worked.
fn normalize_origin(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    // `example.com:8080` *parses*, as a URL whose scheme is `example.com` and
    // whose path is `8080`: a scheme may contain dots. Trusting that first
    // parse dropped every `host:port` grant on the floor, silently, which is
    // the one outcome `allow`'s contract rules out. So a parse that did not
    // land on a web scheme is re-read as a bare authority.
    let parsed = match Url::parse(trimmed) {
        Ok(url) if matches!(url.scheme(), "http" | "https" | "ws" | "wss") => url,
        _ => Url::parse(&format!("https://{trimmed}")).ok()?,
    };

    // A socket address is judged as its HTTP twin, which is what `check`'s
    // documentation already promised: "same host, same allowlist, same loopback
    // exemption."
    //
    // Nothing implemented that promise. A remote `ws://` on an allowed origin
    // came back "could not derive an origin from `ws://…`": a denial with the
    // wrong reason, which sends whoever reads it looking for a malformed URL
    // instead of at their allowlist. It stayed hidden because the proxy rule in
    // `wsclient` refuses remote sockets first inside a box.
    let scheme = match parsed.scheme() {
        "ws" => "http",
        "wss" => "https",
        other => other,
    };
    if !matches!(scheme, "http" | "https") {
        return None;
    }
    let host = parsed.host_str()?;

    match parsed.port() {
        Some(port) => Some(format!("{scheme}://{host}:{port}")),
        None => Some(format!("{scheme}://{host}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).expect("test url parses")
    }

    #[test]
    fn a_host_and_port_grant_is_a_grant() {
        // It parses as scheme `example.com`, path `8080`, so reading the first
        // parse threw the whole entry away and the run reached nothing, with
        // no line saying why.
        let policy = Policy::new().allow("dev.example.com:8080");
        assert!(policy.check(&url("https://dev.example.com:8080/x")).is_allowed());
        assert!(!policy.check(&url("https://dev.example.com/x")).is_allowed());
        // Same for the wildcard spelling, which normalises through here too.
        let wild = Policy::new().allow("*.example.com:8080");
        assert!(wild.check(&url("https://a.example.com:8080/")).is_allowed());
        assert!(!wild.check(&url("https://a.example.com/")).is_allowed());
        // And a scheme this engine cannot fetch is still not a grant.
        assert!(!Policy::new().allow("file://etc").check(&url("https://etc/")).is_allowed());
    }

    #[test]
    fn empty_policy_denies_everything_remote() {
        let policy = Policy::new();
        assert!(!policy.check(&url("https://example.com/a")).is_allowed());
        assert!(!policy.check(&url("http://evil.test/x")).is_allowed());
    }

    #[test]
    fn loopback_is_allowed_by_default_because_it_is_the_dev_server() {
        let policy = Policy::new();
        assert!(policy.check(&url("http://localhost:3000/")).is_allowed());
        assert!(policy.check(&url("http://127.0.0.1:8080/x")).is_allowed());
        // ...and can be taken away for a run that should reach nothing local.
        let strict = Policy::new().set_allow_loopback(false);
        assert!(!strict.check(&url("http://localhost:3000/")).is_allowed());
    }

    #[test]
    fn a_subdomain_wildcard_grants_the_subdomains_h5i_already_permits() {
        // h5i's net.egress accepts `*.host`, and the box's egress list reaches
        // this engine verbatim; treating it as a literal hostname refused every
        // request the sandbox itself would have allowed.
        let policy = Policy::new().allow("*.example.com");
        assert!(policy.check(&url("https://docs.example.com/guide")).is_allowed());
        assert!(policy.check(&url("https://a.b.example.com/")).is_allowed());
        // The apex too: `*.host` in h5i means the host and everything under it.
        assert!(policy.check(&url("https://example.com/")).is_allowed());
        // `.host` is the other spelling h5i accepts.
        assert!(Policy::new().allow(".example.com").check(&url("https://x.example.com/")).is_allowed());
    }

    #[test]
    fn a_wildcard_carries_the_same_scheme_and_port_constraints_as_an_exact_grant() {
        // The bug: the wildcard branch skipped normalize_origin, so `*.host`
        // silently permitted plaintext http on any port while the exact
        // spelling of the same grant refused it.
        let policy = Policy::new().allow("*.example.com");
        assert!(policy.check(&url("https://docs.example.com/")).is_allowed());
        assert!(
            !policy.check(&url("http://docs.example.com/")).is_allowed(),
            "a bare wildcard means https, exactly as a bare host does"
        );
        assert!(
            !policy.check(&url("https://docs.example.com:8443/")).is_allowed(),
            "the port is part of the origin for wildcards too"
        );

        // ...and an explicit scheme/port is honoured.
        let plain = Policy::new().allow("http://*.internal.corp:8080");
        assert!(plain.check(&url("http://api.internal.corp:8080/t")).is_allowed());
        assert!(!plain.check(&url("https://api.internal.corp/t")).is_allowed());
    }

    #[test]
    fn a_redirect_cannot_downgrade_a_wildcard_grant_to_plaintext() {
        // Every hop is re-checked by this same function, so if the wildcard
        // ignored the scheme an https->http hop on the same host would be
        // waved through and the response would travel unencrypted.
        let policy = Policy::new().allow("*.example.com");
        assert!(policy.check(&url("https://a.example.com/x")).is_allowed());
        assert!(!policy.check(&url("http://a.example.com/x")).is_allowed());
    }

    #[test]
    fn a_wildcard_matches_on_a_label_boundary_only() {
        // The classic near-miss: `*.example.com` must not grant
        // `notexample.com` or an attacker-controlled suffix lookalike.
        let policy = Policy::new().allow("*.example.com");
        assert!(!policy.check(&url("https://notexample.com/")).is_allowed());
        assert!(!policy.check(&url("https://example.com.evil.test/")).is_allowed());
        assert!(!policy.check(&url("https://evil-example.com/")).is_allowed());
    }

    /// The gap a name-level allowlist cannot see: the check happens against a
    /// name, the connection happens against an address, and DNS decides the
    /// second one after the first has been approved.
    #[test]
    fn an_allowed_name_that_resolves_inward_is_refused() {
        let policy = Policy::new().allow("https://docs.example.com");
        let url = Url::parse("https://docs.example.com/page").unwrap();

        // The name itself is fine. That is the point.
        assert!(policy.check(&url).is_allowed());

        for addr in [
            "127.0.0.1",
            "10.0.0.1",
            "192.168.1.5",
            "172.16.0.1",
            // The cloud metadata endpoint, which is the one every SSRF
            // write-up reaches for.
            "169.254.169.254",
            "::1",
            // The same loopback address wearing its IPv4-mapped spelling.
            "::ffff:127.0.0.1",
        ] {
            let verdict = policy.check_address(&url, addr.parse().unwrap());
            assert!(
                !verdict.is_allowed(),
                "{addr} should not be reachable through a public name"
            );
        }

        // A public address through the same name is the ordinary case.
        assert!(
            policy
                .check_address(&url, "93.184.216.34".parse().unwrap())
                .is_allowed()
        );
    }

    /// Loopback by *name* is the dev server, which is allowed by design. The
    /// address check must not take that back.
    #[test]
    fn a_loopback_name_may_still_reach_a_loopback_address() {
        let policy = Policy::new();
        for (name, addr) in [
            ("http://localhost:3000/", "127.0.0.1"),
            ("http://127.0.0.1:3000/", "127.0.0.1"),
            ("http://[::1]:3000/", "::1"),
            ("http://app.localhost:3000/", "127.0.0.1"),
        ] {
            let url = Url::parse(name).unwrap();
            assert!(
                policy.check_address(&url, addr.parse().unwrap()).is_allowed(),
                "{name} is the dev server and must stay reachable"
            );
        }
    }

    /// An address written into the URL is what the allowlist already answered
    /// about, so the address check defers to that answer rather than inventing
    /// a second one.
    #[test]
    fn a_literal_address_is_judged_by_the_allowlist_not_by_its_range() {
        let allowed = Policy::new().allow("http://10.1.2.3:8080");
        let url = Url::parse("http://10.1.2.3:8080/admin").unwrap();
        assert!(
            allowed
                .check_address(&url, "10.1.2.3".parse().unwrap())
                .is_allowed(),
            "an operator who granted this address by name meant it"
        );

        let empty = Policy::new();
        assert!(
            !empty
                .check_address(&url, "10.1.2.3".parse().unwrap())
                .is_allowed(),
            "and one that was never granted is still refused"
        );
    }

    #[test]
    fn internal_ranges_are_recognised_in_every_spelling() {
        for addr in [
            "127.0.0.1",
            "10.0.0.1",
            "192.168.0.1",
            "169.254.169.254",
            "100.64.0.1",
        ] {
            assert!(is_internal_address(addr.parse().unwrap()), "{addr}");
        }
        for addr in ["fd00::1", "fe80::1", "::1", "::"] {
            assert!(is_internal_address(addr.parse().unwrap()), "{addr}");
        }
        for addr in ["8.8.8.8", "93.184.216.34", "2606:4700:4700::1111"] {
            assert!(!is_internal_address(addr.parse().unwrap()), "{addr}");
        }
    }

    /// A socket address is judged as its HTTP twin. The promise was in
    /// `check`'s documentation and nothing implemented it, so an allowed
    /// remote socket was denied for "could not derive an origin". A refusal
    /// whose reason pointed at the URL when the answer was the allowlist.
    #[test]
    fn a_socket_address_is_judged_as_its_http_twin() {
        let policy = Policy::new().allow("https://example.com");
        assert!(
            policy.check(&url("wss://example.com/socket")).is_allowed(),
            "granting https should grant wss to the same origin"
        );
        assert!(
            !policy.check(&url("ws://example.com/socket")).is_allowed(),
            "but not the plaintext twin, which is a different origin"
        );

        let plain = Policy::new().allow("http://example.com");
        assert!(plain.check(&url("ws://example.com/socket")).is_allowed());

        // And a refusal names the allowlist rather than the URL's shape.
        let empty = Policy::new();
        let verdict = empty.check(&url("wss://elsewhere.example/socket"));
        assert!(!verdict.is_allowed());
        assert!(
            verdict.reason().unwrap().contains("allowlist"),
            "the reason should point at the allowlist: {:?}",
            verdict.reason()
        );
    }

    #[test]
    fn a_dns_name_that_looks_like_loopback_is_not_loopback() {
        // `127.evil.com` is a valid public hostname. Treating it as loopback
        // because it starts with "127." hands an allowlist bypass to any page
        // that references it.
        let policy = Policy::new();
        assert!(!policy.check(&url("http://127.evil.com/exfil")).is_allowed());
        assert!(!policy.check(&url("http://127.0.0.1.evil.com/")).is_allowed());
        // ...while the real 127/8 block stays loopback.
        assert!(policy.check(&url("http://127.0.0.1:8080/")).is_allowed());
        assert!(policy.check(&url("http://127.9.9.9/")).is_allowed());
    }

    #[test]
    fn ipv6_loopback_is_recognised_despite_the_brackets() {
        // host_str returns `[::1]` with brackets; a bare `::1` comparison
        // never matches, so an IPv6-only dev server would be denied.
        let policy = Policy::new();
        assert!(policy.check(&url("http://[::1]:3000/app")).is_allowed());
        assert!(policy.check(&url("http://[::1]/")).is_allowed());
    }

    #[test]
    fn granting_an_origin_does_not_grant_its_neighbours() {
        let policy = Policy::new().allow("example.com");
        assert!(policy.check(&url("https://example.com/docs")).is_allowed());
        // A subdomain is a different origin. This is the rule that stops
        // "allow the docs site" from also allowing its CDN and its analytics.
        assert!(!policy.check(&url("https://cdn.example.com/x")).is_allowed());
        assert!(!policy.check(&url("https://example.com.evil.test/")).is_allowed());
    }

    #[test]
    fn scheme_and_port_are_part_of_the_origin() {
        let policy = Policy::new().allow("https://example.com");
        assert!(policy.check(&url("https://example.com/a")).is_allowed());
        // Downgrade to http is a different origin, and silently allowing it
        // would let a network attacker strip TLS and stay "in policy".
        assert!(!policy.check(&url("http://example.com/a")).is_allowed());
        assert!(!policy.check(&url("https://example.com:8443/a")).is_allowed());
    }

    #[test]
    fn bare_host_is_read_as_https_not_as_both() {
        let policy = Policy::new().allow("example.com");
        assert!(policy.check(&url("https://example.com/")).is_allowed());
        assert!(!policy.check(&url("http://example.com/")).is_allowed());
    }

    #[test]
    fn non_network_schemes_are_refused_by_name() {
        let policy = Policy::new().allow("example.com");
        // `file:` is the one that matters: a page that can read the box's
        // filesystem through the renderer has walked around the whole point.
        let verdict = policy.check(&url("file:///etc/passwd"));
        assert!(!verdict.is_allowed());
        assert!(verdict.reason().unwrap().contains("file"));
    }

    #[test]
    fn data_urls_are_allowed_because_they_reach_no_network() {
        let policy = Policy::new();
        assert!(policy
            .check(&url("data:text/plain;base64,aGVsbG8="))
            .is_allowed());
    }

    #[test]
    fn denial_reason_names_the_origin_so_a_human_can_act_on_it() {
        let policy = Policy::new();
        let verdict = policy.check(&url("https://tracker.test/pixel.gif"));
        let reason = verdict.reason().expect("denied requests carry a reason");
        assert!(reason.contains("https://tracker.test"), "reason: {reason}");
    }

    #[test]
    fn origins_round_trip_for_the_doctor_output() {
        let policy = Policy::new()
            .allow("example.com")
            .allow("http://localhost:9999")
            .allow("https://docs.rs");
        let listed: Vec<_> = policy.origins().collect();
        assert_eq!(
            listed,
            vec!["http://localhost:9999", "https://docs.rs", "https://example.com"]
        );
    }

    // --- the instrument mode (roadmap-history.md §B19.5, item 5) ---------------------

    #[test]
    fn any_remote_grants_the_open_web_and_nothing_else_changes() {
        let open = Policy::new().set_any_remote(true);
        assert!(open.check(&url("https://anything.test/x")).is_allowed());
        assert!(open.check(&url("http://plain.test/x")).is_allowed());
        assert!(open.allows_any_remote(), "the mode has to be reportable");

        // Still not a scheme grant. `file:` was never fetchable and this does
        // not make it one.
        assert!(!open.check(&url("file:///etc/passwd")).is_allowed());
    }

    #[test]
    fn any_remote_does_not_disable_the_rebinding_guard() {
        // The property that makes this a measurement flag rather than a
        // private-network flag: the *name* check is what widens, and where the
        // name actually went is still decided separately.
        let open = Policy::new().set_any_remote(true);
        let verdict = open.check_address(
            &url("https://public.test/x"),
            "10.0.0.7".parse().expect("addr"),
        );
        assert!(!verdict.is_allowed(), "a public name into RFC1918 must still be refused");
        assert!(verdict.reason().unwrap().contains("internal address"));
    }

    #[test]
    fn any_remote_does_not_reach_a_private_address_written_literally() {
        // `check_address` sends an IP-literal host back to the allowlist, and
        // the allowlist under this flag would otherwise say yes to everything.
        // It asks the narrower question instead, so the blanket grant cannot
        // become a route into private space by spelling.
        let open = Policy::new().set_any_remote(true);
        let literal = url("http://10.0.0.7:8080/x");
        let addr = "10.0.0.7".parse().expect("addr");
        assert!(!open.check_address(&literal, addr).is_allowed());

        // Naming it explicitly still works, because that is somebody deciding.
        let named = Policy::new()
            .set_any_remote(true)
            .allow("http://10.0.0.7:8080");
        assert!(named.check_address(&literal, addr).is_allowed());
    }

    #[test]
    fn any_remote_does_not_let_a_web_page_reach_the_dev_server() {
        // The document rule lives in `check_from` and is untouched: a page the
        // web served may not talk to loopback however wide the allowlist is.
        let open = Policy::new().set_any_remote(true);
        let verdict = open.check_from(
            &url("http://127.0.0.1:3000/secret"),
            Some(&url("https://evil.test/page")),
        );
        assert!(!verdict.is_allowed(), "loopback is still document-scoped: {verdict:?}");
    }

    #[test]
    fn without_the_flag_nothing_remote_is_reachable() {
        // The default this exists to leave alone.
        let closed = Policy::new();
        assert!(!closed.check(&url("https://anything.test/x")).is_allowed());
        assert!(!closed.allows_any_remote());
    }
}
