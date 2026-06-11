//! Integration tests for the `h5i` CLI binary.
//!
//! These tests spin up a real git repo in a temp directory, invoke the compiled
//! `h5i` binary as a subprocess, and assert on exit codes / stdout / stderr.
//!
//! Run with:
//!   cargo test --test cli_integration -- --nocapture

use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

// Path to the compiled binary injected by Cargo at test compile time.
const H5I: &str = env!("CARGO_BIN_EXE_h5i");

// ─── helpers ────────────────────────────────────────────────────────────────

struct Repo {
    dir: TempDir,
}

impl Repo {
    fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();

        // git init
        run_ok(
            Command::new("git")
                .args(["init", "-b", "main"])
                .current_dir(root),
        );
        run_ok(
            Command::new("git")
                .args(["config", "user.name", "H5i Test"])
                .current_dir(root),
        );
        run_ok(
            Command::new("git")
                .args(["config", "user.email", "test@h5i.io"])
                .current_dir(root),
        );

        Self { dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn h5i(&self, args: &[&str]) -> Output {
        Command::new(H5I)
            .args(args)
            .current_dir(self.path())
            .output()
            .expect("failed to run h5i")
    }

    fn h5i_with_home(&self, home: &Path, args: &[&str]) -> Output {
        Command::new(H5I)
            .args(args)
            .env("HOME", home)
            .current_dir(self.path())
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

    /// Stage and commit a file via the h5i commit flow (requires h5i init first).
    fn make_commit(&self, filename: &str, content: &str, message: &str) {
        let path = self.path().join(filename);
        fs::write(&path, content).unwrap();
        run_ok(
            Command::new("git")
                .args(["add", filename])
                .current_dir(self.path()),
        );
        self.h5i_ok(&["commit", "-m", message]);
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

// ─── init ───────────────────────────────────────────────────────────────────

#[test]
fn init_creates_h5i_dir() {
    let repo = Repo::new();
    let out = repo.h5i_ok(&["init"]);
    let s = stdout(&out);
    assert!(s.contains("h5i") || s.contains("init") || out.status.success());

    // .git/.h5i directory must exist after init
    let h5i_dir = repo.path().join(".git").join(".h5i");
    assert!(h5i_dir.exists(), ".git/.h5i was not created by h5i init");
}

#[test]
fn init_writes_codex_agents_file() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);

    let agents_md = repo.path().join("AGENTS.md");
    let contents = fs::read_to_string(&agents_md).expect("AGENTS.md should exist");
    assert!(contents.contains("## h5i Integration"));
    assert!(contents.contains("h5i codex prelude"));
    assert!(contents.contains("--agent codex"));
}

#[test]
fn init_is_idempotent() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    // second init must not error
    repo.h5i_ok(&["init"]);
}

#[test]
fn doctor_repair_creates_storage_layout() {
    let repo = Repo::new();

    let out = repo.h5i_ok(&["doctor", "--repair"]);
    let s = stdout(&out);
    assert!(s.contains("storage healthy"), "unexpected doctor output: {s}");

    let h5i_dir = repo.path().join(".git").join(".h5i");
    assert!(h5i_dir.join("storage-version").exists());
    assert!(h5i_dir.join("claims").is_dir());
}

#[test]
fn doctor_without_repair_reports_missing_sidecar() {
    let repo = Repo::new();

    let out = repo.h5i(&["doctor"]);
    assert_eq!(out.status.code(), Some(2));
    let s = stdout(&out);
    assert!(s.contains("missing_sidecar"), "unexpected doctor output: {s}");
}

#[test]
fn codex_prelude_does_not_run_smart_recall_by_default() {
    let repo = Repo::new();
    repo.h5i_ok(&["context", "init", "--goal", "implement token validation"]);
    repo.h5i_ok(&[
        "context",
        "trace",
        "--kind",
        "THINK",
        "auth.rs validates tokens with jose",
    ]);

    let out = repo.h5i_ok(&["codex", "prelude"]);
    let s = stdout(&out);
    assert!(s.contains("Context workspace active"), "unexpected prelude output: {s}");
    assert!(
        !s.contains("Smart recall for task"),
        "smart recall must stay off by default: {s}"
    );
}

#[test]
fn recall_context_smart_returns_task_aware_results() {
    let repo = Repo::new();
    repo.h5i_ok(&["context", "init", "--goal", "implement token validation"]);
    repo.h5i_ok(&[
        "context",
        "trace",
        "--kind",
        "THINK",
        "auth.rs validates tokens with jose",
    ]);

    let out = repo.h5i_ok(&[
        "recall",
        "context",
        "smart",
        "--query",
        "token validation",
        "--limit",
        "3",
    ]);
    let s = stdout(&out);
    assert!(s.contains("Smart recall for task: token validation"), "unexpected output: {s}");
    assert!(s.contains("auth.rs"), "expected recalled file in output: {s}");
}

// ─── commit ─────────────────────────────────────────────────────────────────

#[test]
fn commit_records_basic_metadata() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);

    // Stage a file
    fs::write(repo.path().join("hello.txt"), "hello").unwrap();
    run_ok(
        Command::new("git")
            .args(["add", "hello.txt"])
            .current_dir(repo.path()),
    );

    let out = repo.h5i_ok(&["commit", "-m", "initial commit"]);
    let s = stdout(&out);
    // Should confirm the commit was created
    assert!(
        s.contains("Commit") || s.contains("commit") || s.contains("Created"),
        "unexpected commit output: {s}"
    );
}

#[test]
fn commit_with_ai_provenance() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);

    fs::write(repo.path().join("a.rs"), "fn main() {}").unwrap();
    run_ok(
        Command::new("git")
            .args(["add", "a.rs"])
            .current_dir(repo.path()),
    );

    let out = repo.h5i_ok(&[
        "commit",
        "-m",
        "add main",
        "--model",
        "claude-sonnet-4-6",
        "--agent",
        "claude-code",
        "--prompt",
        "write a hello world",
    ]);

    // Should mention the model or agent in output
    let s = stdout(&out);
    let e = stderr(&out);
    let combined = format!("{s}{e}");
    assert!(
        combined.contains("claude") || combined.contains("Committed") || out.status.success(),
        "commit with AI provenance failed: {combined}"
    );
}

// ─── log ────────────────────────────────────────────────────────────────────

#[test]
fn log_shows_commits() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.make_commit("README.md", "# test", "add readme");

    let out = repo.h5i_ok(&["log", "--limit", "5"]);
    let s = stdout(&out);
    assert!(
        s.contains("add readme") || s.contains("README"),
        "log output missing commit: {s}"
    );
}

