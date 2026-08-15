//! Differential random testing of the effective-config computation
//! (ROADMAP.md §V4, "model versus Rust"): the Rust `compute_effective` — the
//! function `build_confined_command` enforces from — against the Lean model
//! in `lean/H5iSpec/Model.lean`, over generated policies whose filesystem
//! world is materialized in a tempdir so "exists on the host" and "member of
//! the world handed to the model" coincide by construction.
//!
//! The Lean binary is built with `cd lean && lake build`. When it is absent
//! the test SKIPS LOUDLY (a Rust contributor without a Lean toolchain must
//! not be blocked); set `H5I_DRT_REQUIRE=1` (the Lean CI job does) to turn
//! absence into failure. `H5I_DRT_SEED` / `H5I_DRT_CASES` override the
//! deterministic default seed and case count; a mismatch prints both so the
//! case replays exactly.
//!
//! Known generator gaps, deliberate and named rather than silent (§V4):
//! - `interactive` is always false: `config_lock_paths` reads the real
//!   `$HOME`, which a test must not write to. Covering it needs a
//!   HOME-controlled subprocess harness.
//! - No `~` grant entries, for the same reason (`expand_tilde` reads the
//!   real `$HOME`). The model implements both against its explicit world, so
//!   the logic exists; it is the *differential* coverage that is missing.

#![cfg(target_os = "linux")]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use h5i_core::effective::{self, RunShape};
use h5i_core::sandbox_policy::{
    HomeBind, IsolationClaim, NetMode, PrivateBind, Profile, ResolvedPolicy, RoBind,
};
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
    eprintln!("SKIPPING effective-config DRT: {msg}");
    None
}

/// One generated case: the policy inputs plus the world facts the harness
/// materialized for them. Everything needed to (a) run the Rust side against
/// the real filesystem and (b) hand the Lean side the same facts.
struct Case {
    policy: ResolvedPolicy,
    work: PathBuf,
    abi: i32,
    shape: RunShape,
    files: Vec<String>,
    dirs: Vec<String>,
}

/// A grant-candidate path under the case dir: existing dir, existing file, or
/// missing — the three answers `Path::exists`/`is_dir`/`is_file` distinguish.
fn gen_path(rng: &mut fastrand::Rng, root: &Path, n: usize, c: &mut Case) -> String {
    let p = root.join(format!("p{n}"));
    let s = p.to_string_lossy().into_owned();
    match rng.u8(0..3) {
        0 => {
            std::fs::create_dir_all(&p).unwrap();
            c.dirs.push(s.clone());
        }
        1 => {
            std::fs::write(&p, b"x").unwrap();
            c.files.push(s.clone());
        }
        _ => {}
    }
    s
}

fn gen_case(rng: &mut fastrand::Rng, root: &Path) -> Case {
    let claim = if rng.bool() { IsolationClaim::Process } else { IsolationClaim::Supervised };
    let mut profile = Profile::builtin("default", claim);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let work = work.canonicalize().unwrap();

    let mut c = Case {
        policy: ResolvedPolicy::new(claim, profile.clone()),
        work: work.clone(),
        abi: rng.i32(1..=6),
        shape: RunShape {
            force_netns: rng.bool(),
            notify: rng.bool(),
            egress: rng.bool(),
            pidns: rng.bool(),
            interactive: false, // named gap, see module docs
        },
        files: Vec::new(),
        dirs: Vec::new(),
    };

    let mut path_n = 0usize;
    let mut some_paths = |rng: &mut fastrand::Rng, c: &mut Case, max: usize| -> Vec<String> {
        (0..rng.usize(0..=max))
            .map(|_| {
                path_n += 1;
                gen_path(rng, root, path_n, c)
            })
            .collect()
    };

    profile.fs_read = some_paths(rng, &mut c, 4);
    profile.fs_write = some_paths(rng, &mut c, 3);
    if rng.bool() {
        profile.fs_write.push("$WORK".into());
    }
    if rng.bool() && !profile.fs_read.is_empty() {
        // Duplicate + cross-listed grants: order and multiplicity must match.
        profile.fs_write.push(profile.fs_read[0].clone());
    }
    profile.fs_deny = (0..rng.usize(0..=2)).map(|i| format!("/deny/{i}")).collect();
    profile.net_mode = if rng.bool() { NetMode::Deny } else { NetMode::Host };
    profile.net_egress =
        (0..rng.usize(0..=2)).map(|i| format!("host{i}.example")).collect();
    profile.loopback_ports = (0..rng.usize(0..=2)).map(|_| rng.u16(1024..)).collect();
    profile.unix_sockets = rng.bool();
    profile.mem_bytes = rng.bool().then(|| rng.u64(1..1 << 33));
    profile.max_procs = rng.bool().then(|| rng.u64(1..4096));
    profile.fsize_bytes = rng.bool().then(|| rng.u64(1..1 << 33));
    profile.cpu_secs = rng.bool().then(|| rng.u64(1..86400));
    profile.wall_secs = rng.u64(1..86400);
    profile.env_pass = (0..rng.usize(0..=3)).map(|i| format!("VAR{i}")).collect();
    profile.tools = (0..rng.usize(0..=2)).map(|i| format!("tool{i}")).collect();

    let mut policy = ResolvedPolicy::new(claim, profile);
    policy.work_readonly = rng.bool();
    policy.private_binds = (0..rng.usize(0..=2))
        .map(|i| PrivateBind {
            backing: root.join(format!("priv{i}")),
            rel: format!("shadow/{i}"),
        })
        .collect();
    // Mix a literal `/tmp` target in: `home_binds_in_mount_order` moves it
    // last, and the model must reproduce that stable sort exactly.
    policy.home_binds = (0..rng.usize(0..=3))
        .map(|i| HomeBind {
            backing: root.join(format!("home{i}")),
            target: if rng.bool() { PathBuf::from("/tmp") } else { root.join(format!("t{i}")) },
        })
        .collect();
    policy.ro_binds = (0..rng.usize(0..=2))
        .map(|i| RoBind { backing: root.join(format!("cache{i}")), target: root.join(format!("ct{i}")) })
        .collect();
    policy.cache_write = rng.bool().then(|| RoBind {
        backing: root.join("cw-backing"),
        target: root.join("cw-target"),
    });
    policy.user_egress_allow =
        (0..rng.usize(0..=2)).map(|i| format!("extra{i}.example")).collect();
    policy.loopback_ports = (0..rng.usize(0..=2)).map(|_| rng.u16(1024..)).collect();

    c.policy = policy;
    c
}

