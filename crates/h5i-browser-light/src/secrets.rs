//! Credentials the agent can use and cannot read.
//!
//! ```text
//! $ H5I_SECRET_ACME_PASSWORD=hunter2 h5i browser open https://acme.test/
//! $ h5i browser env
//! H5I_SECRET_ACME_PASSWORD          # the name. never the value
//! $ h5i browser type @e2 '$H5I_SECRET_ACME_PASSWORD'
//! {"ok":true,"ref":"@e2","used":["H5I_SECRET_ACME_PASSWORD"]}
//! ```
//!
//! The model names a credential, the engine resolves it on the way to the field,
//! and the reply echoes the *placeholder*. The value never enters the model's
//! context, so it cannot be repeated back, logged, or carried onward. LOGIN mode
//! is the other answer and has a hole this does not: it withholds the agent's
//! reads but not the *frames*, and the viewer socket is inside the box.
//!
//! Only `H5I_SECRET_*` is reachable. The whole `H5I_*` namespace would also carry
//! engine configuration (`H5I_EGRESS_PROXY`, `H5I_BROWSER_RECEIPTS`), and a
//! page-bound `type` putting the receipts path into a form is disclosure with no
//! upside. A denylist would work until somebody added a variable; a prefix
//! allowlist fails closed.
//!
//! Anything written back out goes through [`Secrets::redact`], which iterates
//! longest value first: with one secret a substring of another, replacing the
//! shorter first leaves the longer one's tail in the clear.

use std::collections::BTreeMap;

/// The one namespace a page-bound value may be drawn from.
pub const PREFIX: &str = "H5I_SECRET_";

/// A value shorter than this is not substituted back out.
///
/// Reverse substitution over a two-character secret would rewrite half the
/// prose it touched. Forward substitution is unaffected: a short secret still
/// resolves, it is only the redaction of *outgoing* text that skips it, and the
/// engine does not write incoming values anywhere anyway.
const MIN_REDACTABLE: usize = 4;

/// What this session may substitute.
#[derive(Debug, Clone, Default)]
pub struct Secrets {
    /// Name to value. Sorted by name for a stable `names()`.
    by_name: BTreeMap<String, String>,
}

impl Secrets {
    /// Read the namespace out of the environment.
    ///
    /// Done once, at session start, so a later `setenv` by anything sharing the
    /// process cannot widen what the agent can reach.
    pub fn from_env() -> Self {
        let mut by_name = BTreeMap::new();
        for (name, value) in std::env::vars() {
            if name.starts_with(PREFIX) && !value.is_empty() {
                by_name.insert(name, value);
            }
        }
        Secrets { by_name }
    }

    /// Build from explicit pairs, applying the same namespace rule.
    ///
    /// The filter is here rather than only in `from_env` so the invariant holds
    /// for every constructor: a `Secrets` cannot contain something outside the
    /// namespace, whatever built it.
    pub fn from_pairs(pairs: &[(&str, &str)]) -> Self {
        Secrets {
            by_name: pairs
                .iter()
                .filter(|(name, value)| name.starts_with(PREFIX) && !value.is_empty())
                .map(|(n, v)| (n.to_string(), v.to_string()))
                .collect(),
        }
    }

    /// The names, and only the names.
    ///
    /// This is the whole discovery surface. An agent learns that a credential
    /// exists and what to call it, which is everything it needs to use one and
    /// nothing it needs to leak one.
    pub fn names(&self) -> Vec<&str> {
        self.by_name.keys().map(String::as_str).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    /// Resolve `$H5I_SECRET_*` placeholders, reporting which were used.
    ///
    /// A placeholder naming something that is not set is left *as written*
    /// rather than replaced with an empty string. Typing an empty password into
    /// a login form produces a failed login that looks like a wrong password,
    /// which is a confusing way to learn that a variable was misspelled; the
    /// literal text at least shows up in the field.
    pub fn substitute(&self, text: &str) -> Resolved {
        let mut out = String::with_capacity(text.len());
        let mut used: Vec<String> = Vec::new();
        let mut missing: Vec<String> = Vec::new();
        let mut rest = text;

        while let Some(at) = rest.find('$') {
            out.push_str(&rest[..at]);
            let after = &rest[at + 1..];
            let end = after
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .unwrap_or(after.len());
            let name = &after[..end];

            if name.starts_with(PREFIX) {
                match self.by_name.get(name) {
                    Some(value) => {
                        out.push_str(value);
                        if !used.iter().any(|u| u == name) {
                            used.push(name.to_string());
                        }
                    }
                    None => {
                        out.push('$');
                        out.push_str(name);
                        if !missing.iter().any(|m| m == name) {
                            missing.push(name.to_string());
                        }
                    }
                }
            } else {
                // Not ours. A `$` in a password is a `$` in a password.
                out.push('$');
                out.push_str(name);
            }
            rest = &after[end..];
        }
        out.push_str(rest);

        Resolved {
            text: out,
            used,
            missing,
        }
    }

    /// Put placeholders back where values appear.
    ///
    /// Longest value first. With `H5I_SECRET_A=hunter` and
    /// `H5I_SECRET_B=hunter2`, replacing `A` first turns `hunter2` into
    /// `$H5I_SECRET_A2` and leaves the `2` in the clear. A partial disclosure
    /// that looks like a successful redaction.
    pub fn redact(&self, text: &str) -> String {
        let mut pairs: Vec<(&String, &String)> = self
            .by_name
            .iter()
            .filter(|(_, value)| value.len() >= MIN_REDACTABLE)
            .collect();
        pairs.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(b.0)));

