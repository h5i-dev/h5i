//! A session, recorded as something that can be run again.
//!
//! The action log ([`crate::receipt::ActionLog`]) is an *audit* record: every
//! verb written before it runs and again after, failures included, because "no
//! record, no action" is a claim about what was attempted. This artifact holds
//! only the steps that *worked*, in a form that means the same thing later.
//!
//! # Why selectors, and why that reaches into the API
//!
//! A `@ref` is an ordinal: the fifth actionable thing in the reading that
//! minted it. Checking it against that reading (`stream::resolve_ref`) makes it
//! safe, not durable. Replay it tomorrow against a page with one more link near
//! the top and `@e5` is a different element.
//!
//! So a recorded step carries the **verified selector** the snapshot minted
//! beside the ref: the simplest CSS selector whose first match is that element,
//! checked with the matcher the action verbs use ([`crate::selector`]). That is
//! why those verbs take a `selector` as well as a `ref`. **Refs are for
//! reading, selectors are for acting.**
//!
//! # Why a replay is worth having here
//!
//! Both reference engines settle on a wall clock, so replaying their recordings
//! is a re-run with different timing and, on a racing page, a different answer.
//! This engine settles on a virtual clock, so a replay visits the same states in
//! the same order. A recording, its request log, and a replay that lands
//! identically make a session that can be **re-executed and diffed**, which is
//! the browser-side form of what roadmap-history.md §B11.5.16 wants from
//! receipts.
//!
//! # What is deliberately not recorded
//!
//! * **Reads.** A snapshot changes nothing. Reads are how a person or a model
//!   decided what to do next, not part of the doing.
//! * **Steps that failed.** A refusal belongs in the audit log. A script that
//!   replays a failure never reaches the state it was recorded from.
//! * **Steps whose handle cannot survive.** With no verifiable selector the
//!   step is dropped and the drop is *counted*, so a short script is visibly
//!   short rather than quietly wrong.
//! * **Credential values.** A `type` that used `$H5I_SECRET_*` records the
//!   placeholder. A recording is a file, and a file is where a credential must
//!   not end up.

use serde::{Deserialize, Serialize};

/// One thing a replay does.
///
/// Flat rather than an enum-per-verb: the wire form of a verb is already a JSON
/// object, and a step is that object with the handle rewritten. Keeping the
/// shapes the same means a replay sends what the recording saw, rather than a
/// translation of it that could drift.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Step {
    /// The wire name, from [`crate::verbs::Verb::name`].
    pub verb: String,
    /// The durable handle, for the verbs that act on an element.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    /// Where to go, for `navigate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// What to type — the placeholder, never a resolved credential.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// How far to scroll.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<i64>,
    /// The state a checkbox or radio was set to.
    ///
    /// A *state*, not a toggle, which is the whole reason `set_checked` exists
    /// beside `click`: replaying a toggle twice returns to where it started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    /// The option a `<select>` was set to, by value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option: Option<String>,
    /// The key that was pressed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// What the element was called when this was recorded.
    ///
    /// Never used to resolve anything — the selector does that. It is here so a
    /// script is readable by a person, and so a replay that lands on the wrong
    /// element has something to say about it beyond a selector string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub named: Option<String>,
}

impl Step {
    /// The request this step sends.
    pub fn request(&self) -> serde_json::Value {
        let mut object = serde_json::Map::new();
        object.insert("verb".into(), serde_json::json!(self.verb));
        if let Some(selector) = &self.selector {
            object.insert("selector".into(), serde_json::json!(selector));
        }
        if let Some(url) = &self.url {
            object.insert("url".into(), serde_json::json!(url));
        }
        if let Some(text) = &self.text {
            object.insert("text".into(), serde_json::json!(text));
        }
        if let Some(by) = self.by {
            object.insert("by".into(), serde_json::json!(by));
        }
        if let Some(checked) = self.checked {
            object.insert("checked".into(), serde_json::json!(checked));
        }
        if let Some(option) = &self.option {
            object.insert("option".into(), serde_json::json!(option));
        }
        if let Some(key) = &self.key {
            object.insert("key".into(), serde_json::json!(key));
        }
        serde_json::Value::Object(object)
    }

