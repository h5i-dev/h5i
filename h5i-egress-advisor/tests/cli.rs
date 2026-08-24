//! End to end: the binary, against receipts shaped like h5i's own.
//!
//! The fixtures in `tests/fixtures` are the two file shapes a user actually
//! has — an `h5i box export` bundle and a live append-only log, torn tail and
//! all. `--box` is exercised against a store built here in the same layout h5i
//! uses (`<repo>/.git/.h5i/env/<agent>/<slug>`), because "find my box" is the
//! half of this tool that has nothing to do with parsing.

use std::path::{Path, PathBuf};
use std::process::Command;

const EXE: &str = env!("CARGO_BIN_EXE_h5i-egress-advisor");

struct Run {
    stdout: String,
    stderr: String,
    code: i32,
}

fn run(args: &[&str]) -> Run {
    run_in(args, Path::new(env!("CARGO_MANIFEST_DIR")))
}

fn run_in(args: &[&str], cwd: &Path) -> Run {
    let out = Command::new(EXE)
        .args(args)
        .current_dir(cwd)
        // Colour would land in every assertion below; the tool honours this
        // as well as its own --no-color.
        .env("NO_COLOR", "1")
        .output()
        .expect("run the advisor");
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code().unwrap_or(-1),
    }
}

fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn a_bundle_becomes_a_question_for_a_human() {
    let r = run(&[&fixture("bundle.json")]);
    assert_eq!(r.code, 1, "refusals found: {}", r.stderr);
    // Grouped across both runs that hit it, loudest first.
    assert!(r.stdout.contains("registry.npmjs.org:443"), "{}", r.stdout);
    assert!(r.stdout.contains("11 refused"), "{}", r.stdout);
    assert!(r.stdout.contains("a3f1c2, 9b04de"), "{}", r.stdout);
    assert!(r.stdout.contains("h5i box allow registry.npmjs.org"));
    // The beacon and the bare address are named, and neither gets a command.
    assert!(r.stdout.contains("no suggestion: this looks like a beacon"));
    assert!(!r.stdout.contains("h5i box allow telemetry.example.net"));
    assert!(!r.stdout.contains("h5i box allow 203.0.113.7"));
    // A non-web port keeps the port in the rule.
    assert!(
        r.stdout
            .contains("h5i box allow cache.internal.example.com:6379")
    );
    // The run with no egress block is still part of the denominator.
    assert!(
        r.stdout.contains("of 2 run(s) with egress verdicts"),
        "{}",
        r.stdout
    );
}

#[test]
fn the_live_log_shape_reads_the_same_way() {
    let r = run(&[&fixture("log.jsonl")]);
    assert_eq!(r.code, 1);
    assert!(r.stdout.contains("files.pythonhosted.org:443"));
    assert!(r.stdout.contains("h5i box allow pypi.org"));
    // pypi was allowed in the second run and refused in the first: say so.
    assert!(r.stdout.contains("also reached 2 time(s)"), "{}", r.stdout);
    // The clamped host list is a warning, not a silent short report.
    assert!(r.stdout.contains("clamped"), "{}", r.stdout);
    // The torn final line is tolerated rather than reported as corruption.
    assert!(!r.stdout.contains("unreadable record"), "{}", r.stdout);
    // No tier in a raw log: it must not claim `h5i box allow` is the answer.
    assert!(
        r.stdout.contains("does not record the isolation tier"),
        "{}",
        r.stdout
    );
}

#[test]
fn json_carries_the_whole_report() {
    let r = run(&[&fixture("bundle.json"), "--json"]);
    assert_eq!(r.code, 1);
    let v: serde_json::Value = serde_json::from_str(&r.stdout).expect("valid json");
    assert_eq!(v["schema"], 1);
    assert_eq!(v["box"]["isolation_claim"], "container");
    assert_eq!(v["box"]["allow_reach"], "proxy");
    assert_eq!(v["totals"]["denied"], 14);
    assert_eq!(v["destinations"][0]["host"], "registry.npmjs.org");
    assert_eq!(v["destinations"][0]["denied"], 11);
    assert_eq!(v["destinations"][0]["example_cmd"], "npm install");
    assert_eq!(
        v["destinations"][0]["suggestion"]["command"],
        "h5i box allow registry.npmjs.org"
    );
}

#[test]
fn toml_emits_a_block_for_the_boxes_box_allow_cannot_reach() {
    let r = run(&[&fixture("bundle.json"), "--toml", "--profile", "review"]);
    assert_eq!(r.code, 1);
    assert!(r.stdout.contains("[profile.review.net]"));
    assert!(r.stdout.contains("\"registry.npmjs.org\","));
    assert!(r.stdout.contains("\"cache.internal.example.com:6379\","));
    // Declined destinations are listed as comments, never as entries.
    assert!(!r.stdout.contains("\"telemetry.example.net\""));
    assert!(r.stdout.contains("#   telemetry.example.net:443"));
    // It must be a table you can paste: every non-comment line belongs to it.
    let body: Vec<&str> = r
        .stdout
        .lines()
        .filter(|l| !l.trim_start().starts_with('#') && !l.trim().is_empty())
        .collect();
    assert_eq!(body[0], "[profile.review.net]");
    assert_eq!(body[body.len() - 1], "]");
}

