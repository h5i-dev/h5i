//! The stdio JSON-RPC bridge — how out-of-process SDKs drive a score.
//!
//! `h5i orchestra serve` speaks line-delimited JSON-RPC 2.0 on stdin/stdout so
//! a host-language SDK (Python first: the `h5i.orchestra` package) can drive a
//! resident [`Conductor`] eagerly: the child process holds everything that
//! must live in one process — the Conductor, the journal's per-label sequence
//! counters and in-flight fail-closed checks, the turn-wait pollers — while
//! the SDK holds the control flow (`if`/`for`/`gather` in the host language,
//! exactly the define-by-run bargain the eDSL makes in Rust).
//!
//! This is deliberately **not** a daemon (design doc §7): no socket, no port,
//! no auth surface, one session per process, exit on stdin EOF. The score
//! stays host-side user code at the same trust level as the shell scripts it
//! replaces; the bridge's only I/O is its own stdio. stdout is protocol-only —
//! logs go to stderr (`H5I_LOG`).
//!
//! Wire format: one JSON object per line. Requests carry `id`/`method`/
//! `params`; responses carry `result` or `error{code,message,data.kind}`.
//! Requests multiplex — long agent turns run concurrently and respond out of
//! order, so `asyncio.gather(claude.work(…), codex.work(…))` maps to two
//! in-flight requests. The one server→client request is `launcher.on_turn`
//! (only with `launcher: "client"`), which the client must answer.
//!
//! Two operations need a client round-trip *inside* a journaled step and get
//! a begin/commit pair instead of one call:
//! - `conductor.step_begin` / `step_commit` / `step_abort` — the
//!   [`Conductor::step`] escape hatch with the closure on the client side.
//! - `conductor.judge_begin` / `judge_commit` / `judge_abort` — a client-side
//!   [`VerdictPolicy`]: begin snapshots the folded run, the client decides,
//!   commit records the verdict and journals it in one step (mirroring
//!   [`Conductor::judge`]).
//!
//! A crash between the client's side effect and `commit` re-runs the step on
//! resume — the same window the in-process eDSL has between a closure's
//! side effect and its journal append; external effects should carry
//! idempotency keys either way.

use super::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, Notify};

/// Protocol version spoken by this binary. `initialize` refuses a mismatched
/// client so the SDK and the binary can evolve independently; capabilities let
/// the SDK feature-detect within a version.
pub const PROTOCOL_VERSION: u64 = 1;

const CAPABILITIES: &[&str] = &[
    "conductor.core", // launch/status/events/roster/compare/note/freeze/verify/apply/patched/trace
    "conductor.step", // step_begin/step_commit/step_abort — remote journaled closures
    "conductor.judge", // built-in policies + judge_begin/judge_commit — remote policies
    "agent.turns",    // hire/work/ask/review/revise
    "gate",           // durable human gates
    "preflight",
    "launcher.client", // server→client launcher.on_turn callbacks
];

// ── Entry points ─────────────────────────────────────────────────────────────

/// Serve one bridge session on this process's stdin/stdout. Returns when the
/// client disconnects (EOF) or sends `shutdown`.
pub async fn serve_stdio(h5i_version: &str) -> Result<(), H5iError> {
    serve(
        tokio::io::BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
        h5i_version,
    )
    .await
}

/// Serve one bridge session over arbitrary streams (tests use in-memory
/// duplex pipes).
pub async fn serve<R, W>(reader: R, writer: W, h5i_version: &str) -> Result<(), H5iError>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<OutMsg>();
    let writer_task = tokio::spawn(async move {
        let mut writer = writer;
        while let Some(msg) = out_rx.recv().await {
            match msg {
                OutMsg::Line(line) => {
                    if writer.write_all(line.as_bytes()).await.is_err()
                        || writer.write_all(b"\n").await.is_err()
                        || writer.flush().await.is_err()
                    {
                        break;
                    }
                }
                OutMsg::Flush(ack) => {
                    let _ = writer.flush().await;
                    let _ = ack.send(());
                }
            }
        }
    });

    let server = Arc::new(Server {
        h5i_version: h5i_version.to_string(),
        out: out_tx.clone(),
        broker: Arc::new(TurnBroker {
            out: out_tx.clone(),
            seq: AtomicU64::new(0),
            waiters: Mutex::new(BTreeMap::new()),
        }),
        initialized: AtomicBool::new(false),
        conductor: Mutex::new(None),
        steps: Mutex::new(BTreeMap::new()),
        shutdown: Notify::new(),
    });

    let mut lines = reader.lines();
    loop {
        tokio::select! {
            _ = server.shutdown.notified() => break,
            line = lines.next_line() => match line {
                Ok(Some(line)) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let server = server.clone();
                    tokio::spawn(async move { server.dispatch_line(line).await });
                }
                Ok(None) | Err(_) => break,
            }
        }
    }

    // The client is gone (or asked to stop): fail any launcher turns still
    // waiting on it, flush what was already enqueued, then stop writing.
    server.broker.fail_all();
    let (ack_tx, ack_rx) = oneshot::channel();
    let _ = out_tx.send(OutMsg::Flush(ack_tx));
    let _ = tokio::time::timeout(Duration::from_secs(2), ack_rx).await;
    writer_task.abort();
    Ok(())
}

