//! What the page is allowed to reach.
//!
//! The policy is an origin allowlist that is fail-closed by default: an empty
//! allowlist permits nothing, which matches `AllowList` on the sandbox side
//! rather than inventing a second, friendlier default. Two rules are worth
//! stating because they are where "allowlist" usually leaks:
//!
//! - **Every redirect hop is checked**, not just the first. An allowed origin
//!   that 302s to a denied one is a denial, and the hop is recorded. A proxy
//!   watching CONNECT lines cannot see that decision; here it is the same code
//!   path as the original request.
//! - **The navigation target is not implicitly trusted for subresources.**
//!   Asking to open a page grants that page's origin (you named it) and
//!   nothing else, so a docs page cannot quietly pull a tracker from a third
//!   party.

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
    allow_loopback: bool,
    max_redirects: usize,
    max_response_bytes: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            origins: BTreeSet::new(),
            // Loopback is the agent's own dev server. It never appears in an
            // egress allowlist and is the whole point of a dev loop, so it is
            // opt-out rather than opt-in — matching the sandbox's own handling.
            allow_loopback: true,
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
        if let Some(normalized) = normalize_origin(origin) {
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

    /// The one decision point. Every request and every redirect hop comes
    /// through here.
    pub fn check(&self, url: &Url) -> Verdict {
        match url.scheme() {
            // `data:` is inline in the document that already passed policy;
            // it reaches no network, so refusing it would only break pages
            // without protecting anything.
            "data" => return Verdict::Allow,
            "http" | "https" => {}
            other => {
                return Verdict::Deny(format!(
                    "scheme `{other}` is not fetchable by this engine (only http, https, data)"
                ));
            }
        }

        let Some(host) = url.host_str() else {
            return Verdict::Deny("request has no host".to_string());
        };

        if self.allow_loopback && is_loopback(host) {
            return Verdict::Allow;
        }

        let Some(origin) = normalize_origin(url.as_str()) else {
            return Verdict::Deny(format!("could not derive an origin from `{url}`"));
        };

        if self.origins.contains(&origin) {
            Verdict::Allow
        } else {
            // Name the origin, not just "denied": this string is what a human
            // reads when a page came back empty, and it is the retry hint.
            Verdict::Deny(format!("origin `{origin}` is not in the allowlist"))
        }
    }
}

fn is_loopback(host: &str) -> bool {
    // `[::1]` arrives with brackets stripped by `host_str`.
    matches!(host, "localhost" | "127.0.0.1" | "::1")
        || host.starts_with("127.")
        || host.ends_with(".localhost")
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

    let parsed = Url::parse(trimmed)
        .or_else(|_| Url::parse(&format!("https://{trimmed}")))
        .ok()?;

    let scheme = parsed.scheme();
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
}
