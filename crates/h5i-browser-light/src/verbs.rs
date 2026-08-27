//! The verb table, and the failures a verb can answer with.
//!
//! Before this, the verb set was written out three times — the clap enum in
//! `main.rs`, the JSON payload that enum built, and a `match verb` over string
//! literals in [`crate::stream`] — and nothing made the three agree. Adding a
//! verb meant remembering all three; forgetting one produced a verb the CLI
//! could send and the session did not know, which answers "unknown verb" to a
//! command the help text advertises.
//!
//! One enum, and every per-verb property is an exhaustive `match` on it, so a
//! new verb is a compile error until each question below has been answered for
//! it deliberately.
//!
//! **One of those questions is a security question**, which is the reason this
//! is a type rather than a tidier `match`. LOGIN mode refuses every verb that
//! reads the page, and it used to do it with a string allowlist:
//!
//! ```ignore
//! if session.login && !matches!(verb, "status" | "login") { ... }
//! ```
//!
//! The default was refusal, so the failure direction was safe, and a *new* verb
//! stayed refused until somebody thought about it. But the allowlist itself was
//! two string literals: one typo widened it, and no test that did not already
//! know about the typo would have caught it. [`Verb::readable_during_login`] is
//! the same rule as a predicate, where a typo is a name that does not resolve
//! and a new verb does not compile until it has answered.

use serde_json::{json, Value};

/// Everything the resident session can be asked to do.
///
/// The wire name is [`Verb::name`] and it is the only spelling: the CLI builds
/// its request from it and the session dispatches on it, so the two cannot
/// drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    /// What the session is on right now.
    Status,
    /// The page as a model should read it.
    Snapshot,
    /// Hand the page to the human at the live view.
    Login,
    /// Go to a URL.
    Navigate,
    /// Scroll the page.
    Scroll,
    /// Put text into a field.
    Type,
    /// Submit the form containing a ref.
    Submit,
    /// Follow or activate a ref.
    Click,
    /// The request log, as the engine wrote it.
    Requests,
    /// Wait until something is on the page, or until nothing can put it there.
    WaitFor,
    /// Wait until a page expression is true.
    WaitForScript,
    /// Pull structured data out of the page by selector.
    Extract,
    /// The page as markdown.
    Markdown,
    /// Which credentials this session could substitute, by name.
    Env,
    /// What this session did, as something that can be run again.
    Script,
    /// What the page publishes about itself: JSON-LD, OpenGraph, meta.
    Structured,
    /// Set a checkbox or radio to a state, rather than toggling it.
    SetChecked,
    /// Choose an option in a `<select>`.
    Select,
    /// Send a key that does something: Enter, Escape, Tab.
    Press,
    /// Locate elements by role and name, the way the outline names them.
    Find,
}

