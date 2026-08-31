//! End-to-end tests for `h5i env`: isolated agent environments
//! (worktree + sandbox + provenance, docs/environments-design.md).
//!
//! These drive the compiled binary as a subprocess against real git
//! repositories and prove the properties that define the feature:
//!
//!   1. `create` fuses a frozen base, a code branch, a git worktree under
//!      `.git/.h5i/env/`, a forked reasoning branch, and a pinned policy.
//!   2. `run` is capture-wrapped and policy-enforced: evidence lands in
//!      `refs/h5i/objects` tagged with the env id and policy digest, and the
//!      child's exit code passes through.
//!   3. `propose`/`apply` is the only road into the parent branch. Apply
//!      refuses without propose, and the mediated commit fails closed on
//!      path-allowlist violations.
//!   4. Isolation claims fail closed: an unsatisfiable claim refuses at
//!      `create` rather than silently downgrading.
//!   5. The kernel sandbox actually confines. These assertions are
//!      *capability-gated* and skip cleanly on hosts without Landlock or
//!      userns.
//!
//! Run with `cargo test --test env_integration -- --nocapture`.

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
/// The capability bits (Landlock, user namespaces, seccomp) can all be present
/// while a hardened kernel still denies `exec` under the full confinement stack,
/// notably AppArmor-restricted unprivileged user namespaces on Ubuntu 24.04 and
/// the GitHub Actions runners. `env create --isolation process` functionally
/// self-tests and fails closed there, so a successful create is the
/// authoritative signal. Cached across tests, the result being host-global.
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

/// Whether this host can actually *run* a supervised-tier confined command.
///
/// The supervised tier needs the whole mediation stack green (seccomp
/// user-notification, cgroup v2 delegation, nftables, namespaces) and refuses
/// rather than downgrading, so plenty of hosts, CI runners especially, will
/// skip every test gated on this.
fn supervised_tier_runnable() -> bool {
    use std::sync::OnceLock;
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        let r = Repo::new();
        let out = r.h5i(&["env", "create", "probe", "--isolation", "supervised"]);
        if !out.status.success() {
            eprintln!(
                "supervised tier not runnable on this host — its tests will skip:\n{}",
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
    /// A fresh repo with one seed commit and a git identity set.
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
        Repo { dir, _root: root }
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

    /// The *latest* receipt recorded for env `<slug>`. Records are appended
    /// chronologically, so the last line is the newest. Important when an env
    /// has several runs.
    fn capture_manifest(&self, slug: &str) -> serde_json::Value {
        let log = self.env_dir(slug).join("receipt.jsonl");
        let blob = std::fs::read_to_string(&log)
            .unwrap_or_else(|_| panic!("no receipt log at {}", log.display()));
        let line = blob
            .lines()
            .rfind(|l| !l.trim().is_empty())
            .expect("a receipt");
        serde_json::from_str(line).expect("receipt json")
    }

    /// Every receipt recorded for env `<slug>`, oldest first.
    fn receipts(&self, slug: &str) -> Vec<serde_json::Value> {
        let log = self.env_dir(slug).join("receipt.jsonl");
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("receipt json"))
            .collect()
    }

    /// The stored raw payload of a receipt.
    ///
    /// Resolved through the public accessor rather than by building the on-disk
    /// path: payload blobs are keyed by their *content* digest, while a receipt
    /// id names the run, and the two are deliberately not the same string.
    fn capture_raw_for(&self, slug: &str, id: &str) -> Vec<u8> {
        h5i_core::receipt::raw_bytes(&self.env_dir(slug), id)
            .unwrap_or_else(|_| panic!("raw payload {id} missing"))
    }
}

fn synthetic_env_manifest(
    repo: &git2::Repository,
    agent: &str,
    slug: &str,
) -> h5i_core::env::EnvManifest {
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let tree = head.tree().unwrap();
    h5i_core::env::EnvManifest {
        id: format!("env/{agent}/{slug}"),
        agent: agent.into(),
        slug: slug.into(),
        base_commit: head.id().to_string(),
        base_tree: tree.id().to_string(),
        parent_branch: "main".into(),
        branch: format!("refs/heads/h5i/env/{agent}/{slug}"),
        source: "repo".into(),
        profile: "default".into(),
        policy_digest: "d".repeat(64),
        effective_digest: None,
        fs_authority: None,
        isolation_claim: "workspace".into(),
        backend: "worktree".into(),
        created_at: "2026-06-11T00:00:00.000000Z".into(),
        updated_at: "2026-06-11T00:00:00.000000Z".into(),
        status: "proposed".into(),
        captures: Vec::new(),
        service_digest: None,
        persona_digest: None,
        pr: None,
        pr_head_ref: None,
        runner_id: None,
        runner: None,
    }
}

fn append_synthetic_env_manifest(repo: &git2::Repository, m: &h5i_core::env::EnvManifest) {
    h5i_core::env::append_env_commit(
        repo,
        &h5i_core::env::EnvEvent {
            ts: m.updated_at.clone(),
            env_id: m.id.clone(),
            agent: m.agent.clone(),
            event: "created".into(),
            detail: Some("synthetic test manifest".into()),
            capture: None,
        },
        Some(m),
        None,
    )
    .expect("append synthetic env manifest");
}

// ─── 1. create: the triple fusion ───────────────────────────────────────────

/// A fail-closed step between the worktree and the manifest used to leave a
/// registered+locked worktree and a branch that `create` refuses to reuse and
/// `rm` cannot see. Recoverable only with `git worktree prune` and
/// `git branch -D` by hand.
#[test]
fn a_failed_create_leaves_nothing_behind_to_clean_up() {
    let r = Repo::new();
    // A malformed [service.*] table: parsed at create, *after* the worktree
    // exists and *before* the manifest is written.
    std::fs::create_dir_all(r.dir.join(".h5i")).unwrap();
    std::fs::write(
        r.dir.join(".h5i/env.toml"),
        "[profile.default]\nisolation = \"workspace\"\n\n[service.web]\nport = 1\n",
    )
    .unwrap();
    git(&r.dir, &["add", "."]);
    git(&r.dir, &["commit", "-m", "add a broken service declaration"]);

    let out = r.h5i(&["env", "create", "broken"]);
    assert!(!out.status.success(), "create must fail closed: {}", out_str(&out));

    // Nothing half-built survives: the box is not listed, and neither the
    // branch nor the worktree registration is left over.
    let listed = r.h5i(&["env", "list"]);
    assert!(
        !String::from_utf8_lossy(&listed.stdout).contains("broken"),
        "a failed create must not leave a box behind: {}",
        out_str(&listed)
    );
    let branches = git(&r.dir, &["branch", "--list", "h5i/env/*/broken"]);
    assert!(
        String::from_utf8_lossy(&branches.stdout).trim().is_empty(),
        "the env branch must be gone: {}",
        out_str(&branches)
    );
    let worktrees = git(&r.dir, &["worktree", "list"]);
    assert!(
        !String::from_utf8_lossy(&worktrees.stdout).contains("broken"),
        "the worktree registration must be gone: {}",
        out_str(&worktrees)
    );

    // And the name is genuinely free again once the declaration is fixed.
    std::fs::write(
        r.dir.join(".h5i/env.toml"),
        "[profile.default]\nisolation = \"workspace\"\n",
    )
    .unwrap();
    git(&r.dir, &["add", "."]);
    git(&r.dir, &["commit", "-m", "fix the service declaration"]);
    let retry = r.h5i(&["env", "create", "broken"]);
    assert!(retry.status.success(), "retry after the fix must work: {}", out_str(&retry));
}

#[test]
fn create_builds_worktree_branch_policy_and_event() {
    let r = Repo::new();
    // `h5i init` drops its own untracked scaffolding (CLAUDE.md, .claude/…).
    // Snapshot the status BEFORE create so we assert create adds nothing.
    let st_before = out_str(&git(&r.dir, &["status", "--porcelain"]));
    let out = out_str(&r.h5i_ok(&["env", "create", "fix-auth"]));
    assert!(out.contains("env/tester/fix-auth"), "{out}");

    // Workspace: a git worktree under .git/.h5i, invisible to the main tree.
    let work = r.work("fix-auth");
    assert!(work.join("README.md").is_file(), "worktree checked out");
    assert!(work.join(".git").is_file(), "worktree gitlink present");
    let st_after = out_str(&git(&r.dir, &["status", "--porcelain"]));
    assert_eq!(
        st_after, st_before,
        "env create must not touch the main working tree"
    );

    // Code branch exists and points at the pinned base.
    let branch = out_str(&git(
        &r.dir,
        &["rev-parse", "refs/heads/h5i/env/tester/fix-auth"],
    ));
    let head = out_str(&git(&r.dir, &["rev-parse", "HEAD"]));
    assert_eq!(
        branch.trim(),
        head.trim(),
        "env branch starts at the frozen base"
    );

    // Manifest pins base/branch/policy.
    let m = r.manifest("fix-auth");
    assert_eq!(m["status"], "created");
    assert_eq!(m["agent"], "tester");
    assert_eq!(m["parent_branch"], "main");
    assert_eq!(m["base_commit"].as_str().unwrap(), head.trim());
    assert_eq!(m["branch"], "refs/heads/h5i/env/tester/fix-auth");
    assert_eq!(m["backend"], "worktree");
    assert_eq!(m["isolation_claim"], "workspace");
    assert_eq!(m["policy_digest"].as_str().unwrap().len(), 64);
    assert!(r.env_dir("fix-auth").join("policy.resolved.toml").is_file());

    // Event log: refs/h5i/env carries the created event.
    let log = out_str(&git(&r.dir, &["show", "refs/h5i/env/meta:events.jsonl"]));
    assert!(log.contains("\"event\":\"created\""), "{log}");
    assert!(log.contains("env/tester/fix-auth"), "{log}");

    // Listed.
    let list = out_str(&r.h5i_ok(&["env", "list"]));
    assert!(list.contains("env/tester/fix-auth"), "{list}");
    assert!(list.contains("created"), "{list}");
}

#[test]
fn create_warns_for_invalid_agent_identity_without_contaminating_json() {
    let r = Repo::new();
    let create = |name: &str, agent: Option<&str>| {
        let mut command = Command::new(H5I);
        command
            .args([
                "box",
                "create",
                name,
                "--isolation",
                "workspace",
                "--profile",
                "default",
                "--json",
            ])
            .current_dir(&r.dir);
        match agent {
            Some(value) => {
                command.env("H5I_AGENT", value);
            }
            None => {
                command.env_remove("H5I_AGENT");
            }
        }
        command.output().expect("failed to run h5i")
    };

    let overlong = "a".repeat(65);
    let cases = [
        ("d", Some("codex.code"), "human", true),
        ("s", Some("claude code"), "human", true),
        ("e", Some(""), "human", true),
        ("l", Some(overlong.as_str()), "human", true),
        ("u", Some("雪"), "human", true),
        ("v", Some("codex"), "codex", false),
        ("t", Some(" codex "), "codex", false),
        ("n", None, "human", false),
    ];
    for (name, value, expected_agent, should_warn) in cases {
        let out = create(name, value);
        assert_eq!(out.status.code(), Some(0), "{}", out_str(&out));
        let manifest: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("create stdout must remain valid JSON");
        assert_eq!(manifest["agent"], expected_agent);
        assert_eq!(manifest["id"], format!("env/{expected_agent}/{name}"));
        assert_eq!(
            manifest["branch"],
            format!("refs/heads/h5i/env/{expected_agent}/{name}")
        );

        let stderr = String::from_utf8_lossy(&out.stderr);
        if should_warn {
            assert_eq!(stderr.lines().count(), 1, "{stderr}");
            assert!(stderr.contains("warning: invalid H5I_AGENT"), "{stderr}");
            assert!(stderr.contains("using 'human'"), "{stderr}");
            // Skip the empty-string case: it has nothing to look for, and
            // `contains("")` is true for every haystack. Filtered rather than
            // nested so the check reads as one condition on both editions.
            if let Some(value) = value.filter(|v| !v.is_empty()) {
                assert!(
                    !stderr.contains(value),
                    "the invalid value must not be echoed: {stderr}"
                );
            }
        } else {
            assert!(stderr.is_empty(), "unexpected warning: {stderr}");
        }
    }
}

#[test]
fn create_audit_all_pins_capture_policy() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "audit-all", "--audit", "all"]);

    let policy = std::fs::read_to_string(r.env_dir("audit-all").join("policy.resolved.toml"))
        .expect("read policy");
    assert!(
        policy.contains("[audit]") && policy.contains("capture = \"all\""),
        "policy should pin audit-all capture mode:\n{policy}"
    );
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

/// The two states a brand-new user meets before git is ready (no repository
/// at all, and a `git init` with no commit behind HEAD) must answer with the
/// command that fixes them. Both used to leak libgit2's diagnosis
/// (`could not find repository at '.'; class=Repository (6)` and
/// `revspec 'HEAD' not found`), which names neither the precondition nor
/// what to do about it.
#[test]
fn create_names_the_fix_when_there_is_no_repo_or_no_commit() {
    let h5i_in = |dir: &Path, args: &[&str]| -> Output {
        Command::new(H5I)
            .args(args)
            .env("H5I_AGENT", "tester")
            .env("H5I_DEFAULT_ISOLATION", "workspace")
            .current_dir(dir)
            .output()
            .expect("failed to run h5i")
    };

    // Outside any repository: name the requirement and the way in.
    let no_repo = TempDir::new().expect("tempdir");
    let out = h5i_in(no_repo.path(), &["env", "create", "test"]);
    assert!(!out.status.success(), "create outside a repo must refuse");
    let said = out_str(&out);
    assert!(
        said.contains("needs to run inside a git repository") && said.contains("git init"),
        "no-repo refusal must name the fix, said: {said}"
    );
    assert!(
        !said.contains("class="),
        "libgit2 internals must not leak: {said}"
    );

    // A fresh `git init` with an unborn HEAD: say what is missing, a commit,
    // and how to make one, rather than "revspec 'HEAD' not found".
    let unborn = TempDir::new().expect("tempdir");
    run_ok(Command::new("git").args(["init", "-b", "main"]).arg(unborn.path()));
    let out = h5i_in(unborn.path(), &["env", "create", "test"]);
    assert!(!out.status.success(), "create on an unborn HEAD must refuse");
    let said = out_str(&out);
    assert!(
        said.contains("no commits yet") && said.contains("--allow-empty"),
        "unborn-HEAD refusal must name the fix, said: {said}"
    );

    // An explicit `--from` that does not resolve keeps the literal diagnosis:
    // there, "revision not found" is the correct one, and rewording it as
    // "make a commit" would point at the wrong problem.
    let r = Repo::new();
    let out = r.h5i(&["env", "create", "test", "--from", "deadbeef"]);
    assert!(!out.status.success(), "an unknown --from must refuse");
    let said = out_str(&out);
    assert!(
        said.contains("cannot resolve base revision 'deadbeef'"),
        "an explicit --from keeps the literal diagnosis, said: {said}"
    );
    assert!(
        !said.contains("no commits yet"),
        "a repo with commits must not be told it has none: {said}"
    );
}

/// A directory under `.git/worktrees/` that is not a worktree registration must
/// not be able to stop `create`, for this env or any other.
///
/// libgit2's `git_worktree_lookup` fails on such a directory, and
/// `git_repository_foreach_worktree` hands that failure up as `1` rather than as
/// an error, which is what `git_branch_is_checked_out` reads as "yes". So one
/// leftover directory made libgit2 answer *checked out* for every branch in the
/// repo, and `create` died on a branch it had made two lines earlier. Nothing
/// about that message points at the leftover, and no amount of picking a
/// different name gets past it.
///
/// Several leftovers, not one: libgit2 filters the invalid entries out of
/// `git_worktree_list` by removing them from a vector *while iterating it*, so
/// adjacent entries survive the filter and reach the lookup that poisons the
/// answer.
#[test]
fn stale_worktree_registrations_do_not_break_create() {
    let r = Repo::new();
    let regs = r.dir.join(".git/worktrees");
    for name in ["h5i-env-tester-gone", "stale-a", "stale-b", "stale-c"] {
        std::fs::create_dir_all(regs.join(name)).unwrap();
    }

    r.h5i_ok(&["env", "create", "after-stale"]);
    assert!(r.work("after-stale").join("README.md").exists());

    // …and the leftovers are gone, so the next `create` is not walking back
    // into the same hole.
    for name in ["h5i-env-tester-gone", "stale-a", "stale-b", "stale-c"] {
        assert!(
            !regs.join(name).exists(),
            "stale registration {name} should have been swept"
        );
    }
    assert!(regs.join("h5i-env-tester-after-stale").is_dir());
}

/// A `create` that fails while making the worktree must leave nothing behind.
///
/// The branch is created immediately before the worktree, and the rollback used
/// to be armed immediately *after* it, so a failure in between left a branch and
/// an `<env>/` directory with no manifest. `list` resolves envs through the
/// manifest and showed nothing, `rm` could not resolve the name either, and
/// retrying the same name answered "already exists" for an env that had never
/// existed.
///
/// The failure is forced by making `.git/worktrees` a regular file: git cannot
/// register a worktree under it, and fails at exactly that step.
#[test]
fn a_create_that_fails_at_the_worktree_leaves_no_branch_or_directory() {
    let r = Repo::new();
    std::fs::write(r.dir.join(".git/worktrees"), "not a directory\n").unwrap();

    let out = r.h5i(&["env", "create", "halfmade"]);
    assert!(!out.status.success(), "create must fail:\n{}", out_str(&out));

    assert!(!r.env_dir("halfmade").exists(), "env dir must be rolled back");
    let branches = out_str(&git(&r.dir, &["branch", "--list", "h5i/env/tester/*"]));
    assert!(
        branches.trim().is_empty(),
        "branch must be rolled back, saw: {branches}"
    );

    // And the name is genuinely free again: not "already exists", not "branch
    // already exists". It just works.
    std::fs::remove_file(r.dir.join(".git/worktrees")).unwrap();
    r.h5i_ok(&["env", "create", "halfmade"]);
}

/// An `<env>/` directory with no manifest is not an environment, and `create`
/// treats it as the leftover it is rather than reporting "already exists" for a
/// box that `list` cannot show and `rm` cannot remove.
#[test]
fn create_reclaims_an_env_directory_left_without_a_manifest() {
    let r = Repo::new();
    std::fs::create_dir_all(r.env_dir("leftover")).unwrap();

    r.h5i_ok(&["env", "create", "leftover"]);
    assert!(r.env_dir("leftover").join("manifest.json").exists());

    // A leftover that still holds a workspace is *not* silently reclaimed.
    // There is something in it to lose, so it is reported with the paths named.
    std::fs::create_dir_all(r.env_dir("held").join("work")).unwrap();
    let out = r.h5i(&["env", "create", "held"]);
    assert!(!out.status.success());
    let text = out_str(&out);
    assert!(
        text.contains("holds a workspace but no manifest") && text.contains("git branch -D"),
        "message should name the leftover and how to clear it: {text}"
    );
}

#[test]
fn create_pins_an_explicit_base_revision() {
    let r = Repo::new();
    let first = out_str(&git(&r.dir, &["rev-parse", "HEAD"]))
        .trim()
        .to_string();
    std::fs::write(r.dir.join("later.txt"), "later\n").unwrap();
    git(&r.dir, &["add", "later.txt"]);
    git(&r.dir, &["commit", "-m", "later"]);

    r.h5i_ok(&["env", "create", "old-base", "--from", &first]);
    let m = r.manifest("old-base");
    assert_eq!(m["base_commit"].as_str().unwrap(), first);
    // The worktree reflects the OLD base. Later.txt is absent.
    assert!(!r.work("old-base").join("later.txt").exists());
}

/// ROADMAP §P1: a kernel-tier create writes
/// `policy.effective.json` from the same computation the sandbox applies and
/// pins its digest in the manifest; a run rewrites it at the apply seam and
/// pins the digest of what it enforced in the capture record. A workspace-tier
/// env writes none. The schema describes the kernel mechanisms and nothing
/// else.
#[test]
#[cfg(target_os = "linux")]
fn effective_config_written_at_create_and_pinned_per_run() {
    if !process_tier_runnable() {
        return;
    }
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "eff", "--isolation", "process"]);

    let path = r.env_dir("eff").join("policy.effective.json");
    let text = std::fs::read_to_string(&path).expect("policy.effective.json written at create");
    let cfg: h5i_core::effective::EffectiveConfig =
        serde_json::from_str(&text).expect("effective config parses");
    assert_eq!(cfg.schema, 1);
    assert_eq!(cfg.claim, "process");
    // The worktree is a rw Landlock grant; fs_deny lives under resolution
    // metadata, never as an enforcement rule (Landlock is allowlist-only).
    let work = r.work("eff").canonicalize().unwrap();
    assert!(cfg.landlock.rw.contains(&work.to_string_lossy().into_owned()));
    // The manifest pins the digest of the exact bytes on disk.
    let m = r.manifest("eff");
    assert_eq!(
        m["effective_digest"].as_str().expect("manifest pins effective_digest"),
        cfg.digest().unwrap()
    );

    // A run rewrites the dump at the apply seam and its capture pins the digest.
    r.h5i_ok(&["env", "run", "eff", "--", "sh", "-c", "echo evidence"]);
    let run_text = std::fs::read_to_string(&path).expect("run rewrote the dump");
    let run_cfg: h5i_core::effective::EffectiveConfig =
        serde_json::from_str(&run_text).unwrap();
    let rec = r.capture_manifest("eff");
    assert_eq!(
        rec["effective_digest"].as_str().expect("capture pins effective_digest"),
        run_cfg.digest().unwrap()
    );

    // The workspace tier writes no dump and pins nothing.
    r.h5i_ok(&["env", "create", "ws"]);
    assert!(!r.env_dir("ws").join("policy.effective.json").exists());
    assert!(r.manifest("ws").get("effective_digest").is_none());

    // With only dump-less neighbors, the run's receipt claims no overlap
    // (the field is omitted when empty). The machine-checked strong answer.
    assert!(rec.get("fs_overlap").is_none(), "solo box must record no overlap: {rec}");

    // A second kernel-tier box on the same repo DOES overlap the first:
    // both hold rw grants into the shared git plumbing (`grant_box_git`),
    // which is true cross-box influence and must be said, not smoothed.
    r.h5i_ok(&["env", "create", "eff2", "--isolation", "process"]);
    r.h5i_ok(&["env", "run", "eff2", "--", "sh", "-c", "true"]);
    let rec2 = r.capture_manifest("eff2");
    let overlap: Vec<&str> = rec2["fs_overlap"]
        .as_array()
        .unwrap_or_else(|| panic!("second box must record its overlap: {rec2}"))
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        overlap.iter().any(|o| o.starts_with("env/tester/eff via ")),
        "overlap must name the sibling box and the shared path: {overlap:?}"
    );
}

