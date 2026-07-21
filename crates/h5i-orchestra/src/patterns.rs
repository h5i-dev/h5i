//! Prebuilt orchestrations, implemented in the public eDSL — readable,
//! forkable, no privileged API (design doc §4.4). `ensemble` reproduces the
//! classic `h5i team` flow; `integrate` is the multi-implementer merge seat;
//! `pipeline` chains role-specialized stages; `arena` ranks independent
//! attempts; `map_reduce` fans a work list out and merges; `debate` argues a
//! question through `ask` turns.
//!
//! Roster note: every agent a pattern uses must be hired before the round is
//! sealed (`add_env` is open-round-only) — hire integrators/moderators up
//! front, alongside the workers.

use super::{approves, Agent, Conductor, VerdictPolicy};
use h5i_core::error::H5iError;
use h5i_core::team::{TeamArtifact, TeamCompareRow, TeamReview, TeamVerdict, TeamVerification};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The classic ensemble: every agent attempts `task` independently, the round
/// is sealed, agents mutually review and revise for up to `rounds` cycles,
/// then (optionally) a neutral verifier runs and a policy decides. Apply is
/// never automatic — the caller inspects the outcome and applies.
pub fn ensemble(c: &Conductor, task: impl Into<String>) -> Ensemble {
    Ensemble {
        c: c.clone(),
        task: task.into(),
        agents: Vec::new(),
        rounds: 1,
        verify_cmd: None,
        verify_tests_from: None,
        isolation: None,
        policy: None,
    }
}

pub struct Ensemble {
    c: Conductor,
    task: String,
    agents: Vec<Agent>,
    rounds: u32,
    verify_cmd: Option<Vec<String>>,
    verify_tests_from: Option<String>,
    isolation: Option<String>,
    policy: Option<Box<dyn VerdictPolicy>>,
}

pub struct EnsembleOutcome {
    /// Each agent's latest artifact after the review cycles, by agent id.
    pub artifacts: Vec<TeamArtifact>,
    /// Every review posted across all cycles.
    pub reviews: Vec<TeamReview>,
    /// The recorded verdict, when a verifier command or policy was configured.
    pub verdict: Option<TeamVerdict>,
    /// Review/revise cycles actually run (early exit on full approval).
    pub rounds_run: u32,
}

impl Ensemble {
    pub fn agents(mut self, agents: impl IntoIterator<Item = Agent>) -> Self {
        self.agents.extend(agents);
        self
    }

    /// Maximum review→revise cycles (default 1); exits early once every
    /// artifact is approved by all of its reviewers.
    pub fn rounds(mut self, n: u32) -> Self {
        self.rounds = n;
        self
    }

    /// Neutral verifier command run against every candidate (same command for
    /// all — divergent commands are refused at verdict time).
    pub fn verify<I, S>(mut self, command: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.verify_cmd = Some(command.into_iter().map(Into::into).collect());
        self
    }

    /// Seal the verifier's test set: a submission id or team agent id whose
    /// base..commit diff is overlaid over every candidate before the verify
    /// command runs (see `Conductor::verify_with_tests`). The sealing agent
    /// must not be one of the candidates (self-sealing fails closed).
    pub fn verify_tests_from(mut self, tests_from: impl Into<String>) -> Self {
        self.verify_tests_from = Some(tests_from.into());
        self
    }

    /// Isolation tier for the verifier (`workspace`/`process`/`container`…).
    pub fn isolation(mut self, tier: impl Into<String>) -> Self {
        self.isolation = Some(tier.into());
        self
    }

