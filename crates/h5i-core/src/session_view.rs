//! What the console shows about browser sessions: the account, and which of
//! them wants a human ([`attention`], from evidence rather than a score).
//!
//! Nothing here reads a stored message. That store holds bodies, cookies and
//! `Authorization` in full, so reading it stays a command someone types.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::browser_session as bs;

/// How much of a ledger the console folds on one poll. Smaller than recon's own
/// cap: this runs every few seconds, for every session.
pub const MAX_LEDGER_BYTES: u64 = 4 * 1024 * 1024;

/// The most fetches one detail view returns, newest last. Each is two log
/// lines, so the wire carries twice this many records.
pub const MAX_REQUESTS_SHOWN: usize = 1000;

/// The most verbs one detail view returns, newest last.
pub const MAX_ACTIONS_SHOWN: usize = 500;

/// How much of a findings log the console folds. Findings are short; a log
/// past this is not one a screen can show anyway.
pub const MAX_FINDINGS_BYTES: u64 = 4 * 1024 * 1024;

/// How much of an action log the console reads on one poll.
pub const MAX_ACTIONS_BYTES: u64 = 8 * 1024 * 1024;

/// The most endpoints one detail view returns.
pub const MAX_ENDPOINTS_SHOWN: usize = 500;

/// Anything newer than this counts as "now" for the working/idle line.
pub const RECENT_SECONDS: i64 = 60;

/// How loudly a session is asking to be looked at.
///
/// Five states, herdr's words because the problem is herdr's. `done` is the one
/// a client clears by looking, so the server reports what is true and each
/// client remembers its own seen set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Attention {
    /// `blocked`, `working`, `done`, `idle` or `unknown`.
    pub state: &'static str,
    /// The evidence that produced the state, in one clause. A badge without
    /// this is a score, and this console does not score.
    pub why: String,
}

impl Attention {
    fn new(state: &'static str, why: impl Into<String>) -> Self {
        Self {
            state,
            why: why.into(),
        }
    }
}

/// What a session's own files say about it.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SessionSignals {
    /// Requests the engine recorded, both phases counted once.
    pub requests: usize,
    /// Requests policy refused. Kept apart from the total: a refusal is the
    /// boundary working, not a failure.
    pub denied: usize,
    /// Origins this session reached, most recent first, capped.
    pub origins: Vec<String>,
    /// The last request's timestamp, RFC3339, when there is one.
    pub last_request_at: Option<String>,
    /// Whether the session was opened with `--capture`, and how many messages
    /// are stored. The bytes stay on disk; this is a count.
    pub captured: Option<usize>,
    /// What a reclaimed store left behind. Kept apart from `captured`, because
    /// "the bytes were kept and then reclaimed" and "the bytes were never
    /// kept" are different facts.
    pub reclaimed: Option<bs::Reclaimed>,
    /// The recon ledger, folded into counts by state.
    pub ledger: Option<LedgerCounts>,
    /// Recon runs that spent requests, newest last.
    pub jobs: Vec<JobRow>,
    /// Findings the agent wrote beside the store, folded by id.
    pub findings: usize,
    /// Verbs the agent asked for, as the action log counts them.
    pub verbs: usize,
}

/// A ledger, as a fleet row needs it.
#[derive(Debug, Clone, Default, Serialize)]
pub struct LedgerCounts {
    pub candidate: usize,
    pub observed: usize,
    pub confirmed: usize,
    pub refused: usize,
    pub gone: usize,
    /// Lines folded, which is what `--since` counts in.
    pub cursor: u64,
    /// Lines that would not read as an observation.
    pub unreadable: u64,
    /// Whether the fold stopped at [`MAX_LEDGER_BYTES`].
    pub truncated: bool,
}

/// One recorded run of a verb that spends requests.
#[derive(Debug, Clone, Serialize)]
pub struct JobRow {
    pub id: String,
    pub verb: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub requests: u64,
    pub written: u64,
    pub stopped: Option<String>,
}

