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

    /// Parse `h5i env probe` output into (landlock, userns, seccomp).
    fn probe(&self) -> (bool, bool, bool) {
        let out = out_str(&self.h5i_ok(&["env", "probe"]));
        let has = |k: &str, v: &str| out.lines().any(|l| l.contains(k) && l.contains(v));
        (
            !has("landlock_abi", "none"),
            has("userns", "true"),
            has("seccomp", "true"),
        )
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

#[test]
fn unimplemented_backends_refuse_at_create() {
    let r = Repo::new();
    for claim in ["container", "hardened-container", "microvm"] {
        let out = r.h5i(&["env", "create", "boxed", "--isolation", claim]);
        assert!(!out.status.success(), "claim {claim} must refuse");
        assert!(out_str(&out).contains("backend"), "{}", out_str(&out));
        assert!(!r.env_dir("boxed").exists(), "no state left behind on refusal");
    }
    let out = r.h5i(&["env", "create", "boxed", "--isolation", "docker"]);
    assert!(!out.status.success(), "unknown claim must refuse");
}

#[test]
fn process_claim_is_all_or_nothing_per_host() {
    let r = Repo::new();
    let (landlock, userns, seccomp) = r.probe();
    let satisfiable = landlock && userns && seccomp;
    let out = r.h5i(&["env", "create", "confined", "--isolation", "process"]);
    if satisfiable {
        assert!(out.status.success(), "{}", out_str(&out));
        assert_eq!(r.manifest("confined")["isolation_claim"], "process");
    } else {
        // Fail closed: refuse with an explicit reason, never downgrade.
        assert!(!out.status.success(), "must refuse on incapable host");
        let text = out_str(&out);
        assert!(text.contains("cannot be satisfied"), "{text}");
        assert!(!r.env_dir("confined").exists());
    }
}

// ─── 7. the kernel sandbox actually confines (capability-gated) ─────────────

/// Write outside $WORK is blocked by Landlock; write inside works; network is
/// unreachable under net.mode=deny. Skips (with a notice) when the host can't
/// satisfy the process claim — the fail-closed path is covered above.
#[test]
fn process_tier_confines_fs_and_network() {
    let r = Repo::new();
    let (landlock, userns, seccomp) = r.probe();
    if !(landlock && userns && seccomp) {
        eprintln!(
            "SKIP process_tier_confines_fs_and_network: host lacks landlock={landlock} \
             userns={userns} seccomp={seccomp}"
        );
        return;
    }

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

/// Env-var allowlist: only `env.pass` variables reach the confined process.
#[test]
fn process_tier_env_allowlist() {
    let r = Repo::new();
    let (landlock, userns, seccomp) = r.probe();
    if !(landlock && userns && seccomp) {
        eprintln!("SKIP process_tier_env_allowlist: host lacks process-tier capabilities");
        return;
    }
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

// ─── 8. probe is honest and machine-readable ────────────────────────────────

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
}
