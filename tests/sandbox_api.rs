//! Integration tests for the read-only Sandbox dashboard API (`h5i serve`).
//!
//! These boot the real axum router (`server::build_router`) against a temp repo
//! created via the `h5i` CLI, then hit the endpoints over loopback with a
//! blocking HTTP client — exercising the full handler path (repo open, env
//! enumeration, risk classification, JSON serialization).

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

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