    /// Verdict policy (default: the built-in rule when `verify` is set,
    /// otherwise no verdict is recorded).
    pub fn judge<P: VerdictPolicy + 'static>(mut self, policy: P) -> Self {
        self.policy = Some(Box::new(policy));
        self
    }

    pub async fn run(self) -> Result<EnsembleOutcome, H5iError> {
        let Ensemble {
            c,
            task,
            agents,
            rounds,
            verify_cmd,
            verify_tests_from,
            isolation,
            policy,
        } = self;
        if agents.len() < 2 {
            return Err(H5iError::Metadata(
                "orchestra ensemble needs at least two agents".into(),
            ));
        }

        // 1. Independent first attempts, in parallel.
        let mut latest: BTreeMap<String, TeamArtifact> = BTreeMap::new();
        let attempts = agents.iter().map(|a| {
            let (a, task) = (a.clone(), task.clone());
            tokio::spawn(async move { a.work(task).expect_independent().await })
        });
        for (agent, handle) in agents.iter().zip(attempts.collect::<Vec<_>>()) {
            let artifact = join_flat(handle).await?;
            latest.insert(agent.id().to_string(), artifact);
        }

        // 2. Seal the round: no cross-agent influence before every first
        //    attempt is frozen (the independence invariant).
        c.freeze().await?;

        // 3. Mutual review → revise cycles, host-language loop.
        let mut all_reviews: Vec<TeamReview> = Vec::new();
        let mut rounds_run = 0;
        for _ in 0..rounds {
            rounds_run += 1;
            // Every ordered (reviewer, target) pair, in parallel.
            let mut handles = Vec::new();
            for reviewer in &agents {
                for target in &agents {
                    if reviewer.id() == target.id() {
                        continue;
                    }
                    let (r, art) = (reviewer.clone(), latest[target.id()].clone());
                    handles.push(tokio::spawn(async move { r.review(&art).await }));
                }
            }
            let mut cycle: Vec<TeamReview> = Vec::new();
            for handle in handles {
                cycle.push(join_flat(handle).await?);
            }

            // Revise every artifact that a reviewer did not approve; feedback
            // from several reviewers is merged into one revise turn.
            let mut revise_handles = Vec::new();
            for agent in &agents {
                let received: Vec<&TeamReview> = cycle
                    .iter()
                    .filter(|r| r.target == agent.id())
                    .collect();
                if received.iter().all(|r| approves(r)) {
                    continue;
                }
                let merged = TeamReview {
                    reviewer: received
                        .iter()
                        .map(|r| r.reviewer.as_str())
                        .collect::<Vec<_>>()
                        .join("+"),
                    target: agent.id().to_string(),
                    round: received.first().map(|r| r.round).unwrap_or(1),
                    body: received
                        .iter()
                        .map(|r| format!("[{}]\n{}", r.reviewer, r.body))
                        .collect::<Vec<_>>()
                        .join("\n\n"),
                    referenced_artifacts: vec![latest[agent.id()].id.clone()],
                };
                let (a, art) = (agent.clone(), latest[agent.id()].clone());
                revise_handles.push((
                    agent.id().to_string(),
                    tokio::spawn(async move { a.revise(&art, &merged).await }),
                ));
            }
            let everyone_approved = revise_handles.is_empty();
            for (agent_id, handle) in revise_handles {
                let artifact = join_flat(handle).await?;
                latest.insert(agent_id, artifact);
            }
            all_reviews.extend(cycle);
            if everyone_approved {
                break;
            }
        }

        // 4. Neutral verification, one artifact at a time (verify worktrees
        //    share on-disk state; parallel `git worktree add` is racy).
        if let Some(cmd) = &verify_cmd {
            for artifact in latest.values() {
                match &verify_tests_from {
                    Some(tf) => {
                        c.verify_with_tests(artifact, tf, cmd.iter().cloned(), isolation.as_deref())
                            .await?
                    }
                    None => {
                        c.verify(artifact, cmd.iter().cloned(), isolation.as_deref())
                            .await?
                    }
                };
            }
        }

        // 5. Verdict.
        let verdict = match (policy, &verify_cmd) {
            (Some(p), _) => Some(c.judge(p).await?),
            (None, Some(_)) => Some(c.judge(super::policy::tests_then_smallest_diff()).await?),
            (None, None) => None,
        };

        Ok(EnsembleOutcome {
            artifacts: latest.into_values().collect(),
            reviews: all_reviews,
            verdict,
            rounds_run,
        })
    }
}

