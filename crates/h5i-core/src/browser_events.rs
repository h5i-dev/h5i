//! The browser terminal's event stream (ROADMAP M11a).
//!
//! One stream, read by three consumers: the console's browser terminal, the
//! terminal viewer (M11b), and the exported receipt. That is the whole point of
//! putting it here rather than inside either viewer — two viewers that each
//! collect their own data are two viewers that disagree, and a disagreement
//! between them is unfalsifiable for whoever is watching.
//!
//! # Two axes, deliberately not collapsed
//!
//! Every event carries **who observed it** and **how complete that observation
//! is**, and those are different questions:
//!
//! * [`Lane`] — host-observed (h5i saw it from outside the box) or box-claimed
//!   (the box said so). The same split [`crate::server`]'s `HOST_OBSERVED_LANES`
//!   already applies to receipts.
//! * [`Grade`] — fail-closed (if it could not be recorded it did not happen) or
//!   best-effort (observed, with known gaps).
//!
//! They are orthogonal, and the interesting case proves it: `h5i-browser-light`
//! writes its request log **inside the box**, so it is box-claimed — and that
//! log is fail-closed by construction, because the engine refuses the fetch when
//! the record cannot be written (`h5i-browser-light`'s `net::Broker`). Chromium's
//! Fetch lane is box-claimed *and* best-effort: attach races and buffer limits
//! leave gaps. A viewer that rendered those two the same way would report
//! coverage it does not have, which is the failure this codebase keeps writing
//! tests against. So the grade travels with the row and the pane renders it.
//!
//! # Correlation is carried, never guessed
//!
//! [`ViewerEvent::caused_by`] is set only where the *source* carries the link:
//! a response row is caused by the request row with its sequence number, a
//! policy refusal is caused by the action that provoked it. Nothing here infers
//! causation from timestamps. Two things that happened close together are two
//! things that happened close together, and a UI that draws an arrow between
//! them on that basis is inventing evidence.
//!
//! # Time
//!
//! [`ViewerEvent::observed_at`] is when **h5i read the record**, not when the
//! box produced it: the request log carries a sequence number and no clock, so
//! an event time would have to be fabricated. Ordering comes from [`EventLog`]'s
//! monotonic `id`, which follows each source's own sequence.

use serde::{Deserialize, Serialize};

use crate::redact::sanitize_display;

/// Longest a single field from the box may be before it is cut. Long enough for
/// a real URL or stack line, short enough that a flood cannot make the console
/// unreadable one row at a time.
const FIELD_CAP: usize = 2048;

/// Who observed an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Lane {
    /// h5i saw it from outside the box: the mediated socket, the egress proxy,
    /// the forward. The box cannot edit these.
    HostObserved,
    /// The box reported it. True unless the box is lying, which is exactly the
    /// thing a reader needs to be able to weigh.
    BoxClaimed,
}

impl Lane {
    pub fn as_str(self) -> &'static str {
        match self {
            Lane::HostObserved => "host-observed",
            Lane::BoxClaimed => "box-claimed",
        }
    }
}

/// How complete the observation behind an event is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Grade {
    /// The record is a precondition of the act: no record, no act. The light
    /// engine's request log is the case that motivated the variant.
    FailClosed,
    /// Observed with known gaps — Chromium's Fetch lane, a drain that ran after
    /// the fact, anything that can miss an event without noticing.
    BestEffort,
}

impl Grade {
    pub fn as_str(self) -> &'static str {
        match self {
            Grade::FailClosed => "fail-closed",
            Grade::BestEffort => "best-effort",
        }
    }
}

/// Why the engine asked for a URL. Mirrors `h5i-browser-light`'s `Initiator`
/// **by value, not by import**: the log is a box-written artifact, so it is
/// parsed as untrusted input rather than deserialized into the producer's own
/// type. An initiator this build does not know becomes [`Initiator::Other`]
/// rather than dropping the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Initiator {
    Navigation,
    Subresource,
    Redirect,
    Other,
}

impl Initiator {
    fn parse(s: &str) -> Self {
        match s {
            "navigation" => Initiator::Navigation,
            "subresource" => Initiator::Subresource,
            "redirect" => Initiator::Redirect,
            _ => Initiator::Other,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Initiator::Navigation => "navigation",
            Initiator::Subresource => "subresource",
            Initiator::Redirect => "redirect",
            Initiator::Other => "other",
        }
    }
}