#[test]
fn env_allow_add_list_remove_and_in_box_refusal() {
    let r = Repo::new();
    // Redirect the user config dir so the test never touches the real
    // ~/.config/h5i (the allowlist is per-user host state, not repo state).
    let cfg = TempDir::new().expect("tempdir");
    let run = |args: &[&str], in_box: bool| -> Output {
        let mut c = Command::new(H5I);
        c.args(args)
            .env("H5I_AGENT", "tester")
            .env("H5I_DEFAULT_ISOLATION", "workspace")
            .env("XDG_CONFIG_HOME", cfg.path())
            .current_dir(&r.dir);
        if in_box {
            c.env("H5I_ENV_ID", "env/tester/boxed");
        }
        c.output().expect("failed to run h5i")
    };

    let out = run(&["env", "allow", "PyPI.org"], false);
    assert!(out.status.success(), "{}", out_str(&out));
    let file = cfg.path().join("h5i").join("egress-allow");
    let text = std::fs::read_to_string(&file).expect("allowlist written");
    assert!(text.contains("pypi.org"), "normalized lowercase rule:\n{text}");

    // Duplicate add is a friendly no-op (one line stays one line).
    run(&["env", "allow", "pypi.org"], false);
    let text = std::fs::read_to_string(&file).unwrap();
    assert_eq!(text.matches("pypi.org").count(), 1, "{text}");

    // Bare `env allow` lists the rules.
    let out = run(&["env", "allow"], false);
    assert!(out.status.success());
    assert!(out_str(&out).contains("pypi.org"));

    let out = run(&["env", "allow", "--json"], false);
    assert!(out.status.success(), "{}", out_str(&out));
    let allowlist: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("valid JSON allowlist");
    assert_eq!(allowlist["rules"], serde_json::json!(["pypi.org"]));
    assert_eq!(allowlist["path"], file.to_string_lossy().as_ref());

    let out = run(&["env", "allow", "pypi.org", "--json"], false);
    assert!(!out.status.success());
    assert!(out_str(&out).contains("--json can only be used when listing"));

    let out = run(&["env", "allow", "--remove", "--json"], false);
    assert!(!out.status.success());
    assert!(out_str(&out).contains("--json can only be used when listing"));

    // Strict intake: a URL is not a host rule.
    let out = run(&["env", "allow", "https://evil.example/x"], false);
    assert!(!out.status.success(), "URL must be rejected");

    // In-box mutation is refused. A confined agent must not widen its own
    // network grants (the file also isn't box-writable; this is the belt).
    let out = run(&["env", "allow", "evil.example"], true);
    assert!(!out.status.success(), "in-box env allow must refuse");
    assert!(
        out_str(&out).contains("inside an env box"),
        "{}",
        out_str(&out)
    );

    let out = run(&["env", "allow", "pypi.org", "--remove"], false);
    assert!(out.status.success());
    assert!(!std::fs::read_to_string(&file).unwrap().contains("pypi.org"));
}

#[test]
fn env_create_pr_pins_pr_head_as_base() {
    let r = Repo::new();
    // A "GitHub-like" remote: a bare repo exposing a PR head at refs/pull/7/head.
    let remote_dir = r.dir.parent().unwrap().join("remote.git");
    run_ok(Command::new("git").args(["init", "--bare"]).arg(&remote_dir));
    git(&r.dir, &["remote", "add", "origin", remote_dir.to_str().unwrap()]);
    git(&r.dir, &["push", "origin", "main"]);
    git(&r.dir, &["checkout", "-b", "feature"]);
    std::fs::write(r.dir.join("pr.txt"), "pr change\n").unwrap();
    git(&r.dir, &["add", "pr.txt"]);
    git(&r.dir, &["commit", "-m", "pr commit"]);
    let pr_head = out_str(&git(&r.dir, &["rev-parse", "HEAD"]))
        .trim()
        .to_string();
    git(&r.dir, &["push", "origin", "HEAD:refs/pull/7/head"]);
    git(&r.dir, &["checkout", "main"]);
    git(&r.dir, &["branch", "-D", "feature"]);

    r.h5i_ok(&["env", "create", "review-pr", "--pr", "7"]);
    let m = r.manifest("review-pr");
    // The immutable base IS the PR head, and the review target is its local
    // tracking branch, not whatever branch happened to be checked out.
    assert_eq!(m["base_commit"].as_str().unwrap(), pr_head);
    assert_eq!(m["parent_branch"].as_str().unwrap(), "pr/7");
    assert_eq!(m["pr"].as_u64().unwrap(), 7);
    let tip = out_str(&git(&r.dir, &["rev-parse", "refs/heads/pr/7"]))
        .trim()
        .to_string();
    assert_eq!(tip, pr_head);
    assert!(r.work("review-pr").join("pr.txt").exists());
    // The throwaway incoming ref was dropped (the branch keeps the commit).
    let gone = Command::new("git")
        .args(["rev-parse", "--verify", "refs/h5i/_incoming/pr-7"])
        .current_dir(&r.dir)
        .output()
        .unwrap();
    assert!(!gone.status.success(), "temp incoming ref must be deleted");

    // Fail closed on collision: a local pr/7 moved elsewhere is never
    // force-updated by a later --pr create.
    git(&r.dir, &["branch", "-f", "pr/7", "main"]);
    let out = r.h5i(&["env", "create", "review-pr-2", "--pr", "7"]);
    assert!(!out.status.success(), "colliding pr/7 must refuse");
    assert!(out_str(&out).contains("already exists"), "{}", out_str(&out));

    // --pr and --from are mutually exclusive.
    let out = r.h5i(&["env", "create", "review-pr-3", "--pr", "7", "--from", "HEAD"]);
    assert!(!out.status.success());
}

// ─── 2. run: capture-wrapped, evidence-tagged, exit-code transparent ────────

#[test]
fn run_captures_evidence_with_env_id_and_policy_digest() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "evidence"]);
    r.h5i_ok(&[
        "env",
        "run",
        "evidence",
        "--",
        "sh",
        "-c",
        "echo out-line; echo err-line >&2",
    ]);

    // The receipt carries the env tags: which env, which enforced policy, and
    // which lane observed it.
    let m = r.capture_manifest("evidence");
    assert_eq!(m["env_id"], "env/tester/evidence");
    let env_manifest = r.manifest("evidence");
    assert_eq!(m["policy_digest"], env_manifest["policy_digest"]);
    assert_eq!(m["source"], "host-env-run");
    assert_eq!(m["exit_code"], 0);
    // Both streams reached the stored payload.
    let payload =
        String::from_utf8_lossy(&r.capture_raw_for("evidence", m["id"].as_str().unwrap()))
            .into_owned();
    assert!(payload.contains("out-line"), "{payload}");
    assert!(payload.contains("err-line"), "{payload}");

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

/// `env status` surfaces evidence STAGED in the spool but not yet ingested
/// (visible mid-session, before the host materializes it at run/shell end).
/// Staged captures, notes, and tee-shim records, with the pending commands.
#[test]
fn env_status_shows_pending_spool_evidence() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "pend"]);
    let spool = r.env_dir("pend").join("spool");
    std::fs::create_dir_all(&spool).unwrap();

    // A staged in-box capture (+ its raw) and a tee-shim record.
    std::fs::write(
        spool.join("cap-1-0.json"),
        r#"{"cmd":"pytest -q","cwd":null,"exit_code":0,"files":[],"cmd_argv":["pytest","-q"]}"#,
    )
    .unwrap();
    std::fs::write(spool.join("cap-1-0.raw"), b"...output...").unwrap();
    std::fs::write(spool.join("cmd-9-0.cmd"), b"ls").unwrap();

    let status = out_str(&r.h5i_ok(&["env", "status", "pend"]));
    assert!(status.contains("pending"), "{status}");
    assert!(
        status.contains("1 capture") && status.contains("1 shim"),
        "breakdown by lane: {status}"
    );
    // The pending command is listed (the useful detail).
    assert!(status.contains("pytest -q"), "{status}");

    // No spool → no pending line at all.
    std::fs::remove_dir_all(&spool).unwrap();
    let status = out_str(&r.h5i_ok(&["env", "status", "pend"]));
    assert!(
        !status.contains("pending"),
        "no pending line when spool empty: {status}"
    );
}

#[test]
fn run_passes_the_exit_code_through() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "fails"]);
    let out = r.h5i(&[
        "env",
        "run",
        "fails",
        "--",
        "sh",
        "-c",
        "echo boom >&2; exit 7",
    ]);
    assert_eq!(out.status.code(), Some(7), "exit code must pass through");
    // The failed run is still evidence.
    let m = r.manifest("fails");
    assert_eq!(m["captures"].as_array().unwrap().len(), 1);
}

#[test]
fn run_executes_inside_the_worktree() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "whereami"]);
    r.h5i_ok(&[
        "env",
        "run",
        "whereami",
        "--",
        "sh",
        "-c",
        "echo probe > made-here.txt",
    ]);
    assert!(r.work("whereami").join("made-here.txt").is_file());
    assert!(
        !r.dir.join("made-here.txt").exists(),
        "parent tree untouched"
    );
}

// ─── 3. propose / apply: the only road into the parent branch ───────────────

#[test]
fn full_lifecycle_create_run_propose_apply() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "feature"]);
    r.h5i_ok(&[
        "env",
        "run",
        "feature",
        "--",
        "sh",
        "-c",
        "printf 'def hello():\\n    return 2\\n' > lib.py && echo done",
    ]);

    // Diff against the frozen base sees the change.
    let diff = out_str(&r.h5i_ok(&["env", "diff", "feature"]));
    assert!(diff.contains("return 2"), "{diff}");
    let stat = out_str(&r.h5i_ok(&["env", "diff", "feature", "--stat"]));
    assert!(stat.contains("lib.py"), "diffstat includes changed file: {stat}");

    let diff_json = out_str(&r.h5i_ok(&["env", "diff", "feature", "--json"]));
    let report: serde_json::Value =
        serde_json::from_str(&diff_json).expect("env diff --json emits valid JSON");
    assert_eq!(report["files_changed"], 1, "one file changed: {diff_json}");
    assert_eq!(report["insertions"], 1, "one changed line added: {diff_json}");
    assert_eq!(report["deletions"], 1, "one changed line removed: {diff_json}");
    assert_eq!(report["files"][0]["path"], "lib.py");
    assert_eq!(report["files"][0]["insertions"], 1);
    assert_eq!(report["files"][0]["deletions"], 1);

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
    assert!(
        lib.contains("return 2"),
        "apply must update the parent working tree"
    );
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
fn propose_accepts_noop_env_and_records_reviewable_state() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "noop"]);

    let main_before = out_str(&git(&r.dir, &["rev-parse", "main"]));
    let branch_before = out_str(&git(
        &r.dir,
        &["rev-parse", "refs/heads/h5i/env/tester/noop"],
    ));

    let brief = out_str(&r.h5i_ok(&["env", "propose", "noop"]));
    assert!(brief.contains("Proposal: env/tester/noop"), "{brief}");
    assert!(brief.contains("0 files changed"), "{brief}");
    assert!(brief.contains("never automatic"), "{brief}");
    assert_eq!(r.manifest("noop")["status"], "proposed");
    assert_eq!(
        out_str(&git(&r.dir, &["rev-parse", "main"])),
        main_before,
        "noop propose must not touch the parent branch"
    );
    assert_eq!(
        out_str(&git(
            &r.dir,
            &["rev-parse", "refs/heads/h5i/env/tester/noop"],
        )),
        branch_before,
        "noop propose must not create an empty env-branch commit"
    );

    let log = out_str(&r.h5i_ok(&["env", "log", "noop"]));
    assert!(
        log.contains("proposed") && log.contains("no new changes"),
        "noop proposal should leave an auditable proposed event:\n{log}"
    );
}

#[test]
fn propose_is_idempotent_after_snapshot_without_new_worktree_changes() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "again"]);
    std::fs::write(r.work("again").join("new.txt"), "from env\n").unwrap();

    let first = out_str(&r.h5i_ok(&["env", "propose", "again"]));
    assert!(first.contains("new.txt"), "{first}");
    let branch_after_first = out_str(&git(
        &r.dir,
        &["rev-parse", "refs/heads/h5i/env/tester/again"],
    ));

    let second = out_str(&r.h5i_ok(&["env", "propose", "again"]));
    assert!(second.contains("Proposal: env/tester/again"), "{second}");
    assert!(
        second.contains("new.txt"),
        "second proposal should still show the proposed diff: {second}"
    );
    assert_eq!(r.manifest("again")["status"], "proposed");
    assert_eq!(
        out_str(&git(
            &r.dir,
            &["rev-parse", "refs/heads/h5i/env/tester/again"],
        )),
        branch_after_first,
        "re-proposing unchanged worktree must not add another snapshot commit"
    );

    let log = out_str(&r.h5i_ok(&["env", "log", "again"]));
    assert!(
        log.matches("proposed").count() >= 2,
        "both proposal attempts should be auditable:\n{log}"
    );
    assert!(
        log.contains("no new changes"),
        "second proposal should record why no snapshot commit was made:\n{log}"
    );
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

/// `--patch` squashes the env's divergence into a single-parent commit on the
/// parent branch, even when a fast-forward would otherwise be possible. The
/// resulting commit carries the env's content but no second (merge) parent.
#[test]
fn apply_patch_mode_squashes_into_single_parent_commit() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "squash"]);
    std::fs::write(r.work("squash").join("env-file.txt"), "from env\n").unwrap();
    r.h5i_ok(&["env", "propose", "squash"]);

    // Parent has NOT moved → a plain `apply` would fast-forward. `--patch`
    // must instead synthesize a fresh squash commit (no fast-forward).
    let out = out_str(&r.h5i_ok(&["env", "apply", "squash", "--patch"]));
    assert!(out.contains("applied onto main"), "{out}");
    assert!(
        !out.contains("fast-forward"),
        "patch mode must never fast-forward: {out}"
    );

    // The env content landed on the parent working tree.
    assert!(r.dir.join("env-file.txt").is_file());
    assert_eq!(r.manifest("squash")["status"], "applied");

    // The applied commit is a single-parent (squash) commit, not a merge.
    let applied = out_str(&git(&r.dir, &["rev-parse", "main"]));
    let applied = applied.trim();
    let parents = out_str(&git(&r.dir, &["rev-list", "--parents", "-n", "1", applied]));
    assert_eq!(
        parents.split_whitespace().count(),
        2,
        "patch mode must produce exactly one parent (commit + 1 parent): {parents}"
    );
    let msg = out_str(&git(&r.dir, &["log", "-1", "--format=%s", applied]));
    assert!(msg.contains("--patch"), "squash commit subject: {msg}");
}

/// `--patch` also squashes when the parent has advanced. The result is a
/// single-parent commit on top of the advanced parent (a 3-way merge tree with
/// only the parent recorded as a parent), not a two-parent merge.
#[test]
fn apply_patch_mode_squashes_over_advanced_parent() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "squash2"]);
    std::fs::write(r.work("squash2").join("env-file.txt"), "from env\n").unwrap();
    r.h5i_ok(&["env", "propose", "squash2"]);

    // Advance the parent on a disjoint file so a 3-way merge is required.
    std::fs::write(r.dir.join("parent-file.txt"), "from parent\n").unwrap();
    git(&r.dir, &["add", "parent-file.txt"]);
    git(&r.dir, &["commit", "-m", "advance parent"]);

    r.h5i_ok(&["env", "apply", "squash2", "--patch"]);
    assert!(r.dir.join("env-file.txt").is_file());
    assert!(r.dir.join("parent-file.txt").is_file());

    let applied = out_str(&git(&r.dir, &["rev-parse", "main"]));
    let applied = applied.trim();
    let parents = out_str(&git(&r.dir, &["rev-list", "--parents", "-n", "1", applied]));
    assert_eq!(
        parents.split_whitespace().count(),
        2,
        "patch over an advanced parent stays single-parent: {parents}"
    );
}

/// Applying a proposed env that never diverged from its parent is a clean
/// no-op: it reports "nothing to apply", marks the env applied, and leaves the
/// parent branch untouched (no empty commit).
#[test]
fn apply_noop_env_reports_nothing_to_apply() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "empty"]);
    // Propose with no worktree changes. The env branch tip stays at base.
    r.h5i_ok(&["env", "propose", "empty"]);

    let before = out_str(&git(&r.dir, &["rev-parse", "main"]));
    let out = out_str(&r.h5i_ok(&["env", "apply", "empty"]));
    assert!(out.contains("nothing to apply"), "{out}");
    assert_eq!(r.manifest("empty")["status"], "applied");
    assert_eq!(
        out_str(&git(&r.dir, &["rev-parse", "main"])),
        before,
        "no-op apply must not write the parent branch"
    );
    let log = out_str(&r.h5i_ok(&["env", "log", "empty"]));
    assert!(
        log.contains("applied") && log.contains("no-op"),
        "no-op apply should record why nothing was applied:\n{log}"
    );
}

/// Apply is a one-shot PROPOSED→APPLIED transition: once an env is applied it
/// is no longer 'proposed', so a second `apply` refuses (apply is never
/// automatic / idempotent-repeat).
#[test]
fn apply_refuses_second_apply() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "once"]);
    std::fs::write(r.work("once").join("once.txt"), "x\n").unwrap();
    r.h5i_ok(&["env", "propose", "once"]);
    r.h5i_ok(&["env", "apply", "once"]);
    assert_eq!(r.manifest("once")["status"], "applied");

    let out = r.h5i(&["env", "apply", "once"]);
    assert!(
        !out.status.success(),
        "re-applying an applied env must refuse"
    );
    assert!(
        out_str(&out).contains("propose"),
        "second apply should point back at propose: {}",
        out_str(&out)
    );
}

#[test]
fn mediated_commit_fails_closed_on_nested_git_repo() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "smuggle"]);
    // An agent (or its build) drops a nested git repo inside $WORK. Staging
    // it would record a gitlink/submodule pointer. Must refuse, not skip.
    let nested = r.work("smuggle").join("vendor/dep");
    std::fs::create_dir_all(&nested).unwrap();
    run_ok(Command::new("git").args(["init"]).arg(&nested));
    std::fs::write(nested.join("f.txt"), "x\n").unwrap();

    let out = r.h5i(&["env", "propose", "smuggle"]);
    assert!(
        !out.status.success(),
        "nested .git must fail the mediated commit"
    );
    let text = out_str(&out);
    assert!(
        text.contains("fail-closed") || text.contains(".git"),
        "{text}"
    );
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

/// A tracked path whose parent directory has been swapped for a symlink now
/// resolves *outside* `$WORK`, and staging it would copy whatever lives there
/// into the reviewed patch. The mediated commit must refuse.
///
/// This is the escape that needs no new file: the agent deletes a tracked
/// directory, links it somewhere else, and the content follows the link. The
/// symlink itself is fine (it is stored as a link blob, never followed); the
/// file *under* it is not.
#[test]
fn mediated_commit_fails_closed_on_a_tracked_path_symlinked_out_of_work() {
    let r = Repo::new();
    // A tracked file inside a real directory, in the base commit.
    std::fs::create_dir_all(r.dir.join("pkg")).unwrap();
    std::fs::write(r.dir.join("pkg/conf.txt"), "in-repo\n").unwrap();
    git(&r.dir, &["add", "."]);
    git(&r.dir, &["commit", "-m", "tracked file in a directory"]);

    r.h5i_ok(&["env", "create", "escape"]);

    // Somewhere outside $WORK, with a file at the same relative path.
    let outside = r.dir.parent().unwrap().join("outside-the-box");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("conf.txt"), "HOST-SECRET-OUTSIDE-WORK\n").unwrap();

    // Swap the tracked directory for a symlink to it.
    let work_pkg = r.work("escape").join("pkg");
    std::fs::remove_dir_all(&work_pkg).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, &work_pkg).unwrap();

    let out = r.h5i(&["env", "propose", "escape"]);
    let text = out_str(&out);
    assert!(
        !out.status.success(),
        "a tracked path resolving outside $WORK must fail the mediated commit:\n{text}"
    );
    assert!(
        text.contains("escapes $WORK") || text.contains("fail-closed"),
        "the refusal should name the escape:\n{text}"
    );
    assert_eq!(r.manifest("escape")["status"], "created");
}