async fn join_flat<T>(
    handle: tokio::task::JoinHandle<Result<T, H5iError>>,
) -> Result<T, H5iError> {
    handle
        .await
        .map_err(|e| H5iError::Internal(format!("orchestra pattern task panicked: {e}")))?
}

// ── integrate ─────────────────────────────────────────────────────────────────

/// The multi-implementer merge seat (design doc §4.3/§4.4): seal the round,
/// then one integrator fuses the given parts in its own env — granted their
/// diffs as materials, honestly stamped non-independent — and optionally the
/// merged artifact is neutrally verified.
pub fn integrate(c: &Conductor, task: impl Into<String>) -> Integrate {
    Integrate {
        c: c.clone(),
        task: task.into(),
        parts: Vec::new(),
        integrator: None,
        verify_cmd: None,
        verify_tests_from: None,
        isolation: None,
    }
}

pub struct Integrate {
    c: Conductor,
    task: String,
    parts: Vec<TeamArtifact>,
    integrator: Option<Agent>,
    verify_cmd: Option<Vec<String>>,
    verify_tests_from: Option<String>,
    isolation: Option<String>,
}

pub struct IntegrateOutcome {
    pub merged: TeamArtifact,
    pub verification: Option<TeamVerification>,
}

impl Integrate {
    pub fn parts<'a>(mut self, parts: impl IntoIterator<Item = &'a TeamArtifact>) -> Self {
        self.parts.extend(parts.into_iter().cloned());
        self
    }

    pub fn integrator(mut self, agent: Agent) -> Self {
        self.integrator = Some(agent);
        self
    }

    pub fn verify<I, S>(mut self, command: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.verify_cmd = Some(command.into_iter().map(Into::into).collect());
        self
    }

    /// Seal the verifier's test set to one of the parts (typically a
    /// test-designer's artifact — pass its submission id, or the designer's
    /// team agent id): the integrator's merged candidate is verified against
    /// that part's tests as overlaid at verify time, so a merge cannot
    /// weaken them (see `Conductor::verify_with_tests`).
    pub fn verify_tests_from(mut self, tests_from: impl Into<String>) -> Self {
        self.verify_tests_from = Some(tests_from.into());
        self
    }

    pub fn isolation(mut self, tier: impl Into<String>) -> Self {
        self.isolation = Some(tier.into());
        self
    }

    pub async fn run(self) -> Result<IntegrateOutcome, H5iError> {
        let integrator = self.integrator.ok_or_else(|| {
            H5iError::Metadata("orchestra integrate needs an integrator agent".into())
        })?;
        if self.parts.is_empty() {
            return Err(H5iError::Metadata(
                "orchestra integrate needs at least one part".into(),
            ));
        }
        // Materials ride the discuss channel, which is sealed-phase-only.
        self.c.freeze().await?;
        let merged = integrator
            .work(format!(
                "{task}\n\nMerge the granted teammate artifacts into one coherent candidate: \
                 apply their patches in this worktree, resolve conflicts (prefer a mechanical \
                 `git merge`/`git apply` first; use judgment only where the changes genuinely \
                 collide), and make the result build.",
                task = self.task
            ))
            .with_materials(self.parts.iter())
            .await?;
        let verification = match &self.verify_cmd {
            Some(cmd) => Some(match &self.verify_tests_from {
                Some(tf) => {
                    self.c
                        .verify_with_tests(&merged, tf, cmd.iter().cloned(), self.isolation.as_deref())
                        .await?
                }
                None => {
                    self.c
                        .verify(&merged, cmd.iter().cloned(), self.isolation.as_deref())
                        .await?
                }
            }),
            None => None,
        };
        Ok(IntegrateOutcome {
            merged,
            verification,
        })
    }
}

// ── pipeline ──────────────────────────────────────────────────────────────────