/// Which of the five states this session is in, and why. Strict about
/// `blocked`: only two things block, and both are somebody waiting on a person.
pub fn attention(
    session: &bs::Session,
    signals: &SessionSignals,
    holder_is_human: bool,
    engine_reachable: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> Attention {
    if holder_is_human {
        return Attention::new(
            "blocked",
            "a human holds the control lock, so the agent cannot drive until `h5i browser release`",
        );
    }
    if let Some(job) = signals.jobs.last()
        && let Some(why) = &job.stopped
        && job.ended_at.is_some()
        && needs_a_person(why)
    {
        return Attention::new("blocked", format!("the last {} run stopped: {why}", job.verb));
    }
    if !session.state.is_live() {
        let ended = session.end_reason.clone().unwrap_or_else(|| {
            format!("the session is {}", state_word(&session.state))
        });
        return Attention::new("done", ended);
    }
    if !engine_reachable {
        return Attention::new(
            "unknown",
            "the record says live and the engine's control file is gone, so this cannot be classified",
        );
    }
    if let Some(job) = signals.jobs.last()
        && job.ended_at.is_none()
    {
        return Attention::new("working", format!("a {} run is in flight", job.verb));
    }
    match signals.last_request_at.as_deref().and_then(seconds_since(now)) {
        Some(age) if age <= RECENT_SECONDS => Attention::new(
            "working",
            format!("{} requests, the last {age}s ago", signals.requests),
        ),
        Some(age) => Attention::new(
            "idle",
            format!("live, and nothing since {age}s ago"),
        ),
        None => Attention::new("idle", "live, and nothing fetched yet"),
    }
}

/// Whether a run's stopping reason is one only a person can answer.
///
/// Spending the allowance the operator set is a bounded run ending as asked; a
/// login that went away, or a policy that refused, is a question.
pub fn needs_a_person(reason: &str) -> bool {
    let reason = reason.to_ascii_lowercase();
    if reason.contains("allowance") || reason.contains("ran out of frontier") {
        return false;
    }
    reason.contains("logged in")
        || reason.contains("login")
        || reason.contains("refused")
        || reason.contains("not in the allowlist")
        || reason.contains("could not be sent")
}

fn state_word(state: &bs::State) -> &'static str {
    match state {
        bs::State::Live => "live",
        bs::State::Closed => "closed",
        bs::State::Died => "dead",
        bs::State::Expired => "expired",
        _ => "ended",
    }
}

/// How long ago a timestamp was, in seconds, or `None` when it will not parse.
fn seconds_since(now: chrono::DateTime<chrono::Utc>) -> impl Fn(&str) -> Option<i64> {
    move |at: &str| {
        chrono::DateTime::parse_from_rfc3339(at)
            .ok()
            .map(|then| (now - then.with_timezone(&chrono::Utc)).num_seconds().max(0))
    }
}

/// Fold a session's request log into the counts a row shows.
///
/// Off the log rather than out of the engine: the console is read-only and
/// asking a live session would be driving it.
pub fn signals_from_receipts(records: &[serde_json::Value]) -> SessionSignals {
    let mut signals = SessionSignals::default();
    for record in records {
        let phase = record.get("phase").and_then(|v| v.as_str()).unwrap_or("");
        if phase != "request" {
            continue;
        }
        signals.requests += 1;
        if record.get("allowed").and_then(|v| v.as_bool()) == Some(false) {
            signals.denied += 1;
        }
        if let Some(at) = record.get("at").and_then(|v| v.as_str()) {
            signals.last_request_at = Some(at.to_string());
        }
        if let Some(url) = record.get("url").and_then(|v| v.as_str())
            && let Ok(parsed) = url::Url::parse(url)
            && let Some(host) = parsed.host_str()
        {
            let origin = match parsed.port() {
                Some(port) => format!("{}://{host}:{port}", parsed.scheme()),
                None => format!("{}://{host}", parsed.scheme()),
            };
            if !signals.origins.contains(&origin) && signals.origins.len() < 8 {
                signals.origins.push(origin);
            }
        }
    }
    signals
}

