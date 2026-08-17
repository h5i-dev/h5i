//! TEMPORARY adversarial review probe 2. Delete after the review.

use h5i_runner::source::export_bundle;
use std::path::Path;
use std::process::Command;

fn g(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn seed(work: &Path) -> String {
    std::fs::create_dir_all(work).unwrap();
    g(work, &["init", "--quiet", "."]);
    g(work, &["config", "user.email", "t@e.com"]);
    g(work, &["config", "user.name", "T"]);
    std::fs::write(work.join("a.txt"), b"one").unwrap();
    g(work, &["add", "-A"]);
    g(work, &["commit", "--quiet", "-m", "one"]);
    g(work, &["rev-parse", "HEAD"])
}

fn receive(host: &Path, bundle: &Path, base: &str, dir: &Path) -> String {
    std::fs::create_dir_all(dir).unwrap();
    g(dir, &["init", "--quiet", "--bare", "."]);
    g(
        dir,
        &[
            "fetch", "--quiet", "--no-tags", "--end-of-options",
            &host.to_string_lossy(), &format!("{base}:refs/h5i/base"),
        ],
    );
    g(
        dir,
        &[
            "fetch", "--quiet", "--no-tags", "--end-of-options",
            &bundle.to_string_lossy(), "refs/h5i/export-src:refs/h5i/tip",
        ],
    );
    g(dir, &["rev-parse", "refs/h5i/tip"])
}

/// The realistic runner cycle: run, propose (export), run again, propose again.
#[test]
fn two_export_cycles_with_an_exec_in_between() {
    let d = tempfile::tempdir().unwrap();
    let work = d.path().join("work");
    let base = seed(&work);

    // cycle 1: the agent writes a file
    std::fs::write(work.join("b.txt"), b"first").unwrap();
    let out1 = d.path().join("e1.bundle");
    let e1 = export_bundle(&work, &base, &out1).expect("export 1");
    let t1 = receive(&work, &out1, &base, &d.path().join("q1"));
    eprintln!(
        "CYCLE1 tree: {:?}",
        g(&d.path().join("q1"), &["ls-tree", "-r", "--name-only", &t1])
    );

    // what the *next* exec inside the box sees
    eprintln!(
        "AFTER EXPORT 1, in-box `git status --porcelain`:\n{}",
        g(&work, &["status", "--porcelain"])
    );
    eprintln!(
        "AFTER EXPORT 1, in-box `git diff --stat HEAD`:\n{}",
        g(&work, &["diff", "--stat", "HEAD"])
    );

    // A perfectly ordinary thing for an agent to do next.
    g(&work, &["add", "-A"]);
    g(&work, &["commit", "--quiet", "-m", "agent commit"]);
    eprintln!(
        "AFTER an in-box `git add -A && git commit`, tree is:\n{}",
        g(&work, &["ls-tree", "-r", "--name-only", "HEAD"])
    );
    eprintln!("worktree still has b.txt: {}", work.join("b.txt").exists());

    // cycle 2
    std::fs::write(work.join("c.txt"), b"second").unwrap();
    let out2 = d.path().join("e2.bundle");
    let e2 = export_bundle(&work, &base, &out2).expect("export 2");
    let t2 = receive(&work, &out2, &base, &d.path().join("q2"));
    eprintln!(
        "CYCLE2 tree: {:?}  has_changes={} (e1 tip {} e2 tip {})",
        g(&d.path().join("q2"), &["ls-tree", "-r", "--name-only", &t2]),
        e2.has_changes,
        e1.tip_commit,
        e2.tip_commit
    );
}

/// Same, without the in-box commit: just export twice.
#[test]
fn export_twice_in_a_row() {
    let d = tempfile::tempdir().unwrap();
    let work = d.path().join("work");
    let base = seed(&work);
    std::fs::write(work.join("b.txt"), b"first").unwrap();
    let out1 = d.path().join("e1.bundle");
    export_bundle(&work, &base, &out1).expect("export 1");
    let out2 = d.path().join("e2.bundle");
    let e2 = export_bundle(&work, &base, &out2).expect("export 2");
    let t2 = receive(&work, &out2, &base, &d.path().join("q2"));
    eprintln!(
        "TWICE tree: {:?} has_changes={}",
        g(&d.path().join("q2"), &["ls-tree", "-r", "--name-only", &t2]),
        e2.has_changes
    );
}

/// A no-change export whose bundle is then read by a receiver that does NOT
/// already hold the tip. The header omits the prerequisite, so nothing tells
/// the reader it needs it.
#[test]
fn no_change_bundle_read_without_the_base() {
    let d = tempfile::tempdir().unwrap();
    let work = d.path().join("work");
    let base = seed(&work);
    let out = d.path().join("e.bundle");
    let e = export_bundle(&work, &base, &out).expect("export");
    assert!(!e.has_changes);
    let q = d.path().join("q");
    std::fs::create_dir_all(&q).unwrap();
    g(&q, &["init", "--quiet", "--bare", "."]);
    let r = Command::new("git")
        .args([
            "fetch", "--no-tags", "--end-of-options",
            &out.to_string_lossy(), "refs/h5i/export-src:refs/h5i/tip",
        ])
        .current_dir(&q)
        .output()
        .unwrap();
    eprintln!(
        "EMPTY-PACK into a repo without the base: status={} stderr={}",
        r.status,
        String::from_utf8_lossy(&r.stderr).trim()
    );
    let ok = Command::new("git")
        .args(["cat-file", "-e", &format!("{}^{{commit}}", e.tip_commit)])
        .current_dir(&q)
        .status()
        .unwrap();
    eprintln!("and the tip object is present afterwards: {}", ok.success());
}