/// Register a real submodule at `sub_path` in the repo's base commit, sourced
/// from a fresh standalone repo. Returns the gitlink commit OID. Uses the local
/// `file://` protocol (explicitly allowed) so no network is touched.
fn add_base_submodule(r: &Repo, src_name: &str, sub_path: &str) -> String {
    let src = r.dir.parent().unwrap().join(src_name);
    run_ok(Command::new("git").args(["init", "-b", "main"]).arg(&src));
    git(&src, &["config", "user.name", "Sub"]);
    git(&src, &["config", "user.email", "sub@h5i.test"]);
    std::fs::write(src.join("m.txt"), "module\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "sub seed"]);
    run_ok(
        Command::new("git")
            .args(["-c", "protocol.file.allow=always", "submodule", "add"])
            .arg(&src)
            .arg(sub_path)
            .current_dir(&r.dir),
    );
    git(&r.dir, &["add", "."]);
    git(&r.dir, &["commit", "-m", "add submodule"]);
    out_str(&git(&r.dir, &["rev-parse", &format!("HEAD:{sub_path}")]))
        .trim()
        .to_string()
}

#[test]
fn mediated_commit_allows_unchanged_base_submodule() {
    // Regression: a repo that legitimately uses a git submodule must still be
    // proposable. The submodule is an upstream gitlink the env inherited at
    // create time, not an agent-smuggled pointer, so it round-trips unchanged
    // instead of tripping the fail-closed gitlink refusal.
    let r = Repo::new();
    let gitlink = add_base_submodule(&r, "sub-src", "examples/dep");

    r.h5i_ok(&["env", "create", "sub"]);
    // The agent makes an ordinary edit, so the mediated commit has real changes
    // to write. The inherited gitlink must survive alongside them.
    std::fs::write(r.work("sub").join("new.txt"), "agent work\n").unwrap();

    // Propose must SUCCEED (previously refused with a gitlink violation).
    r.h5i_ok(&["env", "propose", "sub"]);
    assert_eq!(r.manifest("sub")["status"], "proposed");

    // The committed env-branch tree still carries the submodule at the same OID.
    let tree_line = out_str(&git(
        &r.dir,
        &["ls-tree", "refs/heads/h5i/env/tester/sub", "examples/dep"],
    ));
    assert!(
        tree_line.contains("160000"),
        "gitlink mode preserved: {tree_line}"
    );
    assert!(
        tree_line.contains(&gitlink),
        "gitlink OID {gitlink} preserved: {tree_line}"
    );
}

#[test]
fn mediated_commit_still_rejects_new_gitlink_beside_submodule() {
    // The exemption is scoped to the *registered* base submodule path. It is
    // NOT a blanket "any gitlink allowed". A new nested repo the agent drops at
    // a different path must still fail the mediated commit, even when a legit
    // submodule is present.
    let r = Repo::new();
    add_base_submodule(&r, "sub-src", "examples/dep");

    r.h5i_ok(&["env", "create", "sub"]);
    let nested = r.work("sub").join("vendor/evil");
    std::fs::create_dir_all(&nested).unwrap();
    run_ok(Command::new("git").args(["init"]).arg(&nested));
    std::fs::write(nested.join("f.txt"), "x\n").unwrap();

    let out = r.h5i(&["env", "propose", "sub"]);
    assert!(
        !out.status.success(),
        "a new nested repo must still fail closed: {}",
        out_str(&out)
    );
    let text = out_str(&out);
    assert!(text.contains("vendor/evil"), "{text}");
    // The legit submodule was NOT what tripped it, and the env did not advance.
    assert!(!text.contains("examples/dep"), "{text}");
    assert_eq!(r.manifest("sub")["status"], "created");
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
        .args([
            "env",
            "run",
            "needs-secret",
            "--",
            "sh",
            "-c",
            "echo TOKEN=[$MY_TOKEN]",
        ])
        .env("H5I_AGENT", "tester")
        .env("H5I_SECRET_MY_TOKEN", "supersecret-xyz")
        .current_dir(&r.dir)
        .output()
        .expect("run");
    assert!(out.status.success(), "run failed: {}", out_str(&out));

    // The injected value must NOT appear in the capture, but the surrounding
    // text must, proving the secret was actually injected (then redacted).
    let cap = r.capture_manifest("needs-secret");
    let raw = String::from_utf8_lossy(&r.capture_raw_for(
        "needs-secret",
        cap["id"].as_str().unwrap(),
    ))
    .into_owned();
    assert!(
        !raw.contains("supersecret-xyz"),
        "secret value leaked into the stored payload:\n{raw}"
    );
    assert!(
        raw.contains("[redacted secret]"),
        "expected the injected secret to be redacted (proves it was injected):\n{raw}"
    );

    // A `secret` event records the grant id + fingerprint, never the value.
    let log = out_str(&r.h5i_ok(&["env", "log", "needs-secret"]));
    assert!(
        log.contains("secret") && log.contains("grant=MY_TOKEN"),
        "no secret audit event:\n{log}"
    );
    assert!(
        !log.contains("supersecret-xyz"),
        "secret value leaked into the event log:\n{log}"
    );
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
        .args([
            "env",
            "run",
            "filesec",
            "--",
            "sh",
            "-c",
            "echo KEY=[$(cat $DEPLOY_KEY_FILE)]",
        ])
        .env("H5I_AGENT", "tester")
        .env("H5I_SECRET_DEPLOY_KEY", "topsecret-deploy")
        .current_dir(&r.dir)
        .output()
        .expect("run");
    assert!(out.status.success(), "run failed: {}", out_str(&out));

    // The file-injected value must be redacted from the capture (proves it was
    // delivered via the file and then scrubbed).
    let cap = r.capture_manifest("filesec");
    let raw = String::from_utf8_lossy(&r.capture_raw_for("filesec", cap["id"].as_str().unwrap()))
        .into_owned();
    assert!(!raw.contains("topsecret-deploy"), "secret leaked: {raw}");
    assert!(
        raw.contains("[redacted secret]"),
        "expected redaction marker: {raw}"
    );

    // The audit event records the grant with inject=file, never the value.
    let log = out_str(&r.h5i_ok(&["env", "log", "filesec"]));
    assert!(
        log.contains("grant=DEPLOY_KEY") && log.contains("inject=file"),
        "{log}"
    );
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
    assert!(
        !out.status.success(),
        "run must fail closed when a grant can't be resolved"
    );
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
    // claim must be REFUSED. Never silently downgraded. An impossible claim.
    let out = r.h5i(&["env", "create", "untrusted", "--isolation", "supervised"]);
    if out.status.success() {
        // The only way this succeeds is if the host genuinely has the whole
        // stack green, then the manifest must honestly say 'supervised'.
        assert_eq!(r.manifest("untrusted")["isolation_claim"], "supervised");
    } else {
        let text = out_str(&out);
        assert!(
            text.contains("supervised")
                && (text.contains("refus") || text.contains("cannot be satisfied")),
            "supervised must fail closed with an explanation, got:\n{text}"
        );
    }
}

/// Set up a repo with a `supervised` profile plus optional extra profile TOML
/// and create env `slug`. Returns `None`, so the caller skips cleanly, when the
/// host cannot satisfy the supervised claim.
///
/// Serializes the heavy supervised e2e tests. Each spawns confined children, and
/// several at once under cargo's parallel harness exhaust the host's fork
/// capacity, making unrelated `git` subprocesses flake with EAGAIN. Holding this
/// for the test's duration caps peak fork pressure without serializing the whole
/// suite. Poison-tolerant, so a failing test surfaces its real assertion.
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
    // produces a capture. What we read below; only a setup failure has none.
    let _out = Command::new(H5I)
        .args(&full)
        .env("H5I_AGENT", "tester")
        .current_dir(&r.dir)
        .output()
        .expect("run");
    let cap = r.capture_manifest(slug);
    Some(String::from_utf8_lossy(&r.capture_raw_for(slug, cap["id"].as_str()?)).into_owned())
}

/// Comprehensive live proof of the supervised tier's runtime enforcement, in a
/// SINGLE env with a few *sequential* runs (deliberately not one test per
/// property: many parallel supervised runs forking confined children exhaust
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
    let Some(r) = supervised_env("confine", "") else {
        return;
    };

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
        except OSError: print('NOCONNECT')\n\
        import ctypes,os\n\
        os.chdir(os.environ.get('TMPDIR','/tmp'))\n\
        try:\n\
        \x20_u=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM); _u.bind('probe.sock')\n\
        \x20print('UNIXBIND ALLOWED')\n\
        except OSError as e:\n\
        \x20print('UNIXBIND DENIED',errno.errorcode.get(e.errno,e.errno))\n\
        _l=ctypes.CDLL(None,use_errno=True); ctypes.set_errno(0)\n\
        _l.ptrace(0,0,0,0)\n\
        _pe=ctypes.get_errno()\n\
        print('PTRACE',('DENIED '+errno.errorcode.get(_pe,str(_pe))) if _pe else 'ALLOWED')\n\
        ctypes.set_errno(0); _l.ptrace(10,1,0,0)\n\
        _ae=ctypes.get_errno()\n\
        print('PTATTACH',('DENIED '+errno.errorcode.get(_ae,str(_ae))) if _ae else 'ALLOWED')\n";
    let net = supervised_run_raw(&r, "confine", &["python3", "-c", net_script]).expect("run 1");
    // Default-deny socket gate: only boring inet is allowed.
    assert!(
        net.contains("RAW DENIED EPERM"),
        "raw socket denied:\n{net}"
    );
    assert!(
        net.contains("PACKET DENIED EPERM"),
        "packet socket denied:\n{net}"
    );
    // Linux denies the ungranted family at `socket()`, via the seccomp-notify
    // gate. Seatbelt has no hook there. It filters the *operations*, so the fd
    // is created and `bind`/`connect` are what fail. Same containment (an
    // ungranted AF_UNIX socket cannot be used), different layer, so the
    // assertion follows the mechanism the platform actually has. The macOS half
    // is checked below, where a bind is attempted for real.
    if cfg!(target_os = "macos") {
        assert!(
            net.contains("UNIXBIND DENIED EPERM"),
            "an ungranted AF_UNIX socket must be unusable:\n{net}"
        );
    } else {
        assert!(
            net.contains("UNIX DENIED EPERM"),
            "ungranted AF_UNIX denied:\n{net}"
        );
    }
    assert!(
        net.contains("INET ALLOWED"),
        "ordinary inet socket allowed:\n{net}"
    );
    // Airtight netns under net.mode=deny: no route to any external host.
    assert!(
        net.contains("NOCONNECT") && !net.contains("CONNECTED"),
        "netns must have no egress:\n{net}"
    );
    // The seccomp deny-list blocks ptrace(PTRACE_TRACEME), a classic
    // sandbox-escape vector that would otherwise succeed for an unprivileged
    // process. A bare EPERM here is unambiguous: only the deny-list produces it.
    //
    // macOS has no seccomp, so `PT_TRACE_ME` is not refused, and on its own it
    // is not an escape either: it marks the caller traceable by its own parent,
    // which is inside the box. The vector that matters is attaching to a process
    // the box does not own, and Seatbelt denies that.
    if cfg!(target_os = "macos") {
        assert!(
            net.contains("PTATTACH DENIED"),
            "a box must not be able to ptrace-attach outside itself:\n{net}"
        );
    } else {
        assert!(
            net.contains("PTRACE DENIED EPERM"),
            "ptrace must be seccomp-denied (escape vector):\n{net}"
        );
    }

    // The socket-gate verdicts are recorded in the run's capture EgressSummary.
    //
    // This tally is a product of the seccomp-notify gate, which sees every
    // `socket()` and can count it. macOS has no such hook: enforcement there is
    // the Seatbelt profile plus, when the profile declares an allowlist, the host
    // proxy, and only the proxy produces a per-request tally. So a deny-all
    // profile on macOS enforces (asserted above) while recording no counts:
    // weaker evidence, same boundary.
    if !cfg!(target_os = "macos") {
        let cap = r.capture_manifest("confine");
        let eg = &cap["egress"];
        assert!(
            eg.is_object(),
            "supervised capture must carry an egress summary: {cap}"
        );
        assert!(
            eg["denied"].as_u64().unwrap_or(0) >= 1,
            "denials counted: {eg}"
        );
        assert!(
            eg["allowed"].as_u64().unwrap_or(0) >= 1,
            "allows counted: {eg}"
        );
    }

    // Run 2 (sh): Landlock FS allowlist + seccomp deny-list (unshare).
    let fs_script = "echo in > inside.txt && echo WORK_OK; \
        echo x > /etc/h5i-escape 2>/dev/null && echo ETC_WROTE || echo ETC_DENIED; \
        unshare --mount /bin/true 2>&1; echo unshare_rc=$?";
    let fs = supervised_run_raw(&r, "confine", &["sh", "-c", fs_script]).expect("run 2");
    assert!(fs.contains("WORK_OK"), "writing $WORK succeeds:\n{fs}");
    assert!(
        fs.contains("ETC_DENIED") && !fs.contains("ETC_WROTE"),
        "Landlock denies writes outside $WORK:\n{fs}"
    );
    // `unshare` is a Linux tool exercising a Linux deny-list; on macOS it is
    // simply absent (rc=127), which would prove nothing either way. The FS half
    // above is the part that transfers, and it is asserted on both.
    if !cfg!(target_os = "macos") {
        assert!(
            fs.contains("Operation not permitted") || fs.contains("unshare_rc=1"),
            "seccomp deny-list blocks unshare:\n{fs}"
        );
    }
}

/// A memory limit is enforced for a supervised run: a large allocation under a
/// tight cap does not complete (cgroup memory.max / RLIMIT_DATA). Separate env
/// because it needs a `resources.mem` profile.
#[test]
fn supervised_memory_limit_is_enforced() {
    // Darwin has no cgroups, does not enforce RLIMIT_AS against the mmap'd
    // heap, and scopes RLIMIT_NPROC to the uid rather than the box, so the
    // kernel tiers there apply no memory cap. A documented limitation that
    // `box status` now marks with `*` rather than reporting as enforced. The
    // cap is real at the container and microvm tiers.
    if cfg!(target_os = "macos") {
        eprintln!("skipping: no per-box memory cap at the macOS kernel tiers");
        return;
    }
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
    assert!(
        !raw.contains("ALLOCATED"),
        "a 400MiB alloc under a 64MiB cap must not complete:\n{raw}"
    );
}

fn have_bin(name: &str) -> bool {
    std::process::Command::new(name)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Supervised increment 2: a `net.egress` allowlist confines the netns to
/// exactly the pinned hosts. Slirp4netns provides the uplink, an nftables
/// default-drop ruleset is the airtight L3/L4 guard, and DNS is pinned via a
/// private `/etc/hosts` (no port 53 at all). So an allowlisted host resolves to
/// the pinned IP and connects, while everything else fails closed. Needs real
/// outbound network, so it is *opt-in* via `H5I_TEST_NET=1` (mirrors the
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
    let Some(r) = supervised_env("egbox", "net.egress = [\"example.com\"]\n") else {
        return;
    };

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
    assert!(
        raw.contains("example.com CONNECTED"),
        "allowlisted host must connect:\n{raw}"
    );
    assert!(
        raw.contains("www.cloudflare.com BLOCKED") && !raw.contains("www.cloudflare.com CONNECTED"),
        "a non-allowlisted host must be blocked (fail-closed):\n{raw}"
    );
}

// ─── 3d. agent-family profiles at the supervised tier ───────────────────────

