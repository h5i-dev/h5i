//! A durable handle for an element, beside the ordinal one.
//!
//! `@e5` names a position in the walk that minted it, and the session refuses
//! one against a reading the page has moved on from (`stream::resolve_ref`).
//! That makes the ordinal safe, not durable: a recorded session made of ordinals
//! replays into a different page.
//!
//! So each ref also carries the **simplest CSS selector whose first match is
//! that element**, built the way Lightpanda's `SelectorPath` builds one: start
//! from the element's own segment; if that is not already unique-first, walk
//! ancestors and prepend one **only when it shrinks the match count**, since an
//! ancestor that narrows nothing is length with no information; then fall back
//! to a strict `a > b > c` chain.
//!
//! **Every candidate is verified with the same matcher the action verbs use.** A
//! generated selector the engine's own `querySelectorAll` would resolve
//! differently is worse than no selector, because it looks like a handle.
//!
//! Not built: Lightpanda disambiguates with `:has()` before falling back to
//! `:nth-of-type`, which is markedly more robust on machine-generated markup. It
//! needs `:has()` in the borrowed selector parser, which is unverified here, and
//! emitting selectors the matcher then rejects would produce exactly the
//! plausible-looking handle this module exists to avoid.

use blitz_dom::BaseDocument;

/// Attributes worth putting in a selector, most stable first.
///
/// `id` is handled separately because it can stand alone. The rest are ordered
/// by how likely they are to survive a redeploy: a test hook outlives a form
/// field name, which outlives a placeholder, which outlives a class.
const STABLE_ATTRS: &[&str] = &["data-testid", "data-test-id", "data-test", "name"];

/// How far up to walk before giving up on narrowing.
const MAX_ANCESTORS: usize = 32;

/// Selector results already computed against one unchanged document.
///
/// Every candidate here is verified with a full-document query, and a snapshot
/// mints a selector for *every* ref it serves. The candidates repeat heavily
/// across siblings — fifty rows in a table share every ancestor segment above
/// the row — so without this the same query runs once per ref that shares it.
///
/// Correct only for as long as the document does not change, which is why it is
/// created per snapshot rather than held on the session: a cache that outlived
/// a mutation would verify selectors against a page that had moved on, which is
/// the exact failure the verification exists to prevent.
#[derive(Default)]
pub struct Cache {
    seen: std::collections::HashMap<String, Vec<usize>>,
}

impl Cache {
    pub fn new() -> Self {
        Self::default()
    }

    fn matches(&mut self, doc: &BaseDocument, selector: &str) -> &[usize] {
        // `entry` would need an owned key on every hit, and the hit is the
        // common case here by construction.
        if !self.seen.contains_key(selector) {
            let found = matches(doc, selector);
            self.seen.insert(selector.to_string(), found);
        }
        &self.seen[selector]
    }

    /// Whether this selector's **first** match is the node we want.
    ///
    /// First, not "is among", because that is what an action does with a
    /// selector: `querySelector` semantics. A selector that matches the target
    /// third is not a handle on the target.
    fn resolves_to(&mut self, doc: &BaseDocument, selector: &str, node_id: usize) -> bool {
        self.matches(doc, selector).first() == Some(&node_id)
    }
}

/// The simplest verified selector for one node, or `None` if none can be built.
///
/// `None` rather than a guess: an element whose selector cannot be verified is
/// better reported as having none than handed a string that resolves elsewhere.
pub fn for_node(doc: &BaseDocument, node_id: usize) -> Option<String> {
    for_node_cached(doc, node_id, &mut Cache::new())
}

/// The same, reusing work already done against this document.
pub fn for_node_cached(doc: &BaseDocument, node_id: usize, cache: &mut Cache) -> Option<String> {
    let mut current = local_segment_cached(doc, node_id, cache)?;
    if cache.resolves_to(doc, &current, node_id) {
        return Some(current);
    }

    let mut narrowest = cache.matches(doc, &current).len();
    let mut ancestor = doc.get_node(node_id).and_then(|n| n.parent);
    let mut walked = 0;

    while let Some(id) = ancestor {
        walked += 1;
        if walked > MAX_ANCESTORS {
            break;
        }
        if let Some(segment) = local_segment_cached(doc, id, cache) {
            let candidate = format!("{segment} {current}");
            // One query answers both questions. Asking `matches` for the count
            // and then `resolves_to` for the first element ran the same
            // full-document query twice for every ancestor that narrowed.
            let found = cache.matches(doc, &candidate);
            let count = found.len();
            let is_first = found.first() == Some(&node_id);
            // Only when it actually narrows. An ancestor that leaves the count
            // where it was has added length and no information, and a longer
            // selector is a more brittle one.
            if count < narrowest {
                narrowest = count;
                current = candidate;
                if is_first {
                    return Some(current);
                }
            }
        }
        ancestor = doc.get_node(id).and_then(|n| n.parent);
    }

    strict_path(doc, node_id).filter(|path| cache.resolves_to(doc, path, node_id))
}

