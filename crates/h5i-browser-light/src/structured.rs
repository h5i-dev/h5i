//! What a page says about itself, in the formats it already publishes.
//!
//! An agent asked "what is this page about" has three ways to answer. It can
//! read the outline, which is the page's *content* and costs a few hundred
//! lines. It can read the markdown, which is denser and still prose. Or it can
//! read the metadata the page publishes for exactly this purpose — JSON-LD,
//! OpenGraph, Twitter cards, `<meta>` — which is a few hundred *bytes* and is
//! already structured.
//!
//! The third is nearly free over a DOM we already have, and it is the only one
//! of the three where the page has written the answer down rather than left it
//! to be inferred. A model asked to extract a headline from prose will
//! occasionally invent one; a model handed `"headline": "…"` will not.
//!
//! # The fence still applies
//!
//! Every value here is page-derived and reaches a model that is deciding what
//! to do next, so it is fenced like the outline and collapsed like every other
//! page-derived value: no value may span a line, and a page cannot write the
//! closing marker into its own `og:title`.
//!
//! # What this does not do
//!
//! It does not *validate*. A page claiming `"@type": "Product"` with no price
//! is reported as it stands, because this is a reading of what the page said
//! and not a judgement about whether the page is right. Nor does it merge the
//! formats into one shape: `json_ld`, `open_graph` and `meta` stay apart,
//! because a page that disagrees with itself between two of them is telling an
//! agent something, and folding them together would hide it.

use blitz_dom::BaseDocument;
use serde::{Deserialize, Serialize};

use crate::snapshot::collapse;

/// How many entries of any one kind to report.
///
/// A page with four hundred `<meta>` tags is a page trying to fill a context
/// window. Bounded, and the truncation is reported rather than silent.
const MAX_ENTRIES: usize = 64;

/// The longest single value worth carrying.
///
/// A JSON-LD blob can hold an entire article body. That is content, and there
/// are two verbs for content already.
const MAX_VALUE_BYTES: usize = 4096;

/// What a page publishes about itself.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Structured {
    /// `<script type="application/ld+json">` blocks, parsed.
    ///
    /// Kept as JSON rather than flattened: JSON-LD is a graph, and flattening
    /// one loses the relationships that are the reason it is a graph.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub json_ld: Vec<serde_json::Value>,
    /// `og:*` properties.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_graph: Vec<(String, String)>,
    /// `twitter:*` properties.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub twitter: Vec<(String, String)>,
    /// Everything else with a `name` and a `content`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub meta: Vec<(String, String)>,
    /// `<link rel=...>`, which is where canonical URLs and feeds live.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<(String, String)>,
    /// The document title, which is metadata even though it is not a `<meta>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Named when any list hit [`MAX_ENTRIES`], so a short answer is visibly
    /// short rather than quietly incomplete.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl Structured {
    /// True when the page published nothing at all.
    ///
    /// A result, and worth distinguishing from an error: plenty of pages have
    /// no metadata, and telling an agent "this page publishes none" is a
    /// different answer from "the read failed".
    pub fn is_empty(&self) -> bool {
        self.json_ld.is_empty()
            && self.open_graph.is_empty()
            && self.twitter.is_empty()
            && self.meta.is_empty()
            && self.links.is_empty()
            && self.title.is_none()
    }
}

