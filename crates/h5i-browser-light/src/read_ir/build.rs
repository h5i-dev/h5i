//! DOM and computed style in, Read IR out.
//!
//! Deliberately a transcription of [`crate::snapshot`]'s walker rather than a
//! fresh reading of the page. Every judgement about what a page says, which
//! elements are hidden, what a control is called, when a wrapper stops
//! speaking, is the same function the outline has always called. What changes
//! is only what the walk *writes*: an arena of flat nodes instead of a vector
//! of owned strings.
//!
//! That is the whole safety argument for phase 1. The output is asserted
//! byte-identical against the walker over the corpus (see `equivalence.rs`), so
//! a difference is a bug in this file rather than a new opinion about the web.

use blitz_dom::node::{Node, NodeData};
use blitz_dom::{local_name, BaseDocument};

use crate::snapshot::{
    accessible_name, attr_of, collapse_keeping_indent, describe, direct_text, find_title,
    hidden_from_assistive_tech, hoists_a_block, MAX_DEPTH,
};

use super::model::{ReadFlags, ReadId, ReadNode, ReadRole, RefRecord, TextId};
use super::text_arena::TextArena;

/// One reading of a page, as an arena.
#[derive(Debug)]
pub struct ReadTree {
    pub(crate) nodes: Vec<ReadNode>,
    pub(crate) text: TextArena,
    pub(crate) refs: Vec<RefRecord>,
    pub(crate) url: String,
    pub(crate) title: TextId,
    pub(crate) truncated: bool,
    /// Facts about this engine's reading, not about the page. Rendered outside
    /// the fence; a page cannot write here.
    pub(crate) notes: Vec<String>,
}

impl ReadTree {
    /// Read a resolved document.
    ///
    /// `scripted` decides what `<noscript>` means for this reading, exactly as
    /// it does for [`crate::snapshot::Snapshot::capture`]: a page whose script
    /// ran is not one that should be handed the message written for a reader
    /// who did not run it.
    pub fn capture(doc: &BaseDocument, url: &str, max_lines: usize, scripted: bool) -> Self {
        // A floor, not the budget. Reserving the line budget up front made a
        // forty-line page carry a five-hundred-line arena, which measured as
        // the IR using more memory than the walker it replaces on every small
        // page. Growth from here is a handful of doublings on a large page,
        // against thousands of per-node allocations either way, so the count
        // that drives latency does not notice and the waste goes away.
        const FLOOR_NODES: usize = 64;
        const FLOOR_TEXT: usize = 2 * 1024;
        let reserve = max_lines.min(FLOOR_NODES);
        let mut builder = Builder {
            doc,
            max_lines,
            scripted,
            next_ref: 1,
            tree: ReadTree {
                nodes: Vec::with_capacity(reserve + 1),
                text: TextArena::with_capacity(FLOOR_TEXT.min(max_lines * 32 + 64), reserve),
                refs: Vec::new(),
                url: url.to_string(),
                title: TextId::EMPTY,
                truncated: false,
                notes: Vec::new(),
            },
        };

        // The synthetic root. Never rendered; it exists so every real node has
        // a parent and the arena has an unambiguous index zero.
        builder.tree.nodes.push(ReadNode {
            dom_id: u32::MAX,
            parent: ReadId::ROOT,
            name: TextId::EMPTY,
            href: TextId::EMPTY,
            ref_ordinal: 0,
            role: ReadRole::Document,
            flags: ReadFlags::default(),
            depth: 0,
            level: 0,
        });

        let title = find_title(doc).unwrap_or_default();
        builder.tree.title = builder.tree.text.intern(&title);

        // Blitz keys nodes by slab index, and this arena addresses them with a
        // `u32`. A document that outgrew that cannot be read here, and the one
        // thing that must not happen is reading it anyway with the high bits
        // dropped: every ref would resolve, and some would resolve to the wrong
        // element. Refused instead, out loud, the way every other budget in
        // this engine refuses. Four billion nodes is far past anything the line
        // budget or the parser's own nesting limit would let through, so this
        // is a guard rather than a case.
        if doc.tree().capacity() > u32::MAX as usize {
            builder.tree.truncated = true;
            builder.tree.notes.push(
                "this document has more nodes than this engine can address, so none of it was \
                 read. Nothing below describes the page."
                    .to_string(),
            );
            return builder.tree;
        }

        let root_id = doc.root_element().id;
        builder.walk(root_id, ReadId::ROOT, 0, false, false);

        builder.tree
    }

