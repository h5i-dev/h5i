//! The sans-io agent loop: a state machine that never performs I/O.
//! Every effect (model call, tool run) is returned to the host as data;
//! the host performs it and feeds the result back via `handle`.
//!
//! Loop shape follows mini-swe-agent's DefaultAgent (query -> execute actions
//! until an exit condition, ref: src/minisweagent/agents/default.py) with the
//! Model and Environment implementations moved across the wasm boundary, and
//! hax's structural termination rule: the run ends when the model stops
//! calling tools (ref: hax src/agent_loop.h).
//!
//! Wire-level model interface: OpenAI chat-completions with native tool_calls.
//! The guest builds the full request body; the host does POST(bytes)->bytes
//! and reports only the HTTP status alongside the raw body.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::vec;
use alloc::collections::VecDeque;

use crate::json::Value;

pub const DEFAULT_MAX_STEPS: u32 = 20;
/// Consecutive assistant replies whose tool calls were ALL invalid before
/// giving up (mirrors mini-swe-agent's max_consecutive_format_errors,
/// agents/default.py).
pub const MAX_CONSECUTIVE_FORMAT_ERRORS: u32 = 3;
/// Consecutive retryable model failures (429/5xx/transport) before giving up.
pub const MAX_MODEL_RETRIES: u32 = 3;
/// Tool output cap before it enters history: 8 KiB head + 8 KiB tail,
/// middle-out like the blueprint's _truncate_content (demos/tool_calling.html)
/// rather than a tail chop that would hide compiler errors at the end.
pub const TRUNCATE_HEAD: usize = 8 * 1024;
pub const TRUNCATE_TAIL: usize = 8 * 1024;

/// The fixed tool universe. Schemas are compiled into the guest so the two
/// hosts cannot drift; the host only declares which NAMES it supports.
pub const TOOL_UNIVERSE: &[(&str, &str, &str)] = &[
    (
        "read_file",
        "Read a UTF-8 text file from the workspace. Returns its content.",
        r#"{"type":"object","properties":{"path":{"type":"string","description":"workspace-relative path"}},"required":["path"]}"#,
    ),
    (
        "write_file",
        "Create or overwrite a UTF-8 text file in the workspace.",
        r#"{"type":"object","properties":{"path":{"type":"string","description":"workspace-relative path"},"content":{"type":"string"}},"required":["path","content"]}"#,
    ),
    (
        "list_dir",
        "List entries of a workspace directory. Directories end with '/'.",
        r#"{"type":"object","properties":{"path":{"type":"string","description":"workspace-relative path, empty or '.' for the root"}},"required":[]}"#,
    ),
    (
        "bash",
        "Run a bash command in the workspace and return its output. Only available on hosts that declare it.",
        r#"{"type":"object","properties":{"command":{"type":"string"}},"required":["command"]}"#,
    ),
];

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Raw arguments string exactly as the model sent it (it is a JSON-encoded
    /// object per the chat-completions contract, but may be mangled).
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    System { content: String },
    User { content: String },
    Assistant { content: String, tool_calls: Vec<ToolCall> },
    Tool { call_id: String, content: String },
}

/// What the module asks the host to do next.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// POST this exact body to the chat-completions endpoint.
    CallModel { request: String },
    /// Execute one tool call; `args` is the parsed, validated argument object.
    RunTool { call_id: String, name: String, args: Value },
    Done { status: String, result: String },
}

/// What the host feeds back after performing an effect.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// 2xx response: the raw body, unparsed. The guest owns envelope parsing.
    ModelReply { body: String },
    /// Non-2xx status, or 0 for a transport-level failure (network, CORS...).
    ModelFailed { status: u32, body: String },
    ToolFinished { call_id: String, ok: bool, output: String },
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    AwaitingModel,
    AwaitingTool,
    Finished,
}

pub struct Config {
    pub model: String,
    pub max_steps: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config { model: "mock".to_string(), max_steps: DEFAULT_MAX_STEPS }
    }
}

