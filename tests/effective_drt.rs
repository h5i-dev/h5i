//! Differential random testing of the effective-config computation
//! (ROADMAP.md §V4, "model versus Rust"): the Rust `compute_effective` — the
//! function `build_confined_command` enforces from — against the Lean model
//! in `lean/H5iSpec/Model.lean`, over generated policies whose filesystem
//! world is materialized in a tempdir so "exists on the host" and "member of
//! the world handed to the model" coincide by construction.
//!
//! Four lanes:
//! - `rust_and_lean_model_agree`: the random-policy sweep.
//! - `builtin_and_repo_profiles_agree`: the corpus sweep — the builtin
//!   profile family plus this repo's own `.h5i/env.toml` profiles, with the
//!   world taken from the real host (read-only stats; tilde entries expand
//!   against the real `$HOME`, and the model receives the same).
//! - `interactive_and_tilde_cases_agree`: the HOME-controlled lane. The
//!   Rust side reads the process `$HOME` (`expand_tilde`,
//!   `config_lock_paths`), so this test re-executes itself as a child with
//!   `HOME` pointed at a disposable directory, then generates interactive
//!   shapes and `~` grants whose existence it fully controls.
//! - `rust_and_lean_interferes_agree`: the Rust `effective::interferes`
//!   against the Lean `interferesCheck` (the checker whose soundness backs
//!   the noninterference receipt), over pairs of generated configs.
//!
//! The Lean binary is built with `cd lean && lake build`. When it is absent
//! the tests SKIP LOUDLY (a Rust contributor without a Lean toolchain must
//! not be blocked); set `H5I_DRT_REQUIRE=1` (the Lean CI job does) to turn
//! absence into failure. `H5I_DRT_SEED` / `H5I_DRT_CASES` override the
//! deterministic default seed and case count; a mismatch prints both so the
//! case replays exactly.

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