/// What happened. One variant per pane the browser terminal draws, because a
/// pane with no event kind behind it is a layout waiting for a source — the
/// mistake M11 recorded when it did not build a network pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum EventKind {
    /// The page the engine is showing.
    Navigated { url: String },
    /// A request the engine decided on. `allowed` is the policy decision, and
    /// it is separate from `status`: a denied request never reaches the wire, so
    /// it has a decision and no status at all.
    Request {
        seq: u64,
        method: String,
        url: String,
        initiator: Initiator,
        allowed: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        denied_reason: Option<String>,
    },
    /// The outcome of a request: what came back, or what ended it.
    Response {
        seq: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// A console message or an uncaught page error.
    Console { level: ConsoleLevel, text: String },
    /// A verb the agent sent through the mediated socket.
    AgentAction { action: String, forwarded: bool },
    /// h5i refused something, and why. Always its own event rather than a flag
    /// on the action: a refusal is the thing a reviewer scans for, and a flag
    /// buried in a forwarded row is not scannable.
    PolicyVerdict { subject: String, reason: String },
    /// A source restarted: the file h5i was reading got shorter, which only
    /// happens when a new run cleared the box's `/tmp` and a fresh session
    /// began writing.
    ///
    /// Host-observed, because h5i noticed it about the box rather than being
    /// told. It exists so a restart **looks like a restart**: without it the
    /// reader silently resumes numbering and a viewer holding an old cursor
    /// swallows the head of the new session, reporting a quiet page where there
    /// was a busy one. That is the failure this whole module is arranged to
    /// prevent, and it was live in the first version of this reader.
    SessionReset { source: String },
}

/// Console severity. `log`/`info` chatter is not evidence and is not carried;
/// this mirrors [`crate::receipt::BrowserEvidence`]'s rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConsoleLevel {
    Warning,
    Error,
    /// An uncaught exception or a navigation that failed — the page itself
    /// went wrong, rather than something it printed.
    PageError,
}

impl ConsoleLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            ConsoleLevel::Warning => "warning",
            ConsoleLevel::Error => "error",
            ConsoleLevel::PageError => "page-error",
        }
    }
}

/// What a draft event *is*, so a later draft can point at it. Resolved to real
/// event ids by [`EventLog::extend`]; never serialized, because outside the log
/// a correlation key means nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// The engine's own request sequence number.
    Request(u64),
    /// The nth action in a batch of mediated records.
    Action(usize),
}

/// An event before it has an id. Ingest produces these; the log assigns
/// identity and resolves correlation.
#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    pub kind: EventKind,
    pub lane: Lane,
    pub grade: Grade,
    /// The key this event answers to, if anything can point at it.
    pub key: Option<Key>,
    /// The key of the event that caused this one.
    pub caused_by: Option<Key>,
}

impl Draft {
    fn new(kind: EventKind, lane: Lane, grade: Grade) -> Self {
        Self {
            kind,
            lane,
            grade,
            key: None,
            caused_by: None,
        }
    }
}

/// One event, as a viewer sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewerEvent {
    /// Monotonic within a log. The viewer's cursor, and the only ordering
    /// anything should rely on.
    pub id: u64,
    /// When **h5i read this**, RFC3339. Not when the box produced it — see the
    /// module docs.
    pub observed_at: String,
    pub lane: Lane,
    pub grade: Grade,
    /// The id of the event that caused this one, when the source carried the
    /// link. Never inferred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caused_by: Option<u64>,
    #[serde(flatten)]
    pub kind: EventKind,
}

/// A bounded, append-only view of one box's browser session.
///
/// Bounded because a page that loads a thousand subresources must not become a
/// thousand rows the console has to hold forever, and **counted** because a cap
/// that drops silently reports a quiet session where there was a loud one.
#[derive(Debug, Clone)]
pub struct EventLog {
    events: std::collections::VecDeque<ViewerEvent>,
    /// Correlation keys to the ids that answer them, bounded like the log. A
    /// long session can therefore hold a `caused_by` naming an event the cap has
    /// already discarded; a viewer that cannot find the id renders the row
    /// without a link rather than inventing one. Keys are matched newest-first,
    /// so a sequence number reused by a later session resolves to the recent
    /// event rather than the stale one.
    keys: Vec<(Key, u64)>,
    capacity: usize,
    next_id: u64,
    dropped: u64,
}

impl EventLog {
    pub fn new(capacity: usize) -> Self {
        Self {
            events: std::collections::VecDeque::new(),
            keys: Vec::new(),
            // A zero-capacity log would drop everything and report a clean
            // session, so the floor is one.
            capacity: capacity.max(1),
            next_id: 1,
            dropped: 0,
        }
    }

    /// Events after `cursor`, oldest first. `0` means "everything held".
    pub fn since(&self, cursor: u64) -> Vec<&ViewerEvent> {
        self.events.iter().filter(|e| e.id > cursor).collect()
    }

    /// The newest id, or 0 when nothing has been appended. A viewer polls with
    /// this and gets only what it has not seen.
    pub fn cursor(&self) -> u64 {
        self.events.back().map(|e| e.id).unwrap_or(0)
    }

