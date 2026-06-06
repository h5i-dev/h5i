//! End-to-end git-operation tests for the token-reduction object store.
//!
//! Drives the compiled binary against real git repos (and a shared bare remote)
//! to prove the feature's git plumbing works:
//!
//!   - `capture run` writes a manifest to `refs/h5i/objects` and stores the raw
//!     blob locally; `recall object` rehydrates it byte-for-byte.
//!   - `share push` / `share pull` carry the manifest log between clones, while
//!     the raw blob stays local (git-lfs style) — an unfetched object reads as
//!     "absent" but its summary still travels.
//!   - Divergent object logs union-merge on pull, losing no manifest.
//!   - `objects gc --ttl` evicts the raw but never the git-tracked summary.
//!
//! Run with: cargo test --test objects_e2e

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

const H5I: &str = env!("CARGO_BIN_EXE_h5i");

fn run_ok(cmd: &mut Command) -> Output {
    let out = cmd.output().expect("spawn");
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

struct Clone {
    dir: PathBuf,
}

impl Clone {
    fn h5i(&self, args: &[&str]) -> Output {
        Command::new(H5I)
            .args(args)
            .env_remove("H5I_AGENT")
            .current_dir(&self.dir)
            .output()
            .expect("run h5i")
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
    /// Number of manifests on the local `refs/h5i/objects` tip (0 if absent).
    fn manifest_count(&self) -> usize {
        let out = Command::new("git")
            .args(["show", "refs/h5i/objects:manifests.jsonl"])
            .current_dir(&self.dir)
            .output()
            .expect("git show");
        if !out.status.success() {
            return 0;
        }
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count()
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// First 16-hex object id found in `s` (the manifest id format).
fn first_id(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 16 <= bytes.len() {
        if bytes[i..i + 16].iter().all(|b| b.is_ascii_hexdigit())
            && (i + 16 == bytes.len() || !bytes[i + 16].is_ascii_hexdigit())
            && (i == 0 || !bytes[i - 1].is_ascii_hexdigit())
        {
            return Some(s[i..i + 16].to_string());
        }
        i += 1;
    }
    None
}

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

fn single() -> (TempDir, Clone) {
    let (root, a, _b) = two_clones();
    (root, a)
}

// ── capture writes a ref + stores raw; recall rehydrates exactly ──────────────

#[test]
fn capture_run_stores_manifest_and_rehydrates_losslessly() {
    let (_root, a) = single();

    // Deterministic output we can compare byte-for-byte.
    let prog = "for i in $(seq 1 50); do echo \"row $i\"; done";
    let cap = a.h5i_ok(&["capture", "run", "--min-bytes", "0", "--", "bash", "-c", prog]);
    // The pointer line (stderr) carries the object id.
    let id = first_id(&stderr(&cap)).expect("object id in capture output");

    // A manifest landed on refs/h5i/objects.
    assert_eq!(a.manifest_count(), 1, "exactly one manifest expected");

    // recall objects lists it.
    let list = stdout(&a.h5i_ok(&["recall", "objects"]));
    assert!(list.contains(&id), "recall objects should list {id}:\n{list}");

    // recall object rehydrates the EXACT raw bytes.
    let expected: String = (1..=50).map(|i| format!("row {i}\n")).collect();
    let raw = a.h5i_ok(&["recall", "object", &id]);
    assert_eq!(stdout(&raw), expected, "rehydrated raw must match original exactly");
}

#[test]
fn capture_run_passes_through_exit_code() {
    let (_root, a) = single();
    let out = a.h5i(&["capture", "run", "--min-bytes", "0", "--", "bash", "-c", "echo boom; exit 7"]);
    assert_eq!(out.status.code(), Some(7), "child exit code must pass through");
    // ...and it was still captured.
    assert_eq!(a.manifest_count(), 1);
}

// ── push/pull: manifest travels, raw stays local ─────────────────────────────

#[test]
fn manifest_travels_via_push_pull_but_raw_stays_local() {
    let (_root, a, b) = two_clones();

    a.h5i_ok(&["capture", "run", "--min-bytes", "0", "--", "bash", "-c", "echo hello-from-a"]);
    a.h5i_ok(&["push", "--remote", "origin"]);

    b.h5i_ok(&["pull", "--remote", "origin"]);
    assert_eq!(b.manifest_count(), 1, "manifest should arrive at b");

    let id = first_id(&stdout(&b.h5i_ok(&["recall", "objects"]))).expect("id on b");

    // The raw blob did NOT travel: recall object on b fails with an "absent"
    // message, while the summary is still available.
    let raw = b.h5i(&["recall", "object", &id]);
    assert!(!raw.status.success(), "raw should be absent on b");
    assert!(
        stderr(&raw).contains("absent"),
        "expected an 'absent' message, got: {}",
        stderr(&raw)
    );
    let summary = stdout(&b.h5i_ok(&["recall", "object", &id, "--summary"]));
    assert!(summary.contains("hello-from-a"), "summary should travel: {summary}");
}

// ── divergent logs union-merge on pull ────────────────────────────────────────

#[test]
fn divergent_object_logs_union_merge_on_pull() {
    let (_root, a, b) = two_clones();

    // Shared history: X captured by a and pulled by b.
    a.h5i_ok(&["capture", "run", "--min-bytes", "0", "--", "bash", "-c", "echo X"]);
    a.h5i_ok(&["push", "--remote", "origin"]);
    b.h5i_ok(&["pull", "--remote", "origin"]);
    assert_eq!(b.manifest_count(), 1);

    // Now both capture "offline": a pushes Y, b holds Z → divergence.
    a.h5i_ok(&["capture", "run", "--min-bytes", "0", "--", "bash", "-c", "echo Y"]);
    a.h5i_ok(&["push", "--remote", "origin"]);
    b.h5i_ok(&["capture", "run", "--min-bytes", "0", "--", "bash", "-c", "echo Z"]);

    // b pulls the divergent remote → union-merge keeps all three.
    let pull = b.h5i_ok(&["pull", "--remote", "origin"]);
    assert!(
        stdout(&pull).to_lowercase().contains("merged") || b.manifest_count() == 3,
        "pull output: {}",
        stdout(&pull)
    );
    assert_eq!(b.manifest_count(), 3, "union-merge must keep X, Y and Z");

    // Re-pulling is idempotent (no duplication).
    b.h5i_ok(&["pull", "--remote", "origin"]);
    assert_eq!(b.manifest_count(), 3, "re-pull must not duplicate manifests");
}

// ── gc lifetime: raw evicted, summary kept ────────────────────────────────────

#[test]
fn ttl_gc_evicts_raw_but_keeps_summary() {
    let (_root, a) = single();
    a.h5i_ok(&["capture", "run", "--min-bytes", "0", "--", "bash", "-c", "echo keep-this-summary"]);
    let id = first_id(&stdout(&a.h5i_ok(&["recall", "objects"]))).expect("id");

    // Raw present before gc.
    assert!(a.h5i(&["recall", "object", &id]).status.success());

    // Evict referenced-but-stale blobs (ttl 0 → everything is "older").
    let gc = a.h5i_ok(&["objects", "gc", "--ttl", "0s"]);
    assert!(stdout(&gc).contains("evicted") || stdout(&gc).contains("freed"), "{}", stdout(&gc));

    // Raw now absent, but the summary (manifest) survives.
    assert!(!a.h5i(&["recall", "object", &id]).status.success(), "raw should be evicted");
    let list = stdout(&a.h5i_ok(&["recall", "objects"]));
    assert!(list.contains(&id), "summary must survive gc: {list}");

    // fsck reports the absence.
    let fsck = stdout(&a.h5i_ok(&["objects", "fsck"]));
    assert!(fsck.contains("absent"), "fsck should report the absent blob: {fsck}");
}

// ── structured output: default render + status/tool filters ───────────────────

#[test]
fn capture_emits_structured_default_and_is_queryable() {
    let (_root, a) = single();
    // Fake pytest with a failure, large enough to store.
    std::fs::create_dir_all(a.dir.join("bin")).unwrap();
    let pytest = a.dir.join("bin/pytest");
    std::fs::write(
        &pytest,
        "#!/bin/bash\necho '=== test session starts ==='\nfor i in $(seq 1 200); do echo \"tests/t.py::test_$i PASSED\"; done\necho 'FAILED tests/t.py::test_pay - assert 0 == 100'\necho '=== 1 failed, 200 passed in 4.1s ==='\nexit 1\n",
    )
    .unwrap();
    let mut perms = std::fs::metadata(&pytest).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    std::fs::set_permissions(&pytest, perms).unwrap();

    let path = format!("{}:{}", a.dir.join("bin").display(), std::env::var("PATH").unwrap_or_default());
    let out = Command::new(H5I)
        .args(["capture", "run", "--", "pytest", "-q"])
        .env("PATH", &path)
        .env_remove("H5I_AGENT")
        .current_dir(&a.dir)
        .output()
        .expect("run");
    let summary = stdout(&out);
    // Default output is the compact one-line-per-finding render.
    assert!(summary.contains("pytest test failed"), "expected compact header:\n{summary}");
    assert!(summary.contains("tests/t.py::test_pay"), "expected the failing finding:\n{summary}");
    assert!(summary.contains("1 failed"), "expected counts:\n{summary}");

    // Queryable by structured status and tool.
    let by_status = stdout(&a.h5i_ok(&["recall", "objects", "--status", "failed"]));
    assert!(by_status.contains("1 object matched"), "{by_status}");
    let by_tool = stdout(&a.h5i_ok(&["recall", "objects", "--tool", "pytest"]));
    assert!(by_tool.contains("1 object matched"), "{by_tool}");
}

// ── min-bytes threshold: small output passes through unstored ─────────────────

#[test]
fn small_output_below_threshold_is_not_stored() {
    let (_root, a) = single();

    // Tiny output with the default threshold → passed through, no object created.
    let out = a.h5i_ok(&["capture", "run", "--", "bash", "-c", "echo hi"]);
    assert!(stdout(&out).contains("hi"), "raw should pass through: {}", stdout(&out));
    assert_eq!(a.manifest_count(), 0, "small output must not create a manifest");

    // Large output (over the default threshold) → stored.
    let big = "for i in $(seq 1 400); do echo \"line $i has some content here\"; done";
    a.h5i_ok(&["capture", "run", "--", "bash", "-c", big]);
    assert_eq!(a.manifest_count(), 1, "large output should be captured");

    // --min-bytes 0 forces capture even of tiny output.
    a.h5i_ok(&["capture", "run", "--min-bytes", "0", "--", "bash", "-c", "echo tiny"]);
    assert_eq!(a.manifest_count(), 2);
}

// ── branch / file association + filtered recall ──────────────────────────────

#[test]
fn captures_are_filterable_by_branch_and_file() {
    let (_root, a) = single();

    // Capture on the default branch, tagged with a file.
    a.h5i_ok(&["capture", "run", "--min-bytes", "0", "--file", "src/api.rs", "--", "bash", "-c", "echo on-default"]);

    // Switch to a feature branch and capture there.
    git(&a.dir, &["checkout", "-b", "feature-x"]);
    a.h5i_ok(&["capture", "run", "--min-bytes", "0", "--file", "src/auth.rs", "--", "bash", "-c", "echo on-feature"]);

    // --branch filters to just that branch's captures.
    let feat = stdout(&a.h5i_ok(&["recall", "objects", "--branch", "feature-x"]));
    assert!(feat.contains("⎇ feature-x"), "should show feature-x captures:\n{feat}");
    assert!(feat.contains("1 object matched"), "exactly one on feature-x:\n{feat}");

    // --file filters by associated file (suffix match too).
    let by_file = stdout(&a.h5i_ok(&["recall", "objects", "--file", "auth.rs"]));
    assert!(by_file.contains("1 object matched"), "one capture touches auth.rs:\n{by_file}");

    // A file nobody touched matches nothing.
    let none = stdout(&a.h5i_ok(&["recall", "objects", "--file", "nope.rs"]));
    assert!(none.contains("No captured objects match"), "{none}");
}

// ── manifests persist across a fresh process / repo reopen ────────────────────

#[test]
fn manifests_persist_and_accumulate() {
    let (_root, a) = single();
    for word in ["one", "two", "three"] {
        a.h5i_ok(&["capture", "run", "--min-bytes", "0", "--", "bash", "-c", &format!("echo {word}")]);
    }
    // A brand-new invocation (fresh process) still sees all three.
    assert_eq!(a.manifest_count(), 3);
    let list = stdout(&a.h5i_ok(&["recall", "objects"]));
    assert!(list.contains("3 objects captured"), "expected 3 listed: {list}");
}
