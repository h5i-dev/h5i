//! h5i orchestra — the programmable agent-orchestration eDSL.
//!
//! Design: `roadmap/orchestra-design.md`. An orchestration (a "score") is an
//! ordinary async Rust program handed a [`Conductor`]: agents are hired into
//! sandboxed envs, work/review/revise turns are dispatched over the existing
//! team event log + i5h inboxes, and every effectful step is journaled so a
//! killed score resumes without re-running completed agent turns. There is no
//! graph builder and no `compile()` step — `if`, `for`, and `tokio::join!` are
//! the orchestration language, and the DAG is whatever the journal recorded.
//!
//! The score is host-side user code (the same trust level as the team shell
//! scripts it replaces). Agents stay in their boxes: turns are delivered
//! through the per-env read-only inbox and results come back through the
//! host-validated submit spool — the eDSL adds no new trust surface.
//!
//! ```no_run
//! use h5i_orchestra::{policy, Attach, Conductor};
//! use std::sync::Arc;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), h5i_core::error::H5iError> {
//!     let c = Conductor::builder(".", "myrun").launcher(Arc::new(Attach)).launch()?;
//!     let claude = c.agent("claude").runtime("claude").hire().await?;
//!     let codex = c.agent("codex").runtime("codex").hire().await?;
//!     let task = "implement `h5i pull` mirroring `h5i push`";
//!     let (a, b) = tokio::try_join!(claude.work(task), codex.work(task))?;
//!     c.freeze().await?;
//!     let (ra, rb) = tokio::try_join!(codex.review(&a), claude.review(&b))?;
//!     let _ = (ra, rb);
//!     c.verify(&a, ["cargo", "test", "--quiet"], None).await?;
//!     c.verify(&b, ["cargo", "test", "--quiet"], None).await?;
//!     let verdict = c.judge(policy::tests_then_smallest_diff()).await?;
//!     println!("winner: {:?}", verdict.selected_submission);
//!     Ok(())
//! }
//! ```

mod agent;
mod gate;
mod journal;
mod judge;
mod launcher;
pub mod manifest;
pub mod patterns;
mod preflight;
pub mod rpc;
pub mod trace;

pub use agent::{Agent, AgentBuilder, WorkRequest};
pub use gate::{Gate, GateAnswer};
pub use judge::{policy, VerdictPolicy};
pub use launcher::{Attach, FnLauncher, LaunchResident, RuntimeLauncher, TurnContext, TurnKind};
pub use preflight::Preflight;

use h5i_core::env;
use h5i_core::error::H5iError;
use h5i_core::msg;
use h5i_core::storage;
use h5i_core::team::{
    self, TeamApplyResult, TeamArtifact, TeamReview, TeamRun, TeamStatus, TeamVerdict,
    TeamVerification,
};
use git2::Repository;
use journal::Journal;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ── Conductor ─────────────────────────────────────────────────────────────────

pub(crate) struct Core {
    repo_workdir: PathBuf,
    h5i_root: PathBuf,
    run_id: String,
    actor: String,
    journal: Journal,
    launcher: Arc<dyn RuntimeLauncher>,
    poll_interval: Duration,
    turn_timeout: Duration,
}

impl Core {
    fn repo(&self) -> Result<Repository, H5iError> {
        Ok(Repository::open(&self.repo_workdir)?)
    }
}

/// The handle a score drives its run through. Cheap to clone; every operation
/// opens its own git handle and runs on the blocking pool, so agent futures
/// compose with plain `tokio::join!`/`select!`.
#[derive(Clone)]
pub struct Conductor {
    core: Arc<Core>,
}

pub struct ConductorBuilder {
    repo: PathBuf,
    run: String,
    title: Option<String>,
    base: String,
    max_rounds: u32,
    actor: Option<String>,
    launcher: Arc<dyn RuntimeLauncher>,
    poll_interval: Duration,
    turn_timeout: Duration,
    score_digest: bool,
    digest_override: Option<String>,
}