    /// How many events the cap discarded. Rendered, never hidden.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Append a batch, resolving correlation **within the batch and against
    /// what is already held**.
    ///
    /// Two-pass rather than one: a draft may be caused by another draft in the
    /// same batch that has not been given an id yet (a response and its request
    /// arrive from the same file read), so keys are resolved as ids are
    /// assigned, in order.
    pub fn extend(&mut self, drafts: impl IntoIterator<Item = Draft>, observed_at: &str) {
        for draft in drafts {
            let id = self.next_id;
            self.next_id += 1;

            let caused_by = draft.caused_by.and_then(|k| self.find_key(k));
            if let Some(key) = draft.key {
                self.keys.push((key, id));
                // The key table is bounded like the log itself: a session that
                // makes a million requests must not grow an entry per request
                // that nothing will ever point at again.
                if self.keys.len() > self.capacity {
                    self.keys.remove(0);
                }
            }

            self.events.push_back(ViewerEvent {
                id,
                observed_at: observed_at.to_string(),
                lane: draft.lane,
                grade: draft.grade,
                caused_by,
                kind: draft.kind,
            });
            if self.events.len() > self.capacity {
                self.events.pop_front();
                self.dropped += 1;
            }
        }
    }

    fn find_key(&self, key: Key) -> Option<u64> {
        self.keys
            .iter()
            .rev()
            .find(|(k, _)| *k == key)
            .map(|(_, id)| *id)
    }
}

// ── ingest ───────────────────────────────────────────────────────────────────
//
// Pure functions from a source's bytes to drafts, so every one of them is
// testable without a box, a socket, or a browser.

/// Trim a box-supplied string to something a pane can hold, and strip the
/// control and bidi characters that would otherwise repaint the viewer's own
/// chrome. The web console renders text nodes rather than markup, so this is
/// defence in depth there — and the *only* defence for the terminal viewer
/// reading the same stream (M11b), which is why it happens here at ingest and
/// not in either renderer.
fn clean(s: &str) -> String {
    let mut out = sanitize_display(s);
    if out.chars().count() > FIELD_CAP {
        out = out.chars().take(FIELD_CAP).collect::<String>() + "…";
    }
    out
}

/// Parse `h5i-browser-light`'s request log (`H5I_BROWSER_RECEIPTS`, one JSON
/// object per line) into request and response events.
///
/// **Box-claimed and fail-closed.** The file is written inside the box, so a box
/// that wanted to lie could; and the engine will not fetch what it cannot
/// record, so within that trust boundary there is no request missing from it.
/// Both facts travel with every row.
///
/// Defensive throughout, because this is untrusted input from a box that may be
/// mid-write: a line that is not JSON, an object missing its sequence number, or
/// a phase this build does not know is skipped rather than failing the read. A
/// half-written trailing line is the ordinary case, not an error — the engine
/// appends while the console polls.
pub fn ingest_request_log(text: &str) -> Vec<Draft> {
    let mut drafts = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(seq) = v.get("seq").and_then(serde_json::Value::as_u64) else {
            continue;
        };
        let phase = v.get("phase").and_then(serde_json::Value::as_str).unwrap_or("");

        match phase {
            "request" => {
                let allowed = v
                    .get("allowed")
                    .and_then(serde_json::Value::as_bool)
                    // A row whose decision cannot be read is not evidence of
                    // permission. Fail closed here too: it renders as denied.
                    .unwrap_or(false);
                let denied_reason = v
                    .get("denied_reason")
                    .and_then(serde_json::Value::as_str)
                    .map(clean);
                let mut draft = Draft::new(
                    EventKind::Request {
                        seq,
                        method: clean(
                            v.get("method").and_then(serde_json::Value::as_str).unwrap_or("GET"),
                        ),
                        url: clean(
                            v.get("url").and_then(serde_json::Value::as_str).unwrap_or(""),
                        ),
                        initiator: Initiator::parse(
                            v.get("initiator")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or(""),
                        ),
                        allowed,
                        denied_reason: denied_reason.clone(),
                    },
                    Lane::BoxClaimed,
                    Grade::FailClosed,
                );
                draft.key = Some(Key::Request(seq));
                drafts.push(draft);

                // A refusal is also a policy event, so it appears in the pane a
                // reviewer scans instead of only in the network table.
                if !allowed {
                    let mut verdict = Draft::new(
                        EventKind::PolicyVerdict {
                            subject: clean(
                                v.get("url").and_then(serde_json::Value::as_str).unwrap_or(""),
                            ),
                            reason: denied_reason
                                .unwrap_or_else(|| "denied by the engine's policy".to_string()),
                        },
                        Lane::BoxClaimed,
                        Grade::FailClosed,
                    );
                    verdict.caused_by = Some(Key::Request(seq));
                    drafts.push(verdict);
                }
            }
            "response" => {
                let mut draft = Draft::new(
                    EventKind::Response {
                        seq,
                        status: v
                            .get("status")
                            .and_then(serde_json::Value::as_u64)
                            .and_then(|s| u16::try_from(s).ok()),
                        bytes: v.get("bytes").and_then(serde_json::Value::as_u64),
                        duration_ms: v.get("duration_ms").and_then(serde_json::Value::as_u64),
                        error: v.get("error").and_then(serde_json::Value::as_str).map(clean),
                    },
                    Lane::BoxClaimed,
                    Grade::FailClosed,
                );
                // The one correlation the source genuinely carries.
                draft.caused_by = Some(Key::Request(seq));
                drafts.push(draft);
            }
            _ => continue,
        }
    }
    drafts
}