pub struct Agent {
    messages: Vec<Msg>,
    state: State,
    steps: u32,
    consecutive_format_errors: u32,
    consecutive_model_failures: u32,
    /// tool_calls from the current assistant turn not yet dispatched.
    pending_calls: VecDeque<ToolCall>,
    /// The call currently out with the host.
    in_flight: Option<ToolCall>,
    /// Names the host declared it supports (subset of TOOL_UNIVERSE).
    declared_tools: Vec<String>,
    config: Config,
}

impl Agent {
    /// `tool_names` is the subset of TOOL_UNIVERSE the host actually supports;
    /// `workspace_note` is one host-supplied sentence describing the
    /// environment (e.g. "browser virtual FS" vs "real dir, bash available").
    pub fn start(
        task: &str,
        tool_names: &[String],
        workspace_note: &str,
        config: Config,
    ) -> Result<(Agent, Effect), String> {
        let mut declared = Vec::new();
        for name in tool_names {
            if !TOOL_UNIVERSE.iter().any(|(n, _, _)| n == name) {
                return Err(format!("host declared unknown tool '{}'", name));
            }
            declared.push(name.clone());
        }
        if declared.is_empty() {
            return Err("host must declare at least one tool".to_string());
        }
        let mut agent = Agent {
            messages: vec![
                Msg::System { content: system_prompt(&declared, workspace_note) },
                Msg::User { content: task.to_string() },
            ],
            state: State::AwaitingModel,
            steps: 0,
            consecutive_format_errors: 0,
            consecutive_model_failures: 0,
            pending_calls: VecDeque::new(),
            in_flight: None,
            declared_tools: declared,
            config,
        };
        let effect = agent.call_model();
        Ok((agent, effect))
    }

    pub fn messages(&self) -> &[Msg] {
        &self.messages
    }

    pub fn steps(&self) -> u32 {
        self.steps
    }

    pub fn handle(&mut self, event: Event) -> Effect {
        match (self.state, event) {
            (State::Finished, _) => {
                self.finish("protocol_error", "agent already finished")
            }
            (State::AwaitingModel, Event::ModelReply { body }) => {
                self.consecutive_model_failures = 0;
                self.handle_model_reply(&body)
            }
            (State::AwaitingModel, Event::ModelFailed { status, body }) => {
                self.handle_model_failure(status, &body)
            }
            (State::AwaitingTool, Event::ToolFinished { call_id, ok, output }) => {
                self.handle_tool_finished(&call_id, ok, &output)
            }
            (state, event) => self.finish(
                "protocol_error",
                &format!("host sent {} while agent was in {:?}", kind_of(&event), state),
            ),
        }
    }

    fn handle_model_reply(&mut self, body: &str) -> Effect {
        // Unparseable 2xx envelope is fatal: the transport worked, so this is
        // an endpoint we do not understand, and retrying will not change that.
        let parsed = match parse_envelope(body) {
            Ok(p) => p,
            Err(e) => return self.finish("model_error", &format!("unparseable model envelope: {}", e)),
        };
        self.messages.push(Msg::Assistant {
            content: parsed.content.clone(),
            tool_calls: parsed.tool_calls.clone(),
        });
        if parsed.tool_calls.is_empty() {
            // Structural termination: the model stopped calling tools.
            return self.finish("success", &parsed.content);
        }
        self.pending_calls = parsed.tool_calls.into();
        self.dispatch_next(true)
    }

    /// Dispatch buffered calls one at a time. Invalid calls (undeclared name,
    /// mangled arguments) are answered guest-side with an error tool result —
    /// recoverable, like hax's unknown-tool error output (src/agent_tool.h)
    /// and mini's FormatError feedback loop. `fresh_turn` is true right after
    /// an assistant reply so the format-error counter is updated once per turn.
    fn dispatch_next(&mut self, fresh_turn: bool) -> Effect {
        loop {
            let Some(call) = self.pending_calls.pop_front() else {
                // Reaching the drain on a fresh turn means no call was valid
                // (a valid one returns RunTool below); count it like mini's
                // consecutive FormatErrors.
                if fresh_turn {
                    self.consecutive_format_errors += 1;
                    if self.consecutive_format_errors >= MAX_CONSECUTIVE_FORMAT_ERRORS {
                        return self.finish(
                            "format_error",
                            "too many consecutive replies with only invalid tool calls",
                        );
                    }
                }
                return self.call_model();
            };
            match self.validate_call(&call) {
                Ok(args) => {
                    if fresh_turn {
                        self.consecutive_format_errors = 0;
                    }
                    self.state = State::AwaitingTool;
                    let effect = Effect::RunTool {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        args,
                    };
                    self.in_flight = Some(call);
                    return effect;
                }
                Err(problem) => {
                    self.messages.push(Msg::Tool {
                        call_id: call.id.clone(),
                        content: format!("ERROR: {}", problem),
                    });
                }
            }
        }
    }