/// The recon ledger beside a session, folded into counts.
pub fn ledger_counts(session_dir: &Path) -> Option<LedgerCounts> {
    let path = ledger_path(session_dir);
    let bytes = std::fs::read(&path).ok()?;
    let truncated = bytes.len() as u64 > MAX_LEDGER_BYTES;
    let head = if truncated {
        &bytes[..MAX_LEDGER_BYTES as usize]
    } else {
        &bytes[..]
    };
    let text = String::from_utf8_lossy(head);
    let inventory = h5i_wire::ledger::fold(text.lines());
    let mut counts = LedgerCounts {
        cursor: inventory.cursor,
        unreadable: inventory.unreadable,
        truncated,
        ..LedgerCounts::default()
    };
    for endpoint in &inventory.endpoints {
        let slot = match endpoint.state {
            h5i_wire::ledger::State::Candidate => &mut counts.candidate,
            h5i_wire::ledger::State::Observed => &mut counts.observed,
            h5i_wire::ledger::State::Confirmed => &mut counts.confirmed,
            h5i_wire::ledger::State::Refused => &mut counts.refused,
            h5i_wire::ledger::State::Gone => &mut counts.gone,
        };
        *slot += 1;
    }
    Some(counts)
}

/// The endpoints themselves, for a detail view. Capped.
pub fn ledger_endpoints(session_dir: &Path) -> Vec<h5i_wire::ledger::Endpoint> {
    let path = ledger_path(session_dir);
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new();
    };
    let head = &bytes[..bytes.len().min(MAX_LEDGER_BYTES as usize)];
    let text = String::from_utf8_lossy(head);
    let mut endpoints = h5i_wire::ledger::fold(text.lines()).endpoints;
    endpoints.truncate(MAX_ENDPOINTS_SHOWN);
    endpoints
}

fn ledger_path(session_dir: &Path) -> PathBuf {
    session_dir.join("recon").join("ledger.jsonl")
}

/// Recon's job records for a session, oldest first.
pub fn jobs(session_dir: &Path) -> Vec<JobRow> {
    let dir = session_dir.join("recon").join("jobs");
    let mut jobs: Vec<JobRow> = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return jobs;
    };
    for entry in entries.flatten().take(500) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let field = |name: &str| {
            value
                .get(name)
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        let number = |name: &str| value.get(name).and_then(|v| v.as_u64()).unwrap_or(0);
        jobs.push(JobRow {
            id: field("id").unwrap_or_default(),
            verb: field("verb").unwrap_or_default(),
            started_at: field("started_at").unwrap_or_default(),
            ended_at: field("ended_at"),
            requests: number("requests"),
            written: number("written"),
            stopped: field("stopped"),
        });
    }
    jobs.sort_by(|a, b| a.started_at.cmp(&b.started_at));
    jobs
}

/// How many messages a session stored, or `None` when it captured nothing.
///
/// A count, never the bytes: that directory is the one artifact h5i keeps that
/// holds credentials in full.
pub fn captured(session_dir: &Path) -> Option<usize> {
    let dir = session_dir.join(bs::MESSAGES_DIR);
    let entries = std::fs::read_dir(&dir).ok()?;
    Some(
        entries
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.ends_with(".request.json"))
            })
            .count(),
    )
}

/// One verb the agent asked for, with its result folded onto it.
///
/// The action log writes two lines per verb: `request` before it runs and
/// `result` after. A reader wants one row, and the receipts it spent are the
/// join to the request log: a proxy sees a GET, this row says which verb
/// caused it.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ActionRow {
    pub seq: u64,
    pub at: String,
    pub verb: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// `None` while the verb has not reported back.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    /// Receipt sequence numbers written while this verb ran.
    pub requests: Vec<u64>,
}