/// Turn the mediator's records into agent-action and policy events.
///
/// **Host-observed.** h5i sat on the daemon's control socket and watched each
/// verb go past ([`crate::browser_proxy`]), so unlike everything else here these
/// rows do not depend on the box's word. Best-effort as coverage, though, and
/// for a structural reason worth keeping in front of the reader: the mediator is
/// enforcement against an agent following the documented path, not containment
/// against one that goes looking for the daemon's own socket.
pub fn ingest_actions(actions: &[crate::browser_proxy::ActionRecord]) -> Vec<Draft> {
    let mut drafts = Vec::new();
    for (index, record) in actions.iter().enumerate() {
        let mut action = Draft::new(
            EventKind::AgentAction {
                action: clean(&record.action),
                forwarded: record.forwarded,
            },
            Lane::HostObserved,
            Grade::BestEffort,
        );
        action.key = Some(Key::Action(index));
        drafts.push(action);

        if let Some(why) = &record.refused_because {
            let mut verdict = Draft::new(
                EventKind::PolicyVerdict {
                    subject: clean(&record.action),
                    reason: clean(why),
                },
                Lane::HostObserved,
                Grade::BestEffort,
            );
            verdict.caused_by = Some(Key::Action(index));
            drafts.push(verdict);
        }
    }
    drafts
}

/// Turn a run's drained page evidence into console events.
///
/// **Box-claimed and best-effort**, both by construction: the drain runs after
/// the fact and asks the page what it remembers, so anything the page dropped
/// before the drain reached it is gone. [`crate::receipt::BrowserEvidence`]'s
/// own `truncated` flag is surfaced as an event rather than a field, so a flood
/// is visible in the pane a human is already reading.
pub fn ingest_evidence(evidence: &crate::receipt::BrowserEvidence) -> Vec<Draft> {
    let mut drafts = Vec::new();
    for line in &evidence.console {
        drafts.push(Draft::new(
            EventKind::Console {
                level: ConsoleLevel::Error,
                text: clean(line),
            },
            Lane::BoxClaimed,
            Grade::BestEffort,
        ));
    }
    for line in &evidence.errors {
        drafts.push(Draft::new(
            EventKind::Console {
                level: ConsoleLevel::PageError,
                text: clean(line),
            },
            Lane::BoxClaimed,
            Grade::BestEffort,
        ));
    }
    for line in &evidence.failed_requests {
        drafts.push(Draft::new(
            EventKind::Console {
                level: ConsoleLevel::Warning,
                text: clean(line),
            },
            Lane::BoxClaimed,
            Grade::BestEffort,
        ));
    }
    if evidence.truncated {
        drafts.push(Draft::new(
            EventKind::Console {
                level: ConsoleLevel::Warning,
                text: "h5i: the page evidence was capped, so this list is not complete".into(),
            },
            Lane::HostObserved,
            Grade::BestEffort,
        ));
    }
    drafts
}

/// Parse the mediator's host-side action log (`browser-actions.jsonl`, written
/// by [`crate::browser_proxy::record_actions`]). A line that will not parse is
/// skipped, for the same reason the request log skips one.
pub fn ingest_actions_log(text: &str) -> Vec<Draft> {
    let records: Vec<crate::browser_proxy::ActionRecord> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    ingest_actions(&records)
}

/// What one box's sources have already given up, so the next read can ask only
/// for what is new.
///
/// Held by the console across polls rather than rebuilt per request, and the
/// reason is a defect the first version of this reader shipped with. It
/// re-parsed every source on every poll and numbered events from 1 each time,
/// which is stable only while the files grow by appending — and they do not.
/// **Every run clears the box's private `/tmp`** (`prepare_private_tmp`), so a
/// second browser run starts the request log over at zero bytes, the numbering
/// restarts with it, and a console tab holding a cursor from the first run
/// silently drops the head of the second. A viewer under-reporting a session is
/// exactly what the lane and grade fields exist to prevent elsewhere, so it does
/// not get to happen here through arithmetic.
///
/// Holding the position turns that into its opposite: ids never restart, a
/// shortened file is *detected*, and the restart is emitted as
/// [`EventKind::SessionReset`] — a row a human can see. It also stops the
/// console re-reading and re-parsing whole files once a second.
#[derive(Debug)]
pub struct BoxStream {
    log: EventLog,
    /// Byte offset just past the last **complete** line consumed. A trailing
    /// partial line is deliberately not consumed: the engine appends while this
    /// reads, so a half-written line is the ordinary case and re-reading it next
    /// poll is how it gets picked up whole.
    actions_at: u64,
    requests_at: u64,
    /// Receipt ids already folded in. Receipts are immutable once written, so
    /// identity is enough and no offset is needed.
    receipts_seen: std::collections::HashSet<String>,
}

