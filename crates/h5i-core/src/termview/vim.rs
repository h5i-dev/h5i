//! The keymap: what a keystroke means when the keyboard is the viewer's.
//!
//! The viewer has always been modal. VIEW is where single letters are safe to
//! bind, because nothing typed there reaches the page; INTERACT hands the
//! keyboard and the pointer to the document and keeps exactly one key to escape
//! with. What was missing was anything *in* VIEW: `q`, `i` and `d`, and every
//! other way of moving around a page went through the pointer.
//!
//! That is the wrong instrument in a terminal, and not because the terminal is
//! primitive. A terminal reports cells rather than pixels, so a click lands at
//! the corner of the cell it was in; the viewer hides the cursor, so there is no
//! local echo to aim with; and the only feedback is a frame that arrives over a
//! socket. Chasing pixel parity means pixel-resolution mouse reporting, the
//! progressive keyboard protocol, a composited cursor sprite and input
//! prediction, and even then a drag is not expressible. Naming a target and
//! pressing a key needs none of it.
//!
//! Three rules hold this together:
//!
//! * **The page never sees these keys.** Everything here is decided in VIEW,
//!   where the keyboard is the viewer's. INTERACT still exists, for the canvas,
//!   the map and the drag that no keyboard can express — but only on an engine
//!   that can actually be driven that way, which h5i's own is not.
//! * **A binding that cannot work is not bound.** The terminal viewer watches
//!   boxes running an engine that is not ours, so which keys mean anything is a
//!   property of the session, read from what the engine advertises rather than
//!   inferred from its name. An unavailable key says why instead of doing
//!   nothing ([`Action::Unsupported`]).
//! * **Movement is portable.** Scrolling is expressed as wheel and arrow events
//!   every engine already understands, not as a message of ours, so the keys a
//!   reader uses most work everywhere. Only the parts that genuinely need the
//!   engine (hints, history, insert) are gated.

use std::collections::BTreeSet;

/// What the engine on the other end says its viewer lane can do.
///
/// Read off the `status` message rather than derived from the engine's name.
/// The distinction is not pedantry: the viewer is engine-agnostic by design, and
/// a name match would put engine-specific knowledge in the one component that
/// exists not to have any.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Features(BTreeSet<String>);

/// Built from whatever the `status` message listed, which is a list of strings
/// off a socket. The standard trait rather than an inherent constructor of the
/// same name, so `collect()` works and there is only one spelling.
impl<S: Into<String>> FromIterator<S> for Features {
    fn from_iter<I: IntoIterator<Item = S>>(names: I) -> Features {
        Features(names.into_iter().map(Into::into).collect())
    }
}

impl Features {
    pub fn has(&self, name: &str) -> bool {
        self.0.contains(name)
    }

    /// Nothing advertised. The state a viewer starts in and the one it stays in
    /// for an engine that never says, which is why the default is "no keys
    /// bound" rather than "assume the usual".
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// How far a scroll goes, in terms the viewer can turn into wheel deltas once it
/// knows the viewport.
///
/// Expressed as an intent rather than a pixel count because the pixel count
/// depends on a viewport the keymap does not know and should not have to be
/// given in order to answer "what does `d` mean".
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

/// What a hint press is *for*, decided before the overlay goes up.
///
/// Asking first is what lets one overlay serve three verbs: the labels are the
/// same, so `f`, `F` and `yf` differ only in what happens when one is typed.
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
    /// Kept for the engine that can use it. On one that cannot, offering it is
    /// worse than not having it: h5i's own engine answers a press and a move
    /// with nothing at all, and the only pointer gesture it acts on is a release
    /// over a link — which `f` does better, without aiming, and with a receipt
    /// that names what was activated instead of where. A mode that silently does
    /// one twentieth of what it looks like it does is a trap, so it is offered
    /// only where it is real. See [`resolve`].
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
    /// Nothing is bound to this. Distinguished from `Pending` so a viewer can
    /// clear a half-typed prefix rather than leaving it to swallow the next key.
    Unbound,
}

