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

    /// Run h5i with an explicit `H5I_AGENT` (mimics a host that injects the
    /// identity via env, where it wins over the stored default).
    fn h5i_as(&self, agent: &str, args: &[&str]) -> Output {
        Command::new(H5I)
            .args(args)
            .env("H5I_AGENT", agent)
            .current_dir(&self.dir)
            .output()
            .expect("failed to run h5i")
    }

    /// Spawn h5i without waiting — for launching many sends concurrently.
    fn h5i_spawn(&self, args: &[&str]) -> std::process::Child {
        use std::process::Stdio;
        Command::new(H5I)
            .args(args)
            .env_remove("H5I_AGENT")
            .current_dir(&self.dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn h5i")
    }

    /// Run h5i with `input` piped to stdin (for hook stop_hook_active tests).
    fn h5i_stdin(&self, args: &[&str], input: &str) -> Output {
        use std::io::Write;
        use std::process::Stdio;
        let mut child = Command::new(H5I)
            .args(args)
            .env_remove("H5I_AGENT")
            .current_dir(&self.dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn h5i");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().expect("wait h5i")
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
fn replay_streams_messages_oldest_first() {
    let (_root, a, _b) = two_clones();

    a.h5i_ok(&["msg", "send", "--from", "alice", "bob", "first"]);
    a.h5i_ok(&["msg", "send", "--from", "bob", "alice", "second"]);
    a.h5i_ok(&["msg", "send", "--from", "alice", "bob", "third"]);

    // --interval 0 keeps the test instant; --plain gives greppable rows that
    // are numbered 1..N (each message printed as its own batch must still climb).
    let out = out_str(&a.h5i_ok(&["msg", "replay", "--plain", "--interval", "0"]));
    let p1 = out.find("first").expect("missing first");
    let p2 = out.find("second").expect("missing second");
    let p3 = out.find("third").expect("missing third");
    assert!(p1 < p2 && p2 < p3, "replay not oldest-first: {out}");

    // Numbers climb across the whole thread, not reset per message.
    assert!(out.contains("1\t") && out.contains("2\t") && out.contains("3\t"),
        "replay numbering wrong: {out}");

    // --with restricts to one conversation partner.
    let conv = out_str(&a.h5i_ok(&[
        "msg", "replay", "--plain", "--interval", "0", "--with", "alice",
    ]));
    assert!(conv.contains("first") && conv.contains("third"), "conv: {conv}");
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
fn hook_is_a_clean_noop_inside_an_env_box() {
    // Regression: inside a confined env box ($H5I_ENV_ID set) the msg store
    // (.git/.h5i/msg) is sealed by design. The Stop hook is inherited from the
    // project settings and would otherwise hit EACCES advancing the per-agent
    // read cursor. It must no-op cleanly (exit 0, no output, no error) and leave
    // the message unread for the *host* session to deliver.
    let (_root, a, _b) = two_clones();
    a.h5i_ok(&["msg", "send", "--from", "lead", "dev", "in-box ping"]);

    // Run the hook as if nested in a box.
    let boxed = Command::new(H5I)
        .args(["msg", "hook", "--as", "dev"])
        .env_remove("H5I_AGENT")
        .env("H5I_ENV_ID", "env/dev/sandbox")
        .current_dir(&a.dir)
        .output()
        .expect("run h5i");
    assert!(
        boxed.status.success(),
        "in-box hook must exit 0: {}",
        out_str(&boxed)
    );
    assert!(
        boxed.stdout.is_empty(),
        "in-box hook must deliver nothing: {}",
        out_str(&boxed)
    );
    assert!(
        boxed.stderr.is_empty(),
        "in-box hook must not error: {}",
        String::from_utf8_lossy(&boxed.stderr)
    );

    // Read-state untouched → the host hook (no $H5I_ENV_ID) still delivers it.
    let host = Command::new(H5I)
        .args(["msg", "hook", "--as", "dev"])
        .env_remove("H5I_AGENT")
        .env_remove("H5I_ENV_ID")
        .current_dir(&a.dir)
        .output()
        .expect("run h5i");
    assert!(host.status.success(), "host hook failed: {}", out_str(&host));
    assert!(
        out_str(&host).contains("in-box ping"),
        "host hook must still deliver the unread message: {}",
        out_str(&host)
    );
}

#[test]
fn codex_sync_auto_delivers_inbox_then_clears() {
    let (_root, a, _b) = two_clones();
    // A claude send (also writes "claude" to the shared stored identity).
    a.h5i_ok(&["msg", "send", "--from", "claude", "codex", "review the parser"]);

    // `h5i codex sync` has no Codex session in tests, but still delivers the
    // codex inbox. Identity defaults to "codex" (ignores the stored "claude").
    let out = out_str(&a.h5i_ok(&["codex", "sync"]));
    assert!(out.contains("review the parser"), "codex sync didn't deliver: {out}");
    assert!(out.contains("untrusted collaborator input"), "no framing: {out}");

    // Marked read → a second sync doesn't repeat it.
    let out2 = out_str(&a.h5i_ok(&["codex", "sync"]));
    assert!(!out2.contains("review the parser"), "redelivered: {out2}");

    // And the numbered view is usable for a reply.
    a.h5i_ok(&["msg", "send", "--from", "claude", "codex", "second item"]);
    a.h5i_ok(&["codex", "prelude"]); // delivers + numbers
    let log_before = a.msg_log();
    a.h5i_ok(&["msg", "reply", "--from", "codex", "1", "on it"]);
    let log = a.msg_log();
    assert!(log.len() > log_before.len());
    assert!(log.contains("codex -> claude") || log.contains(r#""from":"codex""#), "reply not from codex: {log}");
}

#[test]
fn watch_plain_emits_one_stream_line_per_message() {
    let (_root, a, _b) = two_clones();
    a.h5i_ok(&["msg", "send", "--from", "claude", "codex", "review the parser"]);

    let out = out_str(&a.h5i_ok(&["msg", "watch", "--as", "codex", "--plain", "--once"]));
    assert!(out.contains("claude → codex"), "stream line missing: {out}");
    assert!(out.contains("review the parser"));
    // No box-drawing banner in the Monitor stream.
    assert!(!out.contains('┌') && !out.contains('│'), "box leaked into stream: {out}");
    assert_eq!(out.lines().filter(|l| !l.trim().is_empty()).count(), 1);
}

#[test]
fn watch_all_streams_channel_without_identity() {
    let (_root, a, _b) = two_clones();
    // A pre-existing message: firehose should NOT replay it (only new arrivals).
    a.h5i_ok(&["msg", "send", "--from", "claude", "codex", "old one"]);

    // No --as, no H5I_AGENT (harness removes it), but --all → no identity error.
    // --once with nothing new after start → exits cleanly, empty.
    let out = a.h5i(&["msg", "watch", "--all", "--once", "--plain"]);
    assert!(out.status.success(), "watch --all errored: {}", String::from_utf8_lossy(&out.stderr));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("no agent identity"), "should not need identity");

    // And plain `watch` with no identity at all also must not error on identity.
    let bare = a.h5i(&["msg", "watch", "--once", "--plain"]);
    assert!(bare.status.success(), "bare watch errored: {}", String::from_utf8_lossy(&bare.stderr));
}

#[test]
fn wait_returns_existing_unread_immediately() {
    let (_root, a, _b) = two_clones();
    a.h5i_ok(&["msg", "send", "--from", "claude", "codex", "already waiting"]);
    // Mail is already there → wait returns at once (well under the timeout).
    let out = a.h5i_ok(&["msg", "wait", "--as", "codex", "--timeout", "30", "--plain"]);
    assert!(out_str(&out).contains("already waiting"), "wait didn't return existing unread");
    // Peek semantics: it did NOT consume — inbox still has it.
    assert!(out_str(&a.h5i_ok(&["msg", "inbox", "--as", "codex"])).contains("already waiting"));
}

#[test]
fn wait_times_out_quietly_when_no_message() {
    let (_root, a, _b) = two_clones();
    let out = a.h5i_ok(&["msg", "wait", "--as", "codex", "--timeout", "1", "--interval", "1", "--plain"]);
    assert!(out.status.success(), "wait should exit 0 on timeout");
    assert!(out_str(&out).trim().is_empty(), "timeout should produce no output");
}

#[test]
fn wait_wakes_on_message_arriving_during_the_wait() {
    let (_root, a, _b) = two_clones();
    // Start waiting, then deliver a message ~1s later from another process.
    let waiter = a.h5i_spawn(&["msg", "wait", "--as", "codex", "--timeout", "20", "--interval", "1", "--plain"]);
    std::thread::sleep(std::time::Duration::from_millis(1200));
    a.h5i_ok(&["msg", "send", "--from", "claude", "codex", "arrived late"]);

    let out = waiter.wait_with_output().expect("waiter");
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("arrived late"),
        "wait did not wake on the late message: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn hook_block_emits_decision_block_and_honors_guard() {
    let (_root, a, _b) = two_clones();
    a.h5i_ok(&["msg", "send", "--from", "claude", "codex", "ping"]);

    // No stop_hook_active → block (force-continue) carrying the message.
    let blocked = out_str(&a.h5i_ok(&["msg", "hook", "--as", "codex", "--block"]));
    assert!(blocked.contains("\"decision\""), "no decision field: {blocked}");
    assert!(blocked.contains("block") && blocked.contains("ping"), "block payload: {blocked}");

    // A new message, but stop_hook_active=true → guard suppresses (no loop).
    a.h5i_ok(&["msg", "send", "--from", "claude", "codex", "ping2"]);
    let guarded = out_str(&a.h5i_stdin(
        &["msg", "hook", "--as", "codex", "--block"],
        r#"{"stop_hook_active":true}"#,
    ));
    assert!(guarded.trim().is_empty(), "guard did not suppress: {guarded}");
}

#[test]
fn session_start_notes_unread_but_does_not_push_monitor() {
    let (_root, a, _b) = two_clones();
    // Identity via env (as a real host injects it) so a `--from codex` send
    // can't clobber which inbox session-start checks.

    // No unread → silent about messaging (no noise in unrelated repos).
    let quiet = out_str(&a.h5i_as("claude", &["hook", "session-start"]));
    assert!(!quiet.contains("unread message"), "noisy when no mail: {quiet}");

    // With unread → a read-only note (not consumed), and NOT a Monitor directive.
    a.h5i_ok(&["msg", "send", "--from", "codex", "claude", "look at this"]);
    let out = out_str(&a.h5i_as("claude", &["hook", "session-start"]));
    assert!(out.contains("1 unread message for claude"), "no unread note: {out}");
    assert!(out.contains("h5i msg inbox"), "no read hint: {out}");
    assert!(!out.contains("Monitor tool"), "should not push Monitor: {out}");

    // The note must NOT consume it — turn delivery still gets it.
    let still = out_str(&a.h5i_as("claude", &["msg", "inbox"]));
    assert!(still.contains("look at this"), "session note consumed the message: {still}");
}

#[test]
fn msg_setup_writes_project_settings_idempotently() {
    let (_root, a, _b) = two_clones();

    // Default scope is project, default hook is autonomous (--block).
    a.h5i_ok(&["msg", "setup", "claude"]);
    let settings_path = a.dir.join(".claude").join("settings.json");
    assert!(settings_path.exists(), "settings.json not written");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(v["env"]["H5I_AGENT"], "claude");
    assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"], "h5i msg hook --block");

    // Re-running is idempotent — still exactly one msg hook entry.
    a.h5i_ok(&["msg", "setup", "claude"]);
    let v2: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    let stop = v2["hooks"]["Stop"].as_array().unwrap();
    let n = stop
        .iter()
        .filter(|e| e["hooks"][0]["command"].as_str().map(|s| s.starts_with("h5i msg hook")).unwrap_or(false))
        .count();
    assert_eq!(n, 1, "duplicate msg hooks after re-run");

    // --no-block switches to the notify-only hook (and stays idempotent).
    a.h5i_ok(&["msg", "setup", "claude", "--no-block"]);
    let v3: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(v3["hooks"]["Stop"][0]["hooks"][0]["command"], "h5i msg hook");
    assert_eq!(v3["hooks"]["Stop"].as_array().unwrap().len(), 1);
}

#[test]
fn concurrent_sends_all_land_via_cas() {
    // Fire many sends at the SAME clone at once. The compare-and-swap retry in
    // append_message_cas must let every one land — no lost appends, no clobber.
    let (_root, a, _b) = two_clones();
    const N: usize = 8;
    let bodies: Vec<String> = (0..N).map(|i| format!("concurrent-{i}")).collect();

    let mut kids = Vec::new();
    for b in &bodies {
        kids.push(a.h5i_spawn(&["msg", "send", "--from", "alice", "bob", b.as_str()]));
    }
    for k in kids {
        let out = k.wait_with_output().expect("wait send");
        assert!(out.status.success(), "a send failed: {}", String::from_utf8_lossy(&out.stderr));
    }

    // Every distinct message is present exactly once.
    let log = a.msg_log();
    let to_bob = log.lines().filter(|l| l.contains(r#""to":"bob""#)).count();
    assert_eq!(to_bob, N, "lost or duplicated appends under contention:\n{log}");
    for b in &bodies {
        assert_eq!(log.matches(b.as_str()).count(), 1, "missing/dup body {b}");
    }
}

#[test]
fn cross_clone_review_done_thread_roundtrip() {
    // The headline workflow end-to-end across two clones:
    //   A(claude) review → push → B(codex) pull + codex sync → done → push → A pull
    let (_root, a, b) = two_clones();

    a.h5i_ok(&[
        "msg", "review", "--from", "claude",
        "--branch", "auth", "--focus", "src/auth.rs", "--pr", "42",
        "codex", "review token refresh",
    ]);
    a.h5i_ok(&["share", "push"]);

    // B pulls and Codex auto-delivery surfaces it (and numbers it for reply).
    b.h5i_ok(&["share", "pull"]);
    let delivered = out_str(&b.h5i_ok(&["codex", "sync"]));
    assert!(delivered.contains("review token refresh"), "codex didn't get it: {delivered}");

    b.h5i_ok(&["msg", "done", "--from", "codex", "1", "fixed in 1a2b3c4"]);
    b.h5i_ok(&["share", "push"]);

    // A pulls the merged log and sees the threaded DONE with the structured
    // review fields intact.
    a.h5i_ok(&["share", "pull"]);
    let msgs: Vec<serde_json::Value> = a
        .msg_log()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("valid jsonl"))
        .collect();

    let review = msgs.iter().find(|m| m["kind"] == "REVIEW_REQUEST").expect("review on A");
    let done = msgs.iter().find(|m| m["kind"] == "DONE").expect("DONE on A");
    let rid = review["id"].as_str().unwrap();

    // Structured review fields survived the push/pull round-trip.
    assert_eq!(review["from"], "claude");
    assert_eq!(review["branch"], "auth");
    assert_eq!(review["focus"][0], "src/auth.rs");
    assert_eq!(review["links"]["pr"], 42);

    // The DONE is a threaded reply from codex back to claude.
    assert_eq!(done["from"], "codex");
    assert_eq!(done["to"], "claude");
    assert_eq!(done["reply_to"], rid, "DONE not threaded to the review");
    assert_eq!(done["thread_id"], rid, "DONE thread root wrong");
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

// ─── realistic multi-agent + read-state regressions ──────────────────────────

#[test]
fn watch_is_non_destructive_does_not_consume_unread() {
    // Regression: `h5i msg watch` (a passive dashboard) used to call inbox with
    // advance=true every tick, so watching as an identity silently consumed that
    // identity's unread mail before the Stop hook / `inbox` could surface it.
    // (This is the bug a side-terminal `watch` as `claude` actually hit.)
    let (_root, a, _b) = two_clones();
    a.h5i_ok(&["msg", "send", "--from", "codex", "claude", "please review auth"]);

    // Watching one tick DISPLAYS the message…
    let w = out_str(&a.h5i_as("claude", &["msg", "watch", "--once", "--plain"]));
    assert!(w.contains("please review auth"), "watch did not show the message: {w}");

    // …but does NOT consume it: an explicit inbox read still delivers it.
    let inbox = out_str(&a.h5i_as("claude", &["msg", "inbox"]));
    assert!(
        inbox.contains("please review auth"),
        "watch consumed unread mail (read-state regression): {inbox}"
    );

    // The explicit read is what advances the cursor.
    let inbox2 = out_str(&a.h5i_as("claude", &["msg", "inbox"]));
    assert!(inbox2.contains("No new messages"), "inbox did not advance: {inbox2}");
}

#[test]
fn watch_shows_both_directions_not_just_inbox() {
    // Regression: `watch --as me` used to stream only the inbox (messages
    // addressed *to* me), so a conversation looked one-sided. It must show the
    // agent's full conversation — sent, received, and broadcasts — like history.
    let (_root, a, _b) = two_clones();
    a.h5i_ok(&["msg", "send", "--from", "codex", "claude", "incoming ping"]);
    a.h5i_ok(&["msg", "send", "--from", "claude", "codex", "outgoing pong"]);
    a.h5i_ok(&["msg", "send", "--from", "claude", "all", "broadcast hi"]);

    let w = out_str(&a.h5i_as("claude", &["msg", "watch", "--once", "--plain"]));
    assert!(w.contains("incoming ping"), "watch dropped incoming: {w}");
    assert!(w.contains("outgoing pong"), "watch dropped the agent's OWN sent message: {w}");
    assert!(w.contains("broadcast hi"), "watch dropped broadcast: {w}");
}

#[test]
fn three_agents_have_independent_read_state_in_one_clone() {
    // claude, codex, reviewer share one clone (the realistic setup). Each has its
    // own per-identity cursor, so one agent reading its inbox must never consume
    // another's.
    let (_root, a, _b) = two_clones();
    a.h5i_ok(&["msg", "send", "--from", "claude", "codex", "codex task"]);
    a.h5i_ok(&["msg", "send", "--from", "claude", "reviewer", "reviewer task"]);

    // codex reads its inbox — sees only its own, consumes only its own.
    let cx = out_str(&a.h5i_ok(&["msg", "inbox", "--as", "codex"]));
    assert!(cx.contains("codex task"), "codex missing its task: {cx}");
    assert!(!cx.contains("reviewer task"), "codex saw reviewer's mail: {cx}");

    // reviewer's inbox is untouched by codex's read.
    let rv = out_str(&a.h5i_ok(&["msg", "inbox", "--as", "reviewer"]));
    assert!(rv.contains("reviewer task"), "reviewer lost its mail after codex read: {rv}");

    // Both cursors are now independently advanced.
    assert!(out_str(&a.h5i_ok(&["msg", "inbox", "--as", "codex"])).contains("No new"));
    assert!(out_str(&a.h5i_ok(&["msg", "inbox", "--as", "reviewer"])).contains("No new"));
}

#[test]
fn broadcast_is_unread_independently_per_recipient() {
    // A broadcast fans out to every non-sender, and each recipient's read-state
    // is independent — one reading it must not mark it read for the others.
    let (_root, a, _b) = two_clones();
    a.h5i_ok(&["msg", "send", "--from", "claude", "all", "standup in 5"]);

    // codex reads (and consumes) the broadcast.
    assert!(
        out_str(&a.h5i_ok(&["msg", "inbox", "--as", "codex"])).contains("standup in 5"),
        "codex did not receive broadcast"
    );
    // reviewer still sees it — read-state did not leak across recipients.
    assert!(
        out_str(&a.h5i_ok(&["msg", "inbox", "--as", "reviewer"])).contains("standup in 5"),
        "broadcast read-state leaked across recipients"
    );
    // The sender never receives its own broadcast.
    assert!(out_str(&a.h5i_ok(&["msg", "inbox", "--as", "claude"])).contains("No new"));
}

#[test]
fn union_merge_dedups_the_shared_base_message() {
    // The common-ancestor message exists on BOTH diverged sides; the union-merge
    // must keep exactly one copy (G-Set idempotency), and re-pulling stays a no-op.
    let (_root, a, b) = two_clones();
    a.h5i_ok(&["msg", "send", "--from", "alice", "bob", "BASE"]);
    a.h5i_ok(&["share", "push"]);
    b.h5i_ok(&["share", "pull"]); // both clones now hold BASE

    // Diverge: each sends while "offline".
    a.h5i_ok(&["msg", "send", "--from", "alice", "bob", "X"]);
    b.h5i_ok(&["msg", "send", "--from", "carol", "bob", "Y"]);
    a.h5i_ok(&["share", "push"]);
    b.h5i_ok(&["share", "pull"]); // B union-merges

    let log = b.msg_log();
    assert_eq!(log.matches(r#""body":"BASE""#).count(), 1, "BASE duplicated by merge: {log}");
    assert!(log.contains(r#""body":"X""#) && log.contains(r#""body":"Y""#), "lost a side: {log}");

    // Re-pulling when already converged adds nothing.
    b.h5i_ok(&["share", "pull"]);
    assert_eq!(b.msg_log().matches(r#""body":"BASE""#).count(), 1, "re-pull duplicated BASE");
}

#[test]
fn structured_fields_survive_divergent_union_merge() {
    // A rich REVIEW_REQUEST (kind + branch + focus + risk) must survive a
    // cross-clone union-merge byte-for-byte, not just the body.
    let (_root, a, b) = two_clones();
    a.h5i_ok(&["msg", "send", "--from", "alice", "bob", "base"]);
    a.h5i_ok(&["share", "push"]);
    b.h5i_ok(&["share", "pull"]);

    // A files a structured review while offline; B sends something concurrently.
    a.h5i_ok(&[
        "msg", "review", "--from", "alice",
        "--branch", "auth", "--focus", "src/auth.rs", "--risk", "expiry edge cases",
        "bob", "check token refresh",
    ]);
    b.h5i_ok(&["msg", "send", "--from", "carol", "bob", "meanwhile"]);
    a.h5i_ok(&["share", "push"]);
    b.h5i_ok(&["share", "pull"]); // union-merge on B

    let log = b.msg_log();
    assert!(log.contains(r#""kind":"REVIEW_REQUEST""#), "kind lost in merge: {log}");
    assert!(log.contains(r#""branch":"auth""#), "branch lost in merge: {log}");
    assert!(log.contains("src/auth.rs"), "focus lost in merge: {log}");
    assert!(log.contains("expiry edge cases"), "risk lost in merge: {log}");
}
