//! Integration tests for the read-only Sandbox dashboard API (`h5i serve`).
//!
//! Gated on the `web` feature — the dashboard (and `h5i_core::server`) only
//! exist when it is enabled, so `--no-default-features` test builds skip this.
#![cfg(feature = "web")]
//!
//! These boot the real axum router (`server::build_router`) against a temp repo
//! created via the `h5i` CLI, then hit the endpoints over loopback with a
//! blocking HTTP client — exercising the full handler path (repo open, env
//! enumeration, risk classification, JSON serialization).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use h5i_core::sandbox::{load_profile, IsolationClaim, NetMode, Profile, POLICY_FILE};
use tempfile::TempDir;

const H5I: &str = env!("CARGO_BIN_EXE_h5i");

struct Harness {
    dir: PathBuf,
    _root: TempDir,
}

impl Harness {
    fn new() -> Harness {
        let root = TempDir::new().expect("tempdir");
        let dir = root.path().join("repo");
        ok(Command::new("git").args(["init", "-b", "main"]).arg(&dir));
        git(&dir, &["config", "user.name", "Sbx Tester"]);
        git(&dir, &["config", "user.email", "sbx@h5i.test"]);
        std::fs::write(dir.join("README.md"), "seed\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-m", "seed"]);
        let h = Harness { dir, _root: root };
        h.h5i(&["init"]);
        h
    }

    fn h5i(&self, args: &[&str]) -> String {
        let out = Command::new(H5I)
            .args(args)
            .env("H5I_AGENT", "tester")
            // Deterministic default tier (the API tests assert `workspace`).
            .env("H5I_DEFAULT_ISOLATION", "workspace")
            .current_dir(&self.dir)
            .output()
            .expect("run h5i");
        assert!(
            out.status.success(),
            "h5i {} failed:\n{}\n{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        String::from_utf8_lossy(&out.stdout).into_string()
    }
}

fn git(dir: &PathBuf, args: &[&str]) {
    ok(Command::new("git").args(args).current_dir(dir));
}

fn ok(cmd: &mut Command) {
    let out = cmd.output().expect("spawn");
    assert!(out.status.success(), "{:?}: {}", cmd, String::from_utf8_lossy(&out.stderr));
}

trait IntoString {
    fn into_string(self) -> String;
}
impl IntoString for std::borrow::Cow<'_, str> {
    fn into_string(self) -> String {
        self.into_owned()
    }
}

fn write_env_toml(dir: &Path, toml_text: &str) {
    let policy_path = dir.join(POLICY_FILE);
    std::fs::create_dir_all(policy_path.parent().expect("policy parent")).unwrap();
    std::fs::write(policy_path, toml_text).unwrap();
}

fn load_env_profile(toml_text: &str, name: &str) -> Result<Profile, h5i_core::error::H5iError> {
    let dir = TempDir::new().expect("tempdir");
    write_env_toml(dir.path(), toml_text);
    load_profile(dir.path(), name, None)
}

#[test]
fn env_toml_minimal_profile_parses_with_safe_defaults() {
    let profile = load_env_profile(
        r#"
[service.dev]
cmd = "sleep 60"

[profile.default]
isolation = "process"
"#,
        "default",
    )
    .expect("minimal profile should parse");

    assert_eq!(profile.name, "default");
    assert_eq!(profile.isolation, IsolationClaim::Process);
    assert_eq!(profile.net_mode, NetMode::Deny);
    assert!(profile.fs_write.iter().any(|path| path == "$WORK"));
    assert!(profile.fs_deny.iter().any(|path| path == "~/.ssh"));
    assert_eq!(profile.max_procs, Some(256));
    assert!(profile.tools.is_empty());
}

#[test]
fn env_toml_rejects_unknown_profile_keys_and_malformed_values() {
    let err = load_env_profile(
        r#"
[profile.default]
isolation = "workspace"
mystery = true
"#,
        "default",
    )
    .expect_err("unknown profile keys must fail closed");
    assert!(err.to_string().contains("unknown field"), "{err}");

    let err = load_env_profile(
        r#"
[profile.default]
isolation = "workspace"
resources = { mem = "lots" }
"#,
        "default",
    )
    .expect_err("malformed resources.mem must fail closed");
    assert!(err.to_string().contains("invalid resources.mem"), "{err}");
}

#[test]
fn env_toml_empty_sections_keep_deny_defaults() {
    let profile = load_env_profile(
        r#"
[profile.default]
isolation = "process"

[profile.default.fs]
[profile.default.net]
[profile.default.env]
"#,
        "default",
    )
    .expect("empty sections should inherit built-in deny defaults");

    assert_eq!(profile.net_mode, NetMode::Deny);
    assert!(profile.fs_read.iter().any(|path| path == "/usr"));
    assert!(profile.fs_write.iter().any(|path| path == "$WORK"));
    assert!(profile.fs_deny.iter().any(|path| path == "~/.config/gh"));
    assert!(profile.env_pass.iter().any(|name| name == "PATH"));
}

#[test]
fn builtin_profiles_resolve_without_env_toml_file() {
    let dir = TempDir::new().expect("tempdir");

    let default = load_profile(dir.path(), "default", None).expect("default built-in");
    assert_eq!(default.isolation, IsolationClaim::Workspace);
    assert_eq!(default.net_mode, NetMode::Host);

    let agent =
        load_profile(dir.path(), "agent", Some(IsolationClaim::Supervised)).expect("agent built-in");
    assert!(
        matches!(agent.name.as_str(), "agent-claude" | "agent-codex"),
        "agent selector should resolve to a concrete runtime profile: {}",
        agent.name
    );
    assert_eq!(agent.isolation, IsolationClaim::Supervised);
    assert!(!agent.net_egress.is_empty());
}

