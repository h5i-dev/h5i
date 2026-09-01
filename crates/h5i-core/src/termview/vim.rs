//! The keymap: what a keystroke means when the keyboard is the viewer's.
//!
//! A pointer is a poor instrument in a terminal — cells rather than pixels, no
//! visible cursor to aim with, and feedback only when a frame comes back over a
//! socket. Naming a target and pressing a key needs none of that.
//!
//! Three rules:
//!
//! * **The page never sees these keys.** They are decided in VIEW. INTERACT
//!   still exists for the canvas and the drag no keyboard can express, but only
//!   where an engine can be driven that way.
//! * **A binding that cannot work is not bound.** Which keys mean anything is
//!   read from what the engine advertises, not inferred from its name, and an
//!   unavailable key says why ([`Action::Unsupported`]).
//! * **Movement is portable.** Scrolling goes out as wheel and arrow events
//!   every engine understands, so the keys a reader uses most work everywhere.

use std::collections::BTreeSet;

/// What the engine on the other end says its viewer lane can do.
///
/// Read off the `status` message, not derived from the engine's name: the viewer
/// is engine-agnostic by design.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Features(BTreeSet<String>);

/// Built from whatever the `status` message listed.
impl<S: Into<String>> FromIterator<S> for Features {
    fn from_iter<I: IntoIterator<Item = S>>(names: I) -> Features {
        Features(names.into_iter().map(Into::into).collect())
    }
}

impl Features {
    pub fn has(&self, name: &str) -> bool {
        self.0.contains(name)
    }

    /// Nothing advertised: the state a viewer starts in, and stays in for an
    /// engine that never says.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// How far a scroll goes. An intent rather than a pixel count, because the count
/// depends on a viewport this keymap does not need to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scroll {
    LineDown,
    LineUp,
    HalfDown,
    HalfUp,
    PageDown,
    PageUp,
    Top,
    Bottom,
}

/// What a hint press is *for*, decided before the overlay goes up. One set of
/// labels serves `f`, `F` and `yf`; only the outcome differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintThen {
    /// Activate it: follow the link, press the button, toggle the box.
    Click,
    /// Put the caret in it and start typing.
    Insert,
    /// Copy where it goes, without going there.
    Yank,
}

impl HintThen {
    /// What the status line says while the overlay is up.
    pub fn prompt(self) -> &'static str {
        match self {
            HintThen::Click => "follow",
            HintThen::Insert => "type into",
            HintThen::Yank => "copy link from",
        }
    }
}

/// What a keystroke in VIEW means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Leave the viewer.
    Quit,
    /// Hand the keyboard and the pointer to the page.
    ///
    /// Offered only where an engine can use it. h5i's own answers a pointer
    /// press and move with nothing, so a mode that appears to work and does not
    /// would be a trap. See [`resolve`].
    Interact,
    /// Toggle the console pane.
    Developer,
    /// Toggle the key list.
    Help,
    Scroll(Scroll),
    Reload,
    /// -1 for back, 1 for forward.
    History(i8),
    /// Put the overlay up, to do this when a label is typed.
    Hints(HintThen),
    /// Put the caret in the first field on the page and start typing.
    InsertFirstField,
    /// Copy the page's own URL.
    YankUrl,
    /// A prefix was consumed (`g`, `y`). Nothing has happened yet.
    Pending,
    /// A binding this engine cannot serve, with the reason to show.
    Unsupported(&'static str),
    /// Nothing is bound to this. Distinct from `Pending` so a viewer can clear a
    /// half-typed prefix rather than let it swallow the next key.
    Unbound,
}

