//! Streaming reassembly for the `i5h` HTTP model: turn an OpenAI-style
//! `chat.completions` SSE stream back into the single non-streaming envelope
//! the sans-io core expects, while handing content deltas to a callback so the
//! host can render tokens live. Streaming is purely a host concern — the wasm
//! module and the core never see a partial response.
//!
//! The reassembly logic (chunk decode, SSE split, delta merge) is pure and
//! unit-tested here; `main.rs` drives it incrementally over the socket.

use h5i_wasm_harness::json::{Value, parse};

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Decode an HTTP/1.1 chunked body, tolerating a trailing incomplete chunk:
/// only the fully-received portion is returned, so it can be called repeatedly
/// as more bytes arrive.
pub fn decode_chunked(bytes: &[u8]) -> String {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(nl) = find(&bytes[i..], b"\r\n") {
        let size_str = String::from_utf8_lossy(&bytes[i..i + nl]);
        let Ok(size) = usize::from_str_radix(size_str.trim(), 16) else { break };
        let data_start = i + nl + 2;
        if size == 0 {
            break; // terminal chunk
        }
        if data_start + size > bytes.len() {
            break; // chunk not fully received yet
        }
        out.extend_from_slice(&bytes[data_start..data_start + size]);
        i = data_start + size;
        if bytes[i..].starts_with(b"\r\n") {
            i += 2;
        } else {
            break; // trailing CRLF not here yet
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Split SSE text into the payloads of *complete* events (terminated by a blank
/// line). A partial trailing event is not returned, so deltas are never fed
/// half-parsed. `data:` lines within one event are joined with newlines.
pub fn split_sse(text: &str) -> Vec<String> {
    let text = text.replace("\r\n", "\n");
    let mut parts: Vec<&str> = text.split("\n\n").collect();
    if !text.ends_with("\n\n") {
        parts.pop(); // last piece is not yet a complete event
    }
    let mut events = Vec::new();
    for part in parts {
        let mut data = String::new();
        for line in part.lines() {
            if let Some(rest) = line.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(rest.trim_start());
            }
        }
        if !data.is_empty() {
            events.push(data);
        }
    }
    events
}

struct ToolAcc {
    index: i64,
    id: String,
    name: String,
    args: String,
}

/// Merges streamed `choices[0].delta` fragments into one assistant message.
pub struct Assembler {
    content: String,
    tools: Vec<ToolAcc>,
    saw_content: bool,
}

impl Assembler {
    pub fn new() -> Self {
        Assembler { content: String::new(), tools: Vec::new(), saw_content: false }
    }

    /// True once any content token has been merged (so the host knows whether
    /// it printed anything and should end the line).
    pub fn rendered_content(&self) -> bool {
        self.saw_content
    }

    /// Merge one SSE `data:` payload. Non-JSON payloads (keep-alives) and
    /// `[DONE]` are ignored here; the caller detects `[DONE]` to stop. Content
    /// deltas are passed to `on_content` for live rendering.
    pub fn push(&mut self, data: &str, on_content: &mut dyn FnMut(&str)) {
        if data == "[DONE]" {
            return;
        }
        let Ok(v) = parse(data) else { return };
        let Some(delta) = v
            .get("choices")
            .and_then(Value::as_arr)
            .and_then(|c| c.first())
            .and_then(|c| c.get("delta"))
        else {
            return;
        };
        if let Some(c) = delta.get("content").and_then(Value::as_str)
            && !c.is_empty()
        {
            self.content.push_str(c);
            self.saw_content = true;
            on_content(c);
        }
        if let Some(tcs) = delta.get("tool_calls").and_then(Value::as_arr) {
            for tc in tcs {
                let index = tc.get("index").and_then(Value::as_f64).unwrap_or(0.0) as i64;
                let pos = self.tools.iter().position(|t| t.index == index).unwrap_or_else(|| {
                    self.tools.push(ToolAcc {
                        index,
                        id: String::new(),
                        name: String::new(),
                        args: String::new(),
                    });
                    self.tools.len() - 1
                });
                let slot = &mut self.tools[pos];
                if let Some(id) = tc.get("id").and_then(Value::as_str)
                    && !id.is_empty()
                {
                    slot.id = id.to_string();
                }
                if let Some(f) = tc.get("function") {
                    if let Some(n) = f.get("name").and_then(Value::as_str) {
                        slot.name.push_str(n);
                    }
                    if let Some(a) = f.get("arguments").and_then(Value::as_str) {
                        slot.args.push_str(a);
                    }
                }
            }
        }
    }

    /// Build the single non-streaming `chat.completions` envelope the core
    /// parses, exactly as a non-streaming endpoint would have returned it.
    pub fn into_envelope(self) -> String {
        let mut msg =
            vec![("role", Value::str("assistant")), ("content", Value::str(&self.content))];
        if !self.tools.is_empty() {
            let arr = self
                .tools
                .iter()
                .map(|t| {
                    Value::obj(vec![
                        ("id", Value::str(&t.id)),
                        ("type", Value::str("function")),
                        (
                            "function",
                            Value::obj(vec![
                                ("name", Value::str(&t.name)),
                                ("arguments", Value::str(&t.args)),
                            ]),
                        ),
                    ])
                })
                .collect();
            msg.push(("tool_calls", Value::Arr(arr)));
        }
        Value::obj(vec![(
            "choices",
            Value::Arr(vec![Value::obj(vec![("message", Value::obj(msg))])]),
        )])
        .dump()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_chunked_partial_is_tolerated() {
        // Two complete chunks then a size line whose data has not arrived.
        let raw = b"4\r\nabcd\r\n3\r\nefg\r\n5\r\nhi";
        assert_eq!(decode_chunked(raw), "abcdefg");
        // Terminal chunk ends decoding.
        let done = b"4\r\nabcd\r\n0\r\n\r\n";
        assert_eq!(decode_chunked(done), "abcd");
    }

    #[test]
    fn split_sse_only_returns_complete_events() {
        let text = "data: a\n\ndata: b\n\ndata: c";
        // "c" has no trailing blank line yet, so it is held back.
        assert_eq!(split_sse(text), vec!["a".to_string(), "b".to_string()]);
        assert_eq!(split_sse("data: a\n\ndata: b\n\n"), vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn reassembles_content_and_tool_calls() {
        // A content stream, then a tool call streamed across fragments.
        let events = [
            r#"{"choices":[{"delta":{"role":"assistant","content":"Hel"}}]}"#,
            r#"{"choices":[{"delta":{"content":"lo"}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"write_file","arguments":"{\"path\":"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a.txt\"}"}}]}}]}"#,
            "[DONE]",
        ];
        let mut seen = String::new();
        let mut asm = Assembler::new();
        for e in events {
            asm.push(e, &mut |c| seen.push_str(c));
        }
        assert_eq!(seen, "Hello", "content deltas were rendered live in order");
        assert!(asm.rendered_content());
        let env = asm.into_envelope();
        let v = parse(&env).unwrap();
        let msg = v.get("choices").unwrap().as_arr().unwrap()[0].get("message").unwrap();
        assert_eq!(msg.get("content").unwrap().as_str().unwrap(), "Hello");
        let tc = &msg.get("tool_calls").unwrap().as_arr().unwrap()[0];
        assert_eq!(tc.get("id").unwrap().as_str().unwrap(), "c1");
        let f = tc.get("function").unwrap();
        assert_eq!(f.get("name").unwrap().as_str().unwrap(), "write_file");
        // The two argument fragments were concatenated into valid JSON.
        assert_eq!(f.get("arguments").unwrap().as_str().unwrap(), r#"{"path":"a.txt"}"#);
    }

    #[test]
    fn keepalives_and_garbage_are_ignored() {
        let mut asm = Assembler::new();
        asm.push("", &mut |_| {});
        asm.push("not json", &mut |_| {});
        asm.push(r#"{"no":"choices"}"#, &mut |_| {});
        assert!(!asm.rendered_content());
        assert!(asm.into_envelope().contains("\"content\":\"\""));
    }
}