/// The action log, folded into one row per verb, oldest first. Capped to the
/// newest [`MAX_ACTIONS_SHOWN`].
pub fn actions(session_dir: &Path) -> Vec<ActionRow> {
    let path = session_dir.join(bs::ACTIONS_FILE);
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new();
    };
    // The tail is the recent half. Reading from the end keeps a long session's
    // newest verbs rather than its first ones.
    let start = bytes.len().saturating_sub(MAX_ACTIONS_BYTES as usize);
    let text = String::from_utf8_lossy(&bytes[start..]);
    let mut rows: Vec<ActionRow> = Vec::new();
    let mut index: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(seq) = value.get("seq").and_then(|v| v.as_u64()) else {
            continue;
        };
        let text_of = |name: &str| {
            value
                .get(name)
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        let phase = text_of("phase").unwrap_or_default();
        let at = text_of("at").unwrap_or_default();
        let slot = match index.get(&seq) {
            Some(&i) => i,
            None => {
                rows.push(ActionRow {
                    seq,
                    at: at.clone(),
                    verb: text_of("verb").unwrap_or_default(),
                    ..ActionRow::default()
                });
                index.insert(seq, rows.len() - 1);
                rows.len() - 1
            }
        };
        let row = &mut rows[slot];
        if let Some(target) = text_of("target") {
            row.target = Some(target);
        }
        if let Some(url) = text_of("url") {
            row.url = Some(url);
        }
        if phase == "result" {
            row.ok = value.get("ok").and_then(|v| v.as_bool());
            row.error = text_of("error");
            row.ended_at = Some(at);
            if let Some(reqs) = value.get("requests").and_then(|v| v.as_array()) {
                row.requests = reqs.iter().filter_map(|r| r.as_u64()).collect();
            }
        }
    }
    let drop = rows.len().saturating_sub(MAX_ACTIONS_SHOWN);
    rows.drain(..drop);
    rows
}

/// One note on a finding, and when it was written.
#[derive(Debug, Clone, Serialize)]
pub struct FindingNote {
    pub at: String,
    pub text: String,
}

/// A finding as the agent left it: the same fold `h5i websec finding` makes,
/// so the console and the CLI agree on what a finding currently says.
#[derive(Debug, Clone, Serialize)]
pub struct FindingRow {
    pub id: String,
    pub title: String,
    /// The agent's own word for where this stands. Free text on purpose.
    pub state: String,
    /// Message ids the finding rests on. Every one names a stored message.
    pub evidence: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repro: Option<String>,
    pub notes: Vec<FindingNote>,
    pub created: String,
    pub updated: String,
}

/// The findings log beside a session, folded by id, oldest first.
pub fn findings(session_dir: &Path) -> Vec<FindingRow> {
    let path = session_dir.join("findings").join("findings.jsonl");
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new();
    };
    let head = &bytes[..bytes.len().min(MAX_FINDINGS_BYTES as usize)];
    let text = String::from_utf8_lossy(head);
    fold_findings(text.lines())
}

/// Titles and states replace; notes and evidence accumulate. A title is a
/// statement of what the finding is, so two of them is one wrong; a note is
/// something learned, which does not stop being true.
pub fn fold_findings<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<FindingRow> {
    let mut rows: Vec<FindingRow> = Vec::new();
    for line in lines {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let text_of = |name: &str| {
            value
                .get(name)
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        let Some(id) = text_of("id") else {
            continue;
        };
        let at = text_of("at").unwrap_or_default();
        let slot = match rows.iter().position(|r| r.id == id) {
            Some(i) => i,
            None => {
                rows.push(FindingRow {
                    id,
                    title: String::new(),
                    state: String::new(),
                    evidence: Vec::new(),
                    repro: None,
                    notes: Vec::new(),
                    created: at.clone(),
                    updated: at.clone(),
                });
                rows.len() - 1
            }
        };
        let row = &mut rows[slot];
        if let Some(title) = text_of("title") {
            row.title = title;
        }
        if let Some(state) = text_of("state") {
            row.state = state;
        }
        if let Some(repro) = text_of("repro") {
            row.repro = Some(repro);
        }
        if let Some(note) = text_of("note") {
            row.notes.push(FindingNote {
                at: at.clone(),
                text: note,
            });
        }
        if let Some(evidence) = value.get("evidence").and_then(|v| v.as_array()) {
            for item in evidence.iter().filter_map(|e| e.as_str()) {
                if !row.evidence.iter().any(|have| have == item) {
                    row.evidence.push(item.to_string());
                }
            }
        }
        row.updated = at;
    }
    rows
}

/// One path under one origin, as the receipts reached it.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SiteEndpoint {
    pub path: String,
    pub methods: Vec<String>,
    pub statuses: Vec<u16>,
    /// Query parameter names seen on this path, in first-seen order.
    pub params: Vec<String>,
    /// Fetches that were allowed.
    pub hits: usize,
    /// Fetches policy refused before the wire.
    pub refused: usize,
    /// Reached by a navigation at least once, rather than only pulled in.
    pub navigated: bool,
    /// The newest receipt on this path, for a reader who wants the row.
    pub last_seq: u64,
}