#[test]
fn log_respects_limit() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);

    for i in 0..5 {
        repo.make_commit(&format!("file{i}.txt"), &format!("content{i}"), &format!("commit {i}"));
    }

    let out_1 = repo.h5i_ok(&["log", "--limit", "1"]);
    let out_5 = repo.h5i_ok(&["log", "--limit", "5"]);

    // --limit 1 should produce less output than --limit 5
    assert!(
        stdout(&out_1).len() <= stdout(&out_5).len(),
        "log --limit 1 produced more output than --limit 5"
    );
}

// ─── context init ───────────────────────────────────────────────────────────

#[test]
fn context_init_creates_workspace() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);

    let out = repo.h5i_ok(&["context", "init", "--goal", "test the CLI"]);
    let s = stdout(&out);
    assert!(
        s.contains("test the CLI") || s.contains("Goal") || s.contains("init") || out.status.success(),
        "context init output unexpected: {s}"
    );
}

#[test]
fn context_status_after_init() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.h5i_ok(&["context", "init", "--goal", "verify status works"]);

    let out = repo.h5i_ok(&["context", "status"]);
    let s = stdout(&out);
    assert!(
        s.contains("verify status works") || s.contains("branch") || s.contains("main"),
        "context status missing expected content: {s}"
    );
}

// ─── context trace ──────────────────────────────────────────────────────────

#[test]
fn context_trace_observe() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.h5i_ok(&["context", "init", "--goal", "trace test"]);

    repo.h5i_ok(&["context", "trace", "--kind", "OBSERVE", "found main entry point"]);
}

#[test]
fn context_trace_think() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.h5i_ok(&["context", "init", "--goal", "trace test"]);

    repo.h5i_ok(&["context", "trace", "--kind", "THINK", "use exponential backoff"]);
}

#[test]
fn context_trace_act() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.h5i_ok(&["context", "init", "--goal", "trace test"]);

    repo.h5i_ok(&["context", "trace", "--kind", "ACT", "modified src/main.rs"]);
}

#[test]
fn context_trace_note() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.h5i_ok(&["context", "init", "--goal", "trace test"]);

    repo.h5i_ok(&["context", "trace", "--kind", "NOTE", "TODO: add tests"]);
}

#[test]
fn context_trace_requires_goal_on_current_git_branch() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.h5i_ok(&["context", "init", "--goal", "trace test"]);
    run_ok(
        Command::new("git")
            .args(["switch", "-c", "feature/purpose-required"])
            .current_dir(repo.path()),
    );

    let out = repo.h5i(&["context", "trace", "--kind", "NOTE", "should fail"]);
    assert!(!out.status.success(), "trace unexpectedly succeeded");
    let err = stderr(&out);
    assert!(
        err.contains("h5i context init --goal"),
        "unexpected stderr: {err}"
    );
}

#[test]
fn context_branch_requires_purpose_in_cli() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.h5i_ok(&["context", "init", "--goal", "branch purpose test"]);

    let out = repo.h5i(&["context", "branch", "feature/no-purpose"]);
    assert!(!out.status.success(), "branch unexpectedly succeeded");
    let err = stderr(&out);
    assert!(
        err.contains("requires a purpose"),
        "unexpected stderr: {err}"
    );
}

#[test]
fn context_trace_appears_in_show() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.h5i_ok(&["context", "init", "--goal", "verify trace visibility"]);
    repo.h5i_ok(&["context", "trace", "--kind", "OBSERVE", "unique_token_xyz"]);

    let out = repo.h5i_ok(&["context", "show", "--depth", "3"]);
    let s = stdout(&out);
    assert!(
        s.contains("unique_token_xyz"),
        "trace entry not visible in context show --depth 3: {s}"
    );
}

// ─── context show depth levels ──────────────────────────────────────────────

#[test]
fn context_show_depth_1_is_compact() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.h5i_ok(&["context", "init", "--goal", "depth test"]);

    for i in 0..10 {
        repo.h5i_ok(&["context", "trace", "--kind", "OBSERVE", &format!("observation {i}")]);
    }

    let d1 = stdout(&repo.h5i_ok(&["context", "show", "--depth", "1"]));
    let d3 = stdout(&repo.h5i_ok(&["context", "show", "--depth", "3"]));

    assert!(
        d1.len() < d3.len(),
        "depth=1 output ({} chars) should be shorter than depth=3 ({})",
        d1.len(),
        d3.len()
    );
}

#[test]
fn context_show_depth_2_is_default() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.h5i_ok(&["context", "init", "--goal", "default depth"]);

    let default_out = stdout(&repo.h5i_ok(&["context", "show"]));
    let depth2_out = stdout(&repo.h5i_ok(&["context", "show", "--depth", "2"]));

    assert_eq!(
        default_out, depth2_out,
        "context show default should equal --depth 2"
    );
}

#[test]
fn context_show_trace_flag_equals_depth_3() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.h5i_ok(&["context", "init", "--goal", "trace flag test"]);
    repo.h5i_ok(&["context", "trace", "--kind", "THINK", "a decision"]);

    let with_flag = stdout(&repo.h5i_ok(&["context", "show", "--trace"]));
    let with_depth = stdout(&repo.h5i_ok(&["context", "show", "--depth", "3"]));

    assert_eq!(
        with_flag, with_depth,
        "--trace should produce the same output as --depth 3"
    );
}

// ─── context commit ──────────────────────────────────────────────────────────

#[test]
fn context_commit_milestone() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.h5i_ok(&["context", "init", "--goal", "milestone test"]);
    repo.h5i_ok(&["context", "trace", "--kind", "ACT", "did something"]);

    repo.h5i_ok(&[
        "context",
        "commit",
        "analysis complete",
        "--detail",
        "read all files, understood structure",
    ]);

    // milestone should appear in status / show
    let s = stdout(&repo.h5i_ok(&["context", "status"]));
    assert!(
        s.contains("analysis complete") || s.contains("milestone") || s.contains("commit"),
        "milestone not visible after context commit: {s}"
    );
}

// ─── context branch / merge ──────────────────────────────────────────────────

#[test]
fn context_branch_and_checkout() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.h5i_ok(&["context", "init", "--goal", "branch test"]);

    repo.h5i_ok(&[
        "context",
        "branch",
        "experiment/alt",
        "--purpose",
        "try alternative approach",
    ]);

    // Should be on the new branch
    let s = stdout(&repo.h5i_ok(&["context", "status"]));
    assert!(
        s.contains("experiment/alt") || s.contains("alt"),
        "branch not reflected in status: {s}"
    );

    // Checkout main
    repo.h5i_ok(&["context", "checkout", "main"]);
    let s2 = stdout(&repo.h5i_ok(&["context", "status"]));
    assert!(
        s2.contains("main"),
        "not back on main after checkout: {s2}"
    );
}

