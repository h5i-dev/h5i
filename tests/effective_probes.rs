//! Conformance probes (ROADMAP.md §V4, "model versus kernel"): the Lean
//! model predicts, from a real box's `policy.effective.json`, which reads
//! and writes its Landlock ruleset must allow and deny — and this test runs
//! those accesses inside the real box and holds the kernel to the
//! prediction. This is `sandbox::verify_exec` generalized: the model is an
//! instrument, and an instrument gets checked against the world.
//!
//! Skips loudly when the Lean binary is missing (`cd lean && lake build`;
//! `H5I_DRT_REQUIRE=1` turns absence into failure) or when this host cannot
//! run the process tier (the same gate the kernel integration tests use).
//!
//! Probe selection is deliberately conservative, and the caps are named:
//! - Read probes: the worktree, the box-side env-dir file (owner-writable,
//!   so a denial is Landlock and not DAC), and the dump's ro grants that
//!   exist. `ls` for directories, `cat` for files — both drive `open`,
//!   which Landlock hooks; `access(2)` is not hooked and is never used.
//! - Write probes only on paths this test's user owns (the worktree and
//!   the env dir): a write denial on a system path would be DAC's answer as
//!   often as Landlock's, and a conformance test must not launder one as
//!   the other.
//! - No `/tmp` (kernel tiers redirect it per env — bind semantics are not
//!   in the prediction layer yet) and no `/proc` (the pidns re-grant is
//!   not either).

#![cfg(target_os = "linux")]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const H5I: &str = env!("CARGO_BIN_EXE_h5i");

fn out_str(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

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
    eprintln!("SKIPPING conformance probes: {msg}");
    None
}

struct Repo {
    dir: PathBuf,
    _root: tempfile::TempDir,
}

impl Repo {
    fn new() -> Repo {
        // NOT the system tempdir: that is under `/tmp`, which the kernel
        // tiers shadow with a per-env private tmp — the whole repo would
        // vanish from inside the box and every probe would read as denied.
        let root =
            tempfile::TempDir::new_in(env!("CARGO_TARGET_TMPDIR")).expect("tempdir");
        let dir = root.path().join("repo");
        let ok = |o: Output| assert!(o.status.success(), "{}", out_str(&o));
        let git = |args: &[&str], cwd: &Path| {
            Command::new("git").args(args).current_dir(cwd).output().expect("git")
        };
        ok(Command::new("git").args(["init", "-b", "main"]).arg(&dir).output().unwrap());
        ok(git(&["config", "user.name", "Probe Tester"], &dir));
        ok(git(&["config", "user.email", "probe@h5i.test"], &dir));
        std::fs::write(dir.join("README.md"), "seed\n").unwrap();
        ok(git(&["add", "."], &dir));
        ok(git(&["commit", "-m", "seed"], &dir));
        Repo { dir, _root: root }
    }

    fn h5i(&self, args: &[&str]) -> Output {
        Command::new(H5I)
            .args(args)
            .env("H5I_AGENT", "tester")
            .env("H5I_DEFAULT_ISOLATION", "workspace")
            .current_dir(&self.dir)
            .output()
            .expect("failed to run h5i")
    }

    fn env_dir(&self, slug: &str) -> PathBuf {
        self.dir.join(".git/.h5i/env/tester").join(slug)
    }
}

#[derive(Clone)]
struct Probe {
    /// Path as the box sees it (kernel tiers share the host's view).
    path: String,
    access: &'static str,
    /// The in-box shell command driving a real `open` on the path.
    cmd: String,
    /// Why this probe is in the set — printed on mismatch.
    why: &'static str,
}

fn read_probe(path: &Path, why: &'static str) -> Probe {
    let p = path.to_string_lossy();
    let cmd = if path.is_dir() {
        format!("ls {p} > /dev/null && echo PROBE_OK")
    } else {
        format!("cat {p} > /dev/null && echo PROBE_OK")
    };
    Probe { path: p.into_owned(), access: "read", cmd, why }
}

fn write_probe(path: &Path, why: &'static str) -> Probe {
    let p = path.to_string_lossy();
    Probe {
        path: p.clone().into_owned(),
        access: "write",
        cmd: format!("echo x >> {p} && echo PROBE_OK"),
        why,
    }
}

#[test]
fn kernel_agrees_with_model_predictions() {
    let Some(bin) = lean_bin() else { return };
    let r = Repo::new();
    let create = r.h5i(&["env", "create", "probes", "--isolation", "process"]);
    if !create.status.success() {
        eprintln!(
            "SKIPPING conformance probes: process tier not runnable here:\n{}",
            out_str(&create)
        );
        return;
    }

    let env_dir = r.env_dir("probes");
    let dump_path = env_dir.join("policy.effective.json");
    let dump: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&dump_path).expect("dump written at create"))
            .unwrap();

    // The probe set, from the dump itself plus the two owner-writable
    // Landlock-boundary paths described in the module docs.
    let work = PathBuf::from(dump["work"].as_str().unwrap());
    std::fs::write(work.join("probe.txt"), "content\n").unwrap();
    let mut probes = vec![
        read_probe(&work.join("probe.txt"), "worktree file must be readable"),
        write_probe(&work.join("probe.txt"), "worktree file must be writable"),
        read_probe(&env_dir.join("manifest.json"), "env dir is outside the box's grants"),
        write_probe(&env_dir.join("probe-w.txt"), "env dir is owner-writable, Landlock must deny"),
    ];
    for g in dump["landlock"]["ro"].as_array().unwrap().iter().take(3) {
        let p = PathBuf::from(g.as_str().unwrap());
        if p.exists() {
            probes.push(read_probe(&p, "dump ro grant must be readable"));
        }
    }

    // The model's verdicts, from the dump the box actually enforces.
    let predict_input = serde_json::json!({
        "config": dump,
        "probes": probes.iter()
            .map(|p| serde_json::json!({"path": p.path, "access": p.access}))
            .collect::<Vec<_>>(),
    });
    let mut child = Command::new(&bin)
        .arg("--predict")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn lean model");
    child.stdin.take().unwrap().write_all(predict_input.to_string().as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "lean predict exited with {:?}", out.status);
    let predictions: Vec<bool> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(predictions.len(), probes.len());

    // The kernel's verdicts, from the real box.
    let mut mismatches = Vec::new();
    for (probe, predicted) in probes.iter().zip(&predictions) {
        let run = r.h5i(&["env", "run", "probes", "--", "sh", "-c", &probe.cmd]);
        let observed = out_str(&run).contains("PROBE_OK");
        if observed != *predicted {
            mismatches.push(format!(
                "{} {} — model says {}, kernel says {} ({})\n  cmd: {}\n  output:\n{}",
                probe.access,
                probe.path,
                if *predicted { "allow" } else { "deny" },
                if observed { "allowed" } else { "denied" },
                probe.why,
                probe.cmd,
                out_str(&run)
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "the kernel disagreed with the model on {} of {} probes:\n{}",
        mismatches.len(),
        probes.len(),
        mismatches.join("\n")
    );
}