impl Conductor {
    /// Start building a conductor for team run `run` in the repository at (or
    /// containing) `repo`. Launching creates the run if it does not exist and
    /// resumes it (replaying the journal) if it does.
    pub fn builder(repo: impl AsRef<Path>, run: &str) -> ConductorBuilder {
        ConductorBuilder {
            repo: repo.as_ref().to_path_buf(),
            run: run.to_string(),
            title: None,
            base: "HEAD".into(),
            max_rounds: 1,
            actor: None,
            launcher: Arc::new(Attach),
            poll_interval: Duration::from_millis(1500),
            turn_timeout: Duration::from_secs(1800),
            score_digest: true,
            digest_override: None,
        }
    }

    pub fn run_id(&self) -> &str {
        &self.core.run_id
    }

    /// Hire an agent into the run. `name` is the roster agent id (and the
    /// journal label), so it must be ref-safe and unique within the run.
    pub fn agent(&self, name: &str) -> AgentBuilder {
        AgentBuilder::new(self.core.clone(), name.to_string())
    }

    /// Run an arbitrary effect exactly once, journaling its serialized result:
    /// the universal escape hatch (design doc §4.3). On resume a completed
    /// step returns its recorded result without re-executing. Steps inside
    /// concurrency must carry distinct labels; in a loop, embed the index
    /// (`format!("fetch/{i}")`).
    pub async fn step<T, F>(&self, label: &str, f: F) -> Result<T, H5iError>
    where
        T: Serialize + DeserializeOwned + Send + 'static,
        F: FnOnce() -> Result<T, H5iError> + Send + 'static,
    {
        journaled(self.core.clone(), label.to_string(), move |_core| f()).await
    }

    /// A label namespace for steps in parallel loops: steps under distinct
    /// scopes can run concurrently without label collisions (which the
    /// journal otherwise fails closed on). `c.scope(format!("item/{i}"))
    /// .step("fetch", …)` journals as `item/<i>/fetch#1`.
    pub fn scope(&self, prefix: impl Into<String>) -> Scope {
        Scope {
            core: self.core.clone(),
            prefix: prefix.into(),
        }
    }

    /// Migration marker for resuming an in-flight run with a changed score.
    /// Per-label step keys already make *additive* changes safe (a new label
    /// never shifts existing keys); `patched` covers the remaining case —
    /// changing what an existing code path does — by keeping both branches
    /// selectable and the choice consistent: it returns `false` (take the old
    /// path) when this process is still replaying steps journaled before the
    /// marker existed, `true` (take the new path) on fresh execution, and the
    /// recorded value on every later resume.
    ///
    /// ```ignore
    /// if c.patched("verify-in-container").await? {
    ///     c.verify(&art, cmd, Some("container")).await?
    /// } else {
    ///     c.verify(&art, cmd, None).await?
    /// };
    /// ```
    pub async fn patched(&self, change_id: &str) -> Result<bool, H5iError> {
        let core = self.core.clone();
        let label = format!("patched/{change_id}");
        journaled(core.clone(), label, move |c| {
            Ok(!c.journal.has_unconsumed())
        })
        .await
    }

    /// Read the folded run state (not journaled — reads are free to repeat).
    pub async fn status(&self) -> Result<TeamStatus, H5iError> {
        let core = self.core.clone();
        run_blocking(move || team::status(&core.repo()?, &core.run_id)).await
    }

    /// Bind every enrolled roster seat as an [`Agent`] handle (not journaled —
    /// binding is a read). This is how a driver picks up a team whose agents
    /// were enrolled elsewhere (`team auto-create`, `team add-env`), instead
    /// of hiring them itself.
    pub async fn roster(&self) -> Result<Vec<Agent>, H5iError> {
        let core = self.core.clone();
        let seats: Vec<(String, String)> = run_blocking(move || {
            let run = team::status(&core.repo()?, &core.run_id)?.run;
            Ok(run
                .agents
                .iter()
                .map(|a| (a.agent_id.clone(), a.env_id.clone()))
                .collect())
        })
        .await?;
        Ok(seats
            .into_iter()
            .map(|(name, env_id)| Agent::bind(self.core.clone(), name, env_id))
            .collect())
    }

