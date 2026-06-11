//! End-to-end tests for `h5i env` — isolated agent environments
//! (worktree + sandbox + provenance, docs/environments-design.md).
//!
//! These tests drive the compiled binary as a subprocess against real git
//! repositories and prove the properties that define the feature:
//!
//!   1. `create` fuses a frozen base, a code branch, a git worktree under
//!      `.git/.h5i/env/`, a forked reasoning branch, and a pinned policy.
//!   2. `run` is capture-wrapped and policy-enforced: evidence lands in
//!      `refs/h5i/objects` tagged with the env id + policy digest, and the
//!      child's exit code passes through.
//!   3. `propose`/`apply` is the only road into the parent branch — apply
//!      refuses without propose, and the mediated commit fails closed on
//!      path-allowlist violations (nested `.git`).
//!   4. Isolation claims fail closed: an unsatisfiable claim refuses at
//!      `create`, it never silently downgrades.
//!   5. The kernel sandbox actually confines (write-outside-$WORK blocked,
//!      network denied) — these assertions are **capability-gated** and skip
//!      cleanly on hosts without Landlock/userns (e.g. stock WSL2).
//!
//! Run with:
//!   cargo test --test env_integration -- --nocapture

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

fn out_str(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Whether this host can actually *run* a process-tier confined command.
///
/// The capability bits (Landlock + user namespaces + seccomp) can all be
/// present while a hardened kernel still denies `exec` under the full
/// confinement stack — notably AppArmor-restricted unprivileged user
/// namespaces on Ubuntu 24.04 / GitHub-Actions runners. `env create
/// --isolation process` now functionally self-tests and fails closed there, so
/// a successful create is the authoritative signal that the kernel tests can
/// run. Cached across tests (the result is host-global).
fn process_tier_runnable() -> bool {
    use std::sync::OnceLock;
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        let r = Repo::new();
        let out = r.h5i(&["env", "create", "probe", "--isolation", "process"]);
        if !out.status.success() {
            eprintln!(
                "process-tier confinement not runnable on this host — kernel tests will skip:\n{}",
                out_str(&out)
            );
        }
        out.status.success()
    })
}

struct Repo {
    dir: PathBuf,
    _root: TempDir,
}

impl Repo {
    /// A fresh repo with one seed commit, `h5i init`-ed, git identity set.
    fn new() -> Repo {
        let root = TempDir::new().expect("tempdir");
        let dir = root.path().join("repo");
        run_ok(Command::new("git").args(["init", "-b", "main"]).arg(&dir));
        git(&dir, &["config", "user.name", "Env Tester"]);
        git(&dir, &["config", "user.email", "env@h5i.test"]);
        std::fs::write(dir.join("README.md"), "seed\n").unwrap();
        std::fs::write(dir.join("lib.py"), "def hello():\n    return 1\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-m", "seed"]);
        let r = Repo { dir, _root: root };
        r.h5i_ok(&["init"]);
        r
    }

    fn h5i(&self, args: &[&str]) -> Output {
        Command::new(H5I)
            .args(args)
            // Hermetic: a fixed identity, no ambient leakage.
            .env("H5I_AGENT", "tester")
            // Pin the default tier so bare `env create` is deterministic + fast
            // (no auto-pick probing / confined runs). Tests that exercise a tier
            // pass `--isolation` or declare it in env.toml; the auto-pick test
            // forces probing with `--isolation auto`.
            .env("H5I_DEFAULT_ISOLATION", "workspace")
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

    fn env_dir(&self, slug: &str) -> PathBuf {
        self.dir.join(".git/.h5i/env/tester").join(slug)
    }

    fn work(&self, slug: &str) -> PathBuf {
        self.env_dir(slug).join("work")
    }

    fn manifest(&self, slug: &str) -> serde_json::Value {
        let text =
            std::fs::read_to_string(self.env_dir(slug).join("manifest.json")).expect("manifest");
        serde_json::from_str(&text).expect("manifest json")
    }

    /// The **latest** capture manifest in refs/h5i/objects tagged for env
    /// `<slug>`. Manifests are appended chronologically, so the last matching
    /// line is the newest capture — important when an env has several runs.
    fn capture_manifest(&self, slug: &str) -> serde_json::Value {
        let blob = out_str(&git(&self.dir, &["show", "refs/h5i/objects:manifests.jsonl"]));
        let id = format!("env/tester/{slug}");
        let line = blob
            .lines()
            .rfind(|l| l.contains(&id))
            .expect("an env-tagged capture");
        serde_json::from_str(line).expect("capture manifest json")
    }

    /// The raw content-addressed blob bytes for a capture's `raw_oid`.
    fn capture_raw(&self, raw_oid: &str) -> Vec<u8> {
        let hex = raw_oid.strip_prefix("sha256:").unwrap_or(raw_oid);
        let path = self
            .dir
            .join(".git/.h5i/objects")
            .join(&hex[0..2])
            .join(&hex[2..4])
            .join(hex);
        std::fs::read(&path).unwrap_or_else(|_| panic!("raw blob {hex} missing"))
    }
}

// ─── 1. create: the triple fusion ───────────────────────────────────────────

#[test]
fn create_builds_worktree_branch_context_policy_and_event() {
    let r = Repo::new();
    // `h5i init` drops its own untracked scaffolding (CLAUDE.md, .claude/…) —
    // snapshot the status BEFORE create so we assert create adds nothing.
    let st_before = out_str(&git(&r.dir, &["status", "--porcelain"]));
    let out = out_str(&r.h5i_ok(&["env", "create", "fix-auth"]));
    assert!(out.contains("env/tester/fix-auth"), "{out}");

    // Workspace: a git worktree under .git/.h5i, invisible to the main tree.
    let work = r.work("fix-auth");
    assert!(work.join("README.md").is_file(), "worktree checked out");
    assert!(work.join(".git").is_file(), "worktree gitlink present");
    let st_after = out_str(&git(&r.dir, &["status", "--porcelain"]));
    assert_eq!(st_after, st_before, "env create must not touch the main working tree");

    // Code branch exists and points at the pinned base.
    let branch = out_str(&git(&r.dir, &["rev-parse", "refs/heads/h5i/env/tester/fix-auth"]));
    let head = out_str(&git(&r.dir, &["rev-parse", "HEAD"]));
    assert_eq!(branch.trim(), head.trim(), "env branch starts at the frozen base");

    // Manifest pins base/branch/context/policy.
    let m = r.manifest("fix-auth");
    assert_eq!(m["status"], "created");
    assert_eq!(m["agent"], "tester");
    assert_eq!(m["parent_branch"], "main");
    assert_eq!(m["base_commit"].as_str().unwrap(), head.trim());
    assert_eq!(m["branch"], "refs/heads/h5i/env/tester/fix-auth");
    assert_eq!(m["context_branch"], "env/tester/fix-auth");
    assert_eq!(m["backend"], "worktree");
    assert_eq!(m["isolation_claim"], "workspace");
    assert_eq!(m["policy_digest"].as_str().unwrap().len(), 64);
    assert!(r.env_dir("fix-auth").join("policy.resolved.toml").is_file());

    // Reasoning branch forked under refs/h5i/context/.
    run_ok(Command::new("git")
        .args(["rev-parse", "refs/h5i/context/env/tester/fix-auth"])
        .current_dir(&r.dir));

    // Event log: refs/h5i/env carries the created event.
    let log = out_str(&git(&r.dir, &["show", "refs/h5i/env:events.jsonl"]));
    assert!(log.contains("\"event\":\"created\""), "{log}");
    assert!(log.contains("env/tester/fix-auth"), "{log}");

    // Listed.
    let list = out_str(&r.h5i_ok(&["env", "list"]));
    assert!(list.contains("env/tester/fix-auth"), "{list}");
    assert!(list.contains("created"), "{list}");
}

#[test]
fn create_refuses_duplicates_and_bad_names() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "dup"]);
    let out = r.h5i(&["env", "create", "dup"]);
    assert!(!out.status.success(), "duplicate env must refuse");
    assert!(out_str(&out).contains("already exists"));

    for bad in ["Fix-Auth", "a/b", "-x", ".hidden"] {
        let out = r.h5i(&["env", "create", bad]);
        assert!(!out.status.success(), "slug '{bad}' must be rejected");
    }
}

#[test]
fn create_pins_an_explicit_base_revision() {
    let r = Repo::new();
    let first = out_str(&git(&r.dir, &["rev-parse", "HEAD"])).trim().to_string();
    std::fs::write(r.dir.join("later.txt"), "later\n").unwrap();
    git(&r.dir, &["add", "later.txt"]);
    git(&r.dir, &["commit", "-m", "later"]);

    r.h5i_ok(&["env", "create", "old-base", "--from", &first]);
    let m = r.manifest("old-base");
    assert_eq!(m["base_commit"].as_str().unwrap(), first);
    // The worktree reflects the OLD base — later.txt is absent.
    assert!(!r.work("old-base").join("later.txt").exists());
}

// ─── 2. run: capture-wrapped, evidence-tagged, exit-code transparent ────────