/// Boot the router on an ephemeral loopback port and return its base URL plus a
/// guard whose drop aborts the server task.
async fn boot(dir: PathBuf) -> (String, tokio::task::JoinHandle<()>) {
    let state = Arc::new(h5i_core::server::AppState { repo_path: dir });
    let app = h5i_core::server::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), handle)
}

/// Blocking GET on a worker thread (reqwest blocking can't run on the tokio
/// runtime thread).
async fn get_json(url: String) -> (u16, serde_json::Value) {
    tokio::task::spawn_blocking(move || {
        let resp = reqwest::blocking::get(&url).expect("request");
        let status = resp.status().as_u16();
        let body: serde_json::Value = if status == 200 {
            resp.json().unwrap_or(serde_json::Value::Null)
        } else {
            serde_json::Value::Null
        };
        (status, body)
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn probe_endpoint_reports_host_tiers() {
    let h = Harness::new();
    let (base, server) = boot(h.dir.clone()).await;

    let (status, body) = get_json(format!("{base}/api/env/probe")).await;
    assert_eq!(status, 200);
    assert!(body["os"].is_string());
    let tiers = body["tiers"].as_array().expect("tiers array");
    // workspace, process, container are always reported.
    let claims: Vec<&str> = tiers.iter().filter_map(|t| t["claim"].as_str()).collect();
    assert!(claims.contains(&"workspace"));
    assert!(claims.contains(&"process"));
    assert!(claims.contains(&"container"));
    assert!(body["process_runnable"].is_boolean());
    // cgroup readiness is always reported (usable or, honestly, why not).
    assert!(body["cgroups"]["usable"].is_boolean());
    if !body["cgroups"]["usable"].as_bool().unwrap() {
        assert!(body["cgroups"]["detail"].is_string(), "unusable cgroups must explain why");
    }

    server.abort();
}

#[tokio::test]
async fn envs_endpoint_lists_created_env_with_risk() {
    let h = Harness::new();
    h.h5i(&["env", "create", "probe-fs"]);
    // A benign run so there is a capture + exec event to classify.
    h.h5i(&["env", "run", "probe-fs", "--", "sh", "-c", "echo hello"]);

    let (base, server) = boot(h.dir.clone()).await;

    // Fleet list.
    let (status, body) = get_json(format!("{base}/api/envs")).await;
    assert_eq!(status, 200);
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 1, "one env expected: {body}");
    let env = &arr[0];
    assert_eq!(env["id"], "env/tester/probe-fs");
    assert_eq!(env["isolation"], "workspace");
    assert!(env["has_workspace"].as_bool().unwrap());
    assert!(env["captures"].as_u64().unwrap() >= 1);
    // The risk roll-up is present and explainable.
    assert!(env["risk"]["score"].is_number());
    assert!(env["risk"]["level"].is_string());
    // A benign echo presses no boundary.
    assert_eq!(env["risk"]["score"], 0);

    // Detail view.
    let (status, detail) =
        get_json(format!("{base}/api/env/tester/probe-fs")).await;
    assert_eq!(status, 200);
    assert_eq!(detail["item"]["id"], "env/tester/probe-fs");
    assert!(detail["events"].as_array().unwrap().iter().any(|e| e["event"] == "exec"));
    assert!(!detail["captures"].as_array().unwrap().is_empty());
    // The enforced policy panel is populated.
    assert_eq!(detail["policy"]["isolation"], "workspace");

    // Unknown env → 404.
    let (status, _) = get_json(format!("{base}/api/env/tester/nope")).await;
    assert_eq!(status, 404);

    server.abort();
}

#[tokio::test]
async fn envs_endpoint_flags_boundary_pressure() {
    let h = Harness::new();
    h.h5i(&["env", "create", "snoop"]);
    // A command whose *text* presses on the boundary (sensitive target). Under
    // workspace isolation it's relabeled "weak isolation" (grey), not red — but
    // it must still surface as a finding the dashboard can show.
    h.h5i(&["env", "run", "snoop", "--", "sh", "-c", "echo reading /etc/shadow"]);

    let (base, server) = boot(h.dir.clone()).await;
    let (status, body) = get_json(format!("{base}/api/envs")).await;
    assert_eq!(status, 200);
    let env = &body.as_array().unwrap()[0];
    let findings = env["risk"]["findings"].as_array().expect("findings");
    assert!(
        findings.iter().any(|f| {
            f["evidence"].as_str().map(|e| e.contains("shadow")).unwrap_or(false)
        }),
        "sensitive-target evidence should appear in findings: {}",
        env["risk"]
    );

    server.abort();
}

#[tokio::test]
async fn capture_inspection_endpoint_renders_evidence() {
    let h = Harness::new();
    h.h5i(&["env", "create", "inspectme"]);
    h.h5i(&["env", "run", "inspectme", "--", "sh", "-c", "echo hello-capture"]);

    let (base, server) = boot(h.dir.clone()).await;

    // Pull the capture id from the env detail, then fetch its rendered evidence.
    let (status, detail) = get_json(format!("{base}/api/env/tester/inspectme")).await;
    assert_eq!(status, 200);
    let cap_id = detail["captures"][0]["id"].as_str().expect("a capture id").to_string();

    let (status, rendered) =
        get_json(format!("{base}/api/env/tester/inspectme/captures/{cap_id}")).await;
    assert_eq!(status, 200);
    assert!(rendered["render"].is_string(), "capture must render: {rendered}");

    // A capture id that doesn't belong to this env → 404 (the handler enforces
    // that the capture is evidence for the named env).
    let (status, _) =
        get_json(format!("{base}/api/env/tester/inspectme/captures/deadbeefdeadbeef")).await;
    assert_eq!(status, 404);

    server.abort();
}
