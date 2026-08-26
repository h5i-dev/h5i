//! `h5i-agent`: the native host for the `h5i-wasm-harness` agent core. It drives the
//! same sans-io state machine (compiled natively rather than to wasm) against a
//! real directory, playing the "WASI-style" host role: real filesystem, an
//! optional plain-HTTP local model. The wasm module (`scripts/build-wasm.sh`)
//! runs byte-identical logic behind the six-symbol ABI; this binary is what you
//! run at a terminal.
//!
//! Default mode is interactive: type a task per line and the agent runs it,
//! keeping the conversation across turns. Pass `--task` for a one-shot,
//! scriptable run instead.
//!
//! Model source (required either way):
//!   --script replies.json   scripted mock model (a JSON array of
//!                           chat-completions response envelopes, replayed in
//!                           order — the shape of mini-swe-agent's
//!                           DeterministicModel, models/test_models.py)
//!   --model-url http://...  real OpenAI-compatible endpoint, http:// only
//!                           (no TLS without dependencies; meant for
//!                           llama.cpp / Ollama on localhost). Responses stream
//!                           by default (tokens render live); --no-stream falls
//!                           back to a single blocking request.

use std::io::{self, Read, Write as IoWrite};
use std::path::{Path, PathBuf};

use h5i_wasm_harness::agent::{Agent, Config, Effect, Event};
use h5i_wasm_harness::json::Value;
use h5i_wasm_harness::proto;

mod stream;
mod tools;

const TOOL_NAMES: [&str; 3] = ["read_file", "write_file", "list_dir"];

fn usage() -> ! {
    eprintln!(
        "usage: h5i-agent (--script replies.json | --model-url URL) [--task \"...\"] \\\n\
         \x20        [--workdir DIR] [--max-steps N] [--workspace-note \"...\"] \\\n\
         \x20        [--no-stream] [--bash] [--dump] [--trace]\n\
         \n\
         With no --task, h5i-agent is interactive: type a task per line; the agent keeps\n\
         the conversation across turns. Ctrl-D or 'exit' quits.\n\
         With --model-url, responses stream and tokens render live unless --no-stream."
    );
    std::process::exit(2);
}

struct Args {
    task: Option<String>,
    script: Option<PathBuf>,
    model_url: Option<String>,
    api_key: Option<String>,
    workdir: PathBuf,
    max_steps: u32,
    note: String,
    no_stream: bool,
    bash: bool,
    dump: bool,
    trace: bool,
}

fn parse_args() -> Args {
    let mut args = Args {
        task: None,
        script: None,
        model_url: None,
        api_key: None,
        workdir: PathBuf::from("."),
        max_steps: 20,
        note: String::from("a real directory on disk; changes persist"),
        no_stream: false,
        bash: false,
        dump: false,
        trace: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let val = |it: &mut dyn Iterator<Item = String>| it.next().unwrap_or_else(|| usage());
        match arg.as_str() {
            "--task" => args.task = Some(val(&mut it)),
            "--script" => args.script = Some(PathBuf::from(val(&mut it))),
            "--model-url" => args.model_url = Some(val(&mut it)),
            "--api-key" => args.api_key = Some(val(&mut it)),
            "--workdir" => args.workdir = PathBuf::from(val(&mut it)),
            "--max-steps" => args.max_steps = val(&mut it).parse().unwrap_or_else(|_| usage()),
            "--workspace-note" => args.note = val(&mut it),
            "--no-stream" => args.no_stream = true,
            "--bash" => args.bash = true,
            "--dump" => args.dump = true,
            "--trace" => args.trace = true,
            "-h" | "--help" => usage(),
            _ => usage(),
        }
    }
    if args.script.is_none() && args.model_url.is_none() {
        usage();
    }
    args
}

trait ModelHost {
    fn call(&mut self, request: &str) -> Event;
}

/// Replays canned response envelopes in order.
struct ScriptedModel {
    replies: Vec<Value>,
    next: usize,
}

impl ScriptedModel {
    fn load(path: &Path) -> Self {
        let text = std::fs::read_to_string(path).expect("script file readable");
        let parsed = h5i_wasm_harness::json::parse(&text).expect("script file is valid JSON");
        let Value::Arr(replies) = parsed else { panic!("script must be a JSON array") };
        ScriptedModel { replies, next: 0 }
    }
}

impl ModelHost for ScriptedModel {
    fn call(&mut self, _request: &str) -> Event {
        match self.replies.get(self.next) {
            Some(reply) => {
                self.next += 1;
                Event::ModelReply { body: reply.dump() }
            }
            None => Event::ModelFailed { status: 400, body: "mock script exhausted".into() },
        }
    }
}

/// Minimal HTTP/1.1 POST over TcpStream. http:// only — good enough for the
/// llama.cpp / Ollama localhost workflow, and keeps the binary dependency-free.
/// Streams by default (tokens render live); `stream = false` blocks for the
/// whole response.
struct HttpModel {
    url: String,
    api_key: Option<String>,
    stream: bool,
}