impl Verb {
    /// Every verb, for the help text and for tests that must cover all of them.
    pub const ALL: &'static [Verb] = &[
        Verb::Status,
        Verb::Snapshot,
        Verb::Login,
        Verb::Navigate,
        Verb::Scroll,
        Verb::Type,
        Verb::Submit,
        Verb::Click,
        Verb::Requests,
        Verb::WaitFor,
        Verb::WaitForScript,
        Verb::Extract,
        Verb::Markdown,
        Verb::Env,
        Verb::Script,
        Verb::Structured,
        Verb::SetChecked,
        Verb::Select,
        Verb::Press,
        Verb::Find,
    ];

    /// The name on the wire.
    pub fn name(self) -> &'static str {
        match self {
            Verb::Status => "status",
            Verb::Snapshot => "snapshot",
            Verb::Login => "login",
            Verb::Navigate => "navigate",
            Verb::Scroll => "scroll",
            Verb::Type => "type",
            Verb::Submit => "submit",
            Verb::Click => "click",
            Verb::Requests => "requests",
            Verb::WaitFor => "wait_for",
            Verb::WaitForScript => "wait_for_script",
            Verb::Extract => "extract",
            Verb::Markdown => "markdown",
            Verb::Env => "env",
            Verb::Script => "script",
            Verb::Structured => "structured",
            Verb::SetChecked => "set_checked",
            Verb::Select => "select",
            Verb::Press => "press",
            Verb::Find => "find",
        }
    }

    /// Resolve a wire name.
    pub fn from_name(name: &str) -> Option<Verb> {
        Verb::ALL.iter().copied().find(|v| v.name() == name)
    }

    /// Whether this verb may run while LOGIN mode is on.
    ///
    /// LOGIN mode exists so a credential typed by a human at the live view is
    /// not in a snapshot the agent asked for. So the rule is: **anything that
    /// reads the page is refused**, and the only exceptions are the two verbs
    /// that would make the mode impossible to leave — one reports that it is
    /// on, the other turns it off.
    pub fn readable_during_login(self) -> bool {
        match self {
            Verb::Status | Verb::Login => true,

            Verb::Snapshot
            | Verb::Navigate
            | Verb::Scroll
            | Verb::Type
            | Verb::Submit
            | Verb::Click
            // The request log names URLs a login flow visited. Engine-written,
            // but still a reading of where the page went, and a human typing a
            // password should not have that read out from under them.
            | Verb::Requests
            | Verb::WaitFor
            | Verb::WaitForScript
            | Verb::Extract
            | Verb::Markdown
            // Asked during a human's login, this is a question about
            // credentials at exactly the wrong moment.
            | Verb::Env
            // The recording names the fields a login flow used and the URLs it
            // visited. Engine-written, like the request log, and refused for
            // the same reason: it is still a reading of what the human at the
            // viewer is doing.
            | Verb::Script
            | Verb::Structured
            | Verb::SetChecked
            | Verb::Select
            | Verb::Press
            | Verb::Find => false,
        }
    }

    /// Whether this verb acts on a `@ref` from a snapshot.
    ///
    /// This is what drives the staleness check in [`crate::stream`]: a verb
    /// that names a ref is a verb whose caller read a snapshot, and the session
    /// checks that the snapshot it read still describes the page before acting
    /// on it. Answering `true` here is what buys that, so a new ref-taking verb
    /// gets the check by existing rather than by remembering.
    pub fn needs_ref(self) -> bool {
        match self {
            Verb::Type
            | Verb::Submit
            | Verb::Click
            | Verb::SetChecked
            | Verb::Select
            | Verb::Press => true,

            Verb::Status
            | Verb::Snapshot
            | Verb::Login
            | Verb::Navigate
            | Verb::Scroll
            | Verb::Requests
            | Verb::WaitFor
            | Verb::WaitForScript
            | Verb::Extract
            | Verb::Markdown
            | Verb::Env
            | Verb::Script
            | Verb::Structured
            // It produces handles rather than consuming one.
            | Verb::Find => false,
        }
    }

    /// Whether a `$H5I_SECRET_*` placeholder in this verb's arguments is
    /// resolved on the way to the page.
    ///
    /// Only where a value is being handed to the page as *content*. Resolving
    /// one into a selector, a URL or a wait condition would put a credential
    /// somewhere it can be read back — out of the DOM, out of the request log,
    /// out of an error message — which is the whole thing the indirection
    /// exists to prevent.
    pub fn substitutes_secrets(self) -> bool {
        match self {
            Verb::Type => true,

            Verb::Status
            | Verb::Snapshot
            | Verb::Login
            | Verb::Navigate
            | Verb::Scroll
            | Verb::Submit
            | Verb::Click
            | Verb::WaitFor
            | Verb::WaitForScript
            | Verb::Requests
            | Verb::Extract
            | Verb::Markdown
            | Verb::Env
            | Verb::Script
            | Verb::Structured
            | Verb::SetChecked
            | Verb::Select
            | Verb::Press
            | Verb::Find => false,
        }
    }

    /// Whether this verb needs a script realm to mean anything.
    ///
    /// Reported rather than discovered. `wait_for_script` on a session started
    /// without `--script` is a question with no engine to answer it, and
    /// silence there reads as a condition that never came true — which is a
    /// different fact and would send an agent down the wrong branch.
    pub fn needs_script(self) -> bool {
        match self {
            Verb::WaitForScript => true,

            Verb::Status
            | Verb::Snapshot
            | Verb::Login
            | Verb::Navigate
            | Verb::Scroll
            | Verb::Type
            | Verb::Submit
            | Verb::Click
            | Verb::Requests
            // `wait_for` reads the DOM, which exists either way. On a page with
            // no script it answers immediately rather than waiting, because
            // nothing can change the answer.
            | Verb::WaitFor
            | Verb::Extract
            | Verb::Markdown
            | Verb::Env
            // Named `script` for what it produces, not for the JS realm; it
            // needs no realm to report what the session did.
            | Verb::Script
            | Verb::Structured
            // These act on the DOM, which exists either way. A page with no
            // script simply has nothing listening for the events they fire.
            | Verb::SetChecked
            | Verb::Select
            | Verb::Press
            | Verb::Find => false,
        }
    }

    /// Whether this verb belongs in a replay.
    ///
    /// State-mutating verbs only. A replay exists to reach a state again, and
    /// the reads are how a model decided what to do next rather than part of
    /// the doing — replaying them would cost time and change nothing.
    ///
    /// Waits are the interesting exclusion. A wait is not a state change, and
    /// the settle it drives happens anyway on the verbs that are recorded; a
    /// replay on this engine's virtual clock reaches the same quiescent state
    /// without being told to wait for it. On a wall-clock engine that would not
    /// be true, which is why theirs record waits and this does not.
    pub fn is_recorded(self) -> bool {
        match self {
            Verb::Navigate
            | Verb::Scroll
            | Verb::Type
            | Verb::Submit
            | Verb::Click
            // `set_checked` especially: it is the *reason* it exists beside
            // `click`. A click on a checkbox is a toggle and replays to a
            // different state; setting one is idempotent and replays to the
            // same one.
            | Verb::SetChecked
            | Verb::Select
            | Verb::Press => true,

            Verb::Status
            | Verb::Snapshot
            | Verb::Requests
            | Verb::WaitFor
            | Verb::WaitForScript
            | Verb::Extract
            | Verb::Markdown
            | Verb::Env
            | Verb::Script
            | Verb::Structured
            | Verb::Find
            // Handing the page to a human is the one step a replay must never
            // reproduce: there is nobody there to take it.
            | Verb::Login => false,
        }
    }

    /// Whether this verb accepts an optional `url` and goes there before it
    /// reads.
    ///
    /// A round trip an agent does not have to spend. `navigate` then `markdown`
    /// is two turns through a model to answer one question, and the model pays
    /// for the intervening reply in context as well as latency. The read verbs
    /// therefore take the URL directly, and the reply says where it ended up so
    /// a redirect is not silent.
    ///
    /// Only *reads* qualify. Fusing a navigation into `type` or `click` would
    /// mean acting on a page whose refs the caller has never seen, which is the
    /// failure the staleness check exists to prevent — the ref would be
    /// resolved against a reading nobody had read.
    pub fn navigates_first(self) -> bool {
        match self {
            Verb::Snapshot
            | Verb::Markdown
            | Verb::Extract
            | Verb::Structured
            | Verb::Find => true,

            // Already a navigation; a second URL would be ambiguous.
            Verb::Navigate
            // Acts on a ref, which must come from a reading the caller has
            // actually seen.
            | Verb::Type
            | Verb::Submit
            | Verb::Click
            | Verb::SetChecked
            | Verb::Select
            | Verb::Press
            // Reads the session rather than the page, so there would be
            // nothing for a navigation to change.
            | Verb::Status
            | Verb::Login
            | Verb::Requests
            | Verb::Env
            | Verb::Script
            | Verb::Scroll
            // A wait fused with a navigation reads as "go here, then tell me
            // when this appears", which is a useful verb and a different one:
            // the settle it would have to run belongs to the navigation, so
            // the reported outcome would describe the load rather than the
            // wait. Left out until something asks for it by name.
            | Verb::WaitFor
            | Verb::WaitForScript => false,
        }
    }

    /// The verb list, for a caller that named something else.
    pub fn known() -> String {
        Verb::ALL
            .iter()
            .map(|v| v.name())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Why a verb could not be done, in a form an agent can act on.
///
/// Two things travel with every failure and neither is decoration.
///
/// **A code**, so a caller branches without parsing prose. The prose is written
/// for a model and will be reworded; the code is the contract.
///
/// **The recovery**, in the message, because the reader is usually a model
/// deciding what to do next and "no such ref" tells it what happened without
/// telling it what to do instead. Both reference engines this was drawn from
/// converged on the same shape, and the one that did not have it reported every
/// failure as one error code with a free-text string, which nothing can branch
/// on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerbError {
    pub code: Code,
    pub message: String,
}