/// One element's own segment, without any ancestor context.
fn local_segment_cached(
    doc: &BaseDocument,
    node_id: usize,
    cache: &mut Cache,
) -> Option<String> {
    let node = doc.get_node(node_id)?;
    let element = node.element_data()?;
    let tag = element.name.local.to_string();

    // An id that resolves to this element is the whole answer. Checked rather
    // than assumed: duplicate ids are legal in the wild and `#dup` names the
    // first one, which may not be this one.
    if let Some(id) = attr(doc, node_id, "id")
        && !id.is_empty()
        && is_css_ident(&id)
    {
        let candidate = format!("#{id}");
        if cache.resolves_to(doc, &candidate, node_id) {
            return Some(candidate);
        }
    }

    for name in STABLE_ATTRS {
        if let Some(value) = attr(doc, node_id, name)
            && !value.is_empty()
        {
            let candidate = format!("{tag}[{name}=\"{}\"]", escape_attr(&value));
            if cache.resolves_to(doc, &candidate, node_id) {
                return Some(candidate);
            }
        }
    }

    // Position among same-tag siblings. Always constructible, never unique on
    // its own, which is what the ancestor walk above is for.
    match nth_of_type(doc, node_id) {
        Some(n) if n > 1 => Some(format!("{tag}:nth-of-type({n})")),
        Some(_) => Some(tag),
        None => Some(tag),
    }
}

/// The full `a > b > c` chain from the root.
fn strict_path(doc: &BaseDocument, node_id: usize) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut current = Some(node_id);
    let mut walked = 0;

    while let Some(id) = current {
        walked += 1;
        if walked > MAX_ANCESTORS {
            return None;
        }
        let node = doc.get_node(id)?;
        if node.element_data().is_none() {
            break;
        }
        let tag = node.element_data()?.name.local.to_string();
        let part = match nth_of_type(doc, id) {
            Some(n) if n > 1 => format!("{tag}:nth-of-type({n})"),
            _ => tag,
        };
        parts.push(part);
        current = node.parent;
    }

    if parts.is_empty() {
        return None;
    }
    parts.reverse();
    Some(parts.join(" > "))
}

/// This node's 1-based position among its same-tag siblings.
fn nth_of_type(doc: &BaseDocument, node_id: usize) -> Option<usize> {
    let node = doc.get_node(node_id)?;
    let tag = &node.element_data()?.name.local;
    let parent = doc.get_node(node.parent?)?;

    let mut n = 0usize;
    for child in parent.children.iter() {
        let Some(sibling) = doc.get_node(*child) else {
            continue;
        };
        let Some(data) = sibling.element_data() else {
            continue;
        };
        if &data.name.local == tag {
            n += 1;
            if *child == node_id {
                return Some(n);
            }
        }
    }
    None
}

fn attr(doc: &BaseDocument, node_id: usize, name: &str) -> Option<String> {
    doc.get_node(node_id)?
        .element_data()?
        .attrs
        .iter()
        .find(|a| &*a.name.local == name)
        .map(|a| a.value.to_string())
}

/// Every match, through the matcher the action verbs use.
fn matches(doc: &BaseDocument, selector: &str) -> Vec<usize> {
    crate::script::dom_api::matches_within(doc, 0, selector)
}