#[test]
fn run_captures_evidence_with_env_id_and_policy_digest() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "evidence"]);
    r.h5i_ok(&["env", "run", "evidence", "--", "sh", "-c", "echo out-line; echo err-line >&2"]);

    // The capture manifest in refs/h5i/objects carries the env tags.
    let manifests = out_str(&git(&r.dir, &["show", "refs/h5i/objects:manifests.jsonl"]));
    let line = manifests
        .lines()
        .find(|l| l.contains("env/tester/evidence"))
        .expect("an env-tagged capture");
    let m: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(m["env_id"], "env/tester/evidence");
    let env_manifest = r.manifest("evidence");
    assert_eq!(m["policy_digest"], env_manifest["policy_digest"]);
    assert_eq!(m["exit_code"], 0);
    // Captured against the env branch, not the parent.
    assert_eq!(m["branch"], "h5i/env/tester/evidence");

    // The env manifest references the capture; status advanced to idle.
    assert_eq!(env_manifest["status"], "idle");
    let caps = env_manifest["captures"].as_array().unwrap();
    assert_eq!(caps.len(), 1);
    assert_eq!(caps[0], m["id"]);

    // The exec event points at the same capture.
    let log = out_str(&r.h5i_ok(&["env", "log", "evidence"]));
    assert!(log.contains("exec"), "{log}");
    assert!(log.contains(m["id"].as_str().unwrap()), "{log}");
}

#[test]
fn run_passes_the_exit_code_through() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "fails"]);
    let out = r.h5i(&["env", "run", "fails", "--", "sh", "-c", "echo boom >&2; exit 7"]);
    assert_eq!(out.status.code(), Some(7), "exit code must pass through");
    // The failed run is still evidence.
    let m = r.manifest("fails");
    assert_eq!(m["captures"].as_array().unwrap().len(), 1);
}

#[test]
fn run_executes_inside_the_worktree() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "whereami"]);
    r.h5i_ok(&["env", "run", "whereami", "--", "sh", "-c", "echo probe > made-here.txt"]);
    assert!(r.work("whereami").join("made-here.txt").is_file());
    assert!(!r.dir.join("made-here.txt").exists(), "parent tree untouched");
}

// ─── 3. propose / apply: the only road into the parent branch ───────────────

#[test]
fn full_lifecycle_create_run_propose_apply() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "feature"]);
    r.h5i_ok(&["env", "run", "feature", "--", "sh", "-c",
        "printf 'def hello():\\n    return 2\\n' > lib.py && echo done"]);

    // Diff against the frozen base sees the change.
    let diff = out_str(&r.h5i_ok(&["env", "diff", "feature"]));
    assert!(diff.contains("return 2"), "{diff}");

    // Propose: mediated commit + review brief; parent branch untouched.
    let before = out_str(&git(&r.dir, &["rev-parse", "main"]));
    let brief = out_str(&r.h5i_ok(&["env", "propose", "feature"]));
    assert!(brief.contains("Proposal: env/tester/feature"), "{brief}");
    assert!(brief.contains("lib.py"), "diffstat in brief: {brief}");
    assert!(brief.contains("never automatic"), "{brief}");
    assert_eq!(
        out_str(&git(&r.dir, &["rev-parse", "main"])),
        before,
        "propose must NEVER write the parent branch"
    );
    assert_eq!(r.manifest("feature")["status"], "proposed");

    // Apply (fast-forward expected: parent didn't move).
    let out = out_str(&r.h5i_ok(&["env", "apply", "feature"]));
    assert!(out.contains("applied onto main"), "{out}");
    let lib = std::fs::read_to_string(r.dir.join("lib.py")).unwrap();
    assert!(lib.contains("return 2"), "apply must update the parent working tree");
    assert_eq!(r.manifest("feature")["status"], "applied");

    // Event log carries the whole lifecycle.
    let log = out_str(&r.h5i_ok(&["env", "log", "feature"]));
    for ev in ["created", "exec", "proposed", "applied"] {
        assert!(log.contains(ev), "missing event {ev}: {log}");
    }
}

#[test]
fn apply_refuses_without_propose() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "eager"]);
    let out = r.h5i(&["env", "apply", "eager"]);
    assert!(!out.status.success());
    assert!(out_str(&out).contains("propose"), "{}", out_str(&out));
}

#[test]
fn apply_merges_when_parent_advanced_and_refuses_conflicts() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "merge-me"]);
    // Env edits lib.py …
    std::fs::write(r.work("merge-me").join("env-file.txt"), "from env\n").unwrap();
    r.h5i_ok(&["env", "propose", "merge-me"]);
    // … while the parent advances independently (disjoint file).
    std::fs::write(r.dir.join("parent-file.txt"), "from parent\n").unwrap();
    git(&r.dir, &["add", "parent-file.txt"]);
    git(&r.dir, &["commit", "-m", "parent advance"]);

    let out = out_str(&r.h5i_ok(&["env", "apply", "merge-me"]));
    assert!(out.contains("applied onto main"), "{out}");
    assert!(r.dir.join("env-file.txt").is_file());
    assert!(r.dir.join("parent-file.txt").is_file());

    // Now a conflicting case: both sides touch the same line.
    r.h5i_ok(&["env", "create", "conflict"]);
    std::fs::write(r.work("conflict").join("README.md"), "env version\n").unwrap();
    r.h5i_ok(&["env", "propose", "conflict"]);
    std::fs::write(r.dir.join("README.md"), "parent version\n").unwrap();
    git(&r.dir, &["add", "README.md"]);
    git(&r.dir, &["commit", "-m", "parent readme"]);
    let out = r.h5i(&["env", "apply", "conflict"]);
    assert!(!out.status.success(), "conflicting apply must refuse");
    assert!(out_str(&out).contains("conflict"), "{}", out_str(&out));
}

#[test]
fn apply_requires_parent_branch_and_clean_tree() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "guard"]);
    std::fs::write(r.work("guard").join("x.txt"), "x\n").unwrap();
    r.h5i_ok(&["env", "propose", "guard"]);

    // Dirty tracked file → refuse.
    std::fs::write(r.dir.join("README.md"), "dirty\n").unwrap();
    let out = r.h5i(&["env", "apply", "guard"]);
    assert!(!out.status.success());
    assert!(out_str(&out).contains("uncommitted"), "{}", out_str(&out));
    git(&r.dir, &["checkout", "--", "README.md"]);

    // Wrong branch → refuse.
    git(&r.dir, &["checkout", "-b", "elsewhere"]);
    let out = r.h5i(&["env", "apply", "guard"]);
    assert!(!out.status.success());
    assert!(out_str(&out).contains("parent branch"), "{}", out_str(&out));
    git(&r.dir, &["checkout", "main"]);

    // Back on main and clean → applies.
    r.h5i_ok(&["env", "apply", "guard"]);
}

#[test]
fn mediated_commit_fails_closed_on_nested_git_repo() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "smuggle"]);
    // An agent (or its build) drops a nested git repo inside $WORK — staging
    // it would record a gitlink/submodule pointer. Must refuse, not skip.
    let nested = r.work("smuggle").join("vendor/dep");
    std::fs::create_dir_all(&nested).unwrap();
    run_ok(Command::new("git").args(["init"]).arg(&nested));
    std::fs::write(nested.join("f.txt"), "x\n").unwrap();

    let out = r.h5i(&["env", "propose", "smuggle"]);
    assert!(!out.status.success(), "nested .git must fail the mediated commit");
    let text = out_str(&out);
    assert!(text.contains("fail-closed") || text.contains(".git"), "{text}");
    // And the env did NOT advance to proposed.
    assert_eq!(r.manifest("smuggle")["status"], "created");

    // The boundary trip is recorded as a durable `violation` event (the
    // dashboard's highest-confidence sandbox-probe signal), not just a CLI error.
    let log = out_str(&r.h5i_ok(&["env", "log", "smuggle"]));
    assert!(
        log.contains("violation"),
        "boundary trip must be persisted as a violation event:\n{log}"
    );
}

// ─── 3b. secrets broker ─────────────────────────────────────────────────────

#[test]
fn secret_grant_is_injected_then_redacted_and_audited() {
    let r = Repo::new();
    // Declare a secret grant in the checked-in profile.
    std::fs::create_dir_all(r.dir.join(".h5i")).unwrap();
    std::fs::write(
        r.dir.join(".h5i/env.toml"),
        "[profile.default]\nsecrets = [\"MY_TOKEN\"]\n",
    )
    .unwrap();
    git(&r.dir, &["add", ".h5i/env.toml"]);
    git(&r.dir, &["commit", "-m", "secret profile"]);

    r.h5i_ok(&["env", "create", "needs-secret"]);

    // Run echoing the secret. The broker must inject MY_TOKEN from the host
    // source, and h5i must scrub the value out of the captured evidence.
    let out = Command::new(H5I)
        .args(["env", "run", "needs-secret", "--", "sh", "-c", "echo TOKEN=[$MY_TOKEN]"])
        .env("H5I_AGENT", "tester")
        .env("H5I_SECRET_MY_TOKEN", "supersecret-xyz")
        .current_dir(&r.dir)
        .output()
        .expect("run");
    assert!(out.status.success(), "run failed: {}", out_str(&out));

    // The injected value must NOT appear in the capture — but the surrounding
    // text must, proving the secret was actually injected (then redacted).
    let cap = r.capture_manifest("needs-secret");
    let summary = cap["summary"].as_str().unwrap_or("");
    assert!(
        !summary.contains("supersecret-xyz"),
        "secret value leaked into the capture summary:\n{summary}"
    );
    assert!(
        summary.contains("[redacted secret]"),
        "expected the injected secret to be redacted (proves it was injected):\n{summary}"
    );
    // And the raw blob is scrubbed too, not just the summary.
    let raw = String::from_utf8_lossy(&r.capture_raw(cap["raw_oid"].as_str().unwrap())).into_owned();
    assert!(!raw.contains("supersecret-xyz"), "secret leaked into the raw blob:\n{raw}");

    // A `secret` event records the grant id + fingerprint, never the value.
    let log = out_str(&r.h5i_ok(&["env", "log", "needs-secret"]));
    assert!(log.contains("secret") && log.contains("grant=MY_TOKEN"), "no secret audit event:\n{log}");
    assert!(!log.contains("supersecret-xyz"), "secret value leaked into the event log:\n{log}");
}