    /// One line, for a person reading a script before running it.
    pub fn render(&self) -> String {
        let target = self
            .selector
            .as_deref()
            .or(self.url.as_deref())
            .unwrap_or("");
        let named = match &self.named {
            Some(name) if !name.is_empty() => format!("  # {name}"),
            _ => String::new(),
        };
        // Whatever this step carries, after the handle. Only one is ever set,
        // because a step is one verb.
        let argument = self
            .text
            .clone()
            .or_else(|| self.option.clone())
            .or_else(|| self.key.clone())
            .or_else(|| self.checked.map(|c| c.to_string()));
        match argument {
            Some(argument) => format!("{} {target} {argument}{named}", self.verb),
            None => format!("{} {target}{named}", self.verb).trim_end().to_string(),
        }
    }
}

/// What a session did, in a form that can be run again.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Recording {
    /// Where the session started, so a replay does not have to be told.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_url: Option<String>,
    pub steps: Vec<Step>,
    /// Actions that happened but could not be recorded durably.
    ///
    /// Counted rather than silently dropped. A replay that is shorter than the
    /// session it came from is a fact the person running it needs, and ROADMAP
    /// §B16.10's rule about silent caps applies to a recording as much as to a
    /// search: bounding coverage without saying so reads as having covered
    /// everything.
    #[serde(default)]
    pub dropped: usize,
}

impl Recording {
    /// Note where this began. Idempotent: only the first navigation counts, and
    /// the rest are steps.
    pub fn start_at(&mut self, url: &str) {
        if self.start_url.is_none() {
            self.start_url = Some(url.to_string());
        }
    }

    pub fn push(&mut self, step: Step) {
        self.steps.push(step);
    }

    /// Record that something happened which cannot be replayed.
    pub fn drop_step(&mut self) {
        self.dropped += 1;
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// The script, as a person would read it.
    pub fn render(&self) -> String {
        let mut out = String::new();
        if let Some(url) = &self.start_url {
            out.push_str(&format!("# recorded from {url}\n"));
        }
        for step in &self.steps {
            out.push_str(&step.render());
            out.push('\n');
        }
        if self.dropped > 0 {
            out.push_str(&format!(
                "# {} action(s) are not in this script: no durable selector could be \
                 verified for what they acted on\n",
                self.dropped
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_sends_the_selector_and_never_a_ref() {
        let step = Step {
            verb: "click".into(),
            selector: Some("#go".into()),
            named: Some("Sign in".into()),
            ..Default::default()
        };
        let request = step.request();
        assert_eq!(request["verb"], "click");
        assert_eq!(request["selector"], "#go");
        assert!(
            request.get("ref").is_none(),
            "an ordinal must never reach a replay: {request:?}"
        );
        // The name is for the reader, not for resolution.
        assert!(request.get("named").is_none(), "{request:?}");
        assert!(step.render().contains("Sign in"));
    }

    #[test]
    fn dropped_steps_are_counted_in_what_a_reader_sees() {
        let mut recording = Recording::default();
        recording.start_at("https://example.com/");
        recording.push(Step {
            verb: "click".into(),
            selector: Some("a.next".into()),
            ..Default::default()
        });
        recording.drop_step();

        let rendered = recording.render();
        assert!(rendered.contains("recorded from https://example.com/"));
        assert!(rendered.contains("click a.next"));
        assert!(
            rendered.contains("1 action(s) are not in this script"),
            "a shorter script has to be visibly shorter:\n{rendered}"
        );
    }

    #[test]
    fn a_recording_round_trips_through_json() {
        let mut recording = Recording::default();
        recording.start_at("https://example.com/");
        recording.push(Step {
            verb: "type".into(),
            selector: Some("input[name=\"user\"]".into()),
            // The placeholder, which is what a recording is allowed to hold.
            text: Some("$H5I_SECRET_PASS".into()),
            ..Default::default()
        });

        let text = serde_json::to_string(&recording).unwrap();
        assert!(
            !text.contains("hunter2"),
            "no resolved credential may reach a file: {text}"
        );
        let back: Recording = serde_json::from_str(&text).unwrap();
        assert_eq!(back, recording);
    }
}
