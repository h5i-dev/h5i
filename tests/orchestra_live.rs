//! Live acceptance gate for the orchestra eDSL (design review item 7):
//! a REAL end-to-end `h5i team run` cycle with resident agent sessions —
//! real `env create`, real tmux sessions, real LLM turns, real spool ingest,
//! neutral verify, verdict.
//!
//! Opt-in only (spends real agent tokens and needs tmux + a runtime binary):
//!
//! ```bash
//! H5I_TEST_LIVE_TEAM=1 cargo test --test orchestra_live -- --nocapture
//! # runtime pair override (default claude,claude):
//! H5I_TEST_LIVE_TEAM=1 H5I_TEST_LIVE_RUNTIMES=claude,codex cargo test --test orchestra_live
//! ```
//!
//! Skips cleanly (like the container tests) when the gate variable, tmux, or
//! the runtime binaries are absent.
//!
//! PREREQUISITE (learned from the first live run): a resident runtime session
//! must be able to start **non-interactively** in a fresh env worktree. Claude
//! Code shows a one-time "trust this folder" prompt on an unseen directory that
//! `--dangerously-skip-permissions` does NOT bypass, which would hang the
//! session before it ever reads its inbox. Pre-accept trust for the runtime you
//! test (run it once interactively in any dir, or seed the runtime's trust
//! config) before setting `H5I_TEST_LIVE_TEAM=1`. This is runtime onboarding,
//! deliberately kept out of `LaunchResident` (which must not encode one
//! runtime's version-specific onboarding flow).

use std::path::Path;
use std::process::Command;

const RUN_ID: &str = "live-acceptance";

fn h5i_bin() -> &'static str {
    env!("CARGO_BIN_EXE_h5i")
}

fn have(cmd: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {cmd} >/dev/null 2>&1")])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn h5i(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(h5i_bin())
        .args(args)
        .current_dir(dir)
        .env("H5I_AGENT", "human")
        .env("H5I", h5i_bin())
        .output()
        .expect("spawn h5i")
}