#[test]
fn secret_file_injection_writes_a_file_and_redacts() {
    let r = Repo::new();
    std::fs::create_dir_all(r.dir.join(".h5i")).unwrap();
    // inject=file is supported on the (default) workspace tier.
    std::fs::write(
        r.dir.join(".h5i/env.toml"),
        "[profile.default.secret.DEPLOY_KEY]\nsource = \"env:H5I_SECRET_DEPLOY_KEY\"\ninject = \"file\"\n",
    )
    .unwrap();
    git(&r.dir, &["add", ".h5i/env.toml"]);
    git(&r.dir, &["commit", "-m", "file secret"]);
    r.h5i_ok(&["env", "create", "filesec"]);

    // The broker sets DEPLOY_KEY_FILE → a path; the command reads it.
    let out = Command::new(H5I)
        .args(["env", "run", "filesec", "--", "sh", "-c", "echo KEY=[$(cat $DEPLOY_KEY_FILE)]"])
        .env("H5I_AGENT", "tester")
        .env("H5I_SECRET_DEPLOY_KEY", "topsecret-deploy")
        .current_dir(&r.dir)
        .output()
        .expect("run");
    assert!(out.status.success(), "run failed: {}", out_str(&out));

    // The file-injected value must be redacted from the capture (proves it was
    // delivered via the file and then scrubbed).
    let cap = r.capture_manifest("filesec");
    let summary = cap["summary"].as_str().unwrap_or("");
    assert!(!summary.contains("topsecret-deploy"), "secret leaked: {summary}");
    assert!(summary.contains("[redacted secret]"), "expected redaction marker: {summary}");

    // The audit event records the grant with inject=file, never the value.
    let log = out_str(&r.h5i_ok(&["env", "log", "filesec"]));
    assert!(log.contains("grant=DEPLOY_KEY") && log.contains("inject=file"), "{log}");
    assert!(!log.contains("topsecret-deploy"));
}

#[test]
fn secret_grant_missing_source_fails_closed() {
    let r = Repo::new();
    std::fs::create_dir_all(r.dir.join(".h5i")).unwrap();
    std::fs::write(
        r.dir.join(".h5i/env.toml"),
        "[profile.default]\nsecrets = [\"ABSENT_TOKEN\"]\n",
    )
    .unwrap();
    git(&r.dir, &["add", ".h5i/env.toml"]);
    git(&r.dir, &["commit", "-m", "secret profile"]);
    r.h5i_ok(&["env", "create", "no-source"]);

    // No host source for ABSENT_TOKEN → the run must refuse (fail-closed).
    let out = r.h5i(&["env", "run", "no-source", "--", "sh", "-c", "echo hi"]);
    assert!(!out.status.success(), "run must fail closed when a grant can't be resolved");
    assert!(out_str(&out).contains("fail-closed") || out_str(&out).contains("not set"));
    // The env did not get stuck in 'running'.
    assert_ne!(r.manifest("no-source")["status"], "running");
}

// ─── 3c. supervised tier (fail-closed) ──────────────────────────────────────

#[test]
fn supervised_claim_refuses_when_stack_incomplete() {
    let _serial = supervised_guard();
    let r = Repo::new();
    // On this host (and any without the full mediation stack) the supervised
    // claim must be REFUSED — never silently downgraded. An impossible claim.
    let out = r.h5i(&["env", "create", "untrusted", "--isolation", "supervised"]);
    if out.status.success() {
        // The only way this succeeds is if the host genuinely has the whole
        // stack green — then the manifest must honestly say 'supervised'.
        assert_eq!(r.manifest("untrusted")["isolation_claim"], "supervised");
    } else {
        let text = out_str(&out);
        assert!(
            text.contains("supervised") && (text.contains("refus") || text.contains("cannot be satisfied")),
            "supervised must fail closed with an explanation, got:\n{text}"
        );
    }
}

/// Set up a repo with a `supervised` profile (plus optional extra profile TOML)
/// and create env `slug`. Returns `None` — so the caller skips cleanly — when
/// the host can't satisfy the supervised claim (e.g. CI without cgroup
/// delegation), exactly like the container tests gate on rootless Podman.
/// Serializes the heavy supervised e2e tests. Each spawns confined children
/// (userns+netns+seccomp+notify), and several running at once under cargo's
/// parallel harness exhaust the host's fork capacity, making unrelated `git`
/// subprocesses flake with EAGAIN. Holding this for the test's duration caps
/// peak fork pressure without serializing the whole suite. Poison-tolerant so a
/// failing test surfaces its real assertion, not a poison panic.
static SUPERVISED_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn supervised_guard() -> std::sync::MutexGuard<'static, ()> {
    SUPERVISED_SERIAL.lock().unwrap_or_else(|p| p.into_inner())
}

fn supervised_env(slug: &str, extra_toml: &str) -> Option<Repo> {
    let r = Repo::new();
    std::fs::create_dir_all(r.dir.join(".h5i")).unwrap();
    std::fs::write(
        r.dir.join(".h5i/env.toml"),
        format!("[profile.default]\nisolation = \"supervised\"\n{extra_toml}"),
    )
    .unwrap();
    git(&r.dir, &["add", ".h5i/env.toml"]);
    git(&r.dir, &["commit", "-m", "supervised profile"]);
    if r.h5i(&["env", "create", slug]).status.success() {
        Some(r)
    } else {
        eprintln!("skipping: supervised tier not satisfiable on this host");
        None
    }
}

fn have_python3() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run `argv` in supervised env `slug` and return the captured raw evidence
/// (stdout+stderr), or `None` if the run couldn't start (skip).
fn supervised_run_raw(r: &Repo, slug: &str, argv: &[&str]) -> Option<String> {
    let mut full = vec!["env", "run", slug, "--"];
    full.extend_from_slice(argv);
    // Run synchronously. A non-zero exit (OOM-killed, denied write, …) still
    // produces a capture — what we read below; only a setup failure has none.
    let _out = Command::new(H5I)
        .args(&full)
        .env("H5I_AGENT", "tester")
        .current_dir(&r.dir)
        .output()
        .expect("run");
    let cap = r.capture_manifest(slug);
    Some(String::from_utf8_lossy(&r.capture_raw(cap["raw_oid"].as_str()?)).into_owned())
}

/// Comprehensive live proof of the supervised tier's runtime enforcement, in a
/// SINGLE env with a few **sequential** runs (deliberately not one test per
/// property — many parallel supervised runs forking confined children exhaust
/// the host's fork capacity and flake unrelated git steps). Covers the
/// seccomp-notify socket gate, the airtight netns, the Landlock FS allowlist,
/// the seccomp deny-list, and the gate-verdict recording. Capability-gated.
#[test]
fn supervised_enforces_runtime_confinement() {
    let _serial = supervised_guard();
    if !have_python3() {
        eprintln!("skipping: python3 unavailable");
        return;
    }
    let Some(r) = supervised_env("confine", "") else { return };

    // Run 1 (python): the socket gate + airtight network, in one process.
    let net_script = "import socket,errno\n\
        def t(n,a):\n\
        \x20try:\n\
        \x20\x20s=socket.socket(*a);s.close();print(n,'ALLOWED')\n\
        \x20except OSError as e:\n\
        \x20\x20print(n,'DENIED',errno.errorcode.get(e.errno,e.errno))\n\
        t('RAW',(socket.AF_INET,socket.SOCK_RAW,socket.IPPROTO_TCP))\n\
        t('PACKET',(17,socket.SOCK_DGRAM,0))\n\
        t('UNIX',(socket.AF_UNIX,socket.SOCK_STREAM))\n\
        t('INET',(socket.AF_INET,socket.SOCK_STREAM,0))\n\
        c=socket.socket(); c.settimeout(3)\n\
        try:\n\
        \x20c.connect(('1.1.1.1',80)); print('CONNECTED')\n\
        except OSError: print('NOCONNECT')\n";
    let net = supervised_run_raw(&r, "confine", &["python3", "-c", net_script]).expect("run 1");
    // Default-deny socket gate: only boring inet is allowed.
    assert!(net.contains("RAW DENIED EPERM"), "raw socket denied:\n{net}");
    assert!(net.contains("PACKET DENIED EPERM"), "packet socket denied:\n{net}");
    assert!(net.contains("UNIX DENIED EPERM"), "ungranted AF_UNIX denied:\n{net}");
    assert!(net.contains("INET ALLOWED"), "ordinary inet socket allowed:\n{net}");
    // Airtight netns under net.mode=deny: no route to any external host.
    assert!(net.contains("NOCONNECT") && !net.contains("CONNECTED"), "netns must have no egress:\n{net}");

    // The socket-gate verdicts are recorded in the run's capture EgressSummary.
    let cap = r.capture_manifest("confine");
    let eg = &cap["egress"];
    assert!(eg.is_object(), "supervised capture must carry an egress summary: {cap}");
    assert!(eg["denied"].as_u64().unwrap_or(0) >= 1, "denials counted: {eg}");
    assert!(eg["allowed"].as_u64().unwrap_or(0) >= 1, "allows counted: {eg}");

    // Run 2 (sh): Landlock FS allowlist + seccomp deny-list (unshare).
    let fs_script = "echo in > inside.txt && echo WORK_OK; \
        echo x > /etc/h5i-escape 2>/dev/null && echo ETC_WROTE || echo ETC_DENIED; \
        unshare --mount /bin/true 2>&1; echo unshare_rc=$?";
    let fs = supervised_run_raw(&r, "confine", &["sh", "-c", fs_script]).expect("run 2");
    assert!(fs.contains("WORK_OK"), "writing $WORK succeeds:\n{fs}");
    assert!(fs.contains("ETC_DENIED") && !fs.contains("ETC_WROTE"), "Landlock denies writes outside $WORK:\n{fs}");
    assert!(
        fs.contains("Operation not permitted") || fs.contains("unshare_rc=1"),
        "seccomp deny-list blocks unshare:\n{fs}"
    );
}