/// Read a document's own account of itself.
///
/// `base` resolves `<link href>`, for the same reason [`crate::extract`] takes
/// one: a relative URL is useless to a caller that is not going to resolve it
/// against the same base, and passing it in keeps this readable from a document
/// that does not carry its own.
pub fn capture(doc: &BaseDocument, base: &url::Url) -> Structured {
    let mut out = Structured::default();
    let mut truncated: Vec<&str> = Vec::new();

    for (node_id, node) in doc.tree().iter() {
        let Some(element) = node.element_data() else {
            continue;
        };
        let tag = element.name.local.as_ref();

        match tag {
            "title" if out.title.is_none() => {
                let text = collapse(&node.text_content());
                if !text.is_empty() {
                    out.title = Some(cap(text));
                }
            }
            "script" => {
                let kind = attr(doc, node_id, "type").unwrap_or_default();
                if !kind.trim().eq_ignore_ascii_case("application/ld+json") {
                    continue;
                }
                if out.json_ld.len() >= MAX_ENTRIES {
                    truncated.push("json_ld");
                    continue;
                }
                // Unparseable JSON-LD is *skipped*, not reported as an empty
                // object: a malformed block is the page's mistake, and
                // inventing a shape for it would put a fiction where a caller
                // expects the page's own words.
                if let Ok(parsed) =
                    serde_json::from_str::<serde_json::Value>(&node.text_content())
                {
                    out.json_ld.push(fence_json(parsed));
                }
            }
            "meta" => {
                let content = match attr(doc, node_id, "content") {
                    Some(value) => cap(collapse(&value)),
                    None => continue,
                };
                if content.is_empty() {
                    continue;
                }
                // `property` is OpenGraph's spelling and `name` is everything
                // else's; Twitter uses both in the wild, which is why the
                // prefix decides the bucket rather than the attribute.
                let key = attr(doc, node_id, "property")
                    .or_else(|| attr(doc, node_id, "name"))
                    .map(|k| collapse(&k).to_ascii_lowercase());
                let Some(key) = key.filter(|k| !k.is_empty()) else {
                    continue;
                };

                let bucket = if key.starts_with("og:") {
                    (&mut out.open_graph, "open_graph")
                } else if key.starts_with("twitter:") {
                    (&mut out.twitter, "twitter")
                } else {
                    (&mut out.meta, "meta")
                };
                if bucket.0.len() >= MAX_ENTRIES {
                    truncated.push(bucket.1);
                    continue;
                }
                bucket.0.push((key, content));
            }
            "link" => {
                let Some(rel) = attr(doc, node_id, "rel") else {
                    continue;
                };
                let rel = collapse(&rel).to_ascii_lowercase();
                // Stylesheets and icons are how the page is *rendered*, not
                // what it is about, and there are a great many of them.
                if rel.is_empty() || matches!(rel.as_str(), "stylesheet" | "icon" | "preload") {
                    continue;
                }
                let Some(href) = attr(doc, node_id, "href") else {
                    continue;
                };
                if out.links.len() >= MAX_ENTRIES {
                    truncated.push("links");
                    continue;
                }
                // Absolute, like `extract` resolves `href`: a relative URL is
                // useless to a caller that is not going to resolve it against
                // the same base.
                let resolved = base
                    .join(&href)
                    .map(|url| url.to_string())
                    .unwrap_or_else(|_| href.to_string());
                out.links.push((rel, cap(collapse(&resolved))));
            }
            _ => {}
        }
    }

    truncated.sort_unstable();
    truncated.dedup();
    for kind in truncated {
        out.notes.push(format!(
            "this page has more than {MAX_ENTRIES} `{kind}` entries; the rest are not listed"
        ));
    }
    out
}

fn attr(doc: &BaseDocument, node_id: usize, name: &str) -> Option<String> {
    doc.get_node(node_id)?
        .attrs()?
        .iter()
        .find(|a| a.name.local.as_ref() == name)
        .map(|a| a.value.to_string())
}

/// Trim one value to something a context window can hold.
fn cap(mut value: String) -> String {
    if value.len() <= MAX_VALUE_BYTES {
        return value;
    }
    // On a character boundary, or the string is no longer valid UTF-8.
    let mut at = MAX_VALUE_BYTES;
    while at > 0 && !value.is_char_boundary(at) {
        at -= 1;
    }
    value.truncate(at);
    value.push_str(" …[truncated]");
    value
}