/// Set up a repo pinning `<profile>` to `supervised` and create env `slug` with
/// it. `None` (skip) when the host cannot satisfy the claim or lacks the
/// profile's tooling. Same gate style as [`supervised_env`].
fn supervised_profile_env(slug: &str, profile: &str) -> Option<Repo> {
    let r = Repo::new();
    std::fs::create_dir_all(r.dir.join(".h5i")).unwrap();
    std::fs::write(
        r.dir.join(".h5i/env.toml"),
        format!("[profile.{profile}]\nisolation = \"supervised\"\n"),
    )
    .unwrap();
    git(&r.dir, &["add", ".h5i/env.toml"]);
    git(&r.dir, &["commit", "-m", "supervised agent profile"]);
    let out = r.h5i(&[
        "env", "create", slug, "--profile", profile, "--isolation", "supervised",
    ]);
    if out.status.success() {
        Some(r)
    } else {
        // Create is fail-closed for this profile on hosts without the stack or
        // without Chrome/agent-browser, and says which. Skipping is correct;
        // silently passing would not be.
        eprintln!(
            "skipping supervised `{profile}`: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        None
    }
}

/// The private `/tmp` bind must not hide the box's own workspace when the
/// repository itself lives under `/tmp`, which every `tempfile` repo in this
/// suite does and which is the common case for CI.
///
/// This is the M4 write-up's first finding, and the write-up is wrong: it
/// recorded the `supervised` plus agent-profile `EINVAL` as "the workspace is
/// under `/tmp`, so the private-`/tmp` redirect shadows the worktree", and
/// called it an unfixed footgun that `create` should refuse. The bind-ordering
/// fix (`home_binds_in_mount_order`, which mounts `/tmp` last) already made this
/// work, and a `create`-time refusal would have rejected a configuration that
/// runs correctly, including this suite's own fixtures.
///
/// So the test pins the working behaviour: the workspace is visible from inside,
/// and `/tmp` is still the empty per-env scratch rather than the host's.
#[test]
fn a_workspace_under_tmp_survives_the_private_tmp_bind() {
    let _serial = supervised_guard();
    let Some(r) = supervised_env("tmpwork", "") else {
        return;
    };
    // `Repo::new()` builds under the system temp dir, so this only means
    // something where that is genuinely `/tmp`.
    if !r.dir.starts_with("/tmp") {
        eprintln!("skipping: fixtures are not under /tmp on this host ({:?})", r.dir);
        return;
    }
    std::fs::write(r.work("tmpwork").join("marker.txt"), "visible\n").unwrap();

    let raw = supervised_run_raw(
        &r,
        "tmpwork",
        &["sh", "-c", "cat marker.txt; echo TMPLIST; ls -A /tmp"],
    )
    .expect("run");
    assert!(
        raw.contains("visible"),
        "the workspace must still resolve inside the box:\n{raw}"
    );
    // And the reason it is worth a test: `/tmp` really is shadowed. The
    // workspace surviving is the mount *order*, not the absence of the bind.
    let after = raw.split("TMPLIST").nth(1).unwrap_or("");
    assert!(
        after.trim().is_empty(),
        "/tmp must still be the empty per-env scratch:\n{raw}"
    );
}

/// The gap the M4 live run exposed: no test ran an agent-family profile at
/// `supervised`, and `supervised` is the only kernel tier that can host one,
/// since `process` refuses the egress these profiles need. Both M4 surprises
/// lived in that gap.
///
/// The property under test is the socket gate's `AF_UNIX` verdict, because that
/// is what silently bricked the browser box: the `agent-browser` daemon's
/// control socket is a filesystem-bound `AF_UNIX` listener, the gate denied the
/// family for every profile, and the daemon died before writing a word. A
/// profile-scoped grant is the fix, so the test has to be per-profile:
/// `browser` gets `AF_UNIX`, and the neighbouring profile that did not ask for
/// it still does not.
#[test]
fn a_browser_box_at_supervised_gets_af_unix_and_its_neighbours_do_not() {
    let _serial = supervised_guard();
    if !have_python3() {
        eprintln!("skipping: python3 unavailable");
        return;
    }
    let Some(r) = supervised_profile_env("bbox", "browser") else {
        return;
    };

    // `python3` on the host is not `python3` in the box: on macOS `/usr/bin/python3`
    // is the Xcode shim, and the box denies `/Applications/Xcode.app`, so it
    // cannot start at all. Probe in-box rather than trusting `have_python3`,
    // which asks the host.
    let probe = supervised_run_raw(&r, "bbox", &["python3", "-c", "print('PY OK')"]);
    if !probe.as_deref().unwrap_or("").contains("PY OK") {
        eprintln!("skipping: python3 does not run inside a box on this host");
        return;
    }

    // Bind a real filesystem-bound listener, which is what the daemon does,
    // rather than just socket(), so a gate that allowed the family but a
    // Landlock grant that refused MAKE_SOCK would still be caught.
    //
    // Under `TMPDIR`, not a literal `/tmp`: only the Linux tiers bind-mount a
    // per-env directory over `/tmp`, and on macOS the literal path is denied
    // outright. Bound from *inside* the directory, so `sun_path` stays a bare
    // filename: a box's private tmp is `<repo>/.git/.h5i/env/<agent>/<slug>/tmp`,
    // and with the repo under a temp dir the absolute socket path runs past the
    // 104-byte `sun_path` limit, which would read here as a policy denial.
    let script = "import socket,os,errno\n\
        d=os.path.join(os.environ.get('TMPDIR','/tmp'),'gate-probe')\n\
        os.makedirs(d,exist_ok=True)\n\
        os.chdir(d)\n\
        try:\n\
        \x20s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM)\n\
        \x20s.bind('x.sock'); s.listen(1); s.close(); print('UNIXBIND OK')\n\
        except OSError as e:\n\
        \x20print('UNIXBIND FAIL',errno.errorcode.get(e.errno,e.errno))\n";
    let raw = supervised_run_raw(&r, "bbox", &["python3", "-c", script]).expect("browser box run");
    assert!(
        raw.contains("UNIXBIND OK"),
        "the browser profile grants AF_UNIX, so its daemon's control socket must bind:\n{raw}"
    );

    // The grant is per-profile, not a tier-wide loosening. A `default` box on
    // the same host, same tier, still gets EPERM. Otherwise the fix would have
    // widened every box to buy one.
    let Some(plain) = supervised_env("plainbox", "") else {
        return;
    };
    let raw = supervised_run_raw(&plain, "plainbox", &["python3", "-c", script]).expect("plain run");
    assert!(
        raw.contains("UNIXBIND FAIL EPERM"),
        "AF_UNIX stays denied for a profile that did not ask for it:\n{raw}"
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
    assert!(
        r.env_dir("doomed").join("manifest.json").is_file(),
        "manifest retained"
    );
    run_ok(
        Command::new("git")
            .args(["rev-parse", "refs/heads/h5i/env/tester/doomed"])
            .current_dir(&r.dir),
    );
    // A live env is NOT gc'd.
    r.h5i_ok(&["env", "create", "alive"]);
    r.h5i_ok(&["env", "gc"]);
    assert!(r.work("alive").exists(), "live env untouched by gc");

    // Run after gc refuses cleanly.
    let out = r.h5i(&["env", "run", "doomed", "--", "true"]);
    assert!(!out.status.success());
}

#[test]
fn rm_erases_workspace_branches_and_manifest() {
    let r = Repo::new();
    let branch = "refs/heads/h5i/env/tester/scratch";
    let ctx_branch = "refs/h5i/context/env/tester/scratch";

    r.h5i_ok(&["env", "create", "scratch"]);
    r.h5i_ok(&["env", "run", "scratch", "--", "sh", "-c", "echo evidence"]);
    // A live env refuses removal without --force.
    let out = r.h5i(&["env", "rm", "scratch"]);
    assert!(
        !out.status.success(),
        "live env must refuse rm without --force"
    );
    assert!(
        r.env_dir("scratch").join("manifest.json").is_file(),
        "manifest still present"
    );

    // --force removes everything: workspace, both branches, on-disk dir.
    r.h5i_ok(&["env", "rm", "scratch", "--force"]);
    assert!(!r.work("scratch").exists(), "workspace gone");
    assert!(!r.env_dir("scratch").exists(), "env dir erased");
    for refname in [branch, ctx_branch] {
        let rp = Command::new("git")
            .args(["rev-parse", "--verify", refname])
            .current_dir(&r.dir)
            .output()
            .expect("git spawn");
        assert!(!rp.status.success(), "{refname} should be deleted");
    }
    // Gone from the list, and a second rm reports no such env.
    assert!(
        !out_str(&r.h5i_ok(&["env", "list"])).contains("scratch"),
        "not listed"
    );
    assert!(
        !r.h5i(&["env", "rm", "scratch", "--force"]).status.success(),
        "already gone"
    );

    // An applied/aborted env removes without --force.
    r.h5i_ok(&["env", "create", "done"]);
    r.h5i_ok(&["env", "abort", "done"]);
    r.h5i_ok(&["env", "rm", "done"]);
    assert!(
        !r.env_dir("done").exists(),
        "aborted env removed without --force"
    );
}

// ─── 6. isolation claims fail closed ────────────────────────────────────────

/// Secure-by-default: `--isolation auto` (which force-probes, ignoring the
/// test's `H5I_DEFAULT_ISOLATION=workspace` pin) selects the *strongest* tier
/// this host can actually run, and the invariant is that the picked tier then
/// runs a command cleanly (auto never lands on an unrunnable tier). Serialized
/// with the other confined-fork tests since auto may pick supervised/process.
#[test]
fn auto_isolation_picks_a_runnable_tier() {
    let _serial = supervised_guard();
    let r = Repo::new();
    let out = r.h5i(&["env", "create", "autobox", "--isolation", "auto"]);
    assert!(
        out.status.success(),
        "auto create must succeed:\n{}",
        out_str(&out)
    );

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
    // hardened-container (gVisor/Kata) still has no adapter in this build.
    let out = r.h5i(&["env", "create", "boxed", "--isolation", "hardened-container"]);
    assert!(!out.status.success(), "hardened-container must refuse");
    assert!(out_str(&out).contains("backend"), "{}", out_str(&out));
    assert!(
        !r.env_dir("boxed").exists(),
        "no state left behind on refusal"
    );
    // An unknown claim name is rejected outright.
    let out = r.h5i(&["env", "create", "boxed", "--isolation", "docker"]);
    assert!(!out.status.success(), "unknown claim must refuse");
}

/// The microvm tier has an adapter, so it refuses for *substantive* reasons,
/// a missing image or a host that cannot virtualize, and never by silently
/// downgrading to a weaker tier.
#[test]
fn microvm_claim_fails_closed_with_an_actionable_reason() {
    let r = Repo::new();
    // No image: a static profile error, true on every host, so it is reported
    // regardless of whether this machine has `msb` or KVM.
    let out = r.h5i(&["env", "create", "boxed", "--isolation", "microvm"]);
    assert!(!out.status.success(), "microvm without an image must refuse");
    let text = out_str(&out);
    assert!(text.contains("requires a base image"), "{text}");
    assert!(
        !r.env_dir("boxed").exists(),
        "no state left behind on refusal"
    );

    // With an image the verdict depends on the host. Either it resolves to the
    // microvm tier, or it refuses saying so. Never a quiet downgrade to a
    // weaker claim, which is the failure mode this whole design exists to avoid.
    let out = r.h5i(&[
        "env",
        "create",
        "vmbox",
        "--isolation",
        "microvm",
        "--image",
        "alpine",
    ]);
    let text = out_str(&out);
    if out.status.success() {
        assert_eq!(r.manifest("vmbox")["isolation_claim"], "microvm");
    } else {
        assert!(
            text.contains("never silently downgrades"),
            "a refusal must say why and must not downgrade: {text}"
        );
        assert!(!r.env_dir("vmbox").exists(), "no state left behind: {text}");
    }
}

#[test]
fn process_claim_is_all_or_nothing_per_host() {
    let r = Repo::new();
    let out = r.h5i(&["env", "create", "confined", "--isolation", "process"]);
    if process_tier_runnable() {
        assert!(out.status.success(), "{}", out_str(&out));
        assert_eq!(r.manifest("confined")["isolation_claim"], "process");
    } else {
        // Fail closed: refuse with an explicit reason, never downgrade. Whether
        // the bits are missing or the confinement simply can't exec on this host.
        assert!(
            !out.status.success(),
            "must refuse when process tier is not runnable"
        );
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
/// satisfy the process claim. The fail-closed path is covered above.
#[test]
fn process_tier_confines_fs_and_network() {
    if !process_tier_runnable() {
        eprintln!(
            "SKIP process_tier_confines_fs_and_network: process tier not runnable on this host"
        );
        return;
    }
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "jail", "--isolation", "process"]);

    // Inside $WORK: writable.
    r.h5i_ok(&[
        "env",
        "run",
        "jail",
        "--",
        "sh",
        "-c",
        "echo ok > inside.txt",
    ]);
    assert!(r.work("jail").join("inside.txt").is_file());

    // Outside $WORK (the parent repo!): must be blocked.
    let escape = r.dir.join("escaped.txt");
    let out = r.h5i(&[
        "env",
        "run",
        "jail",
        "--",
        "sh",
        "-c",
        &format!("echo pwned > {}", escape.display()),
    ]);
    assert!(!out.status.success(), "write outside $WORK must fail");
    assert!(!escape.exists(), "no file may appear outside $WORK");

    // The shared .git IS reachable through the worktree gitlink, but only on
    // the narrow in-box surface (own admin dir + objects + own ref namespace;
    // see env::box_git_grants). A worktree that can't even `rev-parse HEAD`
    // bricks the boxed agent; the write-side jail is proven in
    // `box_git_grants_stay_fail_closed_outside_env_namespace`.
    let out = r.h5i(&[
        "env",
        "run",
        "jail",
        "--",
        "sh",
        "-c",
        "git rev-parse HEAD >/dev/null 2>&1 && echo GIT-OK || echo GIT-BLOCKED",
    ]);
    let text = out_str(&out);
    assert!(text.contains("GIT-OK"), "in-box git must function: {text}");

    // Network: deny means even loopback TCP fails. Use a pure-shell probe.
    let out = r.h5i(&[
        "env",
        "run",
        "jail",
        "--",
        "sh",
        "-c",
        "(exec 3<>/dev/tcp/127.0.0.1/22) 2>/dev/null && echo NET-OPEN || echo NET-CLOSED",
    ]);
    let text = out_str(&out);
    // bash-only /dev/tcp; dash prints an error and exits non-zero → also CLOSED-ish.
    assert!(!text.contains("NET-OPEN"), "network must be denied: {text}");

    // Dangerous syscalls are denied (unshare → EPERM).
    let out = r.h5i(&[
        "env",
        "run",
        "jail",
        "--",
        "sh",
        "-c",
        "unshare -U true 2>/dev/null && echo UNSHARE-OK || echo UNSHARE-BLOCKED",
    ]);
    let text = out_str(&out);
    assert!(
        !text.contains("UNSHARE-OK"),
        "unshare must be denied: {text}"
    );
}

/// Config lockdown: an interactive process-tier session ro-binds the project
/// `.claude` directory so the in-box agent can read its config but can neither
/// edit `settings.json` NOR create a `settings.local.json` (the
/// `disableAllHooks` create-bypass). Writes elsewhere in `$WORK` still work,
/// and the host file is untouched (the mount is ns-local).
#[test]
fn process_tier_config_lockdown_blocks_settings_tamper() {
    if !process_tier_runnable() {
        eprintln!(
            "SKIP process_tier_config_lockdown_blocks_settings_tamper: process tier not runnable"
        );
        return;
    }
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "cfg", "--isolation", "process"]);
    let claude = r.work("cfg").join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    std::fs::write(claude.join("settings.json"), "{\"hooks\":{}}").unwrap();

    // Inherits the real HOME (a temp HOME under /tmp would trip the
    // granted-/tmp-contains-denied-~/.ssh lint). Any home-scope config locks are
    // ns-local and harmless; the assertions below all concern $WORK/.claude.
    let out = r.h5i(&[
        "env", "shell", "cfg", "--", "sh", "-c",
        "cat .claude/settings.json >/dev/null && echo READ-OK || echo READ-FAIL; \
         (echo X > .claude/settings.json) 2>/dev/null && echo EDIT-OK || echo EDIT-BLOCKED; \
         (echo X > .claude/settings.local.json) 2>/dev/null && echo CREATE-OK || echo CREATE-BLOCKED; \
         (echo X > other.txt) 2>/dev/null && echo OTHER-OK || echo OTHER-BLOCKED",
    ]);
    let text = out_str(&out);
    assert!(
        text.contains("READ-OK"),
        "config must stay readable: {text}"
    );
    assert!(
        text.contains("EDIT-BLOCKED"),
        "settings.json must be read-only: {text}"
    );
    assert!(
        text.contains("CREATE-BLOCKED"),
        "settings.local.json create must be blocked: {text}"
    );
    assert!(
        text.contains("OTHER-OK"),
        "writes outside .claude must still work: {text}"
    );
    // The host file is untouched (ns-local mount).
    assert_eq!(
        std::fs::read_to_string(claude.join("settings.json")).unwrap(),
        "{\"hooks\":{}}",
        "host config must be unchanged"
    );
    assert!(
        !claude.join("settings.local.json").exists(),
        "no local settings on host"
    );
}

/// In-box git: the env worktree must be a *functional* checkout under the
/// kernel sandbox. `git status` works, and a commit made inside the box lands
/// on the env's code branch (visible to the host) while `main` is untouched.
/// This is the regression test for the agent-in-box bug where every git/h5i
/// command died on EACCES at the worktree's `commondir` (rendered by libgit2
/// as a misleading "is locked").
#[test]
fn box_git_status_and_commit_work_inside_process_tier() {
    if !process_tier_runnable() {
        eprintln!(
            "SKIP box_git_status_and_commit_work_inside_process_tier: process tier not runnable"
        );
        return;
    }
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "boxgit", "--isolation", "process"]);

    // status: worktree admin dir (index refresh) + commondir reads.
    r.h5i_ok(&["env", "run", "boxgit", "--", "git", "status", "--porcelain"]);

    // commit: objects rw + own branch ref dir rw (+ its reflog dir).
    r.h5i_ok(&[
        "env",
        "run",
        "boxgit",
        "--",
        "sh",
        "-c",
        "echo boxed > boxed.txt && git add boxed.txt && \
         git -c user.name=Box -c user.email=box@h5i.test commit -m in-box-commit",
    ]);

    let env_tip = out_str(&git(
        &r.dir,
        &[
            "log",
            "-1",
            "--format=%s",
            "refs/heads/h5i/env/tester/boxgit",
        ],
    ));
    assert!(
        env_tip.contains("in-box-commit"),
        "host must see the in-box commit: {env_tip}"
    );
    let main_tip = out_str(&git(&r.dir, &["log", "-1", "--format=%s", "main"]));
    assert_eq!(main_tip.trim(), "seed", "main must be untouched");
}

/// The in-box git grants stay narrow: the box can commit to its own env
/// branch, but moving refs outside `refs/heads/h5i/env/<agent>/`, rewriting
/// the repo config (a writable `core.fsmonitor` would execute code on the
/// host), and touching its own manifest (which would let it widen its sandbox
/// on the next run) all fail closed.
#[test]
fn box_git_grants_stay_fail_closed_outside_env_namespace() {
    if !process_tier_runnable() {
        eprintln!(
            "SKIP box_git_grants_stay_fail_closed_outside_env_namespace: process tier not runnable"
        );
        return;
    }
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "boxjail", "--isolation", "process"]);

    // Diverge the env branch first. Otherwise `update-ref main HEAD` would be
    // an undetectable no-op (same oid).
    r.h5i_ok(&[
        "env",
        "run",
        "boxjail",
        "--",
        "sh",
        "-c",
        "echo x > f.txt && git add f.txt && \
         git -c user.name=B -c user.email=b@h5i.test commit -m divergent",
    ]);

    // Moving main is refused, and main does not move.
    let main_before = out_str(&git(&r.dir, &["rev-parse", "main"]));
    let out = r.h5i(&[
        "env",
        "run",
        "boxjail",
        "--",
        "git",
        "update-ref",
        "refs/heads/main",
        "HEAD",
    ]);
    assert!(
        !out.status.success(),
        "moving main from inside the box must fail: {}",
        out_str(&out)
    );
    assert_eq!(
        out_str(&git(&r.dir, &["rev-parse", "main"])),
        main_before,
        "main moved!"
    );

    // Another agent's env namespace is refused too (grant is per-agent).
    let out = r.h5i(&[
        "env",
        "run",
        "boxjail",
        "--",
        "git",
        "update-ref",
        "refs/heads/h5i/env/other/x",
        "HEAD",
    ]);
    assert!(
        !out.status.success(),
        "foreign env namespace must be unwritable: {}",
        out_str(&out)
    );

    // Repo config is read-only.
    let out = r.h5i(&[
        "env",
        "run",
        "boxjail",
        "--",
        "git",
        "config",
        "core.fsmonitor",
        "/bin/false",
    ]);
    assert!(
        !out.status.success(),
        "writing .git/config must fail: {}",
        out_str(&out)
    );
    let cfg = std::fs::read_to_string(r.dir.join(".git/config")).unwrap();
    assert!(
        !cfg.contains("fsmonitor"),
        "config must be unchanged: {cfg}"
    );

    // The env's own manifest/policy dir (the sibling of $WORK) stays sealed.
    let out = r.h5i(&[
        "env",
        "run",
        "boxjail",
        "--",
        "sh",
        "-c",
        "echo x >> ../manifest.json",
    ]);
    assert!(
        !out.status.success(),
        "manifest must be unwritable from the box: {}",
        out_str(&out)
    );

    // Hooks are never granted: planting one from the box must fail.
    let hook = r.dir.join(".git/hooks/pre-commit");
    let out = r.h5i(&[
        "env",
        "run",
        "boxjail",
        "--",
        "sh",
        "-c",
        &format!("printf '#!/bin/sh\\n' > {}", hook.display()),
    ]);
    assert!(
        !out.status.success(),
        "hook planting must fail: {}",
        out_str(&out)
    );
    assert!(!hook.exists(), "no hook may appear: {}", hook.display());

    // Agent hook config is reviewer-controlled: a boxed agent may not plant or
    // rewrite repo-local Claude/Codex hook setup files.
    let out = r.h5i(&[
        "env",
        "run",
        "boxjail",
        "--",
        "sh",
        "-c",
        "mkdir -p .claude .codex && echo pwn > .claude/settings.json && echo pwn > .codex/config.toml",
    ]);
    assert!(
        !out.status.success(),
        "hook config planting must fail closed: {}",
        out_str(&out)
    );
    let work = r.dir.join(".git/.h5i/env/tester/boxjail/work");
    assert!(
        !work.join(".claude/settings.json").exists(),
        "Claude hook config must be removed after tamper"
    );
    assert!(
        !work.join(".codex/config.toml").exists(),
        "Codex hook config must be removed after tamper"
    );
}

/// The wall-clock kill must reap the WHOLE process tree (process-group kill),
/// not just the direct child. A runaway backgrounded descendant must die too.
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
    // blocks for 60s. The 1s wall-clock fires long before 8s, even if the
    // poller slips by several seconds under parallel test load, and a correct
    // group-kill takes the grandchild with it, so the marker never appears.
    let t0 = std::time::Instant::now();
    let out = r.h5i(&[
        "env",
        "run",
        "reap",
        "--",
        "sh",
        "-c",
        "sh -c 'sleep 8; echo alive > survivor.txt' & echo started; sleep 60",
    ]);
    assert!(
        !out.status.success(),
        "timed-out run should not report success"
    );

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

/// A wall-clock kill must surface as the conventional `timeout(1)` exit code
/// *124* (main.rs maps `outcome.timed_out` → `exit(124)`), so callers/CI can
/// distinguish "killed by the deadline" from an ordinary non-zero exit. The
/// reap test above only asserts `!success`; this pins the documented code.
/// Workspace tier, no kernel capabilities needed.
#[test]
fn wall_clock_kill_exits_with_code_124() {
    let r = Repo::new();
    std::fs::create_dir_all(r.dir.join(".h5i")).unwrap();
    std::fs::write(
        r.dir.join(".h5i/env.toml"),
        "[profile.default]\nisolation = \"workspace\"\nresources = { wall = \"1s\" }\n",
    )
    .unwrap();
    r.h5i_ok(&["env", "create", "deadline"]);

    let out = r.h5i(&["env", "run", "deadline", "--", "sh", "-c", "sleep 30"]);
    assert_eq!(
        out.status.code(),
        Some(124),
        "timed-out run must exit 124 (timeout convention):\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// `env shell` (the agent-in-box) runs an interactive, stdio-inherited session
/// inside the env: a command after `--` executes in `$WORK`, its exit code
/// passes through transparently, and the env returns to `idle` with a `shell`
/// event logged (nothing is captured: it's interactive). Workspace tier so it
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
    let out = r.h5i(&[
        "env",
        "shell",
        "box",
        "--",
        "sh",
        "-c",
        "echo hi > from-shell.txt",
    ]);
    assert!(
        out.status.success(),
        "shell command should succeed:\n{}",
        out_str(&out)
    );
    assert!(
        r.work("box").join("from-shell.txt").is_file(),
        "the shell session ran in $WORK"
    );

    // The child's exit code passes through transparently (transparent wrapper).
    let bad = r.h5i(&["env", "shell", "box", "--", "sh", "-c", "exit 7"]);
    assert_eq!(
        bad.status.code(),
        Some(7),
        "shell must pass the child exit code through"
    );

    // No capture is produced (interactive), but the env is back to idle.
    assert_eq!(
        r.manifest("box")["status"],
        "idle",
        "env returns to idle after a shell"
    );
}

/// `env shell --readonly` on the workspace tier must FAIL closed: that tier has
/// no mount namespace / Landlock to pin `$WORK` read-only, so an "observer"
/// there could still write the worktree. Refuse rather than lie. Always-runnable
/// (no kernel confinement needed to observe the refusal).
#[test]
fn env_shell_readonly_refused_on_workspace_tier() {
    let r = Repo::new();
    std::fs::create_dir_all(r.dir.join(".h5i")).unwrap();
    std::fs::write(
        r.dir.join(".h5i/env.toml"),
        "[profile.default]\nisolation = \"workspace\"\n",
    )
    .unwrap();
    r.h5i_ok(&["env", "create", "box"]);

    let out = r.h5i(&["env", "shell", "box", "--readonly", "--", "true"]);
    assert!(
        !out.status.success(),
        "readonly must be refused on the workspace tier:\n{}",
        out_str(&out)
    );
    let text = out_str(&out);
    assert!(
        text.contains("kernel-enforced") && text.contains("read-only"),
        "refusal must explain why (kernel-enforced worktree needed): {text}"
    );
    // Fail-closed: nothing ran, so no artifact and the env is untouched.
    assert!(!r.work("box").join("ran.txt").exists());
}

/// End-to-end on a kernel tier: a `--readonly` observer can READ the worktree
/// but every WRITE to `$WORK` is blocked, and the session leaves the env's
/// status untouched (no running/idle flip, no captures). Capability-gated.
#[test]
fn env_shell_readonly_pins_worktree_read_only() {
    if !process_tier_runnable() {
        eprintln!("SKIP env_shell_readonly_pins_worktree_read_only: process tier not runnable");
        return;
    }
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "obs", "--isolation", "process"]);
    let status_before = r.manifest("obs")["status"].clone();

    // Reads succeed: the seed file is visible read-only.
    let read = r.h5i(&[
        "env", "shell", "obs", "--readonly", "--", "sh", "-c", "cat lib.py",
    ]);
    assert!(
        read.status.success(),
        "an observer must be able to read $WORK:\n{}",
        out_str(&read)
    );

    // Writes are blocked: $WORK is read-only, so the redirection fails and the
    // file never appears.
    let write = r.h5i(&[
        "env",
        "shell",
        "obs",
        "--readonly",
        "--",
        "sh",
        "-c",
        "echo nope > blocked.txt",
    ]);
    assert!(
        !write.status.success(),
        "a write to $WORK must fail under --readonly:\n{}",
        out_str(&write)
    );
    assert!(
        !r.work("obs").join("blocked.txt").exists(),
        "no file may be written to the read-only worktree"
    );

    // The observer never mutated env state: status is exactly what it was.
    assert_eq!(
        r.manifest("obs")["status"],
        status_before,
        "a read-only observer must not change the env status"
    );
    // A normal (read-write) shell still works afterwards and CAN write.
    r.h5i_ok(&[
        "env", "shell", "obs", "--", "sh", "-c", "echo yes > allowed.txt",
    ]);
    assert!(r.work("obs").join("allowed.txt").is_file());
}

