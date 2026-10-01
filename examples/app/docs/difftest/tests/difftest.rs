//! Rust kernel vs extracted Lean kernel. Needs the Lean build; run
//! `scripts/app/difftest.sh`. DIFFTEST_SNAPSHOTS and DIFFTEST_SEED override defaults.

use docs_difftest::*;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command as Proc, Stdio};

#[test]
#[ignore = "needs the Lean executable; run scripts/app/difftest.sh"]
fn rust_and_lean_kernels_agree() {
    let env = |k: &str, d: u64| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let snapshots = env("DIFFTEST_SNAPSHOTS", 200);
    let per_snapshot = 25;
    let mut rng = Rng(env("DIFFTEST_SEED", 1));

    let mut inputs = vec![];
    let mut expected = vec![];
    for i in 0..snapshots {
        let mut s = reachable(&mut rng);
        if i % 4 == 3 {
            perturb(&mut rng, &mut s);
        }
        for _ in 0..per_snapshot {
            let (a, c) = (actor_in(&mut rng, &s), command_in(&mut rng, &s));
            inputs.push(encode_case(&a, &s, &c));
            expected.push(run_rust(&a, &s, &c));
        }
    }

    let bin = std::env::var("DIFFTEST_BIN").unwrap_or_else(|_| {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../proofs/.lake/build/bin/difftest"
        )
        .into()
    });
    let mut child = Proc::new(&bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("run Lean difftest binary");
    let mut stdin = child.stdin.take().unwrap();
    let feed = inputs.clone();
    // Feed from another thread so a full stdout pipe cannot deadlock us.
    let writer = std::thread::spawn(move || {
        for line in feed {
            writeln!(stdin, "{line}").unwrap();
        }
    });
    let got: Vec<String> = BufReader::new(child.stdout.take().unwrap())
        .lines()
        .map(Result::unwrap)
        .collect();
    writer.join().unwrap();
    assert!(child.wait().unwrap().success());

    assert_eq!(
        got.len(),
        expected.len(),
        "Lean produced a different number of lines"
    );
    let mut outcomes = std::collections::BTreeMap::<String, usize>::new();
    for (i, (g, e)) in got.iter().zip(&expected).enumerate() {
        assert_eq!(g, e, "case {i} differs\ninput: {}", inputs[i]);
        *outcomes
            .entry(e.split(' ').take(2).collect::<Vec<_>>().join(" "))
            .or_default() += 1;
    }
    eprintln!("{} cases agree; outcome mix: {outcomes:?}", expected.len());
}
