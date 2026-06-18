//! Zero-dependency micro-benchmark for the offline Prompt Maturity Score.
//!
//! Deliberately avoids a `criterion` dev-dependency (heavy to compile on the
//! memory-constrained hosts this repo targets) — it uses `std::time::Instant`
//! and reports ns/op + throughput for the two public entry points.
//!
//! Run with an optimized build (a debug build is ~10× slower and not
//! representative):
//!
//! ```bash
//! cargo run --release --example bench_prompt_score
//! ```
//!
//! It is a measurement tool, not a test — nothing here asserts. Use it to
//! confirm an optimization actually moved the number before/after a change.

use std::hint::black_box;
use std::time::Instant;

use h5i_core::prompt_score::{score_branch, score_prompt};

/// Representative prompts spanning the realistic length/shape distribution:
/// a one-liner, a crisp tactical ask, a structured multi-step brief, and a
/// rambling wall (worst case for MATTR + lexicon scanning).
fn corpus() -> Vec<String> {
    let short = "fix the bug".to_string();

    let tactical = "Refactor `parse_range()` in src/util.rs so it handles the \
        off-by-one when the upper bound is inclusive. Add a unit test for the \
        empty-range case and make sure the existing tests still pass. Do not \
        change the public signature."
        .to_string();

    let structured = "Implement rate limiting in three steps:\n\
        1. Add a `TokenBucket` struct to src/limit.rs with a configurable refill rate.\n\
        2. Wire it into `handle_request()` so requests over the cap return 429.\n\
        3. Add tests in tests/limit_test.rs covering the empty bucket, the refill \
           boundary, and a burst of concurrent requests.\n\
        Keep the public `Server` signature stable and only touch limit.rs and the test."
        .to_string();

    // A long, low-structure dump — the stress case for the windowed diversity
    // metric and the per-lexicon scans.
    let wall = "We need to improve the system so that it works better and is more \
        robust and handles various cases appropriately, and the code should be clean \
        and efficient and maintainable, and we should probably also make sure things \
        are reasonable and the performance is good enough for normal usage patterns "
        .repeat(8);

    vec![short, tactical, structured, wall]
}

fn bench<F: Fn()>(name: &str, iters: u32, bytes: usize, f: F) {
    // Warm up so we measure steady-state, not first-touch.
    for _ in 0..(iters / 10).max(1) {
        f();
    }
    let start = Instant::now();
    for _ in 0..iters {
        f();
    }
    let elapsed = start.elapsed();
    let per = elapsed.as_secs_f64() / iters as f64;
    let mb_per_s = (bytes as f64 * iters as f64) / elapsed.as_secs_f64() / 1e6;
    println!(
        "{name:<34} {:>10.0} ns/op   {:>8.1} MB/s   ({iters} iters)",
        per * 1e9,
        mb_per_s
    );
}

fn main() {
    let prompts = corpus();
    let labels = ["one-liner", "tactical", "structured", "rambling-wall"];

    println!("== score_prompt (single prompt) ==");
    for (label, p) in labels.iter().zip(&prompts) {
        let iters = 20_000;
        bench(
            &format!("score_prompt/{label}"),
            iters,
            p.len(),
            || {
                black_box(score_prompt(black_box(p)));
            },
        );
    }

    println!("\n== score_branch (rolled-up branch) ==");
    // Simulate branches of increasing size by cycling the corpus.
    for &n in &[8usize, 64, 512] {
        let branch: Vec<&str> = (0..n).map(|i| prompts[i % prompts.len()].as_str()).collect();
        let bytes: usize = branch.iter().map(|s| s.len()).sum();
        let iters = (2_000_000u32 / n as u32).max(20);
        bench(
            &format!("score_branch/{n}-commits"),
            iters,
            bytes,
            || {
                black_box(score_branch(black_box(&branch).iter().copied(), n));
            },
        );
    }
}