/// Resolve one key against the pending prefix (`""`, `"g"` or `"y"`).
///
/// [`Action::Pending`] means the caller should keep this key as the new prefix;
/// anything else means the prefix is spent.
///
/// Case matters here, unlike in [`narrow`]: `H` and `h` are two commands, while
/// a hint label is a name.
pub fn resolve(key: char, pending: &str, features: &Features) -> Action {
    // A missing capability answers with the reason: a key that does nothing is
    // indistinguishable from a dropped keystroke, and gets pressed harder.
    let needs = |feature: &str, action: Action, why: &'static str| {
        if features.has(feature) {
            action
        } else {
            Action::Unsupported(why)
        }
    };

    // `i` is the one binding that fails *open*. Every other gated key is new, so
    // silence means no; handing the page the keyboard predates all of this and is
    // the only way to drive a canvas on an engine that will never send a feature
    // list. So the question for `i` is "did you say no", not "did you say yes".
    let pointer_or_nothing_said = |action: Action, why: &'static str| {
        if features.is_empty() || features.has("pointer") {
            action
        } else {
            Action::Unsupported(why)
        }
    };

    match (pending, key) {
        // ─── prefixes ───────────────────────────────────────────────────────
        ("", 'g') | ("", 'y') => Action::Pending,

        ("g", 'g') => Action::Scroll(Scroll::Top),
        ("g", 'i') => needs(
            "insert",
            Action::InsertFirstField,
            "this session's engine does not offer typing from the viewer",
        ),
        ("y", 'y') => Action::YankUrl,
        ("y", 'f') => needs(
            "hints",
            Action::Hints(HintThen::Yank),
            "this session's engine does not offer a hint overlay",
        ),
        // The prefix is spent either way, so this is `Unbound` rather than a
        // re-resolution of the second key alone: `gq` must not quit.
        ("g" | "y", _) => Action::Unbound,

        // ─── moving ─────────────────────────────────────────────────────────
        ("", 'j') => Action::Scroll(Scroll::LineDown),
        ("", 'k') => Action::Scroll(Scroll::LineUp),
        ("", 'd') => Action::Scroll(Scroll::HalfDown),
        ("", 'u') => Action::Scroll(Scroll::HalfUp),
        ("", ' ') => Action::Scroll(Scroll::PageDown),
        ("", 'b') => Action::Scroll(Scroll::PageUp),
        ("", 'G') => Action::Scroll(Scroll::Bottom),

        // ─── going ──────────────────────────────────────────────────────────
        ("", 'H') => needs(
            "history",
            Action::History(-1),
            "this session's engine does not keep history for the viewer",
        ),
        ("", 'L') => needs(
            "history",
            Action::History(1),
            "this session's engine does not keep history for the viewer",
        ),
        ("", 'r') => needs(
            "reload",
            Action::Reload,
            "this session's engine does not offer reload from the viewer",
        ),

        // ─── acting ─────────────────────────────────────────────────────────
        ("", 'f') => needs(
            "hints",
            Action::Hints(HintThen::Click),
            "this session's engine does not offer a hint overlay",
        ),
        // Vimium's `F` opens a new tab. There are none here and never will be,
        // so the shifted pair of "activate it" is "type into it".
        ("", 'F') => needs(
            "hints",
            Action::Hints(HintThen::Insert),
            "this session's engine does not offer a hint overlay",
        ),

        // ─── the viewer itself ──────────────────────────────────────────────
        ("", 'q') => Action::Quit,
        ("", 'i') => pointer_or_nothing_said(
            Action::Interact,
            "this session's engine is driven from the keyboard: `f` reaches anything \
             a click could, and `F` types into it",
        ),
        // Moved off `d`, which is half-page-down in this idiom.
        ("", 'D') => Action::Developer,
        ("", '?') => Action::Help,

        _ => Action::Unbound,
    }
}

/// What typing `typed` has done to a set of hint labels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Match {
    /// Exactly one label is `typed`. Act on it.
    One(usize),
    /// Several still start with `typed`. Keep the overlay up, showing only
    /// these.
    Several(Vec<usize>),
    /// Nothing starts with `typed`. The human mistyped, and the honest answer
    /// is to say so rather than to act on the nearest thing.
    None,
}

/// Narrow hint labels by what has been typed so far.
///
/// The viewer's half of the hint scheme: the engine mints the labels, each viewer
/// tracks its own human's typing. The web viewer has the same rule in JavaScript.
///
/// Acting on a single hit immediately is sound only because the labels are
/// prefix-free — a complete label cannot also be the start of another, so there
/// is nothing to wait for.
///
/// Case-insensitive, unlike [`resolve`]: a label is a name.
pub fn narrow(labels: &[String], typed: &str) -> Match {
    let typed = typed.to_ascii_lowercase();
    if typed.is_empty() {
        return Match::Several((0..labels.len()).collect());
    }
    let hits: Vec<usize> = labels
        .iter()
        .enumerate()
        .filter(|(_, label)| label.to_ascii_lowercase().starts_with(&typed))
        .map(|(i, _)| i)
        .collect();
    match hits.len() {
        0 => Match::None,
        1 if labels[hits[0]].to_ascii_lowercase() == typed => Match::One(hits[0]),
        _ => Match::Several(hits),
    }
}

/// One message on the wire at a time, and one owed behind it.
///
/// Pulled out of the viewer so the ordering can be tested without a socket. The
/// invariant: everything typed reaches the page, in order, with never more than
/// one message outstanding.
///
/// The caller decides what "owed" carries. For typing it is a *batch* of keys,
/// not a merge — a keystroke is a delta, so dropping the ones in between would
/// lose characters. Batching still pays the engine's relayout and render only
/// once per message, which is where the cost is.
#[derive(Debug, Default)]
pub struct Coalesce {
    in_flight: bool,
    dirty: bool,
}