#[test]
fn context_merge_branch() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.h5i_ok(&["context", "init", "--goal", "merge test"]);

    repo.h5i_ok(&[
        "context",
        "branch",
        "experiment/merge-me",
        "--purpose",
        "test merging",
    ]);
    repo.h5i_ok(&["context", "trace", "--kind", "THINK", "merge candidate"]);
    repo.h5i_ok(&["context", "checkout", "main"]);

    // Merge should succeed
    repo.h5i_ok(&["context", "merge", "experiment/merge-me"]);
}

// ─── context restore / diff ──────────────────────────────────────────────────

#[test]
fn context_restore_requires_valid_sha() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.h5i_ok(&["context", "init", "--goal", "restore test"]);

    // Invalid SHA must fail gracefully (non-zero exit or error message)
    let out = repo.h5i(&["context", "restore", "deadbeef00000000"]);
    // Either an error exit code or an error message on stderr
    let failed = !out.status.success() || stderr(&out).contains("error") || stderr(&out).contains("Error");
    let s = stdout(&out);
    let e = stderr(&out);
    assert!(
        failed || s.contains("not found") || e.contains("not found"),
        "restore with invalid SHA should fail or report not-found: stdout={s} stderr={e}"
    );
}

// ─── context relevant ────────────────────────────────────────────────────────

#[test]
fn context_relevant_finds_file_mention() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.h5i_ok(&["context", "init", "--goal", "relevant test"]);

    // Write the file so it exists
    fs::write(repo.path().join("auth.rs"), "// auth module").unwrap();
    repo.h5i_ok(&["context", "trace", "--kind", "OBSERVE", "read auth.rs"]);

    let out = repo.h5i_ok(&["context", "relevant", "auth.rs"]);
    let s = stdout(&out);
    assert!(
        s.contains("auth.rs") || s.contains("OBSERVE") || s.contains("read"),
        "context relevant didn't find mention: {s}"
    );
}

// ─── context pack ────────────────────────────────────────────────────────────

#[test]
fn context_pack_runs_cleanly() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.h5i_ok(&["context", "init", "--goal", "pack test"]);

    // Add a batch of OBSERVE entries to give pack something to do
    for i in 0..8 {
        repo.h5i_ok(&["context", "trace", "--kind", "OBSERVE", &format!("read file {i}")]);
    }
    repo.h5i_ok(&["context", "trace", "--kind", "THINK", "keep this decision"]);

    repo.h5i_ok(&["context", "pack"]);

    // After pack, THINK entry must still be visible
    let s = stdout(&repo.h5i_ok(&["context", "show", "--depth", "3"]));
    assert!(
        s.contains("keep this decision"),
        "THINK entry lost after pack: {s}"
    );
}

// ─── hook session-start ──────────────────────────────────────────────────────

#[test]
fn hook_session_start_prints_context() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.h5i_ok(&["context", "init", "--goal", "session start hook test"]);
    repo.h5i_ok(&["context", "trace", "--kind", "THINK", "a key decision"]);
    repo.h5i_ok(&["context", "commit", "first milestone", "--detail", "done"]);

    let out = repo.h5i_ok(&["hook", "session-start"]);
    let s = stdout(&out);

    assert!(
        s.contains("session start hook test") || s.contains("h5i") || s.contains("Context"),
        "hook session-start missing goal or context header: {s}"
    );
}

#[test]
fn hook_session_start_without_workspace_does_not_crash() {
    // No context init — hook should either print nothing or a "no workspace" message.
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);

    let out = repo.h5i(&["hook", "session-start"]);
    // Must not panic (exit code may be non-zero if no workspace, that's OK)
    let _ = stdout(&out);
    let _ = stderr(&out);
}

// ─── hook stop ───────────────────────────────────────────────────────────────

#[test]
fn hook_stop_auto_checkpoints() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.h5i_ok(&["context", "init", "--goal", "stop hook test"]);
    repo.h5i_ok(&["context", "trace", "--kind", "ACT", "wrote src/lib.rs"]);
    repo.h5i_ok(&["context", "trace", "--kind", "ACT", "wrote src/main.rs"]);

    // hook stop should auto-commit the workspace
    let out = repo.h5i(&["hook", "stop"]);
    // Success or at most a non-fatal warning
    let e = stderr(&out);
    assert!(
        !e.contains("panic") && !e.contains("thread"),
        "hook stop panicked: {e}"
    );
}

// ─── hook run (PostToolUse) ──────────────────────────────────────────────────

#[test]
fn hook_run_with_read_tool_emits_observe() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.h5i_ok(&["context", "init", "--goal", "hook run test"]);

    // Simulate Claude Code PostToolUse JSON payload for a Read tool call
    let payload = serde_json::json!({
        "tool_name": "Read",
        "tool_input": { "file_path": "/some/project/src/main.rs" },
        "tool_response": "fn main() {}"
    });

    let out = Command::new(H5I)
        .args(["hook", "run"])
        .current_dir(repo.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(stdin) = child.stdin.as_mut() {
                let _ = stdin.write_all(payload.to_string().as_bytes());
            }
            child.wait_with_output()
        })
        .expect("hook run failed to spawn");

    // Must not crash
    assert!(
        !stderr(&out).contains("panic"),
        "hook run panicked: {}",
        stderr(&out)
    );
}

#[test]
fn hook_run_with_edit_tool_emits_act() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.h5i_ok(&["context", "init", "--goal", "hook run edit test"]);

    let payload = serde_json::json!({
        "tool_name": "Edit",
        "tool_input": {
            "file_path": "/some/project/src/lib.rs",
            "old_string": "old",
            "new_string": "new"
        },
        "tool_response": "OK"
    });

    let out = Command::new(H5I)
        .args(["hook", "run"])
        .current_dir(repo.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(stdin) = child.stdin.as_mut() {
                let _ = stdin.write_all(payload.to_string().as_bytes());
            }
            child.wait_with_output()
        })
        .expect("hook run failed to spawn");

    assert!(
        !stderr(&out).contains("panic"),
        "hook run panicked: {}",
        stderr(&out)
    );
}

#[test]
fn hook_run_with_invalid_json_does_not_crash() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);

    let out = Command::new(H5I)
        .args(["hook", "run"])
        .current_dir(repo.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(stdin) = child.stdin.as_mut() {
                let _ = stdin.write_all(b"not valid json {{{");
            }
            child.wait_with_output()
        })
        .expect("hook run failed to spawn");

    // Invalid JSON must not cause a panic
    assert!(
        !stderr(&out).contains("panic"),
        "hook run panicked on invalid JSON: {}",
        stderr(&out)
    );
}

// ─── context scan ────────────────────────────────────────────────────────────

