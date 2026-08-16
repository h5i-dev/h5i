//! Differential test of the box-to-box interference checker: the Rust
//! `effective::interferes` — the function behind the `fs_overlap` receipt —
//! against the Lean `interferesCheck` (`lean/H5iSpec/Noninterference.lean`,
//! via `h5i-spec --interferes`), whose soundness proof backs the receipt.
//!
//! This is the small-surface DRT the pivot keeps (ROADMAP §V4): the checker
//! is a pure prefix-comparability scan over two compiled rulesets, so a
//! random sweep over config pairs is strong evidence the port matches the
//! verified original — unlike the retired whole-config `compute_effective`
//! twin, which mirrored a host-dependent pipeline and cost more to maintain
//! than it caught.
//!
//! The Lean binary is built with `cd lean && lake build`. When it is absent
//! the test SKIPS LOUDLY; set `H5I_DRT_REQUIRE=1` (the Lean CI job does) to
//! turn absence into failure. `H5I_DRT_SEED` overrides the seed.

#![cfg(target_os = "linux")]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use h5i_core::effective::{self, RunShape};
use h5i_core::sandbox_policy::{HomeBind, IsolationClaim, NetMode, PrivateBind, Profile, ResolvedPolicy, RoBind};
use serde_json::{json, Value};

/// The Lean model binary, or None with a loud explanation.
fn lean_bin() -> Option<PathBuf> {
    let path = std::env::var_os("H5I_SPEC_BIN").map(PathBuf::from).unwrap_or_else(|| {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("lean/.lake/build/bin/h5i-spec")
    });
    if path.is_file() {
        return Some(path);
    }
    let msg = format!(
        "Lean model binary not found at {} — build it with `cd lean && lake build`",
        path.display()
    );
    if std::env::var_os("H5I_DRT_REQUIRE").is_some_and(|v| v == "1") {
        panic!("{msg} (H5I_DRT_REQUIRE=1)");
    }
    eprintln!("SKIPPING interferes DRT: {msg}");
    None
}

fn seed() -> u64 {
    std::env::var("H5I_DRT_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(0x5150_5EED)
}

/// Run `h5i-spec --interferes` over one JSON input, expecting a JSON array.
fn lean_interferes(bin: &Path, input: &Value) -> Vec<Value> {
    let mut child = Command::new(bin)
        .arg("--interferes")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn lean model");
    child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "lean model exited with {:?}", out.status);
    match serde_json::from_slice::<Value>(&out.stdout).unwrap() {
        Value::Array(items) => items,
        other => panic!("lean model did not return an array: {other}"),
    }
}

/// A generated policy over paths materialized under `root`, enough to drive
/// `compute_effective` into a realistic grant set.
fn gen_policy(rng: &mut fastrand::Rng, root: &Path) -> (ResolvedPolicy, PathBuf, i32, RunShape) {
    let claim = if rng.bool() { IsolationClaim::Process } else { IsolationClaim::Supervised };
    let mut profile = Profile::builtin("default", claim);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let work = work.canonicalize().unwrap();

    let mut path_n = 0usize;
    let mut some_paths = |rng: &mut fastrand::Rng, max: usize| -> Vec<String> {
        (0..rng.usize(0..=max))
            .map(|_| {
                path_n += 1;
                let p = root.join(format!("p{path_n}"));
                let s = p.to_string_lossy().into_owned();
                match rng.u8(0..3) {
                    0 => {
                        std::fs::create_dir_all(&p).unwrap();
                    }
                    1 => {
                        std::fs::write(&p, b"x").unwrap();
                    }
                    _ => {}
                }
                s
            })
            .collect()
    };

    profile.fs_read = some_paths(rng, 4);
    profile.fs_write = some_paths(rng, 3);
    if rng.bool() {
        profile.fs_write.push("$WORK".into());
    }
    profile.net_mode = if rng.bool() { NetMode::Deny } else { NetMode::Host };
    profile.wall_secs = rng.u64(1..86400);

    let mut policy = ResolvedPolicy::new(claim, profile);
    policy.work_readonly = rng.bool();
    policy.private_binds = (0..rng.usize(0..=2))
        .map(|i| PrivateBind { backing: root.join(format!("priv{i}")), rel: format!("shadow/{i}") })
        .collect();
    policy.home_binds = (0..rng.usize(0..=3))
        .map(|i| HomeBind {
            backing: root.join(format!("home{i}")),
            target: if rng.bool() { PathBuf::from("/tmp") } else { root.join(format!("t{i}")) },
        })
        .collect();
    policy.ro_binds = (0..rng.usize(0..=2))
        .map(|i| RoBind { backing: root.join(format!("cache{i}")), target: root.join(format!("ct{i}")) })
        .collect();

    let shape = RunShape {
        force_netns: rng.bool(),
        notify: rng.bool(),
        egress: rng.bool(),
        pidns: rng.bool(),
        interactive: false,
    };
    (policy, work, rng.i32(1..=6), shape)
}

/// The Rust `interferes` against the Lean `interferesCheck`. Pairs sharing a
/// generation root overlap heavily; pairs on distinct roots mostly do not;
/// self-pairs always do (a box shares every path with itself).
#[test]
fn rust_and_lean_interferes_agree() {
    let Some(bin) = lean_bin() else { return };
    let mut rng = fastrand::Rng::with_seed(seed() ^ 0x1F);
    let tmp = tempfile::tempdir().unwrap();
    let shared = tmp.path().join("shared");
    std::fs::create_dir_all(&shared).unwrap();
    let shared = shared.to_string_lossy().into_owned();
    let effs: Vec<Value> = (0..40)
        .map(|i| {
            let root = tmp.path().join(format!("case{i}"));
            std::fs::create_dir_all(&root).unwrap();
            let (mut policy, work, abi, shape) = gen_policy(&mut rng, &root);
            // Half the cases additionally grant one shared existing dir, so
            // both verdicts occur: those pairs overlap, disjoint-root pairs
            // mostly do not, self-pairs always do.
            if i % 2 == 0 {
                policy.profile.fs_write.push(shared.clone());
            }
            serde_json::to_value(effective::compute_effective(&policy, &work, abi, &shape)).unwrap()
        })
        .collect();
    let mut pairs = Vec::new();
    let mut rust_verdicts = Vec::new();
    for i in 0..effs.len() {
        for j in [i, (i + 1) % effs.len(), (i + 2) % effs.len()] {
            let a: effective::EffectiveConfig = serde_json::from_value(effs[i].clone()).unwrap();
            let b: effective::EffectiveConfig = serde_json::from_value(effs[j].clone()).unwrap();
            rust_verdicts.push(effective::interferes(&a, &b).is_some());
            pairs.push(json!({"a": effs[i], "b": effs[j]}));
        }
    }
    let lean_out = lean_interferes(&bin, &Value::Array(pairs.clone()));
    assert_eq!(lean_out.len(), rust_verdicts.len());
    let mut mismatches = 0;
    for (k, (r, l)) in rust_verdicts.iter().zip(&lean_out).enumerate() {
        if Value::Bool(*r) != *l {
            mismatches += 1;
            eprintln!("INTERFERES MISMATCH pair {k}: rust {r} lean {l}\n{}", pairs[k]);
        }
    }
    assert_eq!(mismatches, 0, "{mismatches} interferes mismatches (seed {})", seed());
}
