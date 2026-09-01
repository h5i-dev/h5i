//! What a key does to a text field.
//!
//! Typing used to reach this engine one way only: `type` set a field's whole
//! value, fired `input` and `change`, and left the caret at the end. That is the
//! right shape for an agent, which knows what it wants the field to say and does
//! not have a caret. It is the wrong shape for a person at a live view, and the
//! gap showed up as "we do not support text input": there was no `keydown` on
//! the page at all, so no caret moved, `Backspace` in the middle of a word did
//! nothing, `Tab` did not reach the next field, and a page listening for typing
//! — an autocomplete, a controlled input, a shortcut handler — never heard any.
//!
//! This is the decision half of the fix, kept apart from the document half in
//! [`crate::engine::Page::key_to_focused`] because it is a table of judgements
//! rather than a manipulation of anything. What `Home` means inside a field, and
//! whether `Ctrl-A` selects all or moves to the start, are arguments to have
//! once and write down.
//!
//! Two rules run through it:
//!
//! * **A key that is not ours is not swallowed.** Anything unmapped is
//!   [`Edit::Ignore`], which still delivers the DOM events and changes no text.
//!   A page's own shortcut must keep working while a field has focus.
//! * **Modified keys are commands, not text.** `Ctrl-S` types no `s`. Shift is
//!   the exception, since a shifted character arrives already shifted, which is
//!   also why the printable case reads `text` rather than re-deriving it.

use serde::{Deserialize, Serialize};

/// One key as a viewer reports it, in the DOM's own vocabulary.
///
/// The same three fields both viewers already send with `input_keyboard`, so
/// nothing new has to be invented on the wire and a viewer that predates this
/// needs no changes to benefit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Key {
    /// DOM `key`: `"a"`, `"Enter"`, `"ArrowLeft"`.
    pub name: String,
    /// The character this keystroke inserts, when it inserts one.
    ///
    /// Sent by the viewer rather than derived here, because deriving it means
    /// re-deciding what a shifted key produces on a layout we cannot see. A
    /// viewer already knows: the terminal read the byte, the browser was handed
    /// `event.key`.
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

    /// A chord, rather than a keystroke that produces text.
    ///
    /// Shift is excluded on purpose: it is how capitals are typed. On macOS the
    /// word-wise and line-wise motions live on Meta and Alt, which is why those
    /// count as commanding here and are mapped rather than ignored.
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
    /// And the one before, which this engine cannot currently do. Mapped anyway,
    /// so the refusal is a missing capability rather than a key that silently
    /// types nothing.
    FocusPrevious,
    /// Not a text edit. The events are still delivered.
    Ignore,
}

impl Edit<'_> {
    /// Whether this is a keystroke that produces text, which is the question
    /// `keypress` answers. A caret move fires `keydown` and `keyup` and no
    /// `keypress`, which is what the DOM does and what a page counting
    /// characters relies on.
    pub fn types(&self) -> bool {
        matches!(self, Edit::Insert(_))
    }

    /// Whether the caret may have moved even when the text did not.
    ///
    /// Asked because the caret is drawn: a viewer showing a page where `Left`
    /// changed nothing visible would look like a dropped keystroke.
    pub fn moves_the_caret(&self) -> bool {
        !matches!(self, Edit::Ignore | Edit::FocusNext | Edit::FocusPrevious)
    }
}

/// Decide what `key` does.
pub fn edit_for(key: &Key) -> Edit<'_> {
    let shift = key.has(modifiers::SHIFT);
    // Where the word-wise and line-wise motions live differs by platform, and
    // both conventions are accepted rather than one being picked: a viewer
    // reports the modifier the human actually held, and refusing the other
    // convention would make this feel broken on half the machines it runs on.
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

        // A single-line field has one line, so up and down are where the caret
        // ends up rather than a row above. Parley answers that correctly for
        // both shapes, so it is left to decide.
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

        // Left to the caller. `Enter` submits a form or inserts a newline
        // depending on the control, which is a question about the document
        // rather than about the key, and `Escape` belongs to whoever is
        // watching.
        "Enter" | "Escape" => Edit::Ignore,

        "a" | "A" if key.has(modifiers::CTRL) || key.has(modifiers::META) => Edit::SelectAll,

        _ => match &key.text {
            // A chord types nothing. `Ctrl-S` is a command, and inserting `s`
            // into the field the human was saving is the failure this guards.
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
        // Not re-derived from the name: a shifted key arrives already shifted,
        // and re-deriving it means re-deciding a layout we cannot see.
        assert_eq!(edit_for(&with(typed("A"), modifiers::SHIFT)), Edit::Insert("A"));
        // Including the ones no keyboard has a cap for.
        assert_eq!(edit_for(&typed("あ")), Edit::Insert("あ"));
        assert_eq!(edit_for(&typed("😀")), Edit::Insert("😀"));
    }

    /// The failure this guards: `Ctrl-S` typing an `s` into the field the human
    /// was trying to save.
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

    /// Both conventions, because a viewer reports the modifier the human held
    /// and refusing one would make this feel broken on half the machines.
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

    /// A key nobody mapped is not swallowed: the events still go to the page, so
    /// a site's own shortcut keeps working while a field has focus.
    #[test]
    fn an_unmapped_key_is_ignored_rather_than_eaten() {
        assert_eq!(edit_for(&key("F5")), Edit::Ignore);
        assert_eq!(edit_for(&key("Enter")), Edit::Ignore);
        assert_eq!(edit_for(&key("Escape")), Edit::Ignore);
        // And a printable key with no text reported is not guessed at.
        assert_eq!(edit_for(&key("Dead")), Edit::Ignore);
    }

    /// `keypress` is for keys that produce text, which is what the DOM does and
    /// what a page counting characters relies on.
    #[test]
    fn only_a_key_that_types_fires_keypress() {
        assert!(edit_for(&typed("a")).types());
        assert!(!edit_for(&key("ArrowLeft")).types());
        assert!(!edit_for(&key("Backspace")).types());
    }

    /// The caret is drawn, so a motion that changed no text is still a new
    /// picture. A viewer that skipped the frame would look like it had dropped
    /// the keystroke.
    #[test]
    fn a_motion_is_worth_a_frame_even_when_the_text_did_not_change() {
        assert!(edit_for(&key("ArrowLeft")).moves_the_caret());
        assert!(edit_for(&key("Home")).moves_the_caret());
        assert!(!edit_for(&key("F5")).moves_the_caret());
        assert!(!edit_for(&key("Tab")).moves_the_caret());
    }
}