/// `env shell` on an already-local env must NOT eagerly sync the shared env
/// roster: it operates on one named env that is already materialized, so the
/// per-start `refs/h5i/env/meta` read + disk writes are pure overhead. A
/// synthetic (malicious `..`-path) manifest planted in the shared ref proves it:
/// an eager sync would print the "skipping shared env manifest" rejection and/or
/// materialize it, and neither may happen on the shell hot path.
#[test]
fn env_shell_existing_local_env_skips_shared_manifest_sync() {
    let r = Repo::new();
    std::fs::create_dir_all(r.dir.join(".h5i")).unwrap();
    std::fs::write(
        r.dir.join(".h5i/env.toml"),
        "[profile.default]\nisolation = \"workspace\"\n",
    )
    .unwrap();
    r.h5i_ok(&["env", "create", "box"]);

    let repo = git2::Repository::open(&r.dir).unwrap();
    let bad = synthetic_env_manifest(&repo, "..", "escape");
    append_synthetic_env_manifest(&repo, &bad);

    let out = r.h5i_ok(&["env", "shell", "box", "--", "sh", "-c", "true"]);
    let rendered = out_str(&out);
    assert!(
        !rendered.contains("skipping shared env manifest"),
        "local env shell should not eagerly sync shared manifests:\n{rendered}"
    );
    assert!(
        !r.dir.join(".git/.h5i/escape/manifest.json").exists(),
        "local env shell must not materialize unrelated shared manifests"
    );
}

/// `isolation=process` with `net.mode=host` must STILL confine the filesystem
/// (Landlock applies without a network namespace). Proving the always-create
/// user namespace works when egress is allowed. Capability-gated.
#[test]
fn process_tier_host_net_still_confines_fs() {
    if !process_tier_runnable() {
        eprintln!(
            "SKIP process_tier_host_net_still_confines_fs: process tier not runnable on this host"
        );
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
    r.h5i_ok(&[
        "env",
        "run",
        "hostnet",
        "--",
        "sh",
        "-c",
        "echo ok > in.txt",
    ]);
    assert!(r.work("hostnet").join("in.txt").is_file());
    // … outside $WORK still blocked.
    let escape = r.dir.join("hostnet-escape.txt");
    let out = r.h5i(&[
        "env",
        "run",
        "hostnet",
        "--",
        "sh",
        "-c",
        &format!("echo x > {}", escape.display()),
    ]);
    assert!(!out.status.success());
    assert!(
        !escape.exists(),
        "host-net env must still confine the filesystem"
    );
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
        .args([
            "env",
            "run",
            "envjail",
            "--",
            "sh",
            "-c",
            "echo SECRET=[$MY_SECRET] PATH_SET=${PATH:+yes}",
        ])
        .env("H5I_AGENT", "tester")
        .env("MY_SECRET", "hunter2")
        .current_dir(&r.dir)
        .output()
        .unwrap();
    let text = out_str(&out);
    assert!(
        text.contains("SECRET=[]"),
        "secrets must not be inherited: {text}"
    );
    assert!(
        text.contains("PATH_SET=yes"),
        "allowlisted PATH must pass: {text}"
    );
}

/// `resources.fsize` caps any single file the confined command writes. A
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
        "env",
        "run",
        "bomb",
        "--",
        "sh",
        "-c",
        "head -c 8388608 /dev/zero > big.bin",
    ]);
    assert!(
        !out.status.success(),
        "writing past the fsize cap must fail"
    );
    let big = r.work("bomb").join("big.bin");
    if big.exists() {
        let sz = std::fs::metadata(&big).unwrap().len();
        assert!(
            sz <= 2 * 1024 * 1024,
            "file should be capped near 1 MiB, got {sz} bytes"
        );
    }
}

/// The PID-namespace jail (design §5 "PID view"): a confined process must not be
/// able to see, or read the `/proc/<pid>/environ` of, host processes. Without
/// it, a build script at the `process` tier could dump the operator's whole
/// environment (every host secret) straight out of `/proc`, defeating the
/// `env.pass` allowlist. Capability-gated.
#[test]
fn process_tier_pid_namespace_hides_host_processes_and_environ() {
    // PID namespaces are a Linux primitive. Darwin has none, and h5i does not
    // pretend otherwise: `box probe` reports mechanism=seatbelt, and the
    // process-hiding property is carried there by `(deny process-info*
    // (target others))` instead. Asserting "the workload is PID 1" on macOS
    // tests a mechanism the platform never claimed.
    if cfg!(target_os = "macos") {
        eprintln!("skipping: no PID namespaces on macOS (Seatbelt tier)");
        return;
    }
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

    // Control: on the host, the same uid can usually read the victim's environ.
    // Proving the secret is genuinely exposed there. Retry briefly: the new env
    // only lands after the child's execve completes. (Some hosts set
    // yama ptrace_scope=2 and forbid it even same-uid; we don't require it: the
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
    // /proc entry, and the secret, is unreachable.
    let out = r.h5i(&[
        "env",
        "run",
        "pidjail",
        "--",
        "sh",
        "-c",
        &format!("cat /proc/{vpid}/environ 2>&1 | tr '\\0' '\\n'; echo DONE"),
    ]);
    let leaked = out_str(&out);

    // The workload is PID 1 of its own namespace ($$ == 1 proves the fresh pidns).
    let pid_out = r.h5i(&["env", "run", "pidjail", "--", "sh", "-c", "echo $$"]);
    let pid_txt = out_str(&pid_out);

    // The box sees only its own namespace's handful of pids, not the host's many.
    let count_out = r.h5i(&[
        "env",
        "run",
        "pidjail",
        "--",
        "sh",
        "-c",
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

/// The supervised tier is the one that claims untrusted-code containment, so it
/// gets the PID namespace too, and the property that matters most there is not
/// visibility but *reach*. The box's user namespace maps back to the operator's
/// real uid, so without a PID namespace a `kill -9` from inside the box lands on
/// any host process that user owns: their editor, their build, the h5i process
/// supervising the box.
///
/// (`/proc/<pid>/environ` was never readable here, the userns already failing
/// `ptrace_may_access`, but argv, process enumeration and signals were.)
#[test]
fn supervised_tier_cannot_see_or_signal_host_processes() {
    if !supervised_tier_runnable() {
        eprintln!("SKIP supervised_tier_cannot_signal...: supervised tier not runnable here");
        return;
    }
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "svjail", "--isolation", "supervised"]);

    let mut victim = Command::new("sleep")
        .arg("120")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn victim host process");
    let vpid = victim.id();

    // Reach: the box must not be able to signal a host process of the same uid.
    let killed = r.h5i(&[
        "env",
        "run",
        "svjail",
        "--",
        "sh",
        "-c",
        &format!("kill -9 {vpid} 2>&1; echo SENT"),
    ]);
    let kill_txt = out_str(&killed);

    // Visibility: the host's own pids are not in the box's namespace at all.
    let seen = r.h5i(&[
        "env",
        "run",
        "svjail",
        "--",
        "sh",
        "-c",
        &format!("test -e /proc/{vpid} && echo VISIBLE || echo hidden"),
    ]);
    let seen_txt = out_str(&seen);

    std::thread::sleep(std::time::Duration::from_millis(200));
    let survived = victim.try_wait().ok().flatten().is_none();
    let _ = victim.kill();
    let _ = victim.wait();

    assert!(
        survived,
        "a supervised box killed a HOST process — the tier claims containment it does not have:\n{kill_txt}"
    );
    assert!(
        seen_txt.lines().any(|l| l.trim() == "hidden"),
        "a host process must not exist in the box's PID namespace, got:\n{seen_txt}"
    );
}

/// The supervised tier's egress allowlist has to keep working with the PID
/// namespace in place, and the interaction is genuinely delicate: the netns
/// handshake forks an `nft` helper, and if that helper is the first child after
/// `CLONE_NEWPID` it becomes the namespace's init, exits, and leaves a dead
/// namespace in which the workload's own fork fails with `ENOMEM`. This proves
/// the ordering (unshare the pidns *after* the helper has come and gone).
#[test]
fn supervised_egress_still_works_with_a_pid_namespace() {
    // Same as the process-tier PID-namespace test: the namespace half of this
    // does not exist on Darwin. macOS egress is covered by
    // `supervised_enforces_runtime_confinement`.
    if cfg!(target_os = "macos") {
        eprintln!("skipping: no PID namespaces on macOS (Seatbelt tier)");
        return;
    }
    if !supervised_tier_runnable() {
        eprintln!("SKIP supervised_egress_still_works...: supervised tier not runnable here");
        return;
    }
    let r = Repo::new();
    // `agent` declares a net.egress allowlist, so this exercises the nft +
    // slirp4netns path rather than the airtight empty-netns one.
    let created = r.h5i(&[
        "env",
        "create",
        "svegress",
        "--isolation",
        "supervised",
        "--profile",
        "agent",
    ]);
    if !created.status.success() {
        eprintln!("SKIP: no egress-capable supervised box here:\n{}", out_str(&created));
        return;
    }
    // The workload must actually start and be PID 1 of its own namespace. A dead
    // namespace shows up as a spawn failure, which is exactly what we are
    // guarding against.
    let out = r.h5i(&["env", "run", "svegress", "--", "sh", "-c", "echo $$"]);
    let txt = out_str(&out);
    assert!(
        out.status.success(),
        "an egress-enabled supervised run must start (a dead pidns fails with ENOMEM):\n{txt}"
    );
    assert!(
        txt.lines().any(|l| l.trim() == "1"),
        "the workload must be PID 1 of a fresh namespace, got:\n{txt}"
    );
}

/// The PID-namespace jail mounts a *fresh* procfs, which shadows the host `/proc`
/// the pre-fork Landlock grant pinned. This proves the in-child re-grant works:
/// the workload can still read its own `/proc/self/*` (otherwise every confined
/// command that touches /proc would break). Capability-gated.
#[test]
fn process_tier_proc_self_is_readable_under_pid_namespace() {
    // There is no procfs on Darwin at all, so there is no freshly-mounted
    // /proc for the workload to read.
    if cfg!(target_os = "macos") {
        eprintln!("skipping: macOS has no procfs");
        return;
    }
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

/// Whether to run the real-container tests. They are *opt-in* via
/// `H5I_TEST_CONTAINER=1`: they pull an image and (for egress) make a live
/// network call, so we never run them implicitly in CI. Where podman may be
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
    r.h5i_ok(&[
        "env",
        "run",
        "box",
        "--",
        "sh",
        "-c",
        "echo from-container > made.txt",
    ]);
    let made = r.work("box").join("made.txt");
    assert!(made.is_file(), "container wrote into the mounted workspace");
    assert_eq!(
        std::fs::read_to_string(&made).unwrap().trim(),
        "from-container"
    );

    // net.mode=deny → no egress.
    let out = r.h5i(&[
        "env",
        "run",
        "box",
        "--",
        "sh",
        "-c",
        "wget -T3 -q -O- http://example.com >/dev/null 2>&1 && echo REACHED || echo BLOCKED",
    ]);
    assert!(
        out_str(&out).contains("BLOCKED"),
        "net deny must block egress: {}",
        out_str(&out)
    );

    // The capture records the container claim in the manifest.
    assert_eq!(r.manifest("box")["isolation_claim"], "container");
}

/// In-container git plumbing: a worktree's `.git` pointer files name
/// host-absolute paths, so the backend bind-mounts the env's plumbing at
/// *identical* paths inside the box (`env::box_git_plumbing`). Busybox ships
/// no git binary, so this proves the mount surface directly: the pointer
/// chain resolves, `objects` is writable, `config` is read-only, hooks stay
/// unreachable.
#[test]
fn container_box_git_plumbing_mounted_at_host_paths() {
    if !container_runnable() {
        eprintln!("SKIP container_box_git_plumbing_mounted_at_host_paths: no rootless podman");
        return;
    }
    let r = Repo::new();
    write_profile(
        &r,
        &format!(
            "[profile.default]\nisolation = \"container\"\nnet.mode = \"deny\"\ncontainer.image = \"{BUSYBOX}\"\n"
        ),
    );
    r.h5i_ok(&["env", "create", "boxc"]);
    let g = r.dir.join(".git");
    let admin = g.join("worktrees/h5i-env-tester-boxc");

    // The whole pointer chain is resolvable from inside: worktree admin dir,
    // shared HEAD/config/objects, and $WORK dual-mounted at its host path
    // (the admin `gitdir` back-pointer names it).
    let out = r.h5i(&[
        "env",
        "run",
        "boxc",
        "--",
        "sh",
        "-c",
        &format!(
            "test -f {a}/commondir && test -r {g}/HEAD && test -r {g}/config && \
         test -d {g}/objects && test -f {w}/.git && echo PLUMB-OK || echo PLUMB-MISSING",
            a = admin.display(),
            g = g.display(),
            w = r.work("boxc").display(),
        ),
    ]);
    assert!(
        out_str(&out).contains("PLUMB-OK"),
        "git plumbing must be mounted: {}",
        out_str(&out)
    );

    // objects is writable (commits need it) …
    let out = r.h5i(&[
        "env",
        "run",
        "boxc",
        "--",
        "sh",
        "-c",
        &format!(
            "touch {g}/objects/h5i-probe && rm {g}/objects/h5i-probe && echo OBJ-RW || echo OBJ-RO",
            g = g.display(),
        ),
    ]);
    assert!(
        out_str(&out).contains("OBJ-RW"),
        "objects must be rw: {}",
        out_str(&out)
    );

    // … while config is read-only and hooks unreachable (never mounted).
    let out = r.h5i(&[
        "env",
        "run",
        "boxc",
        "--",
        "sh",
        "-c",
        &format!(
            "(echo x >> {g}/config) 2>/dev/null && echo CFG-RW || echo CFG-RO; \
         (touch {g}/hooks/pre-commit) 2>/dev/null && echo HOOK-PLANTED || echo HOOK-BLOCKED",
            g = g.display(),
        ),
    ]);
    let text = out_str(&out);
    assert!(
        text.contains("CFG-RO") && !text.contains("CFG-RW"),
        "config must be ro: {text}"
    );
    assert!(
        text.contains("HOOK-BLOCKED") && !text.contains("HOOK-PLANTED"),
        "hooks must stay unreachable: {text}"
    );
    assert!(
        !g.join("hooks/pre-commit").exists(),
        "no hook may appear on the host"
    );
}

/// The container agent-in-box session injects the wrap-bash hook as Claude
/// *managed settings*, read-only, at the unoverridable managed-settings path.
/// The in-box agent cannot write it (root-owned path + ro mount) and, per
/// Claude's merge rules, cannot disable a managed hook from its own config, so
/// in-box command observation cannot be silenced. (`env shell` is the agent
/// path; `env run` does not inject it.)
#[test]
fn container_injects_managed_settings_hook_read_only() {
    if !container_runnable() {
        eprintln!("SKIP container_injects_managed_settings_hook_read_only: no rootless podman");
        return;
    }
    let r = Repo::new();
    write_profile(
        &r,
        &format!(
            "[profile.default]\nisolation = \"container\"\nnet.mode = \"deny\"\ncontainer.image = \"{BUSYBOX}\"\n"
        ),
    );
    r.h5i_ok(&["env", "create", "boxm"]);

    // The managed-settings file is present at the exact path, carries the
    // wrap-bash hook, and is read-only inside the box.
    let out = r.h5i(&[
        "env",
        "shell",
        "boxm",
        "--",
        "sh",
        "-c",
        "cat /etc/claude-code/managed-settings.json; echo ---; \
         (echo x >> /etc/claude-code/managed-settings.json) 2>/dev/null && echo MS-RW || echo MS-RO",
    ]);
    let text = out_str(&out);
    assert!(
        text.contains("h5i hook wrap-bash"),
        "managed hook must be present: {text}"
    );
    assert!(
        text.contains("PreToolUse"),
        "managed hook must target PreToolUse: {text}"
    );
    assert!(
        text.contains("MS-RO") && !text.contains("MS-RW"),
        "managed settings must be read-only in-box: {text}"
    );
    // The host's real managed-settings path is never touched (mount is ns-local).
    assert!(
        !std::path::Path::new("/etc/claude-code/managed-settings.json").exists()
            || std::fs::read_to_string("/etc/claude-code/managed-settings.json")
                .map(|s| !s.contains("h5i hook wrap-bash"))
                .unwrap_or(true),
        "host managed-settings must not be created/modified by the box"
    );
}

/// Container tier: the in-box capture spool is mounted at `/.h5i/spool` (rw,
/// despite the read-only rootfs) and the host ingests what the box writes into
/// it. We write a synthetic `inbox-capture` record from inside the box,
/// sidestepping the need for a glibc-matched `h5i` binary in the image, and
/// prove the mount + host-side ingest end-to-end on container.
#[test]
fn container_env_capture_spool_is_mounted_and_ingested() {
    if !container_runnable() {
        eprintln!("SKIP container_env_capture_spool_is_mounted_and_ingested: no rootless podman");
        return;
    }
    let r = Repo::new();
    write_profile(
        &r,
        &format!(
            "[profile.default]\nisolation = \"container\"\nnet.mode = \"deny\"\ncontainer.image = \"{BUSYBOX}\"\n"
        ),
    );
    r.h5i_ok(&["env", "create", "cspool"]);

    // The box writes a well-formed inbox-capture pair into the mounted spool
    // (what an in-box `h5i capture run` would stage). The rootfs is read-only;
    // /.h5i/spool must be writable because it's a bind mount.
    r.h5i_ok(&[
        "env", "run", "cspool", "--", "sh", "-c",
        "printf '%s' '{\"cmd\":\"echo boxed\",\"cwd\":null,\"exit_code\":0,\"files\":[],\"cmd_argv\":[\"echo\",\"boxed\"]}' \
           > /.h5i/spool/cap-7-0.json && \
         printf 'boxed-output' > /.h5i/spool/cap-7-0.raw && echo staged",
    ]);

    // The host ingested it: env now has the host-env-run capture (the run
    // itself) AND the synthetic inbox-capture.
    let env_manifest = r.manifest("cspool");
    assert!(
        env_manifest["captures"].as_array().unwrap().len() >= 2,
        "host-env-run + ingested inbox-capture: {env_manifest}"
    );
    let inbox = r
        .receipts("cspool")
        .into_iter()
        .find(|m| m["source"] == "inbox-capture")
        .expect("an inbox-capture receipt");
    let raw = r.capture_raw_for("cspool", inbox["id"].as_str().unwrap());
    assert!(
        String::from_utf8_lossy(&raw).contains("boxed-output"),
        "{inbox}"
    );

    let status = out_str(&r.h5i_ok(&["env", "status", "cspool"]));
    assert!(status.contains("inbox-capture=1"), "{status}");
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
        "env",
        "run",
        "egr",
        "--",
        "sh",
        "-c",
        "wget -T8 -q -O- http://example.com | grep -qi 'example domain' && echo OK || echo FAIL",
    ]);
    assert!(
        out_str(&allowed).contains("OK"),
        "allowlisted host must be reachable: {}",
        out_str(&allowed)
    );

    // A non-allowlisted host is blocked (fail-closed at the proxy).
    let denied = r.h5i(&[
        "env",
        "run",
        "egr",
        "--",
        "sh",
        "-c",
        "wget -T8 -q -O- http://www.google.com >/dev/null 2>&1 && echo REACHED || echo BLOCKED",
    ]);
    assert!(
        out_str(&denied).contains("BLOCKED"),
        "non-allowlisted host must be blocked: {}",
        out_str(&denied)
    );
}

// ─── 8. secret redaction in evidence (design §7) ────────────────────────────

const PLANTED_SECRET: &str = "ghp_0123456789012345678901234567890123ab";

#[test]
fn run_redacts_secrets_from_evidence_blob_summary_and_command() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "leaky"]);
    // The secret appears both in the OUTPUT and in the command line itself.
    r.h5i_ok(&[
        "env",
        "run",
        "leaky",
        "--",
        "sh",
        "-c",
        &format!("echo token={PLANTED_SECRET}"),
    ]);

    let m = r.capture_manifest("leaky");
    // The detected rule is recorded (by id, never the value).
    let redactions = m["redactions"].as_array().expect("redactions array");
    assert!(
        redactions.iter().any(|v| v == "GITHUB_PAT"),
        "expected GITHUB_PAT in redactions: {m}"
    );
    // The secret must not survive ANYWHERE in the record …
    let record_line = serde_json::to_string(&m).unwrap();
    assert!(
        !record_line.contains(PLANTED_SECRET),
        "secret leaked into the receipt: {record_line}"
    );
    // … including the command field (it was passed as an argument).
    assert!(
        !m["cmd"].as_str().unwrap().contains(PLANTED_SECRET),
        "secret leaked into cmd"
    );

    // … and not in the stored payload.
    let raw = r.capture_raw_for("leaky", m["id"].as_str().unwrap());
    let raw_str = String::from_utf8_lossy(&raw);
    assert!(
        !raw_str.contains(PLANTED_SECRET),
        "secret leaked into raw blob: {raw_str}"
    );
    assert!(
        raw_str.contains("redacted"),
        "redaction marker expected in raw: {raw_str}"
    );
}