// ── Plumbing ─────────────────────────────────────────────────────────────────

enum OutMsg {
    Line(String),
    Flush(oneshot::Sender<()>),
}

/// Server→client `launcher.on_turn` requests and their pending answers.
struct TurnBroker {
    out: mpsc::UnboundedSender<OutMsg>,
    seq: AtomicU64,
    waiters: Mutex<BTreeMap<String, oneshot::Sender<Result<(), String>>>>,
}

impl TurnBroker {
    fn fail_all(&self) {
        let waiters = std::mem::take(&mut *self.waiters.lock().expect("broker lock"));
        for (_, tx) in waiters {
            let _ = tx.send(Err("bridge connection closed".into()));
        }
    }
}

/// A [`RuntimeLauncher`] that forwards every turn to the client as a
/// `launcher.on_turn` request and blocks (on the blocking pool, where
/// `on_turn` always runs) until the client answers. This is how a Python
/// score plays or spawns its own agents — and how SDK test suites script
/// deterministic agents, mirroring [`FnLauncher`].
struct ClientLauncher {
    broker: Arc<TurnBroker>,
}

impl RuntimeLauncher for ClientLauncher {
    fn on_turn(&self, turn: &TurnContext) -> Result<(), H5iError> {
        let id = format!("srv:{}", self.broker.seq.fetch_add(1, Ordering::SeqCst) + 1);
        let (tx, rx) = oneshot::channel();
        self.broker
            .waiters
            .lock()
            .expect("broker lock")
            .insert(id.clone(), tx);
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "launcher.on_turn",
            "params": turn_to_json(turn),
        });
        self.broker
            .out
            .send(OutMsg::Line(request.to_string()))
            .map_err(|_| H5iError::Internal("orchestra bridge: client connection closed".into()))?;
        match rx.blocking_recv() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(H5iError::Metadata(format!(
                "orchestra bridge: client launcher failed: {e}"
            ))),
            Err(_) => Err(H5iError::Internal(
                "orchestra bridge: client disconnected during a launcher turn".into(),
            )),
        }
    }
}

fn turn_to_json(turn: &TurnContext) -> Value {
    let kind = turn.kind.label();
    let target = match &turn.kind {
        TurnKind::Review { target } => Some(target.clone()),
        _ => None,
    };
    json!({
        "run_id": turn.run_id,
        "agent_id": turn.agent_id,
        "env_id": turn.env_id,
        "kind": kind,
        "target": target,
        "instruction": turn.instruction,
        "repo_workdir": turn.repo_workdir.to_string_lossy(),
        "h5i_root": turn.h5i_root.to_string_lossy(),
        "work_dir": turn.work_dir.as_ref().map(|p| p.to_string_lossy().into_owned()),
        "runtime": turn.runtime,
        "model": turn.model,
    })
}

/// A begin/commit step whose closure runs on the client.
struct PendingStep {
    label: String,
    judge: bool,
    started: Instant,
}

struct Server {
    h5i_version: String,
    out: mpsc::UnboundedSender<OutMsg>,
    broker: Arc<TurnBroker>,
    initialized: AtomicBool,
    conductor: Mutex<Option<Conductor>>,
    steps: Mutex<BTreeMap<String, PendingStep>>,
    shutdown: Notify,
}

/// A refused request: JSON-RPC error code + message (+ the H5iError variant
/// as `data.kind`, so clients can map to typed exceptions).
struct Rej {
    code: i64,
    message: String,
    kind: Option<&'static str>,
}

impl Rej {
    fn invalid_params(message: impl Into<String>) -> Self {
        Rej { code: -32602, message: message.into(), kind: None }
    }
    fn state(message: impl Into<String>) -> Self {
        Rej { code: -32002, message: message.into(), kind: None }
    }
    fn to_json(&self) -> Value {
        json!({
            "code": self.code,
            "message": self.message,
            "data": { "kind": self.kind },
        })
    }
}

impl From<H5iError> for Rej {
    fn from(e: H5iError) -> Self {
        let kind = match &e {
            H5iError::Git(_) => "git",
            H5iError::Metadata(_) => "metadata",
            H5iError::Io(_) | H5iError::IoWithContext { .. } => "io",
            H5iError::Serialization(_) => "serialization",
            H5iError::Internal(_) => "internal",
            _ => "other",
        };
        Rej { code: -32000, message: e.to_string(), kind: Some(kind) }
    }
}

fn parse<T: DeserializeOwned>(params: Value) -> Result<T, Rej> {
    serde_json::from_value(params).map_err(|e| Rej::invalid_params(format!("invalid params: {e}")))
}

async fn blocking<T, F>(f: F) -> Result<T, Rej>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, H5iError> + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(r) => r.map_err(Rej::from),
        Err(e) => Err(Rej {
            code: -32603,
            message: format!("orchestra bridge task panicked: {e}"),
            kind: Some("internal"),
        }),
    }
}

fn ok<T: Serialize>(value: T) -> Result<Value, Rej> {
    serde_json::to_value(value).map_err(|e| Rej::from(H5iError::from(e)))
}

