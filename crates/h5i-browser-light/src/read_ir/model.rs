//! The nodes, ids and roles the Read IR is made of.
//!
//! `docs/design-h5i-ir.md`, "Data model". The shape is Chromium's accessibility
//! abstraction as distilled by AccessKit: stable integer ids, a frozen node
//! carrying a role and a flag word, and text held out of line. What is not
//! taken is AccessKit's 182 roles and 88 property kinds; this engine's reading
//! vocabulary is two dozen entries wide and closed, so a purpose-built enum is
//! smaller than the general schema and cannot drift from the strings the
//! outline actually prints.

/// A node's place in the arena.
///
/// A plain index for now. The design's generation tag belongs with the
/// retained arena in phase 2, where a slot can be freed and refilled; while
/// every tree is built and dropped whole, there is no freed slot to catch a
/// reference into, and a generation would be a field nothing could read.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
#[repr(transparent)]
pub struct ReadId(pub u32);

impl ReadId {
    /// The synthetic document node every tree starts with.
    pub const ROOT: ReadId = ReadId(0);
}

/// A span of page text in the arena. `TextId::EMPTY` is the empty string.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(transparent)]
pub struct TextId(pub u32);

impl TextId {
    pub const EMPTY: TextId = TextId(0);

    pub fn is_empty(self) -> bool {
        self == TextId::EMPTY
    }
}

/// What a node is, for a reader deciding what to do with it.
///
/// `#[repr(u16)]` and no payload, so a node stays copyable and small. The
/// heading level rides on [`ReadNode::level`] rather than splitting this into
/// six variants, which is where the design puts it: one role per *kind* of
/// thing, refinements beside it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u16)]
pub enum ReadRole {
    /// The synthetic root. Never rendered.
    Document,
    Text,
    Heading,
    Paragraph,
    ListItem,
    Cell,
    Label,
    Code,
    Quote,
    Link,
    Button,
    Combobox,
    Textbox,
    Image,
    Checkbox,
    Radio,
}

impl ReadRole {
    /// The exact word the outline prints for this role.
    ///
    /// The single source of the role vocabulary: the snapshot walker renders
    /// through here too, so the outline and the IR cannot drift into printing
    /// two different names for one kind of thing.
    pub fn as_str(self, level: u8) -> &'static str {
        match self {
            ReadRole::Document => "document",
            ReadRole::Text => "text",
            ReadRole::Heading => match level {
                1 => "heading1",
                2 => "heading2",
                3 => "heading3",
                4 => "heading4",
                5 => "heading5",
                _ => "heading6",
            },
            ReadRole::Paragraph => "paragraph",
            ReadRole::ListItem => "listitem",
            ReadRole::Cell => "cell",
            ReadRole::Label => "label",
            ReadRole::Code => "code",
            ReadRole::Quote => "quote",
            ReadRole::Link => "link",
            ReadRole::Button => "button",
            ReadRole::Combobox => "combobox",
            ReadRole::Textbox => "textbox",
            ReadRole::Image => "image",
            ReadRole::Checkbox => "checkbox",
            ReadRole::Radio => "radio",
        }
    }

    /// Roles that structure a page rather than sit inside a sentence.
    ///
    /// One question only: whether a semantic leaf is really a leaf, or a
    /// wrapper that has swallowed a block of structure below it.
    pub fn is_block(self) -> bool {
        matches!(
            self,
            ReadRole::Heading
                | ReadRole::Paragraph
                | ReadRole::ListItem
                | ReadRole::Cell
                | ReadRole::Quote
                | ReadRole::Code
        )
    }
}

/// Boolean facts about a node, packed.
///
/// A bit word rather than a row of `bool` fields, so the set can grow through
/// the later phases without the node growing with it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(transparent)]
pub struct ReadFlags(pub u16);

impl ReadFlags {
    /// An agent can act on this node, so it carries a ref.
    pub const ACTIONABLE: u16 = 1 << 0;
    /// This node came from a grafted frame document, where Blitz resolves no
    /// styles and visibility is judged by markup instead.
    pub const IN_FRAME: u16 = 1 << 1;
    /// This node's text was stored verbatim rather than collapsed.
    ///
    /// True for the lines of a code block and nothing else. Their leading
    /// indentation is meaning, so the builder keeps it, which means the
    /// renderer has to do the normalising the arena did not: every other line
    /// arrives collapsed and can go straight out.
    pub const VERBATIM: u16 = 1 << 2;