impl HttpModel {
    /// Resolve (host:port header value, path, connect address).
    fn target(&self) -> Result<(String, String, String), String> {
        let rest = self
            .url
            .strip_prefix("http://")
            .ok_or("only http:// URLs are supported (use a local model server)")?;
        let (host_port, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let addr = if host_port.contains(':') {
            host_port.to_string()
        } else {
            format!("{}:80", host_port)
        };
        Ok((host_port.to_string(), path.to_string(), addr))
    }

    fn send(&self, body: &str) -> Result<std::net::TcpStream, String> {
        let (host_port, path, addr) = self.target()?;
        let mut stream =
            std::net::TcpStream::connect(&addr).map_err(|e| format!("connect {}: {}", addr, e))?;
        let auth = match &self.api_key {
            Some(key) => format!("Authorization: Bearer {}\r\n", key),
            None => String::new(),
        };
        let request = format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
            path, host_port, auth, body.len(), body
        );
        stream.write_all(request.as_bytes()).map_err(|e| e.to_string())?;
        Ok(stream)
    }

    /// Non-streaming: read the whole response, return (status, body).
    fn post(&self, body: &str) -> Result<(u32, String), String> {
        let mut stream = self.send(body)?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response).map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&response);
        let status: u32 = text
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .ok_or("malformed HTTP response")?;
        let body_start = text.find("\r\n\r\n").map(|i| i + 4).unwrap_or(text.len());
        let head = &text[..body_start];
        let mut payload = text[body_start..].to_string();
        if head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
            payload = stream::decode_chunked(payload.as_bytes());
        }
        Ok((status, payload))
    }

    /// Streaming: request `stream: true`, render content tokens live, and
    /// reassemble the single envelope the core expects. Returns (status,
    /// envelope) for 2xx, or (status, error-body) otherwise.
    fn post_stream(&self, body: &str) -> Result<(u32, String), String> {
        // Ask the endpoint to stream, without disturbing anything the core set.
        let streamed = match h5i_wasm_harness::json::parse(body) {
            Ok(Value::Obj(mut pairs)) => {
                pairs.retain(|(k, _)| k != "stream");
                pairs.push(("stream".to_string(), Value::Bool(true)));
                Value::Obj(pairs).dump()
            }
            _ => body.to_string(),
        };
        let mut conn = self.send(&streamed)?;

        let mut raw = Vec::new();
        let mut buf = [0u8; 4096];
        let mut headers_done = false;
        let mut status = 0u32;
        let mut chunked = false;
        let mut is_2xx = false;
        let mut body_start = 0usize;
        let mut asm = stream::Assembler::new();
        let mut fed = 0usize;
        let mut done = false;

        loop {
            let n = conn.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            raw.extend_from_slice(&buf[..n]);
            if !headers_done {
                if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&raw[..pos]).to_ascii_lowercase();
                    status = String::from_utf8_lossy(&raw[..pos])
                        .split_whitespace()
                        .nth(1)
                        .and_then(|s| s.parse().ok())
                        .ok_or("malformed HTTP response")?;
                    chunked = head.contains("transfer-encoding: chunked");
                    is_2xx = (200..300).contains(&status);
                    body_start = pos + 4;
                    headers_done = true;
                } else {
                    continue;
                }
            }
            if is_2xx {
                let body_bytes = &raw[body_start..];
                let decoded = if chunked {
                    stream::decode_chunked(body_bytes)
                } else {
                    String::from_utf8_lossy(body_bytes).into_owned()
                };
                let events = stream::split_sse(&decoded);
                while fed < events.len() {
                    let ev = events[fed].clone();
                    fed += 1;
                    if ev == "[DONE]" {
                        done = true;
                        break;
                    }
                    asm.push(&ev, &mut |c| {
                        let mut so = io::stdout();
                        let _ = so.write_all(c.as_bytes());
                        let _ = so.flush();
                    });
                }
                if done {
                    break;
                }
            }
        }

        if is_2xx {
            if asm.rendered_content() {
                println!(); // end the streamed line
            }
            Ok((status, asm.into_envelope()))
        } else {
            let body_bytes = &raw[body_start..];
            let decoded = if chunked {
                stream::decode_chunked(body_bytes)
            } else {
                String::from_utf8_lossy(body_bytes).into_owned()
            };
            Ok((status, decoded))
        }
    }
}

impl ModelHost for HttpModel {
    fn call(&mut self, request: &str) -> Event {
        let result = if self.stream { self.post_stream(request) } else { self.post(request) };
        match result {
            Ok((status, body)) if (200..300).contains(&status) => Event::ModelReply { body },
            Ok((status, body)) => Event::ModelFailed { status, body },
            Err(e) => Event::ModelFailed { status: 0, body: e },
        }
    }
}

