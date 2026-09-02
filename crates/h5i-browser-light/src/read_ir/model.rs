//! The nodes, ids and roles the Read IR is made of.
//!
//! Following `docs/design-h5i-ir.md`: integer ids, immutable nodes with roles
//! and flags, and out-of-line text. The closed role enum matches the outline.

/// A node's place in the arena.
///
/// A plain index until phase 2 adds retained, reusable slots.
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
/// Payload-free and compact. [`ReadNode::level`] refines headings.
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
    /// Shared by the snapshot walker and IR renderer.
    pub fn as_str(self, level: u8) -> &'static str {
        match self {
            ReadRole::Document => "document",
            ReadRole::Text => "text",
            ReadRole::Heading => {
                // Tags provide levels 1–6; `role="heading"` defaults to 2.
                debug_assert!(
                    (1..=6).contains(&level),
                    "a heading reached the renderer with level {level}"
                );
                match level {
                    1 => "heading1",
                    2 => "heading2",
                    3 => "heading3",
                    4 => "heading4",
                    5 => "heading5",
                    _ => "heading6",
                }
            }
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
    /// Used to detect wrappers containing block structure.
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
/// A bit word leaves room for later flags without growing the node.
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
    /// Set on code lines whose leading indentation must survive storage.
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
/// Flat and `Copy`; text is stored in the arena and `depth` encodes the tree.
///
/// The 48-byte budget reserves room for phase 3 fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct ReadNode {
    /// The Blitz node this came from, for action dispatch and selectors.
    pub dom_id: u32,
    pub parent: ReadId,
    pub name: TextId,
    /// Resolved `href` or `src`, collapsed. `EMPTY` when the node has neither.
    ///
    /// Inline because links commonly need it and side storage costs more.
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

/// Prevent accidental growth beyond the design budget.
const _: () = assert!(std::mem::size_of::<ReadNode>() <= 48);

/// What an agent can name in a later command, before it is spelled out.
///
/// Self-contained because a ref minted before truncation can outlive its line.
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
        // Print the actual size on failure.
        assert!(
            std::mem::size_of::<ReadNode>() <= 48,
            "ReadNode is {} bytes",
            std::mem::size_of::<ReadNode>()
        );
    }

    #[test]
    fn every_role_prints_the_word_the_outline_uses() {
        for level in 1..=6u8 {
            assert_eq!(
                ReadRole::Heading.as_str(level),
                format!("heading{level}"),
                "heading level {level}"
            );
        }
        // Release builds retain the historical fallback.
        #[cfg(not(debug_assertions))]
        assert_eq!(ReadRole::Heading.as_str(9), "heading6");
        // Every other role ignores the level entirely.
        assert_eq!(ReadRole::Textbox.as_str(0), "textbox");
        assert_eq!(ReadRole::Text.as_str(0), "text");
        assert_eq!(ReadRole::Link.as_str(3), "link");
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