fn bind_pair(backing: &Path, target: &Path) -> Value {
    json!({
        "backing": backing.to_string_lossy(),
        "target": target.to_string_lossy(),
    })
}

/// The `DrtInput` the Lean model reads — the same facts the Rust side is
/// about to read from the policy and the real filesystem.
fn drt_input(c: &Case) -> Value {
    let p = &c.policy.profile;
    json!({
        "claim": c.policy.claim.as_str(),
        "profile": {
            "name": p.name,
            "fs_read": p.fs_read,
            "fs_write": p.fs_write,
            "fs_deny": p.fs_deny,
            "net_mode": match p.net_mode { NetMode::Deny => "deny", NetMode::Host => "host" },
            "net_egress": p.net_egress,
            "loopback_ports": p.loopback_ports,
            "unix_sockets": p.unix_sockets,
            "mem_bytes": p.mem_bytes,
            "max_procs": p.max_procs,
            "fsize_bytes": p.fsize_bytes,
            "cpu_secs": p.cpu_secs,
            "wall_secs": p.wall_secs,
            "env_pass": p.env_pass,
            "tools": p.tools,
        },
        "runtime": {
            "work_readonly": c.policy.work_readonly,
            "private_binds": c.policy.private_binds.iter().map(|b| json!({
                "backing": b.backing.to_string_lossy(),
                "rel": b.rel,
            })).collect::<Vec<_>>(),
            "home_binds": c.policy.home_binds.iter()
                .map(|b| bind_pair(&b.backing, &b.target)).collect::<Vec<_>>(),
            "ro_binds": c.policy.ro_binds.iter()
                .map(|b| bind_pair(&b.backing, &b.target)).collect::<Vec<_>>(),
            "cache_write": c.policy.cache_write.as_ref()
                .map(|b| bind_pair(&b.backing, &b.target)),
            "user_egress_allow": c.policy.user_egress_allow,
            "loopback_ports": c.policy.loopback_ports,
        },
        "work": c.work.to_string_lossy(),
        "landlock_abi": c.abi,
        "shape": {
            "force_netns": c.shape.force_netns,
            "notify": c.shape.notify,
            "egress": c.shape.egress,
            "pidns": c.shape.pidns,
            "interactive": c.shape.interactive,
        },
        "world": { "files": c.files, "dirs": c.dirs, "home": null },
    })
}

/// Strip `null` object entries recursively. serde writes `Option::None` as
/// `null`; Lean's derived `ToJson` omits the field. Nothing in this schema
/// distinguishes the two, so compare modulo that encoding difference.
fn strip_nulls(v: Value) -> Value {
    match v {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k, strip_nulls(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(strip_nulls).collect()),
        other => other,
    }
}

#[test]
fn rust_and_lean_model_agree() {
    let Some(bin) = lean_bin() else { return };
    let seed: u64 =
        std::env::var("H5I_DRT_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(0x5150_5EED);
    let cases_n: usize =
        std::env::var("H5I_DRT_CASES").ok().and_then(|s| s.parse().ok()).unwrap_or(500);
    let mut rng = fastrand::Rng::with_seed(seed);
    let tmp = tempfile::tempdir().unwrap();

    let cases: Vec<Case> = (0..cases_n)
        .map(|i| {
            let root = tmp.path().join(format!("case{i}"));
            std::fs::create_dir_all(&root).unwrap();
            gen_case(&mut rng, &root)
        })
        .collect();

    // The Rust side: the exact function `build_confined_command` enforces from.
    let rust_out: Vec<Value> = cases
        .iter()
        .map(|c| {
            let cfg = effective::compute_effective(&c.policy, &c.work, c.abi, &c.shape);
            strip_nulls(serde_json::to_value(&cfg).unwrap())
        })
        .collect();

    // The Lean side: one process, every case.
    let inputs = Value::Array(cases.iter().map(drt_input).collect());
    let mut child = Command::new(&bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn lean model");
    child.stdin.take().unwrap().write_all(inputs.to_string().as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "lean model exited with {:?}", out.status);
    let lean_out: Vec<Value> = match serde_json::from_slice::<Value>(&out.stdout).unwrap() {
        Value::Array(items) => items.into_iter().map(strip_nulls).collect(),
        other => panic!("lean model did not return an array: {other}"),
    };
    assert_eq!(lean_out.len(), cases.len(), "lean model returned a different case count");

    let mut mismatches = 0;
    for (i, (r, l)) in rust_out.iter().zip(&lean_out).enumerate() {
        if r != l {
            mismatches += 1;
            eprintln!(
                "DRT MISMATCH case {i} (seed {seed}):\ninput: {}\nrust:  {r}\nlean:  {l}\n",
                drt_input(&cases[i])
            );
        }
    }
    assert_eq!(mismatches, 0, "{mismatches} DRT mismatches (seed {seed}, {cases_n} cases)");
}