fn seed() -> u64 {
    std::env::var("H5I_DRT_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(0x5150_5EED)
}

/// Run the Lean binary in `mode` (None = DRT) over one JSON input, expecting
/// a JSON array back.
fn lean_call(bin: &Path, mode: Option<&str>, input: &Value) -> Vec<Value> {
    let mut cmd = Command::new(bin);
    if let Some(m) = mode {
        cmd.arg(m);
    }
    let mut child = cmd
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
    home: Option<String>,
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
            interactive: false, // flipped on by the HOME-controlled lane only
        },
        files: Vec::new(),
        dirs: Vec::new(),
        home: None,
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
        "world": { "files": c.files, "dirs": c.dirs, "home": c.home },
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

/// The Rust verdict for one case, null-stripped for comparison. Computed at
/// call time, so the caller controls when the host (`$HOME`, world files) is
/// read.
fn rust_effective(c: &Case) -> Value {
    let cfg = effective::compute_effective(&c.policy, &c.work, c.abi, &c.shape);
    strip_nulls(serde_json::to_value(&cfg).unwrap())
}

/// Diff pre-computed Rust verdicts against one batched Lean run; returns the
/// mismatch count after printing each mismatch replayably.
fn diff_against_lean(bin: &Path, cases: &[Case], rust_out: &[Value], label: &str) -> usize {
    let inputs = Value::Array(cases.iter().map(drt_input).collect());
    let lean_out: Vec<Value> =
        lean_call(bin, None, &inputs).into_iter().map(strip_nulls).collect();
    assert_eq!(lean_out.len(), cases.len(), "lean model returned a different case count");
    let mut mismatches = 0;
    for (i, (r, l)) in rust_out.iter().zip(&lean_out).enumerate() {
        if r != l {
            mismatches += 1;
            eprintln!(
                "DRT MISMATCH [{label}] case {i} (seed {}):\ninput: {}\nrust:  {r}\nlean:  {l}\n",
                seed(),
                drt_input(&cases[i])
            );
        }
    }
    mismatches
}

#[test]
fn rust_and_lean_model_agree() {
    let Some(bin) = lean_bin() else { return };
    let cases_n: usize =
        std::env::var("H5I_DRT_CASES").ok().and_then(|s| s.parse().ok()).unwrap_or(500);
    let mut rng = fastrand::Rng::with_seed(seed());
    let tmp = tempfile::tempdir().unwrap();

    let cases: Vec<Case> = (0..cases_n)
        .map(|i| {
            let root = tmp.path().join(format!("case{i}"));
            std::fs::create_dir_all(&root).unwrap();
            gen_case(&mut rng, &root)
        })
        .collect();
    let rust_out: Vec<Value> = cases.iter().map(rust_effective).collect();
    let mismatches = diff_against_lean(&bin, &cases, &rust_out, "random");
    assert_eq!(mismatches, 0, "{mismatches} DRT mismatches (seed {}, {cases_n} cases)", seed());
}

/// Harness-side tilde expansion, mirroring `expand_tilde` for world-building.
fn expand_home(entry: &str, home: Option<&str>) -> String {
    if (entry == "~" || entry.starts_with("~/"))
        && let Some(h) = home
    {
        return format!("{h}{}", &entry[1..]);
    }
    entry.to_string()
}

/// The corpus sweep: the builtin profile family plus this repo's own
/// `.h5i/env.toml`, with the world taken from the real host. Read-only: the
/// harness stats the expanded grants, writes nothing outside its tempdir.
#[test]
fn builtin_and_repo_profiles_agree() {
    let Some(bin) = lean_bin() else { return };
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let home = std::env::var("HOME").ok();
    let tmp = tempfile::tempdir().unwrap();
    let mut cases = Vec::new();
    let mut loaded = Vec::new();
    // Each profile pinned to a kernel tier it can resolve on: the agent and
    // browser profiles declare egress allowlists only the supervised tier
    // enforces, and the builtins default to `workspace` with no override.
    for (name, claim) in [
        ("default", IsolationClaim::Process),
        ("agent-claude", IsolationClaim::Supervised),
        ("agent-codex", IsolationClaim::Supervised),
        ("browser", IsolationClaim::Supervised),
        ("web", IsolationClaim::Supervised),
    ] {
        let profile = match h5i_core::sandbox::load_profile(repo_root, name, Some(claim)) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("corpus: profile '{name}' not loadable here — skipped: {e}");
                continue;
            }
        };
        let Some(shape) = effective::captured_run_shape(claim, &profile) else {
            eprintln!("corpus: profile '{name}' resolves outside the kernel tiers — skipped");
            continue;
        };
        let work = tmp.path().join(name);
        std::fs::create_dir_all(&work).unwrap();
        let mut c = Case {
            policy: ResolvedPolicy::new(claim, profile.clone()),
            work: work.canonicalize().unwrap(),
            abi: 3,
            shape,
            files: Vec::new(),
            dirs: Vec::new(),
            home: home.clone(),
        };
        for entry in profile.fs_read.iter().chain(profile.fs_write.iter()) {
            if entry == "$WORK" {
                continue;
            }
            let expanded = expand_home(entry, home.as_deref());
            let p = Path::new(&expanded);
            if p.is_dir() {
                c.dirs.push(expanded);
            } else if p.exists() {
                // Regular files AND non-directory specials (/dev/null …):
                // `World.files` is the "exists, not a directory" bucket.
                c.files.push(expanded);
            }
        }
        c.files.sort();
        c.files.dedup();
        c.dirs.sort();
        c.dirs.dedup();
        loaded.push(name);
        cases.push(c);
    }
    assert!(loaded.len() >= 2, "corpus loaded too few profiles: {loaded:?}");
    eprintln!("corpus profiles: {loaded:?}");
    let rust_out: Vec<Value> = cases.iter().map(rust_effective).collect();
    let mismatches = diff_against_lean(&bin, &cases, &rust_out, "corpus");
    assert_eq!(mismatches, 0, "{mismatches} corpus mismatches over {loaded:?}");
}