impl BoxStream {
    pub fn new(capacity: usize) -> Self {
        Self {
            log: EventLog::new(capacity),
            actions_at: 0,
            requests_at: 0,
            receipts_seen: std::collections::HashSet::new(),
        }
    }

    pub fn log(&self) -> &EventLog {
        &self.log
    }

    /// Fold in whatever the sources have produced since the last call.
    ///
    /// Three sources, read in a fixed order — mediated actions, then the
    /// engine's request log, then the page evidence carried on receipts.
    /// **That order is the read order, not a timeline**: the sources share no
    /// clock (the request log carries a sequence number and no timestamp), so
    /// interleaving them by time would mean inventing one. Within a source the
    /// order is the source's own, which is the ordering that means something.
    ///
    /// Best effort by design: a box that has never opened a browser has none of
    /// these files, and that is an empty stream rather than an error.
    pub fn poll(&mut self, h5i_root: &std::path::Path, m: &crate::env::EnvManifest) {
        let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
        let env_dir = m.dir(h5i_root);

        let actions = crate::browser_proxy::actions_log(&env_dir);
        if let Some(growth) = grown(&actions, &mut self.actions_at) {
            if growth.restarted {
                self.log.extend(reset_draft("mediated actions"), &at);
            }
            self.log.extend(ingest_actions_log(&growth.text), &at);
        }

        if let Some(path) = crate::env::browser_request_log(h5i_root, m) {
            if let Some(growth) = grown(&path, &mut self.requests_at) {
                if growth.restarted {
                    self.log.extend(reset_draft("the engine's request log"), &at);
                }
                self.log.extend(ingest_request_log(&growth.text), &at);
            }
        }

        for record in crate::receipt::list(&env_dir).unwrap_or_default() {
            if !self.receipts_seen.insert(record.id.clone()) {
                continue;
            }
            if let Some(evidence) = &record.browser {
                self.log.extend(ingest_evidence(evidence), &at);
            }
        }
    }
}

fn reset_draft(source: &str) -> Vec<Draft> {
    vec![Draft::new(
        EventKind::SessionReset {
            source: source.to_string(),
        },
        // h5i noticed this about the box; the box did not report it.
        Lane::HostObserved,
        Grade::BestEffort,
    )]
}

/// What a source has grown by.
struct Growth {
    /// Complete lines only.
    text: String,
    /// The file is shorter than what was already consumed, so it was replaced
    /// rather than appended to.
    restarted: bool,
}

