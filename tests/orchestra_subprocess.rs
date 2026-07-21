//! Deterministic cross-process acceptance for the orchestra driver — the gap
//! the live run exposed. The in-process scripted-launcher unit tests
//! (`src/orchestra/tests.rs`) call `team::submit` on the host's own handle, so
//! they never exercise the real path where a **separate box process** writes
//! the submission and the host must observe it. Here a scripted `sh` subprocess
//! plays the agent inside a real `h5i env shell`, so the whole box→host
//! submit/review/ingest path runs for real — no LLM, fully deterministic.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use h5i_core::error::H5iError;
use h5i_orchestra::{patterns, Conductor, FnLauncher, RuntimeLauncher, TurnContext, TurnKind};

fn h5i_bin() -> &'static str {
    env!("CARGO_BIN_EXE_h5i")
}

fn git_ok(dir: &Path, args: &[&str]) {
    let st = Command::new("git").args(args).current_dir(dir).status().unwrap();
    assert!(st.success(), "git {args:?} failed");
}

/// Run one `h5i env shell <env> -- sh -c <script>` as a real subprocess (the
/// box). The script uses `$H5I` for the in-box `h5i` so it runs the binary
/// under test, not whatever stale build is first on the box's PATH (the box
/// resolves bare `h5i` from PATH, which is often an installed release).
fn box_shell(repo: &Path, env_id: &str, script: &str) -> std::process::Output {
    Command::new(h5i_bin())
        .args(["env", "shell", env_id, "--", "sh", "-c", script])
        .current_dir(repo)
        .env("H5I_AGENT", "human")
        .env("H5I", h5i_bin())
        .output()
        .expect("spawn env shell")
}

/// A launcher where a scripted subprocess plays the agent: on a work/revise
/// turn it makes a change, commits, and `h5i team agent submit`s; on a review
/// turn it posts an APPROVE. Each runs in its own process, so the host must
/// observe the writes across a process boundary.
fn subprocess_launcher(repo: std::path::PathBuf) -> Arc<dyn RuntimeLauncher> {
    Arc::new(FnLauncher(move |turn: &TurnContext| {
        let out = match &turn.kind {
            TurnKind::Work | TurnKind::Revise => {
                let n = if matches!(turn.kind, TurnKind::Work) { "w" } else { "r" };
                // worker1 commits manually; worker2 does NOT — it relies on the
                // auto-commit-on-submit fix (the exact workspace-tier failure
                // mode the live run hit). Both must land a real submission.
                let commit = if turn.agent_id == "worker1" {
                    format!("git add {a}_{n}.txt && git commit -q -m '{a} {n}' && ", a = turn.agent_id)
                } else {
                    String::new()
                };
                let script = format!(
                    "echo {a}-{n} > {a}_{n}.txt && {commit}printf 'candidate' > sum.txt && \
                     \"$H5I\" team agent submit --summary-file sum.txt",
                    a = turn.agent_id,
                );
                box_shell(&repo, &turn.env_id, &script)
            }
            TurnKind::Review { target } => {
                let script = format!(
                    "printf 'APPROVE looks good' > rev.txt && \
                     \"$H5I\" team review submit --reviewer {} --target {target} --file rev.txt",
                    turn.agent_id
                );
                box_shell(&repo, &turn.env_id, &script)
            }
            TurnKind::Ask | TurnKind::Reflect => return Ok(()),
        };
        if !out.status.success() {
            return Err(H5iError::Metadata(format!(
                "box turn for '{}' failed: {}",
                turn.agent_id,
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        Ok(())
    }))
}

#[test]
fn driver_observes_cross_process_submissions() {
    // A real repo + two real workspace envs on one team.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git_ok(root, &["init", "-q"]);
    git_ok(root, &["config", "user.name", "t"]);
    git_ok(root, &["config", "user.email", "t@t"]);
    std::fs::write(root.join("README.md"), "base\n").unwrap();
    git_ok(root, &["add", "README.md"]);
    git_ok(root, &["commit", "-qm", "init"]);

    let run = |args: &[&str]| {
        let out = Command::new(h5i_bin())
            .args(args)
            .current_dir(root)
            .env("H5I_AGENT", "human")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "h5i {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["team", "create", "sub"]);
    for i in 1..=2 {
        run(&["env", "create", &format!("w{i}"), "--isolation", "workspace"]);
        run(&[
            "team", "add-env", "sub", &format!("env/human/w{i}"),
            "--as", &format!("worker{i}"), "--runtime", "claude",
        ]);
    }

    // Drive the ensemble with the subprocess launcher — the real box→host path.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let outcome = rt.block_on(async {
        let c = Conductor::builder(root, "sub")
            .actor("human")
            .launcher(subprocess_launcher(root.to_path_buf()))
            .poll_interval(Duration::from_millis(200))
            .turn_timeout(Duration::from_secs(60))
            .without_score_digest()
            .launch()?;
        let agents = c.roster().await?;
        patterns::ensemble(&c, "make a file")
            .agents(agents)
            .rounds(1)
            .run()
            .await
    });

    let outcome = outcome.expect("driver must observe the cross-process submissions");
    assert_eq!(outcome.artifacts.len(), 2, "both agents' submissions observed");
    assert_eq!(outcome.reviews.len(), 2, "both reviews observed");
    // Full approval → the review round exits after one cycle.
    assert_eq!(outcome.rounds_run, 1);
}