fn h5i_ok(dir: &Path, args: &[&str]) -> String {
    let out = h5i(dir, args);
    assert!(
        out.status.success(),
        "h5i {:?} failed:\nstdout: {}\nstderr: {}",
        args,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn git_ok(dir: &Path, args: &[&str]) {
    let st = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("spawn git");
    assert!(st.success(), "git {args:?} failed");
}

/// Kill the per-agent tmux sessions on scope exit, pass or fail.
struct TmuxCleanup(Vec<String>);
impl Drop for TmuxCleanup {
    fn drop(&mut self) {
        for session in &self.0 {
            let _ = Command::new("tmux")
                .args(["kill-session", "-t", session])
                .status();
        }
    }
}

#[test]
fn live_team_run_full_cycle() {
    if std::env::var("H5I_TEST_LIVE_TEAM").ok().as_deref() != Some("1") {
        eprintln!("skipping: set H5I_TEST_LIVE_TEAM=1 to run the live acceptance gate");
        return;
    }
    let runtimes_raw =
        std::env::var("H5I_TEST_LIVE_RUNTIMES").unwrap_or_else(|_| "claude,claude".into());
    let runtimes: Vec<String> = runtimes_raw
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    assert!(runtimes.len() >= 2, "need at least two runtimes");
    if !have("tmux") {
        eprintln!("skipping: tmux not available");
        return;
    }
    for rt in &runtimes {
        if !have(rt) {
            eprintln!("skipping: runtime '{rt}' not available");
            return;
        }
    }

    // ── Arrange: a real repo with the team Stop hook committed into the base
    // (env worktrees check out the base commit, so the hook must be IN it for
    // in-box sessions to stay alive between turns).
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    git_ok(root, &["init", "-q"]);
    git_ok(root, &["config", "user.name", "live-test"]);
    git_ok(root, &["config", "user.email", "live@test"]);
    std::fs::write(root.join("README.md"), "acceptance sandbox\n").unwrap();
    git_ok(root, &["add", "README.md"]);
    git_ok(root, &["commit", "-qm", "init"]);
    h5i_ok(root, &["hook", "setup", "--write", "--team"]);
    git_ok(root, &["add", "-A"]);
    git_ok(root, &["commit", "-qm", "wire h5i team hooks"]);

    // ── Enroll: two real envs (explicit workspace tier — the acceptance is
    // about orchestration, not confinement) on one team.
    h5i_ok(root, &["team", "create", RUN_ID]);
    let mut sessions = Vec::new();
    for (i, rt) in runtimes.iter().take(2).enumerate() {
        let agent = format!("worker{}", i + 1);
        let slug = format!("live-{}", i + 1);
        h5i_ok(
            root,
            &["env", "create", &slug, "--isolation", "workspace"],
        );
        h5i_ok(
            root,
            &[
                "team", "add-env", RUN_ID, &format!("env/human/{slug}"),
                "--as", &agent, "--runtime", rt,
            ],
        );
        sessions.push(format!("h5i-orch-{RUN_ID}-{agent}"));
    }
    let _cleanup = TmuxCleanup(sessions);

    // ── Act: one full hands-off cycle with real resident sessions. The task
    // is deliberately trivial so the acceptance measures the orchestration,
    // not the model.
    let task = "Create a file named answer.txt containing exactly the single line `ok`, \
                and a POSIX script check.sh that exits 0 when answer.txt contains ok \
                (`grep -q '^ok$' answer.txt`). Use git to commit both files. Keep it minimal.";
    let out = Command::new(h5i_bin())
        .args([
            "team", "run", RUN_ID,
            "--task", task,
            "--rounds", "1",
            "--verify-cmd", "sh check.sh",
            "--launch-resident",
            "--poll", "5",
            "--timeout", "600",
            "--json",
        ])
        .current_dir(root)
        .env("H5I_AGENT", "human")
        .env("H5I", h5i_bin())
        .output()
        .expect("spawn team run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    eprintln!("── team run stderr ──\n{stderr}");
    eprintln!("── team run stdout ──\n{stdout}");
    assert!(out.status.success(), "team run failed");

    // ── Assert: two real submissions, a verdict from the neutral verifier,
    // and a coherent trace.
    let outcome: serde_json::Value = serde_json::from_str(stdout.trim()).expect("outcome json");
    let artifacts = outcome["artifacts"].as_array().expect("artifacts");
    assert_eq!(artifacts.len(), 2, "both agents must submit");
    for a in artifacts {
        assert!(a["independent"].as_bool().unwrap_or(false) || !outcome["reviews"].as_array().unwrap().is_empty());
    }
    let verdict = &outcome["verdict"];
    assert!(
        verdict["selected_submission"].is_string(),
        "the neutral verifier must crown a winner: {verdict}"
    );

    let trace = h5i_ok(root, &["team", "trace", RUN_ID]);
    eprintln!("── trace ──\n{trace}");
    assert!(trace.contains("step work/worker1#1"));
    assert!(trace.contains("step work/worker2#1"));
    assert!(trace.contains("verdict"));

    // Resume property, live: re-running the identical command must replay
    // entirely from the journal — fast, no new sessions, no new turns.
    let started = std::time::Instant::now();
    let out2 = Command::new(h5i_bin())
        .args([
            "team", "run", RUN_ID,
            "--task", task,
            "--rounds", "1",
            "--verify-cmd", "sh check.sh",
            "--launch-resident",
            "--poll", "5",
            "--timeout", "600",
            "--json",
        ])
        .current_dir(root)
        .env("H5I_AGENT", "human")
        .env("H5I", h5i_bin())
        .output()
        .expect("spawn team run resume");
    assert!(out2.status.success(), "resume run failed");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(60),
        "resume must replay from the journal, not re-run agent turns"
    );
}
