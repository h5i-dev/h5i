//! What a key does to a text field.
//!
//! The decision half of real keyboard input, kept apart from the document half
//! in [`crate::engine::Page::key_to_focused`]. `type` sets a field's whole value
//! and leaves the caret at the end, which suits an agent; a person needs a caret
//! that moves and a page that hears `keydown`.
//!
//! Design: `docs/design-interminal-browser.md` V4. Two rules run through it:
//!
//! * **An unmapped key is not swallowed.** It becomes [`Edit::Ignore`], which
//!   still delivers the DOM events, so a page's own shortcut keeps working.
//! * **Modified keys are commands, not text.** `Ctrl-S` types no `s`. Shift is
//!   the exception, since shifted characters arrive already shifted.

use serde::{Deserialize, Serialize};

/// One key as a viewer reports it, in the DOM's own vocabulary.
///
/// The same three fields both viewers already send with `input_keyboard`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Key {
    /// DOM `key`: `"a"`, `"Enter"`, `"ArrowLeft"`.
    pub name: String,
    /// The character this keystroke inserts, when it inserts one.
    ///
    /// Reported by the viewer, not derived here: deriving it means re-deciding
    /// what a shifted key produces on a keyboard layout we cannot see.
    #[serde(default)]
    pub text: Option<String>,
    /// CDP's modifier bitmask: 1 alt, 2 ctrl, 4 meta, 8 shift.
    #[serde(default)]
    pub modifiers: u32,
}

pub mod modifiers {
    pub const ALT: u32 = 1;
    pub const CTRL: u32 = 2;
    pub const META: u32 = 4;
    pub const SHIFT: u32 = 8;
}

impl Key {
    fn has(&self, mask: u32) -> bool {
        self.modifiers & mask != 0
    }

    /// A chord, rather than a keystroke that produces text. Shift is excluded:
    /// it is how capitals are typed.
    fn commanding(&self) -> bool {
        self.has(modifiers::CTRL) || self.has(modifiers::META) || self.has(modifiers::ALT)
    }
}

/// What one key should do to the field that has focus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit<'a> {
    /// Put this text in, replacing the selection if there is one.
    Insert(&'a str),
    Backspace,
    DeleteForward,
    BackspaceWord,
    DeleteWord,
    Left,
    Right,
    WordLeft,
    WordRight,
    Up,
    Down,
    LineStart,
    LineEnd,
    TextStart,
    TextEnd,
    SelectLeft,
    SelectRight,
    SelectToLineStart,
    SelectToLineEnd,
    SelectAll,
    /// Leave this control for the next one.
    FocusNext,
    /// And the one before, which this engine cannot yet do. Mapped anyway, so
    /// it reads as a missing capability rather than a key that types nothing.
    FocusPrevious,
    /// Not a text edit. The events are still delivered.
    Ignore,
}

impl Edit<'_> {
    /// Whether this keystroke produces text, which is what `keypress` answers.
    /// A caret move fires `keydown` and `keyup` and no `keypress`, as in a real
    /// browser.
    pub fn types(&self) -> bool {
        matches!(self, Edit::Insert(_))
    }

    /// Whether the caret may have moved even when the text did not. The caret is
    /// drawn, so a motion is still a new picture.
    pub fn moves_the_caret(&self) -> bool {
        !matches!(self, Edit::Ignore | Edit::FocusNext | Edit::FocusPrevious)
    }
}