/// Read the complete lines `path` has grown by since `*offset`, advancing it.
///
/// `None` when there is nothing new to fold in — no file, no growth, or growth
/// that does not yet contain a line terminator.
///
/// Reads bytes rather than a `String` and cuts at the last newline before
/// decoding: a partial write can split a multi-byte character, and decoding
/// first would either fail or silently corrupt the tail. Cutting at a newline
/// guarantees whole lines, which are whole characters.
fn grown(path: &std::path::Path, offset: &mut u64) -> Option<Growth> {
    use std::io::{Read, Seek, SeekFrom};

    let Ok(meta) = std::fs::metadata(path) else {
        // A file that was there and is not any more is a restart. Report it
        // once — the next poll starts from zero and picks up whatever replaces
        // it — rather than staying silent about a session that ended.
        if *offset > 0 {
            *offset = 0;
            return Some(Growth {
                text: String::new(),
                restarted: true,
            });
        }
        return None;
    };

    let len = meta.len();
    let restarted = len < *offset;
    if restarted {
        *offset = 0;
    }
    if len == *offset {
        // Nothing new. A bare restart with an empty replacement still has to be
        // announced, or the row that explains the renumbering never appears.
        return restarted.then(|| Growth {
            text: String::new(),
            restarted,
        });
    }

    let mut file = std::fs::File::open(path).ok()?;
    file.seek(SeekFrom::Start(*offset)).ok()?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).ok()?;

    // Stop at the last newline; anything after it is a line still being
    // written, and it stays unconsumed until it is whole.
    let complete = match buf.iter().rposition(|b| *b == b'\n') {
        Some(i) => i + 1,
        None => {
            return restarted.then(|| Growth {
                text: String::new(),
                restarted,
            })
        }
    };
    *offset += complete as u64;

    Some(Growth {
        text: String::from_utf8_lossy(&buf[..complete]).into_owned(),
        restarted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TS: &str = "2026-08-08T12:00:00Z";

    fn log_with(drafts: Vec<Draft>) -> EventLog {
        let mut log = EventLog::new(64);
        log.extend(drafts, TS);
        log
    }

    #[test]
    fn a_response_points_at_the_request_it_answers() {
        // The one correlation the request log genuinely carries. Everything the
        // browser terminal shows when an action is selected hangs off this, so
        // it is the property to pin first.
        let log = log_with(ingest_request_log(
            r#"{"seq":7,"phase":"request","initiator":"navigation","method":"GET","url":"https://docs.example/","allowed":true}
{"seq":7,"phase":"response","initiator":"navigation","method":"GET","url":"https://docs.example/","allowed":true,"status":200,"bytes":4096,"duration_ms":31}"#,
        ));

        let events = log.since(0);
        assert_eq!(events.len(), 2, "{events:?}");
        let request_id = events[0].id;
        assert_eq!(
            events[1].caused_by,
            Some(request_id),
            "the response must name its request"
        );
    }

    #[test]
    fn a_denied_request_becomes_its_own_policy_event() {
        // A refusal buried as a flag on a network row is not scannable, and the
        // refusals are the rows a reviewer is looking for.
        let log = log_with(ingest_request_log(
            r#"{"seq":1,"phase":"request","initiator":"subresource","method":"GET","url":"https://tracker.example/x.js","allowed":false,"denied_reason":"host not in the allowlist"}"#,
        ));

        let events = log.since(0);
        assert_eq!(events.len(), 2, "a request and a verdict: {events:?}");
        let EventKind::PolicyVerdict { subject, reason } = &events[1].kind else {
            panic!("expected a verdict, got {:?}", events[1].kind);
        };
        assert_eq!(subject, "https://tracker.example/x.js");
        assert_eq!(reason, "host not in the allowlist");
        assert_eq!(events[1].caused_by, Some(events[0].id));
    }

    #[test]
    fn an_unreadable_decision_renders_as_denied_not_as_allowed() {
        // Fail closed on a malformed row too: reading a missing `allowed` as
        // permission would let a corrupted log report a clean session.
        let log = log_with(ingest_request_log(
            r#"{"seq":2,"phase":"request","method":"GET","url":"https://x.example/"}"#,
        ));
        let events = log.since(0);
        let EventKind::Request { allowed, .. } = &events[0].kind else {
            panic!("expected a request");
        };
        assert!(!allowed, "a row with no decision must not read as allowed");
    }

    #[test]
    fn a_half_written_trailing_line_is_ordinary_not_an_error() {
        // The engine appends while the console polls, so this is the common
        // case. Losing the whole read because of it would blank the pane.
        let log = log_with(ingest_request_log(
            "{\"seq\":1,\"phase\":\"request\",\"method\":\"GET\",\"url\":\"https://a.example/\",\"allowed\":true}\n{\"seq\":2,\"phas",
        ));
        assert_eq!(log.len(), 1, "the complete line survives the broken one");
    }

    #[test]
    fn the_two_axes_stay_separate() {
        // The case that motivated having both: the light engine's log is
        // box-claimed *and* fail-closed. Collapsing them into one "trusted"
        // flag would either overstate the box or understate the engine.
        let network = log_with(ingest_request_log(
            r#"{"seq":1,"phase":"request","method":"GET","url":"https://a.example/","allowed":true}"#,
        ));
        let e = network.since(0)[0];
        assert_eq!(e.lane, Lane::BoxClaimed);
        assert_eq!(e.grade, Grade::FailClosed);

        let mediated = log_with(ingest_actions(&[crate::browser_proxy::ActionRecord {
            action: "click".into(),
            forwarded: true,
            refused_because: None,
        }]));
        let a = mediated.since(0)[0];
        assert_eq!(a.lane, Lane::HostObserved, "the mediator watched this itself");
        assert_eq!(a.grade, Grade::BestEffort, "and it is not containment");
    }

    #[test]
    fn a_refused_verb_carries_the_reason_and_names_its_action() {
        let log = log_with(ingest_actions(&[crate::browser_proxy::ActionRecord {
            action: "evaluate".into(),
            forwarded: false,
            refused_because: Some("denied by policy".into()),
        }]));
        let events = log.since(0);
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].caused_by, Some(events[0].id));
        assert_eq!(events[1].lane, Lane::HostObserved);
    }

    #[test]
    fn box_text_cannot_repaint_the_viewer() {
        // The terminal viewer (M11b) reads this same stream and writes straight
        // to a PTY, so an escape sequence surviving ingest is a real escape.
        let log = log_with(ingest_request_log(
            "{\"seq\":1,\"phase\":\"request\",\"method\":\"GET\",\"url\":\"https://a.example/\\u001b[2Jgone\",\"allowed\":true}",
        ));
        let EventKind::Request { url, .. } = &log.since(0)[0].kind else {
            panic!("expected a request");
        };
        assert!(!url.contains('\u{1b}'), "{url}");
    }

    #[test]
    fn a_long_field_is_cut_rather_than_carried() {
        let long = "x".repeat(FIELD_CAP * 2);
        let line = format!(
            r#"{{"seq":1,"phase":"request","method":"GET","url":"https://a.example/{long}","allowed":true}}"#
        );
        let log = log_with(ingest_request_log(&line));
        let EventKind::Request { url, .. } = &log.since(0)[0].kind else {
            panic!("expected a request");
        };
        assert!(url.chars().count() <= FIELD_CAP + 1, "{}", url.len());
    }

    #[test]
    fn the_cap_counts_what_it_drops() {
        // A cap that drops silently reports a quiet session where there was a
        // loud one — the same rule BrowserEvidence's `truncated` flag follows.
        let mut log = EventLog::new(2);
        let drafts: Vec<Draft> = (0..5)
            .map(|i| {
                Draft::new(
                    EventKind::Navigated {
                        url: format!("https://a.example/{i}"),
                    },
                    Lane::HostObserved,
                    Grade::BestEffort,
                )
            })
            .collect();
        log.extend(drafts, TS);

        assert_eq!(log.len(), 2);
        assert_eq!(log.dropped(), 3, "the drops are counted, not hidden");
    }

    #[test]
    fn a_cursor_returns_only_what_the_viewer_has_not_seen() {
        let mut log = EventLog::new(64);
        log.extend(ingest_evidence(&crate::receipt::BrowserEvidence {
            errors: vec!["TypeError at App.tsx:42".into()],
            ..Default::default()
        }), TS);
        let first = log.cursor();
        assert!(first > 0);
        assert!(log.since(first).is_empty(), "nothing new yet");

        log.extend(ingest_evidence(&crate::receipt::BrowserEvidence {
            errors: vec!["second".into()],
            ..Default::default()
        }), TS);
        let fresh = log.since(first);
        assert_eq!(fresh.len(), 1, "only the new one");
    }

    #[test]
    fn capped_evidence_says_so_in_the_pane() {
        let log = log_with(ingest_evidence(&crate::receipt::BrowserEvidence {
            console: vec!["warn".into()],
            truncated: true,
            ..Default::default()
        }));
        let events = log.since(0);
        assert_eq!(events.len(), 2);
        let EventKind::Console { text, .. } = &events[1].kind else {
            panic!("expected a console row");
        };
        assert!(text.contains("capped"), "{text}");
        assert_eq!(
            events[1].lane,
            Lane::HostObserved,
            "h5i is the one saying the list is short, not the box"
        );
    }

    #[test]
    fn the_mediator_writes_actions_this_can_read_back() {
        // The round trip that keeps the actions pane honest. The receipt
        // carries these as rendered text; the sidecar carries them as data, and
        // if the two ends of that ever drift the pane goes quietly empty rather
        // than failing, which is why this is pinned rather than assumed.
        let td = tempfile::TempDir::new().unwrap();
        let written = vec![
            crate::browser_proxy::ActionRecord {
                action: "click".into(),
                forwarded: true,
                refused_because: None,
            },
            crate::browser_proxy::ActionRecord {
                action: "evaluate".into(),
                forwarded: false,
                refused_because: Some("denied by policy".into()),
            },
        ];
        crate::browser_proxy::record_actions(td.path(), "env/human/b", "digest", &written);

        let text =
            std::fs::read_to_string(crate::browser_proxy::actions_log(td.path())).unwrap();
        let log = log_with(ingest_actions_log(&text));
        let events = log.since(0);

        // Two actions, and a verdict for the refused one.
        assert_eq!(events.len(), 3, "{events:?}");
        assert!(events.iter().all(|e| e.lane == Lane::HostObserved));
        let EventKind::AgentAction { action, forwarded } = &events[0].kind else {
            panic!("expected an action, got {:?}", events[0].kind);
        };
        assert_eq!(action, "click");
        assert!(forwarded);
        assert!(matches!(events[2].kind, EventKind::PolicyVerdict { .. }));
    }

    #[test]
    fn a_box_that_never_browsed_has_an_empty_stream_not_an_error() {
        // Every source is absent for a box whose agent never opened anything,
        // and that has to read as nothing happened rather than as a failure.
        assert!(log_with(ingest_actions_log("")).is_empty());
        assert!(log_with(ingest_request_log("")).is_empty());
    }

    // ── the incremental reader ───────────────────────────────────────────────
    //
    // Driven through `grown` against real files, because the whole point of it
    // is what the filesystem does to a reader: partial writes, truncation, and
    // a file that disappears between polls.

    fn write(path: &std::path::Path, text: &str) {
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn a_reader_takes_only_complete_lines_and_comes_back_for_the_rest() {
        // The engine appends while the console polls, so a trailing partial
        // line is the ordinary case. Consuming it would parse half a record and
        // then never see its other half.
        let td = tempfile::TempDir::new().unwrap();
        let path = td.path().join("log.jsonl");
        let mut at = 0u64;

        write(&path, "{\"a\":1}\n{\"b\":2");
        let first = grown(&path, &mut at).expect("something to read");
        assert_eq!(first.text, "{\"a\":1}\n", "only the whole line");
        assert!(!first.restarted);

        // Nothing new until the partial line is finished.
        assert!(grown(&path, &mut at).is_none());

        write(&path, "{\"a\":1}\n{\"b\":2}\n");
        let second = grown(&path, &mut at).expect("the completed line");
        assert_eq!(second.text, "{\"b\":2}\n", "and it is not re-read");
    }

    #[test]
    fn a_cleared_source_reads_as_a_restart_not_as_silence() {
        // THE defect this reader exists to fix. Every run clears the box's
        // private /tmp, so the request log starts over; a reader that just
        // resumed would renumber from the start and a viewer holding a cursor
        // would drop the new session's opening events on the floor.
        let td = tempfile::TempDir::new().unwrap();
        let path = td.path().join("log.jsonl");
        let mut at = 0u64;

        write(&path, "{\"a\":1}\n{\"b\":2}\n");
        grown(&path, &mut at).expect("the first session");
        assert!(at > 0);

        // A new run: shorter file, different content.
        write(&path, "{\"c\":3}\n");
        let after = grown(&path, &mut at).expect("the new session");
        assert!(after.restarted, "the shortening must be noticed");
        assert_eq!(after.text, "{\"c\":3}\n", "and read from the top");
    }

    #[test]
    fn a_source_that_disappears_is_announced_once() {
        let td = tempfile::TempDir::new().unwrap();
        let path = td.path().join("log.jsonl");
        let mut at = 0u64;

        write(&path, "{\"a\":1}\n");
        grown(&path, &mut at).expect("the first read");
        std::fs::remove_file(&path).unwrap();

        let gone = grown(&path, &mut at).expect("the disappearance is news");
        assert!(gone.restarted);
        // ...and only once: a box with no browser must not emit a reset per
        // second for the rest of the console's life.
        assert!(grown(&path, &mut at).is_none(), "silence after the first");
    }

    #[test]
    fn ids_never_restart_so_an_open_viewer_keeps_its_place() {
        // The property a viewer's cursor depends on, stated as a test: across a
        // session boundary the numbering keeps climbing, and the boundary is
        // itself an event rather than a gap in the sequence.
        let mut log = EventLog::new(64);
        log.extend(
            ingest_request_log("{\"seq\":0,\"phase\":\"request\",\"method\":\"GET\",\"url\":\"https://a.example/\",\"allowed\":true}"),
            TS,
        );
        let after_first = log.cursor();

        log.extend(reset_draft("the engine's request log"), TS);
        log.extend(
            ingest_request_log("{\"seq\":0,\"phase\":\"request\",\"method\":\"GET\",\"url\":\"https://b.example/\",\"allowed\":true}"),
            TS,
        );

        let fresh = log.since(after_first);
        assert!(
            fresh.iter().any(|e| matches!(e.kind, EventKind::SessionReset { .. })),
            "the restart is visible: {fresh:?}"
        );
        assert!(
            fresh.iter().all(|e| e.id > after_first),
            "and everything after it is new to a viewer holding that cursor"
        );
        // The reset is h5i's observation about the box, not the box's claim.
        let reset = fresh
            .iter()
            .find(|e| matches!(e.kind, EventKind::SessionReset { .. }))
            .unwrap();
        assert_eq!(reset.lane, Lane::HostObserved);
    }

    #[test]
    fn a_reused_sequence_number_correlates_to_the_current_session() {
        // Sequence numbers restart at 0 with the engine, so the same key turns
        // up again after a reset. Newest-first resolution means a response is
        // matched to its own session's request rather than to a stale one that
        // happens to share a number.
        let mut log = EventLog::new(64);
        log.extend(
            ingest_request_log("{\"seq\":0,\"phase\":\"request\",\"method\":\"GET\",\"url\":\"https://old.example/\",\"allowed\":true}"),
            TS,
        );
        let stale = log.cursor();
        log.extend(
            ingest_request_log(
                "{\"seq\":0,\"phase\":\"request\",\"method\":\"GET\",\"url\":\"https://new.example/\",\"allowed\":true}\n{\"seq\":0,\"phase\":\"response\",\"status\":200}",
            ),
            TS,
        );

        let events = log.since(stale);
        let response = events
            .iter()
            .find(|e| matches!(e.kind, EventKind::Response { .. }))
            .expect("a response");
        let fresh_request = events
            .iter()
            .find(|e| matches!(e.kind, EventKind::Request { .. }))
            .expect("the new request");
        assert_eq!(
            response.caused_by,
            Some(fresh_request.id),
            "not the stale seq 0 from before the restart"
        );
    }

    #[test]
    fn an_event_serializes_flat_with_both_axes_on_it() {
        // The wire shape the console and any future consumer read. Pinned
        // because a renamed field is a silently empty pane.
        let log = log_with(ingest_actions(&[crate::browser_proxy::ActionRecord {
            action: "click".into(),
            forwarded: true,
            refused_because: None,
        }]));
        let json = serde_json::to_value(log.since(0)[0]).unwrap();
        assert_eq!(json["kind"], "agent-action");
        assert_eq!(json["lane"], "host-observed");
        assert_eq!(json["grade"], "best-effort");
        assert_eq!(json["action"], "click");
        assert!(json.get("caused_by").is_none(), "absent, not null: {json}");
    }
}