/// Role-specialized stages in sequence (architect → implementer → reviewer …):
/// stage 1 works independently; the round is sealed; every later stage gets
/// the previous stage's artifact as material. Returns one artifact per stage,
/// in order.
pub async fn pipeline(
    c: &Conductor,
    stages: Vec<(Agent, String)>,
) -> Result<Vec<TeamArtifact>, H5iError> {
    if stages.is_empty() {
        return Err(H5iError::Metadata(
            "orchestra pipeline needs at least one stage".into(),
        ));
    }
    let mut artifacts: Vec<TeamArtifact> = Vec::new();
    for (i, (agent, task)) in stages.into_iter().enumerate() {
        let artifact = if i == 0 {
            let first = agent.work(task).await?;
            c.freeze().await?;
            first
        } else {
            let prev = artifacts.last().expect("stage > 0 has a predecessor");
            agent.work(task).with_materials([prev]).await?
        };
        artifacts.push(artifact);
    }
    Ok(artifacts)
}

// ── arena ─────────────────────────────────────────────────────────────────────

/// Independent attempts, ranked: N agents try the same task with no cross-
/// influence, the round seals, every candidate is (optionally) neutrally
/// verified with one command, a policy decides, and the roster comparison
/// rows come back alongside the verdict.
pub fn arena(c: &Conductor, task: impl Into<String>) -> Arena {
    Arena {
        c: c.clone(),
        task: task.into(),
        agents: Vec::new(),
        verify_cmd: None,
        verify_tests_from: None,
        isolation: None,
        policy: None,
    }
}

pub struct Arena {
    c: Conductor,
    task: String,
    agents: Vec<Agent>,
    verify_cmd: Option<Vec<String>>,
    verify_tests_from: Option<String>,
    isolation: Option<String>,
    policy: Option<Box<dyn VerdictPolicy>>,
}

pub struct ArenaOutcome {
    pub artifacts: Vec<TeamArtifact>,
    pub rows: Vec<TeamCompareRow>,
    pub verdict: Option<TeamVerdict>,
}

impl Arena {
    pub fn agents(mut self, agents: impl IntoIterator<Item = Agent>) -> Self {
        self.agents.extend(agents);
        self
    }

    pub fn verify<I, S>(mut self, command: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.verify_cmd = Some(command.into_iter().map(Into::into).collect());
        self
    }

    /// Seal the verifier's test set: a submission id or team agent id (an
    /// agent OUTSIDE the arena roster — typically a test designer who
    /// submitted before the arena) whose base..commit diff is overlaid over
    /// every candidate before the verify command runs (see
    /// `Conductor::verify_with_tests`). Sealing from one of the competing
    /// candidates fails closed on that candidate's own verification.
    pub fn verify_tests_from(mut self, tests_from: impl Into<String>) -> Self {
        self.verify_tests_from = Some(tests_from.into());
        self
    }

    pub fn isolation(mut self, tier: impl Into<String>) -> Self {
        self.isolation = Some(tier.into());
        self
    }

    pub fn judge<P: VerdictPolicy + 'static>(mut self, policy: P) -> Self {
        self.policy = Some(Box::new(policy));
        self
    }

    pub async fn run(self) -> Result<ArenaOutcome, H5iError> {
        let Arena {
            c,
            task,
            agents,
            verify_cmd,
            verify_tests_from,
            isolation,
            policy,
        } = self;
        if agents.len() < 2 {
            return Err(H5iError::Metadata(
                "orchestra arena needs at least two agents".into(),
            ));
        }
        let handles: Vec<_> = agents
            .iter()
            .map(|a| {
                let (a, task) = (a.clone(), task.clone());
                tokio::spawn(async move { a.work(task).expect_independent().await })
            })
            .collect();
        let mut artifacts = Vec::new();
        for handle in handles {
            artifacts.push(join_flat(handle).await?);
        }
        c.freeze().await?;
        if let Some(cmd) = &verify_cmd {
            for artifact in &artifacts {
                match &verify_tests_from {
                    Some(tf) => {
                        c.verify_with_tests(artifact, tf, cmd.iter().cloned(), isolation.as_deref())
                            .await?
                    }
                    None => {
                        c.verify(artifact, cmd.iter().cloned(), isolation.as_deref())
                            .await?
                    }
                };
            }
        }
        let verdict = match (policy, &verify_cmd) {
            (Some(p), _) => Some(c.judge(p).await?),
            (None, Some(_)) => Some(c.judge(super::policy::tests_then_smallest_diff()).await?),
            (None, None) => None,
        };
        let rows = c.compare().await?;
        Ok(ArenaOutcome {
            artifacts,
            rows,
            verdict,
        })
    }
}