#[test]
fn inspect_renders_a_capture_and_refuses_foreign_ones() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "one"]);
    r.h5i_ok(&["env", "create", "two"]);
    r.h5i_ok(&["env", "run", "one", "--", "sh", "-c", "echo hello-from-one"]);
    let cap = r.manifest("one")["captures"][0]
        .as_str()
        .unwrap()
        .to_string();

    // Inspect from the owning env: renders the capture.
    let out = out_str(&r.h5i_ok(&["env", "inspect", "one", "--capture", &cap]));
    assert!(out.contains(&cap), "{out}");
    assert!(out.contains("exit"), "{out}");

    // Inspecting the SAME capture id from a different env is refused. Evidence
    // is scoped to its environment. The error names both envs so a reviewer can
    // see whose capture it actually is.
    let out = r.h5i(&["env", "inspect", "two", "--capture", &cap]);
    assert!(!out.status.success(), "cross-env inspect must be refused");
    let err = out_str(&out);
    assert!(err.contains("not evidence for"), "{err}");
    assert!(
        err.contains("env/tester/one"),
        "names the owning env: {err}"
    );
}

/// `inspect` renders every header field the design promises (the capture id +
/// env id, the command, a non-zero exit code (verbatim, not masked), the policy
/// digest, the evidence source, the raw object accounting) followed by the
/// structured findings (here the generic-command body carrying the output).
#[test]
fn inspect_renders_all_capture_fields() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "fields"]);
    // A non-zero exit so we prove the code is rendered verbatim, plus a known
    // stdout line we expect to surface in the rendered body.
    r.h5i(&[
        "env",
        "run",
        "fields",
        "--",
        "sh",
        "-c",
        "echo body-marker; exit 3",
    ]);

    let env_m = r.manifest("fields");
    let cap = env_m["captures"][0].as_str().unwrap().to_string();
    let digest = env_m["policy_digest"].as_str().unwrap();

    let out = out_str(&r.h5i_ok(&["env", "inspect", "fields", "--capture", &cap]));
    // Header pairs the capture with its owning env.
    assert!(out.contains(&cap), "header has the capture id: {out}");
    assert!(
        out.contains("env/tester/fields"),
        "header has the env id: {out}"
    );
    // The command line, exactly as run.
    assert!(out.contains("cmd"), "{out}");
    assert!(
        out.contains("echo body-marker"),
        "renders the command: {out}"
    );
    // A non-zero exit is shown verbatim, never masked.
    assert!(
        out.contains("exit") && out.contains("3"),
        "renders exit 3: {out}"
    );
    // Policy digest is shown (truncated to its 12-char prefix).
    assert!(
        out.contains(&digest[..12]),
        "renders policy digest prefix: {out}"
    );
    // Provenance: a host-driven env run.
    assert!(
        out.contains("host-env-run"),
        "renders evidence source: {out}"
    );
    // Raw object accounting.
    assert!(
        out.contains("bytes") && out.contains("lines"),
        "renders raw size accounting: {out}"
    );
    // The structured findings render the captured stdout.
    assert!(
        out.contains("body-marker"),
        "renders the captured output: {out}"
    );
}

/// `inspect --json` emits the same stored manifest that backs the human view,
/// after applying the same env ownership check. Tooling can consume it without
/// scraping the pretty renderer, while cross-env evidence remains refused.
#[test]
fn inspect_json_renders_the_capture_manifest() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "json"]);
    r.h5i_ok(&["env", "create", "other"]);
    r.h5i_ok(&["env", "run", "json", "--", "sh", "-c", "echo json-body"]);

    let env_m = r.manifest("json");
    let cap = env_m["captures"][0].as_str().unwrap().to_string();
    let digest = env_m["policy_digest"].as_str().unwrap();

    let out = r.h5i_ok(&["env", "inspect", "json", "--capture", &cap, "--json"]);
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("inspect --json is valid JSON");

    assert_eq!(v["id"].as_str(), Some(cap.as_str()), "{v:#}");
    assert_eq!(v["env_id"].as_str(), Some("env/tester/json"), "{v:#}");
    assert_eq!(v["exit_code"].as_i64(), Some(0), "{v:#}");
    assert_eq!(v["policy_digest"].as_str(), Some(digest), "{v:#}");
    assert!(
        v["raw_oid"]
            .as_str()
            .is_some_and(|oid| oid.starts_with("sha256:")),
        "{v:#}"
    );
    assert_eq!(v["source"].as_str(), Some("host-env-run"), "{v:#}");
    // The command's output rides in the stored payload, not in the record.
    let payload = String::from_utf8_lossy(&r.capture_raw_for("json", &cap)).into_owned();
    assert!(payload.contains("json-body"), "{payload}");

    let out = r.h5i(&["env", "inspect", "other", "--capture", &cap, "--json"]);
    assert!(!out.status.success(), "cross-env JSON inspect must be refused");
    assert!(out_str(&out).contains("not evidence for"), "{}", out_str(&out));
}

/// A capture handle that resolves to nothing, and one too short to be a prefix,
/// both fail loudly with an actionable message rather than rendering an empty or
/// wrong capture.
#[test]
fn inspect_refuses_unknown_and_too_short_handles() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "handles"]);
    r.h5i_ok(&["env", "run", "handles", "--", "sh", "-c", "echo hi"]);

    // Nonexistent capture id.
    let out = r.h5i(&["env", "inspect", "handles", "--capture", "cap-nope-zzzz"]);
    assert!(!out.status.success(), "unknown capture must be refused");
    assert!(
        out_str(&out).contains("no object matches"),
        "{}",
        out_str(&out)
    );

    // A handle shorter than the 4-char prefix floor.
    let out = r.h5i(&["env", "inspect", "handles", "--capture", "ab"]);
    assert!(!out.status.success(), "too-short handle must be refused");
    assert!(out_str(&out).contains("too short"), "{}", out_str(&out));
}

/// `inspect` re-renders a capture whose evidence contained a secret. The
/// redacted rule is surfaced (so a reviewer knows redaction fired), the secret
/// value appears nowhere in the rendered view, and the placeholder shows in its
/// place: `inspect` must never become a side channel back to the raw secret.
#[test]
fn inspect_surfaces_redactions_without_leaking_the_secret() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "redact"]);
    r.h5i_ok(&[
        "env",
        "run",
        "redact",
        "--",
        "sh",
        "-c",
        &format!("echo token={PLANTED_SECRET}"),
    ]);
    let cap = r.manifest("redact")["captures"][0]
        .as_str()
        .unwrap()
        .to_string();

    let out = out_str(&r.h5i_ok(&["env", "inspect", "redact", "--capture", &cap]));
    // The detected rule is named (by id, never the value).
    assert!(out.contains("GITHUB_PAT"), "names the redacted rule: {out}");
    // The secret value must not survive anywhere in the rendered view.
    assert!(
        !out.contains(PLANTED_SECRET),
        "secret leaked through inspect: {out}"
    );
    // The redaction placeholder shows where the value was.
    assert!(
        out.contains("redacted"),
        "shows the redaction marker: {out}"
    );
}

/// `inspect` resolves the env by its fully-qualified id (not just the bare slug)
/// and resolves the capture from a hex prefix. The same ergonomics the object
/// store offers elsewhere, so a reviewer can paste a short handle.
#[test]
fn inspect_resolves_env_by_full_id_and_capture_by_prefix() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "resolve"]);
    r.h5i_ok(&["env", "run", "resolve", "--", "sh", "-c", "echo resolved"]);
    let cap = r.manifest("resolve")["captures"][0]
        .as_str()
        .unwrap()
        .to_string();
    let prefix = &cap[..8.min(cap.len())];

    // Env addressed by its full id, capture by an 8-char prefix.
    let out = out_str(&r.h5i_ok(&["env", "inspect", "env/tester/resolve", "--capture", prefix]));
    assert!(
        out.contains(&cap),
        "prefix resolved to the full capture: {out}"
    );
    assert!(out.contains("env/tester/resolve"), "{out}");
    assert!(
        out.contains("resolved"),
        "renders the captured output: {out}"
    );
}

// ─── 9. concurrency: the run-lock serializes runs of one env ────────────────

#[test]
fn concurrent_runs_of_one_env_are_serialized() {
    use std::process::Stdio;
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "busy"]);

    // Launch a slow run in the background; it holds the run-lock for ~2s.
    let mut slow = Command::new(H5I)
        .args([
            "env",
            "run",
            "busy",
            "--",
            "sh",
            "-c",
            "sleep 2; echo slow-done",
        ])
        .env("H5I_AGENT", "tester")
        .current_dir(&r.dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn slow run");

    // Give it a moment to take the lock, then a second run must be refused.
    std::thread::sleep(std::time::Duration::from_millis(400));
    let contender = r.h5i(&["env", "run", "busy", "--", "sh", "-c", "echo fast"]);
    assert!(
        !contender.status.success(),
        "second concurrent run must be refused"
    );
    assert!(
        out_str(&contender).contains("busy"),
        "{}",
        out_str(&contender)
    );

    assert!(slow.wait().unwrap().success());
    // After the lock is released, a new run succeeds.
    r.h5i_ok(&["env", "run", "busy", "--", "sh", "-c", "echo after"]);
}

#[test]
fn propose_refuses_while_run_is_active_and_does_not_clobber_status() {
    use std::process::Stdio;
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "race"]);

    let mut slow = Command::new(H5I)
        .args([
            "env",
            "run",
            "race",
            "--",
            "sh",
            "-c",
            "echo from-run > slow.txt; sleep 2",
        ])
        .env("H5I_AGENT", "tester")
        .env("H5I_DEFAULT_ISOLATION", "workspace")
        .current_dir(&r.dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn slow run");

    let mut saw_running = false;
    for _ in 0..50 {
        if r.manifest("race")["status"] == "running" {
            saw_running = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(saw_running, "slow run should enter running state");

    let out = r.h5i(&["env", "propose", "race"]);
    assert!(
        !out.status.success(),
        "propose must fail while env run holds the lock"
    );
    assert!(
        out_str(&out).contains("busy"),
        "expected busy refusal:\n{}",
        out_str(&out)
    );

    assert!(slow.wait().unwrap().success());
    assert_eq!(
        r.manifest("race")["status"],
        "idle",
        "failed propose must not leave the env proposed or clobber the run completion"
    );

    let proposed = out_str(&r.h5i_ok(&["env", "propose", "race"]));
    assert!(proposed.contains("Proposal: env/tester/race"), "{proposed}");
    assert_eq!(r.manifest("race")["status"], "proposed");
}

// ─── 10. event log is secret-safe and carries resource accounting ───────────

#[test]
fn event_log_redacts_command_and_records_resources() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "acct"]);
    r.h5i_ok(&[
        "env",
        "run",
        "acct",
        "--",
        "sh",
        "-c",
        &format!("echo deploying with {PLANTED_SECRET}"),
    ]);

    // The raw event log blob (refs/h5i/env) must not leak the secret passed on
    // the command line, and must carry wall/cpu resource accounting.
    let log = out_str(&git(&r.dir, &["show", "refs/h5i/env/meta:events.jsonl"]));
    assert!(
        !log.contains(PLANTED_SECRET),
        "secret leaked into the env event log: {log}"
    );
    assert!(
        log.contains("redacted"),
        "command should be redacted in the event detail"
    );
    let exec_line = log
        .lines()
        .find(|l| l.contains("\"event\":\"exec\""))
        .expect("exec event");
    assert!(
        exec_line.contains("wall="),
        "exec event must record wall time: {exec_line}"
    );
    assert!(
        exec_line.contains("cpu="),
        "exec event must record cpu time: {exec_line}"
    );

    // The CLI run line surfaces resources too.
    let out = out_str(&r.h5i_ok(&["env", "run", "acct", "--", "sh", "-c", "true"]));
    assert!(
        out.contains("wall "),
        "run output should show wall time: {out}"
    );
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
    let out = r.h5i(&[
        "env",
        "run",
        "pinned",
        "--",
        "sh",
        "-c",
        "echo nope > escaped.txt",
    ]);
    assert!(!out.status.success(), "unlisted command must be refused");
    assert!(out_str(&out).contains("allowlist"), "{}", out_str(&out));
    assert!(
        !r.work("pinned").join("escaped.txt").exists(),
        "refused command must not run"
    );
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
    // cand-b's run fails on purpose. Exit code passes through, so it's not _ok.
    let failed = r.h5i(&["env", "run", "cand-b", "--", "sh", "-c", "exit 2"]);
    assert_eq!(failed.status.code(), Some(2));

    let out = out_str(&r.h5i_ok(&["env", "compare", "cand-a", "cand-b"]));
    assert!(
        out.contains("common base"),
        "shared-base envs report a common base: {out}"
    );
    assert!(out.contains("env/tester/cand-a"), "{out}");
    assert!(out.contains("env/tester/cand-b"), "{out}");
    assert!(out.contains("exit 0"), "cand-a's passing run shows: {out}");
    assert!(out.contains("exit 2"), "cand-b's failing run shows: {out}");

    // JSON form is machine-readable with the diffstat numbers.
    let json = out_str(&r.h5i_ok(&["env", "compare", "cand-a", "cand-b", "--json"]));
    let rows: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 2);
    let b = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "env/tester/cand-b")
        .unwrap();
    assert_eq!(b["insertions"], 3, "untracked-file lines counted: {json}");
    assert_eq!(b["last_exit"], 2);
}

#[test]
fn compare_warns_when_bases_differ() {
    let r = Repo::new();
    let first = out_str(&git(&r.dir, &["rev-parse", "HEAD"]))
        .trim()
        .to_string();
    r.h5i_ok(&["env", "create", "from-old", "--from", &first]);
    // Advance main, then create a second env off the new tip.
    std::fs::write(r.dir.join("moved.txt"), "moved\n").unwrap();
    git(&r.dir, &["add", "moved.txt"]);
    git(&r.dir, &["commit", "-m", "advance"]);
    r.h5i_ok(&["env", "create", "from-new"]);

    let out = out_str(&r.h5i_ok(&["env", "compare", "from-old", "from-new"]));
    assert!(
        out.contains("do NOT share a base"),
        "must warn on split bases: {out}"
    );
}

// ─── 13. base drift + rebase (§9) ───────────────────────────────────────────

#[test]
fn status_reports_drift_and_rebase_refreshes_the_base() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "drifter"]);
    let base0 = out_str(&git(&r.dir, &["rev-parse", "HEAD"]))
        .trim()
        .to_string();

    // No drift initially.
    let st = out_str(&r.h5i_ok(&["env", "status", "drifter"]));
    assert!(st.contains("up to date with parent"), "{st}");
    assert!(
        st.contains(&base0[..12]),
        "status shows the pinned base: {st}"
    );

    // The env makes a change on a disjoint file …
    std::fs::write(r.work("drifter").join("env.txt"), "from env\n").unwrap();
    // … while the parent advances on another file.
    std::fs::write(r.dir.join("lib.py"), "def hello():\n    return 99\n").unwrap();
    git(&r.dir, &["add", "lib.py"]);
    git(&r.dir, &["commit", "-m", "parent moves"]);
    let base1 = out_str(&git(&r.dir, &["rev-parse", "HEAD"]))
        .trim()
        .to_string();

    // status (and the JSON manifest's base) now show drift.
    let st = out_str(&r.h5i_ok(&["env", "status", "drifter"]));
    assert!(
        st.contains("parent advanced 1 commit"),
        "drift surfaced: {st}"
    );

    // Rebase folds the parent's change in and re-pins the base.
    let out = out_str(&r.h5i_ok(&["env", "rebase", "drifter"]));
    assert!(out.contains("rebased onto main"), "{out}");
    assert_eq!(
        r.manifest("drifter")["base_commit"].as_str().unwrap(),
        base1,
        "base re-pinned"
    );

    // Worktree now carries BOTH sides; drift is cleared.
    let lib = std::fs::read_to_string(r.work("drifter").join("lib.py")).unwrap();
    assert!(
        lib.contains("return 99"),
        "parent's change folded in: {lib}"
    );
    assert!(
        r.work("drifter").join("env.txt").is_file(),
        "env's change preserved"
    );
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
    let base0 = out_str(&git(&r.dir, &["rev-parse", "HEAD"]))
        .trim()
        .to_string();

    // Both the env and the parent edit the same file differently.
    std::fs::write(r.work("clash").join("README.md"), "env version\n").unwrap();
    std::fs::write(r.dir.join("README.md"), "parent version\n").unwrap();
    git(&r.dir, &["add", "README.md"]);
    git(&r.dir, &["commit", "-m", "parent readme"]);

    let out = r.h5i(&["env", "rebase", "clash"]);
    assert!(!out.status.success(), "conflicting rebase must refuse");
    assert!(
        out_str(&out).contains("conflicts against the new base"),
        "{}",
        out_str(&out)
    );
    // The base is untouched after a refused rebase.
    assert_eq!(r.manifest("clash")["base_commit"].as_str().unwrap(), base0);
    // The env is still rebase-able (status unchanged). Refusal is not a dead end.
    assert_eq!(r.manifest("clash")["status"].as_str().unwrap(), "created");
}

#[test]
fn rebase_is_a_noop_when_up_to_date() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "still"]);
    let base0 = r.manifest("still")["base_commit"]
        .as_str()
        .unwrap()
        .to_string();

    // Parent never advanced. Nothing to fold.
    let out = out_str(&r.h5i_ok(&["env", "rebase", "still"]));
    assert!(
        out.contains("nothing to rebase"),
        "no-op rebase reports it: {out}"
    );
    // Base + status untouched by the no-op.
    assert_eq!(r.manifest("still")["base_commit"].as_str().unwrap(), base0);
    assert_eq!(r.manifest("still")["status"].as_str().unwrap(), "created");
}

#[test]
fn rebase_is_refused_after_propose() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "locked"]);
    std::fs::write(r.work("locked").join("env.txt"), "from env\n").unwrap();

    // Advance the parent so a rebase would otherwise have work to do …
    std::fs::write(r.dir.join("lib.py"), "def hello():\n    return 99\n").unwrap();
    git(&r.dir, &["add", "lib.py"]);
    git(&r.dir, &["commit", "-m", "parent moves"]);
    let base_pinned = r.manifest("locked")["base_commit"]
        .as_str()
        .unwrap()
        .to_string();

    // … but proposing crosses the line into review: rebase is no longer valid.
    r.h5i_ok(&["env", "propose", "locked"]);
    assert_eq!(r.manifest("locked")["status"].as_str().unwrap(), "proposed");

    let out = r.h5i(&["env", "rebase", "locked"]);
    assert!(!out.status.success(), "rebase after propose must refuse");
    assert!(
        out_str(&out).contains("only valid before propose/apply"),
        "{}",
        out_str(&out)
    );
    // The proposed state's base is left exactly as pinned.
    assert_eq!(
        r.manifest("locked")["base_commit"].as_str().unwrap(),
        base_pinned
    );
}

#[test]
fn rebase_refuses_when_parent_branch_is_gone() {
    let r = Repo::new();
    // Create the env off a side branch so we can later delete its parent.
    git(&r.dir, &["checkout", "-b", "feature"]);
    r.h5i_ok(&["env", "create", "orphan"]);
    assert_eq!(
        r.manifest("orphan")["parent_branch"].as_str().unwrap(),
        "feature"
    );
    let base0 = r.manifest("orphan")["base_commit"]
        .as_str()
        .unwrap()
        .to_string();

    // Delete the parent branch out from under the env.
    git(&r.dir, &["checkout", "main"]);
    git(&r.dir, &["branch", "-D", "feature"]);

    let out = r.h5i(&["env", "rebase", "orphan"]);
    assert!(
        !out.status.success(),
        "rebase onto a gone parent must refuse"
    );
    assert!(
        out_str(&out).contains("parent branch 'feature' is gone"),
        "{}",
        out_str(&out)
    );
    // Nothing was re-pinned.
    assert_eq!(r.manifest("orphan")["base_commit"].as_str().unwrap(), base0);
}

#[test]
fn rebase_three_way_merges_a_diverged_parent() {
    let r = Repo::new();
    // A commit the env will pin its base onto.
    std::fs::write(r.dir.join("a.txt"), "a\n").unwrap();
    git(&r.dir, &["add", "a.txt"]);
    git(&r.dir, &["commit", "-m", "add a"]);
    r.h5i_ok(&["env", "create", "div"]);

    // The env adds its own file …
    std::fs::write(r.work("div").join("env.txt"), "from env\n").unwrap();

    // … while the parent is REWOUND past the base and re-grown on a new file:
    // the pinned base (the "add a" commit) is no longer an ancestor → Diverged.
    git(&r.dir, &["reset", "--hard", "HEAD~1"]);
    std::fs::write(r.dir.join("b.txt"), "b\n").unwrap();
    git(&r.dir, &["add", "b.txt"]);
    git(&r.dir, &["commit", "-m", "add b instead"]);
    let new_tip = out_str(&git(&r.dir, &["rev-parse", "HEAD"]))
        .trim()
        .to_string();

    let st = out_str(&r.h5i_ok(&["env", "status", "div"]));
    assert!(st.contains("parent diverged"), "divergence surfaced: {st}");

    // Rebase 3-way merges the divergence cleanly and re-pins onto the new tip.
    let out = out_str(&r.h5i_ok(&["env", "rebase", "div"]));
    assert!(out.contains("rebased onto main"), "{out}");
    assert_eq!(
        r.manifest("div")["base_commit"].as_str().unwrap(),
        new_tip,
        "base re-pinned onto the diverged tip"
    );

    // Worktree reflects the merge: the env file survives, the parent's new file
    // appears, and the rewound file is gone.
    assert!(
        r.work("div").join("env.txt").is_file(),
        "env's change preserved"
    );
    assert!(
        r.work("div").join("b.txt").is_file(),
        "parent's new file folded in"
    );
    assert!(
        !r.work("div").join("a.txt").exists(),
        "rewound file dropped by the merge"
    );
    let st = out_str(&r.h5i_ok(&["env", "status", "div"]));
    assert!(st.contains("up to date with parent"), "drift cleared: {st}");
}