impl Coalesce {
    /// The human typed. Returns whether to send now.
    pub fn typed(&mut self) -> bool {
        if self.in_flight {
            self.dirty = true;
            return false;
        }
        self.in_flight = true;
        self.dirty = false;
        true
    }

    /// The engine answered. Returns whether something is owed.
    pub fn landed(&mut self) -> bool {
        self.in_flight = false;
        if self.dirty {
            self.dirty = false;
            self.in_flight = true;
            return true;
        }
        false
    }

    /// Nothing is coming. Returns whether the wire is free to send on.
    ///
    /// Not "resend that": the lost message may have arrived, and repeating a
    /// batch would type the word twice. Only what is still pending goes out.
    pub fn timed_out(&mut self) -> bool {
        if !self.in_flight {
            return false;
        }
        self.in_flight = false;
        self.dirty = false;
        self.in_flight = true;
        true
    }

    pub fn waiting(&self) -> bool {
        self.in_flight
    }
}

/// One row of the key list.
pub struct Binding {
    pub keys: &'static str,
    pub what: &'static str,
    /// The feature this needs, or `None` when it works on any engine.
    pub needs: Option<&'static str>,
}

/// Every binding, in the order the key list shows them.
///
/// The same knowledge as [`resolve`], written twice and kept honest by a test.
pub const BINDINGS: &[Binding] = &[
    Binding { keys: "j k", what: "scroll a line", needs: None },
    Binding { keys: "d u", what: "scroll half a page", needs: None },
    Binding { keys: "space b", what: "scroll a page", needs: None },
    Binding { keys: "gg G", what: "top, bottom", needs: None },
    Binding { keys: "f", what: "hint, then follow", needs: Some("hints") },
    Binding { keys: "F", what: "hint, then type into", needs: Some("hints") },
    Binding { keys: "yf", what: "hint, then copy its link", needs: Some("hints") },
    Binding { keys: "gi", what: "type into the first field", needs: Some("insert") },
    Binding { keys: "yy", what: "copy this page's URL", needs: None },
    Binding { keys: "H L", what: "back, forward", needs: Some("history") },
    Binding { keys: "r", what: "reload", needs: Some("reload") },
    Binding { keys: "i", what: "take the pointer (INTERACT)", needs: Some("pointer") },
    Binding { keys: "D", what: "console pane", needs: None },
    Binding { keys: "?", what: "this list", needs: None },
    Binding { keys: "q", what: "leave", needs: None },
];

#[cfg(test)]
mod tests {
    use super::*;

    fn all() -> Features {
        Features::from_iter(["hints", "act", "insert", "history", "reload"])
    }

    /// Movement is portable, so it must not be gated behind a feature list an
    /// older engine will never send.
    #[test]
    fn movement_works_on_an_engine_that_advertises_nothing() {
        let none = Features::default();
        for (key, want) in [
            ('j', Scroll::LineDown),
            ('k', Scroll::LineUp),
            ('d', Scroll::HalfDown),
            ('u', Scroll::HalfUp),
            (' ', Scroll::PageDown),
            ('b', Scroll::PageUp),
            ('G', Scroll::Bottom),
        ] {
            assert_eq!(resolve(key, "", &none), Action::Scroll(want), "key `{key}`");
        }
        assert_eq!(resolve('g', "", &none), Action::Pending);
        assert_eq!(resolve('g', "g", &none), Action::Scroll(Scroll::Top));
    }

    /// A key that cannot work says why rather than doing nothing.
    #[test]
    fn a_binding_the_engine_cannot_serve_explains_itself() {
        let none = Features::from_iter(["something-else-entirely"]);
        for key in ['f', 'F', 'H', 'L', 'r', 'i'] {
            match resolve(key, "", &none) {
                Action::Unsupported(why) => assert!(!why.is_empty(), "key `{key}`"),
                other => panic!("key `{key}` resolved to {other:?} with no engine support"),
            }
        }
        assert!(matches!(resolve('i', "y", &none), Action::Unbound));
        assert!(matches!(resolve('f', "y", &none), Action::Unsupported(_)));
    }

    #[test]
    fn the_same_keys_work_once_the_engine_advertises_them() {
        let all = all();
        assert_eq!(resolve('f', "", &all), Action::Hints(HintThen::Click));
        assert_eq!(resolve('F', "", &all), Action::Hints(HintThen::Insert));
        assert_eq!(resolve('f', "y", &all), Action::Hints(HintThen::Yank));
        assert_eq!(resolve('i', "g", &all), Action::InsertFirstField);
        assert_eq!(resolve('H', "", &all), Action::History(-1));
        assert_eq!(resolve('L', "", &all), Action::History(1));
        assert_eq!(resolve('r', "", &all), Action::Reload);
    }

