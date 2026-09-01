//! End-to-end tests for the runtime-detection lane (design-detect.md D1–D14).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

const H5I: &str = env!("CARGO_BIN_EXE_h5i");

fn run_ok(cmd: &mut Command) -> Output {
    let out = cmd.output().expect("command failed to spawn");
    assert!(
        out.status.success(),
        "command failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    out
}

fn git(dir: &Path, args: &[&str]) -> Output {
    run_ok(Command::new("git").args(args).current_dir(dir))
}

fn out_str(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

struct Repo {
    dir: PathBuf,
    _root: TempDir,
}

impl Repo {
    /// A repo whose `default` profile carries `policy`, committed.
    fn with_policy(policy: &str) -> Repo {
        let root = TempDir::new().expect("tempdir");
        let dir = root.path().join("repo");
        run_ok(Command::new("git").args(["init", "-b", "main"]).arg(&dir));
        git(&dir, &["config", "user.name", "Detect Tester"]);
        git(&dir, &["config", "user.email", "detect@h5i.test"]);
        std::fs::write(dir.join("README.md"), "seed\n").unwrap();
        std::fs::create_dir_all(dir.join(".h5i")).unwrap();
        std::fs::write(dir.join(".h5i/env.toml"), policy).unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-m", "seed"]);
        Repo { dir, _root: root }
    }

    fn h5i(&self, args: &[&str]) -> Output {
        Command::new(H5I)
            .args(args)
            .env("H5I_AGENT", "tester")
            // The workspace tier so these tests run identically on a host with
            // no Landlock: what is under test is the evidence lane, not the
            // confinement.
            .env("H5I_DEFAULT_ISOLATION", "workspace")
            .current_dir(&self.dir)
            .output()
            .expect("h5i failed to spawn")
    }

    fn h5i_ok(&self, args: &[&str]) -> Output {
        let out = self.h5i(args);
        assert!(
            out.status.success(),
            "h5i {} failed:\n{}",
            args.join(" "),
            out_str(&out)
        );
        out
    }

    fn env_dir(&self, slug: &str) -> PathBuf {
        self.dir.join(".git/.h5i/env/tester").join(slug)
    }

    fn last_receipt(&self, slug: &str) -> serde_json::Value {
        let log = self.env_dir(slug).join("receipt.jsonl");
        let blob = std::fs::read_to_string(&log)
            .unwrap_or_else(|_| panic!("no receipt log at {}", log.display()));
        let line = blob
            .lines()
            .rfind(|l| !l.trim().is_empty())
            .expect("a receipt");
        serde_json::from_str(line).expect("receipt json")
    }
}

/// Does this host actually have the collector? Used only to sharpen an
/// assertion, never to skip one. Every test below asserts something on both
/// kinds of host.
fn collector_available() -> bool {
    let out = Command::new(H5I)
        .args(["box", "detect", "probe", "--json"])
        .output()
        .expect("detect probe");
    serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .ok()
        .and_then(|v| v["usable"].as_bool())
        .unwrap_or(false)
}

// ── the block, and what its absence means ───────────────────────────────────

/// No `[detect]` section, no block. An absent block must mean "this profile
/// did not ask", so that a *present* one can carry a real answer.
#[test]
fn a_profile_that_does_not_ask_produces_no_runtime_block() {
    let r = Repo::with_policy("[profile.default]\nisolation = \"workspace\"\n");
    r.h5i_ok(&["env", "create", "quiet"]);
    r.h5i_ok(&["env", "run", "quiet", "--", "echo", "hello"]);

    let rec = r.last_receipt("quiet");
    assert_eq!(rec["source"], "host-env-run");
    assert!(
        rec.get("runtime").is_none(),
        "an unasked-for lane must not appear at all: {rec}"
    );
}

/// The property the whole lane rests on. A profile that asked to be watched
/// gets a block *either way*: with detections when the probe attached, with
/// a reason when it could not. What must never happen is an empty block, or no
/// block, on a run nobody watched. That reads exactly like a quiet box.
#[test]
fn a_run_that_asked_to_be_watched_always_carries_a_block() {
    let r = Repo::with_policy(
        "[profile.default]\nisolation = \"workspace\"\n\n[profile.default.detect]\nenabled = true\n",
    );
    r.h5i_ok(&["env", "create", "watched"]);
    r.h5i_ok(&["env", "run", "watched", "--", "echo", "hello"]);

    let rec = r.last_receipt("watched");
    let rt = rec
        .get("runtime")
        .unwrap_or_else(|| panic!("a profile that asked must get a block: {rec}"));
    assert_eq!(rt["lane"], "kernel-bpf");

    if collector_available() {
        assert!(
            rt.get("unavailable").is_none(),
            "the collector is available here, so the run should have been watched: {rt}"
        );
        assert_eq!(rt["coverage"], "full");
    } else {
        let why = rt["unavailable"]
            .as_str()
            .unwrap_or_else(|| panic!("an unwatched run must say why: {rt}"));
        assert!(!why.is_empty());
        assert_eq!(rt["coverage"], "none");
        // And the detections list must be empty *and* explained. Never empty
        // and silent.
        assert!(
            rt.get("detections").is_none()
                || rt["detections"].as_array().map(|a| a.is_empty()) == Some(true)
        );
    }
}

/// `require = true` means what it says: refuse the run rather than perform it
/// unwatched. On a host that *can* watch, the same profile runs normally.
/// The switch is about the failure, not about the feature.
#[test]
fn require_refuses_the_run_when_the_probe_cannot_attach() {
    let r = Repo::with_policy(
        "[profile.default]\nisolation = \"workspace\"\n\n\
         [profile.default.detect]\nenabled = true\nrequire = true\n",
    );
    r.h5i_ok(&["env", "create", "strict"]);
    let out = r.h5i(&["env", "run", "strict", "--", "echo", "hello"]);
    let text = out_str(&out);

    if collector_available() {
        assert!(out.status.success(), "should have run and been watched:\n{text}");
    } else {
        assert!(
            !out.status.success(),
            "an unwatchable run under `require = true` must refuse:\n{text}"
        );
        assert!(
            text.contains("require = true"),
            "the refusal must name the setting that caused it:\n{text}"
        );
        // And it must be actionable: the reason the probe could not attach.
        assert!(
            text.contains("CAP_BPF") || text.contains("probe") || text.contains("kernel"),
            "the refusal must carry the probe's own reason:\n{text}"
        );
    }
}

// ── fail-closed policy lints ────────────────────────────────────────────────

/// `enabled = true` with `rules = []` reads as watching and watches nothing.
/// Refused at load, like every other policy lint in this product.
#[test]
fn an_enabled_section_that_selects_nothing_is_refused() {
    let r = Repo::with_policy(
        "[profile.default]\nisolation = \"workspace\"\n\n\
         [profile.default.detect]\nenabled = true\nrules = []\n",
    );
    let out = r.h5i(&["env", "create", "empty-rules"]);
    let text = out_str(&out);
    assert!(!out.status.success(), "should have refused:\n{text}");
    assert!(text.contains("watches for nothing"), "{text}");
}

/// `require = true` with `enabled = false` reads as "refuse to run unwatched"
/// and would in fact never watch. A policy that contradicts itself is refused
/// rather than resolved in whichever direction the code happens to take.
#[test]
fn require_without_enabled_is_refused() {
    let r = Repo::with_policy(
        "[profile.default]\nisolation = \"workspace\"\n\n\
         [profile.default.detect]\nrequire = true\n",
    );
    let out = r.h5i(&["env", "create", "contradictory"]);
    let text = out_str(&out);
    assert!(!out.status.success(), "should have refused:\n{text}");
    assert!(text.contains("require = true"), "{text}");
}

/// A rule id nobody provides must stop the run reading as watched. The
/// selector is checked by the collector, so this surfaces as a refusal to
/// attach, with the typo named.
#[test]
fn a_misspelled_rule_makes_the_run_report_itself_unwatched() {
    let r = Repo::with_policy(
        "[profile.default]\nisolation = \"workspace\"\n\n\
         [profile.default.detect]\nenabled = true\nrules = [\"net.direct-egres\"]\n",
    );
    r.h5i_ok(&["env", "create", "typo"]);
    r.h5i_ok(&["env", "run", "typo", "--", "echo", "hello"]);

    let rec = r.last_receipt("typo");
    let rt = &rec["runtime"];
    let why = rt["unavailable"].as_str().unwrap_or("");
    assert!(
        why.contains("net.direct-egres"),
        "the receipt must name the selector that matched nothing: {rt}"
    );
}

// ── the policy digest ───────────────────────────────────────────────────────

/// Whether a box was watched is part of what its policy claimed, so it has to
/// be inside the digest a reviewer compares two boxes by.
#[test]
fn enabling_detection_changes_the_pinned_policy_digest() {
    let plain = Repo::with_policy("[profile.default]\nisolation = \"workspace\"\n");
    plain.h5i_ok(&["env", "create", "a"]);
    let d1 = std::fs::read_to_string(plain.env_dir("a").join("policy.resolved.toml")).unwrap();

    let watched = Repo::with_policy(
        "[profile.default]\nisolation = \"workspace\"\n\n[profile.default.detect]\nenabled = true\n",
    );
    watched.h5i_ok(&["env", "create", "b"]);
    let d2 = std::fs::read_to_string(watched.env_dir("b").join("policy.resolved.toml")).unwrap();

    assert!(d2.contains("[profile.detect]"), "{d2}");
    assert!(!d1.contains("detect"), "an unwatched box's policy must be unchanged:\n{d1}");
    assert_ne!(d1, d2);
}

// ── the verbs ───────────────────────────────────────────────────────────────

/// `detect probe` has to work on the hosts that *cannot* run the collector,
/// because those are the hosts whose users need to know why.
#[test]
fn probe_reports_the_host_and_names_a_fix_when_it_can() {
    let out = run_ok(&mut {
        let mut c = Command::new(H5I);
        c.args(["box", "detect", "probe", "--json"]);
        c
    });
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("detect probe --json must be json");
    assert!(v["os"].is_string());
    if v["usable"] == serde_json::Value::Bool(false) {
        assert!(
            v["detail"].is_string(),
            "an unusable host must say why: {v}"
        );
    }

    // The human form must not be empty, and must not claim the feature works
    // when it does not.
    let human = out_str(&run_ok(&mut {
        let mut c = Command::new(H5I);
        c.args(["box", "detect", "probe"]);
        c
    }));
    assert!(human.contains("Runtime detection"), "{human}");
}

/// The catalogue is the answer to "would it have caught X", so it has to be
/// printable without a repository, a box, or a kernel.
#[test]
fn rules_prints_the_catalogue_and_filters_by_family() {
    let all = out_str(&run_ok(&mut {
        let mut c = Command::new(H5I);
        c.args(["box", "detect", "rules", "--json"]);
        c
    }));
    let rows: Vec<serde_json::Value> = serde_json::from_str(&all).expect("json");
    assert!(rows.len() >= 15, "the catalogue looks truncated: {}", rows.len());
    assert!(rows.iter().any(|r| r["id"] == "net.direct-egress"));
    assert!(rows.iter().all(|r| r["detail"].as_str().is_some_and(|d| !d.is_empty())));

    let net = out_str(&run_ok(&mut {
        let mut c = Command::new(H5I);
        c.args(["box", "detect", "rules", "--filter", "net", "--json"]);
        c
    }));
    let net_rows: Vec<serde_json::Value> = serde_json::from_str(&net).expect("json");
    assert!(net_rows.iter().all(|r| r["family"] == "net"));
    assert!(net_rows.len() < rows.len());

    // A filter that matches nothing is an error, not an empty list: an empty
    // list reads as "no such rules exist".
    let bogus = Command::new(H5I)
        .args(["box", "detect", "rules", "--filter", "nonsense"])
        .output()
        .expect("spawn");
    assert!(!bogus.status.success());
}

/// `detect show` on a box nobody watched must say so, not print nothing.
#[test]
fn show_says_nothing_was_watched_rather_than_printing_an_empty_list() {
    let r = Repo::with_policy("[profile.default]\nisolation = \"workspace\"\n");
    r.h5i_ok(&["env", "create", "unwatched"]);
    r.h5i_ok(&["env", "run", "unwatched", "--", "echo", "hi"]);

    let text = out_str(&r.h5i_ok(&["box", "detect", "show", "unwatched"]));
    assert!(text.contains("no receipt"), "{text}");
    assert!(text.contains("not the same as nothing happening"), "{text}");
}

/// And on a box that asked, it must render the block, including the
/// unavailable one, which is the case a reader most needs to see.
#[test]
fn show_renders_a_block_that_could_not_be_collected() {
    let r = Repo::with_policy(
        "[profile.default]\nisolation = \"workspace\"\n\n[profile.default.detect]\nenabled = true\n",
    );
    r.h5i_ok(&["env", "create", "asked"]);
    r.h5i_ok(&["env", "run", "asked", "--", "echo", "hi"]);

    let text = out_str(&r.h5i_ok(&["box", "detect", "show", "asked"]));
    assert!(text.contains("Runtime detection: env/tester/asked"), "{text}");
    if !collector_available() {
        assert!(text.contains("not observed"), "{text}");
    }

    let json = out_str(&r.h5i_ok(&["box", "detect", "show", "asked", "--json"]));
    let rows: Vec<serde_json::Value> = serde_json::from_str(&json).expect("json");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["runtime"]["lane"], "kernel-bpf");
}

// ── the status page ─────────────────────────────────────────────────────────

/// `box status` must report whether the host can *deliver* what the profile
/// asked for. A page that printed only the profile's intent would be the
/// "reads like enforcement, enforces nothing" failure this product keeps
/// finding in itself.
#[test]
fn status_says_whether_the_host_can_actually_watch() {
    let r = Repo::with_policy(
        "[profile.default]\nisolation = \"workspace\"\n\n[profile.default.detect]\nenabled = true\n",
    );
    r.h5i_ok(&["env", "create", "shown"]);
    let text = out_str(&r.h5i_ok(&["box", "status", "shown"]));
    assert!(text.contains("detect   :"), "{text}");
    if collector_available() {
        assert!(text.contains("kernel-observed"), "{text}");
    } else {
        assert!(text.contains("NOT watching on this host"), "{text}");
    }
}