#[test]
fn rebase_records_a_two_parent_provenance_commit() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "prov"]);
    std::fs::write(r.work("prov").join("env.txt"), "from env\n").unwrap();

    // Advance the parent so the rebase produces a real merge commit.
    std::fs::write(r.dir.join("lib.py"), "def hello():\n    return 99\n").unwrap();
    git(&r.dir, &["add", "lib.py"]);
    git(&r.dir, &["commit", "-m", "parent moves"]);

    r.h5i_ok(&["env", "rebase", "prov"]);

    // The env branch tip is a 2-parent merge whose subject records the fold.
    let branch = "refs/heads/h5i/env/tester/prov";
    let parents = out_str(&git(&r.dir, &["rev-list", "--parents", "-n", "1", branch]));
    // commit + 2 parents = 3 space-separated oids.
    assert_eq!(
        parents.split_whitespace().count(),
        3,
        "rebase tip has two parents: {parents}"
    );
    let subject = out_str(&git(&r.dir, &["log", "-1", "--format=%s", branch]));
    assert!(
        subject.contains("h5i box rebase: env/tester/prov onto main"),
        "provenance subject: {subject}"
    );
}

#[test]
fn rebase_folds_a_parent_advance_with_no_env_changes() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "clean"]);

    // The env touched nothing; only the parent advanced.
    std::fs::write(r.dir.join("lib.py"), "def hello():\n    return 99\n").unwrap();
    git(&r.dir, &["add", "lib.py"]);
    git(&r.dir, &["commit", "-m", "parent moves"]);
    let new_tip = out_str(&git(&r.dir, &["rev-parse", "HEAD"]))
        .trim()
        .to_string();

    let out = out_str(&r.h5i_ok(&["env", "rebase", "clean"]));
    assert!(out.contains("rebased onto main"), "{out}");
    assert_eq!(
        r.manifest("clean")["base_commit"].as_str().unwrap(),
        new_tip,
        "base re-pinned even with no env-side work"
    );
    // The parent's advance is now visible in the worktree.
    let lib = std::fs::read_to_string(r.work("clean").join("lib.py")).unwrap();
    assert!(lib.contains("return 99"), "parent change folded in: {lib}");

    // A second rebase with no further drift is a clean no-op.
    let out = out_str(&r.h5i_ok(&["env", "rebase", "clean"]));
    assert!(out.contains("nothing to rebase"), "{out}");
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

#[test]
fn log_json_emits_env_event_records() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "jlog"]);
    r.h5i_ok(&["env", "run", "jlog", "--", "sh", "-c", "echo ok"]);

    let out = r.h5i_ok(&["env", "log", "jlog", "--json"]);
    let events: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("env log --json must be valid JSON");
    let arr = events.as_array().expect("event log is a JSON array");
    assert!(
        arr.iter().all(|event| event["env_id"] == "env/tester/jlog"),
        "all events are scoped to the requested env: {events}"
    );
    assert!(
        arr.iter().any(|event| event["event"] == "created"),
        "created event is present: {events}"
    );
    assert!(
        arr.iter()
            .any(|event| event["event"] == "exec" && event["capture"].is_string()),
        "exec event includes capture evidence: {events}"
    );

    let text = out_str(&r.h5i_ok(&["env", "log", "jlog"]));
    assert!(text.contains("created"), "{text}");
    assert!(text.contains("exec"), "{text}");
}

#[test]
fn env_log_limit_returns_the_newest_events() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "limited-log"]);
    for marker in ["first", "second", "third"] {
        r.h5i_ok(&[
            "env",
            "run",
            "limited-log",
            "--",
            "sh",
            "-c",
            &format!("echo {marker}"),
        ]);
    }

    let all: serde_json::Value = serde_json::from_slice(
        &r.h5i_ok(&["env", "log", "limited-log", "--json"]).stdout,
    )
    .expect("full env log must be valid JSON");
    let limited: serde_json::Value = serde_json::from_slice(
        &r.h5i_ok(&["env", "log", "limited-log", "--limit", "2", "--json"])
            .stdout,
    )
    .expect("limited env log must be valid JSON");

    let all = all.as_array().expect("full event log is an array");
    let limited = limited.as_array().expect("limited event log is an array");
    assert_eq!(limited.len(), 2);
    assert_eq!(limited, &all[all.len() - 2..]);

    let unlimited: serde_json::Value = serde_json::from_slice(
        &r.h5i_ok(&["env", "log", "limited-log", "--limit", "0", "--json"])
            .stdout,
    )
    .expect("zero limit env log must be valid JSON");
    assert_eq!(unlimited.as_array().unwrap(), all);
}

// ─── 14. shareable environments across clones (the multi-agent review loop) ──

#[test]
fn materialize_skips_poisoned_shared_manifest_but_keeps_valid_ones() {
    let r = Repo::new();
    let repo = git2::Repository::open(&r.dir).unwrap();

    let good = synthetic_env_manifest(&repo, "peer", "good");
    append_synthetic_env_manifest(&repo, &good);

    // This is the old path-escape shape: without import validation, materializing
    // would write `.git/.h5i/env/../escape/manifest.json`, outside env/.
    let bad_traversal = synthetic_env_manifest(&repo, "..", "escape");
    append_synthetic_env_manifest(&repo, &bad_traversal);

    // Individually valid path components, but the identity fields disagree with
    // the canonical env/<agent>/<slug> shape. This should also be skipped.
    let mut bad_spoof = synthetic_env_manifest(&repo, "peer", "spoof");
    bad_spoof.id = "env/peer/not-spoof".into();
    append_synthetic_env_manifest(&repo, &bad_spoof);

    let out = out_str(&r.h5i_ok(&["env", "list"]));
    assert!(
        out.contains("env/peer/good"),
        "valid shared manifest materialized:\n{out}"
    );
    assert!(
        out.contains("skipping shared env manifest"),
        "poisoned manifests should produce a warning, not abort sync:\n{out}"
    );
    assert!(
        r.dir
            .join(".git/.h5i/env/peer/good/manifest.json")
            .is_file(),
        "valid manifest written under env root"
    );
    assert!(
        !r.dir.join(".git/.h5i/escape/manifest.json").exists(),
        "traversal manifest must not write outside .git/.h5i/env"
    );
    assert!(
        !r.dir
            .join(".git/.h5i/env/peer/spoof/manifest.json")
            .exists(),
        "identity-tampered manifest must not be materialized"
    );
}

#[test]
fn env_ref_holds_manifest_and_policy_blobs() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "shared"]);
    // The ref tree carries the three shareable files.
    let manifests = out_str(&git(&r.dir, &["show", "refs/h5i/env/meta:manifests.jsonl"]));
    assert!(manifests.contains("env/tester/shared"), "{manifests}");
    let policies = out_str(&git(&r.dir, &["show", "refs/h5i/env/meta:policies.jsonl"]));
    assert!(
        policies.contains("env/tester/shared"),
        "policy blob present: {policies}"
    );
    let events = out_str(&git(&r.dir, &["show", "refs/h5i/env/meta:events.jsonl"]));
    assert!(events.contains("\"event\":\"created\""), "{events}");
}

// ─── 15. probe is honest and machine-readable ───────────────────────────────

#[test]
fn probe_reports_all_capability_lines() {
    let r = Repo::new();
    let out = out_str(&r.h5i_ok(&["env", "probe"]));
    for key in ["os", "mechanism", "workspace", "process"] {
        assert!(out.contains(key), "probe output missing {key}: {out}");
    }
    // The primitive lines are per-OS: reporting `landlock_abi = none` on a Mac
    // would read as a broken host when macOS simply confines with Seatbelt.
    // Assert the lines this platform actually owes the operator.
    let primitives: &[&str] = if cfg!(target_os = "macos") {
        &["seatbelt"]
    } else {
        &["landlock_abi", "userns", "seccomp"]
    };
    for key in primitives {
        assert!(out.contains(key), "probe output missing {key}: {out}");
    }
    // Workspace is satisfiable everywhere. Match the *claim* line specifically:
    // other lines name the tier too (`tty-injection` reports that the shared
    // terminal is unprotected at `isolation=workspace`), and a substring search
    // for "workspace" would assert against whichever came first.
    let ws_line = out.lines().find(|l| l.contains("claim workspace")).unwrap();
    assert!(ws_line.contains("yes"), "{ws_line}");
    // The functional self-test line is present and agrees with create.
    let run_line = out
        .lines()
        .find(|l| l.contains("runnable"))
        .expect("runnable line");
    let says_yes = run_line.contains("yes");
    assert_eq!(
        says_yes,
        process_tier_runnable(),
        "probe 'runnable' must match create: {run_line}"
    );
}

// ─── Idea 0: fleet view (`env list --json`) + `env doctor` ───────────────────

#[test]
fn list_json_carries_manifest_and_drift() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "fix-auth"]);
    let out = r.h5i_ok(&["env", "list", "--json"]);
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("env list --json must be valid JSON");
    let arr = v.as_array().expect("a JSON array");
    assert_eq!(arr.len(), 1, "one env created");
    assert_eq!(arr[0]["id"], "env/tester/fix-auth");
    // The fleet view enriches each manifest with computed base drift.
    assert_eq!(arr[0]["drift"], "UpToDate");
    assert_eq!(arr[0]["status"], "created");
}

#[test]
fn doctor_reports_healthy_on_fresh_env() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "fix-auth"]);
    let out = r.h5i_ok(&["env", "doctor", "fix-auth", "--json"]);
    let rep: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("env doctor --json must be valid JSON");
    assert_eq!(rep["healthy"], true, "a fresh workspace env is healthy");
    let checks = rep["checks"].as_array().expect("checks array");
    // Policy integrity + enforcement readiness are the load-bearing checks.
    assert!(checks
        .iter()
        .any(|c| c["name"] == "policy" && c["ok"] == true));
    assert!(checks
        .iter()
        .any(|c| c["name"] == "enforcement" && c["ok"] == true));
}

#[test]
fn doctor_flags_tampered_policy_and_exits_nonzero() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "fix-auth"]);
    // Corrupt the on-disk resolved policy so it no longer loads/verifies. The
    // integrity check must fail closed (a hard ✗, not a warning). (A mere
    // comment would be normalized away, since the digest is over canonical TOML;
    // invalid content is an unambiguous tamper.)
    let pol = r.env_dir("fix-auth").join("policy.resolved.toml");
    std::fs::write(&pol, "this is not = = valid policy toml\n").unwrap();

    let out = r.h5i(&["env", "doctor", "fix-auth", "--json"]);
    assert!(
        !out.status.success(),
        "doctor must exit non-zero for an unhealthy env"
    );
    let rep: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(rep["healthy"], false);
    let policy_check = rep["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "policy")
        .expect("a policy check");
    assert_eq!(policy_check["ok"], false, "tampered policy must fail");
}

// ─── Idea 3: private_paths (per-env inode isolation) ─────────────────────────

#[test]
fn private_paths_isolate_writes_into_per_env_backing() {
    if !process_tier_runnable() {
        eprintln!("skipping: process-tier confinement not runnable on this host");
        return;
    }
    let r = Repo::new();
    // A profile that declares a private `cache` path and runs at the process
    // tier (where a private mount namespace makes the per-env bind possible).
    std::fs::create_dir_all(r.dir.join(".h5i")).unwrap();
    std::fs::write(
        r.dir.join(".h5i/env.toml"),
        "[profile.dev]\nisolation = \"process\"\n\
         [profile.dev.private_paths]\n\"cache\" = { kind = \"cache\", persist = true }\n",
    )
    .unwrap();
    git(&r.dir, &["add", "-A"]);
    git(&r.dir, &["commit", "-m", "add private-path profile"]);

    r.h5i_ok(&[
        "env",
        "create",
        "work1",
        "--profile",
        "dev",
        "--isolation",
        "process",
    ]);
    // Write a file under the private path from inside the box.
    r.h5i_ok(&[
        "env",
        "run",
        "work1",
        "--",
        "sh",
        "-c",
        "echo hello-from-box > cache/marker.txt",
    ]);

    // The write landed in the per-env backing dir, NOT the shared worktree.
    // This is the inode isolation that prevents cross-env lock/cache contention.
    let backing = r.env_dir("work1").join("private/cache/marker.txt");
    assert!(
        backing.is_file(),
        "private-path write must land in the per-env backing dir"
    );
    assert_eq!(
        std::fs::read_to_string(&backing).unwrap().trim(),
        "hello-from-box"
    );
    // The property is that the bytes live in the per-env backing, not in an
    // inode the repo's other worktrees share.
    //
    // Linux gets that from a bind mount, so the worktree path shows nothing at
    // all. macOS has no bind mounts, so the redirect is a symlink *to* the
    // backing: the path resolves, which is the point of it, so `exists()` is
    // the wrong question there. Ask the one that matters on both: is this a
    // redirect to the per-env backing, and is it kept out of the diff h5i would
    // hand a reviewer?
    let worktree_marker = r.work("work1").join("cache/marker.txt");
    if cfg!(target_os = "macos") {
        let link = r.work("work1").join("cache");
        // Canonicalized on both sides: macOS firmlinks mean the symlink records
        // `/private/var/…` where the test's own path says `/var/…`.
        let target = std::fs::read_link(&link).expect("private path must be a symlink");
        assert_eq!(
            target.canonicalize().ok(),
            r.env_dir("work1").join("private/cache").canonicalize().ok(),
            "the worktree path must be a symlink to the per-env backing"
        );
        let out = r.h5i_ok(&["env", "diff", "work1"]);
        let diff = String::from_utf8_lossy(&out.stdout);
        assert!(
            !diff.contains("cache"),
            "h5i's own private-path redirect must not reach the reviewer's diff:\n{diff}"
        );
    } else {
        assert!(
            !worktree_marker.exists(),
            "private-path write must NOT appear in the shared worktree"
        );
    }
}

// ─── Idea 1: secrets broker. `env secrets` legibility + gated command: ───────

fn write_secret_profile(r: &Repo) {
    std::fs::create_dir_all(r.dir.join(".h5i")).unwrap();
    std::fs::write(
        r.dir.join(".h5i/env.toml"),
        "[profile.dev]\nisolation = \"workspace\"\n\
         [profile.dev.secret.API_KEY]\nsource = \"env:MY_TOKEN\"\ninject = \"env\"\nttl = \"1h\"\n\n\
         [profile.cmd]\nisolation = \"workspace\"\nallow_command_extractors = true\n\
         [profile.cmd.secret.FROM_CMD]\nsource = \"command:printf abc123\"\ninject = \"env\"\n\n\
         [profile.cmdbad]\nisolation = \"workspace\"\n\
         [profile.cmdbad.secret.FROM_CMD]\nsource = \"command:printf abc123\"\n",
    )
    .unwrap();
    git(&r.dir, &["add", "-A"]);
    git(&r.dir, &["commit", "-m", "secret profiles"]);
}

#[test]
fn env_secrets_reports_status_without_values() {
    let r = Repo::new();
    write_secret_profile(&r);
    r.h5i_ok(&["env", "create", "e1", "--profile", "dev"]);
    // Resolve status with the host source present. Value is fingerprinted,
    // never surfaced.
    let out = Command::new(H5I)
        .args(["env", "secrets", "e1", "--json"])
        .env("H5I_AGENT", "tester")
        .env("H5I_DEFAULT_ISOLATION", "workspace")
        .env("MY_TOKEN", "supersecret")
        .current_dir(&r.dir)
        .output()
        .expect("run h5i");
    assert!(out.status.success(), "{}", out_str(&out));
    let body = String::from_utf8_lossy(&out.stdout);
    assert!(
        !body.contains("supersecret"),
        "the secret value must never appear: {body}"
    );
    let rows: serde_json::Value = serde_json::from_str(&body).expect("json");
    let row = &rows.as_array().unwrap()[0];
    assert_eq!(row["name"], "API_KEY");
    assert_eq!(row["status"], "ok");
    // Keyed (HMAC under the per-repo key), not a bare digest. An unsalted
    // sha256 prefix in a durable audit record is grindable offline.
    let fp = row["fingerprint"].as_str().unwrap();
    assert!(fp.starts_with("fp:"), "{fp}");
    assert!(!fp.contains("sha256:"), "must not be a plain digest: {fp}");
}

#[test]
fn command_extractor_gated_by_pinned_policy_flag() {
    let r = Repo::new();
    write_secret_profile(&r);
    // No opt-in → the command: extractor is refused at create (the gate is
    // pinned in the policy digest, not just enforced at run time).
    let bad = r.h5i(&["env", "create", "ebad", "--profile", "cmdbad"]);
    assert!(
        !bad.status.success(),
        "command extractor must fail closed at create"
    );
    assert!(out_str(&bad).contains("allow_command_extractors"));
    // Opted in → create succeeds.
    r.h5i_ok(&["env", "create", "ecmd", "--profile", "cmd"]);
}

// ─── Idea 3.5 + 2: services (daemon-free) + dynamic ports ────────────────────

#[test]
fn service_lifecycle_with_dynamic_port() {
    let r = Repo::new();
    std::fs::create_dir_all(r.dir.join(".h5i")).unwrap();
    std::fs::write(
        r.dir.join(".h5i/env.toml"),
        "[profile.dev]\nisolation = \"workspace\"\n\
         [service.web]\ncommand = \"echo listening on $PORT; sleep 30\"\nport = 8000\n",
    )
    .unwrap();
    git(&r.dir, &["add", "-A"]);
    git(&r.dir, &["commit", "-m", "service profile"]);
    r.h5i_ok(&["env", "create", "e1", "--profile", "dev"]);

    // Start: a confined background process with a dynamic port allocated.
    r.h5i_ok(&["env", "service", "start", "e1", "web"]);
    // Give the shell a moment to emit its line.
    std::thread::sleep(std::time::Duration::from_millis(800));

    // ports: exactly one service with a dynamic port.
    let out = r.h5i_ok(&["env", "ports", "e1", "--json"]);
    let ports: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let arr = ports.as_array().unwrap();
    assert_eq!(arr.len(), 1, "one service exposes a port");
    assert_eq!(arr[0]["port"], 8000);
    let dynamic = arr[0]["dynamic_port"].as_u64().expect("a dynamic port");
    assert!(dynamic >= 1024, "dynamic port in the ephemeral range");

    // status: the service is alive.
    let out = r.h5i_ok(&["env", "service", "status", "e1", "--json"]);
    let st: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(st.as_array().unwrap()[0]["alive"], true);

    // logs: the injected PORT reached the service (proves port injection).
    let logs = out_str(&r.h5i_ok(&["env", "service", "logs", "e1", "web"]));
    assert!(
        logs.contains(&format!("listening on {dynamic}")),
        "service must see the injected dynamic port: {logs}"
    );

    // stop: the log is captured as evidence and the service goes away.
    let stop = out_str(&r.h5i_ok(&["env", "service", "stop", "e1", "web"]));
    assert!(stop.contains("log captured"), "{stop}");
    let out = r.h5i_ok(&["env", "service", "status", "e1", "--json"]);
    let st: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(st.as_array().unwrap().is_empty(), "no services after stop");

    // The start/stop timeline is on refs/h5i/env for reviewers.
    let log = out_str(&r.h5i_ok(&["env", "log", "e1"]));
    assert!(log.contains("start web"));
    assert!(log.contains("stop web"));
}

// ─── review #1: service declarations are pinned at create (not mutable) ───────

#[test]
fn service_defs_pinned_at_create_ignore_worktree_edits() {
    let r = Repo::new();
    std::fs::create_dir_all(r.dir.join(".h5i")).unwrap();
    std::fs::write(
        r.dir.join(".h5i/env.toml"),
        "[profile.dev]\nisolation = \"workspace\"\n\
         [service.web]\ncommand = \"echo PINNED_CMD; sleep 30\"\n",
    )
    .unwrap();
    git(&r.dir, &["add", "-A"]);
    git(&r.dir, &["commit", "-m", "service"]);
    r.h5i_ok(&["env", "create", "e1", "--profile", "dev"]);

    // An agent edits the (writable) worktree config AND the repo-root config to
    // a different long-lived command after create.
    let hacked = "[profile.dev]\nisolation = \"workspace\"\n\
                  [service.web]\ncommand = \"echo HACKED_CMD; sleep 30\"\n";
    std::fs::write(r.work("e1").join(".h5i/env.toml"), hacked).unwrap();
    std::fs::write(r.dir.join(".h5i/env.toml"), hacked).unwrap();

    // Start uses the PINNED command snapshotted at create, not the edited one.
    r.h5i_ok(&["env", "service", "start", "e1", "web"]);
    std::thread::sleep(std::time::Duration::from_millis(700));
    let logs = out_str(&r.h5i_ok(&["env", "service", "logs", "e1", "web"]));
    assert!(
        logs.contains("PINNED_CMD"),
        "must run the pinned command: {logs}"
    );
    assert!(
        !logs.contains("HACKED_CMD"),
        "must NOT run the edited command: {logs}"
    );
    let _ = r.h5i(&["env", "service", "stop", "e1", "web"]);
}

