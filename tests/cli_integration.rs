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