    fn validate_call(&self, call: &ToolCall) -> Result<Value, String> {
        if !self.declared_tools.iter().any(|n| n == &call.name) {
            return Err(format!(
                "unknown tool '{}'; available: {}",
                call.name,
                self.declared_tools.join(", ")
            ));
        }
        let args = crate::json::parse(&call.arguments)
            .map_err(|e| format!("arguments are not valid JSON: {}", e))?;
        if !matches!(args, Value::Obj(_)) {
            return Err("arguments must be a JSON object".to_string());
        }
        Ok(args)
    }

    fn handle_tool_finished(&mut self, call_id: &str, ok: bool, output: &str) -> Effect {
        let Some(expected) = self.in_flight.take() else {
            return self.finish("protocol_error", "ToolFinished with no call in flight");
        };
        if expected.id != call_id {
            // A host that misroutes call ids is broken code, not a confused
            // model; feeding the mixup back into history would gaslight the
            // model about its own protocol. Fatal.
            return self.finish(
                "protocol_error",
                &format!(
                    "host answered call_id '{}' but '{}' was in flight",
                    call_id, expected.id
                ),
            );
        }
        let truncated = truncate_middle_out(output);
        let content = if ok { truncated } else { format!("ERROR: {}", truncated) };
        self.messages.push(Msg::Tool { call_id: expected.id, content });
        self.dispatch_next(false)
    }

    fn handle_model_failure(&mut self, status: u32, body: &str) -> Effect {
        let retryable = status == 0 || status == 429 || status >= 500;
        if !retryable {
            return self.finish(
                "model_error",
                &format!("model endpoint returned {}: {}", status, truncate_middle_out(body)),
            );
        }
        self.consecutive_model_failures += 1;
        if self.consecutive_model_failures >= MAX_MODEL_RETRIES {
            return self.finish(
                "model_error",
                &format!("model failed {} times, last {}: {}",
                    self.consecutive_model_failures, status, truncate_middle_out(body)),
            );
        }
        // Re-issue the identical request; retry pacing is host policy (the
        // guest has no clock).
        self.build_call_model()
    }

    fn call_model(&mut self) -> Effect {
        if self.steps >= self.config.max_steps {
            return self.finish("limits_exceeded", "step limit reached");
        }
        self.steps += 1;
        self.build_call_model()
    }

    fn build_call_model(&mut self) -> Effect {
        self.state = State::AwaitingModel;
        Effect::CallModel { request: self.build_request() }
    }

    fn build_request(&self) -> String {
        let tools = Value::Arr(
            self.declared_tools
                .iter()
                .map(|name| {
                    let (n, desc, schema) = TOOL_UNIVERSE
                        .iter()
                        .find(|(n, _, _)| n == name)
                        .expect("declared tools are validated at start");
                    Value::obj(vec![
                        ("type", Value::str("function")),
                        (
                            "function",
                            Value::obj(vec![
                                ("name", Value::str(n)),
                                ("description", Value::str(desc)),
                                (
                                    "parameters",
                                    crate::json::parse(schema).expect("static schema parses"),
                                ),
                            ]),
                        ),
                    ])
                })
                .collect(),
        );
        let messages = Value::Arr(self.messages.iter().map(msg_to_value).collect());
        Value::obj(vec![
            ("model", Value::str(&self.config.model)),
            ("messages", messages),
            ("tools", tools),
        ])
        .dump()
    }

    fn finish(&mut self, status: &str, result: &str) -> Effect {
        self.state = State::Finished;
        Effect::Done { status: status.to_string(), result: result.to_string() }
    }
}