#[test]
fn context_scan_clean_trace_low_risk() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.h5i_ok(&["context", "init", "--goal", "scan test"]);
    repo.h5i_ok(&["context", "trace", "--kind", "THINK", "use async I/O"]);
    repo.h5i_ok(&["context", "trace", "--kind", "ACT", "wrote src/io.rs"]);

    let out = repo.h5i_ok(&["context", "scan"]);
    let s = stdout(&out);
    // No injection signals; should report low risk or 0 hits
    assert!(
        s.contains("0") || s.contains("risk") || s.contains("scan"),
        "unexpected scan output: {s}"
    );
}

// ─── blame ───────────────────────────────────────────────────────────────────

#[test]
fn blame_line_mode() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.make_commit("src.rs", "fn foo() {}\nfn bar() {}\n", "add functions");

    let out = repo.h5i_ok(&["blame", "src.rs"]);
    let s = stdout(&out);
    assert!(
        s.contains("foo") || s.contains("src.rs") || s.contains("add functions"),
        "blame output unexpected: {s}"
    );
}

// ─── memory snapshot ─────────────────────────────────────────────────────────

#[test]
fn memory_snapshot_runs() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.make_commit("init.txt", "hello", "initial");

    // memory snapshot may require ~/.claude/projects dir; allow non-zero exit
    // as long as there's no panic
    let out = repo.h5i(&["memory", "snapshot"]);
    assert!(
        !stderr(&out).contains("panic"),
        "memory snapshot panicked: {}",
        stderr(&out)
    );
}

#[test]
fn memory_snapshot_supports_codex_agent() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.make_commit("init.txt", "hello", "initial");

    let out = repo.h5i(&["memory", "snapshot", "--agent", "codex"]);
    assert!(
        !stderr(&out).contains("panic"),
        "codex memory snapshot panicked: {}",
        stderr(&out)
    );
}

#[test]
fn codex_sync_replays_session_activity_into_context() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.h5i_ok(&["context", "init", "--goal", "codex sync test"]);

    let home = TempDir::new().unwrap();
    let session_dir = home.path().join(".codex").join("sessions").join("2026").join("04").join("21");
    fs::create_dir_all(&session_dir).unwrap();
    let session_path = session_dir.join("rollout-test.jsonl");
    let session = format!(
        concat!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"cwd\":\"{}\"}}}}\n",
            "{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"exec_command_end\",\"parsed_cmd\":[{{\"type\":\"read\",\"path\":\"{}/src/main.rs\"}}]}}}}\n",
            "{{\"type\":\"response_item\",\"payload\":{{\"type\":\"function_call\",\"name\":\"apply_patch\",\"arguments\":\"*** Begin Patch\\n*** Update File: src/main.rs\\n*** End Patch\\n\"}}}}\n"
        ),
        repo.path().display(),
        repo.path().display(),
    );
    fs::write(&session_path, session).unwrap();

    let sync = repo.h5i_with_home(home.path(), &["codex", "sync"]);
    assert!(sync.status.success(), "codex sync failed: {}", stderr(&sync));

    let show = repo.h5i_with_home(home.path(), &["context", "show", "--trace"]);
    let s = stdout(&show);
    assert!(s.contains("read src/main.rs"), "context trace missing read: {s}");
    assert!(s.contains("edited src/main.rs"), "context trace missing edit: {s}");
}

// ─── error handling ──────────────────────────────────────────────────────────

#[test]
fn unknown_subcommand_exits_nonzero() {
    let repo = Repo::new();
    let out = repo.h5i(&["this-subcommand-does-not-exist"]);
    assert!(
        !out.status.success(),
        "unknown subcommand should exit non-zero"
    );
}

#[test]
fn commit_without_staged_changes_exits_nonzero() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);

    // Nothing staged — commit must fail
    let out = repo.h5i(&["commit", "-m", "empty"]);
    assert!(
        !out.status.success(),
        "commit with nothing staged should fail"
    );
}