    /// Rank the roster side by side (the arena view; not journaled).
    pub async fn compare(&self) -> Result<Vec<team::TeamCompareRow>, H5iError> {
        let core = self.core.clone();
        run_blocking(move || team::compare(&core.repo()?, &core.h5i_root, &core.run_id)).await
    }

    /// Append a human-readable note to the run's event log (audit trail, not
    /// journaled).
    pub async fn note(&self, text: impl Into<String>) -> Result<(), H5iError> {
        let core = self.core.clone();
        let text = text.into();
        run_blocking(move || {
            let repo = core.repo()?;
            let ev = team::event(
                &core.run_id,
                &core.actor,
                "orch_note",
                0,
                None,
                None,
                format!("orch_note:{}:{}", core.run_id, text),
                serde_json::json!({ "text": text }),
            );
            team::append_event(&repo, &ev)
        })
        .await
    }

    /// Seal the open round (`team freeze`). Two eDSL-specific deviations from
    /// the CLI: missing roster submissions are allowed (the score awaits each
    /// `work()` explicitly, so participation is what the code awaited — and
    /// integrator/moderator seats legitimately sit out the first round), and
    /// it is idempotent under resume (a crash between the freeze and its
    /// journal record folds to the sealed state instead of erroring).
    pub async fn freeze(&self) -> Result<TeamRun, H5iError> {
        journaled(self.core.clone(), "freeze".into(), move |core| {
            let repo = core.repo()?;
            match team::freeze(&repo, &core.run_id, true, &core.actor) {
                Ok(run) => Ok(run),
                Err(e) => {
                    let run = team::status(&repo, &core.run_id)?.run;
                    if run.phase == team::PHASE_SEALED_SUBMIT {
                        Ok(run)
                    } else {
                        Err(e)
                    }
                }
            }
        })
        .await
    }

    /// Neutrally verify an artifact owner's latest submission in a fresh
    /// sandboxed worktree — never the author's box (`team verify`).
    pub async fn verify<I, S>(
        &self,
        artifact: &TeamArtifact,
        command: I,
        isolation: Option<&str>,
    ) -> Result<TeamVerification, H5iError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let owner = artifact.owner_agent.clone();
        let command: Vec<String> = command.into_iter().map(Into::into).collect();
        let isolation = isolation.map(str::to_string);
        journaled(
            self.core.clone(),
            format!("verify/{owner}"),
            move |core| {
                team::verify(
                    &core.repo()?,
                    &core.h5i_root,
                    &core.run_id,
                    &owner,
                    command,
                    isolation.as_deref(),
                    &core.actor,
                )
            },
        )
        .await
    }

    /// Decide and record a verdict with a pluggable policy. The verdict lands
    /// in the event log through the same path as `team finalize`.
    pub async fn judge<P>(&self, policy: P) -> Result<TeamVerdict, H5iError>
    where
        P: VerdictPolicy + 'static,
    {
        journaled(self.core.clone(), "judge".into(), move |core| {
            let repo = core.repo()?;
            let run = team::status(&repo, &core.run_id)?.run;
            let verdict = policy.decide(&run)?;
            team::record_verdict(&repo, &core.run_id, &verdict, &core.actor)?;
            Ok(verdict)
        })
        .await
    }

    /// Apply an artifact onto the current branch, gated on an auto-applicable
    /// verdict selecting it — mediated, exactly like `h5i team apply`.
    pub async fn apply(&self, artifact: &TeamArtifact) -> Result<TeamApplyResult, H5iError> {
        self.apply_inner(artifact, false).await
    }

    /// Apply bypassing the verdict gate — the explicit-human-pick form
    /// (`h5i team apply --force`). Use only after your own gate.
    pub async fn apply_forced(
        &self,
        artifact: &TeamArtifact,
    ) -> Result<TeamApplyResult, H5iError> {
        self.apply_inner(artifact, true).await
    }

    async fn apply_inner(
        &self,
        artifact: &TeamArtifact,
        force: bool,
    ) -> Result<TeamApplyResult, H5iError> {
        let id = artifact.id.clone();
        let owner = artifact.owner_agent.clone();
        journaled(self.core.clone(), format!("apply/{owner}"), move |core| {
            team::apply_winner(
                &core.repo()?,
                &core.h5i_root,
                &core.run_id,
                Some(&id),
                force,
                &core.actor,
            )
        })
        .await
    }
}