// ── map_reduce ────────────────────────────────────────────────────────────────

/// Fan a work list out and merge: each `(agent, task)` assignment runs as its
/// own work turn (assignments to the *same* agent run sequentially — one
/// resident session, and one journal label, per agent), then the round seals
/// and the reducer fuses every part with materials. The reduce seat is
/// exactly the conflict-resolution seat (design doc §9 M4).
pub fn map_reduce(c: &Conductor) -> MapReduce {
    MapReduce {
        c: c.clone(),
        assignments: Vec::new(),
        reducer: None,
    }
}

pub struct MapReduce {
    c: Conductor,
    assignments: Vec<(Agent, String)>,
    reducer: Option<(Agent, String)>,
}

pub struct MapReduceOutcome {
    pub parts: Vec<TeamArtifact>,
    pub merged: Option<TeamArtifact>,
}

impl MapReduce {
    pub fn map(mut self, agent: Agent, task: impl Into<String>) -> Self {
        self.assignments.push((agent, task.into()));
        self
    }

    pub fn reduce(mut self, integrator: Agent, task: impl Into<String>) -> Self {
        self.reducer = Some((integrator, task.into()));
        self
    }

    pub async fn run(self) -> Result<MapReduceOutcome, H5iError> {
        let MapReduce {
            c,
            assignments,
            reducer,
        } = self;
        if assignments.is_empty() {
            return Err(H5iError::Metadata(
                "orchestra map_reduce needs at least one assignment".into(),
            ));
        }
        // Group by agent: cross-agent parallel, same-agent sequential (the
        // journal's per-label discipline and the one-session-per-agent model
        // both require it).
        let mut by_agent: BTreeMap<String, (Agent, Vec<String>)> = BTreeMap::new();
        for (agent, task) in assignments {
            by_agent
                .entry(agent.id().to_string())
                .or_insert_with(|| (agent.clone(), Vec::new()))
                .1
                .push(task);
        }
        let handles: Vec<_> = by_agent
            .into_values()
            .map(|(agent, tasks)| {
                tokio::spawn(async move {
                    let mut parts = Vec::new();
                    for task in tasks {
                        parts.push(agent.work(task).await?);
                    }
                    Ok::<_, H5iError>(parts)
                })
            })
            .collect();
        let mut parts = Vec::new();
        for handle in handles {
            parts.extend(join_flat(handle).await?);
        }
        let merged = match reducer {
            Some((integrator, task)) => Some(
                integrate(&c, task)
                    .parts(parts.iter())
                    .integrator(integrator)
                    .run()
                    .await?
                    .merged,
            ),
            None => {
                c.freeze().await?;
                None
            }
        };
        Ok(MapReduceOutcome { parts, merged })
    }
}

// ── judge_panel ───────────────────────────────────────────────────────────────

/// One judge's scored ballot for a candidate. The judge must ground its call
/// in the run's recorded evidence: `cited_ids` are artifact / verification /
/// review ids that the panel validates against actual run state (a hallucinated
/// citation triggers a re-ask). This is the differentiator — LLM *judgment over
/// recorded evidence*, not vibes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ballot {
    /// Candidate artifact id this ballot scores.
    pub artifact_id: String,
    /// 0–10.
    pub score: u32,
    pub rationale: String,
    /// Evidence ids the rationale is grounded in (artifact/verification/review).
    #[serde(default)]
    pub cited_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JudgeCard {
    ballots: Vec<Ballot>,
}