/// Resolve one key against the pending prefix.
///
/// `pending` is the prefix already typed (`""`, `"g"` or `"y"`). Returning
/// [`Action::Pending`] means the caller should keep the key it just passed as
/// the new prefix; every other answer means the prefix is finished with.
///
/// Uppercase is a different binding rather than a mistyped one, which is the
/// opposite of the rule inside a hint overlay and deliberately so: `H` and `h`
/// are two commands in vim, while a hint label is a name and a name typed in
/// caps is the same name.
pub fn resolve(key: char, pending: &str, features: &Features) -> Action {
    // A binding whose engine support is missing answers with the reason. Doing
    // nothing would be indistinguishable from a dropped keystroke, and the
    // human's next move would be to press it harder.
    let needs = |feature: &str, action: Action, why: &'static str| {
        if features.has(feature) {
            action
        } else {
            Action::Unsupported(why)
        }
    };

    // The one binding that fails *open*, and the asymmetry is the point rather
    // than an oversight.
    //
    // Every other gated key is a capability introduced alongside this keymap, so
    // an engine that has not said it offers one does not offer it. Handing the
    // page the keyboard is not new: it is what this viewer did before any of
    // this existed, and it is the only way to drive a canvas, a map or a drag on
    // a box running an engine that will never send a feature list at all.
    //
    // So the question for `i` is not "did you say yes" but "did you say no". An
    // engine that described its lane and left `pointer` out has said no, and is
    // taken at its word; an engine that described nothing has said nothing, and
    // keeps what already worked.
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
        // A prefix followed by something it does not lead to. The prefix is
        // spent either way, so this is `Unbound` rather than a re-resolution of
        // the second key on its own: `gq` must not quit.
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
        // Vimium's `F` opens in a new tab. There are no tabs here and there
        // never will be (see the roadmap), so the shifted pair of "activate it"
        // is "type into it", which is the other thing a human at a live view
        // spends their time doing.
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
        // Moved off `d`, which is half-page-down everywhere a reader has used
        // this idiom. Shifted rather than relocated to a prefix because it is a
        // panel toggle and belongs next to the other one-key viewer state.
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
/// The viewer's half of the hint scheme. The engine mints the labels, because
/// two viewers numbering one page must not disagree; each viewer narrows them,
/// because what has been typed is that viewer's own state and no engine has any
/// business holding it. The web viewer implements this same rule in JavaScript.
///
/// Deciding a single hit *immediately* is only sound because the engine's labels
/// are prefix-free: a label that equals what was typed cannot also be the start
/// of another, so there is nothing to wait for. Without that property this would
/// have to hold every press until the next one arrived, and a viewer that waits
/// on every keystroke feels broken.
///
/// Case-insensitive, unlike [`resolve`]. A command key and its shift are two
/// commands; a label is a name, and a name typed in caps is the same name.
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
/// Pulled out of the viewer so the ordering can be tested without a socket, a
/// terminal or an engine. The invariant is that everything the human typed
/// reaches the page, in order, while never more than one message is outstanding.
///
/// The caller decides what "owed" carries. For typing it is the keys struck
/// since the last batch went out, and they are *batched*, not merged: a
/// keystroke is a delta, so dropping the ones in between would lose characters.
/// What batching buys is that the expensive half — the engine's relayout and
/// render — is paid once for the batch however many keys it holds, which is the
/// same win merging would have given without the loss.
///
/// Hence [`Coalesce::timed_out`] releases the wire without asking for a repeat:
/// a batch that may already have been applied must not be sent twice, so what
/// goes out afterwards is only what has not been sent yet.
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

    /// Nothing is coming. Returns whether the wire is now free to send on.
    ///
    /// Deliberately not "resend that": the lost message may have arrived and
    /// been applied, and repeating a batch of keystrokes would type the word
    /// twice. It gives the wire back, and the caller sends whatever has piled
    /// up behind it.
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
/// The list and [`resolve`] are the same knowledge written twice, which a test
/// keeps honest: a binding that is documented and not resolvable, or resolvable
/// and undocumented, fails the build rather than surprising somebody at a
/// terminal.
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

    #[test]
    fn movement_works_on_an_engine_that_advertises_nothing() {
        // The keys a reader uses most are expressed as wheel and arrow events
        // every engine understands, so they must not be gated behind a feature
        // list an older engine will never send.
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

    /// The rule that keeps the viewer honest on a box running someone else's
    /// engine: a key that cannot work says why rather than doing nothing.
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

    /// A spent prefix must not let the second key act on its own, or `gq` would
    /// quit the viewer.
    #[test]
    fn a_prefix_that_leads_nowhere_swallows_the_key_rather_than_falling_through() {
        let all = all();
        assert_eq!(resolve('q', "g", &all), Action::Unbound);
        assert_eq!(resolve('j', "y", &all), Action::Unbound);
    }

    /// The opposite of the hint overlay's rule, and deliberately: in vim a
    /// shifted key is a different command, while a hint label is a name.
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

    /// `i` is the one key that fails open, and this pins both directions of it.
    ///
    /// An engine that has said nothing keeps the behaviour this viewer shipped
    /// with, because that is where the pointer is the only way to drive a canvas
    /// or a drag. An engine that described its lane and did not claim `pointer`
    /// has said no, and is taken at its word rather than being allowed to offer
    /// a mode that does nothing: h5i's own engine answers a press and a move
    /// with nothing, and a release only over a link.
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

    /// A label is a name, so the caps-lock version of it is the same name.
    #[test]
    fn a_label_typed_in_caps_is_the_same_label() {
        let out = labels(&["s", "ad", "af"]);
        assert_eq!(narrow(&out, "AD"), Match::One(1));
        assert_eq!(narrow(&out, "A"), Match::Several(vec![1, 2]));
    }

    /// An empty overlay is not a match for anything, including the empty
    /// string: a viewer that treated "nothing typed, nothing to hit" as a hit
    /// would act on a target that is not there.
    #[test]
    fn an_empty_overlay_offers_nothing_to_hit() {
        assert_eq!(narrow(&[], ""), Match::Several(Vec::new()));
        assert_eq!(narrow(&[], "s"), Match::None);
    }

    // ─── coalescing ─────────────────────────────────────────────────────────

    /// A burst of typing becomes one message in flight and one batch owed, not a
    /// queue of them. This is the whole fix for a 40ms round trip: without it,
    /// nineteen characters is nineteen serialized relayouts.
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

    /// A single keystroke with nothing behind it must not send twice.
    #[test]
    fn one_keystroke_is_one_message() {
        let mut c = Coalesce::default();
        assert!(c.typed());
        assert!(!c.landed());
        assert!(!c.waiting());
    }

    /// Typing again after the wire is clear starts a new one rather than
    /// waiting for a reply that already came.
    #[test]
    fn typing_after_a_reply_sends_immediately() {
        let mut c = Coalesce::default();
        c.typed();
        c.landed();
        assert!(c.typed(), "a keystroke after the reply was held back");
    }

    /// The wedge this exists to prevent: a reply that never comes. The web
    /// viewer's forward drops input outright when the control lock is not the
    /// human's, and a dropped message is answered by nothing.
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

    /// And a timeout with nothing outstanding sends nothing.
    #[test]
    fn a_timeout_with_an_empty_wire_sends_nothing() {
        let mut c = Coalesce::default();
        assert!(!c.timed_out());
    }

    // ─── the web viewer's copy ──────────────────────────────────────────────

    /// The two viewers must not drift apart.
    ///
    /// The whole point of giving the web viewer the same keys is that there is
    /// one thing to learn, and a keymap maintained by hand in two languages
    /// drifts silently: nothing fails, one viewer just quietly does something
    /// else. So the JavaScript table is parsed out of the page and compared
    /// against this one, and a change to either without the other fails the
    /// build.
    ///
    /// Deliberately the *documented* table rather than the resolver: what a
    /// reader is promised is what has to match. `every_documented_binding_actually_resolves`
    /// is what ties this table back to the code on this side, and the web
    /// viewer's own `resolve` is tied to it by the same list being rendered as
    /// its key overlay.
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

    /// The key list and the resolver are the same knowledge written twice.
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

    /// And the other direction: a binding that needs a feature must say so in
    /// the list, or the list will offer a key that answers with a refusal.
    /// Checked against an engine that *described* its lane and offered none of
    /// these, rather than against one that said nothing. The empty list is not
    /// the gating case: it is the "we have not been told" case, which `i` treats
    /// differently on purpose.
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