impl ConductorBuilder {
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Base revision the run pins (default `HEAD`).
    pub fn base(mut self, base: impl Into<String>) -> Self {
        self.base = base.into();
        self
    }

    pub fn max_rounds(mut self, n: u32) -> Self {
        self.max_rounds = n.max(1);
        self
    }

    /// Actor recorded on host-side events (default: the clone's resolved
    /// identity, falling back to `human`).
    pub fn actor(mut self, actor: impl Into<String>) -> Self {
        self.actor = Some(actor.into());
        self
    }

    pub fn launcher(mut self, launcher: Arc<dyn RuntimeLauncher>) -> Self {
        self.launcher = launcher;
        self
    }

    /// Event-log poll interval while waiting on a turn (default 1.5s).
    pub fn poll_interval(mut self, d: Duration) -> Self {
        self.poll_interval = d;
        self
    }

    /// Per-turn deadline (default 30min).
    pub fn turn_timeout(mut self, d: Duration) -> Self {
        self.turn_timeout = d;
        self
    }

    /// Skip hashing the score binary at launch (tests; large-binary hosts).
    pub fn without_score_digest(mut self) -> Self {
        self.score_digest = false;
        self
    }

    /// Record this digest as the score's provenance instead of hashing the
    /// current executable — for bridge hosts (`h5i orchestra serve`) where
    /// the score lives outside this process (e.g. a Python file driving the
    /// run over JSON-RPC).
    pub fn score_digest_override(mut self, digest: impl Into<String>) -> Self {
        self.digest_override = Some(digest.into());
        self
    }

    pub fn launch(self) -> Result<Conductor, H5iError> {
        let repo = Repository::discover(&self.repo)?;
        let workdir = repo
            .workdir()
            .ok_or_else(|| {
                H5iError::Metadata("orchestra requires a non-bare repository".into())
            })?
            .to_path_buf();
        let h5i_root = storage::h5i_root_for_repo(&repo)?;
        storage::ensure_layout(&h5i_root)?;
        let actor = match self.actor {
            Some(a) => a,
            None => msg::resolve_identity(&h5i_root, None).unwrap_or_else(|_| "human".into()),
        };

        let refname = format!("refs/h5i/team/{}", self.run);
        let existing = repo.find_reference(&refname).is_ok();
        if !existing {
            team::create(
                &repo,
                &self.run,
                self.title.as_deref().unwrap_or(&self.run),
                &self.base,
                self.max_rounds,
                &actor,
            )?;
        }

        let journal = Journal::open(&workdir, &self.run, &actor);
        if existing {
            tracing::info!(
                run = %self.run,
                steps = journal.replay_len(),
                "orchestra: resuming run — journaled steps replay without re-execution"
            );
        }
        let digest = match (self.digest_override, self.score_digest) {
            (Some(d), _) => Some(d),
            (None, true) => Journal::current_exe_digest(),
            (None, false) => None,
        };
        if let Some(warning) = journal.record_score_start(digest.as_deref())? {
            tracing::warn!(run = %self.run, "orchestra: {warning}");
        }

        Ok(Conductor {
            core: Arc::new(Core {
                repo_workdir: workdir,
                h5i_root,
                run_id: self.run,
                actor,
                journal,
                launcher: self.launcher,
                poll_interval: self.poll_interval,
                turn_timeout: self.turn_timeout,
            }),
        })
    }
}


