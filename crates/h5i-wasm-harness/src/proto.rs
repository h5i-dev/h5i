//! Wire protocol: JSON encoding of the Effect / Event types that cross the
//! wasm<->host boundary, plus the init contract and the dump() shape. One
//! place defines the schema so the wasm wrapper and native hosts cannot drift.
//!
//! init input:   {"task": str, "tools": [str...], "workspace_note": str,
//!                "model": str?, "max_steps": n?}
//! step input:   {"model_reply": {"body": str}}
//!             | {"model_failed": {"status": n, "body": str}}   (0 = transport)
//!             | {"tool_finished": {"call_id": str, "ok": bool, "output": str}}
//! effect out:   {"call_model": {"request": str}}   (raw chat-completions body)
//!             | {"run_tool": {"call_id": str, "name": str, "args": obj}}
//!             | {"done": {"status": str, "result": str}}
//!             | {"fatal": {"message": str}}        (init/protocol failures)
//! dump out:     {"steps": n, "messages": [chat-completions message objs]}

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::vec;

use crate::agent::{msg_to_value, Agent, Config, Effect, Event};
use crate::json::{parse, Value};

pub fn effect_to_json(effect: &Effect) -> String {
    let value = match effect {
        Effect::CallModel { request } => Value::obj(vec![(
            "call_model",
            Value::obj(vec![("request", Value::str(request))]),
        )]),
        Effect::RunTool { call_id, name, args } => Value::obj(vec![(
            "run_tool",
            Value::obj(vec![
                ("call_id", Value::str(call_id)),
                ("name", Value::str(name)),
                ("args", args.clone()),
            ]),
        )]),
        Effect::Done { status, result } => Value::obj(vec![(
            "done",
            Value::obj(vec![
                ("status", Value::str(status)),
                ("result", Value::str(result)),
            ]),
        )]),
    };
    value.dump()
}

pub fn fatal_json(message: &str) -> String {
    Value::obj(vec![("fatal", Value::obj(vec![("message", Value::str(message))]))]).dump()
}

pub fn event_from_json(input: &str) -> Result<Event, String> {
    let value = parse(input)?;
    if let Some(reply) = value.get("model_reply") {
        let body = reply
            .get("body")
            .and_then(Value::as_str)
            .ok_or("model_reply.body must be a string")?;
        return Ok(Event::ModelReply { body: body.to_string() });
    }
    if let Some(failed) = value.get("model_failed") {
        let status = failed
            .get("status")
            .and_then(Value::as_f64)
            .ok_or("model_failed.status must be a number")? as u32;
        let body = failed.get("body").and_then(Value::as_str).unwrap_or("");
        return Ok(Event::ModelFailed { status, body: body.to_string() });
    }
    if let Some(finished) = value.get("tool_finished") {
        let call_id = finished
            .get("call_id")
            .and_then(Value::as_str)
            .ok_or("tool_finished.call_id must be a string")?;
        let ok = match finished.get("ok") {
            Some(Value::Bool(b)) => *b,
            _ => return Err("tool_finished.ok must be a bool".to_string()),
        };
        let output = finished
            .get("output")
            .and_then(Value::as_str)
            .ok_or("tool_finished.output must be a string")?;
        return Ok(Event::ToolFinished {
            call_id: call_id.to_string(),
            ok,
            output: output.to_string(),
        });
    }
    Err("expected model_reply, model_failed, or tool_finished".to_string())
}

pub struct InitParams {
    pub task: String,
    pub tools: Vec<String>,
    pub workspace_note: String,
    pub config: Config,
}

pub fn init_params_from_json(input: &str) -> Result<InitParams, String> {
    let value = parse(input)?;
    let task = value
        .get("task")
        .and_then(Value::as_str)
        .ok_or("task must be a string")?
        .to_string();
    let tools = value
        .get("tools")
        .and_then(Value::as_arr)
        .ok_or("tools must be an array of names")?
        .iter()
        .map(|t| t.as_str().map(|s| s.to_string()).ok_or("tool names must be strings"))
        .collect::<Result<Vec<_>, _>>()?
        .to_vec();
    let workspace_note = value
        .get("workspace_note")
        .and_then(Value::as_str)
        .ok_or("workspace_note must be a string")?
        .to_string();
    let mut config = Config::default();
    if let Some(model) = value.get("model").and_then(Value::as_str) {
        config.model = model.to_string();
    }
    if let Some(n) = value.get("max_steps").and_then(Value::as_f64) {
        config.max_steps = n as u32;
    }
    Ok(InitParams { task, tools, workspace_note, config })
}

/// Start an agent straight from init JSON; returns the agent and the first
/// effect already encoded. Shared by the wasm wrapper and native hosts.
pub fn init_from_json(input: &str) -> Result<(Agent, String), String> {
    let params = init_params_from_json(input)?;
    let (agent, effect) =
        Agent::start(&params.task, &params.tools, &params.workspace_note, params.config)?;
    Ok((agent, effect_to_json(&effect)))
}

/// Feed one encoded event to the agent, get the next encoded effect.
pub fn step_json(agent: &mut Agent, input: &str) -> String {
    match event_from_json(input) {
        Ok(event) => effect_to_json(&agent.handle(event)),
        Err(e) => fatal_json(&e),
    }
}

/// Deterministic transcript: no timestamps, no floats-from-clock, insertion-
/// ordered objects only — required so native and wasm runs can be diffed
/// byte-for-byte in the equivalence test.
pub fn dump_json(agent: &Agent) -> String {
    Value::obj(vec![
        ("steps", Value::Num(agent.steps() as f64)),
        ("messages", Value::Arr(agent.messages().iter().map(msg_to_value).collect())),
    ])
    .dump()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_roundtrip() {
        let (mut agent, first) = init_from_json(
            r#"{"task": "do it", "tools": ["read_file"], "workspace_note": "test", "max_steps": 5}"#,
        )
        .unwrap();
        let value = parse(&first).unwrap();
        let request = value
            .get("call_model")
            .unwrap()
            .get("request")
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        // The request itself is valid chat-completions JSON.
        let req = parse(&request).unwrap();
        assert_eq!(req.get("messages").unwrap().as_arr().unwrap().len(), 2);

        let out = step_json(
            &mut agent,
            r#"{"model_reply": {"body": "{\"choices\":[{\"message\":{\"content\":null,\"tool_calls\":[{\"id\":\"c1\",\"type\":\"function\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"x\\\"}\"}}]}}]}"}}"#,
        );
        assert!(out.contains("\"run_tool\""));
        assert!(out.contains("\"call_id\":\"c1\""));

        let out = step_json(
            &mut agent,
            r#"{"tool_finished": {"call_id": "c1", "ok": true, "output": "data"}}"#,
        );
        assert!(out.contains("\"call_model\""));

        let dump = dump_json(&agent);
        let d = parse(&dump).unwrap();
        assert_eq!(d.get("steps").unwrap().as_f64().unwrap(), 2.0);
        assert!(dump.contains("\"tool_call_id\":\"c1\""));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(event_from_json(r#"{"bogus": {}}"#).is_err());
        assert!(init_params_from_json(r#"{"task": "t"}"#).is_err());
        let (mut agent, _) = init_from_json(
            r#"{"task": "t", "tools": ["read_file"], "workspace_note": "w"}"#,
        )
        .unwrap();
        assert!(step_json(&mut agent, "not json").contains("\"fatal\""));
    }
}
