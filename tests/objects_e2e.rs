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
            // Strip the box's env-capture vars: if the suite runs inside an h5i
            // env box they leak in and make `h5i commit`/`capture run` stage to
            // the env spool for host ingest instead of writing the note/object
            // locally (main.rs ~5559, ~7879), breaking provenance assertions.
            // This temp repo is not the box's env. No-op on host/CI.
            .env_remove("H5I_ENV_ID")
            .env_remove("H5I_ENV_POLICY_DIGEST")
            .env_remove("H5I_ENV_CAPTURE_SPOOL")
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

// ── git-ref blob store: share raw, merge-before-push (no clobber) ──────────────

#[test]
fn objects_push_merges_and_shares_blobs_across_clones() {
    let (_root, a, b) = two_clones();

    // a shares X (manifest + raw blob).
    let cx = a.h5i_ok(&["capture", "run", "--min-bytes", "0", "--", "bash", "-c", "echo XXXX"]);
    let idx = first_id(&stderr(&cx)).expect("id x");
    a.h5i_ok(&["push", "--remote", "origin"]);
    a.h5i_ok(&["objects", "push", "--remote", "origin"]);

    // b learns of X, captures Y, and shares Y's blob. `objects push` must
    // fetch+union-merge the remote first, so it CANNOT clobber a's blob X.
    b.h5i_ok(&["pull", "--remote", "origin"]);
    let cy = b.h5i_ok(&["capture", "run", "--min-bytes", "0", "--", "bash", "-c", "echo YYYY"]);
    let idy = first_id(&stderr(&cy)).expect("id y");
    b.h5i_ok(&["push", "--remote", "origin"]);
    b.h5i_ok(&["objects", "push", "--remote", "origin"]);

    // b never had X's raw locally; the merge-before-push brought it in, so b can
    // recall X — proof the push did not clobber the remote set.
    let rx = b.h5i_ok(&["recall", "object", &idx]);
    assert_eq!(stdout(&rx), "XXXX\n", "X must survive b's objects push (no clobber)");

    // a pulls manifests + blobs and can now recall Y, which it never captured.
    a.h5i_ok(&["pull", "--remote", "origin"]);
    a.h5i_ok(&["objects", "pull", "--remote", "origin"]);
    let ry = a.h5i_ok(&["recall", "object", &idy]);
    assert_eq!(stdout(&ry), "YYYY\n", "Y must be recoverable on a after objects pull");
}