pub struct JudgePanelOutcome {
    /// Every validated ballot, by judge id.
    pub ballots: Vec<(String, Vec<Ballot>)>,
    /// The recorded verdict (highest mean score; ties broken by smallest diff).
    pub verdict: TeamVerdict,
}

/// A panel of judge agents scores the sealed candidates over the run's
/// recorded evidence, citations are validated against real ids (bounded
/// re-ask on a hallucinated citation), and the mean-score winner is recorded
/// as the verdict. Judges are read-only seats — they never submit.
pub fn judge_panel(c: &Conductor, rubric: impl Into<String>) -> JudgePanel {
    JudgePanel {
        c: c.clone(),
        rubric: rubric.into(),
        judges: Vec::new(),
    }
}

pub struct JudgePanel {
    c: Conductor,
    rubric: String,
    judges: Vec<Agent>,
}

impl JudgePanel {
    pub fn judges(mut self, judges: impl IntoIterator<Item = Agent>) -> Self {
        self.judges.extend(judges);
        self
    }

    pub async fn run(self) -> Result<JudgePanelOutcome, H5iError> {
        let JudgePanel { c, rubric, judges } = self;
        if judges.is_empty() {
            return Err(H5iError::Metadata(
                "orchestra judge_panel needs at least one judge".into(),
            ));
        }
        let status = c.status().await?;
        let candidates: Vec<&TeamArtifact> = status.run.submissions.iter().collect();
        if candidates.is_empty() {
            return Err(H5iError::Metadata(
                "orchestra judge_panel: no submissions to judge (freeze/collect first)".into(),
            ));
        }

        // The evidence menu the judges must cite from: submission ids +
        // verification ids + review ids actually present in the run.
        let valid_ids: std::collections::BTreeSet<String> = status
            .run
            .submissions
            .iter()
            .map(|s| s.id.clone())
            .chain(status.run.verifications.iter().map(|v| v.id.clone()))
            .collect();
        let candidate_ids: Vec<String> = candidates.iter().map(|s| s.id.clone()).collect();
        let evidence = render_evidence(&status.run);

        let mut ballots: Vec<(String, Vec<Ballot>)> = Vec::new();
        for judge in &judges {
            let prompt = format!(
                "You are a neutral judge on a review panel. Rubric: {rubric}\n\n\
                 Score EACH candidate 0-10, grounding every rationale in the recorded \
                 evidence below (cite the exact ids you used). Do not run the code; judge \
                 from the evidence.\n\nCandidates: {}\n\nEvidence:\n{evidence}\n\n\
                 Reply as JSON: {{\"ballots\": [{{\"artifact_id\": \"<id>\", \"score\": \
                 <0-10>, \"rationale\": \"<why, citing ids>\", \"cited_ids\": [\"<id>\", …]}}]}}.",
                candidate_ids.join(", ")
            );
            // The judge is a read-only seat: it must not have a submission this
            // round. `ask` never submits, so this holds by construction.
            let card = ask_with_valid_citations(judge, &prompt, &valid_ids, &candidate_ids).await?;
            ballots.push((judge.id().to_string(), card.ballots));
        }

        // Aggregation is a VerdictPolicy — `VerdictPolicy` is the real
        // primitive; the panel's contribution is eliciting evidence-cited
        // ballots, not owning a bespoke verdict. The mean-score rule is
        // expressed as one policy (via `policy::from_fn`) and recorded through
        // the same `c.judge` path as any other verdict, so a caller could just
        // as well score the same ballots with their own policy (median,
        // quorum, veto). Ballots close into the policy; the run's submissions
        // supply the smallest-diff tiebreak.
        let flat: Vec<Ballot> = ballots.iter().flat_map(|(_, bs)| bs.clone()).collect();
        let n_judges = judges.len();
        let verdict = c
            .judge(super::policy::from_fn("panel:mean-score", move |run| {
                Ok(mean_score_verdict(&flat, n_judges, run))
            }))
            .await?;
        Ok(JudgePanelOutcome { ballots, verdict })
    }
}