/// The HOME-controlled lane. The parent re-executes this same test in a
/// child process whose `$HOME` is a disposable directory, so the child can
/// exercise what the other lanes must not touch: `interactive` shapes
/// (`config_lock_paths` stats `$HOME` config files) and `~` grants
/// (`expand_tilde` reads `$HOME`).
#[test]
fn interactive_and_tilde_cases_agree() {
    if std::env::var_os("H5I_DRT_HOME_CHILD").is_some() {
        home_child();
        return;
    }
    let Some(_) = lean_bin() else { return };
    let fake_home = tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    let out = Command::new(exe)
        .args(["interactive_and_tilde_cases_agree", "--exact", "--nocapture"])
        .env("H5I_DRT_HOME_CHILD", "1")
        .env("HOME", fake_home.path())
        .output()
        .expect("spawn HOME-controlled child");
    assert!(
        out.status.success(),
        "HOME-controlled DRT child failed:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The child body: `$HOME` is disposable and fully ours. Home-level state
/// (agent config files) is (re)materialized per case BEFORE the Rust side
/// computes, because it is shared across cases where the per-case tempdir is
/// not.
fn home_child() {
    let bin = lean_bin().expect("child runs only when the parent found the lean binary");
    let home = PathBuf::from(std::env::var_os("HOME").expect("child HOME"));
    let home_s = home.to_string_lossy().into_owned();
    let mut rng = fastrand::Rng::with_seed(seed() ^ 0x40_4E);
    let tmp = tempfile::tempdir().unwrap();
    let n = 200;
    let mut cases = Vec::new();
    let mut rust_out = Vec::new();
    for i in 0..n {
        let root = tmp.path().join(format!("case{i}"));
        std::fs::create_dir_all(&root).unwrap();
        let mut c = gen_case(&mut rng, &root);
        c.home = Some(home_s.clone());
        c.shape.interactive = true;
        // Tilde grants with per-case-unique names, each dir/file/missing.
        for j in 0..rng.usize(0..=2) {
            let entry = format!("~/tg-{i}-{j}");
            let expanded = home.join(format!("tg-{i}-{j}"));
            let s = expanded.to_string_lossy().into_owned();
            match rng.u8(0..3) {
                0 => {
                    std::fs::create_dir_all(&expanded).unwrap();
                    c.dirs.push(s);
                }
                1 => {
                    std::fs::write(&expanded, b"x").unwrap();
                    c.files.push(s);
                }
                _ => {}
            }
            if rng.bool() {
                c.policy.profile.fs_read.push(entry);
            } else {
                c.policy.profile.fs_write.push(entry);
            }
        }
        // Project-scope config-lock dirs (per-case worktree).
        for dir in [".claude", ".codex"] {
            if rng.bool() {
                let p = c.work.join(dir);
                std::fs::create_dir_all(&p).unwrap();
                c.dirs.push(p.to_string_lossy().into_owned());
            }
        }
        // User-scope config-lock files — HOME-level, shared across cases, so
        // create-or-remove per case and compute the Rust side immediately.
        for file in [".claude/settings.json", ".codex/config.toml"] {
            let p = home.join(file);
            if rng.bool() {
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(&p, b"{}").unwrap();
                c.files.push(p.to_string_lossy().into_owned());
            } else {
                let _ = std::fs::remove_file(&p);
            }
        }
        rust_out.push(rust_effective(&c));
        cases.push(c);
    }
    let mismatches = diff_against_lean(&bin, &cases, &rust_out, "home");
    assert_eq!(mismatches, 0, "{mismatches} HOME-lane mismatches (seed {})", seed());
}

/// The Rust `interferes` against the Lean `interferesCheck` — the checker
/// whose Lean soundness proof backs the `fs_overlap` receipt field. Pairs
/// sharing a generation root overlap heavily; pairs on distinct roots mostly
/// do not; self-pairs always do (a box shares every path with itself).
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
            let mut c = gen_case(&mut rng, &root);
            // Half the cases additionally grant one shared existing dir, so
            // both verdicts occur: those pairs overlap, disjoint-root pairs
            // mostly do not, self-pairs always do.
            if i % 2 == 0 {
                c.policy.profile.fs_write.push(shared.clone());
            }
            serde_json::to_value(effective::compute_effective(
                &c.policy, &c.work, c.abi, &c.shape,
            ))
            .unwrap()
        })
        .collect();
    let mut pairs = Vec::new();
    let mut rust_verdicts = Vec::new();
    for i in 0..effs.len() {
        for j in [i, (i + 1) % effs.len(), (i + 2) % effs.len()] {
            let a: effective::EffectiveConfig =
                serde_json::from_value(effs[i].clone()).unwrap();
            let b: effective::EffectiveConfig =
                serde_json::from_value(effs[j].clone()).unwrap();
            rust_verdicts.push(effective::interferes(&a, &b).is_some());
            pairs.push(json!({"a": effs[i], "b": effs[j]}));
        }
    }
    let lean_out = lean_call(&bin, Some("--interferes"), &Value::Array(pairs.clone()));
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