/// A memory limit is enforced for a supervised run: a large allocation under a
/// tight cap does not complete (cgroup memory.max / RLIMIT_AS). Separate env
/// because it needs a `resources.mem` profile.
#[test]
fn supervised_memory_limit_is_enforced() {
    let _serial = supervised_guard();
    if !have_python3() {
        eprintln!("skipping: python3 unavailable");
        return;
    }
    let Some(r) = supervised_env("membox", "[profile.default.resources]\nmem = \"64m\"\n") else {
        return;
    };
    let script = "x=bytearray(400*1024*1024)\n\
        for i in range(0,len(x),4096): x[i]=1\n\
        print('ALLOCATED')\n";
    let raw = supervised_run_raw(&r, "membox", &["python3", "-c", script]).expect("run");
    assert!(!raw.contains("ALLOCATED"), "a 400MiB alloc under a 64MiB cap must not complete:\n{raw}");
}

fn have_bin(name: &str) -> bool {
    std::process::Command::new(name).arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

/// Supervised increment 2: a `net.egress` allowlist confines the netns to
/// exactly the pinned hosts — slirp4netns provides the uplink, an nftables
/// default-drop ruleset is the airtight L3/L4 guard, and DNS is pinned via a
/// private `/etc/hosts` (no port 53 at all). So an allowlisted host resolves to
/// the pinned IP and connects, while everything else fails closed. Needs real
/// outbound network, so it is **opt-in** via `H5I_TEST_NET=1` (mirrors the
/// container tests' `H5I_TEST_CONTAINER`), and capability-gated on the
/// supervised stack + slirp4netns.
#[test]
fn supervised_egress_allowlist_confines_to_pinned_hosts() {
    let _serial = supervised_guard();
    if std::env::var("H5I_TEST_NET").is_err() {
        eprintln!("skipping supervised egress e2e: set H5I_TEST_NET=1 (needs outbound network)");
        return;
    }
    if !have_python3() || !have_bin("slirp4netns") {
        eprintln!("skipping: python3/slirp4netns unavailable");
        return;
    }
    let Some(r) = supervised_env("egbox", "net.egress = [\"example.com\"]\n") else { return };

    // example.com is allowlisted → pinned in /etc/hosts → connects.
    // cloudflare is NOT allowlisted → no /etc/hosts entry, no DNS → fails closed.
    let script = "import socket\n\
        def t(h):\n\
        \x20try:\n\
        \x20\x20s=socket.create_connection((h,443),timeout=8); s.close(); print(h,'CONNECTED')\n\
        \x20except Exception as e:\n\
        \x20\x20print(h,'BLOCKED',type(e).__name__)\n\
        t('example.com')\n\
        t('www.cloudflare.com')\n";
    let raw = supervised_run_raw(&r, "egbox", &["python3", "-c", script]).expect("egress run");
    assert!(raw.contains("example.com CONNECTED"), "allowlisted host must connect:\n{raw}");
    assert!(
        raw.contains("www.cloudflare.com BLOCKED") && !raw.contains("www.cloudflare.com CONNECTED"),
        "a non-allowlisted host must be blocked (fail-closed):\n{raw}"
    );
}

// ─── 4. parallel envs (the arena) ───────────────────────────────────────────

#[test]
fn two_envs_from_one_frozen_base_coexist_and_both_apply() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "alpha"]);
    r.h5i_ok(&["env", "create", "beta"]);

    std::fs::write(r.work("alpha").join("alpha.txt"), "a\n").unwrap();
    std::fs::write(r.work("beta").join("beta.txt"), "b\n").unwrap();
    r.h5i_ok(&["env", "propose", "alpha"]);
    r.h5i_ok(&["env", "propose", "beta"]);

    r.h5i_ok(&["env", "apply", "alpha"]);
    // beta still applies after main moved (clean 3-way merge).
    r.h5i_ok(&["env", "apply", "beta"]);
    assert!(r.dir.join("alpha.txt").is_file());
    assert!(r.dir.join("beta.txt").is_file());
}

// ─── 5. abort / gc ──────────────────────────────────────────────────────────

#[test]
fn abort_preserves_forensics_and_gc_reclaims_workspace() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "doomed"]);
    r.h5i_ok(&["env", "run", "doomed", "--", "sh", "-c", "echo evidence"]);
    r.h5i_ok(&["env", "abort", "doomed"]);
    assert_eq!(r.manifest("doomed")["status"], "aborted");

    // gc reclaims the worktree but keeps manifest + branch + captures.
    let out = out_str(&r.h5i_ok(&["env", "gc"]));
    assert!(out.contains("doomed"), "{out}");
    assert!(!r.work("doomed").exists(), "workspace reclaimed");
    assert!(r.env_dir("doomed").join("manifest.json").is_file(), "manifest retained");
    run_ok(Command::new("git")
        .args(["rev-parse", "refs/heads/h5i/env/tester/doomed"])
        .current_dir(&r.dir));
    // A live env is NOT gc'd.
    r.h5i_ok(&["env", "create", "alive"]);
    r.h5i_ok(&["env", "gc"]);
    assert!(r.work("alive").exists(), "live env untouched by gc");

    // Run after gc refuses cleanly.
    let out = r.h5i(&["env", "run", "doomed", "--", "true"]);
    assert!(!out.status.success());
}

// ─── 6. isolation claims fail closed ────────────────────────────────────────

/// Secure-by-default: `--isolation auto` (which force-probes, ignoring the
/// test's `H5I_DEFAULT_ISOLATION=workspace` pin) selects the *strongest* tier
/// this host can actually run — and the invariant is that the picked tier then
/// runs a command cleanly (auto never lands on an unrunnable tier). Serialized
/// with the other confined-fork tests since auto may pick supervised/process.
#[test]
fn auto_isolation_picks_a_runnable_tier() {
    let _serial = supervised_guard();
    let r = Repo::new();
    let out = r.h5i(&["env", "create", "autobox", "--isolation", "auto"]);
    assert!(out.status.success(), "auto create must succeed:\n{}", out_str(&out));

    let picked = r.manifest("autobox")["isolation_claim"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        ["workspace", "process", "supervised", "container"].contains(&picked.as_str()),
        "auto picked a real tier, got '{picked}'"
    );

    // The keystone invariant: whatever was picked must actually run.
    let run = r.h5i(&["env", "run", "autobox", "--", "sh", "-c", "exit 0"]);
    assert!(
        run.status.success(),
        "auto-picked tier '{picked}' failed to run a command:\n{}",
        out_str(&run)
    );
}

#[test]
fn unimplemented_backends_refuse_at_create() {
    let r = Repo::new();
    // hardened-container/microvm have no adapter in this build → refuse.
    for claim in ["hardened-container", "microvm"] {
        let out = r.h5i(&["env", "create", "boxed", "--isolation", claim]);
        assert!(!out.status.success(), "claim {claim} must refuse");
        assert!(out_str(&out).contains("backend"), "{}", out_str(&out));
        assert!(!r.env_dir("boxed").exists(), "no state left behind on refusal");
    }
    // An unknown claim name is rejected outright.
    let out = r.h5i(&["env", "create", "boxed", "--isolation", "docker"]);
    assert!(!out.status.success(), "unknown claim must refuse");
}

#[test]
fn process_claim_is_all_or_nothing_per_host() {
    let r = Repo::new();
    let out = r.h5i(&["env", "create", "confined", "--isolation", "process"]);
    if process_tier_runnable() {
        assert!(out.status.success(), "{}", out_str(&out));
        assert_eq!(r.manifest("confined")["isolation_claim"], "process");
    } else {
        // Fail closed: refuse with an explicit reason, never downgrade — whether
        // the bits are missing or the confinement simply can't exec on this host.
        assert!(!out.status.success(), "must refuse when process tier is not runnable");
        let text = out_str(&out);
        assert!(
            text.contains("cannot be satisfied") || text.contains("not functional"),
            "{text}"
        );
        assert!(!r.env_dir("confined").exists());
    }
}

