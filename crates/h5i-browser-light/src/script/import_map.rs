//! `<script type="importmap">`: the page saying where its bare specifiers go.
//!
//! [`super::modules::resolve`] refuses a bare specifier, because a loader that
//! rewrites `import "lodash"` to `https://esm.sh/lodash` has turned one line of
//! page script into a request to a third party the engine chose. An import map is
//! different: the page declares the mapping, so `esm.sh` appears in a receipt
//! because the document said so in markup the parser already read.
//!
//! The refusal keeps its target. A bare specifier with no map is still an error
//! naming what would have to exist; with a map it is a URL the page wrote down,
//! going through the same broker, policy check and receipt as any subresource. A
//! map pointing at an ungranted origin is refused at fetch time exactly as a
//! `<script src>` would be, and now names an origin instead of dying at
//! resolution with nothing recorded.
//!
//! `imports` and `scopes` are implemented, which is the whole of what pages use,
//! both under the specification's two-kind rule: a key ending in `/` is a prefix
//! over subtrees, any other key matches exactly, longest key wins.
//!
//! Absent on purpose: `integrity`, which would mean checking a digest this engine
//! does not compute anywhere, named here rather than ignored quietly; multiple
//! maps, where the first wins and later ones are errors per spec; and partial
//! application, since a malformed map is dropped whole with a console line rather
//! than leaving a page that resolves some imports and not others.

use std::collections::BTreeMap;

use url::Url;

/// One page's import map, already resolved against the document.
///
/// Keys stay as written; values are absolute URLs, resolved against the
/// document base when the map was parsed. Resolving at parse time rather than
/// at import time is what makes a relative value (`"./vendor/lodash.js"`) mean
/// the same thing from every importing module, which is what the specification
/// requires and what a reader expects.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ImportMap {
    imports: Specifiers,
    /// Scope prefix (an absolute URL) to the mapping that applies inside it.
    ///
    /// A `BTreeMap` because scope selection is longest-prefix-wins and the
    /// order has to be stable: two scopes of equal length that matched in
    /// different orders on different runs would make a page's module graph
    /// depend on hash iteration order.
    scopes: BTreeMap<String, Specifiers>,
}

/// One mapping table: exact keys and prefix keys, kept apart at parse time.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Specifiers {
    /// Keys that do not end in `/`. Matched whole.
    exact: BTreeMap<String, Url>,
    /// Keys that end in `/`. Match any specifier starting with them, and the
    /// remainder is appended to the value.
    prefix: BTreeMap<String, Url>,
}

impl Specifiers {
    fn parse(object: &serde_json::Map<String, serde_json::Value>, base: &Url) -> Self {
        let mut out = Specifiers::default();
        for (key, value) in object {
            if key.is_empty() {
                continue;
            }
            let Some(text) = value.as_str() else {
                // The specification says a non-string value makes that entry
                // invalid, not the map. Skipped rather than fatal, so one bad
                // line does not cost a page every other mapping it declared.
                continue;
            };
            // Values are resolved against the document, so `"./x.js"` means the
            // same thing however deep the importing module is.
            let Ok(url) = base.join(text) else { continue };
            // A prefix key must have a prefix value: the remainder is appended
            // to it, and appending to something that is not a directory would
            // silently produce a different path.
            if key.ends_with('/') {
                if !url.as_str().ends_with('/') {
                    continue;
                }
                out.prefix.insert(key.clone(), url);
            } else {
                out.exact.insert(key.clone(), url);
            }
        }
        out
    }

    /// Longest prefix wins, exact beats prefix.
    fn resolve(&self, specifier: &str) -> Option<Url> {
        if let Some(url) = self.exact.get(specifier) {
            return Some(url.clone());
        }
        let mut best: Option<(&String, &Url)> = None;
        for (key, url) in &self.prefix {
            if !specifier.starts_with(key.as_str()) {
                continue;
            }
            if best.is_none_or(|(seen, _)| key.len() > seen.len()) {
                best = Some((key, url));
            }
        }
        let (key, url) = best?;
        // The remainder is appended as a path, not joined as a relative URL: a
        // `..` in the tail must not walk out of the subtree the page granted.
        let remainder = &specifier[key.len()..];
        if remainder.contains("..") {
            return None;
        }
        Url::parse(&format!("{url}{remainder}")).ok()
    }

