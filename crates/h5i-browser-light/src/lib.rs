//! h5i browser light: a lightweight visual browser engine for coding agents.
//!
//! The engine exists for one property Chromium cannot give us from outside.
//! h5i's egress proxy is a CONNECT gate: it sees `CONNECT docs.example.com:443`
//! and nothing more, so a browser receipt can name hosts and little else. CDP's
//! Fetch domain can pause a Chromium request and record it, but that coverage
//! fails *open* — attach races, freshly created targets and workers, event
//! buffer limits and disconnects all leave gaps. Here the engine *is* the HTTP
//! client, so the receipt is not an observation of the network, it is the
//! network. If the receipt cannot be written, the request does not happen.
//!
//! What that buys, concretely:
//!
//! - **Fail-closed by construction.** [`net::Broker`] appends the decision
//!   before the wire and the outcome after it. A sink that refuses to record
//!   is a sink that refuses to fetch (see [`receipt::Sink`]).
//! - **Every hop is a decision.** Redirects are followed manually and each hop
//!   is policy-checked and receipted, so an allowed origin cannot bounce a
//!   request to a denied one.
//! - **No script.** This tier does not evaluate page script at all, so the
//!   commonest delivery channel for injected instructions is absent rather
//!   than filtered. When script arrives (ROADMAP M10 tier 3) it is off by
//!   default and gated by policy before evaluation, never "absent by
//!   construction" — that phrase is reserved for a build with no JS engine in
//!   it.
//!
//! The rendering half is assembled, not written: Blitz owns the DOM and drives
//! Stylo for CSS, and vello_cpu rasterises on the CPU because a box has no GPU.
//! Fidelity is explicitly *not* the goal — the two-engine split in ROADMAP 7.1
//! keeps Chromium for the agent's own dev server, and docs-grade pages are this
//! engine's compatibility bar.

pub mod encoding;
pub mod canvas;
pub mod cookies;
pub mod engine;
pub mod extract;
pub mod fonts;
pub mod markdown;
pub mod net;
pub mod policy;
pub mod receipt;
pub mod replay;
pub mod script;
pub mod selector;
pub mod skill;
pub mod secrets;
pub mod snapshot;
pub mod sse;
pub mod structured;
pub mod stream;
pub mod verbs;
pub mod ws;
pub mod wsclient;

pub use engine::{Page, PageFactory, PageOptions};
pub use policy::{Policy, Verdict};
pub use receipt::{JsonlSink, MemorySink, RequestRecord, Sink};

/// What this engine can and cannot do, answered rather than guessed at.
///
/// h5i reads this instead of inferring capability from a version number: a
/// caller that needs `<video>` should be told so by the engine, and routed to
/// the Chromium path (ROADMAP 7.1), not left to discover a blank frame.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Capabilities {
    pub engine: String,
    pub version: String,
    /// Whether *this process* will run page script.
    ///
    /// Reported from the running configuration rather than from what the binary
    /// is capable of, because the answer h5i routes on is "will this engine run
    /// the page", not "could it". Script is opt-in (`--script`), and ROADMAP
    /// §12.5 is why: turning it on changes the box's threat model, so it is a
    /// decision someone makes rather than a default they inherit.
    pub javascript: bool,
    pub screenshot: bool,
    pub snapshot: bool,
    /// A live view h5i's existing viewers can attach to, frames driven by
    /// change rather than by a clock.
    pub live_view: bool,
    /// Deliberately absent, and owned by the Chromium path instead.
    pub video: bool,
    pub webgl: bool,
    pub downloads: bool,
    /// Fetches are refused unless a receipts sink is accepting writes.
    pub fail_closed_receipts: bool,
}

impl Capabilities {
    pub fn current() -> Self {
        Self::with_script(false)
    }

    /// What this engine can do with script either on or off.
    pub fn with_script(javascript: bool) -> Self {
        Self {
            engine: "h5i-browser-light".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            javascript,
            screenshot: true,
            snapshot: true,
            live_view: true,
            video: false,
            webgl: false,
            downloads: false,
            fail_closed_receipts: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_do_not_overclaim() {
        let caps = Capabilities::current();
        // Every one of these is a promise h5i routes on. Tier 1 has no script
        // engine linked, so claiming otherwise would send the wrong pages here.
        assert!(!caps.javascript);
        assert!(!caps.video);
        assert!(!caps.webgl);
        assert!(caps.screenshot);
        assert!(caps.fail_closed_receipts);
    }
}