// ── Request params ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct InitializeParams {
    protocol_version: u64,
    #[serde(default)]
    #[allow(dead_code)]
    client: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    client_version: Option<String>,
}

#[derive(Deserialize)]
struct LaunchParams {
    #[serde(default)]
    repo: Option<String>,
    run: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    max_rounds: Option<u32>,
    #[serde(default)]
    actor: Option<String>,
    /// `attach` (default) | `resident` | `client`.
    #[serde(default)]
    launcher: Option<String>,
    #[serde(default)]
    poll_interval_ms: Option<u64>,
    #[serde(default)]
    turn_timeout_ms: Option<u64>,
    /// Digest of the *client-side score* (e.g. the Python file), recorded as
    /// run provenance. Hashing this server binary would be meaningless, so no
    /// digest is recorded when omitted.
    #[serde(default)]
    score_digest: Option<String>,
}

#[derive(Deserialize)]
struct HireParams {
    name: String,
    #[serde(default)]
    runtime: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    isolation: Option<String>,
    #[serde(default)]
    env: Option<String>,
}

#[derive(Deserialize)]
struct WorkParams {
    agent: String,
    env_id: String,
    task: String,
    #[serde(default)]
    materials: Vec<TeamArtifact>,
    #[serde(default)]
    expect_independent: bool,
}

#[derive(Deserialize)]
struct AskParams {
    agent: String,
    env_id: String,
    prompt: String,
}

#[derive(Deserialize)]
struct ReviewParams {
    reviewer: String,
    env_id: String,
    artifact: TeamArtifact,
}

#[derive(Deserialize)]
struct ReviseParams {
    agent: String,
    env_id: String,
    artifact: TeamArtifact,
    review: TeamReview,
}

#[derive(Deserialize)]
struct VerifyParams {
    artifact: TeamArtifact,
    command: Vec<String>,
    #[serde(default)]
    isolation: Option<String>,
    /// Sealed-tests mode: submission id or team agent id whose base..commit
    /// diff is overlaid over the candidate before the command runs.
    #[serde(default)]
    tests_from: Option<String>,
}

#[derive(Deserialize)]
struct JudgeParams {
    policy: String,
}

#[derive(Deserialize)]
struct TokenParams {
    token: String,
}

#[derive(Deserialize)]
struct StepBeginParams {
    label: String,
}

#[derive(Deserialize)]
struct StepCommitParams {
    token: String,
    result: Value,
}

#[derive(Deserialize)]
struct JudgeCommitParams {
    token: String,
    verdict: TeamVerdict,
}

#[derive(Deserialize)]
struct ApplyParams {
    artifact: TeamArtifact,
    #[serde(default)]
    force: bool,
}

#[derive(Deserialize)]
struct PatchedParams {
    change_id: String,
}

#[derive(Deserialize)]
struct NoteParams {
    text: String,
}

#[derive(Deserialize)]
struct GateParams {
    question: String,
    #[serde(default)]
    to: Option<String>,
}

#[derive(Deserialize)]
struct SeatRef {
    agent: String,
    env_id: String,
}

#[derive(Deserialize)]
struct PreflightParams {
    #[serde(default)]
    live: Vec<SeatRef>,
    #[serde(default)]
    min_isolation: Option<String>,
    #[serde(default)]
    clean_worktree: bool,
}

#[derive(Deserialize)]
struct TraceParams {
    #[serde(default)]
    format: Option<String>,
}

// ── Dispatch ──────────────────────────────────────────────────────────────────

impl Server {
    fn send(&self, value: Value) {
        let _ = self.out.send(OutMsg::Line(value.to_string()));
    }

