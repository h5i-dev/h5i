//! Differential test of the filesystem-authority validator: the Rust port
//! `h5i_core::fs_authority::validate` against the Lean `H5iFs.validate`
//! (`lean/H5iFs/Validate.lean`, via `h5i-spec --validate`), whose
//! `validate_sound` is the proof the port stands in for (ROADMAP §VF.4).
//!
//! The checker is a small pure function — resolve grants through a measured
//! world, then a subset check — so a random sweep over worlds (with symlinks,
//! aliases, and loops), policies, and plans is strong evidence the port
//! matches the verified original, the same discipline as `interferes_drt`.
//!
//! Skips loudly when the Lean binary is absent; `H5I_DRT_REQUIRE=1` (the Lean
//! CI job) turns absence into failure. `H5I_DRT_SEED` overrides the seed.

#![cfg(target_os = "linux")]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use h5i_core::fs_authority::{self, Entry, EffectivePlan, FsState, NodeKind, Policy};
use serde_json::{json, Value};

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
    eprintln!("SKIPPING validate DRT: {msg}");
    None
}

fn seed() -> u64 {
    std::env::var("H5I_DRT_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(0x5A11_DA7E)
}

/// Small name pool, so generated paths and symlink targets resolve some of the
/// time (and miss the rest), exercising both verdicts and the fuel cutoff.
const POOL: &[&str] = &["a", "b", "c", "work"];

fn gen_path(rng: &mut fastrand::Rng) -> Vec<String> {
    (0..rng.usize(1..=3)).map(|_| POOL[rng.usize(0..POOL.len())].to_string()).collect()
}

/// One generated case: a measured world (with symlinks and possible aliases),
/// a policy, and a plan.
fn gen_case(rng: &mut fastrand::Rng) -> (FsState, Policy, EffectivePlan) {
    let n = rng.u64(3..=8);
    let mut nodes = vec![(0u64, NodeKind::Dir)];
    for id in 1..n {
        let kind = match rng.u8(0..3) {
            0 => NodeKind::File,
            1 => NodeKind::Dir,
            _ => NodeKind::Symlink(gen_path(rng)),
        };
        nodes.push((id, kind));
    }
    let mut entries = Vec::new();
    for id in 1..n {
        let parent = rng.u64(0..id); // an earlier node, so the graph is reachable-ish
        let name = POOL[rng.usize(0..POOL.len())].to_string();
        // Occasionally alias: point a fresh name at an existing object (hard link).
        let child = if id > 1 && rng.bool() { rng.u64(0..id) } else { id };
        entries.push(Entry { parent, name, child });
    }
    let fs = FsState { nodes, entries, root: 0 };

    let policy = Policy {
        may_read: (0..n).filter(|_| rng.bool()).collect(),
        may_write: (0..n).filter(|_| rng.bool()).collect(),
    };

    let mut ro = Vec::new();
    let mut rw = Vec::new();
    for _ in 0..rng.usize(0..=4) {
        if rng.bool() {
            ro.push(gen_path(rng));
        } else {
            rw.push(gen_path(rng));
        }
    }
    (fs, policy, EffectivePlan { ro, rw })
}

fn kind_json(id: u64, k: &NodeKind) -> Value {
    match k {
        NodeKind::File => json!({"id": id, "kind": "file"}),
        NodeKind::Dir => json!({"id": id, "kind": "dir"}),
        NodeKind::Symlink(t) => json!({"id": id, "kind": "symlink", "target": t}),
    }
}

fn case_json(fs: &FsState, pol: &Policy, plan: &EffectivePlan) -> Value {
    json!({
        "policy": {"mayRead": pol.may_read, "mayWrite": pol.may_write},
        "world": {
            "nodes": fs.nodes.iter().map(|(id, k)| kind_json(*id, k)).collect::<Vec<_>>(),
            "entries": fs.entries.iter().map(|e| json!({
                "parent": e.parent, "name": e.name, "child": e.child
            })).collect::<Vec<_>>(),
            "content": [],
            "root": fs.root,
        },
        "plan": {"ro": plan.ro, "rw": plan.rw},
    })
}

fn lean_validate(bin: &Path, input: &Value) -> Vec<Value> {
    let mut child = Command::new(bin)
        .arg("--validate")
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

#[test]
fn rust_and_lean_validate_agree() {
    let Some(bin) = lean_bin() else { return };
    let mut rng = fastrand::Rng::with_seed(seed());
    let cases: Vec<(FsState, Policy, EffectivePlan)> = (0..200).map(|_| gen_case(&mut rng)).collect();

    let rust: Vec<bool> =
        cases.iter().map(|(fs, pol, plan)| fs_authority::validate(pol, fs, plan)).collect();
    let inputs = Value::Array(cases.iter().map(|(fs, pol, plan)| case_json(fs, pol, plan)).collect());
    let lean = lean_validate(&bin, &inputs);
    assert_eq!(lean.len(), rust.len(), "lean returned a different case count");

    let mut mismatches = 0;
    for (i, (r, l)) in rust.iter().zip(&lean).enumerate() {
        if Value::Bool(*r) != *l {
            mismatches += 1;
            eprintln!(
                "VALIDATE MISMATCH case {i} (seed {}): rust {r} lean {l}\n{}",
                seed(),
                case_json(&cases[i].0, &cases[i].1, &cases[i].2)
            );
        }
    }
    assert_eq!(mismatches, 0, "{mismatches} validate mismatches (seed {})", seed());
}