    /// How many lines this reading would print.
    pub fn line_count(&self) -> usize {
        self.nodes.len() - 1
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// The page's own title, already collapsed.
    pub fn title(&self) -> &str {
        self.text.resolve(self.title)
    }

    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    pub fn push_note(&mut self, note: String) {
        self.notes.push(note);
    }

    /// The lines, in the order they print.
    pub fn nodes(&self) -> &[ReadNode] {
        &self.nodes[1..]
    }

    pub fn refs(&self) -> &[RefRecord] {
        &self.refs
    }

    /// Resolve a text span.
    pub fn text(&self, id: TextId) -> &str {
        self.text.resolve(id)
    }

    /// Bytes of page text held, for budget accounting.
    pub fn text_bytes(&self) -> usize {
        self.text.bytes()
    }

    /// The page's words and nothing else, one line each.
    ///
    /// What `--text` asks for. Blank lines are dropped rather than printed,
    /// because a reading is not a layout: a node with no words of its own
    /// contributed structure, and structure is what the outline is for.
    ///
    /// The caller fences this. It is page content like any other.
    pub fn plain_text(&self) -> String {
        let mut out = String::with_capacity(self.text_bytes() + self.line_count());
        for node in self.nodes() {
            if node.name.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(self.text(node.name));
        }
        out
    }
}

struct Builder<'a> {
    doc: &'a BaseDocument,
    tree: ReadTree,
    max_lines: usize,
    scripted: bool,
    /// The `N` in the next `eN`. Minted before the line is written, which
    /// matters at the budget's edge: the walker records a ref and *then* tries
    /// to print it, so one ref past the cut is a ref that exists with no line.
    /// Faithfully reproduced, because the ref list is output too.
    next_ref: u32,
}

/// A text node's own content, borrowed rather than rebuilt.
///
/// [`Node::text_content`] concatenates a whole subtree into a fresh `String`,
/// which for a leaf text node is an allocation to copy a string that is already
/// sitting there.
fn text_of(node: &Node) -> &str {
    match &node.data {
        NodeData::Text(data) => &data.content,
        _ => "",
    }
}