    async fn dispatch_line(self: Arc<Self>, line: String) {
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                self.send(json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": { "code": -32700, "message": format!("parse error: {e}") },
                }));
                return;
            }
        };
        if let Some(method) = msg.get("method").and_then(|m| m.as_str()).map(String::from) {
            let id = msg.get("id").cloned();
            let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
            let outcome = self.clone().handle(&method, params).await;
            if let Some(id) = id {
                match outcome {
                    Ok(result) => self.send(json!({ "jsonrpc": "2.0", "id": id, "result": result })),
                    Err(rej) => self.send(json!({ "jsonrpc": "2.0", "id": id, "error": rej.to_json() })),
                }
            }
            return;
        }
        // No method: this is the client answering a server→client request
        // (`launcher.on_turn`).
        if let Some(id) = msg.get("id").and_then(|v| v.as_str()) {
            let waiter = self.broker.waiters.lock().expect("broker lock").remove(id);
            if let Some(tx) = waiter {
                let outcome = match msg.get("error") {
                    Some(err) => Err(err
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("launcher error")
                        .to_string()),
                    None => Ok(()),
                };
                let _ = tx.send(outcome);
            }
        }
    }

    fn conductor(&self) -> Result<Conductor, Rej> {
        self.conductor
            .lock()
            .expect("conductor lock")
            .clone()
            .ok_or_else(|| Rej::state("no run launched — call conductor.launch first"))
    }

    async fn handle(self: Arc<Self>, method: &str, params: Value) -> Result<Value, Rej> {
        if method == "initialize" {
            let p: InitializeParams = parse(params)?;
            if p.protocol_version != PROTOCOL_VERSION {
                return Err(Rej::state(format!(
                    "protocol version mismatch: client speaks {}, this h5i speaks {} — \
                     upgrade the older side",
                    p.protocol_version, PROTOCOL_VERSION
                )));
            }
            self.initialized.store(true, Ordering::SeqCst);
            return Ok(json!({
                "protocol_version": PROTOCOL_VERSION,
                "h5i_version": self.h5i_version,
                "capabilities": CAPABILITIES,
            }));
        }
        if !self.initialized.load(Ordering::SeqCst) {
            return Err(Rej::state("not initialized — send initialize first"));
        }

        match method {
            "shutdown" => {
                self.shutdown.notify_one();
                Ok(Value::Null)
            }

            "conductor.launch" => {
                if self.conductor.lock().expect("conductor lock").is_some() {
                    return Err(Rej::state(
                        "a run is already launched on this bridge — one session drives one run",
                    ));
                }
                let p: LaunchParams = parse(params)?;
                let launcher: Arc<dyn RuntimeLauncher> = match p.launcher.as_deref() {
                    None | Some("attach") => Arc::new(Attach),
                    Some("resident") => Arc::new(LaunchResident),
                    Some("client") => Arc::new(ClientLauncher { broker: self.broker.clone() }),
                    Some(other) => {
                        return Err(Rej::invalid_params(format!(
                            "unknown launcher '{other}' (attach | resident | client)"
                        )))
                    }
                };
                let c = blocking(move || {
                    let mut b = Conductor::builder(p.repo.as_deref().unwrap_or("."), &p.run)
                        .launcher(launcher);
                    if let Some(t) = p.title {
                        b = b.title(t);
                    }
                    if let Some(base) = p.base {
                        b = b.base(base);
                    }
                    if let Some(n) = p.max_rounds {
                        b = b.max_rounds(n);
                    }
                    if let Some(a) = p.actor {
                        b = b.actor(a);
                    }
                    if let Some(ms) = p.poll_interval_ms {
                        b = b.poll_interval(Duration::from_millis(ms));
                    }
                    if let Some(ms) = p.turn_timeout_ms {
                        b = b.turn_timeout(Duration::from_millis(ms));
                    }
                    b = match p.score_digest {
                        Some(d) => b.score_digest_override(d),
                        None => b.without_score_digest(),
                    };
                    b.launch()
                })
                .await?;
                let replayed = c.core.journal.replay_len();
                let (run_id, actor) = (c.run_id().to_string(), c.core.actor.clone());
                let mut guard = self.conductor.lock().expect("conductor lock");
                if guard.is_some() {
                    return Err(Rej::state("a run is already launched on this bridge"));
                }
                *guard = Some(c);
                Ok(json!({ "run_id": run_id, "actor": actor, "replayed_steps": replayed }))
            }

            "conductor.status" => ok(self.conductor()?.status().await.map_err(Rej::from)?.run),
            "conductor.events" => ok(self.conductor()?.status().await.map_err(Rej::from)?.events),
            "conductor.compare" => ok(self.conductor()?.compare().await.map_err(Rej::from)?),
            "conductor.roster" => {
                let agents = self.conductor()?.roster().await.map_err(Rej::from)?;
                Ok(Value::Array(
                    agents
                        .iter()
                        .map(|a| json!({ "agent_id": a.id(), "env_id": a.env_id() }))
                        .collect(),
                ))
            }
            "conductor.note" => {
                let p: NoteParams = parse(params)?;
                self.conductor()?.note(p.text).await.map_err(Rej::from)?;
                Ok(Value::Null)
            }
            "conductor.freeze" => ok(self.conductor()?.freeze().await.map_err(Rej::from)?),
            "conductor.verify" => {
                let p: VerifyParams = parse(params)?;
                let c = self.conductor()?;
                let v = match p.tests_from.as_deref() {
                    Some(tf) => {
                        c.verify_with_tests(&p.artifact, tf, p.command, p.isolation.as_deref())
                            .await
                    }
                    None => c.verify(&p.artifact, p.command, p.isolation.as_deref()).await,
                };
                ok(v.map_err(Rej::from)?)
            }
            "conductor.judge" => {
                let p: JudgeParams = parse(params)?;
                match p.policy.as_str() {
                    "tests_then_smallest_diff" => ok(self
                        .conductor()?
                        .judge(policy::tests_then_smallest_diff())
                        .await
                        .map_err(Rej::from)?),
                    other => Err(Rej::invalid_params(format!(
                        "unknown built-in policy '{other}' (built-ins: tests_then_smallest_diff); \
                         for a custom policy use judge_begin/judge_commit"
                    ))),
                }
            }
            "conductor.apply" => {
                let p: ApplyParams = parse(params)?;
                let c = self.conductor()?;
                let result = if p.force {
                    c.apply_forced(&p.artifact).await
                } else {
                    c.apply(&p.artifact).await
                };
                ok(result.map_err(Rej::from)?)
            }
            "conductor.patched" => {
                let p: PatchedParams = parse(params)?;
                ok(self.conductor()?.patched(&p.change_id).await.map_err(Rej::from)?)
            }
            "conductor.preflight" => {
                let p: PreflightParams = parse(params)?;
                let c = self.conductor()?;
                let seats: Vec<Agent> = p
                    .live
                    .into_iter()
                    .map(|s| Agent::bind(c.core.clone(), s.agent, s.env_id))
                    .collect();
                let mut pf = c.preflight().require_live(seats.iter());
                if let Some(t) = p.min_isolation {
                    pf = pf.require_isolation(t);
                }
                if p.clean_worktree {
                    pf = pf.require_clean_worktree();
                }
                pf.run().await.map_err(Rej::from)?;
                Ok(Value::Null)
            }
            "conductor.trace" => {
                let p: TraceParams = parse(params)?;
                let c = self.conductor()?;
                let status = c.status().await.map_err(Rej::from)?;
                let rendered = match p.format.as_deref() {
                    None | Some("text") => trace::render_trace(c.run_id(), &status.events),
                    Some("dot") => trace::render_trace_dot(c.run_id(), &status.events),
                    Some(other) => {
                        return Err(Rej::invalid_params(format!(
                            "unknown trace format '{other}' (text | dot)"
                        )))
                    }
                };
                Ok(Value::String(rendered))
            }

            "conductor.step_begin" => {
                let p: StepBeginParams = parse(params)?;
                if p.label.trim().is_empty() {
                    return Err(Rej::invalid_params("step label must be non-empty"));
                }
                let core = self.conductor()?.core.clone();
                let key = core.journal.next_key(&p.label).map_err(Rej::from)?;
                if let Some(replayed) = core.journal.replay_as::<Value>(&key) {
                    core.journal.finish(&p.label);
                    let result = replayed.map_err(Rej::from)?;
                    return Ok(json!({ "replayed": true, "result": result }));
                }
                self.steps.lock().expect("steps lock").insert(
                    key.clone(),
                    PendingStep { label: p.label, judge: false, started: Instant::now() },
                );
                Ok(json!({ "replayed": false, "token": key }))
            }
            "conductor.step_commit" => {
                let p: StepCommitParams = parse(params)?;
                let pending = self
                    .steps
                    .lock()
                    .expect("steps lock")
                    .remove(&p.token)
                    .ok_or_else(|| Rej::state(format!("unknown step token '{}'", p.token)))?;
                if pending.judge {
                    return Err(Rej::state("token belongs to judge_begin — use judge_commit"));
                }
                let core = self.conductor()?.core.clone();
                let duration_ms = pending.started.elapsed().as_millis() as u64;
                let (token, label, result) = (p.token, pending.label, p.result);
                let recorded = {
                    let (core, label, result) = (core.clone(), label.clone(), result.clone());
                    blocking(move || {
                        core.journal.record(&token, &label, &result, duration_ms)?;
                        Ok(())
                    })
                    .await
                };
                core.journal.finish(&label);
                recorded?;
                Ok(result)
            }
            "conductor.judge_begin" => {
                let c = self.conductor()?;
                // Snapshot before allocating the label so a status failure
                // can't wedge the in-flight `judge` label.
                let run = c.status().await.map_err(Rej::from)?.run;
                let core = c.core.clone();
                let key = core.journal.next_key("judge").map_err(Rej::from)?;
                if let Some(replayed) = core.journal.replay_as::<TeamVerdict>(&key) {
                    core.journal.finish("judge");
                    let verdict = replayed.map_err(Rej::from)?;
                    return Ok(json!({ "replayed": true, "verdict": verdict }));
                }
                self.steps.lock().expect("steps lock").insert(
                    key.clone(),
                    PendingStep { label: "judge".into(), judge: true, started: Instant::now() },
                );
                Ok(json!({ "replayed": false, "token": key, "run": run }))
            }
            "conductor.judge_commit" => {
                let p: JudgeCommitParams = parse(params)?;
                let pending = self
                    .steps
                    .lock()
                    .expect("steps lock")
                    .remove(&p.token)
                    .ok_or_else(|| Rej::state(format!("unknown judge token '{}'", p.token)))?;
                if !pending.judge {
                    return Err(Rej::state("token belongs to step_begin — use step_commit"));
                }
                let core = self.conductor()?.core.clone();
                let duration_ms = pending.started.elapsed().as_millis() as u64;
                let recorded = {
                    let (core, token, verdict) = (core.clone(), p.token, p.verdict.clone());
                    blocking(move || {
                        let repo = core.repo()?;
                        team::record_verdict(&repo, &core.run_id, &verdict, &core.actor)?;
                        core.journal.record(&token, "judge", &verdict, duration_ms)?;
                        Ok(())
                    })
                    .await
                };
                core.journal.finish("judge");
                recorded?;
                ok(p.verdict)
            }
            "conductor.step_abort" | "conductor.judge_abort" => {
                let p: TokenParams = parse(params)?;
                let pending = self.steps.lock().expect("steps lock").remove(&p.token);
                if let Some(pending) = pending {
                    self.conductor()?.core.journal.finish(&pending.label);
                }
                Ok(Value::Null)
            }

            "agent.hire" => {
                let p: HireParams = parse(params)?;
                let mut b = self.conductor()?.agent(&p.name);
                if let Some(r) = p.runtime {
                    b = b.runtime(r);
                }
                if let Some(m) = p.model {
                    b = b.model(m);
                }
                if let Some(e) = p.effort {
                    b = b.effort(e);
                }
                if let Some(pr) = p.profile {
                    b = b.profile(pr);
                }
                if let Some(i) = p.isolation {
                    b = b.isolation(i);
                }
                if let Some(e) = p.env {
                    b = b.env(e);
                }
                let agent = b.hire().await.map_err(Rej::from)?;
                Ok(json!({ "agent_id": agent.id(), "env_id": agent.env_id() }))
            }
            "agent.work" => {
                let p: WorkParams = parse(params)?;
                let agent = Agent::bind(self.conductor()?.core.clone(), p.agent, p.env_id);
                let mut request = agent.work(p.task);
                if !p.materials.is_empty() {
                    request = request.with_materials(p.materials.iter());
                }
                if p.expect_independent {
                    request = request.expect_independent();
                }
                ok(request.await.map_err(Rej::from)?)
            }
            "agent.ask" => {
                let p: AskParams = parse(params)?;
                let agent = Agent::bind(self.conductor()?.core.clone(), p.agent, p.env_id);
                ok(agent.ask::<Value>(p.prompt).await.map_err(Rej::from)?)
            }
            "agent.review" => {
                let p: ReviewParams = parse(params)?;
                let agent = Agent::bind(self.conductor()?.core.clone(), p.reviewer, p.env_id);
                ok(agent.review(&p.artifact).await.map_err(Rej::from)?)
            }
            "agent.revise" => {
                let p: ReviseParams = parse(params)?;
                let agent = Agent::bind(self.conductor()?.core.clone(), p.agent, p.env_id);
                ok(agent.revise(&p.artifact, &p.review).await.map_err(Rej::from)?)
            }

            "gate.ask" => {
                let p: GateParams = parse(params)?;
                let mut gate = self.conductor()?.gate(p.question);
                if let Some(to) = p.to {
                    gate = gate.to(to);
                }
                ok(gate.answer().await.map_err(Rej::from)?)
            }

            other => Err(Rej {
                code: -32601,
                message: format!("unknown method '{other}'"),
                kind: None,
            }),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod rpc_tests {
    use super::*;
    use git2::Oid;
    use std::fs;
    use std::path::Path;
    use tokio::io::{AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

    fn sig() -> git2::Signature<'static> {
        git2::Signature::now("Test", "test@example.com").unwrap()
    }

    fn init_repo(dir: &Path) -> Repository {
        let repo = Repository::init(dir).unwrap();
        {
            let mut cfg = repo.config().unwrap();
            cfg.set_str("user.name", "Test").unwrap();
            cfg.set_str("user.email", "test@example.com").unwrap();
        }
        let work = repo.workdir().unwrap();
        fs::write(work.join("README.md"), "hello\n").unwrap();
        let mut idx = repo.index().unwrap();
        idx.add_path(Path::new("README.md")).unwrap();
        idx.write().unwrap();
        let tree_oid = idx.write_tree().unwrap();
        {
            let tree = repo.find_tree(tree_oid).unwrap();
            repo.commit(Some("HEAD"), &sig(), &sig(), "init", &tree, &[])
                .unwrap();
        }
        repo
    }

    struct Client {
        reader: tokio::io::Lines<BufReader<ReadHalf<DuplexStream>>>,
        writer: WriteHalf<DuplexStream>,
        next_id: u64,
        server: tokio::task::JoinHandle<Result<(), H5iError>>,
    }

    impl Client {
        async fn connect() -> Client {
            let (client_side, server_side) = tokio::io::duplex(1 << 20);
            let (srv_read, srv_write) = tokio::io::split(server_side);
            let server = tokio::spawn(serve(BufReader::new(srv_read), srv_write, "test"));
            let (cli_read, cli_write) = tokio::io::split(client_side);
            Client {
                reader: BufReader::new(cli_read).lines(),
                writer: cli_write,
                next_id: 0,
                server,
            }
        }

        async fn send_raw(&mut self, value: Value) {
            self.writer
                .write_all(format!("{value}\n").as_bytes())
                .await
                .unwrap();
        }

        async fn recv(&mut self) -> Value {
            let line = self.reader.next_line().await.unwrap().expect("server line");
            serde_json::from_str(&line).unwrap()
        }

        /// Send `method` and await *its* response (skipping unrelated lines is
        /// not needed — these tests issue one request at a time).
        async fn call(&mut self, method: &str, params: Value) -> Value {
            self.next_id += 1;
            let id = self.next_id;
            self.send_raw(json!({
                "jsonrpc": "2.0", "id": id, "method": method, "params": params,
            }))
            .await;
            let response = self.recv().await;
            assert_eq!(response["id"], json!(id), "response pairs with request");
            response
        }

        async fn expect_ok(&mut self, method: &str, params: Value) -> Value {
            let response = self.call(method, params).await;
            assert!(
                response.get("error").is_none(),
                "{method} failed: {}",
                response["error"]
            );
            response["result"].clone()
        }

        async fn initialize(&mut self) {
            let result = self
                .expect_ok("initialize", json!({ "protocol_version": PROTOCOL_VERSION }))
                .await;
            assert_eq!(result["protocol_version"], json!(PROTOCOL_VERSION));
        }

        async fn shutdown(mut self) {
            self.expect_ok("shutdown", json!({})).await;
            drop(self.writer);
            let _ = self.server.await;
        }
    }

    #[tokio::test]
    async fn handshake_gates_and_versions() {
        let mut client = Client::connect().await;

        // Anything before initialize is refused.
        let response = client.call("conductor.status", json!({})).await;
        assert_eq!(response["error"]["code"], json!(-32002));

        // A protocol mismatch is refused.
        let response = client
            .call("initialize", json!({ "protocol_version": 999 }))
            .await;
        assert!(response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("protocol version mismatch"));

        client.initialize().await;
        let response = client.call("no.such.method", json!({})).await;
        assert_eq!(response["error"]["code"], json!(-32601));

        // Launched-state gating: conductor ops before launch are refused.
        let response = client.call("conductor.freeze", json!({})).await;
        assert_eq!(response["error"]["code"], json!(-32002));

        client.shutdown().await;
    }

    #[tokio::test]
    async fn launch_step_journal_and_resume() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let repo_path = dir.path().to_string_lossy().into_owned();

        // Session 1: launch, journal a step, freeze.
        let mut client = Client::connect().await;
        client.initialize().await;
        let launched = client
            .expect_ok(
                "conductor.launch",
                json!({ "repo": repo_path, "run": "bridge", "actor": "human" }),
            )
            .await;
        assert_eq!(launched["run_id"], json!("bridge"));
        assert_eq!(launched["replayed_steps"], json!(0));

        // Double-launch is refused.
        let response = client
            .call("conductor.launch", json!({ "repo": repo_path, "run": "bridge" }))
            .await;
        assert_eq!(response["error"]["code"], json!(-32002));

        // Fresh execution: patched() is true.
        let patched = client
            .expect_ok("conductor.patched", json!({ "change_id": "x" }))
            .await;
        assert_eq!(patched, json!(true));

        // Remote journaled step: begin → client computes → commit.
        let begun = client
            .expect_ok("conductor.step_begin", json!({ "label": "fetch" }))
            .await;
        assert_eq!(begun["replayed"], json!(false));
        let token = begun["token"].as_str().unwrap().to_string();
        assert_eq!(token, "fetch#1");
        let committed = client
            .expect_ok(
                "conductor.step_commit",
                json!({ "token": token, "result": { "rows": 3 } }),
            )
            .await;
        assert_eq!(committed, json!({ "rows": 3 }));

        // Concurrent same-label steps fail closed (journal discipline).
        let begun = client
            .expect_ok("conductor.step_begin", json!({ "label": "flaky" }))
            .await;
        let token = begun["token"].as_str().unwrap().to_string();
        assert_eq!(token, "flaky#1");
        let response = client
            .call("conductor.step_begin", json!({ "label": "flaky" }))
            .await;
        assert!(response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("concurrent steps"));
        // Abort releases the label for retry; the aborted attempt consumed
        // its sequence number (exactly like a failed closure in the Rust
        // eDSL), so the retry records under flaky#2.
        client
            .expect_ok("conductor.step_abort", json!({ "token": token }))
            .await;
        let begun = client
            .expect_ok("conductor.step_begin", json!({ "label": "flaky" }))
            .await;
        let token = begun["token"].as_str().unwrap().to_string();
        assert_eq!(token, "flaky#2");
        client
            .expect_ok(
                "conductor.step_commit",
                json!({ "token": token, "result": "second" }),
            )
            .await;

        client.expect_ok("conductor.freeze", json!({})).await;
        let status = client.expect_ok("conductor.status", json!({})).await;
        assert_eq!(status["phase"], json!("sealed_submit"));
        let trace = client
            .expect_ok("conductor.trace", json!({}))
            .await;
        assert!(trace.as_str().unwrap().contains("step fetch#1"));
        client.shutdown().await;

        // Session 2: resume — journaled steps replay without re-execution.
        let mut client = Client::connect().await;
        client.initialize().await;
        let launched = client
            .expect_ok(
                "conductor.launch",
                json!({ "repo": repo_path, "run": "bridge", "actor": "human" }),
            )
            .await;
        assert!(launched["replayed_steps"].as_u64().unwrap() >= 4);

        let patched = client
            .expect_ok("conductor.patched", json!({ "change_id": "x" }))
            .await;
        assert_eq!(patched, json!(true), "patched replays its recorded value");

        let begun = client
            .expect_ok("conductor.step_begin", json!({ "label": "fetch" }))
            .await;
        assert_eq!(begun["replayed"], json!(true));
        assert_eq!(begun["result"], json!({ "rows": 3 }));
        // The aborted flaky#1 never recorded: a resume re-executes it live,
        // and the retried flaky#2 replays — the score retrying the same way
        // stays aligned.
        let begun = client
            .expect_ok("conductor.step_begin", json!({ "label": "flaky" }))
            .await;
        assert_eq!(begun["replayed"], json!(false));
        client
            .expect_ok("conductor.step_abort", json!({ "token": begun["token"] }))
            .await;
        let begun = client
            .expect_ok("conductor.step_begin", json!({ "label": "flaky" }))
            .await;
        assert_eq!(begun["replayed"], json!(true));
        assert_eq!(begun["result"], json!("second"));

        // freeze replays (idempotent under resume).
        client.expect_ok("conductor.freeze", json!({})).await;
        client.shutdown().await;
    }

    #[tokio::test]
    async fn judge_begin_commit_records_custom_verdict() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let repo_path = dir.path().to_string_lossy().into_owned();

        let mut client = Client::connect().await;
        client.initialize().await;
        client
            .expect_ok(
                "conductor.launch",
                json!({ "repo": repo_path, "run": "judged", "actor": "human" }),
            )
            .await;
        client.expect_ok("conductor.freeze", json!({})).await;

        let begun = client.expect_ok("conductor.judge_begin", json!({})).await;
        assert_eq!(begun["replayed"], json!(false));
        assert_eq!(begun["run"]["id"], json!("judged"));
        let token = begun["token"].as_str().unwrap().to_string();
        let verdict = json!({
            "selected_submission": null,
            "method": "custom:none",
            "decided_by": "python-policy",
            "can_auto_apply": false,
            "reasons": ["no candidates"],
        });
        let committed = client
            .expect_ok(
                "conductor.judge_commit",
                json!({ "token": token, "verdict": verdict }),
            )
            .await;
        assert_eq!(committed["method"], json!("custom:none"));

        // The verdict landed on the run.
        let status = client.expect_ok("conductor.status", json!({})).await;
        assert_eq!(status["verdict"]["method"], json!("custom:none"));
        client.shutdown().await;

        // A resumed session replays judge#1 without re-deciding.
        let mut client = Client::connect().await;
        client.initialize().await;
        client
            .expect_ok(
                "conductor.launch",
                json!({ "repo": repo_path, "run": "judged", "actor": "human" }),
            )
            .await;
        let begun = client.expect_ok("conductor.judge_begin", json!({})).await;
        assert_eq!(begun["replayed"], json!(true));
        assert_eq!(begun["verdict"]["method"], json!("custom:none"));
        client.shutdown().await;
    }

    #[tokio::test]
    async fn client_launcher_round_trips_turns() {
        // Unit-level: the broker/launcher pair over a real channel, with the
        // client side scripted by hand (the full loop is covered by the SDK's
        // integration suite against the real binary).
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<OutMsg>();
        let broker = Arc::new(TurnBroker {
            out: out_tx,
            seq: AtomicU64::new(0),
            waiters: Mutex::new(BTreeMap::new()),
        });
        let launcher = ClientLauncher { broker: broker.clone() };
        let turn = TurnContext {
            run_id: "r".into(),
            agent_id: "claude".into(),
            env_id: "env/claude/r-claude".into(),
            kind: TurnKind::Review { target: "codex".into() },
            instruction: "look".into(),
            repo_workdir: "/tmp/x".into(),
            h5i_root: "/tmp/x/.h5i".into(),
            work_dir: None,
            runtime: Some("claude".into()),
            model: None,
            effort: None,
            };
        let worker = tokio::task::spawn_blocking(move || launcher.on_turn(&turn));

        // The "client": read the request off the channel, answer it.
        let msg = out_rx.recv().await.expect("launcher request");
        let OutMsg::Line(line) = msg else { panic!("expected a line") };
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["method"], json!("launcher.on_turn"));
        assert_eq!(request["params"]["kind"], json!("review"));
        assert_eq!(request["params"]["target"], json!("codex"));
        let id = request["id"].as_str().unwrap().to_string();
        let waiter = broker.waiters.lock().unwrap().remove(&id).unwrap();
        waiter.send(Ok(())).unwrap();
        worker.await.unwrap().expect("turn succeeds");

        // And a failing answer surfaces as an error.
        let launcher = ClientLauncher { broker: broker.clone() };
        let turn = TurnContext {
            run_id: "r".into(),
            agent_id: "claude".into(),
            env_id: "e".into(),
            kind: TurnKind::Work,
            instruction: "go".into(),
            repo_workdir: "/tmp/x".into(),
            h5i_root: "/tmp/x/.h5i".into(),
            work_dir: None,
            runtime: None,
            model: None,
            effort: None,
            };
        let worker = tokio::task::spawn_blocking(move || launcher.on_turn(&turn));
        let msg = out_rx.recv().await.expect("launcher request");
        let OutMsg::Line(line) = msg else { panic!("expected a line") };
        let request: Value = serde_json::from_str(&line).unwrap();
        let id = request["id"].as_str().unwrap().to_string();
        let waiter = broker.waiters.lock().unwrap().remove(&id).unwrap();
        waiter.send(Err("no session".into())).unwrap();
        let err = worker.await.unwrap().unwrap_err();
        assert!(err.to_string().contains("no session"), "{err}");
        let _ = Oid::zero(); // keep the git2 test import exercised
    }
}