/// A step-label namespace (see [`Conductor::scope`]). Scopes nest.
pub struct Scope {
    core: Arc<Core>,
    prefix: String,
}

impl Scope {
    pub fn scope(&self, sub: impl Into<String>) -> Scope {
        Scope {
            core: self.core.clone(),
            prefix: format!("{}/{}", self.prefix, sub.into()),
        }
    }

    /// A journaled step under this scope's label namespace. Takes `self` by
    /// value so a scope can be built and used inline
    /// (`c.scope(format!("item/{i}")).step("fetch", …)`) without a borrow
    /// outliving the temporary.
    pub async fn step<T, F>(self, label: &str, f: F) -> Result<T, H5iError>
    where
        T: Serialize + DeserializeOwned + Send + 'static,
        F: FnOnce() -> Result<T, H5iError> + Send + 'static,
    {
        journaled(
            self.core.clone(),
            format!("{}/{label}", self.prefix),
            move |_core| f(),
        )
        .await
    }
}


// ── Internals ─────────────────────────────────────────────────────────────────

async fn run_blocking<T, F>(f: F) -> Result<T, H5iError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, H5iError> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| H5iError::Internal(format!("orchestra blocking task panicked: {e}")))?
}

/// Execute `f` exactly once under the journal: replay the recorded result when
/// the step key is already journaled, otherwise run live and record. This is
/// the durability kernel every eDSL operation goes through.
async fn journaled<T, F>(core: Arc<Core>, label: String, f: F) -> Result<T, H5iError>
where
    T: Serialize + DeserializeOwned + Send + 'static,
    F: FnOnce(&Core) -> Result<T, H5iError> + Send + 'static,
{
    let key = core.journal.next_key(&label)?;
    if let Some(replayed) = core.journal.replay_as::<T>(&key) {
        tracing::debug!(step = %key, "orchestra: replaying journaled step");
        core.journal.finish(&label);
        return replayed;
    }
    let outer = core.clone();
    let outer_label = label.clone();
    let result = run_blocking(move || {
        let started = Instant::now();
        let value = f(&core)?;
        let duration_ms = started.elapsed().as_millis() as u64;
        core.journal.record(&key, &label, &value, duration_ms)?;
        Ok(value)
    })
    .await;
    // Always release the label — a failed step must stay retryable in-process.
    outer.journal.finish(&outer_label);
    result
}

fn turn_context(
    core: &Core,
    agent_id: &str,
    env_id: &str,
    kind: TurnKind,
    instruction: &str,
) -> TurnContext {
    let work_dir = env::find(&core.h5i_root, env_id)
        .ok()
        .map(|m| m.work_dir(&core.h5i_root))
        .filter(|p| p.exists());
    let seat = core
        .repo()
        .ok()
        .and_then(|repo| team::status(&repo, &core.run_id).ok())
        .and_then(|s| {
            s.run
                .agents
                .iter()
                .find(|a| a.agent_id == agent_id)
                .map(|a| (a.runtime.clone(), a.model.clone()))
        });
    let (runtime, model) = seat.unwrap_or((None, None));
    TurnContext {
        run_id: core.run_id.clone(),
        agent_id: agent_id.to_string(),
        env_id: env_id.to_string(),
        kind,
        instruction: instruction.to_string(),
        repo_workdir: core.repo_workdir.clone(),
        h5i_root: core.h5i_root.clone(),
        work_dir,
        runtime,
        model,
    }
}

