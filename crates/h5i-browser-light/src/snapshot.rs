//! The page as a model should read it.
//!
//! A screenshot is for the human; this is for the agent. The output is an
//! indented outline of semantic elements with stable refs on the ones worth
//! addressing, which is the shape that has won across the tooling: cheap in
//! tokens, deterministic between runs, and free of the decoration that a
//! pixel view forces a model to re-derive every step.
//!
//! Two rules keep it honest:
//!
//! - **Refs are assigned only to things an agent could act on** (links,
//!   controls, images). Numbering every `div` would make the refs unstable
//!   the moment a layout wrapper changes.
//! - **Semantic leaves are not recursed into.** A heading's text belongs on
//!   the heading's line, not scattered across three anonymous children, so
//!   the outline reads like the document rather than like the DOM.

use blitz_dom::node::Node;
use blitz_dom::{local_name, BaseDocument};
use serde::{Deserialize, Serialize};

/// How deep to walk before giving up. Documents nest far deeper than they
/// read; past this the outline is noise.
const MAX_DEPTH: usize = 24;

/// A ref an agent can name in a later command (`@e3`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RefEntry {
    /// The `e3` form, without the `@`.
    pub id: String,
    /// The Blitz node this ref resolves to. Tier 2 needs this to dispatch a
    /// click; Tier 1 exposes it so the mapping is inspectable rather than
    /// implicit.
    pub node_id: usize,
    pub role: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
}

/// One line of the outline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub depth: usize,
    pub role: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
}

/// The captured page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub url: String,
    pub title: String,
    pub lines: Vec<Line>,
    pub refs: Vec<RefEntry>,
    /// Set when the walk hit its line budget, so a caller never mistakes a
    /// truncated outline for a short page.
    pub truncated: bool,
}

struct Walker<'a> {
    doc: &'a BaseDocument,
    lines: Vec<Line>,
    refs: Vec<RefEntry>,
    next_ref: usize,
    max_lines: usize,
    truncated: bool,
}

impl Snapshot {
    /// Capture an outline of the resolved document.
    pub fn capture(doc: &BaseDocument, url: &str, max_lines: usize) -> Self {
        let mut walker = Walker {
            doc,
            lines: Vec::new(),
            refs: Vec::new(),
            next_ref: 1,
            max_lines,
            truncated: false,
        };

        let root_id = doc.root_element().id;
        walker.walk(root_id, 0, false);

        Snapshot {
            url: url.to_string(),
            title: find_title(doc).unwrap_or_default(),
            lines: walker.lines,
            refs: walker.refs,
            truncated: walker.truncated,
        }
    }

    /// The text form an agent reads.
    pub fn render(&self) -> String {
        let mut out = String::new();
        if !self.title.is_empty() {
            out.push_str(&format!("# {}\n", self.title));
        }
        if !self.url.is_empty() {
            out.push_str(&format!("url: {}\n", self.url));
        }
        if !out.is_empty() {
            out.push('\n');
        }

        for line in &self.lines {
            let indent = "  ".repeat(line.depth.min(MAX_DEPTH));
            out.push_str(&indent);
            out.push_str("- ");
            out.push_str(&line.role);
            if !line.text.is_empty() {
                out.push_str(&format!(" \"{}\"", line.text));
            }
            if let Some(reference) = &line.reference {
                out.push_str(&format!(" [ref={reference}]"));
            }
            if let Some(href) = &line.href {
                out.push_str(&format!(" -> {href}"));
            }
            out.push('\n');
        }

        if self.truncated {
            out.push_str("\n… snapshot truncated at the line budget\n");
        }
        out
    }

    /// Resolve `@e3` or `e3` to the node it names.
    pub fn resolve(&self, reference: &str) -> Option<&RefEntry> {
        let wanted = reference.trim_start_matches('@');
        self.refs.iter().find(|entry| entry.id == wanted)
    }
}

