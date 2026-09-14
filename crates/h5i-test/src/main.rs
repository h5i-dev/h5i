//! Repository-portable security regression tests.
//!
//! h5i owns execution and evidence. The repository owns the property being
//! tested: an arbitrary external oracle reads the result bundle and decides
//! pass or fail. This deliberately contains no assertion language.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use clap::Parser;
use h5i_core::browser_session as bs;
use h5i_wire::message::{Body, StoredRequest, StoredResponse, body_file, message_file};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;
use wait_timeout::ChildExt;

const SCHEMA: &str = "h5i.test/v1";
const MAX_ORACLE_OUTPUT: usize = 64 * 1024;
static SESSION_NONCE: AtomicU64 = AtomicU64::new(1);

#[derive(Parser)]
#[command(
    name = "h5i test",
    version,
    about = "Replay portable attack flows and check them with repository-owned oracles."
)]
struct Cli {
    /// One test file, or a directory searched for .yaml, .yml and .json files.
    #[arg(value_name = "PATH", default_value = ".h5i-tests/tests")]
    path: PathBuf,
    /// Application base URL. Test request paths are resolved against it.
    #[arg(long, value_name = "URL")]
    target: String,
    /// Directory for result.json, JUnit and response artifacts.
    #[arg(long, value_name = "DIR", default_value = "h5i-test-results")]
    output: PathBuf,
    /// OpenAPI JSON or YAML used as the coverage denominator.
    #[arg(long, value_name = "FILE")]
    openapi: Option<PathBuf>,
    /// Fail when oracle-checked operation coverage is below this percentage.
    /// Without this flag coverage is report-only.
    #[arg(long, value_name = "PERCENT")]
    min_coverage: Option<f64>,
    /// Print the complete run result to stdout as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TestFile {
    version: String,
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    actors: BTreeMap<String, Actor>,
    requests: BTreeMap<String, RequestTemplate>,
    flow: Vec<SendStep>,
    oracle: Oracle,
    #[serde(default)]
    cleanup: Vec<SendStep>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Actor {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestTemplate {
    #[serde(default = "default_method")]
    method: String,
    path: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    json: Option<Value>,
}

fn default_method() -> String {
    "GET".to_string()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SendStep {
    send: String,
    #[serde(default, rename = "as")]
    actor: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    save: Option<String>,
    #[serde(default)]
    set: Vec<String>,
    #[serde(default)]
    unset: Vec<String>,
    #[serde(default)]
    create: bool,
    #[serde(default)]
    no_follow: bool,
    #[serde(default)]
    extract: BTreeMap<String, String>,
    #[serde(default)]
    covers: Option<Covers>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Covers {
    operation: String,
    #[serde(default)]
    mutation: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Oracle {
    command: Vec<String>,
    #[serde(default)]
    inputs: Vec<String>,
    #[serde(default = "default_timeout")]
    timeout_seconds: u64,
}

fn default_timeout() -> u64 {
    60
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Status {
    Pass,
    Fail,
    Error,
}

#[derive(Debug, Serialize)]
struct StepResult {
    name: String,
    actor: String,
    request: String,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    response: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    covers: Option<CoversResult>,
}

#[derive(Debug, Serialize)]
struct CoversResult {
    operation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    mutation: Option<String>,
}

#[derive(Debug, Serialize)]
struct TestResult {
    id: String,
    name: String,
    file: String,
    status: Status,
    duration_ms: u128,
    steps: Vec<StepResult>,
    oracle: OracleResult,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    cleanup_errors: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    covered_operations: BTreeSet<String>,
    mutations: BTreeSet<String>,
}

#[derive(Debug, Default, Serialize)]
struct OracleResult {
    ran: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    #[serde(skip_serializing_if = "String::is_empty")]
    stdout: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    stderr: String,
}

#[derive(Debug, Serialize)]
struct Coverage {
    total_operations: usize,
    covered_operations: usize,
    percent: f64,
    operations: Vec<String>,
    untested: Vec<String>,
    mutations: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    minimum: Option<f64>,
    gate_passed: bool,
}

#[derive(Debug, Serialize)]
struct RunResult {
    schema: &'static str,
    target: String,
    status: Status,
    passed: usize,
    failed: usize,
    errors: usize,
    tests: Vec<TestResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    coverage: Option<Coverage>,
}

#[derive(Debug, Serialize)]
struct OracleBundle {
    schema: &'static str,
    test: String,
    target: String,
    responses: BTreeMap<String, ResponseArtifact>,
    variables: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
struct ResponseArtifact {
    status: Option<u16>,
    headers: Vec<(String, String)>,
    body: String,
    request: String,
}

struct Session {
    logical: String,
    name: String,
    id: String,
}

struct TestContext<'a> {
    h5i: &'a OsString,
    target: &'a Url,
    target_text: &'a str,
    output: &'a Path,
    declared_actors: &'a BTreeMap<String, Actor>,
    requests: &'a BTreeMap<String, RequestTemplate>,
    sessions: BTreeMap<String, Session>,
    variables: BTreeMap<String, String>,
    responses: BTreeMap<String, ResponseArtifact>,
    covered: BTreeSet<String>,
    mutations: BTreeSet<String>,
}

fn main() {
    match run(Cli::parse()) {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("{error:#}");
            std::process::exit(2);
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<i32> {
    if let Some(minimum) = cli.min_coverage
        && (!minimum.is_finite() || !(0.0..=100.0).contains(&minimum))
    {
        anyhow::bail!("--min-coverage must be between 0 and 100");
    }
    if cli.min_coverage.is_some() && cli.openapi.is_none() {
        anyhow::bail!(
            "--min-coverage needs --openapi: without a denominator there is no percentage"
        );
    }
    let target =
        Url::parse(&cli.target).map_err(|e| anyhow::anyhow!("--target is not a URL: {e}"))?;
    if !matches!(target.scheme(), "http" | "https") {
        anyhow::bail!("--target must use http or https");
    }
    create_private_dir(&cli.output)?;
    let files = discover(&cli.path)?;
    if files.is_empty() {
        anyhow::bail!(
            "{} contains no .yaml, .yml or .json tests",
            cli.path.display()
        );
    }
    refuse_duplicate_ids(&files)?;

    let h5i = std::env::var_os("H5I_BIN").unwrap_or_else(|| OsString::from("h5i"));
    let mut tests = Vec::new();
    for file in files {
        tests.push(run_file(&h5i, &target, &cli.target, &cli.output, &file));
    }

    let coverage = match &cli.openapi {
        Some(path) => Some(coverage(path, &tests, cli.min_coverage)?),
        None => None,
    };
    let passed = tests.iter().filter(|t| t.status == Status::Pass).count();
    let failed = tests.iter().filter(|t| t.status == Status::Fail).count();
    let errors = tests.iter().filter(|t| t.status == Status::Error).count();
    let coverage_failed = coverage.as_ref().is_some_and(|c| !c.gate_passed);
    let status = if errors > 0 {
        Status::Error
    } else if failed > 0 || coverage_failed {
        Status::Fail
    } else {
        Status::Pass
    };
    let result = RunResult {
        schema: SCHEMA,
        target: cli.target,
        status,
        passed,
        failed,
        errors,
        tests,
        coverage,
    };
    let result_path = cli.output.join("result.json");
    fs::write(&result_path, serde_json::to_vec_pretty(&result)?)?;
    fs::write(cli.output.join("junit.xml"), junit(&result))?;

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        print_human(&result, &result_path);
    }
    Ok(match status {
        Status::Pass => 0,
        Status::Fail => 1,
        Status::Error => 2,
    })
}

fn run_file(
    h5i: &OsString,
    target: &Url,
    target_text: &str,
    output: &Path,
    file: &Path,
) -> TestResult {
    let started = std::time::Instant::now();
    match run_file_inner(h5i, target, target_text, output, file) {
        Ok(mut result) => {
            result.duration_ms = started.elapsed().as_millis();
            result
        }
        Err(error) => TestResult {
            id: file
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("invalid")
                .to_string(),
            name: file.display().to_string(),
            file: file.display().to_string(),
            status: Status::Error,
            duration_ms: started.elapsed().as_millis(),
            steps: Vec::new(),
            oracle: OracleResult::default(),
            cleanup_errors: Vec::new(),
            error: Some(format!("{error:#}")),
            covered_operations: BTreeSet::new(),
            mutations: BTreeSet::new(),
        },
    }
}

fn run_file_inner(
    h5i: &OsString,
    target: &Url,
    target_text: &str,
    output_root: &Path,
    file: &Path,
) -> anyhow::Result<TestResult> {
    let text = fs::read_to_string(file)
        .map_err(|e| anyhow::anyhow!("{} could not be read: {e}", file.display()))?;
    let plan: TestFile = serde_yaml::from_str(&text)
        .map_err(|e| anyhow::anyhow!("{} is not an h5i test: {e}", file.display()))?;
    validate(&plan, file)?;
    let test_output = output_root.join(&plan.id);
    create_private_dir(&test_output)?;
    create_private_dir(&test_output.join("responses"))?;

    let mut ctx = TestContext {
        h5i,
        target,
        target_text,
        output: &test_output,
        declared_actors: &plan.actors,
        requests: &plan.requests,
        sessions: BTreeMap::new(),
        variables: BTreeMap::new(),
        responses: BTreeMap::new(),
        covered: BTreeSet::new(),
        mutations: BTreeSet::new(),
    };
    let mut steps = Vec::new();
    let mut flow_error = None;
    for (index, step) in plan.flow.iter().enumerate() {
        match ctx.send(step, index, false) {
            Ok(result) => steps.push(result),
            Err(error) => {
                flow_error = Some(format!("{error:#}"));
                break;
            }
        }
    }

    let mut oracle = OracleResult::default();
    let mut status = Status::Error;
    let mut oracle_conclusive = false;
    if flow_error.is_none() {
        let bundle = OracleBundle {
            schema: SCHEMA,
            test: plan.id.clone(),
            target: target_text.to_string(),
            responses: std::mem::take(&mut ctx.responses),
            variables: ctx.variables.clone(),
        };
        let oracle_path = test_output.join("oracle-input.json");
        let prepared = serde_json::to_vec_pretty(&bundle)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| fs::write(&oracle_path, bytes).map_err(anyhow::Error::from));
        match prepared.and_then(|()| run_oracle(&plan.oracle, file, &oracle_path, &test_output)) {
            Ok(result) => {
                oracle = result;
                status = match oracle.exit_code {
                    Some(0) => {
                        oracle_conclusive = true;
                        Status::Pass
                    }
                    Some(1) => {
                        oracle_conclusive = true;
                        Status::Fail
                    }
                    _ => Status::Error,
                };
            }
            Err(error) => {
                oracle.stderr = format!("{error:#}");
            }
        }
    }

    let mut cleanup_errors = Vec::new();
    for (index, step) in plan.cleanup.iter().enumerate() {
        if let Err(error) = ctx.send(step, index, true) {
            cleanup_errors.push(format!("{error:#}"));
        }
    }
    ctx.stop_sessions();
    if status == Status::Pass && !cleanup_errors.is_empty() {
        status = Status::Error;
    }
    // Coverage is conclusive for both outcomes an oracle actually decided.
    // An execution/oracle error is not evidence that the operation was tested.
    let (covered_operations, mutations) = if oracle_conclusive {
        (
            std::mem::take(&mut ctx.covered),
            std::mem::take(&mut ctx.mutations),
        )
    } else {
        (BTreeSet::new(), BTreeSet::new())
    };

    Ok(TestResult {
        id: plan.id.clone(),
        name: plan.name.unwrap_or_else(|| plan.id.clone()),
        file: file.display().to_string(),
        status,
        duration_ms: 0,
        steps,
        oracle,
        cleanup_errors,
        error: flow_error,
        covered_operations,
        mutations,
    })
}

impl TestContext<'_> {
    fn send(&mut self, step: &SendStep, index: usize, cleanup: bool) -> anyhow::Result<StepResult> {
        let actor = step.actor.as_deref().unwrap_or("default");
        if actor != "default" && !self.declared_actors.contains_key(actor) {
            anyhow::bail!("step {} uses undeclared actor `{actor}`", index + 1);
        }
        self.ensure_session(actor)?;
        let template = self.requests.get(&step.send).ok_or_else(|| {
            anyhow::anyhow!("step {} names no request `{}`", index + 1, step.send)
        })?;
        let method = substitute(&template.method, &self.variables)?;
        let path = substitute(&template.path, &self.variables)?;
        let url = self.target.join(&path).map_err(|e| {
            anyhow::anyhow!("request `{}` path `{path}` is not a URL: {e}", step.send)
        })?;
        let mut headers = Vec::new();
        for (name, value) in &template.headers {
            headers.push((name.clone(), substitute(value, &self.variables)?));
        }
        let body = match (&template.body, &template.json) {
            (Some(_), Some(_)) => anyhow::bail!("request `{}` gives both body and json", step.send),
            (Some(body), None) => substitute(body, &self.variables)?.into_bytes(),
            (None, Some(value)) => {
                if !headers
                    .iter()
                    .any(|(n, _)| n.eq_ignore_ascii_case("content-type"))
                {
                    headers.push(("content-type".to_string(), "application/json".to_string()));
                }
                serde_json::to_vec(&substitute_json(value, &self.variables)?)?
            }
            (None, None) => Vec::new(),
        };
        let sets = step
            .set
            .iter()
            .map(|s| substitute(s, &self.variables))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let unsets = step
            .unset
            .iter()
            .map(|s| substitute(s, &self.variables))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let request = json!({
            "id": index,
            "verb": "resend",
            "request": {"method": method, "url": url.as_str(), "headers": headers, "body": body},
            "set": sets,
            "unset": unsets,
            "create": step.create,
            "no_follow": step.no_follow,
        });
        let session_name = self
            .sessions
            .get(actor)
            .expect("session was made")
            .name
            .clone();
        let answer = rpc(self.h5i, &session_name, &request)?;
        if answer.get("ok").and_then(Value::as_bool) == Some(false) || answer.get("error").is_some()
        {
            anyhow::bail!("step {} `{}`: {}", index + 1, step.send, answer);
        }
        let seq = answer.get("seq").and_then(Value::as_u64).ok_or_else(|| {
            anyhow::anyhow!(
                "step {} `{}` returned no message sequence",
                index + 1,
                step.send
            )
        })?;
        let session_id = self
            .sessions
            .get(actor)
            .expect("session was made")
            .id
            .clone();
        let store = bs::dir(&bs::root()?, &session_id).join(bs::MESSAGES_DIR);
        let stored_request: StoredRequest = read_json(&message_file(&store, seq, "request"))?;
        let response: StoredResponse = read_json(&message_file(&store, seq, "response"))?;
        let save = match (&step.save, cleanup) {
            (Some(name), true) => format!("cleanup_{name}"),
            (None, true) => format!("cleanup_{}", index + 1),
            (Some(name), false) => name.clone(),
            (None, false) => format!("step_{}", index + 1),
        };
        if self.responses.contains_key(&save) {
            anyhow::bail!("response name `{save}` is used more than once");
        }
        let response_dir = self.output.join("responses");
        let body_path = response_dir.join(format!("{save}.body"));
        copy_body(&store, &response.body, &body_path)?;
        let request_body_path = response_dir.join(format!("{save}.request.body"));
        copy_body(&store, &stored_request.body, &request_body_path)?;
        let request_path = response_dir.join(format!("{save}.request.json"));
        fs::write(
            &request_path,
            serde_json::to_vec_pretty(&public_request(
                &stored_request,
                &relative_to(self.output, &request_body_path),
            ))?,
        )?;
        let artifact = ResponseArtifact {
            status: response.status,
            headers: response.headers.clone(),
            body: relative_to(self.output, &body_path),
            request: relative_to(self.output, &request_path),
        };
        for (name, spec) in &step.extract {
            let value = extract(spec, &response, &body_path)?;
            self.variables.insert(name.clone(), value);
        }
        if !cleanup {
            if let Some(covers) = &step.covers {
                self.covered.insert(covers.operation.clone());
                if let Some(mutation) = &covers.mutation {
                    self.mutations.insert(mutation.clone());
                }
            }
            self.responses.insert(save.clone(), artifact);
        }
        Ok(StepResult {
            name: step.name.clone().unwrap_or_else(|| step.send.clone()),
            actor: actor.to_string(),
            request: step.send.clone(),
            ok: true,
            response: (!cleanup).then_some(save),
            status: response.status,
            error: None,
            covers: step.covers.as_ref().map(|c| CoversResult {
                operation: c.operation.clone(),
                mutation: c.mutation.clone(),
            }),
        })
    }

    fn ensure_session(&mut self, actor: &str) -> anyhow::Result<()> {
        if self.sessions.contains_key(actor) {
            return Ok(());
        }
        let nonce = SESSION_NONCE.fetch_add(1, Ordering::Relaxed);
        let name = format!("h5i-test-{}-{nonce}", std::process::id());
        let output = Command::new(self.h5i)
            .args([
                "browser",
                "open",
                self.target_text,
                "--session",
                &name,
                "--new",
                "--capture",
                "--no-sandbox",
                "--json",
            ])
            .output()
            .map_err(|e| anyhow::anyhow!("could not start actor `{actor}`: {e}"))?;
        if !output.status.success() {
            anyhow::bail!(
                "could not start actor `{actor}`: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let record: Value = serde_json::from_slice(&output.stdout)
            .map_err(|e| anyhow::anyhow!("actor `{actor}` session returned invalid JSON: {e}"))?;
        let id = record
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("actor `{actor}` session returned no id"))?;
        self.sessions.insert(
            actor.to_string(),
            Session {
                logical: actor.to_string(),
                name,
                id: id.to_string(),
            },
        );
        Ok(())
    }

    fn stop_sessions(&mut self) {
        for (_, session) in std::mem::take(&mut self.sessions) {
            let _ = Command::new(self.h5i)
                .args([
                    "browser",
                    "close",
                    "--session",
                    &session.name,
                    "--capture-drop",
                    "--json",
                ])
                .output();
            let _ = Command::new(self.h5i)
                .args(["browser", "rm", &session.name, "--force", "--json"])
                .output();
            let _ = &session.logical;
        }
    }
}

impl Drop for TestContext<'_> {
    fn drop(&mut self) {
        self.stop_sessions();
    }
}

fn rpc(h5i: &OsString, session: &str, request: &Value) -> anyhow::Result<Value> {
    let mut child = Command::new(h5i)
        .args(["browser", "rpc", "--stdio", "--session", session])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("RPC stdin was not opened"))?;
        serde_json::to_writer(&mut *stdin, request)?;
        stdin.write_all(b"\n")?;
    }
    drop(child.stdin.take());
    let output = child.wait_with_output()?;
    if !output.status.success() {
        anyhow::bail!(
            "browser RPC failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let line = String::from_utf8(output.stdout)?;
    serde_json::from_str(line.trim()).map_err(Into::into)
}

fn run_oracle(
    oracle: &Oracle,
    test_file: &Path,
    result: &Path,
    artifacts: &Path,
) -> anyhow::Result<OracleResult> {
    if oracle.command.is_empty() {
        anyhow::bail!("oracle.command cannot be empty");
    }
    if oracle.timeout_seconds == 0 {
        anyhow::bail!("oracle.timeout_seconds must be greater than zero");
    }
    let base = test_file.parent().unwrap_or_else(|| Path::new("."));
    let program = if oracle.command[0].contains(std::path::MAIN_SEPARATOR) {
        base.join(&oracle.command[0]).into_os_string()
    } else {
        OsString::from(&oracle.command[0])
    };
    let stdout_path = artifacts.join("oracle.stdout");
    let stderr_path = artifacts.join("oracle.stderr");
    let stdout_file = fs::File::create(&stdout_path)?;
    let stderr_file = fs::File::create(&stderr_path)?;
    let mut command = Command::new(program);
    command.args(&oracle.command[1..]);
    command.current_dir(base);
    command.env("H5I_TEST_RESULT", absolute(result)?);
    command.env("H5I_TEST_ARTIFACTS", absolute(artifacts)?);
    command.env("H5I_TEST_INPUTS", oracle.inputs.join(","));
    command.stdout(Stdio::from(stdout_file));
    command.stderr(Stdio::from(stderr_file));
    let mut child = command
        .spawn()
        .map_err(|e| anyhow::anyhow!("oracle `{}` could not run: {e}", oracle.command.join(" ")))?;
    let status = match child.wait_timeout(Duration::from_secs(oracle.timeout_seconds))? {
        Some(status) => Some(status),
        None => {
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    };
    let mut stderr = bounded(&fs::read(&stderr_path)?);
    if status.is_none() {
        if !stderr.is_empty() {
            stderr.push('\n');
        }
        stderr.push_str(&format!(
            "oracle timed out after {} seconds",
            oracle.timeout_seconds
        ));
    }
    Ok(OracleResult {
        ran: true,
        exit_code: status.and_then(|s| s.code()),
        stdout: bounded(&fs::read(&stdout_path)?),
        stderr,
    })
}

fn validate(plan: &TestFile, file: &Path) -> anyhow::Result<()> {
    if plan.version != SCHEMA {
        anyhow::bail!(
            "{} uses version `{}`; this build accepts `{SCHEMA}`",
            file.display(),
            plan.version
        );
    }
    if !safe_component(&plan.id) {
        anyhow::bail!(
            "{} id must use only letters, digits, '.', '_' and '-'",
            file.display()
        );
    }
    if plan.flow.is_empty() {
        anyhow::bail!("{} has no flow steps", file.display());
    }
    if plan.oracle.command.is_empty() {
        anyhow::bail!("{} has an empty oracle command", file.display());
    }
    let mut saved = BTreeSet::new();
    for (index, step) in plan.flow.iter().enumerate() {
        if !plan.requests.contains_key(&step.send) {
            anyhow::bail!(
                "{} step {} names no request `{}`",
                file.display(),
                index + 1,
                step.send
            );
        }
        let save = step
            .save
            .clone()
            .unwrap_or_else(|| format!("step_{}", index + 1));
        if !safe_component(&save) {
            anyhow::bail!(
                "{} step {} response name must use only letters, digits, '.', '_' and '-'",
                file.display(),
                index + 1
            );
        }
        if !saved.insert(save.clone()) {
            anyhow::bail!(
                "{} uses response name `{save}` more than once",
                file.display()
            );
        }
    }
    for input in &plan.oracle.inputs {
        if !saved.contains(input) {
            anyhow::bail!(
                "{} oracle input `{input}` is not a saved flow response",
                file.display()
            );
        }
    }
    for (index, step) in plan.cleanup.iter().enumerate() {
        if !plan.requests.contains_key(&step.send) {
            anyhow::bail!(
                "{} cleanup step {} names no request `{}`",
                file.display(),
                index + 1,
                step.send
            );
        }
        if let Some(save) = &step.save
            && !safe_component(save)
        {
            anyhow::bail!(
                "{} cleanup step {} response name must use only letters, digits, '.', '_' and '-'",
                file.display(),
                index + 1
            );
        }
    }
    Ok(())
}

fn discover(path: &Path) -> anyhow::Result<Vec<PathBuf>> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    if !path.is_dir() {
        anyhow::bail!("{} is not a test file or directory", path.display());
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(path)? {
        let path = entry?.path();
        if path.is_file()
            && matches!(
                path.extension().and_then(|s| s.to_str()),
                Some("yaml" | "yml" | "json")
            )
        {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn refuse_duplicate_ids(files: &[PathBuf]) -> anyhow::Result<()> {
    let mut seen: BTreeMap<String, &Path> = BTreeMap::new();
    for file in files {
        let Ok(text) = fs::read_to_string(file) else {
            continue;
        };
        let Ok(value) = serde_yaml::from_str::<Value>(&text) else {
            continue;
        };
        let Some(id) = value.get("id").and_then(Value::as_str) else {
            continue;
        };
        if let Some(first) = seen.insert(id.to_string(), file) {
            anyhow::bail!(
                "test id `{id}` is used by both {} and {}; artifact ownership would be ambiguous",
                first.display(),
                file.display()
            );
        }
    }
    Ok(())
}

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && value != "."
        && value != ".."
}

fn substitute(text: &str, variables: &BTreeMap<String, String>) -> anyhow::Result<String> {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after
            .find('}')
            .ok_or_else(|| anyhow::anyhow!("`{text}` opens `${{` and never closes it"))?;
        let key = &after[..end];
        let value = if let Some(env) = key.strip_prefix("env.") {
            std::env::var(env)
                .map_err(|_| anyhow::anyhow!("`{text}` needs environment variable `{env}`"))?
        } else {
            variables
                .get(key)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("`{text}` uses unbound variable `{key}`"))?
        };
        out.push_str(&value);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn substitute_json(value: &Value, variables: &BTreeMap<String, String>) -> anyhow::Result<Value> {
    match value {
        Value::String(text) => Ok(Value::String(substitute(text, variables)?)),
        Value::Array(values) => Ok(Value::Array(
            values
                .iter()
                .map(|v| substitute_json(v, variables))
                .collect::<anyhow::Result<_>>()?,
        )),
        Value::Object(values) => Ok(Value::Object(
            values
                .iter()
                .map(|(k, v)| Ok((k.clone(), substitute_json(v, variables)?)))
                .collect::<anyhow::Result<_>>()?,
        )),
        other => Ok(other.clone()),
    }
}

fn extract(spec: &str, response: &StoredResponse, body_path: &Path) -> anyhow::Result<String> {
    let (kind, argument) = spec.split_once(':').unwrap_or((spec, ""));
    let body = fs::read(body_path)?;
    let text = String::from_utf8_lossy(&body);
    let found = match kind {
        "regex" => regex::Regex::new(argument)?
            .captures(&text)
            .and_then(|c| c.get(1).or_else(|| c.get(0)))
            .map(|m| m.as_str().to_string()),
        "json" => serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|v| json_at(&v, argument).cloned())
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| v.to_string())
            }),
        "header" => response
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(argument))
            .map(|(_, value)| value.clone()),
        "status" if argument.is_empty() => response.status.map(|v| v.to_string()),
        _ => anyhow::bail!("unknown extractor `{spec}`; use regex:, json:, header: or status"),
    };
    found.ok_or_else(|| anyhow::anyhow!("extractor `{spec}` found nothing"))
}

fn json_at<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut at = value;
    for part in path
        .trim_start_matches("$.")
        .split('.')
        .filter(|p| !p.is_empty())
    {
        at = match at {
            Value::Object(map) => map.get(part)?,
            Value::Array(items) => items.get(part.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(at)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> anyhow::Result<T> {
    serde_json::from_slice(&fs::read(path)?).map_err(Into::into)
}

fn copy_body(store: &Path, body: &Body, output: &Path) -> anyhow::Result<()> {
    match body {
        Body::Empty => fs::write(output, [])?,
        Body::Stored {
            sha256,
            truncated: false,
            ..
        } => {
            let source = body_file(store, sha256)
                .ok_or_else(|| anyhow::anyhow!("stored response names an invalid body hash"))?;
            fs::copy(source, output)?;
        }
        Body::Stored {
            truncated: true, ..
        } => anyhow::bail!(
            "response body was truncated; an oracle cannot decide from partial evidence"
        ),
        Body::Skipped { reason, .. } => {
            anyhow::bail!("response body was not captured ({reason:?})")
        }
    }
    Ok(())
}

fn public_request(request: &StoredRequest, body: &str) -> Value {
    json!({
        "method": request.method,
        "url": request.url,
        "headers": request.headers.iter().map(|(name, value)| {
            let secret = matches!(name.to_ascii_lowercase().as_str(), "cookie" | "authorization" | "proxy-authorization");
            (name, if secret { "[redacted]" } else { value })
        }).collect::<Vec<_>>(),
        "body": body,
    })
}

fn coverage(path: &Path, tests: &[TestResult], minimum: Option<f64>) -> anyhow::Result<Coverage> {
    let text = fs::read_to_string(path)?;
    let document: Value = serde_yaml::from_str(&text)
        .map_err(|e| anyhow::anyhow!("{} is not OpenAPI JSON or YAML: {e}", path.display()))?;
    let paths = document
        .get("paths")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("{} has no OpenAPI paths object", path.display()))?;
    let mut all = BTreeSet::new();
    for (path_name, item) in paths {
        let Some(methods) = item.as_object() else {
            continue;
        };
        for (method, operation) in methods {
            let upper = method.to_ascii_uppercase();
            if !matches!(
                upper.as_str(),
                "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS" | "TRACE"
            ) {
                continue;
            }
            let id = operation
                .get("operationId")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("{upper} {path_name}"));
            if !all.insert(id.clone()) {
                anyhow::bail!(
                    "{} contains duplicate operation identity `{id}`",
                    path.display()
                );
            }
        }
    }
    let declared: BTreeSet<String> = tests
        .iter()
        .flat_map(|t| t.covered_operations.iter().cloned())
        .collect();
    let unknown: Vec<_> = declared.difference(&all).cloned().collect();
    if !unknown.is_empty() {
        anyhow::bail!(
            "tests claim OpenAPI operations that do not exist: {}",
            unknown.join(", ")
        );
    }
    let covered: Vec<_> = declared.intersection(&all).cloned().collect();
    let untested: Vec<_> = all.difference(&declared).cloned().collect();
    let percent = if all.is_empty() {
        100.0
    } else {
        covered.len() as f64 * 100.0 / all.len() as f64
    };
    let mutations = tests
        .iter()
        .flat_map(|t| t.mutations.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok(Coverage {
        total_operations: all.len(),
        covered_operations: covered.len(),
        percent,
        operations: covered,
        untested,
        mutations,
        minimum,
        gate_passed: minimum.is_none_or(|m| percent + f64::EPSILON >= m),
    })
}

fn junit(result: &RunResult) -> String {
    let coverage_failure = usize::from(result.coverage.as_ref().is_some_and(|c| !c.gate_passed));
    let mut out = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuite name=\"h5i security tests\" tests=\"{}\" failures=\"{}\" errors=\"{}\">\n",
        result.tests.len() + coverage_failure,
        result.failed + coverage_failure,
        result.errors
    );
    for test in &result.tests {
        out.push_str(&format!(
            "  <testcase classname=\"h5i\" name=\"{}\" time=\"{:.3}\">\n",
            xml(&test.name),
            test.duration_ms as f64 / 1000.0
        ));
        let message = test
            .error
            .as_deref()
            .or_else(|| (!test.oracle.stderr.is_empty()).then_some(test.oracle.stderr.as_str()))
            .unwrap_or("");
        match test.status {
            Status::Fail => out.push_str(&format!(
                "    <failure message=\"security property did not hold\">{}</failure>\n",
                xml(message)
            )),
            Status::Error => out.push_str(&format!(
                "    <error message=\"test could not decide\">{}</error>\n",
                xml(message)
            )),
            Status::Pass => {}
        }
        out.push_str("  </testcase>\n");
    }
    if coverage_failure == 1 {
        out.push_str("  <testcase classname=\"h5i\" name=\"minimum coverage\"><failure message=\"coverage below requested minimum\"/></testcase>\n");
    }
    out.push_str("</testsuite>\n");
    out
}

fn print_human(result: &RunResult, path: &Path) {
    for test in &result.tests {
        let mark = match test.status {
            Status::Pass => "✔",
            Status::Fail => "✘",
            Status::Error => "!",
        };
        println!(
            "  {mark} {} ({:?}, {} ms)",
            test.name, test.status, test.duration_ms
        );
        if let Some(error) = &test.error {
            println!("    {error}");
        }
        if !test.oracle.stderr.is_empty() {
            println!("    {}", test.oracle.stderr.trim());
        }
    }
    if let Some(coverage) = &result.coverage {
        println!(
            "\n  checked operations: {} / {} ({:.1}%)",
            coverage.covered_operations, coverage.total_operations, coverage.percent
        );
        if let Some(minimum) = coverage.minimum {
            println!(
                "  minimum requested  : {minimum:.1}% ({})",
                if coverage.gate_passed {
                    "met"
                } else {
                    "not met"
                }
            );
        }
    }
    println!("\n  results: {}", path.display());
}

fn create_private_dir(path: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn relative_to(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

fn absolute(path: &Path) -> anyhow::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn bounded(bytes: &[u8]) -> String {
    let head = &bytes[..bytes.len().min(MAX_ORACLE_OUTPUT)];
    let mut text = String::from_utf8_lossy(head).into_owned();
    if bytes.len() > head.len() {
        text.push_str(&format!(
            "\n[{} more bytes omitted]",
            bytes.len() - head.len()
        ));
    }
    text
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variables_include_environment_and_extracted_values() {
        let mut vars = BTreeMap::new();
        vars.insert("csrf".to_string(), "token".to_string());
        // PATH is present on every supported build environment and its value is
        // irrelevant; this checks the namespace without mutating global env.
        assert!(substitute("${env.PATH}", &vars).unwrap().len() > 1);
        assert_eq!(substitute("x=${csrf}", &vars).unwrap(), "x=token");
        assert!(substitute("${missing}", &vars).is_err());
    }

    #[test]
    fn openapi_coverage_is_optional_until_a_minimum_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let spec = dir.path().join("openapi.yaml");
        fs::write(&spec, "openapi: 3.0.0\npaths:\n  /users/{id}:\n    get:\n      operationId: getUser\n    delete: {}\n").unwrap();
        let test = TestResult {
            id: "t".into(),
            name: "t".into(),
            file: "t.yaml".into(),
            status: Status::Pass,
            duration_ms: 1,
            steps: vec![],
            oracle: OracleResult::default(),
            cleanup_errors: vec![],
            error: None,
            covered_operations: BTreeSet::from(["getUser".into()]),
            mutations: BTreeSet::from(["identifier-substitution".into()]),
        };
        let report = coverage(&spec, &[test], None).unwrap();
        assert_eq!(report.total_operations, 2);
        assert_eq!(report.covered_operations, 1);
        assert!(report.gate_passed);
        let gated = coverage(&spec, &[], Some(1.0)).unwrap();
        assert!(!gated.gate_passed);
    }

    #[test]
    fn json_paths_read_objects_and_arrays() {
        let value = json!({"users": [{"id": 7}]});
        assert_eq!(json_at(&value, "$.users.0.id"), Some(&json!(7)));
    }

    #[test]
    fn junit_keeps_oracle_output_inert() {
        assert_eq!(
            xml("<failure x='y'>&"),
            "&lt;failure x=&apos;y&apos;&gt;&amp;"
        );
    }

    #[test]
    fn artifact_names_are_one_safe_path_component() {
        for safe in ["idor", "invoice-1", "a_b.json"] {
            assert!(safe_component(safe), "{safe}");
        }
        for unsafe_name in ["", ".", "..", "../escape", "a/b"] {
            assert!(!safe_component(unsafe_name), "{unsafe_name}");
        }
    }

    #[test]
    fn duplicate_test_ids_are_refused_before_artifacts_are_written() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.yaml");
        let b = dir.path().join("b.yaml");
        fs::write(&a, "id: same\n").unwrap();
        fs::write(&b, "id: same\n").unwrap();
        let error = refuse_duplicate_ids(&[a, b]).unwrap_err();
        assert!(error.to_string().contains("used by both"), "{error}");
    }
}
