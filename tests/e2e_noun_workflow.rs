//! End-to-end tests for the four-noun CLI surface
//! (`capture` / `recall` / `audit` / `share`).
//!
//! Each test:
//!   1. Spins up a fresh git repo in a temp directory
//!   2. Runs `h5i init` and the noun-verb workflow under test
//!   3. Asserts on stdout / exit status / on-disk state
//!
//! These tests intentionally exercise the rewritten noun surface — the
//! legacy verbs are covered by `tests/cli_integration.rs`. Running both
//! suites guarantees the pre-clap argv rewriter stays in sync with the
//! actual verb implementations.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

const H5I: &str = env!("CARGO_BIN_EXE_h5i");

// ── helpers ─────────────────────────────────────────────────────────────────

struct Repo {
    dir: TempDir,
}

impl Repo {
    fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        run_ok(Command::new("git").args(["init", "-b", "main"]).current_dir(root));
        run_ok(
            Command::new("git")
                .args(["config", "user.name", "E2E Test"])
                .current_dir(root),
        );
        run_ok(
            Command::new("git")
                .args(["config", "user.email", "e2e@h5i.test"])
                .current_dir(root),
        );
        Self { dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn h5i(&self, args: &[&str]) -> Output {
        // Strip the box's env-capture vars: if the suite runs inside an h5i env
        // box, these leak in and make `h5i commit` stage notes for host ingest
        // instead of writing `refs/h5i/notes` (main.rs ~5559), breaking the
        // notes/provenance assertions. This temp repo is not the box's env, so
        // the vars don't belong here. No-op on host/CI.
        Command::new(H5I)
            .args(args)
            .current_dir(self.path())
            .env_remove("H5I_ENV_ID")
            .env_remove("H5I_ENV_POLICY_DIGEST")
            .env_remove("H5I_ENV_CAPTURE_SPOOL")
            .output()
            .expect("failed to run h5i")
    }

    fn h5i_ok(&self, args: &[&str]) -> Output {
        let out = self.h5i(args);
        assert!(
            out.status.success(),
            "h5i {} failed:\nstdout: {}\nstderr: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        out
    }

    /// Stage `path` and create an AI-attributed commit via the capture verb.
    fn capture_commit(&self, file: &str, content: &str, msg: &str, prompt: &str) {
        fs::write(self.path().join(file), content).unwrap();
        run_ok(
            Command::new("git")
                .args(["add", file])
                .current_dir(self.path()),
        );
        self.h5i_ok(&[
            "capture",
            "commit",
            "-m",
            msg,
            "--prompt",
            prompt,
            "--model",
            "claude-sonnet-4-6",
            "--agent",
            "claude-code",
        ]);
    }
}

fn run_ok(cmd: &mut Command) -> Output {
    let out = cmd.output().expect("command failed to spawn");
    assert!(
        out.status.success(),
        "command failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    out
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

// ── capture ─────────────────────────────────────────────────────────────────

#[test]
fn capture_commit_creates_h5i_notes() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.capture_commit("a.txt", "alpha\n", "add alpha", "create the alpha file");

    // refs/h5i/notes should now exist with at least one entry.
    let out = run_ok(
        Command::new("git")
            .args(["for-each-ref", "refs/h5i/notes"])
            .current_dir(repo.path()),
    );
    let listing = String::from_utf8_lossy(&out.stdout);
    assert!(
        listing.contains("refs/h5i/notes"),
        "refs/h5i/notes ref not created: {listing}"
    );
}

#[test]
fn capture_commit_records_model_agent_and_prompt() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.capture_commit(
        "b.txt",
        "beta\n",
        "add beta",
        "create the beta file with retry handling",
    );

    let out = repo.h5i_ok(&["recall", "log", "--limit", "5"]);
    let s = stdout(&out);
    assert!(s.contains("claude-sonnet-4-6"), "model not in log: {s}");
    assert!(s.contains("claude-code"), "agent not in log: {s}");
    assert!(s.contains("create the beta file"), "prompt not in log: {s}");
}

// ── recall ──────────────────────────────────────────────────────────────────

#[test]
fn recall_log_lists_capture_commits() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.capture_commit("a.txt", "alpha\n", "msg-alpha", "make alpha");
    repo.capture_commit("b.txt", "beta\n", "msg-beta", "make beta");

    let out = repo.h5i_ok(&["recall", "log", "--limit", "10"]);
    let s = stdout(&out);
    assert!(s.contains("msg-alpha"), "expected first commit in log");
    assert!(s.contains("msg-beta"), "expected second commit in log");
}

#[test]
fn recall_context_show_after_init_renders_goal() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.h5i_ok(&[
        "recall",
        "context",
        "init",
        "--goal",
        "wire up the retry loop",
    ]);

