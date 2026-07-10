//! The orchestra journal — durable step results on the team event log.
//!
//! Every effectful eDSL operation (an agent turn, a verification, a
//! `Conductor::step` closure) is journaled: it executes once, its serialized
//! result is appended to `refs/h5i/team/<run>` as an `orch_step` event through
//! the same CAS `append_event` path every team event uses, and a resumed score
//! replays the recorded result instead of re-executing. Failures are *not*
//! journaled — an error propagates to the score and a resume retries the step.
//!
//! Step identity is `(label, per-label sequence)`, rendered as `label#seq`.
//! Sequence numbers count per label, not globally, so concurrent branches with
//! distinct labels produce stable keys regardless of interleaving. The one
//! discipline this asks of user code: steps that run concurrently must carry
//! distinct labels (agent operations get this by construction — their labels
//! embed the agent id).

use crate::error::H5iError;
use crate::team;
use git2::Repository;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Event kind of one journaled step result.
pub(crate) const STEP_EVENT_KIND: &str = "orch_step";
/// Event kind of one score process start (digest provenance, never replayed).
pub(crate) const SCORE_EVENT_KIND: &str = "orch_score";

/// Inline result cap. Journal entries live in the run's `events.jsonl`; a step
/// result beyond this is refused loudly (route bulk data through the capture
/// store and journal the capture id) rather than silently bloating the ref.
const MAX_INLINE_RESULT: usize = 64 * 1024;

pub(crate) struct Journal {
    repo_path: PathBuf,
    run_id: String,
    actor: String,
    state: Mutex<State>,
}

struct State {
    /// Next sequence number per label (1-based).
    seq: BTreeMap<String, u32>,
    /// Recorded results from prior invocations of this run, keyed by step key.
    replay: BTreeMap<String, serde_json::Value>,
    /// Keys this process has replayed or recorded — what `patched` uses to
    /// tell "resuming an older run" from "executing fresh".
    consumed: std::collections::BTreeSet<String>,
}

impl Journal {
    /// Open the journal for `run_id`, loading every previously recorded step
    /// result. A run with no ref yet (about to be created) starts empty.
    pub(crate) fn open(repo_path: &Path, run_id: &str, actor: &str) -> Self {
        let mut replay = BTreeMap::new();
        if let Ok(repo) = Repository::open(repo_path) {
            if let Ok(events) = team::read_events(&repo, run_id) {
                for ev in events.iter().filter(|e| e.kind == STEP_EVENT_KIND) {
                    if let Some(key) = ev.payload.get("key").and_then(|v| v.as_str()) {
                        let result = ev
                            .payload
                            .get("result")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        replay.insert(key.to_string(), result);
                    }
                }
            }
        }
        Journal {
            repo_path: repo_path.to_path_buf(),
            run_id: run_id.to_string(),
            actor: actor.to_string(),
            state: Mutex::new(State {
                seq: BTreeMap::new(),
                replay,
                consumed: Default::default(),
            }),
        }
    }

    /// Number of journaled steps loaded at open — what a resume will replay.
    pub(crate) fn replay_len(&self) -> usize {
        self.state.lock().expect("journal lock").replay.len()
    }

    /// Allocate the next step key for `label` (`label#1`, `label#2`, …).
    pub(crate) fn next_key(&self, label: &str) -> String {
        let mut st = self.state.lock().expect("journal lock");
        let seq = st.seq.entry(label.to_string()).or_insert(0);
        *seq += 1;
        format!("{label}#{seq}")
    }

    /// Replay the recorded result for `key` as `T`, if one exists. A recorded
    /// value that no longer deserializes as `T` is a resume divergence (the
    /// score changed shape mid-run) and fails closed with the offending key.
    pub(crate) fn replay_as<T: DeserializeOwned>(
        &self,
        key: &str,
    ) -> Option<Result<T, H5iError>> {
        let value = {
            let mut st = self.state.lock().expect("journal lock");
            let v = st.replay.get(key).cloned();
            if v.is_some() {
                st.consumed.insert(key.to_string());
            }
            v
        }?;
        let run_id = self.run_id.clone();
        Some(serde_json::from_value(value).map_err(move |e| {
            H5iError::Metadata(format!(
                "orchestra resume divergence: journaled step '{key}' no longer matches the \
                 score's expected result type ({e}) — the score changed shape mid-run; \
                 inspect the recorded steps with `h5i team trace {run_id}`, then finish the \
                 run with the original score or start a new run"
            ))
        }))
    }

