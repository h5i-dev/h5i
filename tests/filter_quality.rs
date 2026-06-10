//! Quality tests for the token-reduction filter.
//!
//! These assert the two properties that matter for the feature's purpose:
//!
//!   1. **It actually cuts tokens.** For realistic, sizeable outputs the summary
//!      is a large fraction smaller than the raw, measured with the same
//!      tokenizer h5i records in manifests.
//!   2. **It keeps the information that matters.** The signal an agent needs
//!      (failing test names, panic/assertion values, error messages, the buried
//!      error in a noisy log) survives into the summary — and the *full* raw is
//!      always exactly recoverable from the object store, so nothing is ever
//!      truly lost.
//!
//! Run with: cargo test --test filter_quality

use h5i_core::objects::{self, CaptureOptions};
use h5i_core::token_filter::{filter, FilterConfig, OutputKind};

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

fn cfg(kind: OutputKind, cmd: Option<Vec<String>>) -> FilterConfig {
    FilterConfig { kind, cmd, ..Default::default() }
}

/// Assert the summary is at most `max_ratio` of the raw token count, and that
/// the raw was genuinely large (so the ratio is meaningful). Returns (raw, sum).
fn assert_token_cut(res: &h5i_core::token_filter::FilterResult, min_raw: usize, max_ratio: f64) {
    let raw = res.raw_tokens.expect("raw tokens (tiktoken)");
    let sum = res.summary_tokens.expect("summary tokens (tiktoken)");
    assert!(raw >= min_raw, "fixture too small to be meaningful: {raw} raw tokens");
    assert!(
        (sum as f64) <= (raw as f64) * max_ratio,
        "insufficient reduction: {sum} summary tokens is > {:.0}% of {raw} raw",
        max_ratio * 100.0
    );
    assert!(sum <= raw, "summary must never inflate tokens ({sum} > {raw})");
}

fn keeps(res: &h5i_core::token_filter::FilterResult, needles: &[&str]) {
    for n in needles {
        assert!(
            res.summary.contains(n),
            "summary dropped required signal {n:?}\n--- summary ---\n{}",
            res.summary
        );
    }
}

fn drops(res: &h5i_core::token_filter::FilterResult, needles: &[&str]) {
    for n in needles {
        assert!(
            !res.summary.contains(n),
            "summary kept noise it should drop {n:?}\n--- summary ---\n{}",
            res.summary
        );
    }
}

// ── 1+2: pytest failure ───────────────────────────────────────────────────────

#[test]
fn pytest_failure_cuts_tokens_and_keeps_failures() {
    let mut raw = String::from("============ test session starts ============\n");
    for i in 0..200 {
        raw.push_str(&format!("tests/test_mod.py::test_{i} PASSED\n"));
    }
    raw.push_str("=================== FAILURES ===================\n");
    raw.push_str("__________________ test_payments __________________\n");
    raw.push_str("    def test_payments():\n>       assert charge(100) == 100\n");
    raw.push_str("E       assert 0 == 100\n");
    raw.push_str("=============== short test summary info ===============\n");
    raw.push_str("FAILED tests/test_mod.py::test_payments - assert 0 == 100\n");
    raw.push_str("============ 1 failed, 200 passed in 9.10s ============\n");

    let res = filter(&raw, &cfg(OutputKind::Auto, Some(argv(&["pytest", "-q"]))));
    keeps(&res, &["1 failed, 200 passed", "FAILED tests/test_mod.py::test_payments", "assert 0 == 100"]);
    drops(&res, &["test_5 PASSED"]); // bulk of passing noise is gone
    assert_token_cut(&res, 300, 0.35);
}

#[test]
fn pytest_all_pass_keeps_count_and_is_tiny() {
    let mut raw = String::from("============ test session starts ============\n");
    for i in 0..300 {
        raw.push_str(&format!("tests/test_mod.py::test_{i} PASSED\n"));
    }
    raw.push_str("============ 300 passed in 5.0s ============\n");
    let res = filter(&raw, &cfg(OutputKind::Auto, Some(argv(&["pytest"]))));
    keeps(&res, &["300 passed"]); // the one useful fact survives
    assert!(res.summary.lines().count() <= 2, "all-pass should be ~1 line");
    assert_token_cut(&res, 400, 0.05);
}

// ── 1+2: cargo test failure ───────────────────────────────────────────────────

#[test]
fn cargo_failure_cuts_tokens_keeps_panic_and_label() {
    let mut raw = String::new();
    for i in 0..60 {
        raw.push_str(&format!("   Compiling crate_{i} v0.1.0\n"));
    }
    raw.push_str("running 90 tests\n");
    for i in 0..88 {
        raw.push_str(&format!("test mod::t_{i} ... ok\n"));
    }
    raw.push_str("test mod::auth ... FAILED\n\nfailures:\n\n---- mod::auth stdout ----\n");
    raw.push_str("thread 'mod::auth' panicked at src/auth.rs:55:9:\n");
    raw.push_str("assertion `left == right` failed\n  left: 401\n  right: 200\n\n");
    raw.push_str("test result: FAILED. 88 passed; 1 failed; 0 ignored\n");
    raw.push_str("error: test failed, to get more output, run again\n");

    let res = filter(&raw, &cfg(OutputKind::Auto, Some(argv(&["cargo", "test"]))));
    keeps(&res, &["test failed", "panicked at src/auth.rs:55:9", "left: 401", "right: 200"]);
    drops(&res, &["Compiling crate_3", "mod::t_5 ... ok"]);
    assert!(!res.summary.contains("Cargo test: ok"), "must not mislabel a failure as ok");
    assert_token_cut(&res, 300, 0.5);
}