    fn is_empty(&self) -> bool {
        self.exact.is_empty() && self.prefix.is_empty()
    }
}

impl ImportMap {
    /// Parse one map's JSON, resolved against the document URL.
    ///
    /// `Err` is a map that is not usable at all; the message is what goes to
    /// the console, so it says what a page author would need to fix.
    pub fn parse(text: &str, base: &Url) -> Result<ImportMap, String> {
        let value: serde_json::Value = serde_json::from_str(text)
            .map_err(|e| format!("the import map is not valid JSON: {e}"))?;
        let Some(object) = value.as_object() else {
            return Err("an import map must be a JSON object".to_string());
        };

        let imports = object
            .get("imports")
            .and_then(serde_json::Value::as_object)
            .map(|o| Specifiers::parse(o, base))
            .unwrap_or_default();

        let mut scopes = BTreeMap::new();
        if let Some(declared) = object.get("scopes").and_then(serde_json::Value::as_object) {
            for (prefix, mapping) in declared {
                let Some(mapping) = mapping.as_object() else {
                    continue;
                };
                // A scope prefix is a URL, resolved against the document like
                // any other, so `"/app/"` names this origin's `/app/`.
                let Ok(resolved) = base.join(prefix) else {
                    continue;
                };
                scopes.insert(resolved.to_string(), Specifiers::parse(mapping, base));
            }
        }

        Ok(ImportMap { imports, scopes })
    }

    /// Whether this map maps anything. An empty map is kept rather than
    /// discarded so `has_map` can distinguish "the page declared one" from "the
    /// page declared none", which are different things to tell an author.
    pub fn is_empty(&self) -> bool {
        self.imports.is_empty() && self.scopes.values().all(Specifiers::is_empty)
    }

    /// Resolve `specifier` as imported from `referrer`.
    ///
    /// `None` means the map says nothing about it, which is not an error: the
    /// caller then falls through to ordinary URL resolution, and a bare
    /// specifier that reaches the end of that gets the refusal it always got.
    pub fn resolve(&self, specifier: &str, referrer: &Url) -> Option<Url> {
        // Scopes first, longest matching prefix wins, then the top-level
        // `imports`. That order is what lets a page override one dependency's
        // view of a package without changing everybody else's.
        let referrer = referrer.as_str();
        let mut candidates: Vec<(&String, &Specifiers)> = self
            .scopes
            .iter()
            .filter(|(prefix, _)| referrer.starts_with(prefix.as_str()))
            .collect();
        candidates.sort_by_key(|(prefix, _)| std::cmp::Reverse(prefix.len()));
        for (_, mapping) in candidates {
            if let Some(url) = mapping.resolve(specifier) {
                return Some(url);
            }
        }
        self.imports.resolve(specifier)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://example.test/app/index.html").unwrap()
    }

    #[test]
    fn a_bare_specifier_resolves_to_what_the_page_wrote_down() {
        // The whole point: the destination is the page's, not the engine's.
        let map = ImportMap::parse(
            r#"{"imports": {"lodash": "https://cdn.test/lodash.js"}}"#,
            &base(),
        )
        .unwrap();
        assert_eq!(
            map.resolve("lodash", &base()).map(|u| u.to_string()),
            Some("https://cdn.test/lodash.js".to_string())
        );
    }