// ─── 7. the kernel sandbox actually confines (capability-gated) ─────────────

/// Write outside $WORK is blocked by Landlock; write inside works; network is
/// unreachable under net.mode=deny. Skips (with a notice) when the host can't
/// satisfy the process claim — the fail-closed path is covered above.
#[test]
fn process_tier_confines_fs_and_network() {
    if !process_tier_runnable() {
        eprintln!("SKIP process_tier_confines_fs_and_network: process tier not runnable on this host");
        return;
    }
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "jail", "--isolation", "process"]);

    // Inside $WORK: writable.
    r.h5i_ok(&["env", "run", "jail", "--", "sh", "-c", "echo ok > inside.txt"]);
    assert!(r.work("jail").join("inside.txt").is_file());

    // Outside $WORK (the parent repo!): must be blocked.
    let escape = r.dir.join("escaped.txt");
    let out = r.h5i(&[
        "env", "run", "jail", "--", "sh", "-c",
        &format!("echo pwned > {}", escape.display()),
    ]);
    assert!(!out.status.success(), "write outside $WORK must fail");
    assert!(!escape.exists(), "no file may appear outside $WORK");

    // The shared .git must be unreachable through the worktree gitlink.
    let out = r.h5i(&["env", "run", "jail", "--", "sh", "-c",
        "git -C . rev-parse HEAD 2>/dev/null || echo GIT-BLOCKED"]);
    let text = out_str(&out);
    assert!(text.contains("GIT-BLOCKED"), "shared .git must be hidden: {text}");

    // Network: deny means even loopback TCP fails. Use a pure-shell probe.
    let out = r.h5i(&["env", "run", "jail", "--", "sh", "-c",
        "(exec 3<>/dev/tcp/127.0.0.1/22) 2>/dev/null && echo NET-OPEN || echo NET-CLOSED"]);
    let text = out_str(&out);
    // bash-only /dev/tcp; dash prints an error and exits non-zero → also CLOSED-ish.
    assert!(!text.contains("NET-OPEN"), "network must be denied: {text}");

    // Dangerous syscalls are denied (unshare → EPERM).
    let out = r.h5i(&["env", "run", "jail", "--", "sh", "-c",
        "unshare -U true 2>/dev/null && echo UNSHARE-OK || echo UNSHARE-BLOCKED"]);
    let text = out_str(&out);
    assert!(!text.contains("UNSHARE-OK"), "unshare must be denied: {text}");
}

/// The wall-clock kill must reap the WHOLE process tree (process-group kill),
/// not just the direct child — a runaway backgrounded descendant must die too.
/// Runs at the workspace tier so it needs no kernel capabilities.
#[test]
fn wall_clock_kill_reaps_descendant_processes() {
    let r = Repo::new();
    std::fs::create_dir_all(r.dir.join(".h5i")).unwrap();
    std::fs::write(
        r.dir.join(".h5i/env.toml"),
        "[profile.default]\nisolation = \"workspace\"\nresources = { wall = \"1s\" }\n",
    )
    .unwrap();
    r.h5i_ok(&["env", "create", "reap"]);

    // Background a grandchild that writes a marker 8s in, while the foreground
    // blocks for 60s. The 1s wall-clock fires long before 8s — even if the
    // poller slips by several seconds under parallel test load — and a correct
    // group-kill takes the grandchild with it, so the marker never appears.
    let t0 = std::time::Instant::now();
    let out = r.h5i(&[
        "env", "run", "reap", "--", "sh", "-c",
        "sh -c 'sleep 8; echo alive > survivor.txt' & echo started; sleep 60",
    ]);
    assert!(!out.status.success(), "timed-out run should not report success");

    // Wait until we are safely past the grandchild's 8s write point, then the
    // marker must still be absent (it was group-killed at ~1s).
    while t0.elapsed() < std::time::Duration::from_secs(11) {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert!(
        !r.work("reap").join("survivor.txt").exists(),
        "a backgrounded descendant survived the wall-clock kill (no process-group kill)"
    );
}

/// `env shell` (the agent-in-box) runs an interactive, stdio-inherited session
/// inside the env: a command after `--` executes in `$WORK`, its exit code
/// passes through transparently, and the env returns to `idle` with a `shell`
/// event logged (nothing is captured — it's interactive). Workspace tier so it
/// needs no kernel capabilities.
#[test]
fn env_shell_runs_in_box_and_passes_exit_code() {
    let r = Repo::new();
    std::fs::create_dir_all(r.dir.join(".h5i")).unwrap();
    std::fs::write(
        r.dir.join(".h5i/env.toml"),
        "[profile.default]\nisolation = \"workspace\"\n",
    )
    .unwrap();
    r.h5i_ok(&["env", "create", "box"]);

    // A command after `--` runs (non-interactively) inside the box, in $WORK.
    let out = r.h5i(&["env", "shell", "box", "--", "sh", "-c", "echo hi > from-shell.txt"]);
    assert!(out.status.success(), "shell command should succeed:\n{}", out_str(&out));
    assert!(
        r.work("box").join("from-shell.txt").is_file(),
        "the shell session ran in $WORK"
    );

    // The child's exit code passes through transparently (transparent wrapper).
    let bad = r.h5i(&["env", "shell", "box", "--", "sh", "-c", "exit 7"]);
    assert_eq!(bad.status.code(), Some(7), "shell must pass the child exit code through");

    // No capture is produced (interactive), but the env is back to idle.
    assert_eq!(r.manifest("box")["status"], "idle", "env returns to idle after a shell");
}

/// `isolation=process` with `net.mode=host` must STILL confine the filesystem
/// (Landlock applies without a network namespace) — proving the always-create
/// user namespace works when egress is allowed. Capability-gated.
#[test]
fn process_tier_host_net_still_confines_fs() {
    if !process_tier_runnable() {
        eprintln!("SKIP process_tier_host_net_still_confines_fs: process tier not runnable on this host");
        return;
    }
    let r = Repo::new();
    std::fs::create_dir_all(r.dir.join(".h5i")).unwrap();
    std::fs::write(
        r.dir.join(".h5i/env.toml"),
        "[profile.default]\nisolation = \"process\"\nnet.mode = \"host\"\n",
    )
    .unwrap();
    r.h5i_ok(&["env", "create", "hostnet"]);

    // Inside $WORK still writable …
    r.h5i_ok(&["env", "run", "hostnet", "--", "sh", "-c", "echo ok > in.txt"]);
    assert!(r.work("hostnet").join("in.txt").is_file());
    // … outside $WORK still blocked.
    let escape = r.dir.join("hostnet-escape.txt");
    let out = r.h5i(&[
        "env", "run", "hostnet", "--", "sh", "-c",
        &format!("echo x > {}", escape.display()),
    ]);
    assert!(!out.status.success());
    assert!(!escape.exists(), "host-net env must still confine the filesystem");
}

/// Env-var allowlist: only `env.pass` variables reach the confined process.
#[test]
fn process_tier_env_allowlist() {
    if !process_tier_runnable() {
        eprintln!("SKIP process_tier_env_allowlist: process tier not runnable on this host");
        return;
    }
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "envjail", "--isolation", "process"]);
    let out = Command::new(H5I)
        .args(["env", "run", "envjail", "--", "sh", "-c", "echo SECRET=[$MY_SECRET] PATH_SET=${PATH:+yes}"])
        .env("H5I_AGENT", "tester")
        .env("MY_SECRET", "hunter2")
        .current_dir(&r.dir)
        .output()
        .unwrap();
    let text = out_str(&out);
    assert!(text.contains("SECRET=[]"), "secrets must not be inherited: {text}");
    assert!(text.contains("PATH_SET=yes"), "allowlisted PATH must pass: {text}");
}

/// `resources.fsize` caps any single file the confined command writes — a
/// disk-bomb backstop (RLIMIT_FSIZE → SIGXFSZ). Capability-gated.
#[test]
fn process_tier_fsize_caps_disk_bomb() {
    if !process_tier_runnable() {
        eprintln!("SKIP process_tier_fsize_caps_disk_bomb: process tier not runnable on this host");
        return;
    }
    let r = Repo::new();
    std::fs::create_dir_all(r.dir.join(".h5i")).unwrap();
    std::fs::write(
        r.dir.join(".h5i/env.toml"),
        "[profile.default]\nisolation = \"process\"\nresources = { fsize = \"1M\" }\n",
    )
    .unwrap();
    r.h5i_ok(&["env", "create", "bomb"]);

    // Try to write 8 MiB into a single file; the 1 MiB RLIMIT_FSIZE kills it.
    let out = r.h5i(&[
        "env", "run", "bomb", "--", "sh", "-c",
        "head -c 8388608 /dev/zero > big.bin",
    ]);
    assert!(!out.status.success(), "writing past the fsize cap must fail");
    let big = r.work("bomb").join("big.bin");
    if big.exists() {
        let sz = std::fs::metadata(&big).unwrap().len();
        assert!(sz <= 2 * 1024 * 1024, "file should be capped near 1 MiB, got {sz} bytes");
    }
}

