//! End-to-end tests for the forum (`h5i forum`).
//!
//! The unit tests in `crates/h5i-core/src/forum*.rs` cover the pure decisions:
//! what a role may do, what a ceiling refuses, how a status projects. They
//! cannot catch the failures that would actually break the product, because
//! every one of those lives in the seam between a box and the host — a binding
//! file the injection path does not read, a spool record the drain filters out,
//! an inbox nobody writes, an identity taken from the wrong place.
//!
//! So these drive the **compiled binary** against a real repository with real
//! boxes in it, and simulate the in-box side the way the host does: by setting
//! the same four variables `env run` injects. That is not a shortcut around the
//! sandbox — it is exactly the interface the in-box CLI has, and running it this
//! way lets the whole round trip be tested on a host of any tier.
//!
//! The three claims under test are the ones the product is sold on:
//!
//! 1. **The box writes what, never who.** A record that names its own sender
//!    does not have that field read.
//! 2. **Communication never grants authority.** A box that exceeds a thread's
//!    ceiling is refused, and no post moves what any box can reach.
//! 3. **Only humans change trust boundaries.** The four governing verbs are
//!    refused inside a box, and revocation takes effect on the next pass.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

const H5I: &str = env!("CARGO_BIN_EXE_h5i");

// ─── a repository with boxes in it ───────────────────────────────────────────

struct Repo {
    dir: PathBuf,
    _root: TempDir,
}

impl Repo {
    fn new() -> Repo {
        let root = TempDir::new().expect("tempdir");
        let dir = root.path().join("repo");
        ok(Command::new("git").args(["init", "-b", "main"]).arg(&dir));
        git(&dir, &["config", "user.name", "Forum Tester"]);
        git(&dir, &["config", "user.email", "forum@h5i.test"]);
        std::fs::write(dir.join("README.md"), "seed\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-m", "seed"]);
        Repo { dir, _root: root }
    }

    /// Run a host-side `h5i` command that is expected to succeed.
    fn h5i(&self, args: &[&str]) -> Output {
        let out = self.try_h5i(args);
        assert!(
            out.status.success(),
            "h5i {} failed:\nstdout: {}\nstderr: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        out
    }

    fn try_h5i(&self, args: &[&str]) -> Output {
        Command::new(H5I)
            .args(args)
            .env("H5I_AGENT", "tester")
            .env("H5I_DEFAULT_ISOLATION", "workspace")
            .current_dir(&self.dir)
            .output()
            .expect("failed to run h5i")
    }

    /// Run `h5i` as if from inside `box_id`, with the four variables the host
    /// injects when it starts a session. This is the in-box CLI's whole
    /// interface to the outside, so exercising it here exercises the real path.
    fn in_box(&self, box_id: &str, identity: &str, args: &[&str]) -> Output {
        let dir = self.env_dir(box_id);
        std::fs::create_dir_all(dir.join("spool")).unwrap();
        std::fs::create_dir_all(dir.join("inbox")).unwrap();
        Command::new(H5I)
            .args(args)
            .env("H5I_ENV_ID", box_id)
            .env("H5I_ENV_POLICY_DIGEST", "sha256:test")
            .env("H5I_ENV_CAPTURE_SPOOL", dir.join("spool"))
            .env("H5I_ENV_INBOX", dir.join("inbox"))
            .env("H5I_AGENT", identity)
            .current_dir(&self.dir)
            .output()
            .expect("failed to run in-box h5i")
    }

    fn env_dir(&self, box_id: &str) -> PathBuf {
        // `env/<agent>/<slug>` under the sidecar root.
        self.dir.join(".git/.h5i").join(box_id)
    }

    /// Put a box on the forum.
    ///
    /// Always `--allow-unconfined`, because the harness pins the workspace tier
    /// to stay hermetic (box creation never probes the host) and the forum
    /// refuses an unconfined participant by default. These tests are about
    /// forum mechanics, not about confinement; the guard itself has its own two
    /// tests at the bottom of this file.
    fn attach(&self, box_name: &str, as_name: &str, role: &str) -> Output {
        self.h5i(&[
            "forum",
            "attach",
            box_name,
            "--as",
            as_name,
            "--role",
            role,
            "--allow-unconfined",
        ])
    }

    /// Open a thread and return its id.
    fn create_thread(&self, title: &str, ceiling: Option<&str>) -> String {
        let mut args = vec!["forum", "create", title];
        if let Some(c) = ceiling {
            args.push("--ceiling");
            args.push(c);
        }
        self.h5i(&args);
        let listing = self.forum_json();
        listing["threads"][0]["header"]["id"]
            .as_str()
            .expect("a thread id")
            .to_string()
    }

    fn forum_json(&self) -> serde_json::Value {
        let out = self.h5i(&["forum", "status", "--json"]);
        serde_json::from_slice(&out.stdout).expect("forum status --json")
    }

    fn thread_json(&self, id: &str) -> serde_json::Value {
        let out = self.h5i(&["forum", "read", id, "--json"]);
        serde_json::from_slice(&out.stdout).expect("forum read --json")
    }
}