/// Whether a value can go after `#` without quoting.
///
/// Conservative: anything outside this is written as an attribute selector
/// instead, or skipped. A CSS identifier escaper is a small pile of rules and
/// getting one subtly wrong produces a selector that parses and matches the
/// wrong thing.
fn is_css_ident(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with(|c: char| c.is_ascii_digit())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn escape_attr(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(html: &str) -> crate::engine::Page {
        let broker = crate::net::LocalBroker::new(
                crate::policy::Policy::new(),
                std::sync::Arc::new(crate::receipt::MemorySink::new()),
                None,
            )
            .expect("broker");
        let fonts = crate::fonts::load(&[], &crate::fonts::default_font_dirs(), Some(2));
        let factory = crate::engine::PageFactory::new(
            broker,
            fonts.sources.clone(),
            crate::engine::PageOptions::default(),
        );
        let base = url::Url::parse("https://app.example/").unwrap();
        factory.from_html(html, &base)
    }

    /// The selector for the first ref whose name matches, and the node it named.
    fn selector_for(html: &str, name: &str) -> (String, usize) {
        let page = page(html);
        let snapshot = page.snapshot();
        let entry = snapshot
            .refs
            .iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("no ref named {name:?} in {:?}", snapshot.refs));
        let dom = page.dom();
        let doc = dom.borrow();
        let selector = for_node(&doc, entry.node_id)
            .unwrap_or_else(|| panic!("no selector for {name:?}"));
        (selector, entry.node_id)
    }

    #[test]
    fn an_id_that_resolves_is_the_whole_answer() {
        let (selector, _) = selector_for(
            "<html><body><button id='save'>Save</button></body></html>",
            "Save",
        );
        assert_eq!(selector, "#save");
    }

    #[test]
    fn a_duplicate_id_is_not_trusted() {
        // Duplicate ids are legal in the wild and `#dup` names the first one.
        // Trusting it would hand out a selector that resolves to a different
        // element, which is worse than handing out none.
        let html = "<html><body><div><button id='dup'>First</button></div>\
                    <div><button id='dup'>Second</button></div></body></html>";
        let (selector, node) = selector_for(html, "Second");
        assert_ne!(selector, "#dup", "it would resolve to the first one");

        let p = page(html);
        let dom = p.dom();
        let doc = dom.borrow();
        // Whatever it produced, it resolves to the element it was asked about.
        let all = matches(&doc, &selector);
        assert_eq!(all.first().copied(), Some(node), "{selector}");
    }

    #[test]
    fn a_stable_attribute_beats_a_position() {
        let (selector, _) = selector_for(
            "<html><body><form><input name='q'><input name='acct'></form></body></html>",
            "acct",
        );
        assert!(
            selector.contains("[name=\"acct\"]"),
            "a name outlives a position: {selector}"
        );
    }

    #[test]
    fn an_ancestor_is_added_only_when_it_narrows() {
        // Two identical buttons in different containers. The selector has to
        // reach for an ancestor, and the one it reaches for has to be the one
        // that distinguishes them.
        let html = "<html><body>\
                    <div id='left'><button>Go</button></div>\
                    <div id='right'><button>Go</button></div>\
                    </body></html>";
        let p = page(html);
        let snapshot = p.snapshot();
        let dom = p.dom();
        let doc = dom.borrow();

        let buttons: Vec<_> = snapshot.refs.iter().filter(|r| r.name == "Go").collect();
        assert_eq!(buttons.len(), 2, "the fixture needs two");

        for entry in buttons {
            let selector = for_node(&doc, entry.node_id).expect("a selector");
            assert_eq!(
                matches(&doc, &selector).first().copied(),
                Some(entry.node_id),
                "{selector} does not resolve to the element it describes"
            );
        }
    }

    #[test]
    fn every_ref_on_a_realistic_page_gets_a_selector_that_resolves_to_it() {
        // The property that matters, asserted over a whole page rather than a
        // hand-picked element: whatever this produces, the engine's own matcher
        // agrees it names that element first.
        let html = "<html><body>\
                    <nav><a href='/a'>One</a><a href='/b'>Two</a></nav>\
                    <main>\
                      <form action='/s'>\
                        <input name='user'><input name='pass' type='password'>\
                        <select name='role'><option>admin</option></select>\
                        <textarea name='bio'></textarea>\
                        <button type='submit'>Sign in</button>\
                      </form>\
                      <ul><li><a href='/1'>Item</a></li><li><a href='/2'>Item</a></li></ul>\
                    </main></body></html>";
        let p = page(html);
        let snapshot = p.snapshot();
        let dom = p.dom();
        let doc = dom.borrow();

        assert!(snapshot.refs.len() >= 8, "{:?}", snapshot.refs);
        for entry in &snapshot.refs {
            let selector = for_node(&doc, entry.node_id)
                .unwrap_or_else(|| panic!("no selector for {:?}", entry.id));
            assert_eq!(
                matches(&doc, &selector).first().copied(),
                Some(entry.node_id),
                "ref {} ({}) got {selector:?}, which resolves elsewhere",
                entry.id,
                entry.role
            );
        }
    }

    #[test]
    fn an_ident_that_needs_escaping_is_not_written_as_an_id() {
        assert!(is_css_ident("save-button"));
        assert!(is_css_ident("save_button"));
        assert!(!is_css_ident("2fast"), "cannot start with a digit");
        assert!(!is_css_ident("has space"));
        assert!(!is_css_ident("has.dot"), "a dot would read as a class");
        assert!(!is_css_ident(""));
    }
}