/// The PID-namespace jail (design §5 "PID view"): a confined process must not be
/// able to see — or read the `/proc/<pid>/environ` of — host processes. Without
/// it, a build script at the `process` tier could dump the operator's whole
/// environment (every host secret) straight out of `/proc`, defeating the
/// `env.pass` allowlist. Capability-gated.
#[test]
fn process_tier_pid_namespace_hides_host_processes_and_environ() {
    if !process_tier_runnable() {
        eprintln!("SKIP process_tier_pid_namespace...: process tier not runnable on this host");
        return;
    }
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "pidjail", "--isolation", "process"]);

    // A long-lived host process holding a secret in its environment.
    let secret = "h5i-leak-canary-9c3f1a2b";
    let mut victim = Command::new("sleep")
        .arg("120")
        .env("H5I_LEAK_CANARY", secret)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn victim host process");
    let vpid = victim.id();

    // Control: on the host, the same uid can usually read the victim's environ —
    // proving the secret is genuinely exposed there. Retry briefly: the new env
    // only lands after the child's execve completes. (Some hosts set
    // yama ptrace_scope=2 and forbid it even same-uid; we don't require it — the
    // namespace assertions below stand on their own.)
    let mut host_can_read = false;
    for _ in 0..50 {
        let e = std::fs::read(format!("/proc/{vpid}/environ")).unwrap_or_default();
        if String::from_utf8_lossy(&e).contains(secret) {
            host_can_read = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    // Inside the box: the victim's PID does not exist in the new namespace, so its
    // /proc entry — and the secret — is unreachable.
    let out = r.h5i(&[
        "env", "run", "pidjail", "--", "sh", "-c",
        &format!("cat /proc/{vpid}/environ 2>&1 | tr '\\0' '\\n'; echo DONE"),
    ]);
    let leaked = out_str(&out);

    // The workload is PID 1 of its own namespace ($$ == 1 proves the fresh pidns).
    let pid_out = r.h5i(&["env", "run", "pidjail", "--", "sh", "-c", "echo $$"]);
    let pid_txt = out_str(&pid_out);

    // The box sees only its own namespace's handful of pids, not the host's many.
    let count_out = r.h5i(&[
        "env", "run", "pidjail", "--", "sh", "-c",
        "ls -1 /proc | grep -E '^[0-9]+$' | wc -l",
    ]);
    // h5i appends an evidence summary line, so pick the bare-integer line the
    // command actually printed (not the "◈ evidence …" line).
    let visible: usize = out_str(&count_out)
        .lines()
        .filter_map(|l| {
            let t = l.trim();
            (!t.is_empty() && t.bytes().all(|b| b.is_ascii_digit())).then(|| t.parse().ok())?
        })
        .next()
        .unwrap_or(9999);

    let _ = victim.kill();
    let _ = victim.wait();

    if host_can_read {
        eprintln!("control OK: same-uid host read of the victim environ exposed the secret");
    } else {
        eprintln!("note: host won't expose the victim environ (ptrace_scope?); namespace checks still apply");
    }
    // The core security property: regardless of host policy, a confined process
    // must not see a host process's environ (its pid isn't even in the namespace).
    assert!(
        !leaked.contains(secret),
        "confined process read a HOST process's /proc/environ — PID-namespace leak:\n{leaked}"
    );
    assert!(
        pid_txt.lines().any(|l| l.trim() == "1"),
        "the workload must be PID 1 of a fresh namespace, got: {pid_txt}"
    );
    assert!(
        visible < 20,
        "the box must see only its own namespace's pids (saw {visible}); a host view shows far more"
    );
}

/// The PID-namespace jail mounts a *fresh* procfs, which shadows the host `/proc`
/// the pre-fork Landlock grant pinned. This proves the in-child re-grant works:
/// the workload can still read its own `/proc/self/*` (otherwise every confined
/// command that touches /proc would break). Capability-gated.
#[test]
fn process_tier_proc_self_is_readable_under_pid_namespace() {
    if !process_tier_runnable() {
        eprintln!("SKIP process_tier_proc_self...: process tier not runnable on this host");
        return;
    }
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "procok", "--isolation", "process"]);
    // No redirection to /dev/null (the default policy grants it read-only); read
    // /proc/self directly and gate the marker on a successful read.
    let out = r.h5i(&[
        "env", "run", "procok", "--", "sh", "-c",
        "head -1 /proc/self/status | grep -q '^Name:' && grep -q '^Pid:' /proc/self/status && echo PROC-OK",
    ]);
    let text = out_str(&out);
    assert!(
        text.contains("PROC-OK"),
        "the workload must still read its own /proc on the freshly-mounted procfs: {text}"
    );
}

// ─── 7b. container backend (rootless podman; design phase 4) ────────────────

/// Whether to run the real-container tests. They are **opt-in** via
/// `H5I_TEST_CONTAINER=1`: they pull an image and (for egress) make a live
/// network call, so we never run them implicitly in CI — where podman may be
/// present but the network/image pull would be a flakiness and surprise-egress
/// risk. Locally: `H5I_TEST_CONTAINER=1 cargo test`. When opted in, this still
/// functionally verifies rootless podman actually runs (skips if it can't).
/// The container backend's security-critical *logic* is covered by the
/// podman-free unit tests in `src/container.rs`.
fn container_runnable() -> bool {
    use std::sync::OnceLock;
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        if std::env::var("H5I_TEST_CONTAINER").as_deref() != Ok("1") {
            return false;
        }
        Command::new("podman")
            .args(["run", "--rm", "docker.io/library/busybox:latest", "true"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

const BUSYBOX: &str = "docker.io/library/busybox:latest";

fn write_profile(r: &Repo, toml: &str) {
    std::fs::create_dir_all(r.dir.join(".h5i")).unwrap();
    std::fs::write(r.dir.join(".h5i/env.toml"), toml).unwrap();
}

#[test]
fn container_create_fails_closed_without_image() {
    let r = Repo::new();
    // A container profile with no image is refused at create (fail closed),
    // whether or not a runtime is present.
    write_profile(&r, "[profile.default]\nisolation = \"container\"\n");
    let out = r.h5i(&["env", "create", "noimg"]);
    assert!(!out.status.success());
    assert!(out_str(&out).contains("image"), "{}", out_str(&out));
    assert!(!r.env_dir("noimg").exists());
}

#[test]
fn net_egress_under_process_fails_closed() {
    let r = Repo::new();
    write_profile(
        &r,
        "[profile.default]\nisolation = \"process\"\nnet.egress = [\"pypi.org\"]\n",
    );
    let out = r.h5i(&["env", "create", "egr"]);
    assert!(!out.status.success(), "egress under process must refuse");
    assert!(out_str(&out).contains("net.egress"), "{}", out_str(&out));
}

#[test]
fn container_runs_with_workspace_mount_and_net_deny() {
    if !container_runnable() {
        eprintln!("SKIP container_runs_with_workspace_mount_and_net_deny: no rootless podman");
        return;
    }
    let r = Repo::new();
    write_profile(
        &r,
        &format!(
            "[profile.default]\nisolation = \"container\"\nnet.mode = \"deny\"\ncontainer.image = \"{BUSYBOX}\"\n"
        ),
    );
    r.h5i_ok(&["env", "create", "box"]);

    // The command runs in the container, /work is the worktree (writable).
    r.h5i_ok(&["env", "run", "box", "--", "sh", "-c", "echo from-container > made.txt"]);
    let made = r.work("box").join("made.txt");
    assert!(made.is_file(), "container wrote into the mounted workspace");
    assert_eq!(std::fs::read_to_string(&made).unwrap().trim(), "from-container");

    // net.mode=deny → no egress.
    let out = r.h5i(&[
        "env", "run", "box", "--", "sh", "-c",
        "wget -T3 -q -O- http://example.com >/dev/null 2>&1 && echo REACHED || echo BLOCKED",
    ]);
    assert!(out_str(&out).contains("BLOCKED"), "net deny must block egress: {}", out_str(&out));

    // The capture records the container claim in the manifest.
    assert_eq!(r.manifest("box")["isolation_claim"], "container");
}

#[test]
fn container_egress_allowlist_permits_only_listed_hosts() {
    if !container_runnable() {
        eprintln!("SKIP container_egress_allowlist_permits_only_listed_hosts: no rootless podman");
        return;
    }
    let r = Repo::new();
    write_profile(
        &r,
        &format!(
            "[profile.default]\nisolation = \"container\"\nnet.egress = [\"example.com:80\"]\ncontainer.image = \"{BUSYBOX}\"\n"
        ),
    );
    r.h5i_ok(&["env", "create", "egr"]);

    // Allowlisted host is reachable through the DNS-pinned proxy.
    let allowed = r.h5i(&[
        "env", "run", "egr", "--", "sh", "-c",
        "wget -T8 -q -O- http://example.com | grep -qi 'example domain' && echo OK || echo FAIL",
    ]);
    assert!(out_str(&allowed).contains("OK"), "allowlisted host must be reachable: {}", out_str(&allowed));

    // A non-allowlisted host is blocked (fail-closed at the proxy).
    let denied = r.h5i(&[
        "env", "run", "egr", "--", "sh", "-c",
        "wget -T8 -q -O- http://www.google.com >/dev/null 2>&1 && echo REACHED || echo BLOCKED",
    ]);
    assert!(out_str(&denied).contains("BLOCKED"), "non-allowlisted host must be blocked: {}", out_str(&denied));
}

// ─── 8. secret redaction in evidence (design §7) ────────────────────────────

const PLANTED_SECRET: &str = "ghp_0123456789012345678901234567890123ab";

#[test]
fn run_redacts_secrets_from_evidence_blob_summary_and_command() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "leaky"]);
    // The secret appears both in the OUTPUT and in the command line itself.
    r.h5i_ok(&[
        "env", "run", "leaky", "--", "sh", "-c",
        &format!("echo token={PLANTED_SECRET}"),
    ]);

    let m = r.capture_manifest("leaky");
    // The detected rule is recorded (by id, never the value).
    let redactions = m["redactions"].as_array().expect("redactions array");
    assert!(
        redactions.iter().any(|v| v == "GITHUB_PAT"),
        "expected GITHUB_PAT in redactions: {m}"
    );
    // The secret must not survive ANYWHERE in the manifest line …
    let manifest_line = serde_json::to_string(&m).unwrap();
    assert!(!manifest_line.contains(PLANTED_SECRET), "secret leaked into manifest: {manifest_line}");
    // … including the command field (it was passed as an argument).
    assert!(!m["cmd"].as_str().unwrap().contains(PLANTED_SECRET), "secret leaked into cmd");

    // … and not in the content-addressed raw blob (which travels via push).
    let raw = r.capture_raw(m["raw_oid"].as_str().unwrap());
    let raw_str = String::from_utf8_lossy(&raw);
    assert!(!raw_str.contains(PLANTED_SECRET), "secret leaked into raw blob: {raw_str}");
    assert!(raw_str.contains("redacted"), "redaction marker expected in raw: {raw_str}");
}