/// The machine-readable half of a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    /// The verb name is not one this session has.
    UnknownVerb,
    /// A required argument is missing, or is the wrong shape.
    BadRequest,
    /// No snapshot has been served, so a `@ref` names nothing yet.
    NoSnapshot,
    /// The ref is not on this page at all.
    NoSuchRef,
    /// The ref exists, but the page moved since the snapshot that minted it.
    StaleRef,
    /// The ref is on the page and is the wrong kind of thing for this verb.
    WrongRole,
    /// The policy refused it.
    Refused,
    /// LOGIN mode is on and this verb reads the page.
    LoginMode,
    /// The verb needs a script realm and this session has none.
    NoScript,
    /// A wait ran out of budget.
    Timeout,
    /// A selector matched nothing.
    NoMatch,
    /// The engine failed at something that is not the caller's fault.
    Internal,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        match self {
            Code::UnknownVerb => "unknown-verb",
            Code::BadRequest => "bad-request",
            Code::NoSnapshot => "no-snapshot",
            Code::NoSuchRef => "no-such-ref",
            Code::StaleRef => "stale-ref",
            Code::WrongRole => "wrong-role",
            Code::Refused => "refused",
            Code::LoginMode => "login-mode",
            Code::NoScript => "no-script",
            Code::Timeout => "timeout",
            Code::NoMatch => "no-match",
            Code::Internal => "internal",
        }
    }

    /// Whether this is the caller's mistake to fix.
    ///
    /// The split both reference engines make, and it matters for the agent
    /// loop: a selector that matched nothing is something a model can correct
    /// on its next turn, while a policy refusal is not, and reporting the first
    /// the way the second is reported ends the self-correction rather than
    /// prompting it. Callers use this to decide whether a retry is worth
    /// anything.
    pub fn caller_can_fix(self) -> bool {
        match self {
            Code::BadRequest
            | Code::NoSnapshot
            | Code::NoSuchRef
            | Code::StaleRef
            | Code::WrongRole
            | Code::NoMatch
            | Code::UnknownVerb => true,

            Code::Refused
            | Code::LoginMode
            | Code::NoScript
            | Code::Timeout
            | Code::Internal => false,
        }
    }
}

