//! End-to-end host loop over the JSON boundary — the same `init` / `step` /
//! `dump` string interface the wasm module exposes, driven by a scripted mock
//! model and an in-memory VFS. This is the native half of the cross-host
//! equivalence idea: the wasm module, fed the identical scripted session,
//! produces the identical `agent_dump()` transcript.

use std::collections::BTreeMap;

use h5i_wasm_harness::json::{Value, parse};
use h5i_wasm_harness::proto::{dump_json, init_from_json, step_json};

/// Build one chat-completions response envelope (`choices[0].message`).
fn envelope(content: &str, calls: &[(&str, &str, &str)]) -> String {
    let mut msg = vec![("role", Value::str("assistant")), ("content", Value::str(content))];
    if !calls.is_empty() {
        let tcs = calls
            .iter()
            .map(|(id, name, args)| {
                Value::obj(vec![
                    ("id", Value::str(id)),
                    ("type", Value::str("function")),
                    (
                        "function",
                        Value::obj(vec![
                            ("name", Value::str(name)),
                            ("arguments", Value::str(args)),
                        ]),
                    ),
                ])
            })
            .collect();
        msg.push(("tool_calls", Value::Arr(tcs)));
    }
    Value::obj(vec![(
        "choices",
        Value::Arr(vec![Value::obj(vec![("message", Value::obj(msg))])]),
    )])
    .dump()
}

fn model_reply_event(body: &str) -> String {
    Value::obj(vec![("model_reply", Value::obj(vec![("body", Value::str(body))]))]).dump()
}

fn tool_finished_event(call_id: &str, ok: bool, output: &str) -> String {
    Value::obj(vec![(
        "tool_finished",
        Value::obj(vec![
            ("call_id", Value::str(call_id)),
            ("ok", Value::Bool(ok)),
            ("output", Value::str(output)),
        ]),
    )])
    .dump()
}

/// In-memory tool executor, mirroring the `i5h` host's semantics.
fn run_tool(vfs: &mut BTreeMap<String, String>, name: &str, args: &Value) -> (bool, String) {
    let path = args.get("path").and_then(Value::as_str).unwrap_or("").to_string();
    match name {
        "write_file" => {
            let content = args.get("content").and_then(Value::as_str).unwrap_or("").to_string();
            let n = content.len();
            vfs.insert(path.clone(), content);
            (true, format!("wrote {} bytes to {}", n, path))
        }
        "read_file" => match vfs.get(&path) {
            Some(c) => (true, c.clone()),
            None => (false, format!("no such file: {}", path)),
        },
        "list_dir" => (true, vfs.keys().cloned().collect::<Vec<_>>().join("\n")),
        other => (false, format!("no executor for {}", other)),
    }
}

#[test]
fn write_then_read_then_finish() {
    let init = r#"{"task":"create hello.txt with hi, then read it back",
        "tools":["read_file","write_file","list_dir"],
        "workspace_note":"in-memory vfs","max_steps":10}"#;

    // The mock model's scripted turns, in order.
    let mut replies = vec![
        envelope("", &[("c1", "write_file", r#"{"path":"hello.txt","content":"hi"}"#)]),
        envelope("", &[("c2", "read_file", r#"{"path":"hello.txt"}"#)]),
        envelope("done: file says hi", &[]),
    ]
    .into_iter();

    let mut vfs = BTreeMap::new();
    let (mut agent, mut effect_json) = init_from_json(init).expect("init");

    let final_status = loop {
        let effect = parse(&effect_json).expect("effect is valid JSON");
        if effect.get("call_model").is_some() {
            let reply = replies.next().expect("model called more times than scripted");
            effect_json = step_json(&mut agent, &model_reply_event(&reply));
        } else if let Some(rt) = effect.get("run_tool") {
            let call_id = rt.get("call_id").and_then(Value::as_str).unwrap().to_string();
            let name = rt.get("name").and_then(Value::as_str).unwrap().to_string();
            let args = rt.get("args").cloned().unwrap();
            let (ok, output) = run_tool(&mut vfs, &name, &args);
            effect_json = step_json(&mut agent, &tool_finished_event(&call_id, ok, &output));
        } else if let Some(done) = effect.get("done") {
            break done.get("status").and_then(Value::as_str).unwrap().to_string();
        } else {
            panic!("unexpected effect: {}", effect_json);
        }
    };

    assert_eq!(final_status, "success");

    // The write actually happened through the boundary.
    assert_eq!(vfs.get("hello.txt").map(String::as_str), Some("hi"));

    // The transcript is well-formed and records the tool exchange.
    let dump = dump_json(&agent);
    let d = parse(&dump).expect("dump is valid JSON");
    let messages = d.get("messages").and_then(Value::as_arr).expect("messages array");
    let roles: Vec<&str> =
        messages.iter().filter_map(|m| m.get("role").and_then(Value::as_str)).collect();
    // system, user, assistant(write), tool(c1), assistant(read), tool(c2), assistant(final)
    assert_eq!(roles.first(), Some(&"system"));
    assert!(roles.iter().filter(|r| **r == "tool").count() == 2, "two tool results recorded");
    assert!(dump.contains("\"tool_call_id\":\"c1\""));
    assert!(dump.contains("\"tool_call_id\":\"c2\""));
}

#[test]
fn bad_step_input_is_fatal_not_panic() {
    let (mut agent, _first) = init_from_json(
        r#"{"task":"t","tools":["read_file"],"workspace_note":"w"}"#,
    )
    .expect("init");
    let out = step_json(&mut agent, "this is not json");
    assert!(out.contains("\"fatal\""), "protocol errors surface as a fatal effect, not a trap");
}