impl Walker<'_> {
    /// Walk one node.
    ///
    /// `in_prose` is set once we are inside a semantic leaf whose own text has
    /// already been emitted. Under it, prose is suppressed (the parent line
    /// already said it) but **actionable descendants are still emitted**: a
    /// link inside a paragraph and an input inside a label are precisely what
    /// an agent came for, and an outline that drops them is worse than no
    /// outline, because it looks complete.
    fn walk(&mut self, node_id: usize, depth: usize, in_prose: bool) {
        if depth > MAX_DEPTH {
            return;
        }
        if self.lines.len() >= self.max_lines {
            self.truncated = true;
            return;
        }

        let Some(node) = self.doc.get_node(node_id) else {
            return;
        };

        // Text that is not inside a semantic leaf still carries meaning
        // (loose copy in a `div`), so it is emitted rather than dropped.
        if node.is_text_node() {
            if in_prose {
                return;
            }
            let text = collapse(&node.text_content());
            if !text.is_empty() {
                self.push(Line {
                    depth,
                    role: "text".to_string(),
                    text,
                    reference: None,
                    href: None,
                });
            }
            return;
        }

        let Some(element) = node.element_data() else {
            // Not an element and not text: a comment or the document node.
            // Nothing to say about it, but its children may matter.
            for child in node.children.clone() {
                self.walk(child, depth, in_prose);
            }
            return;
        };

        let tag = element.name.local.as_ref().to_string();

        // Never in the outline, and never recursed into: their text content is
        // code, not page content, and emitting it is how a snapshot ends up
        // full of minified CSS.
        if matches!(
            tag.as_str(),
            "script" | "style" | "head" | "title" | "meta" | "link"
        ) {
            return;
        }

        let descriptor = describe(&tag, node);

        match descriptor {
            Some(Descriptor {
                role,
                takes_ref,
                is_leaf,
            }) => {
                let name = accessible_name(&tag, node);
                let href = attr_of(node, "href")
                    .or_else(|| attr_of(node, "src"))
                    .map(|value| value.to_string());

                // Inside prose, only actionable elements earn a line; the
                // surrounding words were already emitted by the parent.
                let worth_a_line = takes_ref || (!name.is_empty() && !in_prose);

                let child_depth = if worth_a_line {
                    let reference = if takes_ref {
                        let id = format!("e{}", self.next_ref);
                        self.next_ref += 1;
                        self.refs.push(RefEntry {
                            id: id.clone(),
                            node_id,
                            role: role.to_string(),
                            name: name.clone(),
                            href: href.clone(),
                        });
                        Some(id)
                    } else {
                        None
                    };

                    self.push(Line {
                        depth,
                        role: role.to_string(),
                        text: name,
                        reference,
                        href,
                    });
                    depth + 1
                } else {
                    depth
                };

                // Always recurse. A leaf's own words are done, so its subtree
                // continues in prose mode, where only actionable things speak.
                for child in node.children.clone() {
                    self.walk(child, child_depth, in_prose || is_leaf);
                }
            }
            None => {
                // An unremarkable container (div, span, section). It gets no
                // line of its own, and its children keep this depth so the
                // outline tracks meaning rather than markup nesting.
                for child in node.children.clone() {
                    self.walk(child, depth, in_prose);
                }
            }
        }
    }

    fn push(&mut self, line: Line) {
        if self.lines.len() >= self.max_lines {
            self.truncated = true;
            return;
        }
        self.lines.push(line);
    }
}

struct Descriptor {
    role: &'static str,
    takes_ref: bool,
    /// Leaves carry their own text and are not recursed into.
    is_leaf: bool,
}

/// Read the two attributes the role decision depends on, then decide.
///
/// The decision itself lives in [`role_for`], which takes plain values rather
/// than a `Node`, so every branch is testable without standing up a document.
fn describe(tag: &str, node: &Node) -> Option<Descriptor> {
    let input_type = attr_of(node, "type").map(str::to_ascii_lowercase);
    let has_href = attr_of(node, "href").is_some();
    role_for(tag, input_type.as_deref(), has_href)
}

fn role_for(tag: &str, input_type: Option<&str>, has_href: bool) -> Option<Descriptor> {
    let d = |role, takes_ref, is_leaf| {
        Some(Descriptor {
            role,
            takes_ref,
            is_leaf,
        })
    };

    match tag {
        "h1" => d("heading1", false, true),
        "h2" => d("heading2", false, true),
        "h3" => d("heading3", false, true),
        "h4" => d("heading4", false, true),
        "h5" => d("heading5", false, true),
        "h6" => d("heading6", false, true),
        "p" => d("paragraph", false, true),
        "li" => d("listitem", false, true),
        "td" | "th" => d("cell", false, true),
        "label" => d("label", false, true),
        "pre" | "code" => d("code", false, true),
        "blockquote" => d("quote", false, true),

        // Actionable: these are what a ref is for.
        "a" => {
            if has_href {
                d("link", true, true)
            } else {
                None
            }
        }
        "button" => d("button", true, true),
        "select" => d("combobox", true, false),
        "textarea" => d("textbox", true, true),
        "img" => d("image", true, true),
        "input" => match input_type.unwrap_or("text") {
            // A snapshot full of hidden CSRF fields spends the agent's budget
            // on controls it cannot use.
            "hidden" => None,
            "button" | "submit" | "reset" => d("button", true, true),
            "checkbox" => d("checkbox", true, true),
            "radio" => d("radio", true, true),
            _ => d("textbox", true, true),
        },

        _ => None,
    }
}

/// Look an attribute up by name.
///
/// Deliberately string-based rather than `local_name!`: that macro only
/// resolves atoms markup5ever interned ahead of time, and this needs
/// `aria-label` and `placeholder` as readily as `href`.
fn attr_of<'a>(node: &'a Node, name: &str) -> Option<&'a str> {
    node.attrs()?
        .iter()
        .find(|attr| attr.name.local.as_ref() == name)
        .map(|attr| attr.value.as_str())
}

