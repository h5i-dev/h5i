//! Runtime launchers — how an agent turn's resident session is brought up.
//! Split out of `mod.rs` by concern; see the module docs there.

use super::*;

/// What kind of turn is being dispatched to an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnKind {
    /// Produce a candidate and `h5i team agent submit` it.
    Work,
    /// Review `target`'s granted artifacts and `h5i team review submit`.
    Review { target: String },
    /// Address a received review, then re-submit.
    Revise,
    /// Answer with data (JSON) via `h5i team agent reply` — no submission.
    Ask,
}

/// Everything a launcher needs to bring up / drive one agent turn. The
/// instruction is already in the agent's per-env inbox when `on_turn` runs —
/// completion is detected through the event log, never through the launcher.
#[derive(Debug, Clone)]
pub struct TurnContext {
    pub run_id: String,
    pub agent_id: String,
    pub env_id: String,
    pub kind: TurnKind,
    pub instruction: String,
    pub repo_workdir: PathBuf,
    pub h5i_root: PathBuf,
    /// The env's worktree, when materialized on this clone.
    pub work_dir: Option<PathBuf>,
    /// The roster runtime adapter (`claude`, `codex`, …), when recorded.
    pub runtime: Option<String>,
    /// The roster model override (`--model`), when recorded.
    pub model: Option<String>,
}

/// Session bring-up strategy (design doc §5.1). `Attach` is the default: the
/// resident interactive session (Stop-hook held, `team-launch.sh`-style) picks
/// the turn out of its inbox; the launcher does nothing. [`LaunchResident`]
/// spawns that same warm session itself (tmux). A headless per-turn
/// `claude -p` spawn is rejected — cold boots and stateless turns defeat the
/// resident-session execution model.
pub trait RuntimeLauncher: Send + Sync {
    fn on_turn(&self, turn: &TurnContext) -> Result<(), H5iError>;
}

/// The default launcher: rely on resident sessions, do nothing per turn.
pub struct Attach;

impl RuntimeLauncher for Attach {
    fn on_turn(&self, _turn: &TurnContext) -> Result<(), H5iError> {
        Ok(())
    }
}

/// Launch-resident (design doc §5.1): the score brings up the same warm
/// interactive session a human would — `h5i env shell <env> -- <runtime> …`
/// in a detached tmux session, created once per agent and reused for every
/// turn (the Stop hook keeps it parked on the inbox between turns). Requires
/// `tmux` and a roster runtime with a known adapter; fails closed otherwise.
/// This internalizes `scripts/team-launch.sh`'s tmux mode.
pub struct LaunchResident;

impl RuntimeLauncher for LaunchResident {
    fn on_turn(&self, turn: &TurnContext) -> Result<(), H5iError> {
        use std::process::Command;
        let session = format!("h5i-orch-{}-{}", turn.run_id, turn.agent_id);
        let alive = Command::new("tmux")
            .args(["has-session", "-t", &session])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if alive {
            return Ok(());
        }
        // A roster model override (`--model`) — keeps a trivial run on a fast
        // model instead of the session default.
        let model_flag = turn
            .model
            .as_deref()
            .map(|m| format!(" --model {}", shell_quote(m)))
            .unwrap_or_default();
        let runtime_argv = match turn.runtime.as_deref() {
            Some("claude") => format!(
                "claude --dangerously-skip-permissions{model_flag} {}",
                shell_quote(team::AGENT_BOOTSTRAP)
            ),
            Some("codex") => format!(
                "codex --sandbox danger-full-access{model_flag} {}",
                shell_quote(team::AGENT_BOOTSTRAP)
            ),
            Some(other) => {
                return Err(H5iError::Metadata(format!(
                    "orchestra LaunchResident has no adapter for runtime '{other}' — \
                     bring the session up yourself (team-launch.sh) and use Attach"
                )))
            }
            None => {
                return Err(H5iError::Metadata(format!(
                    "orchestra LaunchResident: agent '{}' has no roster runtime — \
                     hire it with .runtime(\"claude\"|\"codex\")",
                    turn.agent_id
                )))
            }
        };
        // `$H5I` overrides the binary, mirroring the scripts' convention —
        // needed when driving a dev build that isn't first on PATH.
        let h5i = std::env::var("H5I").unwrap_or_else(|_| "h5i".into());
        let cmd = format!(
            "{} env shell {} -- {runtime_argv}",
            shell_quote(&h5i),
            turn.env_id
        );
        let spawned = Command::new("tmux")
            .args(["new-session", "-d", "-s", &session, &cmd])
            .status()
            .map_err(|e| {
                H5iError::Metadata(format!(
                    "orchestra LaunchResident requires tmux (spawn failed: {e}) — \
                     install tmux or bring sessions up yourself and use Attach"
                ))
            })?;
        if !spawned.success() {
            return Err(H5iError::Metadata(format!(
                "orchestra LaunchResident: tmux new-session failed for '{session}'"
            )));
        }
        tracing::info!(session = %session, agent = %turn.agent_id, "orchestra: resident session launched");
        Ok(())
    }
}

/// POSIX single-quote escaping for one argv word.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Wrap a closure as a launcher — for tests and for embedding scenarios where
/// the host program itself plays (or spawns) the agent.
pub struct FnLauncher<F>(pub F);

impl<F> RuntimeLauncher for FnLauncher<F>
where
    F: Fn(&TurnContext) -> Result<(), H5iError> + Send + Sync,
{
    fn on_turn(&self, turn: &TurnContext) -> Result<(), H5iError> {
        (self.0)(turn)
    }
}