    /// A spent prefix must not let the second key act alone: `gq` is not `q`.
    #[test]
    fn a_prefix_that_leads_nowhere_swallows_the_key_rather_than_falling_through() {
        let all = all();
        assert_eq!(resolve('q', "g", &all), Action::Unbound);
        assert_eq!(resolve('j', "y", &all), Action::Unbound);
    }

    /// Unlike a hint label, a shifted key is a different command.
    #[test]
    fn case_selects_a_different_binding_rather_than_being_forgiven() {
        let all = all();
        assert_ne!(resolve('d', "", &all), resolve('D', "", &all));
        assert_eq!(resolve('D', "", &all), Action::Developer);
        assert_ne!(resolve('f', "", &all), resolve('F', "", &all));
    }

    /// The one key that was already bound and must keep meaning what it meant.
    #[test]
    fn leaving_is_still_q() {
        assert_eq!(resolve('q', "", &all()), Action::Quit);
        assert_eq!(resolve('q', "", &Features::default()), Action::Quit);
    }

    /// `i` fails open, and this pins both directions: silence keeps the
    /// behaviour that shipped, a described lane without `pointer` is a no.
    #[test]
    fn the_pointer_is_offered_only_where_it_is_real() {
        // Said nothing: keep what worked.
        assert_eq!(resolve('i', "", &Features::default()), Action::Interact);

        // Said so: bind it.
        let pointing = Features::from_iter(["hints", "pointer"]);
        assert_eq!(resolve('i', "", &pointing), Action::Interact);

        // Described a lane without it: refuse, and point at the better path
        // rather than leaving the key to do a twentieth of what it looks like.
        match resolve('i', "", &all()) {
            Action::Unsupported(why) => {
                assert!(why.contains("keyboard"), "{why}");
                assert!(why.contains('f'), "{why}");
            }
            other => panic!("a keyboard-only engine offered the pointer: {other:?}"),
        }
    }

    // ─── narrowing ──────────────────────────────────────────────────────────