#[test]
fn inspect_renders_a_capture_and_refuses_foreign_ones() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "one"]);
    r.h5i_ok(&["env", "create", "two"]);
    r.h5i_ok(&["env", "run", "one", "--", "sh", "-c", "echo hello-from-one"]);
    let cap = r.manifest("one")["captures"][0].as_str().unwrap().to_string();

    // Inspect from the owning env: renders the capture.
    let out = out_str(&r.h5i_ok(&["env", "inspect", "one", "--capture", &cap]));
    assert!(out.contains(&cap), "{out}");
    assert!(out.contains("exit"), "{out}");

    // Inspecting the SAME capture id from a different env is refused — evidence
    // is scoped to its environment.
    let out = r.h5i(&["env", "inspect", "two", "--capture", &cap]);
    assert!(!out.status.success(), "cross-env inspect must be refused");
    assert!(out_str(&out).contains("not evidence for"), "{}", out_str(&out));
}

// ─── 9. concurrency: the run-lock serializes runs of one env ────────────────

#[test]
fn concurrent_runs_of_one_env_are_serialized() {
    use std::process::Stdio;
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "busy"]);

    // Launch a slow run in the background; it holds the run-lock for ~2s.
    let mut slow = Command::new(H5I)
        .args(["env", "run", "busy", "--", "sh", "-c", "sleep 2; echo slow-done"])
        .env("H5I_AGENT", "tester")
        .current_dir(&r.dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn slow run");

    // Give it a moment to take the lock, then a second run must be refused.
    std::thread::sleep(std::time::Duration::from_millis(400));
    let contender = r.h5i(&["env", "run", "busy", "--", "sh", "-c", "echo fast"]);
    assert!(!contender.status.success(), "second concurrent run must be refused");
    assert!(out_str(&contender).contains("busy"), "{}", out_str(&contender));

    assert!(slow.wait().unwrap().success());
    // After the lock is released, a new run succeeds.
    r.h5i_ok(&["env", "run", "busy", "--", "sh", "-c", "echo after"]);
}

// ─── 10. event log is secret-safe and carries resource accounting ───────────

#[test]
fn event_log_redacts_command_and_records_resources() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "acct"]);
    r.h5i_ok(&[
        "env", "run", "acct", "--", "sh", "-c",
        &format!("echo deploying with {PLANTED_SECRET}"),
    ]);

    // The raw event log blob (refs/h5i/env) must not leak the secret passed on
    // the command line, and must carry wall/cpu resource accounting.
    let log = out_str(&git(&r.dir, &["show", "refs/h5i/env:events.jsonl"]));
    assert!(!log.contains(PLANTED_SECRET), "secret leaked into the env event log: {log}");
    assert!(log.contains("redacted"), "command should be redacted in the event detail");
    let exec_line = log.lines().find(|l| l.contains("\"event\":\"exec\"")).expect("exec event");
    assert!(exec_line.contains("wall="), "exec event must record wall time: {exec_line}");
    assert!(exec_line.contains("cpu="), "exec event must record cpu time: {exec_line}");

    // The CLI run line surfaces resources too.
    let out = out_str(&r.h5i_ok(&["env", "run", "acct", "--", "sh", "-c", "true"]));
    assert!(out.contains("wall "), "run output should show wall time: {out}");
}

// ─── 11. tool allowlist enforcement (defense in depth) ──────────────────────

#[test]
fn tools_allowlist_is_enforced_at_run() {
    let r = Repo::new();
    std::fs::create_dir_all(r.dir.join(".h5i")).unwrap();
    std::fs::write(
        r.dir.join(".h5i/env.toml"),
        "[profile.default]\nisolation = \"workspace\"\ntools = [\"echo\", \"true\"]\n",
    )
    .unwrap();
    r.h5i_ok(&["env", "create", "pinned"]);

    // Listed program runs.
    r.h5i_ok(&["env", "run", "pinned", "--", "true"]);
    // Unlisted program is refused (and never executes).
    let out = r.h5i(&["env", "run", "pinned", "--", "sh", "-c", "echo nope > escaped.txt"]);
    assert!(!out.status.success(), "unlisted command must be refused");
    assert!(out_str(&out).contains("allowlist"), "{}", out_str(&out));
    assert!(!r.work("pinned").join("escaped.txt").exists(), "refused command must not run");
}

// ─── 12. the arena: compare environments from one base ──────────────────────

#[test]
fn compare_ranks_environments_and_flags_split_bases() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "cand-a"]);
    r.h5i_ok(&["env", "create", "cand-b"]);

    std::fs::write(r.work("cand-a").join("a.txt"), "one line\n").unwrap();
    std::fs::write(r.work("cand-b").join("b.txt"), "x\ny\nz\n").unwrap();
    r.h5i_ok(&["env", "run", "cand-a", "--", "sh", "-c", "echo a-ok"]);
    // cand-b's run fails on purpose — exit code passes through, so it's not _ok.
    let failed = r.h5i(&["env", "run", "cand-b", "--", "sh", "-c", "exit 2"]);
    assert_eq!(failed.status.code(), Some(2));

    let out = out_str(&r.h5i_ok(&["env", "compare", "cand-a", "cand-b"]));
    assert!(out.contains("common base"), "shared-base envs report a common base: {out}");
    assert!(out.contains("env/tester/cand-a"), "{out}");
    assert!(out.contains("env/tester/cand-b"), "{out}");
    assert!(out.contains("exit 0"), "cand-a's passing run shows: {out}");
    assert!(out.contains("exit 2"), "cand-b's failing run shows: {out}");

    // JSON form is machine-readable with the diffstat numbers.
    let json = out_str(&r.h5i_ok(&["env", "compare", "cand-a", "cand-b", "--json"]));
    let rows: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 2);
    let b = rows.as_array().unwrap().iter().find(|r| r["id"] == "env/tester/cand-b").unwrap();
    assert_eq!(b["insertions"], 3, "untracked-file lines counted: {json}");
    assert_eq!(b["last_exit"], 2);
}

#[test]
fn compare_warns_when_bases_differ() {
    let r = Repo::new();
    let first = out_str(&git(&r.dir, &["rev-parse", "HEAD"])).trim().to_string();
    r.h5i_ok(&["env", "create", "from-old", "--from", &first]);
    // Advance main, then create a second env off the new tip.
    std::fs::write(r.dir.join("moved.txt"), "moved\n").unwrap();
    git(&r.dir, &["add", "moved.txt"]);
    git(&r.dir, &["commit", "-m", "advance"]);
    r.h5i_ok(&["env", "create", "from-new"]);

    let out = out_str(&r.h5i_ok(&["env", "compare", "from-old", "from-new"]));
    assert!(out.contains("do NOT share a base"), "must warn on split bases: {out}");
}

// ─── 13. base drift + rebase (§9) ───────────────────────────────────────────

