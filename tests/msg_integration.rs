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
            // Hermetic: never inherit an ambient identity from the developer's
            // shell (a repo using h5i sets H5I_AGENT in .claude/settings.json,
            // which would otherwise leak in and break identity-resolution tests).
            .env_remove("H5I_AGENT")
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

    /// The raw on-disk message log (the JSONL blob in refs/h5i/msg).
    fn msg_log(&self) -> String {
        let out = git(&self.dir, &["show", "refs/h5i/msg:messages.jsonl"]);
        String::from_utf8_lossy(&out.stdout).into_owned()
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
fn pulled_message_cannot_inject_terminal_escapes() {
    let (_root, a, _b) = two_clones();
    a.h5i_ok(&["msg", "as", "bob"]);
    // A hostile body: clear-screen escape + a newline that would forge a row.
    a.h5i_ok(&[
        "msg",
        "send",
        "--from",
        "mallory",
        "bob",
        "\x1b[2Jwiped\nFAKE  9 lines injected",
    ]);

    // Color is auto-disabled on a pipe, so any ESC byte would be injected.
    for view in [vec!["msg"], vec!["msg", "inbox", "--as", "bob"], vec!["msg", "history"]] {
        let out = a.h5i_ok(&view);
        assert!(
            !out.stdout.contains(&0x1b),
            "ESC leaked into `{}` output",
            view.join(" ")
        );
    }

    // The newline must not forge an extra row in line-per-message --plain output.
    let plain = a.h5i_ok(&["msg", "history", "--plain"]);
    let lines = plain.stdout.iter().filter(|&&b| b == b'\n').count();
    assert_eq!(lines, 1, "newline in body forged extra plain rows");
    assert!(!plain.stdout.contains(&0x1b));
}

#[test]
fn invalid_identities_are_rejected() {
    let (_root, a, _b) = two_clones();
    // `as` with a space / control char / path separator must fail.
    assert!(!a.h5i(&["msg", "as", "bad name"]).status.success());
    assert!(!a.h5i(&["msg", "as", "ev/il"]).status.success());
    // `send --from` with a bad sender must fail before anything is written.
    assert!(!a.h5i(&["msg", "send", "--from", "a b", "bob", "hi"]).status.success());
    // A valid one still works.
    assert!(a.h5i(&["msg", "as", "good-name.1"]).status.success());
}

#[test]
fn typed_review_carries_kind_and_structured_fields() {
    let (_root, a, _b) = two_clones();
    a.h5i_ok(&["msg", "as", "claude"]);
    a.h5i_ok(&[
        "msg", "review",
        "--branch", "auth-refactor",
        "--focus", "src/auth.rs",
        "--focus", "src/session.rs",
        "--risk", "token refresh edge cases",
        "--pr", "42",
        "codex", "review token refresh before PR",
    ]);

    // On the wire: kind + structured fields, not a tag.
    let log = a.msg_log();
    assert!(log.contains(r#""kind":"REVIEW_REQUEST""#), "log: {log}");
    assert!(log.contains(r#""branch":"auth-refactor""#));
    assert!(log.contains("src/session.rs"));
    assert!(log.contains(r#""risk":"token refresh edge cases""#));
    assert!(log.contains(r#""links":{"pr":42}"#));

    // In the rich view (colour off on a pipe): kind badge + detail rows.
    let hist = out_str(&a.h5i_ok(&["msg", "history"]));
    assert!(hist.contains("REVIEW_REQUEST"), "hist: {hist}");
    assert!(hist.contains("branch auth-refactor"));
    assert!(hist.contains("risk: token refresh edge cases"));
}

#[test]
fn ack_done_decline_are_typed_threaded_replies() {
    let (_root, a, _b) = two_clones();
    a.h5i_ok(&["msg", "as", "codex"]);
    a.h5i_ok(&["msg", "ask", "--from", "claude", "codex", "inspect failing auth test"]);

    // Populate the numbered view, then DONE the request.
    a.h5i_ok(&["msg", "inbox", "--as", "codex"]);
    a.h5i_ok(&["msg", "done", "1", "fixed in 1a2b3c4"]);

    let log = a.msg_log();
    // The original ASK and the threaded DONE reply.
    assert!(log.contains(r#""kind":"ASK""#), "log: {log}");
    assert!(log.contains(r#""kind":"DONE""#));
    // The DONE carries reply_to + thread_id pointing at the ASK.
    let ask_id = log
        .lines()
        .find(|l| l.contains(r#""kind":"ASK""#))
        .and_then(|l| l.split(r#""id":""#).nth(1))
        .and_then(|s| s.split('"').next())
        .expect("ask id")
        .to_string();
    assert!(log.contains(&format!(r#""reply_to":"{ask_id}""#)), "no reply_to: {log}");
    assert!(log.contains(&format!(r#""thread_id":"{ask_id}""#)), "no thread_id: {log}");
}

#[test]
fn risk_broadcast_carries_priority_and_focus() {
    let (_root, a, _b) = two_clones();
    a.h5i_ok(&[
        "msg", "risk", "--from", "lead", "--priority", "high", "--focus", "src/auth.rs",
        "all", "auth cache crosses request boundaries",
    ]);
    let log = a.msg_log();
    assert!(log.contains(r#""kind":"RISK""#), "log: {log}");
    assert!(log.contains(r#""priority":"high""#));
    assert!(log.contains(r#""to":"all""#));
}

#[test]
fn hook_output_frames_messages_as_untrusted() {
    let (_root, a, _b) = two_clones();
    a.h5i_ok(&["msg", "as", "dev"]);
    a.h5i_ok(&["msg", "review", "--from", "lead", "dev", "please review"]);
    let hook = out_str(&a.h5i_ok(&["msg", "hook", "--as", "dev"]));
    assert!(hook.contains("untrusted collaborator input"), "hook: {hook}");
    assert!(hook.contains("REVIEW_REQUEST"));
    // No imperative "New instruction:" framing.
    assert!(!hook.contains("New instruction"));
}

#[test]
fn hook_emits_systemmessage_json_by_default_plain_with_flag() {
    let (_root, a, _b) = two_clones();
    a.h5i_ok(&["msg", "as", "dev"]);
    a.h5i_ok(&["msg", "send", "--from", "lead", "dev", "ping one"]);

    // Default: a Claude Code `systemMessage` JSON object.
    let json = out_str(&a.h5i_ok(&["msg", "hook", "--as", "dev"]));
    assert!(json.trim_start().starts_with('{'), "expected JSON: {json}");
    assert!(json.contains("\"systemMessage\""), "no systemMessage: {json}");
    assert!(json.contains("ping one"));

    // --plain: raw text for Codex / other hosts.
    a.h5i_ok(&["msg", "send", "--from", "lead", "dev", "ping two"]);
    let plain = out_str(&a.h5i_ok(&["msg", "hook", "--as", "dev", "--plain"]));
    assert!(!plain.trim_start().starts_with('{'), "plain must not be JSON: {plain}");
    assert!(plain.contains("ping two"));
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