    #[test]
    fn a_relative_value_resolves_against_the_document_not_the_importer() {
        // Otherwise the same map means different things depending on which
        // module did the importing, which is exactly the surprise the spec
        // resolves values early to avoid.
        let map =
            ImportMap::parse(r#"{"imports": {"util": "./lib/util.js"}}"#, &base()).unwrap();
        let deep = Url::parse("https://example.test/app/deep/nested/mod.js").unwrap();
        assert_eq!(
            map.resolve("util", &deep).map(|u| u.to_string()),
            Some("https://example.test/app/lib/util.js".to_string())
        );
    }

    #[test]
    fn a_trailing_slash_key_maps_a_whole_subtree() {
        let map = ImportMap::parse(
            r#"{"imports": {"pkg/": "https://cdn.test/pkg@1/"}}"#,
            &base(),
        )
        .unwrap();
        assert_eq!(
            map.resolve("pkg/deep/thing.js", &base()).map(|u| u.to_string()),
            Some("https://cdn.test/pkg@1/deep/thing.js".to_string())
        );
    }

    #[test]
    fn the_longest_matching_prefix_wins() {
        let map = ImportMap::parse(
            r#"{"imports": {"a/": "https://cdn.test/one/", "a/b/": "https://cdn.test/two/"}}"#,
            &base(),
        )
        .unwrap();
        assert_eq!(
            map.resolve("a/b/x.js", &base()).map(|u| u.to_string()),
            Some("https://cdn.test/two/x.js".to_string())
        );
        assert_eq!(
            map.resolve("a/z.js", &base()).map(|u| u.to_string()),
            Some("https://cdn.test/one/z.js".to_string())
        );
    }

    #[test]
    fn a_prefix_key_cannot_be_walked_out_of() {
        // The remainder is appended, so `..` in it would reach outside the
        // subtree the page granted. Refused rather than normalised, because a
        // page writing `..` into a package path is not doing what the map says.
        let map = ImportMap::parse(
            r#"{"imports": {"pkg/": "https://cdn.test/pkg@1/"}}"#,
            &base(),
        )
        .unwrap();
        assert_eq!(map.resolve("pkg/../../etc/passwd", &base()), None);
    }

    #[test]
    fn a_prefix_key_needs_a_prefix_value() {
        // `{"pkg/": "https://cdn.test/pkg.js"}` would append the remainder onto
        // a filename. Dropped, so the specifier falls through to the ordinary
        // refusal rather than fetching a nonsense URL.
        let map =
            ImportMap::parse(r#"{"imports": {"pkg/": "https://cdn.test/pkg.js"}}"#, &base())
                .unwrap();
        assert_eq!(map.resolve("pkg/x.js", &base()), None);
    }

    #[test]
    fn a_scope_overrides_the_top_level_for_modules_under_it() {
        let map = ImportMap::parse(
            r#"{
                 "imports": {"dep": "https://cdn.test/dep@2.js"},
                 "scopes": {"/legacy/": {"dep": "https://cdn.test/dep@1.js"}}
               }"#,
            &base(),
        )
        .unwrap();
        let legacy = Url::parse("https://example.test/legacy/old.js").unwrap();
        assert_eq!(
            map.resolve("dep", &legacy).map(|u| u.to_string()),
            Some("https://cdn.test/dep@1.js".to_string())
        );
        assert_eq!(
            map.resolve("dep", &base()).map(|u| u.to_string()),
            Some("https://cdn.test/dep@2.js".to_string())
        );
    }

    #[test]
    fn an_unmapped_specifier_says_nothing_rather_than_guessing() {
        // The property the refusal depends on. A map that answered every
        // question would be the CDN-inventing loader under a new name.
        let map =
            ImportMap::parse(r#"{"imports": {"lodash": "https://cdn.test/l.js"}}"#, &base())
                .unwrap();
        assert_eq!(map.resolve("react", &base()), None);
    }

    #[test]
    fn a_malformed_map_is_refused_whole() {
        assert!(ImportMap::parse("{not json", &base()).is_err());
        assert!(ImportMap::parse("[]", &base()).is_err());
    }

    #[test]
    fn one_bad_entry_does_not_cost_the_others() {
        let map = ImportMap::parse(
            r#"{"imports": {"good": "https://cdn.test/g.js", "bad": 7}}"#,
            &base(),
        )
        .unwrap();
        assert!(map.resolve("good", &base()).is_some());
        assert_eq!(map.resolve("bad", &base()), None);
    }
}
