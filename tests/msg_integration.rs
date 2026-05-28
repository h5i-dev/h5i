//! End-to-end tests for `h5i msg` — cross-agent messaging over `refs/h5i/msg`.
//!
//! These tests drive the compiled binary as a subprocess against real git
//! repositories, including a shared bare remote, to prove the two properties
//! that distinguish h5i messaging from a machine-local message store:
//!
//!   1. Messages travel between clones via `h5i share push` / `h5i share pull`.
//!   2. When two clones each send while "offline", a pull union-merges the two
//!      append-only logs so no message is lost.
//!
//! Run with:
//!   cargo test --test msg_integration -- --nocapture

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

const H5I: &str = env!("CARGO_BIN_EXE_h5i");

// ─── helpers ────────────────────────────────────────────────────────────────

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

fn git(dir: &Path, args: &[&str]) -> Output {
    run_ok(Command::new("git").args(args).current_dir(dir))
}

/// A working clone with a stable identity, addressed through the h5i binary.
struct Clone {
    dir: PathBuf,
}

impl Clone {
    fn h5i(&self, args: &[&str]) -> Output {
        Command::new(H5I)
            .args(args)
            .current_dir(&self.dir)
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
}

fn out_str(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Build a bare remote and two working clones, each `h5i init`-ed with a git
/// identity. Returns `(tempdir, clone_a, clone_b)`; the tempdir owns all paths.
fn two_clones() -> (TempDir, Clone, Clone) {
    let root = TempDir::new().expect("tempdir");
    let bare = root.path().join("origin.git");
    run_ok(Command::new("git").args(["init", "--bare", "-b", "main"]).arg(&bare));

    let mut clones = Vec::new();
    for name in ["a", "b"] {
        let dir = root.path().join(name);
        run_ok(Command::new("git").arg("clone").arg(&bare).arg(&dir));
        git(&dir, &["config", "user.name", &format!("Clone {name}")]);
        git(&dir, &["config", "user.email", &format!("{name}@h5i.test")]);
        // Seed one ordinary commit so the clone has a HEAD and a non-empty
        // history (push of code refs is irrelevant here, but keeps git happy).
        std::fs::write(dir.join("README.md"), "seed\n").unwrap();
        git(&dir, &["add", "README.md"]);
        git(&dir, &["commit", "-m", "seed"]);
        let c = Clone { dir };
        c.h5i_ok(&["init"]);
        clones.push(c);
    }
    let b = clones.pop().unwrap();
    let a = clones.pop().unwrap();
    (root, a, b)
}

// ─── single-repo behaviour ────────────────────────────────────────────────────

#[test]
fn send_inbox_history_roundtrip_in_one_repo() {
    let (_root, a, _b) = two_clones();

    a.h5i_ok(&["msg", "send", "--from", "alice", "bob", "hello", "bob"]);

    // bob sees the message...
    let inbox = out_str(&a.h5i_ok(&["msg", "inbox", "--as", "bob"]));
    assert!(inbox.contains("hello bob"), "inbox missing message: {inbox}");

    // ...and the cursor advanced, so a second check is empty.
    let inbox2 = out_str(&a.h5i_ok(&["msg", "inbox", "--as", "bob"]));
    assert!(
        inbox2.contains("No new messages"),
        "cursor did not advance: {inbox2}"
    );

    // history still shows it regardless of read-state.
    let hist = out_str(&a.h5i_ok(&["msg", "history"]));
    assert!(hist.contains("hello bob"), "history missing message: {hist}");

    // roster knows both participants.
    let team = out_str(&a.h5i_ok(&["msg", "team"]));
    assert!(team.contains("alice") && team.contains("bob"), "team: {team}");
}

#[test]
fn inbox_without_identity_errors_cleanly() {
    let (_root, a, _b) = two_clones();
    // No identity stored, no --as, no env → actionable error, non-zero exit.
    let out = a.h5i(&["msg", "inbox"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("identity"), "expected identity hint, got: {err}");
}

#[test]
fn peek_does_not_consume_messages() {
    let (_root, a, _b) = two_clones();
    a.h5i_ok(&["msg", "send", "--from", "alice", "bob", "ping"]);

    a.h5i_ok(&["msg", "inbox", "--as", "bob", "--peek"]);
    // Still unread after a peek.
    let again = out_str(&a.h5i_ok(&["msg", "inbox", "--as", "bob", "--peek"]));
    assert!(again.contains("ping"), "peek consumed the message: {again}");
}

// ─── cross-clone sharing ──────────────────────────────────────────────────────

#[test]
fn message_travels_between_clones_via_push_pull() {
    let (_root, a, b) = two_clones();

    a.h5i_ok(&["msg", "send", "--from", "alice", "bob", "hi", "from", "alice"]);
    a.h5i_ok(&["share", "push"]);

    b.h5i_ok(&["share", "pull"]);
    let inbox = out_str(&b.h5i_ok(&["msg", "inbox", "--as", "bob"]));
    assert!(
        inbox.contains("hi from alice"),
        "message did not cross clones: {inbox}"
    );
}

#[test]
fn divergent_sends_union_merge_on_pull_without_loss() {
    let (_root, a, b) = two_clones();

    // Common base message, shared to both clones.
    a.h5i_ok(&["msg", "send", "--from", "alice", "bob", "base"]);
    a.h5i_ok(&["share", "push"]);
    b.h5i_ok(&["share", "pull"]);

    // Both clones send concurrently while "offline".
    a.h5i_ok(&["msg", "send", "--from", "alice", "bob", "from-alice"]);
    b.h5i_ok(&["msg", "send", "--from", "carol", "bob", "from-carol"]);

    // A publishes first; B's push would be rejected (non-fast-forward), so B
    // pulls — which must union-merge rather than drop either side.
    a.h5i_ok(&["share", "push"]);
    b.h5i_ok(&["share", "pull"]);

    let hist_b = out_str(&b.h5i_ok(&["msg", "history"]));
    assert!(hist_b.contains("base"), "B lost base: {hist_b}");
    assert!(hist_b.contains("from-alice"), "B lost alice's msg: {hist_b}");
    assert!(hist_b.contains("from-carol"), "B lost its own msg: {hist_b}");

    // B publishes the merge; A fast-forwards and now sees all three too.
    b.h5i_ok(&["share", "push"]);
    a.h5i_ok(&["share", "pull"]);
    let hist_a = out_str(&a.h5i_ok(&["msg", "history"]));
    assert!(hist_a.contains("base"), "A lost base: {hist_a}");
    assert!(hist_a.contains("from-alice"), "A lost its own msg: {hist_a}");
    assert!(hist_a.contains("from-carol"), "A lost carol's msg: {hist_a}");
}

#[test]
fn as_sets_identity_so_later_commands_need_no_flag() {
    let (_root, a, _b) = two_clones();
    a.h5i_ok(&["msg", "as", "codex"]);
    // whoami reflects it
    let who = out_str(&a.h5i_ok(&["msg", "whoami"]));
    assert!(who.contains("codex"), "whoami: {who}");
    // send without --from uses the stored identity
    a.h5i_ok(&["msg", "send", "claude", "ready"]);
    let hist = out_str(&a.h5i_ok(&["msg", "history", "--plain"]));
    assert!(hist.contains("codex -> claude"), "history: {hist}");
}

#[test]
fn bare_msg_renders_dashboard_with_git_proof() {
    let (_root, a, _b) = two_clones();
    a.h5i_ok(&["msg", "as", "codex"]);
    a.h5i_ok(&["msg", "send", "--from", "claude", "codex", "review the PR"]);

    let dash = out_str(&a.h5i_ok(&["msg"]));
    assert!(dash.contains("AGENT RADIO"), "no header band: {dash}");
    assert!(dash.contains("GIT PROOF"), "no git-proof band: {dash}");
    assert!(dash.contains("refs/h5i/msg"), "no ref in proof: {dash}");
    assert!(dash.contains("review the PR"), "message missing: {dash}");

    // The dashboard is a glance — it must NOT consume unread.
    let inbox = out_str(&a.h5i_ok(&["msg", "inbox", "--as", "codex"]));
    assert!(inbox.contains("review the PR"), "dashboard consumed unread: {inbox}");
}

#[test]
fn reply_targets_the_numbered_senders() {
    let (_root, a, _b) = two_clones();
    a.h5i_ok(&["msg", "as", "codex"]);
    a.h5i_ok(&["msg", "send", "--from", "claude", "codex", "ping one"]);
    a.h5i_ok(&["msg", "send", "--from", "reviewer", "codex", "ping two"]);

    // Populate the numbered view, then reply to #2 (from "reviewer").
    a.h5i_ok(&["msg", "inbox", "--as", "codex"]);
    a.h5i_ok(&["msg", "reply", "2", "answering reviewer"]);

    let conv = out_str(&a.h5i_ok(&["msg", "history", "--with", "reviewer", "--plain"]));
    assert!(
        conv.contains("codex -> reviewer\t\tanswering reviewer"),
        "reply did not target reviewer: {conv}"
    );
}

#[test]
fn reply_without_a_view_fails_clearly() {
    let (_root, a, _b) = two_clones();
    a.h5i_ok(&["msg", "as", "codex"]);
    let out = a.h5i(&["msg", "reply", "1", "nothing to reply to"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("last view"));
}

#[test]
fn tag_survives_the_round_trip() {
    let (_root, a, _b) = two_clones();
    a.h5i_ok(&["msg", "send", "--from", "lead", "--tag", "risk", "dev", "token cache is stale"]);
    let plain = out_str(&a.h5i_ok(&["msg", "history", "--plain"]));
    // plain history column 4 is the tag.
    assert!(plain.contains("\trisk\t"), "tag missing in plain output: {plain}");
}

#[test]
fn hook_emits_unread_then_clears() {
    let (_root, a, _b) = two_clones();
    a.h5i_ok(&["msg", "send", "--from", "lead", "dev", "review", "the", "PR"]);

    // Turn-delivery hook prints the notification and marks read.
    let hook = out_str(&a.h5i_ok(&["msg", "hook", "--as", "dev"]));
    assert!(hook.contains("review the PR"), "hook output: {hook}");

    // Nothing left to deliver → silent.
    let hook2 = out_str(&a.h5i_ok(&["msg", "hook", "--as", "dev"]));
    assert!(hook2.trim().is_empty(), "hook should be silent now: {hook2}");
}