/// Deliver one turn: i5h ASK + per-env inbox copy (the same wire format as
/// `team dispatch`, but targeted at one agent), then hand the launcher its
/// signal.
fn dispatch_turn(
    core: &Core,
    agent_id: &str,
    env_id: &str,
    kind: TurnKind,
    instruction: &str,
) -> Result<(), H5iError> {
    let repo = core.repo()?;
    let run = team::status(&repo, &core.run_id)?.run;
    let message = msg::send_msg(
        &repo,
        &core.h5i_root,
        &core.actor,
        agent_id,
        instruction,
        msg::SendOpts {
            kind: Some("ASK".into()),
            links: Some(serde_json::json!({
                "team": core.run_id,
                "round": run.current_round,
                "agent_id": agent_id,
            })),
            ..Default::default()
        },
    )?;
    env::fan_out_to_env_inbox(&core.h5i_root, agent_id, Some(&core.run_id), &message);
    core.launcher
        .on_turn(&turn_context(core, agent_id, env_id, kind, instruction))
}

/// Poll the event log until `probe` yields, draining box-side spools each
/// round so submissions staged inside sealed envs land as events. Interval
/// polling matches every existing wait surface in h5i (`msg wait`, the team
/// hooks); dispatch latency comes from the resident session, not this poll,
/// and a file-watch would add a dependency the repo does not carry.
fn wait_until<T>(
    core: &Core,
    what: &str,
    mut probe: impl FnMut(&Repository) -> Result<Option<T>, H5iError>,
) -> Result<T, H5iError> {
    let repo = core.repo()?;
    let deadline = Instant::now() + core.turn_timeout;
    loop {
        // Best-effort: an env without local spool state must not fail the wait.
        if let Err(e) = team::sync_outbound(&repo, &core.h5i_root, &core.run_id) {
            tracing::debug!("orchestra: sync_outbound: {e}");
        }
        if let Some(found) = probe(&repo)? {
            return Ok(found);
        }
        if Instant::now() >= deadline {
            return Err(H5iError::Metadata(format!(
                "orchestra: timed out after {:?} waiting for {what} — is a resident session \
                 attached for this agent? (bring one up with team-launch.sh, or pass a \
                 launcher to Conductor::builder)",
                core.turn_timeout
            )));
        }
        std::thread::sleep(core.poll_interval);
    }
}

/// Count `submitted` events for an agent — a re-submit of an unchanged
/// candidate still appends one, so this (not a changed id) is how `revise`
/// detects that the agent responded.
fn submission_event_count(
    repo: &Repository,
    run_id: &str,
    agent_id: &str,
) -> Result<usize, H5iError> {
    let events = team::read_events(repo, run_id)?;
    Ok(events
        .iter()
        .filter(|e| e.kind == "submitted")
        .filter(|e| e.payload.get("owner_agent").and_then(|v| v.as_str()) == Some(agent_id))
        .count())
}

fn latest_submission_id(run: &TeamRun, agent_id: &str) -> Option<String> {
    run.agents
        .iter()
        .find(|a| a.agent_id == agent_id)
        .and_then(|a| a.latest_submission_id.clone())
}

fn count_reviews(
    repo: &Repository,
    run_id: &str,
    reviewer: &str,
    target: &str,
) -> Result<usize, H5iError> {
    Ok(review_events(repo, run_id, reviewer, target)?.0)
}

/// Count `review_submitted` events by (reviewer, target) and return the newest
/// matching review, in one event-log pass.
fn review_events(
    repo: &Repository,
    run_id: &str,
    reviewer: &str,
    target: &str,
) -> Result<(usize, Option<TeamReview>), H5iError> {
    let events = team::read_events(repo, run_id)?;
    let mut count = 0usize;
    let mut newest = None;
    for ev in events.iter().filter(|e| e.kind == "review_submitted") {
        if let Ok(review) = serde_json::from_value::<TeamReview>(ev.payload.clone()) {
            if review.reviewer == reviewer && review.target == target {
                count += 1;
                newest = Some(review);
            }
        }
    }
    Ok((count, newest))
}