    pub fn contains(self, bit: u16) -> bool {
        self.0 & bit != 0
    }

    pub fn set(&mut self, bit: u16) {
        self.0 |= bit;
    }
}

/// One line of the reading.
///
/// Deliberately flat and `Copy`: no `String`, no `Vec`, no per-node
/// allocation of any kind. Text lives in the arena, the role is an enum, and
/// the tree is expressed by `depth` over the preorder the builder emits in.
///
/// The design budgets 48 bytes to leave room for phase 3's `local_revision`
/// and `subtree_fingerprint`; what phase 1 reads is well inside that, and the
/// assertion below is what keeps a future field honest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct ReadNode {
    /// The Blitz node this came from, for action dispatch and selectors.
    pub dom_id: u32,
    pub parent: ReadId,
    pub name: TextId,
    /// Resolved `href` or `src`, collapsed. `EMPTY` when the node has neither.
    ///
    /// Inline rather than in a side table, unlike the design's sketch: a
    /// target is printed on the same line as the name, every link has one, and
    /// a side allocation for the commonest actionable node on the web would
    /// cost more than the four bytes it saves.
    pub href: TextId,
    /// 1-based position in the ref list, or 0 for a node that takes no ref.
    pub ref_ordinal: u32,
    pub role: ReadRole,
    pub flags: ReadFlags,
    /// Indentation depth, which is the count of *emitted* ancestors rather
    /// than of DOM ancestors: containers the reading flattens do not indent it.
    pub depth: u8,
    /// Heading level. Meaningless for every other role.
    pub level: u8,
}

/// The node is the thing there are tens of thousands of. If it ever grows past
/// the design's budget, that is a decision someone makes with a benchmark in
/// hand, not something that happens by accident in a patch that adds a field.
const _: () = assert!(std::mem::size_of::<ReadNode>() <= 48);

/// What an agent can name in a later command, before it is spelled out.
///
/// Self-contained rather than a pointer into the node arena, and that is a
/// faithfulness requirement rather than a convenience: the walker mints a ref
/// and *then* tries to write its line, so at the budget's edge a ref can
/// outlive the line it was minted for. Pointing at a node that was never
/// written would lose the ref; carrying its own answer does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefRecord {
    /// The Blitz node this ref resolves to.
    pub dom_id: u32,
    pub role: ReadRole,
    pub level: u8,
    pub name: TextId,
    pub href: TextId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_node_stays_small() {
        // Stated as a number as well as an assertion, so a change shows up as a
        // number moving rather than as a constant being edited.
        assert!(
            std::mem::size_of::<ReadNode>() <= 48,
            "ReadNode is {} bytes",
            std::mem::size_of::<ReadNode>()
        );
    }

    #[test]
    fn every_role_prints_the_word_the_outline_uses() {
        assert_eq!(ReadRole::Heading.as_str(1), "heading1");
        assert_eq!(ReadRole::Heading.as_str(6), "heading6");
        // Out of range clamps to the deepest, which is what the tag table can
        // never produce and an explicit ARIA role should not be able to either.
        assert_eq!(ReadRole::Heading.as_str(9), "heading6");
        assert_eq!(ReadRole::Textbox.as_str(0), "textbox");
        assert_eq!(ReadRole::Text.as_str(0), "text");
    }

    #[test]
    fn block_roles_are_the_ones_that_stop_a_wrapper_speaking() {
        for role in [
            ReadRole::Heading,
            ReadRole::Paragraph,
            ReadRole::ListItem,
            ReadRole::Cell,
            ReadRole::Quote,
            ReadRole::Code,
        ] {
            assert!(role.is_block(), "{role:?} structures a page");
        }
        // A label names something inline; a link speaks for itself.
        assert!(!ReadRole::Label.is_block());
        assert!(!ReadRole::Link.is_block());
        assert!(!ReadRole::Text.is_block());
    }

    #[test]
    fn flags_are_independent_bits() {
        let mut flags = ReadFlags::default();
        assert!(!flags.contains(ReadFlags::ACTIONABLE));
        flags.set(ReadFlags::ACTIONABLE);
        assert!(flags.contains(ReadFlags::ACTIONABLE));
        assert!(!flags.contains(ReadFlags::IN_FRAME));
        flags.set(ReadFlags::IN_FRAME);
        assert!(flags.contains(ReadFlags::ACTIONABLE) && flags.contains(ReadFlags::IN_FRAME));
    }
}