/// Mean-score aggregation over judge ballots, expressed as pure logic so it
/// can back a `VerdictPolicy`: highest mean score wins, ties broken by the
/// smallest diff. A panel is advisory over evidence (not a neutral
/// re-execution), so the verdict is never auto-applicable — apply stays an
/// explicit human decision.
fn mean_score_verdict(
    ballots: &[Ballot],
    n_judges: usize,
    run: &h5i_core::team::TeamRun,
) -> TeamVerdict {
    let method = format!("panel:mean-score({n_judges} judges)");
    let mut best: Option<(String, f64)> = None;
    for cand in &run.submissions {
        let scores: Vec<u32> = ballots
            .iter()
            .filter(|b| b.artifact_id == cand.id)
            .map(|b| b.score.min(10))
            .collect();
        if scores.is_empty() {
            continue;
        }
        let mean = scores.iter().sum::<u32>() as f64 / scores.len() as f64;
        let better = match &best {
            None => true,
            Some((cur_id, cur_mean)) => {
                mean > *cur_mean + f64::EPSILON
                    || ((mean - *cur_mean).abs() <= f64::EPSILON
                        && smaller_diff(cand, cur_id, &run.submissions))
            }
        };
        if better {
            best = Some((cand.id.clone(), mean));
        }
    }
    match best {
        Some((id, mean)) => TeamVerdict {
            selected_submission: Some(id.clone()),
            method,
            decided_by: "judge-panel".into(),
            can_auto_apply: false,
            reasons: vec![format!("{id} won the panel with mean score {mean:.1}/10")],
        },
        None => TeamVerdict {
            selected_submission: None,
            method,
            decided_by: "judge-panel".into(),
            can_auto_apply: false,
            reasons: vec!["no candidate received a ballot".into()],
        },
    }
}

/// Ask a judge for a card, re-asking (bounded) if it cites ids not in the run
/// or scores an artifact that isn't a candidate — this is what makes the
/// panel evidence-grounded rather than free-associating.
async fn ask_with_valid_citations(
    judge: &Agent,
    base_prompt: &str,
    valid_ids: &std::collections::BTreeSet<String>,
    candidate_ids: &[String],
    ) -> Result<JudgeCard, H5iError> {
    let mut prompt = base_prompt.to_string();
    for attempt in 0..3 {
        let card: JudgeCard = judge.ask(&prompt).await?;
        let mut problems: Vec<String> = Vec::new();
        for b in &card.ballots {
            if !candidate_ids.contains(&b.artifact_id) {
                problems.push(format!("scored non-candidate '{}'", b.artifact_id));
            }
            for cited in &b.cited_ids {
                if !valid_ids.contains(cited) {
                    problems.push(format!("cited unknown evidence id '{cited}'"));
                }
            }
        }
        if problems.is_empty() {
            return Ok(card);
        }
        if attempt == 2 {
            return Err(H5iError::Metadata(format!(
                "orchestra judge_panel: judge '{}' kept citing invalid evidence: {}",
                judge.id(),
                problems.join("; ")
            )));
        }
        prompt = format!(
            "{base_prompt}\n\nYour previous reply had problems: {}. Score ONLY the listed \
             candidates and cite ONLY ids that appear in the evidence.",
            problems.join("; ")
        );
    }
    unreachable!()
}