/// Decide what `key` does.
pub fn edit_for(key: &Key) -> Edit<'_> {
    let shift = key.has(modifiers::SHIFT);
    // Both platform conventions for word-wise and line-wise motion are accepted;
    // picking one would feel broken on half the machines this runs on.
    let word = key.has(modifiers::CTRL) || key.has(modifiers::ALT);
    let line = key.has(modifiers::META);

    match key.name.as_str() {
        "Backspace" if word => Edit::BackspaceWord,
        "Backspace" => Edit::Backspace,
        "Delete" if word => Edit::DeleteWord,
        "Delete" => Edit::DeleteForward,

        "ArrowLeft" if shift => Edit::SelectLeft,
        "ArrowLeft" if line => Edit::LineStart,
        "ArrowLeft" if word => Edit::WordLeft,
        "ArrowLeft" => Edit::Left,
        "ArrowRight" if shift => Edit::SelectRight,
        "ArrowRight" if line => Edit::LineEnd,
        "ArrowRight" if word => Edit::WordRight,
        "ArrowRight" => Edit::Right,

        // Parley resolves up and down correctly for a single-line field too,
        // so it is left to decide.
        "ArrowUp" if line => Edit::TextStart,
        "ArrowUp" => Edit::Up,
        "ArrowDown" if line => Edit::TextEnd,
        "ArrowDown" => Edit::Down,

        "Home" if shift => Edit::SelectToLineStart,
        "Home" if word => Edit::TextStart,
        "Home" => Edit::LineStart,
        "End" if shift => Edit::SelectToLineEnd,
        "End" if word => Edit::TextEnd,
        "End" => Edit::LineEnd,

        "Tab" if shift => Edit::FocusPrevious,
        "Tab" => Edit::FocusNext,

        // Left to the caller: what `Enter` does depends on the control, and
        // `Escape` belongs to whoever is watching.
        "Enter" | "Escape" => Edit::Ignore,

        "a" | "A" if key.has(modifiers::CTRL) || key.has(modifiers::META) => Edit::SelectAll,

        _ => match &key.text {
            // A chord types nothing: `Ctrl-S` must not insert an `s` into the
            // field somebody was saving.
            Some(text) if !key.commanding() && !text.is_empty() => Edit::Insert(text),
            _ => Edit::Ignore,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str) -> Key {
        Key {
            name: name.to_string(),
            text: None,
            modifiers: 0,
        }
    }

    fn typed(text: &str) -> Key {
        Key {
            name: text.to_string(),
            text: Some(text.to_string()),
            modifiers: 0,
        }
    }

    fn with(mut k: Key, mods: u32) -> Key {
        k.modifiers = mods;
        k
    }

    #[test]
    fn a_printable_key_inserts_the_text_the_viewer_reported() {
        assert_eq!(edit_for(&typed("a")), Edit::Insert("a"));
        assert_eq!(edit_for(&with(typed("A"), modifiers::SHIFT)), Edit::Insert("A"));
        // Including what no keyboard has a cap for.
        assert_eq!(edit_for(&typed("あ")), Edit::Insert("あ"));
        assert_eq!(edit_for(&typed("😀")), Edit::Insert("😀"));
    }

    /// `Ctrl-S` must not type an `s` into the field being saved.
    #[test]
    fn a_chord_is_a_command_and_types_nothing() {
        for m in [modifiers::CTRL, modifiers::META, modifiers::ALT] {
            assert_eq!(edit_for(&with(typed("s"), m)), Edit::Ignore, "modifier {m}");
        }
        // Shift is not a chord. It is how capitals are typed.
        assert_eq!(
            edit_for(&with(typed("S"), modifiers::SHIFT)),
            Edit::Insert("S")
        );
    }

    #[test]
    fn the_editing_keys_map_to_edits_rather_than_to_text() {
        assert_eq!(edit_for(&key("Backspace")), Edit::Backspace);
        assert_eq!(edit_for(&key("Delete")), Edit::DeleteForward);
        assert_eq!(edit_for(&key("ArrowLeft")), Edit::Left);
        assert_eq!(edit_for(&key("ArrowRight")), Edit::Right);
        assert_eq!(edit_for(&key("Home")), Edit::LineStart);
        assert_eq!(edit_for(&key("End")), Edit::LineEnd);
        assert_eq!(edit_for(&key("Tab")), Edit::FocusNext);
    }

    /// Both platform conventions are accepted.
    #[test]
    fn word_and_line_motions_are_accepted_in_either_platforms_spelling() {
        for m in [modifiers::CTRL, modifiers::ALT] {
            assert_eq!(edit_for(&with(key("ArrowLeft"), m)), Edit::WordLeft);
            assert_eq!(edit_for(&with(key("Backspace"), m)), Edit::BackspaceWord);
        }
        assert_eq!(
            edit_for(&with(key("ArrowLeft"), modifiers::META)),
            Edit::LineStart
        );
        assert_eq!(
            edit_for(&with(key("ArrowUp"), modifiers::META)),
            Edit::TextStart
        );
    }

    #[test]
    fn shift_with_a_motion_selects_rather_than_moves() {
        assert_eq!(
            edit_for(&with(key("ArrowLeft"), modifiers::SHIFT)),
            Edit::SelectLeft
        );
        assert_eq!(
            edit_for(&with(key("Home"), modifiers::SHIFT)),
            Edit::SelectToLineStart
        );
        assert_eq!(
            edit_for(&with(typed("a"), modifiers::CTRL)),
            Edit::SelectAll,
            "Ctrl-A is select-all, not an `a`"
        );
    }

    /// An unmapped key still reaches the page, so a site's own shortcut works.
    #[test]
    fn an_unmapped_key_is_ignored_rather_than_eaten() {
        assert_eq!(edit_for(&key("F5")), Edit::Ignore);
        assert_eq!(edit_for(&key("Enter")), Edit::Ignore);
        assert_eq!(edit_for(&key("Escape")), Edit::Ignore);
        // And a printable key with no text reported is not guessed at.
        assert_eq!(edit_for(&key("Dead")), Edit::Ignore);
    }

    /// `keypress` is for keys that produce text, as in a real browser.
    #[test]
    fn only_a_key_that_types_fires_keypress() {
        assert!(edit_for(&typed("a")).types());
        assert!(!edit_for(&key("ArrowLeft")).types());
        assert!(!edit_for(&key("Backspace")).types());
    }

    /// A motion that changed no text is still a new picture: the caret moved.
    #[test]
    fn a_motion_is_worth_a_frame_even_when_the_text_did_not_change() {
        assert!(edit_for(&key("ArrowLeft")).moves_the_caret());
        assert!(edit_for(&key("Home")).moves_the_caret());
        assert!(!edit_for(&key("F5")).moves_the_caret());
        assert!(!edit_for(&key("Tab")).moves_the_caret());
    }
}