#[test]
fn objects_pull_is_graceful_when_nothing_shared() {
    let (_root, a) = single();
    // Nobody has pushed refs/h5i/objects-data → pull should succeed (exit 0)
    // with a friendly note rather than failing on a missing remote ref.
    let out = a.h5i_ok(&["objects", "pull", "--remote", "origin"]);
    assert!(
        stdout(&out).to_lowercase().contains("no shared raw blobs"),
        "expected a friendly empty-store message, got: {}",
        stdout(&out)
    );
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
        .env_remove("H5I_ENV_ID")
        .env_remove("H5I_ENV_POLICY_DIGEST")
        .env_remove("H5I_ENV_CAPTURE_SPOOL")
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
fn small_successful_output_below_threshold_is_not_stored() {
    let (_root, a) = single();

    // Tiny *successful* output with the default threshold → passed through, no
    // object created (nothing to reduce, and exit 0 carries no signal to keep).
    let out = a.h5i_ok(&["capture", "run", "--", "bash", "-c", "echo hi"]);
    assert!(stdout(&out).contains("hi"), "raw should pass through: {}", stdout(&out));
    assert_eq!(a.manifest_count(), 0, "small successful output must not create a manifest");

    // Large output (over the default threshold) → stored.
    let big = "for i in $(seq 1 400); do echo \"line $i has some content here\"; done";
    a.h5i_ok(&["capture", "run", "--", "bash", "-c", big]);
    assert_eq!(a.manifest_count(), 1, "large output should be captured");

    // --min-bytes 0 forces capture even of tiny output.
    a.h5i_ok(&["capture", "run", "--min-bytes", "0", "--", "bash", "-c", "echo tiny"]);
    assert_eq!(a.manifest_count(), 2);
}

#[test]
fn small_failing_output_is_stored_despite_threshold() {
    let (_root, a) = single();

    // Tiny output, but the command FAILED → the signal-aware gate stores it
    // anyway (provenance + searchability), even though it's far under the byte
    // threshold. `h5i` (not `h5i_ok`): capture passes the nonzero code through.
    let out = a.h5i(&["capture", "run", "--", "bash", "-c", "echo 'boom: assertion failed'; exit 1"]);
    assert_eq!(out.status.code(), Some(1), "exit code is passed through");
    assert!(stdout(&out).contains("boom"), "raw still shown: {}", stdout(&out));
    assert_eq!(a.manifest_count(), 1, "a small failure must be captured for later recall");

    // …and it is searchable after the fact.
    let hits = stdout(&a.h5i_ok(&["recall", "search", "boom"]));
    assert!(hits.contains("1 capture matched"), "small failure should be searchable:\n{hits}");

    // A subsequent small *successful* command is still passed through unstored,
    // so the store stays free of trivia.
    a.h5i_ok(&["capture", "run", "--", "bash", "-c", "echo all good"]);
    assert_eq!(a.manifest_count(), 1, "small success stays unstored");
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

// ── recall search: query the normalized findings across captures ──────────────

/// A fake `pytest` body whose one failure ⇒ a stable finding (id
/// `tests/t.py::test_pay`, message `assert 0 == 100`, location `tests/t.py`).
/// `"$@"` is echoed so callers can perturb the raw bytes (→ a distinct object)
/// while keeping the failing finding — and thus its fingerprint — identical.
const FAILING_PYTEST: &str = "echo \"args: $@\"\n\
     echo '=== test session starts ==='\n\
     for i in $(seq 1 80); do echo \"tests/t.py::test_$i PASSED some padding to clear the size threshold\"; done\n\
     echo 'FAILED tests/t.py::test_pay - assert 0 == 100'\n\
     echo '=== 1 failed, 80 passed in 4.1s ==='\n\
     exit 1";

/// Install an executable fake tool at `<dir>/bin/<name>` that runs `body`.
fn install_fake_tool(dir: &Path, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let p = bin.join(name);
    std::fs::write(&p, format!("#!/bin/bash\n{body}\n")).unwrap();
    let mut perms = std::fs::metadata(&p).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&p, perms).unwrap();
}

/// Run `h5i <args>` with the clone's `bin/` prepended to PATH (so fake tools
/// resolve). Does not assert success — `capture run` passes through the wrapped
/// command's exit code, which is nonzero for a failing tool.
fn h5i_path(c: &Clone, args: &[&str]) -> Output {
    let path = format!(
        "{}:{}",
        c.dir.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    Command::new(H5I)
        .args(args)
        .env("PATH", path)
        .env_remove("H5I_AGENT")
        // See Clone::h5i: keep the box's env-capture vars from making
        // `capture run` stage to the env spool instead of storing locally.
        .env_remove("H5I_ENV_ID")
        .env_remove("H5I_ENV_POLICY_DIGEST")
        .env_remove("H5I_ENV_CAPTURE_SPOOL")
        .current_dir(&c.dir)
        .output()
        .expect("run h5i")
}

#[test]
fn search_finds_findings_by_text_path_and_metadata() {
    let (_root, a) = single();
    install_fake_tool(&a.dir, "pytest", FAILING_PYTEST);
    h5i_path(&a, &["capture", "run", "--min-bytes", "0", "--", "pytest", "-q"]);
    assert_eq!(a.manifest_count(), 1, "the failing run should be captured");

    // Free-text query on the finding message, rendered with its location.
    let q = stdout(&h5i_path(&a, &["recall", "search", "100"]));
    assert!(q.contains("1 capture matched"), "message query:\n{q}");
    assert!(q.contains("assert 0 == 100"), "shows the finding:\n{q}");
    assert!(q.contains("tests/t.py"), "shows the location:\n{q}");

    // Query on the test id; --path on the location (suffix match).
    assert!(stdout(&h5i_path(&a, &["recall", "search", "pay"])).contains("1 capture matched"));
    assert!(stdout(&h5i_path(&a, &["objects", "search", "--path", "t.py"])).contains("1 capture matched"));

    // Manifest-level structured filters.
    assert!(stdout(&h5i_path(&a, &["recall", "search", "--status", "failed"])).contains("1 capture matched"));
    assert!(stdout(&h5i_path(&a, &["recall", "search", "--tool", "pytest"])).contains("1 capture matched"));
    assert!(stdout(&h5i_path(&a, &["recall", "search", "--severity", "failure"])).contains("1 capture matched"));

    // Filters that exclude: this finding is `failure`, not `error`; and a term
    // that appears nowhere.
    assert!(stdout(&h5i_path(&a, &["recall", "search", "--severity", "error"])).contains("No captured findings match"));
    assert!(stdout(&h5i_path(&a, &["recall", "search", "zzz-not-a-real-token"])).contains("No captured findings match"));
}

#[test]
fn search_validates_enum_and_duration_arguments() {
    let (_root, a) = single();
    // Each invalid enum value is rejected before any work, with a helpful message.
    let bad_sev = a.h5i(&["recall", "search", "--severity", "bogus"]);
    assert!(!bad_sev.status.success());
    assert!(stderr(&bad_sev).contains("invalid --severity"), "{}", stderr(&bad_sev));

    let bad_status = a.h5i(&["recall", "search", "--status", "nope"]);
    assert!(!bad_status.status.success());
    assert!(stderr(&bad_status).contains("invalid --status"), "{}", stderr(&bad_status));

    let bad_kind = a.h5i(&["recall", "search", "--kind", "weird"]);
    assert!(!bad_kind.status.success());
    assert!(stderr(&bad_kind).contains("invalid --kind"), "{}", stderr(&bad_kind));

    // A malformed --since duration is an error too.
    let bad_since = a.h5i(&["recall", "search", "--since", "not-a-duration"]);
    assert!(!bad_since.status.success(), "bad --since should error");
}

#[test]
fn search_since_includes_a_recent_capture() {
    let (_root, a) = single();
    install_fake_tool(&a.dir, "pytest", FAILING_PYTEST);
    h5i_path(&a, &["capture", "run", "--min-bytes", "0", "--", "pytest", "-q"]);
    // A generous window comfortably includes the just-now capture.
    let hits = stdout(&h5i_path(&a, &["recall", "search", "--since", "1h", "--severity", "failure"]));
    assert!(hits.contains("1 capture matched"), "recent capture within 1h:\n{hits}");
}

#[test]
fn search_fingerprint_tracks_recurrence_across_distinct_captures() {
    let (_root, a) = single();
    install_fake_tool(&a.dir, "pytest", FAILING_PYTEST);

    // Two runs with different args ⇒ different raw bytes ⇒ two distinct objects,
    // but the same failing finding ⇒ the same fingerprint.
    h5i_path(&a, &["capture", "run", "--min-bytes", "0", "--", "pytest", "-q", "--seed=1"]);
    h5i_path(&a, &["capture", "run", "--min-bytes", "0", "--", "pytest", "-q", "--seed=2"]);
    assert_eq!(a.manifest_count(), 2, "two distinct captures expected");

    // Pull the fingerprint out of one capture's manifest.
    let id = first_id(&stdout(&a.h5i_ok(&["recall", "objects"]))).expect("a capture id");
    let manifest = stdout(&a.h5i_ok(&["objects", "get", &id, "--manifest"]));
    let v: serde_json::Value = serde_json::from_str(&manifest).expect("manifest json");
    let fp = v["structured"]["findings"][0]["fingerprint"]
        .as_str()
        .expect("a fingerprint")
        .to_string();

    // Searching that fingerprint (by prefix) finds BOTH recurrences.
    let hits = stdout(&a.h5i_ok(&["recall", "search", "--fingerprint", &fp[..fp.len().min(8)]]));
    assert!(hits.contains("2 captures matched"), "fingerprint recurrence:\n{hits}");
}

#[test]
fn search_on_empty_store_reports_no_match() {
    let (_root, a) = single();
    // No captures yet: both a free-text and a no-arg search exit 0 and say so.
    let q = a.h5i_ok(&["recall", "search", "anything"]);
    assert!(stdout(&q).contains("No captured findings match"), "{}", stdout(&q));
    let bare = a.h5i_ok(&["recall", "search"]);
    assert!(stdout(&bare).contains("No captured findings match"), "{}", stdout(&bare));
}

// ── recall object --format: re-observe the captured structured view ───────────

#[test]
fn recall_object_format_reproduces_the_observed_structured_view() {
    let (_root, a) = single();
    install_fake_tool(&a.dir, "pytest", FAILING_PYTEST);

    // What the agent observes at capture time when it asks for the YAML view.
    // (The "▢ h5i object …" pointer goes to stderr, so stdout is pure YAML.)
    let observed = stdout(&h5i_path(
        &a,
        &["capture", "run", "--min-bytes", "0", "--format", "structured", "--", "pytest", "-q"],
    ));
    assert!(observed.contains("tool: pytest"), "capture YAML:\n{observed}");
    assert!(observed.contains("fingerprint:"), "capture YAML:\n{observed}");

    let id = first_id(&stdout(&a.h5i_ok(&["recall", "objects"]))).expect("a capture id");

    // recall object --format yaml reproduces that YAML byte-for-byte, without
    // ever rehydrating the raw bytes.
    let recalled = stdout(&a.h5i_ok(&["recall", "object", &id, "--format", "yaml"]));
    assert_eq!(
        recalled.trim_end(),
        observed.trim_end(),
        "recalled YAML must equal what the agent observed"
    );

    // `structured` is an alias for `yaml`; `compact` reproduces the default view.
    let via_structured = stdout(&a.h5i_ok(&["recall", "object", &id, "--format", "structured"]));
    assert_eq!(via_structured, recalled, "structured is an alias for yaml");
    let compact = stdout(&a.h5i_ok(&["recall", "object", &id, "--format", "compact"]));
    assert!(compact.contains("pytest test failed"), "compact view:\n{compact}");
    assert!(compact.contains("tests/t.py::test_pay"), "compact view:\n{compact}");

    // --format json is valid JSON carrying the same finding.
    let json = stdout(&a.h5i_ok(&["recall", "object", &id, "--format", "json"]));
    let v: serde_json::Value = serde_json::from_str(&json).expect("valid structured json");
    assert_eq!(v["tool"], "pytest");
    assert_eq!(v["findings"][0]["id"], "tests/t.py::test_pay");

    // --format summary still falls back to the legacy free-text summary.
    let summ = stdout(&a.h5i_ok(&["recall", "object", &id, "--format", "summary"]));
    assert!(!summ.trim().is_empty(), "summary format yields text");
}

// ── objects put: ingest a file / stdin (no min-bytes gate) ────────────────────

#[test]
fn put_ingests_a_file_and_rehydrates_losslessly() {
    let (_root, a) = single();
    let body = "line one\nline two with src/widget.rs:7 mentioned\nline three\n";
    std::fs::write(a.dir.join("build.log"), body).unwrap();

    // `objects put` always stores (no size gate) and tags the related file.
    a.h5i_ok(&["objects", "put", "build.log", "--file", "src/widget.rs"]);
    assert_eq!(a.manifest_count(), 1, "put always stores, even tiny input");

    // Rehydrates byte-for-byte (id comes from the listing — the pointer with the
    // id is printed to stderr, not stdout).
    let id = first_id(&stdout(&a.h5i_ok(&["recall", "objects"]))).expect("a put id");
    let raw = stdout(&a.h5i_ok(&["recall", "object", &id]));
    assert_eq!(raw, body, "put → recall object round-trips exactly");

    // Tagged file is queryable via list --file (suffix match).
    let listed = stdout(&a.h5i_ok(&["recall", "objects", "--file", "widget.rs"]));
    assert!(listed.contains("1 object matched"), "tagged file should filter:\n{listed}");
}

#[test]
fn put_reads_from_stdin() {
    use std::io::Write;
    let (_root, a) = single();

    let mut child = Command::new(H5I)
        .args(["objects", "put", "-"])
        .env_remove("H5I_AGENT")
        .env_remove("H5I_ENV_ID")
        .env_remove("H5I_ENV_POLICY_DIGEST")
        .env_remove("H5I_ENV_CAPTURE_SPOOL")
        .current_dir(&a.dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn put -");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"piped payload from stdin\n")
        .unwrap();
    let out = child.wait_with_output().expect("wait");
    assert!(out.status.success(), "put - should succeed: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(a.manifest_count(), 1, "stdin input is stored");

    let id = first_id(&stdout(&a.h5i_ok(&["recall", "objects"]))).expect("a put id");
    let raw = stdout(&a.h5i_ok(&["recall", "object", &id]));
    assert_eq!(raw, "piped payload from stdin\n");
}

// ── objects pin / unpin: protect a blob from gc, then release it ──────────────

#[test]
fn pin_protects_blob_from_ttl_gc_then_unpin_allows_eviction() {
    let (_root, a) = single();
    // Two stored captures (force storage of small output).
    a.h5i_ok(&["capture", "run", "--min-bytes", "0", "--", "bash", "-c", "echo first"]);
    a.h5i_ok(&["capture", "run", "--min-bytes", "0", "--", "bash", "-c", "echo second"]);

    // Collect both ids in listing order (newest first).
    let list = stdout(&a.h5i_ok(&["recall", "objects"]));
    let ids: Vec<String> = list.lines().filter_map(first_id).collect();
    assert_eq!(ids.len(), 2, "two captures listed:\n{list}");
    let (pinned, other) = (&ids[0], &ids[1]);

    // Pin one; `gc --ttl 0s` evicts every unpinned referenced blob (age ≥ 0 is
    // always true), so the other is evicted while the pinned one survives.
    a.h5i_ok(&["objects", "pin", pinned]);
    let gc = stdout(&a.h5i_ok(&["objects", "gc", "--ttl", "0s"]));
    assert!(gc.contains("1 pinned"), "one blob pinned:\n{gc}");

    // Pinned raw is still present; the unpinned one's raw is gone (summary kept).
    assert!(a.h5i(&["recall", "object", pinned]).status.success(), "pinned raw survives");
    let evicted = a.h5i(&["recall", "object", other]);
    assert!(!evicted.status.success(), "unpinned raw evicted");
    assert!(stderr(&evicted).contains("absent"), "absent message:\n{}", stderr(&evicted));

    // Unpin, gc again → the previously-pinned blob is now evictable too.
    a.h5i_ok(&["objects", "unpin", pinned]);
    a.h5i_ok(&["objects", "gc", "--ttl", "0s"]);
    assert!(!a.h5i(&["recall", "object", pinned]).status.success(), "unpinned blob now evicted");
}

// ── objects fsck: cross-check manifests against the store ─────────────────────

#[test]
fn fsck_reports_absent_blob_after_eviction() {
    let (_root, a) = single();
    a.h5i_ok(&["capture", "run", "--min-bytes", "0", "--", "bash", "-c", "echo present"]);

    // Healthy: one manifest, nothing absent or orphaned.
    let clean = stdout(&a.h5i_ok(&["objects", "fsck"]));
    assert!(clean.contains("1 manifests · 0 absent · 0 orphan"), "clean fsck:\n{clean}");

    // Evict the raw (manifest stays) → fsck now flags it absent.
    a.h5i_ok(&["objects", "gc", "--ttl", "0s"]);
    let after = stdout(&a.h5i_ok(&["objects", "fsck"]));
    assert!(after.contains("1 absent"), "fsck flags the evicted blob:\n{after}");
    assert!(after.contains("absent"), "{after}");
}

// ── objects filters: list / verify the built-in rule set ──────────────────────

#[test]
fn filters_lists_builtin_rules_and_verifies_golden_tests() {
    let (_root, a) = single();
    let listed = stdout(&a.h5i_ok(&["objects", "filters"]));
    assert!(listed.contains("built-in command filters"), "filters listing:\n{listed}");

    // The golden self-tests for the declarative rules must pass.
    let verified = stdout(&a.h5i_ok(&["objects", "filters", "--verify"]));
    assert!(verified.contains("passed"), "golden tests should pass:\n{verified}");
}

// ── search rendering: --kind, --limit truncation, per-capture finding cap ──────

#[test]
fn search_kind_filter_and_limit_truncation() {
    let (_root, a) = single();
    install_fake_tool(&a.dir, "pytest", FAILING_PYTEST);
    // Three distinct captures of the same failing finding.
    for seed in ["1", "2", "3"] {
        h5i_path(&a, &["capture", "run", "--min-bytes", "0", "--", "pytest", "-q", seed]);
    }
    assert_eq!(a.manifest_count(), 3);

    // --kind selects the test-failure findings across all three captures.
    let by_kind = stdout(&a.h5i_ok(&["recall", "search", "--kind", "test_failure"]));
    assert!(by_kind.contains("3 captures matched"), "kind filter:\n{by_kind}");

    // --limit caps the number of captures shown and says so.
    let limited = stdout(&a.h5i_ok(&["recall", "search", "--kind", "test_failure", "--limit", "2"]));
    assert!(limited.contains("showing 2"), "limit truncation note:\n{limited}");
}

const MULTI_FAIL_PYTEST: &str = "echo '=== test session starts ==='\n\
     for i in $(seq 1 60); do echo \"tests/t.py::test_pass_$i PASSED padding padding padding\"; done\n\
     for i in $(seq 1 10); do echo \"FAILED tests/t.py::test_case_$i - assert $i == 0\"; done\n\
     echo '=== 10 failed, 60 passed in 9.9s ==='\n\
     exit 1";

#[test]
fn search_caps_findings_per_capture_with_more_marker() {
    let (_root, a) = single();
    install_fake_tool(&a.dir, "pytest", MULTI_FAIL_PYTEST);
    h5i_path(&a, &["capture", "run", "--min-bytes", "0", "--", "pytest", "-q"]);

    // 10 findings in one capture; search shows at most 8 then a "+N more" marker.
    let hits = stdout(&a.h5i_ok(&["recall", "search", "--severity", "failure"]));
    assert!(hits.contains("1 capture matched"), "one capture:\n{hits}");
    assert!(hits.contains("10 findings"), "all ten findings counted:\n{hits}");
    assert!(hits.contains("more finding"), "per-capture cap shows a +N more marker:\n{hits}");
}