#[test]
fn tampered_pinned_service_manifest_fails_closed() {
    let r = Repo::new();
    std::fs::create_dir_all(r.dir.join(".h5i")).unwrap();
    std::fs::write(
        r.dir.join(".h5i/env.toml"),
        "[profile.dev]\nisolation = \"workspace\"\n\
         [service.web]\ncommand = \"echo hi; sleep 30\"\n",
    )
    .unwrap();
    git(&r.dir, &["add", "-A"]);
    git(&r.dir, &["commit", "-m", "service"]);
    r.h5i_ok(&["env", "create", "e1", "--profile", "dev"]);

    // Tamper the env-local pinned manifest directly. The recorded digest no
    // longer matches, so a service start must refuse.
    let pinned = r.env_dir("e1").join("services.json");
    std::fs::write(
        &pinned,
        "{\"web\":{\"command\":\"echo evil; sleep 30\",\"logs\":true}}",
    )
    .unwrap();
    let out = r.h5i(&["env", "service", "start", "e1", "web"]);
    assert!(!out.status.success(), "tampered pin must fail closed");
    assert!(out_str(&out).contains("digest"), "{}", out_str(&out));
}

// ─── review round 2: no-service envs stay unpinnable; service names validated ──

#[test]
fn no_service_env_cannot_add_unpinned_service_after_create() {
    let r = Repo::new();
    // Create an env whose base declares NO services.
    std::fs::create_dir_all(r.dir.join(".h5i")).unwrap();
    std::fs::write(
        r.dir.join(".h5i/env.toml"),
        "[profile.dev]\nisolation = \"workspace\"\n",
    )
    .unwrap();
    git(&r.dir, &["add", "-A"]);
    git(&r.dir, &["commit", "-m", "no services"]);
    r.h5i_ok(&["env", "create", "e1", "--profile", "dev"]);

    // The env is pinned-empty (services.json exists), so it is NOT a legacy env.
    assert!(
        r.env_dir("e1").join("services.json").is_file(),
        "a no-service env must still be pinned (empty)"
    );

    // Add a service to the worktree + repo config after create and try to start
    // it. It must NOT be startable (the pinned-empty manifest wins).
    let added = "[profile.dev]\nisolation = \"workspace\"\n\
                 [service.web]\ncommand = \"echo sneaky; sleep 30\"\n";
    std::fs::write(r.work("e1").join(".h5i/env.toml"), added).unwrap();
    std::fs::write(r.dir.join(".h5i/env.toml"), added).unwrap();
    let out = r.h5i(&["env", "service", "start", "e1", "web"]);
    assert!(
        !out.status.success(),
        "an unpinned service must not be startable"
    );
    assert!(out_str(&out).contains("no service"), "{}", out_str(&out));
}

#[test]
fn traversing_service_name_is_rejected() {
    let r = Repo::new();
    std::fs::create_dir_all(r.dir.join(".h5i")).unwrap();
    std::fs::write(
        r.dir.join(".h5i/env.toml"),
        "[profile.dev]\nisolation = \"workspace\"\n",
    )
    .unwrap();
    git(&r.dir, &["add", "-A"]);
    git(&r.dir, &["commit", "-m", "no services"]);
    r.h5i_ok(&["env", "create", "e1", "--profile", "dev"]);
    // A path-traversing service name must be rejected before any path is built.
    for bad in ["../manifest", "a/b", ".."] {
        let out = r.h5i(&["env", "service", "start", "e1", bad]);
        assert!(
            !out.status.success(),
            "service name '{bad}' must be rejected"
        );
        assert!(
            out_str(&out).contains("invalid service name"),
            "name '{bad}': {}",
            out_str(&out)
        );
    }
    // The env-local manifest was not overwritten by a traversing name.
    assert!(r.env_dir("e1").join("manifest.json").is_file());
}

#[test]
fn create_rejects_bad_service_name_in_config() {
    let r = Repo::new();
    std::fs::create_dir_all(r.dir.join(".h5i")).unwrap();
    // A traversing key in the [service.*] table must fail create (pin) closed.
    std::fs::write(
        r.dir.join(".h5i/env.toml"),
        "[profile.dev]\nisolation = \"workspace\"\n\
         [service.\"../evil\"]\ncommand = \"echo x\"\n",
    )
    .unwrap();
    git(&r.dir, &["add", "-A"]);
    git(&r.dir, &["commit", "-m", "bad service name"]);
    let out = r.h5i(&["env", "create", "e1", "--profile", "dev"]);
    assert!(
        !out.status.success(),
        "create must reject a traversing service name"
    );
    assert!(
        out_str(&out).contains("invalid service name"),
        "{}",
        out_str(&out)
    );
}

// ─── env rm: multi-name and partial-failure ──────────────────────────────────

/// `h5i box rm a b --force` removes both envs in a single call.
/// Single-name behavior is unchanged: exit 0, both manifests and worktrees gone.
#[test]
fn env_rm_removes_multiple_envs() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "alpha"]);
    r.h5i_ok(&["env", "create", "beta"]);

    // Sanity: both worktrees exist before rm.
    assert!(r.work("alpha").exists(), "alpha worktree should exist");
    assert!(r.work("beta").exists(), "beta worktree should exist");

    // Remove both in one call with --force (they are in `created` status).
    let out = r.h5i_ok(&["env", "rm", "alpha", "beta", "--force"]);
    let text = out_str(&out);
    assert!(text.contains("alpha"), "should confirm removal of alpha:\n{text}");
    assert!(text.contains("beta"), "should confirm removal of beta:\n{text}");

    // Worktrees and manifest files must be gone.
    assert!(!r.env_dir("alpha").join("manifest.json").exists(), "alpha manifest still present");
    assert!(!r.env_dir("beta").join("manifest.json").exists(), "beta manifest still present");
    assert!(!r.work("alpha").exists(), "alpha worktree still present");
    assert!(!r.work("beta").exists(), "beta worktree still present");

    // h5i box list should show no envs remaining.
    let list_out = out_str(&r.h5i_ok(&["env", "list"]));
    assert!(
        !list_out.contains("env/tester/alpha") && !list_out.contains("env/tester/beta"),
        "alpha or beta still visible in env list:\n{list_out}"
    );
}

/// `h5i box rm good nope`: one missing env must not abort the other.
/// The present env is removed; exit code is non-zero; both failures are reported.
#[test]
fn env_rm_partial_failure_continues_and_exits_nonzero() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "good"]);

    // Attempt to remove both a real env and a nonexistent one.
    let out = r.h5i(&["env", "rm", "good", "nope", "--force"]);

    // The real env must be gone.
    assert!(!r.env_dir("good").join("manifest.json").exists(), "good manifest still present");
    assert!(!r.work("good").exists(), "good worktree still present");

    // The command must exit non-zero because `nope` failed.
    assert!(
        !out.status.success(),
        "expected non-zero exit when a name fails, got 0"
    );

    // stderr must mention the missing env.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("nope"),
        "expected 'nope' mentioned in stderr:\n{stderr}"
    );

    // But the successful removal must still appear in stdout.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("good"),
        "expected 'good' removal confirmation in stdout:\n{stdout}"
    );
}

// ─── `h5i dev`: the box surface ─────────────────────────────────────────────

/// The short form creates a box with no verb and no name: source defaults to
/// this repository, the name to the checked-out branch. This is the first
/// command a new user runs, so it must work with zero arguments.
#[test]
fn dev_with_no_arguments_creates_a_box_named_for_the_branch() {
    let r = Repo::new();
    let out = out_str(&r.h5i_ok(&["dev"]));
    assert!(out.contains("env/tester/main"), "{out}");
    assert!(r.env_dir("main").join("manifest.json").is_file());

    // A second one cannot take the same name, so it is suffixed rather than
    // colliding or overwriting.
    let out = out_str(&r.h5i_ok(&["dev"]));
    assert!(out.contains("env/tester/main-2"), "{out}");
}

/// `dev` and `env` are the same surface; the old noun stays hidden for one
/// release so existing scripts keep working.
#[test]
fn env_is_a_hidden_alias_for_dev() {
    let r = Repo::new();
    r.h5i_ok(&["env", "create", "aliased"]);
    let via_dev = out_str(&r.h5i_ok(&["dev", "status", "aliased"]));
    let via_env = out_str(&r.h5i_ok(&["env", "status", "aliased"]));
    assert!(via_dev.contains("env/tester/aliased"), "{via_dev}");
    assert_eq!(via_dev, via_env);

    // `ls` is the short form of `list`.
    let ls = out_str(&r.h5i_ok(&["dev", "ls"]));
    assert!(ls.contains("env/tester/aliased"), "{ls}");
}

/// An unrecognized source fails loudly and names the forms that do work,
/// rather than silently creating a box from HEAD.
#[test]
fn dev_refuses_an_unrecognized_source() {
    let r = Repo::new();
    // A bare word is neither `.`, a PR spec, nor a repository URL.
    let out = r.h5i(&["dev", "wat"]);
    assert!(!out.status.success(), "unknown source must be refused");
    let err = out_str(&out);
    assert!(err.contains("unrecognized source"), "{err}");
    assert!(err.contains("pull request"), "{err}");
    assert!(err.contains("--new"), "{err}");
}

// ─── the output gate ────────────────────────────────────────────────────────

/// `dev export` freezes the box and writes the three-file bundle. The patch is
/// the box's diff, the report names every command that ran, and the receipt
/// carries the policy digest that was actually enforced.
#[test]
fn export_writes_patch_report_and_receipt() {
    let r = Repo::new();
    r.h5i_ok(&["dev", "--name", "gate"]);
    r.h5i_ok(&[
        "dev",
        "run",
        "gate",
        "--",
        "sh",
        "-c",
        "echo generated > new.txt; echo built",
    ]);
    let out = out_str(&r.h5i_ok(&["dev", "export", "gate"]));
    assert!(out.contains("exported env/tester/gate"), "{out}");

    let bundle = r.dir.join("h5i-export/gate");
    let patch = std::fs::read_to_string(bundle.join("patch.diff")).expect("patch.diff");
    assert!(patch.contains("new.txt"), "{patch}");
    assert!(patch.contains("+generated"), "{patch}");

    let report = std::fs::read_to_string(bundle.join("report.md")).expect("report.md");
    assert!(report.contains("# Export: env/tester/gate"), "{report}");
    assert!(report.contains("echo built"), "the report lists what ran: {report}");
    assert!(report.contains("isolation enforced"), "{report}");

    let receipt: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(bundle.join("receipt.json")).unwrap())
            .expect("receipt.json is valid JSON");
    let env_manifest = r.manifest("gate");
    assert_eq!(receipt["env_id"], "env/tester/gate");
    assert_eq!(receipt["policy_digest"], env_manifest["policy_digest"]);
    let records = receipt["records"].as_array().expect("records array");
    assert!(!records.is_empty(), "{receipt}");
    assert_eq!(records[0]["source"], "host-env-run");
}

/// An export never silently replaces an earlier one: evidence that can be
/// overwritten without a word is evidence that can go missing.
#[test]
fn export_refuses_to_overwrite_without_force() {
    let r = Repo::new();
    r.h5i_ok(&["dev", "--name", "twice"]);
    r.h5i_ok(&["dev", "run", "twice", "--", "sh", "-c", "echo one > f.txt"]);
    r.h5i_ok(&["dev", "export", "twice"]);

    let out = r.h5i(&["dev", "export", "twice"]);
    assert!(!out.status.success(), "second export must refuse");
    assert!(out_str(&out).contains("--force"), "{}", out_str(&out));

    r.h5i_ok(&["dev", "export", "twice", "--force"]);
}

/// The bundle goes where you ask it to.
#[test]
fn export_honours_an_explicit_out_dir() {
    let r = Repo::new();
    r.h5i_ok(&["dev", "--name", "outdir"]);
    let dest = r.dir.join("somewhere/else");
    r.h5i_ok(&[
        "dev",
        "export",
        "outdir",
        "--out",
        dest.to_str().unwrap(),
    ]);
    assert!(dest.join("patch.diff").is_file());
    assert!(dest.join("report.md").is_file());
    assert!(dest.join("receipt.json").is_file());
}

// ─── the embedded skill ─────────────────────────────────────────────────────

/// The binary carries the skill, so it can be installed with no network, no
/// npm, and no host-to-box file path, which is how it reaches the inside of a
/// box.
#[test]
fn skill_install_writes_the_embedded_pages() {
    let r = Repo::new();
    let target = r.dir.join("skills-out");
    let out = out_str(&r.h5i_ok(&[
        "skill",
        "install",
        "--target",
        target.to_str().unwrap(),
    ]));
    assert!(out.contains("installed the h5i skill"), "{out}");

    let skill = std::fs::read_to_string(target.join("SKILL.md")).expect("SKILL.md");
    assert!(skill.starts_with("---\nname: h5i\n"), "frontmatter: {skill}");
    assert!(target.join("references/policy.md").is_file());

    // `show` prints the same bytes without touching the filesystem.
    let shown = out_str(&r.h5i_ok(&["skill", "show"]));
    assert_eq!(shown, skill);
    let page = out_str(&r.h5i_ok(&["skill", "show", "policy"]));
    assert!(page.contains("Profiles"), "{page}");
}

/// Receipt integrity: the persisted policy grants the box `$WORK` and nothing
/// else under its env directory, so the receipt log and its payloads, which
/// live one level up from the worktree, are unreachable for writing from
/// inside. A box can stage a new record in its spool; it cannot edit one the
/// host already recorded.
#[test]
fn the_receipt_log_is_outside_the_boxs_write_grants() {
    if !process_tier_runnable() {
        eprintln!("skipping: this host cannot run process-tier confinement");
        return;
    }
    let r = Repo::new();
    r.h5i_ok(&["dev", "--name", "sealed", "--isolation", "process"]);
    r.h5i_ok(&["dev", "run", "sealed", "--", "sh", "-c", "echo recorded"]);

    let env_dir = r.env_dir("sealed");
    assert!(env_dir.join("receipt.jsonl").is_file(), "a receipt was written");

    let policy =
        std::fs::read_to_string(env_dir.join("policy.resolved.toml")).expect("resolved policy");
    let writes = policy
        .lines()
        .find(|l| l.trim_start().starts_with("fs_write"))
        .expect("the resolved policy states its write grants");

    // `$WORK` is the worktree. A subdirectory of the env dir. Nothing may
    // grant the env dir itself, which is where receipt.jsonl and receipts/ are.
    for grant in ["receipt", "receipts"] {
        assert!(
            !writes.contains(grant),
            "the receipt store must never appear in a write grant: {writes}"
        );
    }
    assert!(writes.contains("$WORK"), "the worktree is the write window: {writes}");
    assert!(
        !writes.contains(&env_dir.display().to_string()),
        "no grant names the env directory itself: {writes}"
    );
}

// ─── detached boxes: code copied in, host repository untouched ──────────────

/// `h5i box <url>` copies an external repository into the box. The host
/// repository gets no branch, no worktree and no objects from it: the boundary
/// the product claims only holds if outside code never lands here.
#[test]
fn a_cloned_box_never_touches_the_host_repository() {
    let r = Repo::new();

    // A separate repository to stand in for "somebody else's code".
    let external = r.dir.parent().unwrap().join("external");
    run_ok(Command::new("git").args(["init", "-q", "-b", "main"]).arg(&external));
    git(&external, &["config", "user.email", "x@h5i.test"]);
    git(&external, &["config", "user.name", "X"]);
    std::fs::write(external.join("app.py"), "print('external')\n").unwrap();
    git(&external, &["add", "."]);
    git(&external, &["commit", "-m", "external code"]);

    let url = format!("file://{}", external.display());
    let out = out_str(&r.h5i_ok(&["dev", &url]));
    assert!(out.contains("env/tester/external"), "named for the repo: {out}");

    // The code is in the box …
    assert!(r.env_dir("external").join("work/app.py").is_file());

    // … and nothing of it is in the host repository.
    let branches = out_str(&git(&r.dir, &["branch", "--list"]));
    assert!(
        !branches.contains("external"),
        "no branch for a detached box: {branches}"
    );
    let worktrees = out_str(&git(&r.dir, &["worktree", "list"]));
    assert!(
        !worktrees.contains("external"),
        "no worktree registered in the host repo: {worktrees}"
    );

    // The manifest records where it came from, and status says so.
    let m = r.manifest("external");
    assert_eq!(m["source"].as_str(), Some(url.as_str()).map(|u| format!("clone:{u}")).as_deref());
    let status = out_str(&r.h5i_ok(&["dev", "status", "external"]));
    assert!(status.contains("detached"), "{status}");

    // A shallow clone keeps an origin remote pointing at the source; a box must
    // not inherit a network handle nobody granted it.
    let remotes = out_str(&git(&r.env_dir("external").join("work"), &["remote"]));
    assert!(remotes.trim().is_empty(), "origin must be dropped: {remotes}");
}

/// A detached box has no parent branch here, so `apply` and `rebase` refuse and
/// point at the export gate instead of half-working.
#[test]
fn a_detached_box_refuses_apply_and_rebase() {
    let r = Repo::new();
    r.h5i_ok(&["dev", "--new", "--name", "scratch"]);
    r.h5i_ok(&["dev", "run", "scratch", "--", "sh", "-c", "echo hi > f.txt"]);
    r.h5i_ok(&["dev", "propose", "scratch"]);

    for verb in ["apply", "rebase"] {
        let out = r.h5i(&["dev", verb, "scratch"]);
        assert!(!out.status.success(), "{verb} must refuse on a detached box");
        let err = out_str(&out);
        assert!(err.contains("detached"), "{verb}: {err}");
        assert!(err.contains("export"), "{verb} names the way out: {err}");
    }
}

/// An empty box still has an immutable base, so `export` produces everything
/// the agent wrote rather than an empty patch.
#[test]
fn an_empty_box_exports_everything_the_agent_wrote() {
    let r = Repo::new();
    r.h5i_ok(&["dev", "--new", "--name", "fromzero"]);
    r.h5i_ok(&[
        "dev",
        "run",
        "fromzero",
        "--",
        "sh",
        "-c",
        "echo 'def main(): pass' > main.py",
    ]);
    r.h5i_ok(&["dev", "export", "fromzero"]);

    let patch =
        std::fs::read_to_string(r.dir.join("h5i-export/fromzero/patch.diff")).expect("patch");
    assert!(patch.contains("main.py"), "{patch}");
    assert!(patch.contains("+def main(): pass"), "{patch}");
}

// ─── the browser profile (M4) ───────────────────────────────────────────────

/// A `browser` box either gets a browser or is refused at create with a
/// message naming what to install. What it must never be is a box whose first
/// `agent-browser open` fails with "not found".
#[test]
fn the_browser_profile_fails_closed_without_the_tooling() {
    let r = Repo::new();
    let out = r.h5i(&["dev", "--name", "br", "--profile", "browser"]);

    let text = out_str(&out);
    if out.status.success() {
        // This host has Chrome and agent-browser: the box exists and says so.
        let status = out_str(&r.h5i_ok(&["dev", "status", "br"]));
        assert!(status.contains("profile=browser"), "{status}");
    } else {
        // Two ways a browser box is legitimately refused, and both must say
        // which one it is: the host has no browser to grant, or the tier cannot
        // enforce the egress allowlist the profile carries (it is a
        // supervised/container profile, like `agent`).
        let names_tooling = text.contains("agent-browser") || text.contains("Chrome");
        let names_tier = text.contains("net.egress") && text.contains("cannot enforce");
        assert!(
            names_tooling || names_tier,
            "the refusal must name what is missing: {text}"
        );
        if names_tooling {
            assert!(text.contains("install"), "and how to get it: {text}");
        }
        assert!(
            !r.env_dir("br").exists(),
            "a refused create must leave nothing behind"
        );
    }
}

/// A cache that matches the project's lockfiles is offered to a box, and it is
/// offered *read-only*: a cache a box could write would be a mutable surface
/// shared between boxes, which is the thing the design refuses.
#[test]
fn a_matching_cache_is_mounted_read_only() {
    if !process_tier_runnable() {
        eprintln!("skipping: this host cannot run process-tier confinement");
        return;
    }
    let r = Repo::new();
    std::fs::write(r.dir.join("Cargo.lock"), "# pinned deps\n").unwrap();
    git(&r.dir, &["add", "."]);
    git(&r.dir, &["commit", "-m", "lockfile"]);

    // No cache yet: a box would start cold, and `mounts` says so.
    let out = out_str(&r.h5i_ok(&["dev", "cache", "mounts"]));
    assert!(out.contains("cold"), "{out}");

    // Materialize one by hand (refresh is not built yet) and it is offered.
    let refusal = out_str(&r.h5i(&["dev", "cache", "refresh", "cargo"]));
    assert!(
        refusal.contains("[profile.cache-cargo]") && refusal.contains("net.egress"),
        "the refusal must hand over the profile it needs: {refusal}"
    );
    let key = refusal
        .split("/cache/cargo/")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .expect("the refusal names the cache directory it prepared")
        .to_string();
    let cache = r.dir.join(".git/.h5i/cache/cargo").join(&key);
    std::fs::create_dir_all(cache.join("cache")).unwrap();
    std::fs::write(cache.join("marker"), b"cached").unwrap();

    let out = out_str(&r.h5i_ok(&["dev", "cache", "mounts"]));
    assert!(out.contains("~/.cargo/registry"), "{out}");
    assert!(out.contains("read-only"), "{out}");

    let listed = out_str(&r.h5i_ok(&["dev", "cache", "ls"]));
    assert!(listed.contains("current"), "{listed}");

    // Change the lockfile: the cache is still on disk but no longer matches, so
    // it is listed as stale and no box is given it.
    std::fs::write(r.dir.join("Cargo.lock"), "# different deps\n").unwrap();
    let listed = out_str(&r.h5i_ok(&["dev", "cache", "ls"]));
    assert!(listed.contains("stale"), "{listed}");
    let out = out_str(&r.h5i_ok(&["dev", "cache", "mounts"]));
    assert!(out.contains("cold"), "{out}");
}