impl VerbError {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        VerbError {
            code,
            message: message.into(),
        }
    }

    /// The reply an agent receives.
    pub fn reply(&self) -> Value {
        json!({
            "ok": false,
            "code": self.code.as_str(),
            "error": self.message,
            "retryable": self.code.caller_can_fix(),
        })
    }

    pub fn unknown_verb(name: &str) -> Self {
        VerbError::new(
            Code::UnknownVerb,
            format!(
                "`{name}` is not a verb this session has. It knows: {}.",
                Verb::known()
            ),
        )
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        VerbError::new(Code::BadRequest, message)
    }

    /// A `@ref` that named nothing, and what to do about it.
    pub fn no_such_ref(reference: &str) -> Self {
        VerbError::new(
            Code::NoSuchRef,
            format!(
                "`{reference}` is not on this page. Take a `snapshot` to get the refs this \
                 page actually has — they are minted per snapshot and a page that changed \
                 has different ones."
            ),
        )
    }

    /// No snapshot has been served yet.
    pub fn no_snapshot(reference: &str) -> Self {
        VerbError::new(
            Code::NoSnapshot,
            format!(
                "this session has not served a snapshot, so `{reference}` names nothing. \
                 Take a `snapshot` first and act on a ref it gave you."
            ),
        )
    }

    /// The ref is stale: the page moved under it.
    ///
    /// The message names what the ref points at *now*, because that is what
    /// tells the reader whether the page merely re-rendered or genuinely
    /// changed, and it is the one piece of evidence the session has that the
    /// agent does not.
    pub fn stale_ref(reference: &str, now: &str) -> Self {
        VerbError::new(
            Code::StaleRef,
            format!(
                "`{reference}` came from a snapshot this page has moved on from: it now names \
                 {now}. Refs are numbered by position in the snapshot that minted them, so \
                 acting on this one would act on a different element than the one you read. \
                 Take a fresh `snapshot` and use its refs."
            ),
        )
    }

    pub fn wrong_role(reference: &str, role: &str, wanted: &str) -> Self {
        VerbError::new(
            Code::WrongRole,
            format!("`{reference}` is a {role}, not {wanted}."),
        )
    }

    /// This verb needs a realm and there is none.
    ///
    /// Named as a routing answer, not as a failure: the caller's next move is
    /// to ask `capabilities` or use the Chromium path, not to retry.
    pub fn no_script(verb: Verb) -> Self {
        VerbError::new(
            Code::NoScript,
            format!(
                "`{}` needs a script realm and this session has none. Start `serve` with \
                 `--script`, or route this page to the chromium engine — `capabilities` \
                 reports which this invocation is.",
                verb.name()
            ),
        )
    }

    /// A selector that matched nothing on the page.
    ///
    /// In-band rather than protocol: a selector the caller can correct is a
    /// different failure from a policy refusal, and reporting the first the way
    /// the second is reported ends a self-correction loop instead of prompting
    /// one.
    pub fn no_match(message: impl Into<String>) -> Self {
        VerbError::new(Code::NoMatch, message)
    }

    pub fn refused(message: impl Into<String>) -> Self {
        VerbError::new(Code::Refused, message)
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_round_trips_and_none_collide() {
        let mut seen = std::collections::HashSet::new();
        for verb in Verb::ALL {
            assert!(
                seen.insert(verb.name()),
                "two verbs answer to {:?}",
                verb.name()
            );
            assert_eq!(Verb::from_name(verb.name()), Some(*verb));
        }
        assert_eq!(Verb::from_name("typewrite"), None);
    }

    #[test]
    fn login_mode_admits_only_the_two_verbs_that_let_it_end() {
        // The rule this type exists for. Every other verb reads the page in
        // some form, and a credential typed into a page the agent can read has
        // been handed to the agent. `status` reports the mode; `login` ends it.
        // Anything else being readable here is a bug, and this assertion is
        // what makes it one rather than a typo nobody notices.
        let readable: Vec<&str> = Verb::ALL
            .iter()
            .filter(|v| v.readable_during_login())
            .map(|v| v.name())
            .collect();
        assert_eq!(readable, vec!["status", "login"]);
    }

    #[test]
    fn only_verbs_that_name_a_ref_ask_for_one() {
        let refs: Vec<&str> = Verb::ALL
            .iter()
            .filter(|v| v.needs_ref())
            .map(|v| v.name())
            .collect();
        assert_eq!(
            refs,
            vec!["type", "submit", "click", "set_checked", "select", "press"]
        );
    }

    #[test]
    fn a_secret_is_resolved_only_where_it_is_handed_to_the_page() {
        // Anywhere else and the value can be read back: a selector lands in an
        // error, a URL lands in the request log, a wait condition lands in a
        // reply. `type` is the one place it goes into the page and stops.
        let substituting: Vec<&str> = Verb::ALL
            .iter()
            .filter(|v| v.substitutes_secrets())
            .map(|v| v.name())
            .collect();
        assert_eq!(substituting, vec!["type"]);
    }

    #[test]
    fn a_failure_carries_a_code_and_says_what_to_do_next() {
        let e = VerbError::stale_ref("@e5", "a link \"Sign out\"");
        let reply = e.reply();
        assert_eq!(reply["ok"], false);
        assert_eq!(reply["code"], "stale-ref");
        assert_eq!(reply["retryable"], true);
        let text = reply["error"].as_str().unwrap();
        assert!(text.contains("snapshot"), "no recovery in {text:?}");
        assert!(text.contains("Sign out"), "does not say what it names now");
    }

    #[test]
    fn a_policy_refusal_is_not_offered_as_retryable() {
        // The split that keeps a self-correction loop from spinning: a model
        // can fix a selector, and cannot fix an allowlist.
        assert!(!Code::Refused.caller_can_fix());
        assert!(!Code::LoginMode.caller_can_fix());
        assert!(Code::NoMatch.caller_can_fix());
        assert!(Code::StaleRef.caller_can_fix());
    }

    #[test]
    fn every_code_has_a_distinct_string() {
        let codes = [
            Code::UnknownVerb,
            Code::BadRequest,
            Code::NoSnapshot,
            Code::NoSuchRef,
            Code::StaleRef,
            Code::WrongRole,
            Code::Refused,
            Code::LoginMode,
            Code::NoScript,
            Code::Timeout,
            Code::NoMatch,
            Code::Internal,
        ];
        let mut seen = std::collections::HashSet::new();
        for code in codes {
            assert!(seen.insert(code.as_str()), "duplicate {:?}", code.as_str());
        }
    }

    #[test]
    fn the_unknown_verb_message_lists_what_is_known() {
        let e = VerbError::unknown_verb("typewrite");
        let text = e.message;
        assert!(text.contains("typewrite"));
        for verb in Verb::ALL {
            assert!(text.contains(verb.name()), "{} missing", verb.name());
        }
    }
}
