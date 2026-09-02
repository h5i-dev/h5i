//! A durable handle for an element, beside the ordinal one.

use blitz_dom::BaseDocument;

/// Attributes worth putting in a selector, most stable first.
///
/// `id` is handled separately because it can stand alone. The rest are ordered
/// by how likely they are to survive a redeploy: a test hook outlives a form
/// field name, which outlives a placeholder, which outlives a class.
const STABLE_ATTRS: &[&str] = &["data-testid", "data-test-id", "data-test", "name"];

/// The attribute a link carries when it carries nothing else.
///
/// Last resort before falling back to position, and the reason it is worth
/// having: a link is the commonest actionable element on a page and the one
/// least likely to have any of the attributes above, so without this every
/// link walked its ancestors, and each step of that walk is a full-document
/// query. Measured on a 72-ref page, the walk was 4.5 ms against 0.13 ms for
/// reading the whole page.
///
/// It is also the better handle. `a[href="/pricing"]` says what the link is;
/// `section:nth-of-type(37) p a` says where it happened to sit this morning,
/// and stops being true when anything is inserted above it.
const LINK_ATTR: &str = "href";

/// How long an attribute value may be before it is not worth putting in a
/// selector. A tracking URL with forty query parameters is unique, and it is
/// also unreadable and no more durable than the position it replaced.
const MAX_ATTR_VALUE: usize = 120;

/// How far up to walk before giving up on narrowing.
const MAX_ANCESTORS: usize = 32;

/// Selector results already computed against one unchanged document.
///
/// Two maps, because the module asks two different questions and only one of
/// them needs the whole answer. "Is this selector a handle on that element"
/// needs the first match and nothing else; "does adding this ancestor narrow
/// it" needs the count. Stylo will stop at the first match if asked to, and on
/// a page of three hundred refs, whose selectors all resolve to exactly one
/// element, asking the narrower question is between two and three times
/// faster.
#[derive(Default)]
pub struct Cache {
    seen: std::collections::HashMap<String, Vec<usize>>,
    firsts: std::collections::HashMap<String, Option<usize>>,
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