fn git(dir: &Path, args: &[&str]) {
    ok(Command::new("git").args(args).current_dir(dir));
}

fn ok(cmd: &mut Command) {
    let out = cmd.output().expect("spawn");
    assert!(
        out.status.success(),
        "{:?} failed: {}",
        cmd,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// ─── the round trip ──────────────────────────────────────────────────────────

#[test]
fn two_boxes_hold_a_conversation_through_the_host() {
    let repo = Repo::new();
    let thread = repo.create_thread("fix the auth refresh race", None);
    repo.h5i(&["box", "create", "worker-box"]);
    repo.h5i(&["box", "create", "review-box"]);
    repo.attach("worker-box", "claude-worker", "worker");
    repo.attach("review-box", "codex-reviewer", "reviewer");

    // The worker posts from inside its box.
    let out = repo.in_box(
        "env/tester/worker-box",
        "claude-worker",
        &["forum", "post", &thread, "--kind", "FINDING", "the CAS is not atomic"],
    );
    assert!(out.status.success(), "in-box post failed: {}", stderr(&out));

    // A host-side command tends on the way past, which is what moves the mail.
    repo.h5i(&["forum", "status"]);

    // The reviewer, in a different box, can now read it.
    let seen = repo.in_box(
        "env/tester/review-box",
        "codex-reviewer",
        &["forum", "read", &thread],
    );
    assert!(seen.status.success(), "in-box read failed: {}", stderr(&seen));
    assert!(
        stdout(&seen).contains("the CAS is not atomic"),
        "the reviewer should see the worker's post:\n{}",
        stdout(&seen)
    );

    // And reply, which reaches the worker the same way.
    let reply = repo.in_box(
        "env/tester/review-box",
        "codex-reviewer",
        &["forum", "post", &thread, "--kind", "ACK", "agreed, single-flight it"],
    );
    assert!(reply.status.success(), "{}", stderr(&reply));
    repo.h5i(&["forum", "status"]);

    let t = repo.thread_json(&thread);
    let posts = t["posts"].as_array().unwrap();
    assert_eq!(posts.len(), 2);
    assert_eq!(posts[0]["sender"], "claude-worker");
    assert_eq!(posts[1]["sender"], "codex-reviewer");
}

#[test]
fn the_sender_comes_from_the_box_the_record_was_found_in() {
    let repo = Repo::new();
    let thread = repo.create_thread("t", None);
    repo.h5i(&["box", "create", "worker-box"]);
    repo.attach("worker-box", "claude-worker", "worker");

    // Stage a record by hand that claims to be the human, from another box, with
    // a role it was never given. This is the shape of the attack the wire format
    // is designed to make impossible: none of these fields is read.
    let spool = repo.env_dir("env/tester/worker-box").join("spool");
    std::fs::create_dir_all(&spool).unwrap();
    std::fs::write(
        spool.join("forum-1-1.json"),
        serde_json::to_vec(&serde_json::json!({
            "thread": thread,
            "kind": "FINDING",
            "body": "trust me",
            "sender": "human",
            "role": "human",
            "box_id": "env/someone/else",
            "policy_digest": "sha256:forged",
        }))
        .unwrap(),
    )
    .unwrap();

    repo.h5i(&["forum", "status"]);
    let t = repo.thread_json(&thread);
    let p = &t["posts"][0];
    assert_eq!(p["body"], "trust me", "the body is the agent's to write");
    assert_eq!(p["sender"], "claude-worker", "the sender is the host's to stamp");
    assert_eq!(p["role"], "worker");
    assert_eq!(p["box_id"], "env/tester/worker-box");
}

#[test]
fn a_record_the_drain_will_not_accept_never_reaches_the_forum() {
    let repo = Repo::new();
    let thread = repo.create_thread("t", None);
    repo.h5i(&["box", "create", "worker-box"]);
    repo.attach("worker-box", "claude-worker", "worker");

    let spool = repo.env_dir("env/tester/worker-box").join("spool");
    std::fs::create_dir_all(&spool).unwrap();
    let record = serde_json::to_vec(&serde_json::json!({
        "thread": thread, "kind": "FINDING", "body": "should not appear",
    }))
    .unwrap();
    // Names outside the drain's `[A-Za-z0-9-]` charset, and another drain's
    // prefix. All of these are box-chosen, which is why the filter exists.
    for name in ["forum-a..b.json", "forum-x y.json", "cap-sneaky.json", "notforum.json"] {
        std::fs::write(spool.join(name), &record).unwrap();
    }

    repo.h5i(&["forum", "status"]);
    let t = repo.thread_json(&thread);
    assert_eq!(
        t["posts"].as_array().unwrap().len(),
        0,
        "no record with a rejected name may be posted"
    );
}

// ─── who may do what ─────────────────────────────────────────────────────────

#[test]
fn the_governing_verbs_are_refused_inside_a_box() {
    let repo = Repo::new();
    let thread = repo.create_thread("t", None);
    repo.h5i(&["box", "create", "worker-box"]);
    repo.attach("worker-box", "claude-worker", "worker");

    for args in [
        vec!["forum", "create", "a thread of my own"],
        vec!["forum", "attach", "worker-box", "--as", "me"],
        vec!["forum", "revoke", "codex-reviewer"],
        vec!["forum", "close", thread.as_str()],
    ] {
        let out = repo.in_box("env/tester/worker-box", "claude-worker", &args);
        assert!(
            !out.status.success(),
            "`h5i {}` must be refused inside a box",
            args.join(" ")
        );
        assert!(
            stderr(&out).contains("only the human on the host"),
            "the refusal should say why:\n{}",
            stderr(&out)
        );
    }
}

#[test]
fn an_observer_may_read_and_may_not_post() {
    let repo = Repo::new();
    let thread = repo.create_thread("t", None);
    repo.h5i(&["box", "create", "watch-box"]);
    repo.attach("watch-box", "watcher", "observer");
    repo.h5i(&["forum", "post", &thread, "--kind", "FINDING", "something to see"]);
    repo.h5i(&["forum", "status"]);

    let read = repo.in_box("env/tester/watch-box", "watcher", &["forum", "read", &thread]);
    assert!(read.status.success());
    assert!(stdout(&read).contains("something to see"));

    // The observer stages a post; the host refuses it at ingest, because the
    // role check lives where authority is decided, not where text is typed.
    let post = repo.in_box(
        "env/tester/watch-box",
        "watcher",
        &["forum", "post", &thread, "--kind", "FINDING", "let me in"],
    );
    assert!(post.status.success(), "staging succeeds; ingest is the gate");
    repo.h5i(&["forum", "status"]);

    let t = repo.thread_json(&thread);
    let bodies: Vec<&str> = t["posts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["body"].as_str().unwrap())
        .collect();
    assert!(
        !bodies.contains(&"let me in"),
        "an observer's post must not reach the forum: {bodies:?}"
    );
}

// ─── the ceiling ─────────────────────────────────────────────────────────────

#[test]
fn a_box_that_exceeds_the_ceiling_is_refused_and_not_re_confined() {
    let repo = Repo::new();
    // A ceiling that denies network, and a box whose profile allows it.
    std::fs::create_dir_all(repo.dir.join(".h5i")).unwrap();
    std::fs::write(
        repo.dir.join(".h5i/env.toml"),
        r#"
[profile.sealed]
isolation = "workspace"

[profile.sealed.net]
mode = "deny"

[profile.reaching]
isolation = "workspace"

[profile.reaching.net]
mode = "host"
"#,
    )
    .unwrap();
    git(&repo.dir, &["add", "."]);
    git(&repo.dir, &["commit", "-m", "policy"]);

    repo.create_thread("sealed work", Some("sealed"));
    repo.h5i(&["box", "create", "loose-box", "--profile", "reaching"]);

    // `--allow-unconfined` so the tier guard is not what refuses this: the
    // assertion below is about the *ceiling*, and a test that passed for the
    // wrong reason would stop covering it.
    let out = repo.try_h5i(&[
        "forum",
        "attach",
        "loose-box",
        "--as",
        "loose",
        "--role",
        "worker",
        "--allow-unconfined",
    ]);
    assert!(!out.status.success(), "an over-privileged box must not attach");
    let msg = stderr(&out);
    assert!(msg.contains("exceeds the ceiling"), "{msg}");
    assert!(
        msg.contains("net.mode"),
        "the refusal should name what exceeded it:\n{msg}"
    );

    // Refused means refused: nothing was written, so the box is not half on the
    // forum with a quietly weakened profile.
    let forum = repo.forum_json();
    assert!(
        forum["roster"].as_array().unwrap().is_empty(),
        "a refused attach must leave no roster entry"
    );
    let binding = repo.env_dir("env/tester/loose-box").join("team-identity");
    assert!(!binding.exists(), "a refused attach must leave no binding");
}

#[test]
fn a_box_under_the_ceiling_attaches() {
    let repo = Repo::new();
    std::fs::create_dir_all(repo.dir.join(".h5i")).unwrap();
    std::fs::write(
        repo.dir.join(".h5i/env.toml"),
        r#"
[profile.sealed]
isolation = "workspace"

[profile.sealed.net]
mode = "deny"
"#,
    )
    .unwrap();
    git(&repo.dir, &["add", "."]);
    git(&repo.dir, &["commit", "-m", "policy"]);

    repo.create_thread("sealed work", Some("sealed"));
    repo.h5i(&["box", "create", "tight-box", "--profile", "sealed"]);
    repo.attach("tight-box", "tight", "worker");

    let forum = repo.forum_json();
    assert_eq!(forum["roster"][0]["agent"], "tight");
}

// ─── revocation ──────────────────────────────────────────────────────────────

#[test]
fn revoking_takes_the_conversation_away_and_records_what_comes_after() {
    let repo = Repo::new();
    let thread = repo.create_thread("t", None);
    repo.h5i(&["box", "create", "worker-box"]);
    repo.attach("worker-box", "claude-worker", "worker");
    repo.h5i(&["forum", "post", &thread, "--kind", "FINDING", "context"]);
    repo.h5i(&["forum", "status"]);

    let inbox = repo.env_dir("env/tester/worker-box").join("inbox");
    assert!(
        std::fs::read_dir(&inbox).unwrap().count() > 0,
        "the box should have the thread before revocation"
    );

    repo.h5i(&["forum", "revoke", "claude-worker"]);
    assert_eq!(
        std::fs::read_dir(&inbox).unwrap().count(),
        0,
        "revocation must take the conversation out of the box immediately"
    );

    // It keeps posting anyway. The post is recorded carrying its refusal, not
    // dropped: a forum that silently swallows what it refuses teaches its
    // readers that nothing was refused.
    let spool = repo.env_dir("env/tester/worker-box").join("spool");
    std::fs::create_dir_all(&spool).unwrap();
    std::fs::write(
        spool.join("forum-9-9.json"),
        serde_json::to_vec(&serde_json::json!({
            "thread": thread, "kind": "CLAIM", "body": "still here",
        }))
        .unwrap(),
    )
    .unwrap();
    repo.h5i(&["forum", "status"]);

    let t = repo.thread_json(&thread);
    let last = t["posts"].as_array().unwrap().last().unwrap();
    assert_eq!(last["body"], "still here");
    assert!(
        last["denied"].as_str().unwrap().contains("revoked"),
        "the refusal must be on the record: {last}"
    );
    assert_eq!(
        t["status"], "open",
        "a refused CLAIM must not move the thread's state"
    );
}

// ─── the taint ───────────────────────────────────────────────────────────────

#[test]
fn a_box_shown_a_peers_text_is_marked_and_one_that_is_not_stays_clean() {
    let repo = Repo::new();
    let thread = repo.create_thread("t", None);
    repo.h5i(&["box", "create", "talker"]);
    repo.h5i(&["box", "create", "verifier"]);
    repo.attach("talker", "talker", "worker");
    repo.h5i(&["forum", "post", &thread, "--kind", "FINDING", "peer text"]);
    repo.h5i(&["forum", "status"]);

    let status = stdout(&repo.h5i(&["box", "status", "talker"]));
    assert!(
        status.contains("peer-influenced"),
        "a box that was shown a peer's text must say so:\n{status}"
    );

    // The verifier was never attached, so it read nothing and is untainted.
    // This is the whole mechanism behind "check it with a box that read none of
    // it": not a flag, just a box that is not on the forum.
    let clean = stdout(&repo.h5i(&["box", "status", "verifier"]));
    assert!(
        !clean.contains("peer-influenced"),
        "a box that was never on the forum must stay clean:\n{clean}"
    );
}

// ─── the closed thread ───────────────────────────────────────────────────────

#[test]
fn closing_hides_a_thread_from_the_boxes_and_keeps_it_for_the_human() {
    let repo = Repo::new();
    let thread = repo.create_thread("t", None);
    repo.h5i(&["box", "create", "worker-box"]);
    repo.attach("worker-box", "claude-worker", "worker");
    repo.h5i(&["forum", "post", &thread, "--kind", "FINDING", "note"]);
    repo.h5i(&["forum", "status"]);

    repo.h5i(&["forum", "close", &thread]);

    let inbox = repo.env_dir("env/tester/worker-box").join("inbox");
    assert_eq!(
        std::fs::read_dir(&inbox).unwrap().count(),
        0,
        "a closed thread must leave the box's inbox"
    );

    let listed = stdout(&repo.h5i(&["forum", "list"]));
    assert!(!listed.contains(&thread[..8]), "closed threads leave the live list");
    // Closing is a post, so the ref is still there and nothing was deleted.
    assert!(
        stdout(&repo.h5i(&["forum", "read", &thread])).contains("closed"),
        "the close itself is on the record"
    );

    let all = stdout(&repo.h5i(&["forum", "list", "--all"]));
    assert!(all.contains(&thread[..8]), "and stay readable with --all");

    let t = repo.thread_json(&thread);
    let kinds: Vec<&str> = t["posts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        vec!["FINDING", "CLOSED"],
        "closing appends rather than deleting, and the note survives"
    );
    assert_eq!(t["status"], "closed");
}

// ─── the tier the forum rests on ─────────────────────────────────────────────

/// A box the host cannot confine is refused, because it could rewrite the forum.
///
/// Measured before this guard existed: on the workspace tier a box read the
/// forum's bare repository, wrote a file into it, and deleted a ref. Every
/// other tier makes those paths invisible — a stat returns ENOENT, not
/// EACCES. Attaching is the moment the forum starts making claims about a
/// participant, and it must not make them about one that can edit the claims.
#[test]
fn a_box_the_host_cannot_confine_is_refused() {
    let repo = Repo::new();
    repo.create_thread("t", None);
    repo.h5i(&["box", "create", "loose"]); // the harness pins the workspace tier

    let out = repo.try_h5i(&["forum", "attach", "loose", "--as", "loose"]);
    assert!(!out.status.success(), "an unconfined box must not attach");
    let msg = stderr(&out);
    assert!(msg.contains("workspace tier"), "{msg}");
    assert!(
        msg.contains("--isolation process"),
        "the refusal should say how to fix it:\n{msg}"
    );
    assert!(
        repo.forum_json()["roster"].as_array().unwrap().is_empty(),
        "a refused attach must leave no roster entry"
    );
}

/// And the operator can still take that risk deliberately, on a host that has
/// no kernel tier at all — loudly, and never by default.
#[test]
fn an_unconfined_box_can_be_attached_only_with_the_explicit_flag() {
    let repo = Repo::new();
    repo.create_thread("t", None);
    repo.h5i(&["box", "create", "loose"]);

    let out = repo.h5i(&[
        "forum",
        "attach",
        "loose",
        "--as",
        "loose",
        "--allow-unconfined",
    ]);
    assert_eq!(repo.forum_json()["roster"][0]["agent"], "loose");
    assert!(
        stdout(&out).contains("unconfined") || stderr(&out).contains("unconfined"),
        "attaching one anyway must say so:\nstdout: {}\nstderr: {}",
        stdout(&out),
        stderr(&out)
    );
}

/// A box id is a path, and paths get reused.
///
/// Remove a box and create another with the same name and it inherits
/// `env/<agent>/<slug>` — and, if membership were decided by the roster alone,
/// a human's decision about a *different* box. Measured before the check
/// existed: a recreated box that had never been attached was handed the forum's
/// threads on the first pass.
///
/// Membership is confirmed from both ends now. The roster says which identity a
/// box carries; the binding file in its env directory says the box was actually
/// bound to it, and `box rm` takes that file with the directory.
#[test]
fn a_recreated_box_does_not_inherit_the_membership_of_the_one_it_replaced() {
    let repo = Repo::new();
    repo.create_thread("t", None);
    repo.h5i(&["box", "create", "worker-box"]);
    repo.attach("worker-box", "claude-worker", "worker");
    repo.h5i(&["forum", "status"]);

    let inbox = repo.env_dir("env/tester/worker-box").join("inbox");
    assert_eq!(
        std::fs::read_dir(&inbox).unwrap().count(),
        1,
        "the attached box should have the thread"
    );

    repo.h5i(&["box", "rm", "worker-box", "--force"]);
    repo.h5i(&["box", "create", "worker-box"]);
    repo.h5i(&["forum", "status"]);

    let inbox = repo.env_dir("env/tester/worker-box").join("inbox");
    let delivered = std::fs::read_dir(&inbox).map(|d| d.count()).unwrap_or(0);
    assert_eq!(
        delivered, 0,
        "a box that was never attached must not be handed the conversation"
    );

    // And attaching it for real still works, retiring the identity it replaced.
    repo.attach("worker-box", "claude-2", "worker");
    repo.h5i(&["forum", "status"]);
    assert_eq!(
        std::fs::read_dir(&inbox).unwrap().count(),
        1,
        "a genuine attach delivers again"
    );

    let roster = repo.forum_json();
    let rows = roster["roster"].as_array().unwrap();
    let active: Vec<&str> = rows
        .iter()
        .filter(|e| e["revoked_at"].is_null())
        .map(|e| e["agent"].as_str().unwrap())
        .collect();
    assert_eq!(active, vec!["claude-2"], "one identity per box: {rows:?}");
}