fn render_evidence(run: &h5i_core::team::TeamRun) -> String {
    let mut s = String::new();
    s.push_str("Submissions:\n");
    for sub in &run.submissions {
        s.push_str(&format!(
            "- {} by {} (round {}, +{}/-{} over {} files, independent={})\n",
            sub.id,
            sub.owner_agent,
            sub.round,
            sub.insertions,
            sub.deletions,
            sub.files_changed,
            sub.independent
        ));
    }
    if !run.verifications.is_empty() {
        s.push_str("Verifications:\n");
        for v in &run.verifications {
            s.push_str(&format!(
                "- {} for {} (applies_cleanly={}, tests_passed={}, cmd `{}`)\n",
                v.id,
                v.submission_id,
                v.applies_cleanly,
                v.tests_passed,
                v.command.join(" ")
            ));
        }
    }
    s
}

fn smaller_diff(cand: &TeamArtifact, other_id: &str, all: &[TeamArtifact]) -> bool {
    match all.iter().find(|a| a.id == other_id) {
        Some(other) => {
            (cand.files_changed, cand.insertions, &cand.id)
                < (other.files_changed, other.insertions, &other.id)
        }
        None => false,
    }
}

// ── debate ────────────────────────────────────────────────────────────────────

/// The moderator's structured conclusion of a debate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebateConclusion {
    /// The prevailing side's agent id.
    pub winner: String,
    pub rationale: String,
}

pub struct DebateOutcome {
    /// `(agent_id, argument)` in speaking order.
    pub transcript: Vec<(String, String)>,
    pub conclusion: Option<DebateConclusion>,
}

/// Argue a question through data turns: each side speaks in alternating
/// order for `rounds` rounds (seeing the transcript so far), then an optional
/// moderator concludes. Pure `ask` — no artifacts, no freeze.
pub fn debate(c: &Conductor, question: impl Into<String>) -> Debate {
    Debate {
        _c: c.clone(),
        question: question.into(),
        sides: Vec::new(),
        moderator: None,
        rounds: 1,
    }
}

pub struct Debate {
    _c: Conductor,
    question: String,
    sides: Vec<Agent>,
    moderator: Option<Agent>,
    rounds: u32,
}

impl Debate {
    pub fn sides(mut self, sides: impl IntoIterator<Item = Agent>) -> Self {
        self.sides.extend(sides);
        self
    }

    pub fn moderator(mut self, agent: Agent) -> Self {
        self.moderator = Some(agent);
        self
    }

    pub fn rounds(mut self, n: u32) -> Self {
        self.rounds = n.max(1);
        self
    }

    pub async fn run(self) -> Result<DebateOutcome, H5iError> {
        if self.sides.len() < 2 {
            return Err(H5iError::Metadata(
                "orchestra debate needs at least two sides".into(),
            ));
        }
        let mut transcript: Vec<(String, String)> = Vec::new();
        for round in 1..=self.rounds {
            for side in &self.sides {
                let context = if transcript.is_empty() {
                    String::from("You open the debate.")
                } else {
                    let mut s = String::from("Transcript so far:\n");
                    for (who, what) in &transcript {
                        s.push_str(&format!("- {who}: {what}\n"));
                    }
                    s
                };
                let argument: String = side
                    .ask(format!(
                        "Debate (round {round}/{rounds}): {question}\n\n{context}\n\nMake your \
                         strongest argument for your side, as a single JSON string.",
                        rounds = self.rounds,
                        question = self.question,
                    ))
                    .await?;
                transcript.push((side.id().to_string(), argument));
            }
        }
        let conclusion = match &self.moderator {
            Some(moderator) => {
                let mut s = String::new();
                for (who, what) in &transcript {
                    s.push_str(&format!("- {who}: {what}\n"));
                }
                Some(
                    moderator
                        .ask::<DebateConclusion>(format!(
                            "You moderate this debate: {question}\n\nTranscript:\n{s}\nDecide \
                             which side prevailed. Reply as JSON: {{\"winner\": \
                             \"<agent-id>\", \"rationale\": \"<why>\"}}.",
                            question = self.question,
                        ))
                        .await?,
                )
            }
            None => None,
        };
        Ok(DebateOutcome {
            transcript,
            conclusion,
        })
    }
}