pub fn msg_to_value(msg: &Msg) -> Value {
    match msg {
        Msg::System { content } => Value::obj(vec![
            ("role", Value::str("system")),
            ("content", Value::str(content)),
        ]),
        Msg::User { content } => Value::obj(vec![
            ("role", Value::str("user")),
            ("content", Value::str(content)),
        ]),
        Msg::Assistant { content, tool_calls } => {
            let mut pairs = vec![
                ("role", Value::str("assistant")),
                ("content", Value::str(content)),
            ];
            if !tool_calls.is_empty() {
                pairs.push((
                    "tool_calls",
                    Value::Arr(
                        tool_calls
                            .iter()
                            .map(|tc| {
                                Value::obj(vec![
                                    ("id", Value::str(&tc.id)),
                                    ("type", Value::str("function")),
                                    (
                                        "function",
                                        Value::obj(vec![
                                            ("name", Value::str(&tc.name)),
                                            ("arguments", Value::str(&tc.arguments)),
                                        ]),
                                    ),
                                ])
                            })
                            .collect(),
                    ),
                ));
            }
            Value::obj(pairs)
        }
        Msg::Tool { call_id, content } => Value::obj(vec![
            ("role", Value::str("tool")),
            ("tool_call_id", Value::str(call_id)),
            ("content", Value::str(content)),
        ]),
    }
}

struct ParsedReply {
    content: String,
    tool_calls: Vec<ToolCall>,
}

/// Parse a chat-completions response envelope: choices[0].message.
fn parse_envelope(body: &str) -> Result<ParsedReply, String> {
    let value = crate::json::parse(body)?;
    let message = value
        .get("choices")
        .and_then(Value::as_arr)
        .and_then(|c| c.first())
        .and_then(|c| c.get("message"))
        .ok_or("missing choices[0].message")?;
    let content = message
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("") // content may legitimately be null alongside tool_calls
        .to_string();
    let mut tool_calls = Vec::new();
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_arr) {
        for call in calls {
            let function = call.get("function").ok_or("tool_call missing function")?;
            tool_calls.push(ToolCall {
                id: call
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or("tool_call missing id")?
                    .to_string(),
                name: function
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or("tool_call missing function.name")?
                    .to_string(),
                arguments: function
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("{}")
                    .to_string(),
            });
        }
    }
    Ok(ParsedReply { content, tool_calls })
}

fn system_prompt(declared: &[String], workspace_note: &str) -> String {
    let mut prompt = String::from(
        "You are a minimal coding agent operating on a workspace through tools. \
         Work in small steps: call one or more tools, read the results, continue. \
         When the task is complete (or impossible), reply WITHOUT any tool call; \
         that final message is your report and ends the run.\n\nAvailable tools:\n",
    );
    for name in declared {
        if let Some((n, desc, _)) = TOOL_UNIVERSE.iter().find(|(n, _, _)| n == name) {
            prompt.push_str(&format!("- {}: {}\n", n, desc));
        }
    }
    prompt.push_str("\nWorkspace: ");
    prompt.push_str(workspace_note);
    prompt
}