// ── 1+2: JSON payload ─────────────────────────────────────────────────────────

#[test]
fn json_payload_cuts_tokens_keeps_error_fields() {
    let mut rows = String::new();
    for i in 0..400 {
        rows.push_str(&format!("{{\"id\":{i},\"name\":\"item-{i}\",\"ok\":true}},"));
    }
    let raw = format!(
        "{{\"status\":\"error\",\"code\":503,\"message\":\"db timeout after 30s\",\"rows\":[{}]}}",
        rows.trim_end_matches(',')
    );
    let res = filter(&raw, &cfg(OutputKind::Json, None));
    assert_eq!(res.kind, OutputKind::Json);
    keeps(&res, &["status", "db timeout after 30s", "code"]);
    assert_token_cut(&res, 400, 0.2);
}

// ── 1+2: noisy log with a buried error (template folding) ─────────────────────

#[test]
fn noisy_log_folds_hard_but_keeps_the_buried_error() {
    let mut raw = String::new();
    for i in 0..500 {
        raw.push_str(&format!("2026-06-05T10:00:{} INFO handled request {i} in {}ms\n", i % 60, i % 9));
    }
    raw.push_str("2026-06-05T10:05:01 ERROR db connection pool exhausted at pool.rs:88\n");
    for i in 500..1000 {
        raw.push_str(&format!("2026-06-05T10:06:{} INFO handled request {i} ok\n", i % 60));
    }
    let res = filter(&raw, &cfg(OutputKind::Log, None));
    keeps(&res, &["ERROR db connection pool exhausted at pool.rs:88"]);
    assert!(res.summary.contains("(×"), "repeated lines should fold");
    assert!(res.summary.lines().count() < 10, "should collapse to a handful of lines");
    assert_token_cut(&res, 1000, 0.05);
}

// ── 1+2: declarative rule (gcc) ────────────────────────────────────────────────

#[test]
fn gcc_rule_cuts_tokens_keeps_errors_drops_include_chain() {
    // The gcc rule strips include-chain lines (and "N warnings generated"),
    // which is the bulk in heavily-templated C/C++ builds.
    let mut raw = String::new();
    for i in 0..60 {
        raw.push_str(&format!("In file included from /usr/include/h{i}.h:1:\n"));
        raw.push_str("                 from main.c:1:\n");
    }
    raw.push_str("main.c:10:5: error: use of undeclared identifier 'foo'\n");
    raw.push_str("main.c:15:12: warning: unused variable 'x' [-Wunused-variable]\n");
    raw.push_str("2 warnings generated.\n");

    let res = filter(&raw, &cfg(OutputKind::Auto, Some(argv(&["gcc", "-O2", "main.c"]))));
    keeps(&res, &["error: use of undeclared identifier 'foo'", "warning: unused variable 'x'"]);
    drops(&res, &["In file included from", "2 warnings generated"]);
    assert_token_cut(&res, 200, 0.5);
}

// ── 1+2: unknown command / generic with buried errors ─────────────────────────

#[test]
fn generic_output_keeps_errors_anywhere() {
    let mut raw = String::new();
    for i in 0..600 {
        raw.push_str(&format!("step {i}: doing some unremarkable work\n"));
    }
    raw.push_str("FATAL: out of memory while allocating buffer\n");
    for i in 0..200 {
        raw.push_str(&format!("cleanup task {i} done\n"));
    }
    let res = filter(&raw, &cfg(OutputKind::Auto, None));
    keeps(&res, &["FATAL: out of memory"]);
    assert_token_cut(&res, 800, 0.2);
}

// ── 2 (guarantee): nothing is ever truly lost — raw rehydrates exactly ─────────

#[test]
fn aggressive_summary_still_has_lossless_raw_in_store() {
    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    let h5i_root = dir.path().join(".git").join(".h5i");
    std::fs::create_dir_all(&h5i_root).unwrap();

    // A log we know summarizes very aggressively.
    let mut raw = String::new();
    for i in 0..1000 {
        raw.push_str(&format!("2026-06-05 INFO request {i} served in {}ms\n", i % 7));
    }
    raw.push_str("ERROR upstream 503 at gateway.rs:12\n");

    let outcome = objects::capture(
        &repo,
        &h5i_root,
        raw.as_bytes(),
        CaptureOptions {
            kind: OutputKind::Auto,
            cmd: None,
            cwd: None,
            exit_code: None,
            git_tree: None,
            files: Vec::new(),
            cmd_argv: Vec::new(),
            filter: cfg(OutputKind::Log, None),
            env_id: None,
            policy_digest: None,
            redact: false,
        },
    )
    .unwrap();

    // Summary is small and keeps the error...
    assert!(outcome.manifest.summary.len() < raw.len() / 5);
    assert!(outcome.manifest.summary.contains("ERROR upstream 503"));
    // ...but the FULL raw is recoverable byte-for-byte from the store.
    let restored = objects::load_raw(&h5i_root, &outcome.manifest).unwrap().unwrap();
    assert_eq!(restored, raw.as_bytes(), "rehydrated raw must equal the original exactly");
    // And the manifest carries the full digest, not a truncated one.
    assert_eq!(outcome.manifest.hex().len(), 64);
}