/// What to call this element.
///
/// Text content is the usual answer, but the elements an agent most wants to
/// address are exactly the ones that have none: an image's name is its `alt`,
/// and an empty input's is its placeholder or label. Falling back to "" would
/// render a snapshot of anonymous `textbox` lines nobody can tell apart.
fn accessible_name(tag: &str, node: &Node) -> String {
    let from_attr = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| attr_of(node, name))
            .map(collapse)
            .filter(|value| !value.is_empty())
    };

    match tag {
        "img" => from_attr(&["alt", "title"]).unwrap_or_default(),
        "input" | "textarea" => {
            from_attr(&["aria-label", "placeholder", "value", "title", "name"]).unwrap_or_default()
        }
        _ => {
            let text = collapse(&node.text_content());
            if text.is_empty() {
                from_attr(&["aria-label", "title"]).unwrap_or_default()
            } else {
                text
            }
        }
    }
}

fn find_title(doc: &BaseDocument) -> Option<String> {
    doc.tree().iter().find_map(|(_, node)| {
        if node.data.is_element_with_tag_name(&local_name!("title")) {
            let text = collapse(&node.text_content());
            (!text.is_empty()).then_some(text)
        } else {
            None
        }
    })
}

/// Collapse runs of whitespace and trim, so an outline line is one line.
fn collapse(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_space = false;
    for ch in input.chars() {
        if ch.is_whitespace() {
            in_space = true;
            continue;
        }
        if in_space && !out.is_empty() {
            out.push(' ');
        }
        in_space = false;
        out.push(ch);
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_collapses_to_a_single_line() {
        assert_eq!(collapse("  hello\n\n   world \t"), "hello world");
        assert_eq!(collapse("\n\n  "), "");
    }

    #[test]
    fn hidden_inputs_get_no_role_and_therefore_no_ref() {
        assert!(role_for("input", Some("hidden"), false).is_none());
        // ...while the ones an agent can act on all take a ref.
        for kind in ["text", "email", "password", "submit", "checkbox", "radio"] {
            let descriptor = role_for("input", Some(kind), false)
                .unwrap_or_else(|| panic!("input type={kind} should have a role"));
            assert!(descriptor.takes_ref, "input type={kind} should take a ref");
        }
    }

    #[test]
    fn an_anchor_without_href_is_not_a_link() {
        // `<a name="x">` is a bookmark target, not something to click, and
        // giving it a ref invites an agent to try.
        assert!(role_for("a", None, false).is_none());
        assert_eq!(role_for("a", None, true).unwrap().role, "link");
    }

    #[test]
    fn plain_containers_get_no_line_of_their_own() {
        for tag in ["div", "span", "section", "main", "nav"] {
            assert!(role_for(tag, None, false).is_none(), "{tag} should be transparent");
        }
    }

    #[test]
    fn headings_and_paragraphs_are_leaves_that_carry_their_text() {
        let heading = role_for("h1", None, false).expect("h1 has a role");
        assert!(heading.is_leaf);
        assert!(!heading.takes_ref, "a heading is not actionable");
        assert_eq!(heading.role, "heading1");
    }

    #[test]
    fn refs_resolve_by_either_spelling() {
        let snapshot = Snapshot {
            url: "https://example.com/".to_string(),
            title: "T".to_string(),
            lines: Vec::new(),
            refs: vec![RefEntry {
                id: "e2".to_string(),
                node_id: 42,
                role: "button".to_string(),
                name: "Submit".to_string(),
                href: None,
            }],
            truncated: false,
        };

        assert_eq!(snapshot.resolve("e2").unwrap().node_id, 42);
        assert_eq!(snapshot.resolve("@e2").unwrap().node_id, 42);
        assert!(snapshot.resolve("e9").is_none());
    }

    #[test]
    fn render_marks_truncation_so_a_short_outline_is_not_mistaken_for_a_short_page() {
        let snapshot = Snapshot {
            url: String::new(),
            title: String::new(),
            lines: vec![Line {
                depth: 0,
                role: "paragraph".to_string(),
                text: "hi".to_string(),
                reference: None,
                href: None,
            }],
            refs: Vec::new(),
            truncated: true,
        };
        assert!(snapshot.render().contains("truncated"));
    }

    #[test]
    fn render_puts_refs_and_hrefs_where_an_agent_expects_them() {
        let snapshot = Snapshot {
            url: "https://example.com/".to_string(),
            title: "Docs".to_string(),
            lines: vec![Line {
                depth: 1,
                role: "link".to_string(),
                text: "Guide".to_string(),
                reference: Some("e1".to_string()),
                href: Some("https://example.com/guide".to_string()),
            }],
            refs: Vec::new(),
            truncated: false,
        };

        let rendered = snapshot.render();
        assert!(rendered.contains("# Docs"));
        assert!(rendered.contains("  - link \"Guide\" [ref=e1] -> https://example.com/guide"));
    }
}