/// Run the agent from `first` to its `Done`, performing every effect against
/// the real filesystem / the model. Returns the final (status, result).
fn drive(
    agent: &mut Agent,
    mut effect: Effect,
    model: &mut dyn ModelHost,
    workdir: &Path,
    trace: bool,
) -> (String, String) {
    loop {
        match effect {
            Effect::Done { status, result } => return (status, result),
            Effect::CallModel { ref request } => {
                if trace {
                    eprintln!("[model call]");
                }
                let event = model.call(request);
                effect = agent.handle(event);
            }
            Effect::RunTool { ref call_id, ref name, args: ref tool_args } => {
                if trace {
                    eprintln!("[tool] {} {}", name, tool_args.dump());
                }
                let (ok, output) = match tools::run(workdir, name, tool_args) {
                    Ok(out) => (true, out),
                    Err(e) => (false, e),
                };
                let event = Event::ToolFinished { call_id: call_id.clone(), ok, output };
                effect = agent.handle(event);
            }
        }
    }
}

fn tool_names(bash: bool) -> Vec<String> {
    let mut names: Vec<String> = TOOL_NAMES.iter().map(|s| s.to_string()).collect();
    if bash {
        names.push("bash".to_string());
    }
    names
}

/// One-shot: run a single task and exit non-zero if it did not succeed.
/// When `streaming`, the answer was already rendered live, so only failures and
/// `--dump` add to stdout.
fn run_once(args: &Args, model: &mut dyn ModelHost, workdir: &Path, task: &str, streaming: bool) {
    let (mut agent, first) = Agent::start(
        task,
        &tool_names(args.bash),
        &args.note,
        Config { model: "host-configured".into(), max_steps: args.max_steps },
    )
    .expect("valid start parameters");
    let (status, result) = drive(&mut agent, first, model, workdir, args.trace);
    if args.dump {
        println!("{}", proto::dump_json(&agent));
    } else if streaming {
        if status != "success" {
            println!("status: {}", status);
            println!("{}", result);
        }
    } else {
        println!("status: {}", status);
        println!("{}", result);
    }
    if status != "success" {
        std::process::exit(1);
    }
}

/// Interactive REPL: read a task per line, run it, keep the conversation.
fn run_interactive(args: &Args, model: &mut dyn ModelHost, workdir: &Path, streaming: bool) {
    eprintln!("h5i-agent — interactive agent. workspace: {}", workdir.display());
    eprintln!("type a task and press enter; Ctrl-D or 'exit' to quit.");
    let stdin = io::stdin();
    let mut agent: Option<Agent> = None;
    loop {
        eprint!("\n\u{bb} ");
        let _ = io::stderr().flush();
        let mut line = String::new();
        match stdin.read_line(&mut line) {
            Ok(0) => {
                eprintln!();
                break;
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("input error: {}", e);
                break;
            }
        }
        let task = line.trim();
        if task.is_empty() {
            continue;
        }
        if matches!(task, "exit" | "quit" | ":q") {
            break;
        }

        // First task starts the agent; later ones continue the conversation.
        let first = if let Some(a) = agent.as_mut() {
            a.resume(task)
        } else {
            let (a, e) = Agent::start(
                task,
                &tool_names(args.bash),
                &args.note,
                Config { model: "host-configured".into(), max_steps: args.max_steps },
            )
            .expect("valid start parameters");
            agent = Some(a);
            e
        };

        let ag = agent.as_mut().expect("agent initialized above");
        let (status, result) = drive(ag, first, model, workdir, args.trace);
        if args.dump {
            println!("{}", proto::dump_json(ag));
        } else if streaming {
            // The answer already streamed live; surface only failures.
            if status != "success" {
                eprintln!("[{}]", status);
                println!("{}", result);
            }
        } else {
            if status != "success" {
                eprintln!("[{}]", status);
            }
            println!("{}", result);
        }
    }
}

fn main() {
    let args = parse_args();
    std::fs::create_dir_all(&args.workdir).expect("workdir creatable");
    let workdir = args.workdir.canonicalize().expect("workdir resolvable");

    if args.bash {
        eprintln!(
            "bash tool ENABLED — the model can run shell commands in {} (a real shell, not a sandbox)",
            workdir.display()
        );
    }

    // The scripted mock never streams; a real endpoint streams unless opted out.
    let streaming = args.model_url.is_some() && !args.no_stream;
    let mut model: Box<dyn ModelHost> = match (&args.script, &args.model_url) {
        (Some(path), _) => Box::new(ScriptedModel::load(path)),
        (None, Some(url)) => Box::new(HttpModel {
            url: url.clone(),
            api_key: args.api_key.clone(),
            stream: streaming,
        }),
        _ => usage(),
    };

    match &args.task {
        Some(task) => run_once(&args, model.as_mut(), &workdir, task, streaming),
        None => run_interactive(&args, model.as_mut(), &workdir, streaming),
    }
}