/// Cap text middle-out: head + tail with an explicit marker, so build/test
/// failures at the END of long output survive (ref: _truncate_content in
/// wasm-agents-blueprint demos/tool_calling.html; hax src/tools/output_cap.h).
pub fn truncate_middle_out(text: &str) -> String {
    if text.len() <= TRUNCATE_HEAD + TRUNCATE_TAIL {
        return text.to_string();
    }
    let head_end = floor_char_boundary(text, TRUNCATE_HEAD);
    let tail_start = ceil_char_boundary(text, text.len() - TRUNCATE_TAIL);
    format!(
        "{}\n[... truncated, {} bytes total ...]\n{}",
        &text[..head_end],
        text.len(),
        &text[tail_start..]
    )
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

fn kind_of(event: &Event) -> &'static str {
    match event {
        Event::ModelReply { .. } => "ModelReply",
        Event::ModelFailed { .. } => "ModelFailed",
        Event::ToolFinished { .. } => "ToolFinished",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::borrow::ToOwned;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_owned().to_string()).collect()
    }

    fn start() -> (Agent, Effect) {
        Agent::start(
            "create hello.txt containing hi",
            &names(&["read_file", "write_file", "list_dir"]),
            "test workspace",
            Config::default(),
        )
        .unwrap()
    }

    fn envelope(content: &str, calls: &[(&str, &str, &str)]) -> Event {
        let tool_calls: alloc::vec::Vec<String> = calls
            .iter()
            .map(|(id, name, args)| {
                format!(
                    r#"{{"id":{},"type":"function","function":{{"name":{},"arguments":{}}}}}"#,
                    Value::str(id).dump(),
                    Value::str(name).dump(),
                    Value::str(args).dump()
                )
            })
            .collect();
        let tc = if calls.is_empty() {
            String::new()
        } else {
            format!(r#","tool_calls":[{}]"#, tool_calls.join(","))
        };
        Event::ModelReply {
            body: format!(
                r#"{{"choices":[{{"message":{{"role":"assistant","content":{}{}}}}}]}}"#,
                Value::str(content).dump(),
                tc
            ),
        }
    }

    #[test]
    fn full_trace_write_then_done() {
        let (mut agent, effect) = start();
        let Effect::CallModel { request } = effect else { panic!("expected CallModel") };
        let req = crate::json::parse(&request).unwrap();
        assert_eq!(req.get("model").unwrap().as_str().unwrap(), "mock");
        assert_eq!(req.get("tools").unwrap().as_arr().unwrap().len(), 3);

        let effect = agent.handle(envelope(
            "",
            &[("call_1", "write_file", r#"{"path":"hello.txt","content":"hi"}"#)],
        ));
        let Effect::RunTool { call_id, name, args } = effect else { panic!("expected RunTool") };
        assert_eq!(call_id, "call_1");
        assert_eq!(name, "write_file");
        assert_eq!(args.get("path").unwrap().as_str().unwrap(), "hello.txt");

        let effect = agent.handle(Event::ToolFinished {
            call_id: "call_1".to_string(),
            ok: true,
            output: "written".to_string(),
        });
        let Effect::CallModel { request } = effect else { panic!("expected CallModel") };
        assert!(request.contains("\"tool_call_id\":\"call_1\""));

        let effect = agent.handle(envelope("created the file", &[]));
        assert_eq!(
            effect,
            Effect::Done { status: "success".to_string(), result: "created the file".to_string() }
        );
    }

    #[test]
    fn fence_in_file_body_survives() {
        // The counterexample that killed textual actions: a write_file body
        // containing a markdown fence must arrive intact.
        let body = "# Readme\n```rust\nfn main() {}\n```\n";
        let (mut agent, _) = start();
        let args = format!(r#"{{"path":"README.md","content":{}}}"#, Value::str(body).dump());
        let effect = agent.handle(envelope("", &[("c1", "write_file", &args)]));
        let Effect::RunTool { args, .. } = effect else { panic!("expected RunTool") };
        assert_eq!(args.get("content").unwrap().as_str().unwrap(), body);
    }

    #[test]
    fn parallel_calls_buffered_sequential() {
        let (mut agent, _) = start();
        let effect = agent.handle(envelope(
            "",
            &[
                ("c1", "read_file", r#"{"path":"a"}"#),
                ("c2", "read_file", r#"{"path":"b"}"#),
            ],
        ));
        let Effect::RunTool { call_id, .. } = effect else { panic!() };
        assert_eq!(call_id, "c1");
        let effect = agent.handle(Event::ToolFinished {
            call_id: "c1".to_string(),
            ok: true,
            output: "A".to_string(),
        });
        let Effect::RunTool { call_id, .. } = effect else { panic!("second call must dispatch before next CallModel") };
        assert_eq!(call_id, "c2");
        let effect = agent.handle(Event::ToolFinished {
            call_id: "c2".to_string(),
            ok: true,
            output: "B".to_string(),
        });
        assert!(matches!(effect, Effect::CallModel { .. }));
    }

    #[test]
    fn invalid_calls_are_recoverable_and_capped() {
        let (mut agent, _) = start();
        // Undeclared tool + mangled args in one turn -> both self-answered,
        // next CallModel carries the error results.
        let effect = agent.handle(envelope(
            "",
            &[
                ("c1", "bash", r#"{"command":"ls"}"#), // not declared by this host
                ("c2", "read_file", r#"{"path": nope}"#), // mangled JSON
            ],
        ));
        let Effect::CallModel { request } = effect else { panic!("expected CallModel") };
        assert!(request.contains("unknown tool 'bash'"));
        assert!(request.contains("arguments are not valid JSON"));

        // Two more all-invalid turns exhaust the format budget.
        let _ = agent.handle(envelope("", &[("c3", "bash", "{}")]));
        let effect = agent.handle(envelope("", &[("c4", "bash", "{}")]));
        assert!(matches!(effect, Effect::Done { ref status, .. } if status == "format_error"));
    }

    #[test]
    fn model_retry_only_on_retryable_status() {
        let (mut agent, _) = start();
        let effect = agent.handle(Event::ModelFailed { status: 503, body: "overloaded".to_string() });
        assert!(matches!(effect, Effect::CallModel { .. }), "5xx retries");
        let effect = agent.handle(Event::ModelFailed { status: 0, body: "network".to_string() });
        assert!(matches!(effect, Effect::CallModel { .. }), "transport retries");
        let effect = agent.handle(Event::ModelFailed { status: 429, body: "rate".to_string() });
        assert!(matches!(effect, Effect::Done { ref status, .. } if status == "model_error"),
            "third consecutive failure gives up");

        let (mut agent, _) = start();
        let effect = agent.handle(Event::ModelFailed { status: 401, body: "bad key".to_string() });
        assert!(matches!(effect, Effect::Done { ref status, .. } if status == "model_error"),
            "auth errors do not retry");
    }

    #[test]
    fn step_limit_enforced() {
        let (mut agent, _) = Agent::start(
            "t",
            &names(&["read_file"]),
            "w",
            Config { model: "mock".to_string(), max_steps: 2 },
        )
        .unwrap();
        let _ = agent.handle(envelope("", &[("c1", "read_file", r#"{"path":"a"}"#)]));
        let _ = agent.handle(Event::ToolFinished { call_id: "c1".to_string(), ok: true, output: "x".to_string() });
        let effect = agent.handle(envelope("", &[("c2", "read_file", r#"{"path":"a"}"#)]));
        let Effect::RunTool { .. } = effect else { panic!() };
        let effect = agent.handle(Event::ToolFinished { call_id: "c2".to_string(), ok: true, output: "x".to_string() });
        assert!(matches!(effect, Effect::Done { ref status, .. } if status == "limits_exceeded"));
    }

    #[test]
    fn unparseable_envelope_is_fatal() {
        let (mut agent, _) = start();
        let effect = agent.handle(Event::ModelReply { body: "<html>gateway error</html>".to_string() });
        assert!(matches!(effect, Effect::Done { ref status, .. } if status == "model_error"));
    }

    #[test]
    fn call_id_mismatch_is_fatal() {
        // A misrouting host is a protocol violation, not model confusion
        // (thread post 12) — never fed back into model history.
        let (mut agent, _) = start();
        let _ = agent.handle(envelope("", &[("c1", "read_file", r#"{"path":"a"}"#)]));
        let effect = agent.handle(Event::ToolFinished {
            call_id: "WRONG".to_string(),
            ok: true,
            output: "x".to_string(),
        });
        assert!(
            matches!(effect, Effect::Done { ref status, ref result }
                if status == "protocol_error" && result.contains("was in flight"))
        );
    }

    #[test]
    fn truncation_middle_out() {
        let long = "a".repeat(TRUNCATE_HEAD) + &"b".repeat(TRUNCATE_TAIL) + "END";
        let out = truncate_middle_out(&long);
        assert!(out.contains("truncated"));
        assert!(out.ends_with("END"), "tail must survive");
        assert!(out.starts_with('a'), "head must survive");
        assert_eq!(truncate_middle_out("short"), "short");
    }

    #[test]
    fn rejects_unknown_declared_tool() {
        assert!(Agent::start("t", &names(&["teleport"]), "w", Config::default()).is_err());
    }
}