    /// True while previously journaled steps remain un-replayed — i.e. this
    /// process is resuming a run whose journal was written before the current
    /// point in the score. `Conductor::patched` uses this to keep migration
    /// branches consistent.
    pub(crate) fn has_unconsumed(&self) -> bool {
        let st = self.state.lock().expect("journal lock");
        st.replay.keys().any(|k| !st.consumed.contains(k))
    }

    /// Record a completed step's result. Executes-once semantics come from the
    /// caller consulting `replay_as` first; this only appends.
    pub(crate) fn record<T: Serialize>(
        &self,
        key: &str,
        label: &str,
        value: &T,
    ) -> Result<(), H5iError> {
        let result = serde_json::to_value(value)?;
        let rendered = serde_json::to_string(&result)?;
        if rendered.len() > MAX_INLINE_RESULT {
            return Err(H5iError::Metadata(format!(
                "orchestra step '{key}' result is {} bytes — beyond the {MAX_INLINE_RESULT}-byte \
                 inline journal cap; store bulk output as a capture (`h5i capture run`) and \
                 journal the capture id instead",
                rendered.len()
            )));
        }
        let repo = Repository::open(&self.repo_path)?;
        let ev = team::event(
            &self.run_id,
            &self.actor,
            STEP_EVENT_KIND,
            0,
            None,
            None,
            format!("{STEP_EVENT_KIND}:{}:{key}", self.run_id),
            serde_json::json!({ "key": key, "label": label, "result": result }),
        );
        team::append_event(&repo, &ev)?;
        let mut st = self.state.lock().expect("journal lock");
        st.replay.insert(key.to_string(), result);
        st.consumed.insert(key.to_string());
        Ok(())
    }

    /// Record a score process start with the binary's digest (provenance, not a
    /// journaled step). Returns a warning when the digest differs from the last
    /// recorded one — the run is being resumed by a different binary, so step
    /// keys may have shifted.
    pub(crate) fn record_score_start(
        &self,
        digest: Option<&str>,
    ) -> Result<Option<String>, H5iError> {
        let repo = Repository::open(&self.repo_path)?;
        let previous = team::read_events(&repo, &self.run_id)
            .ok()
            .and_then(|events| {
                events
                    .iter()
                    .rev()
                    .find(|e| e.kind == SCORE_EVENT_KIND)
                    .and_then(|e| e.payload.get("digest").and_then(|v| v.as_str()).map(String::from))
            });
        let ev = team::event(
            &self.run_id,
            &self.actor,
            SCORE_EVENT_KIND,
            0,
            None,
            None,
            format!("{SCORE_EVENT_KIND}:{}:{}", self.run_id, chrono::Utc::now().to_rfc3339()),
            serde_json::json!({ "digest": digest }),
        );
        team::append_event(&repo, &ev)?;
        Ok(match (previous.as_deref(), digest) {
            (Some(prev), Some(cur)) if prev != cur => Some(format!(
                "score binary changed since this run was journaled ({} → {}); resumed step keys \
                 may have shifted — verify the replay before trusting it",
                &prev[..prev.len().min(12)],
                &cur[..cur.len().min(12)],
            )),
            _ => None,
        })
    }

    /// Best-effort sha256 of the running score binary, for `record_score_start`.
    pub(crate) fn current_exe_digest() -> Option<String> {
        use sha2::{Digest, Sha256};
        let exe = std::env::current_exe().ok()?;
        let bytes = std::fs::read(exe).ok()?;
        Some(format!("{:x}", Sha256::digest(bytes)))
    }
}