        let mut out = text.to_string();
        for (name, value) in pairs {
            if out.contains(value.as_str()) {
                out = out.replace(value.as_str(), &format!("${name}"));
            }
        }
        out
    }
}

/// What a substitution did.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Resolved {
    /// The text to hand to the page.
    pub text: String,
    /// Names that were resolved. Reported so a receipt can say a credential was
    /// used without carrying it.
    pub used: Vec<String>,
    /// Placeholders that named nothing. Left as written in `text`.
    pub missing: Vec<String>,
}

impl Resolved {
    /// Whether anything was substituted.
    pub fn substituted(&self) -> bool {
        !self.used.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secrets() -> Secrets {
        Secrets::from_pairs(&[
            ("H5I_SECRET_USER", "alice"),
            ("H5I_SECRET_PASS", "hunter2"),
        ])
    }

    #[test]
    fn a_placeholder_resolves_and_names_what_it_used() {
        let got = secrets().substitute("$H5I_SECRET_PASS");
        assert_eq!(got.text, "hunter2");
        assert_eq!(got.used, vec!["H5I_SECRET_PASS"]);
        assert!(got.missing.is_empty());
        assert!(got.substituted());
    }

    #[test]
    fn only_the_secret_namespace_is_reachable() {
        // The narrowing that matters. h5i uses `H5I_*` for engine
        // configuration, and a page-bound `type` must not be able to put the
        // receipts path or the proxy address into a form.
        let s = Secrets::from_pairs(&[("H5I_EGRESS_PROXY", "http://proxy:3128")]);
        let got = s.substitute("$H5I_EGRESS_PROXY");
        assert_eq!(
            got.text, "$H5I_EGRESS_PROXY",
            "an engine variable is not a credential and is not substituted"
        );
        assert!(got.used.is_empty());
        assert!(
            got.missing.is_empty(),
            "and it is not even reported as a missing one"
        );
        assert!(s.names().is_empty(), "nor is it discoverable");
    }

    #[test]
    fn a_missing_placeholder_stays_as_written() {
        // An empty string here produces a failed login that looks like a wrong
        // password, which is a confusing way to learn about a typo.
        let got = secrets().substitute("$H5I_SECRET_NOPE");
        assert_eq!(got.text, "$H5I_SECRET_NOPE");
        assert_eq!(got.missing, vec!["H5I_SECRET_NOPE"]);
        assert!(got.used.is_empty());
    }

    #[test]
    fn a_dollar_that_is_not_ours_survives() {
        let got = secrets().substitute("pa$$word$USER");
        assert_eq!(got.text, "pa$$word$USER");
        assert!(got.used.is_empty());
    }

    #[test]
    fn placeholders_can_sit_inside_other_text() {
        let got = secrets().substitute("user=$H5I_SECRET_USER;pass=$H5I_SECRET_PASS;");
        assert_eq!(got.text, "user=alice;pass=hunter2;");
        assert_eq!(got.used, vec!["H5I_SECRET_USER", "H5I_SECRET_PASS"]);
    }

    #[test]
    fn redaction_takes_the_longest_value_first() {
        // The subtle one. With a short secret that is a prefix of a long one,
        // replacing the short one first leaves the tail of the long one in the
        // clear. A partial disclosure that looks like a successful redaction.
        let s = Secrets::from_pairs(&[
            ("H5I_SECRET_SHORT", "hunter"),
            ("H5I_SECRET_LONG", "hunter2"),
        ]);
        let out = s.redact("logged in with hunter2");
        assert_eq!(out, "logged in with $H5I_SECRET_LONG");
        assert!(!out.contains('2'), "a tail survived: {out}");
    }

    #[test]
    fn redaction_skips_values_too_short_to_be_distinctive() {
        let s = Secrets::from_pairs(&[("H5I_SECRET_TINY", "ab")]);
        assert_eq!(s.redact("a table of absolutes"), "a table of absolutes");
    }

    #[test]
    fn names_are_the_whole_discovery_surface() {
        let s = secrets();
        let names = s.names();
        assert_eq!(names, vec!["H5I_SECRET_PASS", "H5I_SECRET_USER"]);
        // There is deliberately no accessor that returns a value. If one is
        // ever added, this test is where the argument for it belongs.
        let listed = format!("{names:?}");
        assert!(!listed.contains("hunter2"), "{listed}");
        assert!(!listed.contains("alice"), "{listed}");
    }
}
