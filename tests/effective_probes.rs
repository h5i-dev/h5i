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
//! - Bind semantics ARE in the prediction layer (`H5iSpec/Predict.lean`):
//!   accesses beneath a bind target are judged on the rebased source path,
//!   and a read-only remount denies writes outright. So the private-`/tmp`
//!   redirect is probed — through the *run-shape* dump, because binds are
//!   runtime state: the test runs a warmup command first and reads the dump
//!   that run wrote at the apply seam.
//! - Symlinks and procfs are in the layer too. The harness plants links in
//!   the worktree and reports them as facts (`symlinks` in the predict
//!   input); the model chases them and judges the *resolved* object — so a
//!   link to an ungranted path is predicted denied, links being how grants
//!   would otherwise be smuggled. `/proc` under a pidns is the private
//!   procfs with its read-only re-grant: reads allowed, writes denied, and
//!   existence is namespace-local (`box-local` checks carry the harness's
//!   a-priori knowledge: `/proc/self` exists, a host pid's entry does not).

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
    /// A-priori in-box presence, for `box-local` checks (private procfs):
    /// the host cannot be stat'd for those, so the harness states what it
    /// knows by construction. `None` for host-stattable checks.
    present: Option<bool>,
}

fn read_probe(path: &Path, why: &'static str) -> Probe {
    let p = path.to_string_lossy();
    let cmd = if path.is_dir() {
        format!("ls {p} > /dev/null && echo PROBE_OK")
    } else {
        format!("cat {p} > /dev/null && echo PROBE_OK")
    };
    Probe { path: p.into_owned(), access: "read", cmd, why, present: None }
}