    fn labels(of: &[&str]) -> Vec<String> {
        of.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn typing_a_whole_label_picks_exactly_one_target() {
        let out = labels(&["s", "ad", "af"]);
        assert_eq!(narrow(&out, "s"), Match::One(0));
        assert_eq!(narrow(&out, "ad"), Match::One(1));
    }

    #[test]
    fn a_partial_label_keeps_the_overlay_up() {
        let out = labels(&["s", "ad", "af"]);
        assert_eq!(narrow(&out, "a"), Match::Several(vec![1, 2]));
        assert_eq!(narrow(&out, ""), Match::Several(vec![0, 1, 2]));
    }

    #[test]
    fn a_key_no_label_starts_with_matches_nothing() {
        assert_eq!(narrow(&labels(&["s", "ad"]), "z"), Match::None);
    }

    /// A label is a name, so caps lock does not change it.
    #[test]
    fn a_label_typed_in_caps_is_the_same_label() {
        let out = labels(&["s", "ad", "af"]);
        assert_eq!(narrow(&out, "AD"), Match::One(1));
        assert_eq!(narrow(&out, "A"), Match::Several(vec![1, 2]));
    }

    /// An empty overlay hits nothing, including on the empty string.
    #[test]
    fn an_empty_overlay_offers_nothing_to_hit() {
        assert_eq!(narrow(&[], ""), Match::Several(Vec::new()));
        assert_eq!(narrow(&[], "s"), Match::None);
    }

    // ─── coalescing ─────────────────────────────────────────────────────────

    /// A burst becomes one message in flight and one batch owed, not a queue of
    /// them: otherwise every character is a serialized relayout.
    #[test]
    fn a_burst_of_typing_puts_one_message_on_the_wire_and_owes_one_more() {
        let mut c = Coalesce::default();
        assert!(c.typed(), "the first keystroke goes straight out");
        for _ in 0..18 {
            assert!(!c.typed(), "a keystroke went out while one was in flight");
        }
        assert!(c.landed(), "the buffer moved, so one more is owed");
        assert!(!c.landed(), "nothing was owed after that one");
    }

    /// One keystroke must not send twice.
    #[test]
    fn one_keystroke_is_one_message() {
        let mut c = Coalesce::default();
        assert!(c.typed());
        assert!(!c.landed());
        assert!(!c.waiting());
    }

    /// Typing after the wire clears starts a new message immediately.
    #[test]
    fn typing_after_a_reply_sends_immediately() {
        let mut c = Coalesce::default();
        c.typed();
        c.landed();
        assert!(c.typed(), "a keystroke after the reply was held back");
    }

    /// The wedge this prevents: the web viewer's forward drops input when the
    /// control lock is not the human's, and a dropped message answers nothing.
    #[test]
    fn a_reply_that_never_arrives_does_not_wedge_typing_forever() {
        let mut c = Coalesce::default();
        c.typed();
        c.typed();
        assert!(c.waiting());
        assert!(c.timed_out(), "the timeout did not resend");
        assert!(c.waiting(), "the resend is itself in flight");
        assert!(!c.landed());
    }

    /// A timeout with an empty wire sends nothing.
    #[test]
    fn a_timeout_with_an_empty_wire_sends_nothing() {
        let mut c = Coalesce::default();
        assert!(!c.timed_out());
    }

    // ─── the web viewer's copy ──────────────────────────────────────────────

    /// The two viewers must not drift apart. A keymap maintained by hand in two
    /// languages drifts silently, so the JavaScript table is parsed out of the
    /// page and compared against this one.
    ///
    /// The *documented* table rather than the resolver: what a reader is promised
    /// is what has to match. `every_documented_binding_actually_resolves` ties
    /// this table to the code on this side.
    #[test]
    fn the_web_viewer_binds_the_same_keys_to_the_same_things() {
        let page = include_str!("../viewer.html");
        let table = page
            .split_once("const BINDINGS = [")
            .expect("the web viewer has a BINDINGS table")
            .1
            .split_once("\n];")
            .expect("the table is closed")
            .0;

        let rows: Vec<(String, String, Option<String>)> = table
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with('['))
            .map(|line| {
                let cells: Vec<&str> = line
                    .trim_start_matches('[')
                    .trim_end_matches(&[',', ']'][..])
                    .split("\", ")
                    .collect();
                assert_eq!(cells.len(), 3, "unparsable row: {line}");
                let unquote = |cell: &str| cell.trim().trim_matches('"').to_string();
                let need = unquote(cells[2]);
                (
                    unquote(cells[0]),
                    unquote(cells[1]),
                    (need != "null").then_some(need),
                )
            })
            .collect();

        assert_eq!(
            rows.len(),
            BINDINGS.len(),
            "the two key lists are different lengths"
        );
        for (row, binding) in rows.iter().zip(BINDINGS) {
            assert_eq!(row.0, binding.keys, "keys differ");
            assert_eq!(row.1, binding.what, "descriptions differ for `{}`", binding.keys);
            assert_eq!(
                row.2.as_deref(),
                binding.needs,
                "engine requirement differs for `{}`",
                binding.keys
            );
        }
    }

    /// The key list and the resolver must agree.
    #[test]
    fn every_documented_binding_actually_resolves() {
        let all = all();
        for binding in BINDINGS {
            for token in binding.keys.split(' ') {
                let (pending, key) = match token {
                    "space" => ("", ' '),
                    "gg" => ("g", 'g'),
                    "gi" => ("g", 'i'),
                    "yy" => ("y", 'y'),
                    "yf" => ("y", 'f'),
                    other => {
                        let mut chars = other.chars();
                        let key = chars.next().expect("a key");
                        assert!(chars.next().is_none(), "unhandled token `{other}`");
                        ("", key)
                    }
                };
                let action = resolve(key, pending, &all);
                assert!(
                    !matches!(action, Action::Unbound | Action::Pending),
                    "`{token}` is in the key list but resolves to {action:?}"
                );
            }
        }
    }

    /// And the reverse: a gated binding must be marked gated in the list.
    /// Checked against an engine that *described* its lane and offered none of
    /// these. An empty list is the "not told" case, which `i` treats differently.
    #[test]
    fn the_key_list_declares_the_same_requirements_the_resolver_enforces() {
        let none = Features::from_iter(["something-else-entirely"]);
        for binding in BINDINGS {
            let token = binding.keys.split(' ').next().expect("a key");
            let (pending, key) = match token {
                "space" => ("", ' '),
                "gg" => ("g", 'g'),
                "gi" => ("g", 'i'),
                "yy" => ("y", 'y'),
                "yf" => ("y", 'f'),
                other => ("", other.chars().next().expect("a key")),
            };
            let gated = matches!(resolve(key, pending, &none), Action::Unsupported(_));
            assert_eq!(
                gated,
                binding.needs.is_some(),
                "`{}` is gated by the resolver but not by the key list (or the reverse)",
                binding.keys
            );
        }
    }
}
