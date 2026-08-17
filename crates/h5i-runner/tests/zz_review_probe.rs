//! TEMPORARY adversarial review probe. Delete after the review.

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
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn try_g(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git");
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
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

/// Read the bundle the way the receiving side does: a bare repo seeded with the
/// base from a trusted copy, then the untrusted bundle.
fn receive(host: &Path, bundle: &Path, base: &str, dir: &Path) -> Result<String, String> {
    std::fs::create_dir_all(dir).unwrap();
    g(dir, &["init", "--quiet", "--bare", "."]);
    try_g(
        dir,
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-write-fetch-head",
            "--end-of-options",
            &host.to_string_lossy(),
            &format!("{base}:refs/h5i/base"),
        ],
    )?;
    try_g(
        dir,
        &[
            "-c",
            "transfer.fsckObjects=true",
            "-c",
            "fetch.fsckObjects=true",
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-write-fetch-head",
            "--end-of-options",
            &bundle.to_string_lossy(),
            "refs/h5i/export-src:refs/h5i/tip",
        ],
    )?;
    try_g(dir, &["rev-parse", "refs/h5i/tip"])
}

#[test]
fn probe_no_changes_at_all() {
    let d = tempfile::tempdir().unwrap();
    let work = d.path().join("work");
    let base = seed(&work);
    // Nothing happened in the box.
    let out = d.path().join("e.bundle");
    let e = export_bundle(&work, &base, &out).expect("export");
    eprintln!(
        "NO-CHANGES: has_changes={} tip={} base={} bytes={}",
        e.has_changes, e.tip_commit, base, e.bytes
    );
    let hdr = std::fs::read(&out).unwrap();
    eprintln!(
        "NO-CHANGES header: {:?}",
        String::from_utf8_lossy(&hdr[..hdr.len().min(120)])
    );
    match receive(&work, &out, &base, &d.path().join("q")) {
        Ok(tip) => eprintln!("NO-CHANGES receive OK tip={tip}"),
        Err(e) => eprintln!("NO-CHANGES receive FAILED: {e}"),
    }
    // and git's own opinion of the file
    eprintln!(
        "NO-CHANGES verify: {:?}",
        try_g(&work, &["bundle", "verify", &out.to_string_lossy()])
    );
}

#[test]
fn probe_ordinary_change() {
    let d = tempfile::tempdir().unwrap();
    let work = d.path().join("work");
    let base = seed(&work);
    std::fs::write(work.join("b.txt"), b"new work").unwrap();
    let out = d.path().join("e.bundle");
    let e = export_bundle(&work, &base, &out).expect("export");
    eprintln!("ORDINARY: has_changes={} bytes={}", e.has_changes, e.bytes);
    match receive(&work, &out, &base, &d.path().join("q")) {
        Ok(tip) => {
            assert_eq!(tip, e.tip_commit);
            eprintln!("ORDINARY receive OK");
        }
        Err(err) => panic!("ORDINARY receive FAILED: {err}"),
    }
}

#[test]
fn probe_merge_tip_two_parents() {
    let d = tempfile::tempdir().unwrap();
    let work = d.path().join("work");
    let base = seed(&work);
    g(&work, &["checkout", "--quiet", "-b", "side"]);
    std::fs::write(work.join("s.txt"), b"side").unwrap();
    g(&work, &["add", "-A"]);
    g(&work, &["commit", "--quiet", "-m", "side"]);
    let side = g(&work, &["rev-parse", "HEAD"]);
    g(&work, &["checkout", "--quiet", "-"]);
    std::fs::write(work.join("m.txt"), b"main").unwrap();
    g(&work, &["add", "-A"]);
    g(&work, &["commit", "--quiet", "-m", "main"]);
    let mainc = g(&work, &["rev-parse", "HEAD"]);
    g(&work, &["merge", "--quiet", "--no-ff", "-m", "merge", &side]);
    let merged = g(&work, &["rev-parse", "HEAD"]);
    eprintln!("MERGE: base={base} side={side} main={mainc} merged={merged}");
    // export against the original base, which is an ancestor of both parents
    let out = d.path().join("e.bundle");
    let e = export_bundle(&work, &base, &out).expect("export");
    eprintln!("MERGE: has_changes={} tip={}", e.has_changes, e.tip_commit);
    match receive(&work, &out, &base, &d.path().join("q")) {
        Ok(tip) => eprintln!("MERGE receive OK tip={tip} expected={}", e.tip_commit),
        Err(err) => eprintln!("MERGE receive FAILED: {err}"),
    }
}

