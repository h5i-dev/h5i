//! Prebuilt orchestrations, implemented in the public eDSL — readable,
//! forkable, no privileged API. `ensemble` reproduces the classic `h5i team`
//! flow (N independent workers, mutual peer review, revise, verify, verdict);
//! `arena`, `pipeline`, `map_reduce`, `debate`, and `integrate` follow in M4
//! (design doc §4.4, §9).

use super::{approves, Agent, Conductor, VerdictPolicy};
use crate::error::H5iError;
use crate::team::{TeamArtifact, TeamReview, TeamVerdict};
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
            tokio::spawn(async move { a.work(task).await })
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
                c.verify(artifact, cmd.iter().cloned(), isolation.as_deref())
                    .await?;
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
        .map_err(|e| H5iError::Internal(format!("orchestra ensemble task panicked: {e}")))?
}