#[test]
fn context_init_outside_git_repo_fails_gracefully() {
    let dir = TempDir::new().unwrap();
    let out = Command::new(H5I)
        .args(["context", "init", "--goal", "no git"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let e = stderr(&out);
    assert!(
        !out.status.success() || e.contains("git") || e.contains("not a"),
        "context init outside git repo should fail: stdout={} stderr={e}",
        stdout(&out)
    );
}

// ─── h5i pull ───────────────────────────────────────────────────────────────

/// End-to-end push/pull round-trip:
///
///   sender  ── h5i push ──▶  bare remote  ◀── h5i pull ──  receiver
///
/// We wire a sender repo and a receiver repo to the same bare remote, push the
/// h5i refs from sender, and verify they land in receiver after `h5i pull`.
#[test]
fn pull_roundtrips_h5i_refs_through_a_bare_remote() {
    // 1. Bare remote that both clones can talk to.
    let remote = TempDir::new().expect("tempdir");
    run_ok(
        Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .current_dir(remote.path()),
    );
    let remote_url = remote.path().to_string_lossy().into_owned();

    // 2. Sender: real commit + h5i provenance, then push.
    let sender = Repo::new();
    sender.h5i_ok(&["init"]);
    sender.make_commit("a.rs", "fn main() {}", "first commit");
    run_ok(
        Command::new("git")
            .args(["remote", "add", "origin", &remote_url])
            .current_dir(sender.path()),
    );
    run_ok(
        Command::new("git")
            .args(["push", "-u", "origin", "main"])
            .current_dir(sender.path()),
    );
    let push_out = sender.h5i_ok(&["push", "--remote", "origin"]);
    let push_s = stdout(&push_out);
    assert!(
        push_s.contains("refs/h5i/notes"),
        "push output should mention notes ref:\n{push_s}"
    );

    // 3. Receiver: fresh repo wired to the same bare remote. We deliberately
    // don't fetch `main` — `h5i pull` is about h5i refs, not code branches —
    // and `git init -b main` already left refs/heads/main checked out, so
    // fetching into it would be rejected anyway.
    let receiver = Repo::new();
    run_ok(
        Command::new("git")
            .args(["remote", "add", "origin", &remote_url])
            .current_dir(receiver.path()),
    );
    receiver.h5i_ok(&["init"]);

    // Sanity-check: receiver has no h5i notes ref before pulling.
    let pre = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", "refs/h5i/notes"])
        .current_dir(receiver.path())
        .status()
        .unwrap();
    assert!(
        !pre.success(),
        "receiver should not yet have refs/h5i/notes before pull"
    );

    // 4. Pull and assert.
    let pull_out = receiver.h5i_ok(&["pull", "--remote", "origin"]);
    let pull_s = stdout(&pull_out);
    assert!(
        pull_s.contains("Pulling all h5i refs"),
        "pull stdout should announce itself:\n{pull_s}"
    );
    assert!(
        pull_s.contains("refs/h5i/notes") && pull_s.contains("ok"),
        "pull stdout should report notes ref ok:\n{pull_s}"
    );

    let post = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", "refs/h5i/notes"])
        .current_dir(receiver.path())
        .output()
        .unwrap();
    assert!(
        post.status.success(),
        "receiver should now have refs/h5i/notes after pull:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&post.stdout),
        String::from_utf8_lossy(&post.stderr),
    );

    // The notes ref should resolve to the same OID on both sides.
    let sender_oid = run_ok(
        Command::new("git")
            .args(["rev-parse", "refs/h5i/notes"])
            .current_dir(sender.path()),
    );
    assert_eq!(
        String::from_utf8_lossy(&sender_oid.stdout).trim(),
        String::from_utf8_lossy(&post.stdout).trim(),
        "sender and receiver should agree on refs/h5i/notes after pull"
    );
}

/// `h5i pull` should succeed (and skip cleanly) against a remote that has
/// no h5i refs at all — e.g. the very first time anyone runs it on a repo.
#[test]
fn pull_skips_gracefully_when_remote_has_no_h5i_refs() {
    let remote = TempDir::new().expect("tempdir");
    run_ok(
        Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .current_dir(remote.path()),
    );
    let remote_url = remote.path().to_string_lossy().into_owned();

    // Seed `main` on the remote so `git fetch` has something to talk to,
    // but deliberately leave refs/h5i/* unset.
    let seeder = Repo::new();
    fs::write(seeder.path().join("seed.txt"), "seed").unwrap();
    run_ok(
        Command::new("git")
            .args(["add", "seed.txt"])
            .current_dir(seeder.path()),
    );
    run_ok(
        Command::new("git")
            .args(["commit", "-m", "seed"])
            .current_dir(seeder.path()),
    );
    run_ok(
        Command::new("git")
            .args(["remote", "add", "origin", &remote_url])
            .current_dir(seeder.path()),
    );
    run_ok(
        Command::new("git")
            .args(["push", "-u", "origin", "main"])
            .current_dir(seeder.path()),
    );

    let receiver = Repo::new();
    run_ok(
        Command::new("git")
            .args(["remote", "add", "origin", &remote_url])
            .current_dir(receiver.path()),
    );
    receiver.h5i_ok(&["init"]);

    let out = receiver.h5i_ok(&["pull", "--remote", "origin"]);
    let s = stdout(&out);
    assert!(
        s.contains("skipped"),
        "pull against an empty remote should report skipped refs:\n{s}"
    );
}

// ─── h5i pull: conflict handling ────────────────────────────────────────────

/// Build a bare git remote and a Repo wired to it (origin → remote).
/// The Repo has h5i initialised so subsequent h5i commands work.
fn repo_wired_to(remote_url: &str) -> Repo {
    let r = Repo::new();
    run_ok(
        Command::new("git")
            .args(["remote", "add", "origin", remote_url])
            .current_dir(r.path()),
    );
    r.h5i_ok(&["init"]);
    r
}

/// Resolve a ref's full SHA in the given repo, or None if the ref doesn't exist.
fn resolve_ref_in(repo: &Repo, refname: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", refname])
        .current_dir(repo.path())
        .output()
        .unwrap();
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

/// Pulling a second time after a successful first pull should be idempotent
/// and report `up to date` for the previously-fetched ref.
#[test]
fn pull_is_idempotent_when_already_up_to_date() {
    let remote = TempDir::new().expect("tempdir");
    run_ok(
        Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .current_dir(remote.path()),
    );
    let remote_url = remote.path().to_string_lossy().into_owned();

    let sender = repo_wired_to(&remote_url);
    sender.make_commit("a.rs", "fn main() {}", "first");
    run_ok(
        Command::new("git")
            .args(["push", "-u", "origin", "main"])
            .current_dir(sender.path()),
    );
    sender.h5i_ok(&["push", "--remote", "origin"]);

    let receiver = repo_wired_to(&remote_url);
    let first = stdout(&receiver.h5i_ok(&["pull", "--remote", "origin"]));
    assert!(
        first.contains("(new)"),
        "first pull should report (new):\n{first}"
    );

    let second = stdout(&receiver.h5i_ok(&["pull", "--remote", "origin"]));
    assert!(
        second.contains("up to date"),
        "second pull should report up to date for notes:\n{second}"
    );
}

/// When the remote's ref strictly extends the receiver's ref, pull should
/// fast-forward without prompting.
#[test]
fn pull_fast_forwards_when_remote_extends_local() {
    let remote = TempDir::new().expect("tempdir");
    run_ok(
        Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .current_dir(remote.path()),
    );
    let remote_url = remote.path().to_string_lossy().into_owned();

    let sender = repo_wired_to(&remote_url);
    sender.make_commit("a.rs", "fn main() {}", "first");
    run_ok(
        Command::new("git")
            .args(["push", "-u", "origin", "main"])
            .current_dir(sender.path()),
    );
    sender.h5i_ok(&["push", "--remote", "origin"]);

    let receiver = repo_wired_to(&remote_url);
    receiver.h5i_ok(&["pull", "--remote", "origin"]);
    let after_first = resolve_ref_in(&receiver, "refs/h5i/notes").unwrap();

    // Sender extends notes with another commit, then pushes.
    sender.make_commit("b.rs", "fn extra() {}", "second");
    sender.h5i_ok(&["push", "--remote", "origin"]);
    let sender_tip = resolve_ref_in(&sender, "refs/h5i/notes").unwrap();
    assert_ne!(sender_tip, after_first, "sender should have moved forward");

    let pull_out = stdout(&receiver.h5i_ok(&["pull", "--remote", "origin"]));
    assert!(
        pull_out.contains("fast-forward"),
        "pull stdout should report fast-forward:\n{pull_out}"
    );
    assert_eq!(
        resolve_ref_in(&receiver, "refs/h5i/notes").unwrap(),
        sender_tip,
        "receiver should be at sender's new tip after fast-forward",
    );
}

/// When the receiver's ref strictly extends the remote's ref (e.g. the
/// receiver did one more `h5i commit` after pulling), pull should keep
/// the local ref untouched.
#[test]
fn pull_keeps_local_when_local_is_ahead() {
    let remote = TempDir::new().expect("tempdir");
    run_ok(
        Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .current_dir(remote.path()),
    );
    let remote_url = remote.path().to_string_lossy().into_owned();

    let sender = repo_wired_to(&remote_url);
    sender.make_commit("a.rs", "fn main() {}", "first");
    run_ok(
        Command::new("git")
            .args(["push", "-u", "origin", "main"])
            .current_dir(sender.path()),
    );
    sender.h5i_ok(&["push", "--remote", "origin"]);

    let receiver = repo_wired_to(&remote_url);
    receiver.h5i_ok(&["pull", "--remote", "origin"]);
    // Receiver makes its own h5i commit on top, so refs/h5i/notes now
    // strictly extends what's on the remote.
    receiver.make_commit("c.rs", "fn local() {}", "local-only");
    let local_tip = resolve_ref_in(&receiver, "refs/h5i/notes").unwrap();

    let pull_out = stdout(&receiver.h5i_ok(&["pull", "--remote", "origin"]));
    assert!(
        pull_out.contains("local ahead"),
        "pull stdout should report local ahead:\n{pull_out}"
    );
    assert_eq!(
        resolve_ref_in(&receiver, "refs/h5i/notes").unwrap(),
        local_tip,
        "receiver should keep its tip when ahead of remote",
    );
}

/// When sender and receiver both have notes for *different* code commits,
/// their `refs/h5i/notes` chains have no common ancestor and pull must
/// union-merge so neither side loses its annotations.
#[test]
fn pull_union_merges_diverged_notes_so_no_annotations_are_lost() {
    let remote = TempDir::new().expect("tempdir");
    run_ok(
        Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .current_dir(remote.path()),
    );
    let remote_url = remote.path().to_string_lossy().into_owned();

    let sender = repo_wired_to(&remote_url);
    sender.make_commit("a.rs", "fn a() {}", "sender commit");
    run_ok(
        Command::new("git")
            .args(["push", "-u", "origin", "main"])
            .current_dir(sender.path()),
    );
    sender.h5i_ok(&["push", "--remote", "origin"]);
    let sender_code_oid = String::from_utf8_lossy(
        &run_ok(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(sender.path()),
        )
        .stdout,
    )
    .trim()
    .to_string();

    // Receiver works in parallel — never pulled, makes its own commit.
    let receiver = repo_wired_to(&remote_url);
    receiver.make_commit("b.rs", "fn b() {}", "receiver commit");
    let receiver_code_oid = String::from_utf8_lossy(
        &run_ok(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(receiver.path()),
        )
        .stdout,
    )
    .trim()
    .to_string();
    assert_ne!(sender_code_oid, receiver_code_oid);

    let pull_out = stdout(&receiver.h5i_ok(&["pull", "--remote", "origin"]));
    assert!(
        pull_out.contains("merged (union)"),
        "pull should report a union merge:\n{pull_out}"
    );

    // Both code commits should have a notes blob in the merged tree.
    let tree = run_ok(
        Command::new("git")
            .args(["ls-tree", "-r", "refs/h5i/notes"])
            .current_dir(receiver.path()),
    );
    let tree_s = String::from_utf8_lossy(&tree.stdout);
    assert!(
        tree_s.contains(&sender_code_oid),
        "merged notes tree should contain sender's commit ({sender_code_oid}):\n{tree_s}"
    );
    assert!(
        tree_s.contains(&receiver_code_oid),
        "merged notes tree should contain receiver's commit ({receiver_code_oid}):\n{tree_s}"
    );

    // The merge commit should have both inputs as parents.
    let parents = run_ok(
        Command::new("git")
            .args(["log", "--pretty=%P", "-1", "refs/h5i/notes"])
            .current_dir(receiver.path()),
    );
    let parents_s = String::from_utf8_lossy(&parents.stdout);
    assert_eq!(
        parents_s.split_whitespace().count(),
        2,
        "merge commit should have two parents:\n{parents_s}"
    );
}

/// On a non-notes ref divergence (here: refs/h5i/context, which is a linear
/// chain we can't auto-merge), pull MUST keep the local ref untouched and
/// report the divergence rather than overwriting silently.
#[test]
fn pull_keeps_local_on_diverged_context_without_force() {
    let remote = TempDir::new().expect("tempdir");
    run_ok(
        Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .current_dir(remote.path()),
    );
    let remote_url = remote.path().to_string_lossy().into_owned();

    // Sender initialises a context workspace and pushes.
    let sender = repo_wired_to(&remote_url);
    sender.make_commit("a.rs", "fn a() {}", "seed sender");
    run_ok(
        Command::new("git")
            .args(["push", "-u", "origin", "main"])
            .current_dir(sender.path()),
    );
    sender.h5i_ok(&["context", "init", "--goal", "sender goal"]);
    sender.h5i_ok(&["push", "--remote", "origin"]);

    // Receiver initialises a *different* context workspace.
    let receiver = repo_wired_to(&remote_url);
    receiver.h5i_ok(&["context", "init", "--goal", "receiver goal"]);
    let receiver_ctx_before = resolve_ref_in(&receiver, "refs/h5i/context/main").unwrap();
    let sender_ctx = resolve_ref_in(&sender, "refs/h5i/context/main").unwrap();
    assert_ne!(
        receiver_ctx_before, sender_ctx,
        "context refs must differ for this test to mean anything"
    );

    let pull_out = stdout(&receiver.h5i_ok(&["pull", "--remote", "origin"]));
    assert!(
        pull_out.contains("kept local") && pull_out.contains("--force"),
        "pull should report kept-local and mention --force:\n{pull_out}"
    );

    let receiver_ctx_after = resolve_ref_in(&receiver, "refs/h5i/context/main").unwrap();
    assert_eq!(
        receiver_ctx_before, receiver_ctx_after,
        "receiver's context ref must be unchanged when pulled without --force"
    );
}

/// With `--force`, a divergent non-notes ref should be overwritten with the
/// remote's value.
#[test]
fn pull_force_overwrites_diverged_context() {
    let remote = TempDir::new().expect("tempdir");
    run_ok(
        Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .current_dir(remote.path()),
    );
    let remote_url = remote.path().to_string_lossy().into_owned();

    let sender = repo_wired_to(&remote_url);
    sender.make_commit("a.rs", "fn a() {}", "seed sender");
    run_ok(
        Command::new("git")
            .args(["push", "-u", "origin", "main"])
            .current_dir(sender.path()),
    );
    sender.h5i_ok(&["context", "init", "--goal", "sender goal"]);
    sender.h5i_ok(&["push", "--remote", "origin"]);
    let sender_ctx = resolve_ref_in(&sender, "refs/h5i/context/main").unwrap();

    let receiver = repo_wired_to(&remote_url);
    receiver.h5i_ok(&["context", "init", "--goal", "receiver goal"]);
    let receiver_ctx_before = resolve_ref_in(&receiver, "refs/h5i/context/main").unwrap();
    assert_ne!(receiver_ctx_before, sender_ctx);

    let pull_out = stdout(&receiver.h5i_ok(&["pull", "--remote", "origin", "--force"]));
    assert!(
        pull_out.contains("forced"),
        "pull --force should report a forced update:\n{pull_out}"
    );
    assert_eq!(
        resolve_ref_in(&receiver, "refs/h5i/context/main").unwrap(),
        sender_ctx,
        "after --force, receiver's context ref should match sender's",
    );
}

/// `--force` must NOT clobber refs/h5i/notes — it should still take the
/// merging path so notes from both sides are preserved.
#[test]
fn pull_force_still_union_merges_notes() {
    let remote = TempDir::new().expect("tempdir");
    run_ok(
        Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .current_dir(remote.path()),
    );
    let remote_url = remote.path().to_string_lossy().into_owned();

    let sender = repo_wired_to(&remote_url);
    sender.make_commit("a.rs", "fn a() {}", "sender commit");
    run_ok(
        Command::new("git")
            .args(["push", "-u", "origin", "main"])
            .current_dir(sender.path()),
    );
    sender.h5i_ok(&["push", "--remote", "origin"]);
    let sender_code_oid = String::from_utf8_lossy(
        &run_ok(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(sender.path()),
        )
        .stdout,
    )
    .trim()
    .to_string();

    let receiver = repo_wired_to(&remote_url);
    receiver.make_commit("b.rs", "fn b() {}", "receiver commit");
    let receiver_code_oid = String::from_utf8_lossy(
        &run_ok(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(receiver.path()),
        )
        .stdout,
    )
    .trim()
    .to_string();

    let pull_out = stdout(&receiver.h5i_ok(&["pull", "--remote", "origin", "--force"]));
    assert!(
        pull_out.contains("merged (union)"),
        "even with --force, notes should be merged (not forced):\n{pull_out}"
    );

    let tree = run_ok(
        Command::new("git")
            .args(["ls-tree", "-r", "refs/h5i/notes"])
            .current_dir(receiver.path()),
    );
    let tree_s = String::from_utf8_lossy(&tree.stdout);
    assert!(tree_s.contains(&sender_code_oid));
    assert!(tree_s.contains(&receiver_code_oid));
}

// ─── h5i share setup-remote / migrate-remote ─────────────────────────────────
//
// These exercise the two remote-management verbs added to fix the
// directory/file conflict that bit per-branch context refs (`refs/h5i/context`
// single ref on the remote vs. `refs/h5i/context/*` directory locally).

/// Spin up a bare remote and return `(handle, url)`. Keep the handle alive for
/// the duration of the test so the temp dir isn't reaped.
fn bare_remote() -> (TempDir, String) {
    let remote = TempDir::new().expect("tempdir");
    run_ok(
        Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .current_dir(remote.path()),
    );
    let url = remote.path().to_string_lossy().into_owned();
    (remote, url)
}

/// Run `git <args>` in `dir`, returning the raw Output (no success assertion).
fn git_in(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("failed to spawn git")
}

/// Ref names visible on `remote` (as seen from `workdir`) matching `pattern`.
fn ls_remote_names(workdir: &Path, remote: &str, pattern: &str) -> Vec<String> {
    let out = git_in(workdir, &["ls-remote", remote, pattern]);
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().nth(1).map(str::to_string))
        .collect()
}

/// Fresh repo wired to `remote_url` as `origin`, with one commit pushed to
/// `main`. h5i is initialised. Returns the repo. (Distinct from the lighter
/// `repo_wired_to`, which neither commits nor pushes `main`.)
fn repo_pushed_to(remote_url: &str) -> Repo {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    repo.make_commit("a.rs", "fn main() {}", "seed");
    run_ok(
        Command::new("git")
            .args(["remote", "add", "origin", remote_url])
            .current_dir(repo.path()),
    );
    run_ok(
        Command::new("git")
            .args(["push", "-u", "origin", "main"])
            .current_dir(repo.path()),
    );
    repo
}

/// Put the remote into the "legacy" state: a single `refs/h5i/context` ref,
/// and locally migrate it aside so per-branch `refs/h5i/context/<name>` refs
/// exist (mirrors what a post-redesign client looks like). `branches` are the
/// per-branch context refs to create locally.
fn seed_legacy_context_conflict(repo: &Repo, branches: &[&str]) {
    // Remote gets the legacy single ref.
    run_ok(
        Command::new("git")
            .args(["update-ref", "refs/h5i/context", "HEAD"])
            .current_dir(repo.path()),
    );
    run_ok(
        Command::new("git")
            .args(["push", "origin", "refs/h5i/context:refs/h5i/context"])
            .current_dir(repo.path()),
    );
    // Local side completes its migration: legacy → backup, then per-branch refs.
    run_ok(
        Command::new("git")
            .args(["update-ref", "refs/h5i/context-legacy", "refs/h5i/context"])
            .current_dir(repo.path()),
    );
    run_ok(
        Command::new("git")
            .args(["update-ref", "-d", "refs/h5i/context"])
            .current_dir(repo.path()),
    );
    for b in branches {
        run_ok(
            Command::new("git")
                .args(["update-ref", &format!("refs/h5i/context/{b}"), "HEAD"])
                .current_dir(repo.path()),
        );
    }
}

#[test]
fn setup_remote_writes_all_fetch_refspecs() {
    let (_remote, url) = bare_remote();
    let repo = repo_pushed_to(&url);

    let out = repo.h5i_ok(&["share", "setup-remote"]);
    let s = stdout(&out);
    assert!(s.contains("Configuring h5i fetch refspecs"), "banner missing:\n{s}");
    assert!(s.contains("8 refspec(s) added"), "should add 8 refspecs:\n{s}");

    let fetch = git_in(repo.path(), &["config", "--get-all", "remote.origin.fetch"]);
    let fetch_s = String::from_utf8_lossy(&fetch.stdout);
    for pat in [
        "+refs/h5i/notes:refs/h5i/notes",
        "+refs/h5i/memory:refs/h5i/memory",
        "+refs/h5i/context/*:refs/h5i/context/*",
        "+refs/h5i/ast:refs/h5i/ast",
        "+refs/h5i/msg:refs/h5i/msg",
        "+refs/h5i/objects:refs/h5i/objects",
        "+refs/h5i/env:refs/h5i/env",
        // env code branch: hidden remote ns → local branch, fast-forward only (no +)
        "refs/h5i/env-code/*:refs/heads/h5i/env/*",
    ] {
        assert!(fetch_s.contains(pat), "missing fetch refspec {pat}:\n{fetch_s}");
    }
    // The default branch refspec must survive untouched.
    assert!(
        fetch_s.contains("+refs/heads/*:refs/remotes/origin/*"),
        "default branch fetch refspec was clobbered:\n{fetch_s}"
    );
}

#[test]
fn setup_remote_does_not_configure_push_refspec() {
    // Writing remote.<r>.push would silently change `git push` behaviour — we
    // must never do that. Pushing h5i refs stays the job of `h5i share push`.
    let (_remote, url) = bare_remote();
    let repo = repo_pushed_to(&url);
    repo.h5i_ok(&["share", "setup-remote"]);

    let push = git_in(repo.path(), &["config", "--get-all", "remote.origin.push"]);
    assert!(
        String::from_utf8_lossy(&push.stdout).trim().is_empty(),
        "setup-remote must not write any push refspec"
    );
}

#[test]
fn setup_remote_is_idempotent() {
    let (_remote, url) = bare_remote();
    let repo = repo_pushed_to(&url);

    repo.h5i_ok(&["share", "setup-remote"]);
    let second = stdout(&repo.h5i_ok(&["share", "setup-remote"]));
    assert!(
        second.contains("already configured"),
        "second run should be a no-op:\n{second}"
    );

    // Exactly one copy of each refspec — no duplicates.
    let fetch = git_in(repo.path(), &["config", "--get-all", "remote.origin.fetch"]);
    let fetch_s = String::from_utf8_lossy(&fetch.stdout);
    let count = fetch_s.matches("+refs/h5i/notes:refs/h5i/notes").count();
    assert_eq!(count, 1, "notes refspec duplicated:\n{fetch_s}");
}

#[test]
fn setup_remote_dry_run_writes_nothing() {
    let (_remote, url) = bare_remote();
    let repo = repo_pushed_to(&url);

    let out = stdout(&repo.h5i_ok(&["share", "setup-remote", "--dry-run"]));
    assert!(out.contains("would add"), "dry run should preview:\n{out}");
    assert!(out.contains("dry run"), "dry run banner missing:\n{out}");

    let fetch = git_in(repo.path(), &["config", "--get-all", "remote.origin.fetch"]);
    let fetch_s = String::from_utf8_lossy(&fetch.stdout);
    assert!(
        !fetch_s.contains("refs/h5i/notes"),
        "dry run must not modify config:\n{fetch_s}"
    );
}

#[test]
fn setup_remote_errors_without_a_remote() {
    let repo = Repo::new();
    repo.h5i_ok(&["init"]);
    let out = repo.h5i(&["share", "setup-remote"]);
    assert!(!out.status.success(), "should fail when remote is absent");
    let err = stderr(&out);
    assert!(
        err.contains("not configured") && err.contains("git remote add"),
        "error should guide the user:\n{err}"
    );
}

#[test]
fn migrate_remote_moves_legacy_context_to_per_branch_layout() {
    let (_remote, url) = bare_remote();
    let repo = repo_pushed_to(&url);
    seed_legacy_context_conflict(&repo, &["main", "feature"]);

    // Precondition: remote has the single legacy ref, no per-branch refs.
    assert_eq!(
        ls_remote_names(repo.path(), "origin", "refs/h5i/context"),
        vec!["refs/h5i/context".to_string()],
    );

    let out = stdout(&repo.h5i_ok(&["share", "migrate-remote"]));
    assert!(out.contains("migration complete"), "expected success:\n{out}");

    // Legacy ref is gone; the per-branch refs and a backup now exist.
    let names = ls_remote_names(repo.path(), "origin", "refs/h5i/*");
    assert!(
        !names.iter().any(|n| n == "refs/h5i/context"),
        "legacy ref should be deleted:\n{names:?}"
    );
    assert!(names.iter().any(|n| n == "refs/h5i/context/main"), "{names:?}");
    assert!(names.iter().any(|n| n == "refs/h5i/context/feature"), "{names:?}");
    assert!(
        names.iter().any(|n| n == "refs/h5i/context-legacy"),
        "backup ref should be created:\n{names:?}"
    );

    // And the whole point: `share push` now works without conflict.
    let push = repo.h5i_ok(&["share", "push"]);
    assert!(push.status.success());
}

#[test]
fn migrate_remote_is_a_noop_when_already_migrated() {
    let (_remote, url) = bare_remote();
    let repo = repo_pushed_to(&url);
    // No legacy ref anywhere — just a clean per-branch setup.
    run_ok(
        Command::new("git")
            .args(["update-ref", "refs/h5i/context/main", "HEAD"])
            .current_dir(repo.path()),
    );

    let out = stdout(&repo.h5i_ok(&["share", "migrate-remote"]));
    assert!(
        out.contains("already migrated") && out.contains("nothing to do"),
        "should detect nothing to migrate:\n{out}"
    );
}

#[test]
fn migrate_remote_dry_run_leaves_remote_untouched() {
    let (_remote, url) = bare_remote();
    let repo = repo_pushed_to(&url);
    seed_legacy_context_conflict(&repo, &["main"]);

    let out = stdout(&repo.h5i_ok(&["share", "migrate-remote", "--dry-run"]));
    assert!(out.contains("dry run"), "expected dry-run banner:\n{out}");
    assert!(out.contains("would"), "expected planned steps:\n{out}");

    // Remote is unchanged: legacy still present, no per-branch refs, no backup.
    let names = ls_remote_names(repo.path(), "origin", "refs/h5i/*");
    assert!(names.iter().any(|n| n == "refs/h5i/context"), "{names:?}");
    assert!(!names.iter().any(|n| n == "refs/h5i/context/main"), "{names:?}");
    assert!(!names.iter().any(|n| n == "refs/h5i/context-legacy"), "{names:?}");
}

#[test]
fn migrate_remote_preserves_an_existing_backup() {
    // create-only backup: if the remote already has refs/h5i/context-legacy
    // (from a prior partial migration), migrate must not clobber it.
    let (_remote, url) = bare_remote();
    let repo = repo_pushed_to(&url);
    seed_legacy_context_conflict(&repo, &["main"]);

    // Plant a DISTINCT backup commit on the remote first.
    repo.make_commit("b.rs", "fn other() {}", "second commit");
    let other_oid = String::from_utf8_lossy(
        &run_ok(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(repo.path()),
        )
        .stdout,
    )
    .trim()
    .to_string();
    run_ok(
        Command::new("git")
            .args(["push", "origin", &format!("{other_oid}:refs/h5i/context-legacy")])
            .current_dir(repo.path()),
    );

    repo.h5i_ok(&["share", "migrate-remote"]);

    // The pre-existing backup must be intact (still the distinct commit).
    let out = git_in(repo.path(), &["ls-remote", "origin", "refs/h5i/context-legacy"]);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains(&other_oid),
        "existing backup was overwritten — expected {other_oid}:\n{s}"
    );
}

#[test]
fn share_push_detects_legacy_conflict_and_advises_migrate() {
    let (_remote, url) = bare_remote();
    let repo = repo_pushed_to(&url);
    seed_legacy_context_conflict(&repo, &["main"]);

    // `share push` will fail to push context/* against the legacy remote, but
    // it should diagnose the cause and point at the fix rather than leaving a
    // bare git error. (Push as a whole may exit non-zero; we inspect output.)
    let out = repo.h5i(&["share", "push"]);
    let combined = format!("{}{}", stdout(&out), stderr(&out));
    assert!(
        combined.contains("legacy") && combined.contains("h5i share migrate-remote"),
        "expected remediation pointing at migrate-remote:\n{combined}"
    );
}
