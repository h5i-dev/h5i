//! Anti-gaming regression tests for the prompt maturity score: pasted machine
//! artifacts (error logs, stack traces, compiler output) must not read as
//! prompt craft.

use h5i_core::prompt_score::score_prompt;

/// A realistic pasted artifact: cargo test failure output with a Rust panic
/// backtrace. Nobody *wrote* this; it was copy-pasted.
fn error_log() -> String {
    let mut log = String::from(
        "running 12 tests\n\
         test parser::tests::parses_empty ... ok\n\
         test parser::tests::parses_nested ... FAILED\n\
         test parser::tests::round_trips ... ok\n\
         \n\
         failures:\n\
         \n\
         ---- parser::tests::parses_nested stdout ----\n\
         thread 'parser::tests::parses_nested' panicked at src/parser.rs:214:9:\n\
         assertion `left == right` failed\n\
           left: Some(Node { kind: List, children: 3 })\n\
           right: Some(Node { kind: List, children: 2 })\n\
         note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n\
         stack backtrace:\n\
            0: rust_begin_unwind\n\
                      at /rustc/07dca48/library/std/src/panicking.rs:652:5\n\
            1: core::panicking::panic_fmt\n\
                      at /rustc/07dca48/library/core/src/panicking.rs:72:14\n\
            2: h5i::parser::tests::parses_nested\n\
                      at ./src/parser.rs:214:9\n\
            3: core::ops::function::FnOnce::call_once\n\
                      at /rustc/07dca48/library/core/src/ops/function.rs:250:5\n",
    );
    // Pad with more log-shaped lines the way a real paste often does — repeated
    // assertion diffs and error lines from subsequent test cases.
    for i in 0..40 {
        log.push_str(&format!(
            "test parser::tests::case_{i} ... FAILED\n\
             error[E0308]: mismatched types --> src/parser.rs:{}:{}\n\
             expected `Option<Node>`, found `Result<Node, ParseError>`\n\
             assertion failed: expected 2 children but the list node returned {} at line {}\n",
            200 + i,
            5 + (i % 7),
            i % 5,
            100 + i,
        ));
    }
    log
}

/// The observed failure mode: "fix this" + a wall of pasted log scores far
/// above the bare ask, purely on the strength of machine-generated text.
#[test]
fn pasted_error_log_does_not_inflate_score() {
    let bare = score_prompt("fix this failing test");
    let with_log = score_prompt(&format!("fix this failing test\n\n{}", error_log()));

    // Attaching evidence may legitimately help a little, but a pasted log must
    // not move a lazy ask up a whole maturity band on its own.
    assert!(
        with_log.score <= bare.score + 10.0,
        "pasted log inflated score: bare {} → with log {}",
        bare.score,
        with_log.score
    );
}

/// A crisp, well-crafted prompt with NO log must outscore a lazy ask + huge log.
#[test]
fn crafted_prompt_beats_lazy_ask_with_log_wall() {
    let crafted = score_prompt(
        "The nested-list parser miscounts children. Fix `parse_nested()` in \
         src/parser.rs so a trailing separator does not produce a phantom child. \
         Add a regression test covering the trailing-separator case and keep the \
         public signature unchanged. Done when `cargo test parser::` passes.",
    );
    let lazy_with_log = score_prompt(&format!("fix\n\n{}", error_log()));
    assert!(
        crafted.score > lazy_with_log.score + 15.0,
        "crafted {} should clearly beat lazy+log {}",
        crafted.score,
        lazy_with_log.score
    );
}

/// Doubling the pasted log must not raise the score: evidence credit saturates
/// instead of scaling with paste volume.
#[test]
fn log_volume_has_diminishing_returns() {
    let log = error_log();
    let once = score_prompt(&format!("fix this failing test\n\n{}", log));
    let thrice = score_prompt(&format!("fix this failing test\n\n{}{}{}", log, log, log));
    assert!(
        thrice.score <= once.score + 2.0,
        "more log should not mean more maturity: 1x {} vs 3x {}",
        once.score,
        thrice.score
    );
}

/// The other half of the v1 bug: attaching a log to a *good* prompt used to
/// TANK the score (53 → 35) because the paste drowned diversity and tripped
/// the repetition penalty. Evidence must never hurt.
#[test]
fn attaching_log_never_hurts_a_crafted_prompt() {
    let crafted = "The nested-list parser miscounts children. Fix `parse_nested()` in \
                   src/parser.rs so a trailing separator does not produce a phantom child. \
                   Add a regression test covering the trailing-separator case and keep the \
                   public signature unchanged. Done when `cargo test parser::` passes.";
    let alone = score_prompt(crafted);
    let with_log = score_prompt(&format!("{crafted}\n\n{}", error_log()));
    assert!(
        with_log.score >= alone.score,
        "evidence must never hurt: alone {} vs with log {}",
        alone.score,
        with_log.score
    );
}

/// A pure paste with no authored ask at all is not prompt craft.
#[test]
fn log_only_prompt_is_nascent() {
    let s = score_prompt(&error_log());
    assert!(s.score < 10.0, "log-only scored {}", s.score);
    assert_eq!(s.words, 0, "machine output must not count as authored words");
}

#[test]
#[ignore]
fn diag_print_scores() {
    let log = error_log();
    for (name, p) in [
        ("bare-vague", "fix this failing test".to_string()),
        ("vague+log", format!("fix this failing test\n\n{log}")),
        ("medium", "The nested parser test fails. Please fix the child counting bug in the parser module.".to_string()),
        ("medium+log", format!("The nested parser test fails. Please fix the child counting bug in the parser module.\n\n{log}")),
        ("log-only", log.clone()),
        ("crafted", "The nested-list parser miscounts children. Fix `parse_nested()` in src/parser.rs so a trailing separator does not produce a phantom child. Add a regression test covering the trailing-separator case and keep the public signature unchanged. Done when `cargo test parser::` passes.".to_string()),
        ("crafted+log", format!("The nested-list parser miscounts children. Fix `parse_nested()` in src/parser.rs so a trailing separator does not produce a phantom child. Add a regression test covering the trailing-separator case and keep the public signature unchanged. Done when `cargo test parser::` passes.\n\n{log}")),
    ] {
        let s = score_prompt(&p);
        println!("{name:12} score={:6.2} level={:?} words={} unscored={:?} obj={:.2} grd={:.2} dir={:.2} ctx={:.2} ex={:.2} evid={:.2}",
            s.score, s.level, s.words, s.unscored,
            s.breakdown.objective, s.breakdown.grounding, s.breakdown.direction,
            s.breakdown.context, s.breakdown.examples, s.breakdown.evidence);
    }
}