#[test]
fn probe_base_not_an_ancestor() {
    let d = tempfile::tempdir().unwrap();
    let work = d.path().join("work");
    let base = seed(&work);
    // An orphan line: the tip does not descend from `base`.
    g(&work, &["checkout", "--quiet", "--orphan", "other"]);
    std::fs::write(work.join("z.txt"), b"z").unwrap();
    g(&work, &["add", "-A"]);
    g(&work, &["commit", "--quiet", "-m", "orphan"]);
    let out = d.path().join("e.bundle");
    let e = export_bundle(&work, &base, &out).expect("export");
    let hdr = std::fs::read(&out).unwrap();
    eprintln!(
        "NON-ANCESTOR header: {:?}",
        String::from_utf8_lossy(&hdr[..hdr.len().min(120)])
    );
    eprintln!("NON-ANCESTOR tip={} base={base}", e.tip_commit);
    match receive(&work, &out, &base, &d.path().join("q")) {
        Ok(tip) => eprintln!("NON-ANCESTOR receive OK tip={tip}"),
        Err(err) => eprintln!("NON-ANCESTOR receive FAILED: {err}"),
    }
}

#[test]
fn probe_side_effects_on_the_box_repo() {
    let d = tempfile::tempdir().unwrap();
    let work = d.path().join("work");
    let base = seed(&work);
    std::fs::write(work.join("b.txt"), b"new").unwrap();
    let idx_before = std::fs::metadata(work.join(".git/index")).unwrap();
    let head_before = g(&work, &["rev-parse", "HEAD"]);
    let out = d.path().join("e.bundle");
    let _ = export_bundle(&work, &base, &out).expect("export");
    let idx_after = std::fs::metadata(work.join(".git/index")).unwrap();
    let head_after = g(&work, &["rev-parse", "HEAD"]);
    eprintln!(
        "SIDE-EFFECT: index mtime changed={} len {}->{}; HEAD {}->{}",
        idx_before.modified().unwrap() != idx_after.modified().unwrap(),
        idx_before.len(),
        idx_after.len(),
        head_before,
        head_after
    );
    eprintln!(
        "SIDE-EFFECT: git status after export:\n{}",
        g(&work, &["status", "--porcelain"])
    );
    eprintln!(
        "SIDE-EFFECT: reflog:\n{}",
        try_g(&work, &["reflog", "-3"]).unwrap_or_default()
    );
}

#[test]
fn probe_gitignored_and_deleted() {
    let d = tempfile::tempdir().unwrap();
    let work = d.path().join("work");
    let base = seed(&work);
    std::fs::write(work.join(".gitignore"), b"secret.txt\nbuild/\n").unwrap();
    g(&work, &["add", "-A"]);
    g(&work, &["commit", "--quiet", "-m", "ignore"]);
    let base2 = g(&work, &["rev-parse", "HEAD"]);
    std::fs::write(work.join("secret.txt"), b"agent output nobody sees").unwrap();
    std::fs::remove_file(work.join("a.txt")).unwrap();
    std::fs::write(work.join("kept.txt"), b"kept").unwrap();
    let out = d.path().join("e.bundle");
    let e = export_bundle(&work, &base2, &out).expect("export");
    let q = d.path().join("q");
    let tip = receive(&work, &out, &base2, &q).expect("receive");
    let names = g(&q, &["ls-tree", "-r", "--name-only", &tip]);
    eprintln!("IGNORED/DELETED: tree = {names:?} (has_changes={})", e.has_changes);
    let _ = base;
}

#[test]
fn probe_gitlink_and_alternates() {
    let d = tempfile::tempdir().unwrap();
    // A second repo on the "runner host" the box should not be able to reach.
    let other = d.path().join("other");
    let other_head = seed(&other);
    std::fs::write(other.join("SECRET.txt"), b"runner side secret").unwrap();
    g(&other, &["add", "-A"]);
    g(&other, &["commit", "--quiet", "-m", "secret"]);
    let secret_commit = g(&other, &["rev-parse", "HEAD"]);

    let work = d.path().join("work");
    let base = seed(&work);
    // The box writes an alternates file pointing at the other repo.
    std::fs::write(
        work.join(".git/objects/info/alternates"),
        format!("{}\n", other.join(".git/objects").display()),
    )
    .unwrap();
    // and points a tree entry at an object it does not own
    let entry = format!("160000 commit {secret_commit}\tsub\n");
    let mktree = Command::new("git")
        .args(["mktree"])
        .current_dir(&work)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            use std::io::Write;
            c.stdin.as_mut().unwrap().write_all(entry.as_bytes())?;
            c.wait_with_output()
        })
        .unwrap();
    eprintln!(
        "ALTERNATES: mktree -> {:?}",
        String::from_utf8_lossy(&mktree.stdout).trim()
    );
    let out = d.path().join("e.bundle");
    match export_bundle(&work, &base, &out) {
        Ok(e) => eprintln!("ALTERNATES export ok, tip={} ", e.tip_commit),
        Err(e) => eprintln!("ALTERNATES export err: {e}"),
    }
    let _ = other_head;
}