    /// Whether this selector's *first* match is the node we want.
    ///
    /// First, not "is among", because that is what an action does with a
    /// selector: `querySelector` semantics. A selector that matches the target
    /// third is not a handle on the target.
    ///
    /// Answered with a query that stops there, which is the same question the
    /// full one was being asked and then read the first element of. The two
    /// agree by construction, both walking the tree in document order through
    /// the same matcher, and `both_query_modes_name_the_same_element` is what
    /// says so rather than this comment.
    fn resolves_to(&mut self, doc: &BaseDocument, selector: &str, node_id: usize) -> bool {
        // A selector whose every match is already known does not need a second
        // query to be asked about its first one.
        if let Some(found) = self.seen.get(selector) {
            return found.first() == Some(&node_id);
        }
        if !self.firsts.contains_key(selector) {
            let first = first_match(doc, selector);
            self.firsts.insert(selector.to_string(), first);
        }
        self.firsts[selector] == Some(node_id)
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

    // A link's target, when nothing steadier named it.
    //
    // Tried after the attributes above and before position, and only when it
    // is short enough to read. Safe to try at all because every candidate here
    // is verified: an `href="#"` shared by forty links resolves to the first
    // of them, fails the check, and costs one query before the walk it would
    // have done anyway.
    if let Some(value) = attr(doc, node_id, LINK_ATTR)
        && !value.is_empty()
        && value.len() <= MAX_ATTR_VALUE
    {
        let candidate = format!("{tag}[{LINK_ATTR}=\"{}\"]", escape_attr(&value));
        if cache.resolves_to(doc, &candidate, node_id) {
            return Some(candidate);
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

/// The first match, through the same matcher, stopping there.
fn first_match(doc: &BaseDocument, selector: &str) -> Option<usize> {
    crate::script::dom_api::first_match_in_document(doc, selector)
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
    fn a_link_is_named_by_where_it_goes() {
        // Nothing else names these two: same tag, same text, no id, no name.
        // Without the href they would each need an ancestor walk, and each
        // step of that is a full-document query.
        let html = "<html><body>\
                    <section><p>see <a href='/pricing'>Read more</a></p></section>\
                    <section><p>see <a href='/docs'>Read more</a></p></section>\
                    </body></html>";
        let p = page(html);
        let snapshot = p.snapshot();
        let dom = p.dom();
        let doc = dom.borrow();

        let links: Vec<_> = snapshot.refs.iter().filter(|r| r.role == "link").collect();
        assert_eq!(links.len(), 2, "{:?}", snapshot.refs);
        for entry in links {
            let selector = for_node(&doc, entry.node_id).expect("a selector");
            assert!(
                selector.starts_with("a[href="),
                "a link should be named by its target, got {selector}"
            );
            assert_eq!(
                matches(&doc, &selector).first().copied(),
                Some(entry.node_id),
                "{selector} resolves elsewhere"
            );
        }
    }

    #[test]
    fn a_target_too_many_links_share_is_not_a_handle() {
        // The case the verification exists for. Every link points at `#`, so
        // `a[href="#"]` names the first one; the rest have to fall back, and
        // whatever they fall back to still has to resolve to them.
        let html = "<html><body>\
                    <div id='a'><a href='#'>One</a></div>\
                    <div id='b'><a href='#'>Two</a></div>\
                    </body></html>";
        let p = page(html);
        let snapshot = p.snapshot();
        let dom = p.dom();
        let doc = dom.borrow();

        let links: Vec<_> = snapshot.refs.iter().filter(|r| r.role == "link").collect();
        assert_eq!(links.len(), 2, "{:?}", snapshot.refs);
        for entry in &links {
            let selector = for_node(&doc, entry.node_id).expect("a selector");
            assert_eq!(
                matches(&doc, &selector).first().copied(),
                Some(entry.node_id),
                "{selector} resolves elsewhere"
            );
        }
        // The second one cannot be the bare shared target.
        let second = for_node(&doc, links[1].node_id).expect("a selector");
        assert_ne!(second, "a[href=\"#\"]");
    }

    #[test]
    fn an_unreadable_target_is_left_to_position() {
        let long = "/x?".to_string() + &"p=1&".repeat(60);
        assert!(long.len() > MAX_ATTR_VALUE);
        let html = format!(
            "<html><body><section><a href='{long}'>Go</a></section></body></html>"
        );
        let p = page(&html);
        let snapshot = p.snapshot();
        let dom = p.dom();
        let doc = dom.borrow();
        let entry = snapshot.refs.iter().find(|r| r.role == "link").expect("a link");
        let selector = for_node(&doc, entry.node_id).expect("a selector");
        assert!(
            !selector.contains("p=1&p=1"),
            "a tracking URL is unique and unreadable: {selector}"
        );
        assert_eq!(matches(&doc, &selector).first().copied(), Some(entry.node_id));
    }

    /// The two query modes name the same element, or the faster one is
    /// answering a different question.
    ///
    /// This is the whole safety argument for asking Stylo to stop at the first
    /// match: `resolves_to` used to read the first element of every match and
    /// now asks for only that one, and the two have to agree on every selector
    /// this module can produce. Checked over the selectors it actually
    /// produces, on the shapes that make it produce different kinds.
    #[test]
    fn both_query_modes_name_the_same_element() {
        let pages = [
            "<html><body><a href='/a'>one</a><a href='/b'>two</a>\
             <a href='#'>x</a><a href='#'>y</a></body></html>",
            "<html><body><div id='l'><button>Go</button></div>\
             <div id='r'><button>Go</button></div></body></html>",
            "<html><body><form><input name='u'><input name='p'>\
             <select name='s'><option>a</option></select>\
             <button type='submit'>Send</button></form></body></html>",
            "<html><body><section><p>t <a href='/x'>link</a></p></section>\
             <section><p>t <a href='/y'>link</a></p></section></body></html>",
            "<html><body><ul><li><a href='/1'>Item</a></li>\
             <li><a href='/2'>Item</a></li></ul></body></html>",
        ];
        let mut checked = 0usize;
        for html in pages {
            let p = page(html);
            let snapshot = p.snapshot();
            let dom = p.dom();
            let doc = dom.borrow();
            assert!(!snapshot.refs.is_empty(), "fixture mints no refs: {html}");
            for entry in &snapshot.refs {
                let Some(selector) = for_node(&doc, entry.node_id) else {
                    continue;
                };
                assert_eq!(
                    first_match(&doc, &selector),
                    matches(&doc, &selector).first().copied(),
                    "{selector} is a different element depending on how it is asked"
                );
                // ...and it is still the element it was built for.
                assert_eq!(first_match(&doc, &selector), Some(entry.node_id), "{selector}");
                checked += 1;
            }
        }
        assert!(checked >= 12, "only {checked} selectors were compared");
    }

    /// Both caches answer for one document, so they must not disagree either.
    #[test]
    fn the_two_caches_agree_within_one_reading() {
        let html = "<html><body><div id='l'><button>Go</button></div>                    <div id='r'><button>Go</button></div>                    <a href='#'>x</a><a href='#'>y</a></body></html>";
        let p = page(html);
        let snapshot = p.snapshot();
        let dom = p.dom();
        let doc = dom.borrow();

        for entry in &snapshot.refs {
            // Warm the all-matches side first, then ask the first-match
            // question, and then the other way round on a fresh cache.
            let mut warm = Cache::new();
            let selector = for_node_cached(&doc, entry.node_id, &mut warm).expect("a selector");
            let via_warm = warm.resolves_to(&doc, &selector, entry.node_id);

            let mut cold = Cache::new();
            let via_cold = cold.resolves_to(&doc, &selector, entry.node_id);

            assert!(via_warm, "{selector} stopped resolving once its matches were known");
            assert_eq!(via_warm, via_cold, "{selector} depends on cache order");
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

#[cfg(test)]
mod query_mode_measurement {
    use super::*;

    /// The first match, through the same matcher, stopping there.
    fn first(doc: &BaseDocument, selector: &str) -> Option<usize> {
        crate::script::dom_api::first_match_in_document(doc, selector)
    }

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
        factory.from_html(html, &url::Url::parse("https://app.example/").unwrap())
    }

    fn median(mut body: impl FnMut()) -> f64 {
        body();
        let mut samples: Vec<f64> = (0..9)
            .map(|_| {
                let start = std::time::Instant::now();
                body();
                start.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
        samples[samples.len() / 2]
    }

    /// How much of the selector pass a first-match query could give back.
    ///
    /// Run with `cargo test --release -p h5i-browser --lib
    /// query_mode_measurement -- --ignored --nocapture`. Ignored by default
    /// because it is a measurement, not an assertion: it prints a ceiling for
    /// a change, and the change is only worth making if the ceiling is.
    #[test]
    #[ignore = "a measurement, run explicitly"]
    fn how_much_would_stopping_at_the_first_match_save() {
        let links = (0..300)
            .map(|n| format!("<a href='/l{n}'>link {n}</a>"))
            .collect::<String>();
        let sections = (0..120)
            .map(|n| {
                format!(
                    "<section><h2>S{n}</h2><p>text <a href='/link/{n}'>a link</a></p>\
                     <ul><li>one</li><li>two</li></ul></section>"
                )
            })
            .collect::<String>();
        let forms = (0..150)
            .map(|n| format!("<label for='f{n}'>Field {n}</label><input id='f{n}' name='f{n}'>"))
            .collect::<String>();

        for (name, body) in [
            ("links300", links),
            ("sections120", sections),
            ("forms150", forms),
        ] {
            let p = page(&format!("<html><body>{body}</body></html>"));
            let snapshot = p.snapshot();
            let dom = p.dom();
            let doc = dom.borrow();

            // The selector each ref actually ends up with. That is the query
            // the pass is dominated by once a link resolves on its href.
            let mut cache = Cache::new();
            let selectors: Vec<String> = snapshot
                .refs
                .iter()
                .filter_map(|entry| for_node_cached(&doc, entry.node_id, &mut cache))
                .collect();

            // Both modes must agree about the first match, or the faster one is
            // answering a different question.
            for selector in &selectors {
                assert_eq!(
                    first(&doc, selector),
                    matches(&doc, selector).first().copied(),
                    "{selector} disagreed between query modes"
                );
            }

            let all = median(|| {
                for selector in &selectors {
                    std::hint::black_box(matches(&doc, selector));
                }
            });
            let firsts = median(|| {
                for selector in &selectors {
                    std::hint::black_box(first(&doc, selector));
                }
            });
            // Where in the document the targets sit, which is what decides the
            // saving: a first match at the end costs a full walk either way.
            let depth: Vec<usize> = selectors
                .iter()
                .filter_map(|s| {
                    let hits = matches(&doc, s);
                    hits.first().map(|_| hits.len())
                })
                .collect();
            let single = depth.iter().filter(|n| **n == 1).count();

            println!(
                "{name:<12} refs {:>4}  QueryAll {all:>7.3} ms  QueryFirst {firsts:>7.3} ms  \
                 ratio {:>5.2}x  ({single}/{} selectors match exactly one element)",
                selectors.len(),
                all / firsts.max(f64::MIN_POSITIVE),
                selectors.len(),
            );
        }
    }
}