/// Count `agent_reply` events by agent and return the newest body, one pass.
fn agent_reply_events(
    repo: &Repository,
    run_id: &str,
    agent_id: &str,
) -> Result<(usize, Option<String>), H5iError> {
    let events = team::read_events(repo, run_id)?;
    let mut count = 0usize;
    let mut newest = None;
    for ev in events.iter().filter(|e| e.kind == "agent_reply") {
        if ev.payload.get("agent_id").and_then(|v| v.as_str()) == Some(agent_id) {
            count += 1;
            newest = ev
                .payload
                .get("body")
                .and_then(|v| v.as_str())
                .map(String::from);
        }
    }
    Ok((count, newest))
}

/// Parse an agent's data reply as `T`: whole-body JSON first, then a fenced or
/// embedded JSON value, then (for string-shaped `T`) the raw body itself.
fn parse_json_reply<T: DeserializeOwned>(body: &str) -> Result<T, String> {
    let trimmed = body.trim();
    if let Ok(v) = serde_json::from_str::<T>(trimmed) {
        return Ok(v);
    }
    // ```json … ``` fences.
    let unfenced = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|s| s.strip_suffix("```"))
        .map(str::trim);
    if let Some(inner) = unfenced {
        if let Ok(v) = serde_json::from_str::<T>(inner) {
            return Ok(v);
        }
    }
    // First embedded object/array.
    for (open, close) in [('{', '}'), ('[', ']')] {
        if let (Some(start), Some(end)) = (trimmed.find(open), trimmed.rfind(close)) {
            if start < end {
                if let Ok(v) = serde_json::from_str::<T>(&trimmed[start..=end]) {
                    return Ok(v);
                }
            }
        }
    }
    // A bare unquoted string for `T = String`-shaped types.
    if let Ok(v) = serde_json::from_value::<T>(serde_json::Value::String(trimmed.to_string())) {
        return Ok(v);
    }
    Err(serde_json::from_str::<T>(trimmed)
        .err()
        .map(|e| e.to_string())
        .unwrap_or_else(|| "unparseable reply".into()))
}

/// Approval detection for review bodies and gate replies. Approving verdicts
/// come in many surface forms — `APPROVE`, `LGTM`, and (very commonly from
/// agents) a `Verdict: approve` header — so scanning only the literal first
/// token is too brittle (it read `Verdict: approve` as "Verdict" = not
/// approved, which spuriously triggered revise rounds). Instead: look at the
/// first non-empty line, strip a leading `verdict:`/`decision:`/`result:`
/// label, and check the first remaining word against the approval set. Still
/// conservative — an approval token must lead the (delabeled) first line, so
/// "I can't approve this" or "changes before approve" do not count.
fn first_token_approves(body: &str) -> bool {
    let Some(line) = body.lines().map(str::trim).find(|l| !l.is_empty()) else {
        return false;
    };
    // Strip one leading label like "Verdict:" / "Decision:" / "Result:".
    let rest = line
        .split_once(':')
        .map(|(label, after)| {
            let l = label.trim().to_ascii_lowercase();
            if matches!(l.as_str(), "verdict" | "decision" | "result" | "review" | "status") {
                after.trim()
            } else {
                line
            }
        })
        .unwrap_or(line);
    matches!(
        rest.split_whitespace()
            .next()
            .map(|t| t.trim_matches(|c: char| !c.is_ascii_alphanumeric()).to_ascii_uppercase())
            .as_deref(),
        Some("APPROVE") | Some("APPROVED") | Some("LGTM") | Some("YES") | Some("OK")
    )
}

/// Approval convention applied to a `TeamReview` (see [`first_token_approves`]).
pub fn approves(review: &TeamReview) -> bool {
    first_token_approves(&review.body)
}

#[cfg(test)]
mod tests;