    let out = repo.h5i_ok(&["recall", "context", "show"]);
    let s = stdout(&out);
    assert!(
        s.to_lowercase().contains("retry loop")
            || s.to_lowercase().contains("wire up"),
        "context show should surface the goal text; got: {s}",
    );
}

#[test]
fn recall_context_status_reports_active_workspace() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.h5i_ok(&[
        "recall",
        "context",
        "init",
        "--goal",
        "do the production hardening pass",
    ]);

    let out = repo.h5i_ok(&["recall", "context", "status"]);
    let s = stdout(&out);
    // Status reliably mentions the active branch ("main") and the goal.
    assert!(
        s.contains("main") || s.to_lowercase().contains("branch"),
        "status should describe the active context branch; got: {s}"
    );
}

// ── audit ───────────────────────────────────────────────────────────────────

#[test]
fn audit_review_succeeds_on_clean_branch() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.capture_commit("a.txt", "alpha\n", "add alpha", "make alpha");

    // Even with a trivial diff, audit review must return cleanly. Output is
    // empty in many cases — we assert only on exit code here.
    repo.h5i_ok(&["audit", "review", "--limit", "10"]);
}

// ── share ───────────────────────────────────────────────────────────────────

#[test]
fn share_pr_body_renders_markdown_skeleton() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.capture_commit(
        "a.txt",
        "alpha\n",
        "first capture",
        "create alpha for the share test",
    );

    let out = repo.h5i_ok(&["share", "pr", "body", "--limit", "5"]);
    let s = stdout(&out);
    assert!(
        s.starts_with("<!-- h5i:pr-comment v1 -->"),
        "PR body must start with the sticky-comment marker; got: {s}",
    );
    // The header carries the h5i brand mark — either as "# 🪙 N% AI-authored …"
    // (Receipt's punchline headline when AI commits are present) or as
    // "## 🪙 h5i provenance" (Minimal / empty-fallback). Asserting on the
    // 🪙 keeps the test resilient to headline copy changes.
    assert!(
        s.contains("🪙"),
        "PR body missing the h5i brand header; got: {s}",
    );
    assert!(
        s.contains("Generated by"),
        "PR body must include the footer credit; got: {s}",
    );
}

#[test]
fn share_pr_body_emits_checks_pass_when_clean() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    // A trivial alpha commit will not trigger CREDENTIAL_LEAK or DUPLICATED_CODE.
    repo.capture_commit(
        "trivial.txt",
        "hello world\n",
        "add trivial file",
        "noop test commit",
    );

    let out = repo.h5i_ok(&["share", "pr", "body", "--limit", "5"]);
    let s = stdout(&out);
    // Each check now gets its own callout when clean. Security is the
    // marquee positive signal (TIP, green) and gets an h3 heading;
    // duplicate-code pass is a quieter NOTE.
    assert!(
        s.contains("✅ Security scan clean"),
        "clean branch must surface the security pass callout; got: {s}",
    );
    assert!(
        s.contains("Duplicate-code scan clean"),
        "clean branch must surface the duplicate pass note; got: {s}",
    );
}

// ── deprecation hints ───────────────────────────────────────────────────────

#[test]
fn legacy_commit_verb_still_works_and_hints_to_capture() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    fs::write(repo.path().join("c.txt"), "gamma\n").unwrap();
    run_ok(
        Command::new("git")
            .args(["add", "c.txt"])
            .current_dir(repo.path()),
    );

    let out = repo.h5i_ok(&["commit", "-m", "legacy form", "--prompt", "p"]);
    let combined = format!("{}{}", stdout(&out), stderr(&out));
    assert!(
        combined.contains("h5i capture commit") || combined.contains("capture commit"),
        "legacy `h5i commit` should print a one-line deprecation hint pointing \
         at `h5i capture commit`; got:\n{combined}",
    );
}

#[test]
fn legacy_log_verb_still_works_and_hints_to_recall() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.capture_commit("a.txt", "alpha\n", "hint test", "hint test");

    // `h5i log` is the legacy alias for `h5i recall log`. It must still work
    // and emit a deprecation hint on stderr.
    let out = repo.h5i_ok(&["log", "--limit", "5"]);
    let combined = format!("{}{}", stdout(&out), stderr(&out));
    assert!(
        combined.contains("recall log") || combined.contains("h5i recall"),
        "legacy `h5i log` should hint at `h5i recall log`; got:\n{combined}"
    );
}