/// One origin the session touched, and what under it.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SiteOrigin {
    pub origin: String,
    pub hits: usize,
    pub refused: usize,
    pub endpoints: Vec<SiteEndpoint>,
}

/// The most paths one origin keeps on the map. Past this the fold counts and
/// says so through `hits` rather than growing the screen without bound.
pub const MAX_SITEMAP_PATHS: usize = 2000;

/// The request log folded into origins and paths: what this session reached,
/// and what it was refused. The same shape `h5i websec sitemap` prints, from
/// the same receipts, so the two never disagree about where a session went.
pub fn sitemap(records: &[serde_json::Value]) -> Vec<SiteOrigin> {
    let mut origins: Vec<SiteOrigin> = Vec::new();
    let mut paths = 0usize;
    for record in records {
        let Some(url) = record.get("url").and_then(|v| v.as_str()) else {
            continue;
        };
        let Ok(parsed) = url::Url::parse(url) else {
            continue;
        };
        let Some(host) = parsed.host_str() else {
            continue;
        };
        let origin = match parsed.port() {
            Some(port) => format!("{}://{host}:{port}", parsed.scheme()),
            None => format!("{}://{host}", parsed.scheme()),
        };
        let phase = record.get("phase").and_then(|v| v.as_str()).unwrap_or("");
        let allowed = record.get("allowed").and_then(|v| v.as_bool()).unwrap_or(true);
        let seq = record.get("seq").and_then(|v| v.as_u64()).unwrap_or(0);
        let o = match origins.iter().position(|o| o.origin == origin) {
            Some(i) => i,
            None => {
                origins.push(SiteOrigin {
                    origin,
                    ..SiteOrigin::default()
                });
                origins.len() - 1
            }
        };
        let origin = &mut origins[o];
        let path = parsed.path().to_string();
        let e = match origin.endpoints.iter().position(|e| e.path == path) {
            Some(i) => i,
            None => {
                if paths >= MAX_SITEMAP_PATHS {
                    if phase == "request" {
                        if allowed {
                            origin.hits += 1;
                        } else {
                            origin.refused += 1;
                        }
                    }
                    continue;
                }
                paths += 1;
                origin.endpoints.push(SiteEndpoint {
                    path,
                    ..SiteEndpoint::default()
                });
                origin.endpoints.len() - 1
            }
        };
        let endpoint = &mut origin.endpoints[e];
        endpoint.last_seq = endpoint.last_seq.max(seq);
        if let Some(method) = record.get("method").and_then(|v| v.as_str())
            && !endpoint.methods.iter().any(|m| m == method)
        {
            endpoint.methods.push(method.to_string());
        }
        for (name, _) in parsed.query_pairs() {
            if !endpoint.params.iter().any(|p| *p == name) && endpoint.params.len() < 64 {
                endpoint.params.push(name.into_owned());
            }
        }
        if record.get("initiator").and_then(|v| v.as_str()) == Some("navigation") {
            endpoint.navigated = true;
        }
        match phase {
            "request" => {
                if allowed {
                    endpoint.hits += 1;
                    origin.hits += 1;
                } else {
                    endpoint.refused += 1;
                    origin.refused += 1;
                }
            }
            "response" => {
                if let Some(status) = record.get("status").and_then(|v| v.as_u64())
                    && let Ok(status) = u16::try_from(status)
                    && !endpoint.statuses.contains(&status)
                {
                    endpoint.statuses.push(status);
                    endpoint.statuses.sort_unstable();
                }
            }
            _ => {}
        }
    }
    // Busiest origins first; within one, paths in the order they were reached.
    origins.sort_by_key(|o| std::cmp::Reverse(o.hits + o.refused));
    origins
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn session(state: bs::State) -> bs::Session {
        bs::Session {
            id: "br_test".into(),
            name: None,
            engine: bs::Engine::H5iLight,
            lane: bs::Lane::EngineClaimed,
            placement: bs::Placement::Host,
            url: "https://target.test/".into(),
            started_at: "2026-09-07T10:00:00Z".into(),
            expires_at: None,
            storage: bs::Storage::Ephemeral,
            policy_digest: "sha256:test".into(),
            identity: "native".into(),
            identity_digest: "test".into(),
            restored_from: None,
            state,
            ended_at: None,
            end_reason: None,
            confinement: crate::browser_sandbox::Confinement::Process,
            enclosing_box: None,
            control: bs::Control::default(),
            logs: bs::Logs::default(),
            permissive_cors: false,
        }
    }

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now()
    }

    #[test]
    fn a_human_holding_the_lock_is_the_loudest_thing_on_the_screen() {
        let verdict = attention(&session(bs::State::Live), &SessionSignals::default(), true, true, now());
        assert_eq!(verdict.state, "blocked");
        assert!(verdict.why.contains("control lock"), "{}", verdict.why);
    }

    #[test]
    fn a_bounded_run_that_spent_its_allowance_is_not_a_question() {
        // The operator asked for 120 requests and got 120. Marking that
        // `blocked` would put every finished run in the loudest column.
        let signals = SessionSignals {
            jobs: vec![JobRow {
                id: "job_1".into(),
                verb: "paths".into(),
                started_at: "2026-09-07T10:00:00Z".into(),
                ended_at: Some("2026-09-07T10:01:00Z".into()),
                requests: 120,
                written: 120,
                stopped: Some("spent its allowance of 120 requests".into()),
            }],
            last_request_at: Some("2026-09-07T10:01:00Z".into()),
            ..SessionSignals::default()
        };
        let verdict = attention(&session(bs::State::Live), &signals, false, true, now());
        assert_ne!(verdict.state, "blocked", "{}", verdict.why);
    }

    #[test]
    fn a_run_that_stopped_for_a_reason_asks_for_a_person() {
        let signals = SessionSignals {
            jobs: vec![JobRow {
                id: "job_1".into(),
                verb: "paths".into(),
                started_at: "2026-09-07T10:00:00Z".into(),
                ended_at: Some("2026-09-07T10:01:00Z".into()),
                requests: 40,
                written: 40,
                stopped: Some(
                    "the page this walk started from answers differently now, so the session \
                     is no longer logged in as `alice`"
                        .into(),
                ),
            }],
            ..SessionSignals::default()
        };
        let verdict = attention(&session(bs::State::Live), &signals, false, true, now());
        assert_eq!(verdict.state, "blocked");
        assert!(verdict.why.contains("logged in"), "{}", verdict.why);
    }

    #[test]
    fn a_live_session_that_just_fetched_is_working_and_a_quiet_one_is_not() {
        let mut signals = SessionSignals {
            requests: 3,
            last_request_at: Some(
                (now() - chrono::Duration::seconds(5)).to_rfc3339(),
            ),
            ..SessionSignals::default()
        };
        assert_eq!(
            attention(&session(bs::State::Live), &signals, false, true, now()).state,
            "working"
        );

        signals.last_request_at = Some((now() - chrono::Duration::seconds(600)).to_rfc3339());
        let quiet = attention(&session(bs::State::Live), &signals, false, true, now());
        assert_eq!(quiet.state, "idle");
        assert!(quiet.why.contains("600s"), "{}", quiet.why);
    }

    #[test]
    fn an_ended_session_is_done_until_a_client_has_looked() {
        let verdict = attention(
            &session(bs::State::Closed),
            &SessionSignals::default(),
            false,
            false,
            now(),
        );
        assert_eq!(
            verdict.state, "done",
            "the server reports what is true; each client remembers what it has seen"
        );
    }

    #[test]
    fn a_live_record_with_no_engine_is_not_classified() {
        let verdict = attention(
            &session(bs::State::Live),
            &SessionSignals::default(),
            false,
            false,
            now(),
        );
        assert_eq!(verdict.state, "unknown");
        assert!(verdict.why.contains("control file"), "{}", verdict.why);
    }

    #[test]
    fn receipts_fold_into_counts_that_keep_refusals_apart() {
        let records = vec![
            json!({"phase": "request", "at": "2026-09-07T10:00:00Z", "url": "https://a.test/one", "allowed": true}),
            json!({"phase": "response", "at": "2026-09-07T10:00:01Z", "url": "https://a.test/one", "allowed": true, "status": 200}),
            json!({"phase": "request", "at": "2026-09-07T10:00:02Z", "url": "https://b.test/x", "allowed": false}),
        ];
        let signals = signals_from_receipts(&records);
        assert_eq!(signals.requests, 2, "a request and its response are one fetch");
        assert_eq!(signals.denied, 1);
        assert_eq!(signals.origins, vec!["https://a.test", "https://b.test"]);
        assert_eq!(signals.last_request_at.as_deref(), Some("2026-09-07T10:00:02Z"));
    }

    #[test]
    fn verbs_fold_onto_one_row_and_keep_the_receipts_they_spent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(bs::ACTIONS_FILE),
            concat!(
                r#"{"seq":0,"at":"2026-09-11T00:00:00Z","phase":"request","verb":"open","url":"https://a.test/"}"#,
                "\n",
                r#"{"seq":0,"at":"2026-09-11T00:00:01Z","phase":"result","verb":"open","ok":true,"requests":[0,1,2]}"#,
                "\n",
                r#"{"seq":1,"at":"2026-09-11T00:00:02Z","phase":"request","verb":"click","target":"@e3"}"#,
                "\n",
                "not json\n",
            ),
        )
        .unwrap();
        let rows = actions(dir.path());
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].verb, "open");
        assert_eq!(rows[0].ok, Some(true));
        assert_eq!(rows[0].requests, vec![0, 1, 2]);
        assert_eq!(rows[0].ended_at.as_deref(), Some("2026-09-11T00:00:01Z"));
        assert_eq!(rows[1].target.as_deref(), Some("@e3"));
        assert_eq!(rows[1].ok, None, "a verb that has not reported back is in flight");
    }

    #[test]
    fn findings_fold_like_the_workbench_does() {
        let lines = [
            r#"{"id":"finding_1","at":"t1","title":"idor","state":"suspected","note":"first","evidence":["req_3"]}"#,
            r#"{"id":"finding_1","at":"t2","state":"confirmed","note":"second","evidence":["req_3","req_9"]}"#,
            r#"{"id":"finding_2","at":"t3","title":"other"}"#,
        ];
        let rows = fold_findings(lines.iter().copied());
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].title, "idor");
        assert_eq!(rows[0].state, "confirmed", "the newest state replaces");
        assert_eq!(rows[0].notes.len(), 2, "notes accumulate");
        assert_eq!(rows[0].evidence, vec!["req_3", "req_9"], "evidence is a set");
        assert_eq!(rows[0].created, "t1");
        assert_eq!(rows[0].updated, "t2");
        assert_eq!(rows[1].id, "finding_2");
    }

    #[test]
    fn the_sitemap_keeps_refusals_apart_from_hits() {
        let records = vec![
            json!({"seq":0,"phase":"request","initiator":"navigation","method":"GET","url":"https://a.test/x?id=1","allowed":true}),
            json!({"seq":0,"phase":"response","initiator":"navigation","method":"GET","url":"https://a.test/x?id=1","allowed":true,"status":200}),
            json!({"seq":1,"phase":"request","initiator":"subresource","method":"POST","url":"https://a.test/x?id=2&q=z","allowed":true}),
            json!({"seq":1,"phase":"response","initiator":"subresource","method":"POST","url":"https://a.test/x?id=2&q=z","allowed":true,"status":404}),
            json!({"seq":2,"phase":"request","initiator":"subresource","method":"GET","url":"https://cdn.test/f.woff","allowed":false,"denied_reason":"off scope"}),
            json!({"seq":2,"phase":"response","initiator":"subresource","method":"GET","url":"https://cdn.test/f.woff","allowed":false}),
        ];
        let map = sitemap(&records);
        assert_eq!(map.len(), 2);
        let a = &map[0];
        assert_eq!(a.origin, "https://a.test");
        assert_eq!(a.hits, 2);
        assert_eq!(a.endpoints.len(), 1);
        let x = &a.endpoints[0];
        assert_eq!(x.methods, vec!["GET", "POST"]);
        assert_eq!(x.statuses, vec![200, 404]);
        assert_eq!(x.params, vec!["id", "q"]);
        assert!(x.navigated);
        assert_eq!(x.last_seq, 1);
        let cdn = &map[1];
        assert_eq!(cdn.hits, 0);
        assert_eq!(cdn.refused, 1);
        assert_eq!(cdn.endpoints[0].refused, 1);
    }
}