#[test]
fn min_drops_the_long_tail() {
    let r = run(&[&fixture("bundle.json"), "--min", "5"]);
    assert!(r.stdout.contains("registry.npmjs.org"));
    assert!(!r.stdout.contains("telemetry.example.net"));
}

#[test]
fn a_receipt_with_nothing_refused_exits_clean() {
    let dir = scratch("clean");
    let log = dir.join("receipt.jsonl");
    std::fs::write(
        &log,
        "{\"id\":\"aa\",\"env_id\":\"env/claude/x\",\"egress\":{\"allowed\":4,\"denied\":0,\
         \"hosts\":[{\"host\":\"api.github.com\",\"port\":443,\"allowed\":4,\"denied\":0}]}}\n",
    )
    .unwrap();
    let r = run(&[log.to_str().unwrap()]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert!(r.stdout.contains("Nothing was refused"), "{}", r.stdout);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_box_is_found_in_the_store_the_way_h5i_lays_it_out() {
    let repo = scratch("store");
    let env = repo.join(".git/.h5i/env/claude/mybox");
    std::fs::create_dir_all(&env).unwrap();
    std::fs::write(
        env.join("manifest.json"),
        r#"{"id":"env/claude/mybox","agent":"claude","slug":"mybox","profile":"hardened",
            "isolation_claim":"supervised","backend":"worktree"}"#,
    )
    .unwrap();
    std::fs::copy(fixture("log.jsonl"), env.join("receipt.jsonl")).unwrap();
    // A nested directory, to prove the walk upwards works from anywhere.
    let deep = repo.join("src/inner");
    std::fs::create_dir_all(&deep).unwrap();

    let r = run_in(&["--box", "mybox"], &deep);
    assert_eq!(r.code, 1, "{}{}", r.stdout, r.stderr);
    assert!(r.stdout.contains("files.pythonhosted.org"), "{}", r.stdout);
    // The manifest supplies what the log cannot: profile and tier.
    assert!(r.stdout.contains("profile hardened"), "{}", r.stdout);
    assert!(
        r.stdout.contains("does not reach a supervised box"),
        "{}",
        r.stdout
    );
    // And --toml then knows which profile to write.
    let t = run_in(&["--box", "claude/mybox", "--toml"], &deep);
    assert!(t.stdout.contains("[profile.hardened.net]"), "{}", t.stdout);

    let missing = run_in(&["--box", "nope"], &deep);
    assert_eq!(missing.code, 2);
    assert!(
        missing.stderr.contains("claude/mybox"),
        "{}",
        missing.stderr
    );
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn a_directory_resolves_to_the_receipt_inside_it() {
    let r = run(&[&format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR"))]);
    // fixtures/ holds no receipt.json or receipt.jsonl at its top level.
    assert_eq!(r.code, 2);
    assert!(r.stderr.contains("not an h5i receipt"), "{}", r.stderr);

    let dir = scratch("bundle-dir");
    std::fs::copy(fixture("bundle.json"), dir.join("receipt.json")).unwrap();
    let r = run(&[dir.to_str().unwrap()]);
    assert_eq!(r.code, 1, "{}{}", r.stdout, r.stderr);
    assert!(r.stdout.contains("registry.npmjs.org"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn what_is_not_a_receipt_is_said_plainly() {
    let dir = scratch("junk");
    let f = dir.join("notes.txt");
    std::fs::write(&f, "these are not receipts\n").unwrap();
    let r = run(&[f.to_str().unwrap()]);
    assert_eq!(r.code, 2);
    assert!(r.stderr.contains("not an h5i receipt"), "{}", r.stderr);

    let missing = run(&["/nonexistent/receipt.json"]);
    assert_eq!(missing.code, 2);
    assert!(missing.stderr.contains("/nonexistent/receipt.json"));

    let nothing = run(&[]);
    assert_eq!(nothing.code, 2);
    assert!(nothing.stderr.contains("pass a receipt path"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn help_says_what_it_will_and_will_not_do() {
    let r = run(&["--help"]);
    assert_eq!(r.code, 0);
    assert!(r.stdout.contains("--toml"));
    assert!(r.stdout.contains("Nothing it prints is executed"));
}

/// A unique directory under the crate's target dir — no temp-file dependency
/// for a tool whose whole point is a small, auditable surface.
fn scratch(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-scratch")
        .join(format!("{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