impl Builder<'_> {
    fn lines(&self) -> usize {
        self.tree.nodes.len() - 1
    }

    /// Write one line, unless the budget is spent.
    #[allow(clippy::too_many_arguments)]
    fn emit(
        &mut self,
        parent: ReadId,
        depth: usize,
        role: ReadRole,
        level: u8,
        name: TextId,
        href: TextId,
        ref_ordinal: u32,
        in_frame: bool,
        verbatim: bool,
        dom_id: usize,
    ) -> Option<ReadId> {
        if self.lines() >= self.max_lines {
            self.tree.truncated = true;
            return None;
        }
        let mut flags = ReadFlags::default();
        if ref_ordinal != 0 {
            flags.set(ReadFlags::ACTIONABLE);
        }
        if in_frame {
            flags.set(ReadFlags::IN_FRAME);
        }
        if verbatim {
            flags.set(ReadFlags::VERBATIM);
        }
        let id = ReadId(self.tree.nodes.len() as u32);
        self.tree.nodes.push(ReadNode {
            dom_id: dom_id as u32,
            parent,
            name,
            href,
            ref_ordinal,
            role,
            flags,
            depth: depth.min(MAX_DEPTH) as u8,
            level,
        });
        Some(id)
    }

    /// Walk one node.
    ///
    /// `in_prose` is set once we are inside a semantic leaf whose own text has
    /// already been emitted. Under it, prose is suppressed but actionable
    /// descendants still speak: a link inside a paragraph is precisely what an
    /// agent came for.
    fn walk(&mut self, node_id: usize, parent: ReadId, depth: usize, in_prose: bool, in_frame: bool) {
        if depth > MAX_DEPTH {
            return;
        }
        if self.lines() >= self.max_lines {
            self.tree.truncated = true;
            return;
        }

        // Hoisted out of `self` on purpose. The walker holds `&'a BaseDocument`,
        // so a node borrowed through this local lives for `'a` and not for the
        // `&mut self` the recursion needs; without it every recursion site has
        // to clone the child list to end the borrow, which is one heap
        // allocation per element in the document.
        let doc = self.doc;
        let Some(node) = doc.get_node(node_id) else {
            return;
        };

        if node.is_text_node() {
            if in_prose {
                return;
            }
            let text = self.tree.text.collapse_into(text_of(node));
            if !text.is_empty() {
                self.emit(
                    parent,
                    depth,
                    ReadRole::Text,
                    0,
                    text,
                    TextId::EMPTY,
                    0,
                    in_frame,
                    false,
                    node_id,
                );
            }
            return;
        }

        let Some(element) = node.element_data() else {
            // Not an element and not text: a comment or the document node.
            // Nothing to say about it, but its children may matter.
            for &child in &node.children {
                self.walk(child, parent, depth, in_prose, in_frame);
            }
            return;
        };

        let tag: &str = element.name.local.as_ref();
        // Once the walk crosses into a frame it stays "in frame" for the whole
        // subtree: the styling gap is a property of the subtree, not the node.
        let in_frame = in_frame || tag == "iframe" || tag == "frame";

        // Never in the outline, and never recursed into: their text content is
        // code, not page content.
        if matches!(
            tag,
            "script" | "style" | "head" | "title" | "meta" | "link"
        ) {
            return;
        }

        // `<noscript>` is addressed to a reader who did not run the script. If
        // this reading did, it is a message for somebody else.
        if tag == "noscript" && self.scripted {
            return;
        }

        // A closed `<details>` shows its `<summary>` and nothing else. Blitz
        // does not apply the UA rule that hides the body, so it is applied here.
        if tag == "details" && attr_of(node, "open").is_none() {
            for &child in &node.children {
                let is_summary = doc
                    .get_node(child)
                    .map(|kid| kid.data.is_element_with_tag_name(&local_name!("summary")))
                    .unwrap_or(false);
                if is_summary {
                    self.walk(child, parent, depth, in_prose, in_frame);
                }
            }
            return;
        }

        // Content the page does not display is not content this reading carries.
        let displayed = match node.primary_styles() {
            // Blitz resolves no primary styles for a node it will not render, and
            // a grafted frame subtree is exactly that: inside one, "no styles"
            // means "outside the styled tree", not "hidden by the page".
            None => in_frame,
            Some(styles) => !styles.clone_display().is_none(),
        };
        if !displayed {
            return;
        }
        if in_frame {
            if attr_of(node, "hidden").is_some() {
                return;
            }
            if let Some(inline) = attr_of(node, "style")
                && inline
                    .replace(' ', "")
                    .to_ascii_lowercase()
                    .contains("display:none")
            {
                return;
            }
        }

        // `aria-hidden="true"` hides a subtree from anything reading the page,
        // and this reading is one of those things. Content a screen reader is
        // told to ignore is one of the places instructions aimed at whatever is
        // reading the page get put.
        if hidden_from_assistive_tech(node) {
            return;
        }

        let Some(descriptor) = describe(tag, node) else {
            // An unremarkable container (div, span, section). No line of its
            // own, and its children keep this depth, so the reading tracks
            // meaning rather than markup nesting.
            for &child in &node.children {
                self.walk(child, parent, depth, in_prose, in_frame);
            }
            return;
        };

        let role = descriptor.role;
        let level = descriptor.level;
        let takes_ref = descriptor.takes_ref;
        let is_leaf = descriptor.is_leaf;

        // A wrapper that has swallowed a block of structure is not a leaf,
        // whatever its tag says. Not for a ref-taking element, whose name is how
        // an agent tells one control from another, and not for `code`, whose
        // whole point is that its text is carried verbatim.
        let hoisting = is_leaf && !takes_ref && role != ReadRole::Code && hoists_a_block(doc, node);

        let name = if hoisting {
            direct_text(doc, node)
        } else {
            accessible_name(tag, node)
        };

        // Inside prose, only actionable elements earn a line; the surrounding
        // words were already emitted by the parent.
        let worth_a_line = takes_ref || (!name.is_empty() && !in_prose);

        let (child_parent, child_depth) = if worth_a_line {
            // Collapsed like every other page-derived value, and for a sharper
            // reason than tidiness: an attribute value may contain a literal
            // newline, and a field that can start a line can forge the end of
            // the untrusted-content fence.
            let href = match attr_of(node, "href").or_else(|| attr_of(node, "src")) {
                Some(raw) => self.tree.text.collapse_into(raw),
                None => TextId::EMPTY,
            };

            // For `<pre>`, the same text again but split on its own line
            // breaks. A single surviving line is deliberately *not* a code
            // block: the walker's `stop > start` leaves one-line `<pre>` to the
            // ordinary path, and a reading that differed here would differ from
            // every outline taken before it.
            let preformatted: Vec<String> = if tag == "pre" {
                let raw = node.text_content();
                let pieces: Vec<String> = raw
                    .lines()
                    // Leading indentation is meaning in code, so it is kept.
                    .map(collapse_keeping_indent)
                    .collect();
                let start = pieces.iter().position(|line| !line.trim().is_empty());
                let stop = pieces.iter().rposition(|line| !line.trim().is_empty());
                match (start, stop) {
                    (Some(start), Some(stop)) if stop > start => pieces[start..=stop].to_vec(),
                    _ => Vec::new(),
                }
            } else {
                Vec::new()
            };

            let multiline_code = role == ReadRole::Code && !preformatted.is_empty();

            // Interned once and spent twice, on the ref and on the line. The
            // two cannot disagree, which is the point: an agent addresses a
            // control by the name the outline printed for it.
            //
            // Skipped entirely for a code block, whose accessible name is its
            // whole text run flattened onto one line and is about to be thrown
            // away in favour of the real line breaks.
            let name_id = if multiline_code {
                TextId::EMPTY
            } else {
                self.tree.text.intern(&name)
            };

            let ref_ordinal = if takes_ref {
                let ordinal = self.next_ref;
                self.next_ref += 1;
                self.tree.refs.push(RefRecord {
                    dom_id: node_id as u32,
                    role,
                    level,
                    name: name_id,
                    href,
                });
                ordinal
            } else {
                0
            };

            let mut last = None;
            if multiline_code {
                for piece in &preformatted {
                    let text = self.tree.text.intern(piece);
                    last = self.emit(
                        parent, depth, role, level, text, href, ref_ordinal, in_frame, true,
                        node_id,
                    );
                }
            } else {
                last = self.emit(
                    parent, depth, role, level, name_id, href, ref_ordinal, in_frame, false,
                    node_id,
                );
            }
            (last.unwrap_or(parent), depth + 1)
        } else {
            (parent, depth)
        };

        if hoisting {
            // The direct text is already on this node's line, so the text nodes
            // it came from are skipped rather than emitted again. Everything
            // else recurses in full: this node never claimed to have said it.
            for &child in &node.children {
                let is_text = doc.get_node(child).map(|kid| kid.is_text_node()).unwrap_or(false);
                if !is_text {
                    self.walk(child, child_parent, child_depth, in_prose, in_frame);
                }
            }
        } else {
            // Always recurse. A leaf's own words are done, so its subtree
            // continues in prose mode, where only actionable things speak.
            for &child in &node.children {
                self.walk(child, child_parent, child_depth, in_prose || is_leaf, in_frame);
            }
        }
    }
}