fn write_probe(path: &Path, why: &'static str) -> Probe {
    let p = path.to_string_lossy();
    Probe {
        path: p.clone().into_owned(),
        access: "write",
        cmd: format!("echo x >> {p} && echo PROBE_OK"),
        why,
        present: None,
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
    assert!(dump_path.is_file(), "dump written at create");
    // Binds are runtime state (`prepare_*` runs per invocation), so predict
    // against the RUN-shape dump: a warmup run rewrites the file at the
    // apply seam with the binds every later probe run will get too. The
    // warmup also seeds a file in the box's private `/tmp`, for the
    // existence probes below.
    let warmup = r.h5i(&[
        "env", "run", "probes", "--", "sh", "-c",
        "echo seeded > /tmp/h5i-seeded.txt",
    ]);
    assert!(warmup.status.success(), "warmup run failed:\n{}", out_str(&warmup));
    let dump: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&dump_path).expect("run rewrote the dump"))
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
    // The private-`/tmp` redirect, when the run dump declares it: reads and
    // writes under `/tmp` are judged on the rebased backing path, and the
    // box's own scratch must be usable.
    let host_tmp_file =
        PathBuf::from(format!("/tmp/h5i-host-probe-{}.txt", std::process::id()));
    let binds = dump["binds"].as_array().unwrap();
    if binds.iter().any(|b| b["target"] == "/tmp" && b["writable"] == true) {
        probes.push(Probe {
            path: "/tmp".into(),
            access: "read",
            cmd: "ls /tmp > /dev/null && echo PROBE_OK".into(),
            why: "private tmp bind must be readable",
            present: None,
        });
        probes.push(Probe {
            path: "/tmp/h5i-probe.txt".into(),
            access: "write",
            cmd: "echo x >> /tmp/h5i-probe.txt && echo PROBE_OK".into(),
            why: "private tmp bind must be writable",
            present: None,
        });
        // Existence through the bind, WITHIN one run: seed and read back in
        // the same invocation. Not across runs — this suite's own first
        // failure established that the private-tmp scratch is wiped per run
        // (`prepare_private_tmp` re-runs each invocation), which is env
        // lifecycle above the mount layer: the model correctly resolved the
        // warmup's seeded file to an existing backing path, and the next
        // run's wipe removed it before the probe could look. So existence
        // facts are only carried within the invocation that stats them.
        probes.push(Probe {
            path: "/tmp/h5i-roundtrip.txt".into(),
            access: "write",
            cmd: "echo s > /tmp/h5i-roundtrip.txt \
                  && cat /tmp/h5i-roundtrip.txt > /dev/null && echo PROBE_OK"
                .into(),
            why: "private tmp scratch must round-trip within a run",
            present: None,
        });
        // …and a file planted on the HOST's real /tmp: permitted, but the
        // resolved backing path has no such object, so the read must fail
        // with ENOENT — the exact confusion that broke the first version of
        // this harness, now a prediction instead of a surprise.
        std::fs::write(&host_tmp_file, b"host side\n").expect("plant host /tmp file");
        probes.push(Probe {
            path: host_tmp_file.to_string_lossy().into_owned(),
            access: "read",
            cmd: format!(
                "cat {} > /dev/null && echo PROBE_OK",
                host_tmp_file.to_string_lossy()
            ),
            why: "host /tmp file must be invisible behind the private-tmp bind",
            present: None,
        });
    } else {
        eprintln!("probe cap: run dump declares no writable /tmp bind — tmp probes skipped");
    }

    // Symlinks: planted host-side in the worktree (unbound, so link object
    // and host object coincide), reported to the model as facts. One at an
    // ungranted target (the env manifest — exists, owner-readable, outside
    // every grant: the smuggling attempt), one at a granted worktree file
    // (the legitimate alias).
    let link_secret = work.join("link-secret");
    let link_alias = work.join("link-alias");
    let manifest_path = env_dir.join("manifest.json");
    std::os::unix::fs::symlink(&manifest_path, &link_secret).unwrap();
    std::os::unix::fs::symlink(work.join("probe.txt"), &link_alias).unwrap();
    let symlinks = serde_json::json!([
        { "link": link_secret.to_string_lossy(), "target": manifest_path.to_string_lossy() },
        { "link": link_alias.to_string_lossy(),
          "target": work.join("probe.txt").to_string_lossy() },
    ]);
    probes.push(Probe {
        path: link_secret.to_string_lossy().into_owned(),
        access: "read",
        cmd: format!("cat {} > /dev/null && echo PROBE_OK", link_secret.to_string_lossy()),
        why: "a worktree symlink must not smuggle an ungranted target",
        present: None,
    });
    probes.push(Probe {
        path: link_alias.to_string_lossy().into_owned(),
        access: "read",
        cmd: format!("cat {} > /dev/null && echo PROBE_OK", link_alias.to_string_lossy()),
        why: "a worktree symlink to a granted file confers exactly that file",
        present: None,
    });

    // procfs, when the run shape carries a pid namespace: the private
    // procfs is re-granted read-only, and its contents are namespace-local.
    if dump["run"]["pidns"] == true {
        probes.push(Probe {
            path: "/proc/self/status".into(),
            access: "read",
            cmd: "cat /proc/self/status > /dev/null && echo PROBE_OK".into(),
            why: "private procfs must be readable (the re-grant)",
            present: Some(true),
        });
        probes.push(Probe {
            path: "/proc/self/comm".into(),
            access: "write",
            cmd: "echo x > /proc/self/comm && echo PROBE_OK".into(),
            why: "private procfs re-grant is read-only",
            present: Some(true),
        });
        let host_pid = std::process::id();
        probes.push(Probe {
            path: format!("/proc/{host_pid}/status"),
            access: "read",
            cmd: format!("cat /proc/{host_pid}/status > /dev/null && echo PROBE_OK"),
            why: "host pids must be invisible in the private pid namespace",
            present: Some(false),
        });
    } else {
        eprintln!("probe cap: run shape has no pidns — procfs probes skipped");
    }

    // The model's verdicts, from the dump the box actually enforces.
    let predict_input = serde_json::json!({
        "config": dump,
        "probes": probes.iter()
            .map(|p| serde_json::json!({"path": p.path, "access": p.access}))
            .collect::<Vec<_>>(),
        "symlinks": symlinks,
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
    let verdicts: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(verdicts.len(), probes.len());

    // The model owns the semantics (permission, resolution through the
    // binds, what must exist); the harness owns the measurement: stat the
    // model-resolved `real` path on the host, BEFORE any probe runs, so a
    // probe's own writes cannot retroactively change an expectation.
    let expected: Vec<(bool, String)> = verdicts
        .iter()
        .zip(&probes)
        .map(|(v, probe)| {
            let allow = v["allow"].as_bool().unwrap();
            let real = PathBuf::from(v["real"].as_str().unwrap());
            let present = match v["check"].as_str().unwrap() {
                "exists" => real.exists(),
                "creatable" => {
                    real.exists() || real.parent().is_some_and(|p| p.is_dir())
                }
                // Namespace-local: the harness states what it knows by
                // construction, because no host stat can answer this.
                "box-local" => probe
                    .present
                    .expect("box-local verdict on a probe without a-priori presence"),
                other => panic!("unknown existence check '{other}'"),
            };
            (allow && present, format!("allow={allow} real={} present={present}", real.display()))
        })
        .collect();

    // The kernel's answers, from the real box.
    let mut mismatches = Vec::new();
    for (probe, (want, verdict)) in probes.iter().zip(&expected) {
        let run = r.h5i(&["env", "run", "probes", "--", "sh", "-c", &probe.cmd]);
        let observed = out_str(&run).contains("PROBE_OK");
        if observed != *want {
            mismatches.push(format!(
                "{} {} — model expects {}, kernel says {} ({})\n  model: {}\n  cmd: {}\n  output:\n{}",
                probe.access,
                probe.path,
                if *want { "success" } else { "failure" },
                if observed { "success" } else { "failure" },
                probe.why,
                verdict,
                probe.cmd,
                out_str(&run)
            ));
        }
    }
    let _ = std::fs::remove_file(&host_tmp_file);
    assert!(
        mismatches.is_empty(),
        "the kernel disagreed with the model on {} of {} probes:\n{}",
        mismatches.len(),
        probes.len(),
        mismatches.join("\n")
    );
}
