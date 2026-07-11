//! Preflight checks — fail the predictable ways up front (dead sessions,
//! weak isolation, dirty apply state) instead of at minute 30. Split out of
//! `mod.rs` by concern.

use super::*;


/// Up-front checks that turn the worst runtime failure modes (dispatching
/// into a dead session and timing out; verdicts on weaker isolation than
/// intended; apply refused at the very end for a dirty tree) into one
/// predictable first error. Read-only, not journaled. All configured checks
/// run; failures are reported together.
pub struct Preflight {
    core: Arc<Core>,
    live: Vec<(String, String)>,
    min_isolation: Option<String>,
    clean_worktree: bool,
}

impl Conductor {
    pub fn preflight(&self) -> Preflight {
        Preflight {
            core: self.core.clone(),
            live: Vec::new(),
            min_isolation: None,
            clean_worktree: false,
        }
    }
}

impl Preflight {
    /// Require a live resident session per agent. Heuristic: an interactive
    /// session holds its env's writer lock, so the lock being free across
    /// several samples means nothing is attached.
    pub fn require_live<'a>(mut self, agents: impl IntoIterator<Item = &'a Agent>) -> Self {
        self.live.extend(
            agents
                .into_iter()
                .map(|a| (a.name.clone(), a.env_id.clone())),
        );
        self
    }

    /// Require every roster env to claim at least this isolation tier
    /// (`workspace` < `process` < `supervised` < `container`).
    pub fn require_isolation(mut self, tier: impl Into<String>) -> Self {
        self.min_isolation = Some(tier.into());
        self
    }

    /// Require a clean host working tree (what `apply` will demand at the
    /// very end — fail now instead).
    pub fn require_clean_worktree(mut self) -> Self {
        self.clean_worktree = true;
        self
    }

    pub async fn run(self) -> Result<(), H5iError> {
        let Preflight {
            core,
            live,
            min_isolation,
            clean_worktree,
        } = self;
        run_blocking(move || {
            let mut failures: Vec<String> = Vec::new();

            for (agent, env_id) in &live {
                match env::find(&core.h5i_root, env_id) {
                    Ok(m) => {
                        let dir = m.dir(&core.h5i_root);
                        // Sample twice with a gap: a brief host op can hold the
                        // lock for one sample; a resident session holds it for
                        // both. Dead = free on every sample.
                        let mut held = env::writer_session_live(&dir);
                        if !held {
                            std::thread::sleep(Duration::from_millis(250));
                            held = env::writer_session_live(&dir);
                        }
                        if !held {
                            failures.push(format!(
                                "no live session for '{agent}' ({env_id}) — bring one up \
                                 (team-launch.sh / LaunchResident) or dispatch will wait \
                                 out the full turn timeout"
                            ));
                        }
                    }
                    Err(_) => failures.push(format!(
                        "agent '{agent}': env {env_id} is not materialized on this clone"
                    )),
                }
            }

            if let Some(min) = &min_isolation {
                let rank = |t: &str| match t {
                    "workspace" => Some(0),
                    "process" => Some(1),
                    "supervised" => Some(2),
                    "container" => Some(3),
                    _ => None,
                };
                match rank(min) {
                    None => failures.push(format!("unknown isolation tier '{min}'")),
                    Some(need) => {
                        let run = team::status(&core.repo()?, &core.run_id)?.run;
                        for a in &run.agents {
                            match rank(&a.isolation_claim) {
                                Some(got) if got >= need => {}
                                _ => failures.push(format!(
                                    "agent '{}' env claims isolation '{}' — below the \
                                     required '{min}'",
                                    a.agent_id, a.isolation_claim
                                )),
                            }
                        }
                    }
                }
            }

            if clean_worktree {
                let repo = core.repo()?;
                let mut opts = git2::StatusOptions::new();
                opts.include_untracked(true).recurse_untracked_dirs(true);
                if !repo.statuses(Some(&mut opts))?.is_empty() {
                    failures.push(
                        "host working tree is not clean — apply will refuse; commit or \
                         stash first"
                            .into(),
                    );
                }
            }

            if failures.is_empty() {
                Ok(())
            } else {
                Err(H5iError::Metadata(format!(
                    "orchestra preflight failed:\n  - {}",
                    failures.join("\n  - ")
                )))
            }
        })
        .await
    }
}