/// Collapse every string inside a JSON value.
///
/// The fence in [`crate::snapshot::Snapshot::render`] rests on no page-derived
/// value spanning a line. JSON-LD is page-derived and arbitrarily nested, so
/// the collapse has to reach every leaf — a forged fence marker inside
/// `{"description": "…"}` is exactly as effective as one in a heading.
fn fence_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(text) => serde_json::Value::String(cap(collapse(&text))),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(fence_json).collect())
        }
        serde_json::Value::Object(fields) => serde_json::Value::Object(
            fields
                .into_iter()
                .map(|(key, value)| (collapse(&key), fence_json(value)))
                .collect(),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{PageFactory, PageOptions};
    use crate::net::Broker;
    use crate::policy::Policy;
    use crate::receipt::MemorySink;
    use std::sync::Arc;

    fn structured_of(html: &str) -> Structured {
        let broker = Arc::new(
            Broker::new(Policy::new(), Arc::new(MemorySink::new()), None).expect("broker"),
        );
        let fonts = crate::fonts::load(&[], &crate::fonts::default_font_dirs(), Some(2));
        let factory = PageFactory::new(broker, fonts.sources.clone(), PageOptions::default());
        let page = factory.from_html(
            html,
            &url::Url::parse("https://site.example/article").unwrap(),
        );
        let dom = page.dom();
        let doc = dom.borrow();
        capture(&doc, &url::Url::parse("https://site.example/article").unwrap())
    }

    #[test]
    fn a_page_that_publishes_metadata_is_read_in_a_few_hundred_bytes() {
        let found = structured_of(
            r#"<html><head>
                 <title>How widgets work</title>
                 <meta property="og:title" content="How widgets work">
                 <meta property="og:type" content="article">
                 <meta name="twitter:card" content="summary">
                 <meta name="description" content="A guide.">
                 <link rel="canonical" href="/article/widgets">
                 <script type="application/ld+json">
                   {"@type": "Article", "headline": "How widgets work"}
                 </script>
               </head><body><p>words</p></body></html>"#,
        );

        assert_eq!(found.title.as_deref(), Some("How widgets work"));
        assert!(found
            .open_graph
            .contains(&("og:type".to_string(), "article".to_string())));
        assert!(found
            .twitter
            .contains(&("twitter:card".to_string(), "summary".to_string())));
        assert!(found
            .meta
            .contains(&("description".to_string(), "A guide.".to_string())));
        // Absolute, like `extract` resolves an href.
        assert!(found
            .links
            .contains(&(
                "canonical".to_string(),
                "https://site.example/article/widgets".to_string()
            )));
        assert_eq!(found.json_ld.len(), 1);
        assert_eq!(found.json_ld[0]["headline"], "How widgets work");
    }

    /// A page with no metadata is a result, not a failure. Plenty of pages
    /// publish none, and "this page says nothing about itself" is an answer.
    #[test]
    fn a_page_with_no_metadata_is_empty_rather_than_an_error() {
        let found = structured_of("<html><body><p>just words</p></body></html>");
        assert!(found.is_empty(), "{found:?}");
    }

    /// The fence rests on no page-derived value spanning a line, and JSON-LD is
    /// page-derived and arbitrarily nested. A marker written into a nested
    /// string is exactly as effective as one in a heading unless the collapse
    /// reaches every leaf.
    #[test]
    fn a_fence_marker_inside_nested_json_ld_cannot_span_a_line() {
        let found = structured_of(
            "<html><head><script type=\"application/ld+json\">\
               {\"a\": {\"b\": [\"line one\\nline two\"]}}\
             </script></head><body></body></html>",
        );
        let leaf = found.json_ld[0]["a"]["b"][0].as_str().unwrap();
        assert!(!leaf.contains('\n'), "a nested value spanned a line: {leaf:?}");
        assert_eq!(leaf, "line one line two");
    }

    /// Malformed JSON-LD is skipped rather than reported as an empty object:
    /// inventing a shape for it would put a fiction where a caller expects the
    /// page's own words.
    #[test]
    fn unparseable_json_ld_is_skipped_rather_than_guessed() {
        let found = structured_of(
            "<html><head><script type=\"application/ld+json\">{not json</script>\
             </head><body></body></html>",
        );
        assert!(found.json_ld.is_empty());
    }

    /// Stylesheets and icons are how a page is rendered, not what it is about,
    /// and a page has a great many of them.
    #[test]
    fn rendering_links_are_not_metadata() {
        let found = structured_of(
            "<html><head>\
               <link rel=\"stylesheet\" href=\"/a.css\">\
               <link rel=\"icon\" href=\"/i.png\">\
               <link rel=\"alternate\" href=\"/feed.xml\">\
             </head><body></body></html>",
        );
        assert_eq!(found.links.len(), 1, "{:?}", found.links);
        assert_eq!(found.links[0].0, "alternate");
    }

    /// A page with hundreds of tags is a page trying to fill a context window.
    /// Bounded, and the truncation named rather than silent.
    #[test]
    fn an_overlong_list_is_capped_and_says_so() {
        let mut html = String::from("<html><head>");
        for at in 0..(MAX_ENTRIES + 20) {
            html.push_str(&format!("<meta name=\"k{at}\" content=\"v{at}\">"));
        }
        html.push_str("</head><body></body></html>");

        let found = structured_of(&html);
        assert_eq!(found.meta.len(), MAX_ENTRIES);
        assert!(
            found.notes.iter().any(|note| note.contains("meta")),
            "a capped list must say so: {:?}",
            found.notes
        );
    }
}