#[test]
fn status_reports_drift_and_rebase_refreshes_the_base() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "drifter"]);
    let base0 = out_str(&git(&r.dir, &["rev-parse", "HEAD"])).trim().to_string();

    // No drift initially.
    let st = out_str(&r.h5i_ok(&["env", "status", "drifter"]));
    assert!(st.contains("up to date with parent"), "{st}");
    assert!(st.contains(&base0[..12]), "status shows the pinned base: {st}");

    // The env makes a change on a disjoint file …
    std::fs::write(r.work("drifter").join("env.txt"), "from env\n").unwrap();
    // … while the parent advances on another file.
    std::fs::write(r.dir.join("lib.py"), "def hello():\n    return 99\n").unwrap();
    git(&r.dir, &["add", "lib.py"]);
    git(&r.dir, &["commit", "-m", "parent moves"]);
    let base1 = out_str(&git(&r.dir, &["rev-parse", "HEAD"])).trim().to_string();

    // status (and the JSON manifest's base) now show drift.
    let st = out_str(&r.h5i_ok(&["env", "status", "drifter"]));
    assert!(st.contains("parent advanced 1 commit"), "drift surfaced: {st}");

    // Rebase folds the parent's change in and re-pins the base.
    let out = out_str(&r.h5i_ok(&["env", "rebase", "drifter"]));
    assert!(out.contains("rebased onto main"), "{out}");
    assert_eq!(r.manifest("drifter")["base_commit"].as_str().unwrap(), base1, "base re-pinned");

    // Worktree now carries BOTH sides; drift is cleared.
    let lib = std::fs::read_to_string(r.work("drifter").join("lib.py")).unwrap();
    assert!(lib.contains("return 99"), "parent's change folded in: {lib}");
    assert!(r.work("drifter").join("env.txt").is_file(), "env's change preserved");
    let st = out_str(&r.h5i_ok(&["env", "status", "drifter"]));
    assert!(st.contains("up to date with parent"), "drift cleared: {st}");

    // The rebased env still applies cleanly onto the advanced parent.
    r.h5i_ok(&["env", "propose", "drifter"]);
    r.h5i_ok(&["env", "apply", "drifter"]);
    assert!(r.dir.join("env.txt").is_file());
}

#[test]
fn rebase_refuses_on_conflict_and_keeps_the_base() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "clash"]);
    let base0 = out_str(&git(&r.dir, &["rev-parse", "HEAD"])).trim().to_string();

    // Both the env and the parent edit the same file differently.
    std::fs::write(r.work("clash").join("README.md"), "env version\n").unwrap();
    std::fs::write(r.dir.join("README.md"), "parent version\n").unwrap();
    git(&r.dir, &["add", "README.md"]);
    git(&r.dir, &["commit", "-m", "parent readme"]);

    let out = r.h5i(&["env", "rebase", "clash"]);
    assert!(!out.status.success(), "conflicting rebase must refuse");
    assert!(out_str(&out).contains("conflicts against the new base"), "{}", out_str(&out));
    // The base is untouched after a refused rebase.
    assert_eq!(r.manifest("clash")["base_commit"].as_str().unwrap(), base0);
}

#[test]
fn status_json_still_emits_the_manifest() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "j"]);
    let json = out_str(&r.h5i_ok(&["env", "status", "j", "--json"]));
    let v: serde_json::Value = serde_json::from_str(&json).expect("status --json is JSON");
    assert_eq!(v["id"], "env/tester/j");
    assert_eq!(v["status"], "created");
}

// ─── 14. shareable environments across clones (the multi-agent review loop) ──

/// A clone addressed through the h5i binary under a fixed agent identity.
struct Clone {
    dir: PathBuf,
    agent: &'static str,
}

impl Clone {
    fn h5i(&self, args: &[&str]) -> Output {
        Command::new(H5I)
            .args(args)
            .env("H5I_AGENT", self.agent)
            .current_dir(&self.dir)
            .output()
            .expect("run h5i")
    }
    fn ok(&self, args: &[&str]) -> Output {
        let out = self.h5i(args);
        assert!(out.status.success(), "h5i {} failed:\n{}", args.join(" "), out_str(&out));
        out
    }
}

/// Build a bare origin and two h5i-init'd clones (agents `claude` and `codex`).
fn two_clones() -> (TempDir, Clone, Clone) {
    let root = TempDir::new().expect("tempdir");
    let bare = root.path().join("origin.git");
    run_ok(Command::new("git").args(["init", "-q", "--bare", "-b", "main"]).arg(&bare));

    let a = root.path().join("A");
    run_ok(Command::new("git").args(["clone", "-q"]).arg(&bare).arg(&a));
    git(&a, &["config", "user.email", "a@h5i.test"]);
    git(&a, &["config", "user.name", "A"]);
    std::fs::write(a.join("lib.py"), "def f():\n    return 1\n").unwrap();
    git(&a, &["add", "."]);
    git(&a, &["commit", "-m", "seed"]);
    git(&a, &["push", "-q", "origin", "main"]);
    let ca = Clone { dir: a, agent: "claude" };
    ca.ok(&["init"]);

    let b = root.path().join("B");
    run_ok(Command::new("git").args(["clone", "-q"]).arg(&bare).arg(&b));
    git(&b, &["config", "user.email", "b@h5i.test"]);
    git(&b, &["config", "user.name", "B"]);
    let cb = Clone { dir: b, agent: "codex" };
    cb.ok(&["init"]);

    (root, ca, cb)
}

#[test]
fn env_travels_to_another_clone_for_review_and_apply() {
    let (_root, a, b) = two_clones();

    // Clone A (claude): create, run, edit, propose, push.
    a.ok(&["env", "create", "fix-auth"]);
    a.ok(&["env", "run", "fix-auth", "--", "sh", "-c", "echo running-tests"]);
    std::fs::write(
        a.dir.join(".git/.h5i/env/claude/fix-auth/work/lib.py"),
        "def f():\n    return 2  # fixed\n",
    )
    .unwrap();
    a.ok(&["env", "propose", "fix-auth"]);
    a.ok(&["push"]);

    // Clone B (codex) cannot see it yet.
    assert!(!out_str(&b.ok(&["env", "list"])).contains("fix-auth"));

    // After pull, the env is materialized locally.
    let pulled = out_str(&b.ok(&["pull"]));
    assert!(pulled.contains("materialized") || pulled.contains("refs/h5i/env"), "{pulled}");
    let list = out_str(&b.ok(&["env", "list"]));
    assert!(list.contains("env/claude/fix-auth"), "B sees the env: {list}");
    assert!(list.contains("proposed"), "{list}");

    // B reviews the diff — from the pushed code branch (B has no worktree).
    let diff = out_str(&b.ok(&["env", "diff", "fix-auth"]));
    assert!(diff.contains("return 2"), "B reviews the proposed diff: {diff}");

    // B sees the policy digest + evidence in status, and the manifest via JSON.
    let json = out_str(&b.ok(&["env", "status", "fix-auth", "--json"]));
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["agent"], "claude", "manifest authorship preserved across clones");
    let cap = v["captures"][0].as_str().expect("a capture id").to_string();

    // B inspects the evidence (it travelled via refs/h5i/objects).
    let insp = out_str(&b.ok(&["env", "inspect", "fix-auth", "--capture", &cap]));
    assert!(insp.contains("running-tests") || insp.contains("sh"), "B inspects evidence: {insp}");

    // Workspace-mutating ops on B refuse clearly — the worktree is on A.
    // (propose passes the status guard, then the mediated commit needs work/.)
    let no_ws = b.h5i(&["env", "propose", "fix-auth"]);
    assert!(!no_ws.status.success());
    assert!(out_str(&no_ws).contains("another clone"), "{}", out_str(&no_ws));

    // B applies onto main (the code branch was fetched).
    git(&b.dir, &["checkout", "-q", "main"]);
    let applied = out_str(&b.ok(&["env", "apply", "fix-auth"]));
    assert!(applied.contains("applied onto main"), "{applied}");
    let lib = std::fs::read_to_string(b.dir.join("lib.py")).unwrap();
    assert!(lib.contains("return 2"), "apply updated B's working tree: {lib}");

    // The applied status round-trips back: B pushes, A pulls, A sees applied.
    git(&b.dir, &["push", "-q", "origin", "main"]);
    b.ok(&["push"]);
    a.ok(&["pull"]);
    let a_status = out_str(&a.ok(&["env", "status", "fix-auth", "--json"]));
    let av: serde_json::Value = serde_json::from_str(&a_status).unwrap();
    assert_eq!(av["status"], "applied", "applied status propagated back to A: {a_status}");
}

#[test]
fn env_ref_holds_manifest_and_policy_blobs() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "shared"]);
    // The ref tree carries the three shareable files.
    let manifests = out_str(&git(&r.dir, &["show", "refs/h5i/env:manifests.jsonl"]));
    assert!(manifests.contains("env/tester/shared"), "{manifests}");
    let policies = out_str(&git(&r.dir, &["show", "refs/h5i/env:policies.jsonl"]));
    assert!(policies.contains("env/tester/shared"), "policy blob present: {policies}");
    let events = out_str(&git(&r.dir, &["show", "refs/h5i/env:events.jsonl"]));
    assert!(events.contains("\"event\":\"created\""), "{events}");
}

// ─── 15. probe is honest and machine-readable ───────────────────────────────

#[test]
fn probe_reports_all_capability_lines() {
    let r = Repo::new();
    let out = out_str(&r.h5i_ok(&["env", "probe"]));
    for key in ["os", "landlock_abi", "userns", "seccomp", "workspace", "process"] {
        assert!(out.contains(key), "probe output missing {key}: {out}");
    }
    // Workspace is satisfiable everywhere.
    let ws_line = out.lines().find(|l| l.contains("workspace")).unwrap();
    assert!(ws_line.contains("yes"), "{ws_line}");
    // The functional self-test line is present and agrees with create.
    let run_line = out.lines().find(|l| l.contains("runnable")).expect("runnable line");
    let says_yes = run_line.contains("yes");
    assert_eq!(says_yes, process_tier_runnable(), "probe 'runnable' must match create: {run_line}");
}
