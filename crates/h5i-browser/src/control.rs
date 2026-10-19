//! The session control channel: one JSON request per line, one JSON reply per
//! line. `requests`, `resend`, `socket`, `grpc` and the plugins speak to it; it
//! acts on the network layer the capture proxy shares, and holds no page.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

use base64::Engine as _;
use h5i_error::H5iError;
use serde_json::{json, Value};

use crate::broker::Broker;
use crate::receipt::ActionLog;
use crate::verbs::{Verb, VerbError};

/// How many distinct unknown verb names one session counts. The names come off
/// the wire, so the map is bounded.
const MAX_UNKNOWN_VERBS: usize = 64;

/// Where the control channel is advertised, and where the verbs are recorded.
#[derive(Default)]
pub struct ServeOptions {
    /// Where to advertise the control port. A CLI that reads it reaches this
    /// session's network layer rather than starting its own.
    pub control_file: Option<PathBuf>,
    /// Listen on a Unix socket at this path instead of a loopback port.
    pub control_socket: Option<PathBuf>,
    /// Where to record the verbs an agent asks for.
    pub action_log: Option<PathBuf>,
}

/// The network layer a control client acts on, and what it has been asked.
struct Session {
    broker: Arc<dyn Broker>,
    /// Where the agent's own verbs are recorded, when h5i asked for one. When
    /// it is set it is a precondition: see [`crate::receipt::ActionLog::begin`].
    actions: Option<ActionLog>,
    /// Verb names callers asked for that this session does not have, counted.
    unknown_verbs: BTreeMap<String, u64>,
}

/// Serve the control channel until the process ends. Verbs run one at a time,
/// so the receipts a verb causes are the ones between its marks in the log.
pub fn serve(broker: Arc<dyn Broker>, options: ServeOptions) -> Result<(), H5iError> {
    // Opened before the channel is advertised: a session that cannot record
    // what it is asked to do should fail at startup, where someone is watching,
    // rather than on the agent's first verb.
    let actions = match &options.action_log {
        Some(path) => Some(ActionLog::create(path)?),
        None => None,
    };
    let session = Arc::new(Mutex::new(Session {
        broker,
        actions,
        unknown_verbs: BTreeMap::new(),
    }));

    #[cfg(unix)]
    if let Some(path) = &options.control_socket {
        let listener = bind_control_socket(path)?;
        eprintln!("h5i: session control on {}", path.display());
        for incoming in listener.incoming() {
            let Ok(stream) = incoming else { continue };
            let session = session.clone();
            thread::spawn(move || {
                if let Err(error) = serve_control(stream, &session) {
                    eprintln!("h5i: control client: {error}");
                }
            });
        }
        let _ = std::fs::remove_file(path);
        return Ok(());
    }
    #[cfg(not(unix))]
    if let Some(path) = &options.control_socket {
        return Err(H5iError::Metadata(format!(
            "a Unix control socket ({}) is not available on this platform; the loopback \
             control port is the channel.",
            path.display()
        )));
    }

    // Bound before anything is advertised, so a client that finds the file
    // never finds a port nobody is listening on.
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| H5iError::Metadata(format!("could not bind a control port: {e}")))?;
    let port = local_port(&listener)?;
    if let Some(path) = &options.control_file {
        write_port_file(path, port)?;
    }
    eprintln!("h5i: session control on 127.0.0.1:{port}");
    for incoming in listener.incoming() {
        let Ok(stream) = incoming else { continue };
        let session = session.clone();
        thread::spawn(move || {
            if let Err(error) = serve_control(stream, &session) {
                eprintln!("h5i: control client: {error}");
            }
        });
    }
    if let Some(path) = &options.control_file {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

/// Bind a control socket, replacing a stale one a killed session left behind.
#[cfg(unix)]
fn bind_control_socket(path: &Path) -> Result<UnixListener, H5iError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| H5iError::with_path(e, parent))?;
    }
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path).map_err(|e| H5iError::with_path(e, path))?;
    // Whoever can connect to this socket can send requests as this session.
    restrict_to_owner(path)?;
    Ok(listener)
}

/// Make a session artifact readable, and connectable, only by its owner: the
/// channel is authority over the session, so it is not left to the umask.
#[cfg(unix)]
fn restrict_to_owner(path: &Path) -> Result<(), H5iError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|e| {
        // Fatal, and saying why: a socket anyone can connect to is authority
        // over the session.
        H5iError::Metadata(format!(
            "could not restrict `{}` to this user ({e}). It is the session's control \
             channel — whoever can open it can drive the browser and use its credentials — \
             so the session will not start rather than serve on one anyone can reach.",
            path.display()
        ))
    })
}

/// Windows has no mode bits to set, and inventing an ACL check here would be a
/// guess wearing the shape of a guarantee.
#[cfg(not(unix))]
fn restrict_to_owner(_path: &Path) -> Result<(), H5iError> {
    Ok(())
}

/// One control connection, over TCP or a Unix socket alike.
fn serve_control<S: ControlStream>(stream: S, session: &Mutex<Session>) -> Result<(), H5iError> {
    let mut writer = stream
        .dup()
        .map_err(|e| H5iError::Metadata(format!("could not clone the socket: {e}")))?;
    let reader = BufReader::new(stream);

    for line in reader.lines() {
        let line = line.map_err(H5iError::Io)?;
        if line.trim().is_empty() {
            continue;
        }
        let answer = match serde_json::from_str::<Value>(&line) {
            Ok(request) => {
                let mut session = session.lock().unwrap_or_else(|e| e.into_inner());
                recorded_verb(&mut session, &request)
            }
            Err(error) => VerbError::bad_request(format!(
                "the control channel takes one JSON object per line; this was not JSON: {error}"
            ))
            .reply(),
        };
        write_line(&mut writer, &answer)?;
    }
    Ok(())
}


/// A connection the control protocol can be spoken over.
trait ControlStream: Read + Write + Send + Sized + 'static {
    fn dup(&self) -> std::io::Result<Self>;
}

impl ControlStream for TcpStream {
    fn dup(&self) -> std::io::Result<Self> {
        self.try_clone()
    }
}

#[cfg(unix)]
impl ControlStream for UnixStream {
    fn dup(&self) -> std::io::Result<Self> {
        self.try_clone()
    }
}

/// Ask a session one thing over a Unix socket.
#[cfg(unix)]
pub fn ask_unix(path: &Path, request: &Value) -> Result<Value, H5iError> {
    let stream = UnixStream::connect(path).map_err(|e| {
        H5iError::Metadata(format!(
            "no session answering on {} ({e}). Start one with `h5i browser proxy <url>`.",
            path.display()
        ))
    })?;
    exchange(stream, request)
}

/// Read a port advertised in a file by [`write_port_file`].
pub fn read_port_file(path: &Path) -> Result<u16, H5iError> {
    let text = std::fs::read_to_string(path).map_err(|e| H5iError::with_path(e, path))?;
    text.trim()
        .parse()
        .map_err(|_| H5iError::Metadata(format!("{} does not hold a port", path.display())))
}

/// Ask a session one thing over its loopback port. A connection per request.
pub fn ask(port: u16, request: &Value) -> Result<Value, H5iError> {
    let stream = TcpStream::connect(("127.0.0.1", port)).map_err(|e| {
        H5iError::Metadata(format!(
            "no session answering on 127.0.0.1:{port} ({e}). Start one with \
             `h5i browser proxy <url>`."
        ))
    })?;
    exchange(stream, request)
}

/// Write one JSON value and its newline in one call. `writeln!` on a `Value`
/// writes each fragment separately, and the socket is unbuffered.
fn write_line<W: Write>(writer: &mut W, value: &Value) -> Result<(), H5iError> {
    let mut body = serde_json::to_string(value).map_err(|e| {
        H5iError::Metadata(format!("could not render a control message: {e}"))
    })?;
    body.push('\n');
    writer.write_all(body.as_bytes()).map_err(H5iError::Io)?;
    writer.flush().map_err(H5iError::Io)
}

/// One request, one answer, on a connection that is already open.
fn exchange<S: ControlStream>(stream: S, request: &Value) -> Result<Value, H5iError> {
    let mut writer = stream
        .dup()
        .map_err(|e| H5iError::Metadata(format!("could not clone the socket: {e}")))?;
    write_line(&mut writer, request)?;

    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(H5iError::Io)?;
    if line.trim().is_empty() {
        return Err(H5iError::Metadata(
            "the session closed without answering".into(),
        ));
    }
    serde_json::from_str(&line)
        .map_err(|e| H5iError::Metadata(format!("the session answered with non-JSON: {e}")))
}

/// Record a verb, run it, record how it went.
fn recorded_verb(session: &mut Session, request: &Value) -> Value {
    let verb = request.get("verb").and_then(Value::as_str).unwrap_or("");
    // Whatever the verb aims at, under one name, so a reader does not have to
    // know which verbs take a URL and which take a ref.
    let target = request
        .get("url")
        .or_else(|| request.get("ref"))
        .or_else(|| request.get("by"))
        .map(|v| match v.as_str() {
            Some(s) => s.to_string(),
            None => v.to_string(),
        });

    let Some(log) = &session.actions else {
        let reply = control_verb(session, request);
        return redact_reply(session.broker.as_ref(), reply);
    };

    let seq = match log.begin(verb, target.as_deref()) {
        Ok(seq) => seq,
        // No record, no action. The agent is told why in the same shape as any
        // other refusal, so this does not read as the verb having half-happened.
        Err(error) => {
            return VerbError::refused(format!(
                "refusing to act: the action could not be recorded: {error}"
            ))
            .reply()
        }
    };

    // Taken before the verb runs: every receipt past it is this verb's.
    let mark = session.broker.high_water();
    let outer = std::time::Instant::now();
    let t_begin = outer.elapsed();

    let answer = control_verb(session, request);
    let t_verb = outer.elapsed();

    let ok = answer.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let url = answer.get("url").and_then(Value::as_str);
    // `resend`'s refusals carry their reason as `message`.
    let error = answer
        .get("error")
        .or_else(|| answer.get("message"))
        .and_then(Value::as_str);
    let caused = session.broker.caused_since(mark);
    if let Some(log) = &session.actions {
        log.finish(
            seq,
            verb,
            target.as_deref(),
            crate::receipt::ActionOutcome {
                ok,
                url: url.map(str::to_string),
                error: error.map(str::to_string),
                requests: caused,
            },
        );
    }
    let t_finish = outer.elapsed();
    // Redacted after the action log is written, so the log keeps the session's
    // own account and only what leaves for the agent is scrubbed.
    let mut out = redact_reply(session.broker.as_ref(), answer);
    if timing_wanted()
        && let Some(map) = out.get_mut("timing_ms").and_then(Value::as_object_mut)
    {
        let ms = |d: std::time::Duration| json!(d.as_secs_f64() * 1000.0);
        map.insert("log_begin".into(), ms(t_begin));
        map.insert("verb".into(), ms(t_verb - t_begin));
        map.insert("log_finish".into(), ms(t_finish - t_verb));
        map.insert("redact".into(), ms(outer.elapsed() - t_finish));
        map.insert("recorded_total".into(), ms(outer.elapsed()));
    }
    out
}

/// Put credential placeholders back into anything on its way out.
fn redact_reply(broker: &dyn crate::broker::Broker, value: Value) -> Value {
    // Collect every string, redact them in one call, put them back. Skipped
    // when the broker holds nothing to redact.
    if !broker.has_redactions() {
        return value;
    }

    let mut texts: Vec<String> = Vec::new();
    collect_strings(&value, &mut texts);
    if texts.is_empty() {
        return value;
    }
    let mut redacted = broker.redact_all(&texts).into_iter();
    put_strings(value, &mut redacted)
}

fn collect_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => out.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|item| collect_strings(item, out)),
        // Values, not keys: a field *name* is this channel's own vocabulary, and
        // a secret that happened to look like one would be a stranger thing
        // than a leak.
        Value::Object(fields) => fields.values().for_each(|item| collect_strings(item, out)),
        _ => {}
    }
}

/// The same traversal, in the same order, putting each answer back where its
/// question came from.
fn put_strings(value: Value, redacted: &mut impl Iterator<Item = String>) -> Value {
    match value {
        Value::String(text) => Value::String(redacted.next().unwrap_or(text)),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| put_strings(item, redacted))
                .collect(),
        ),
        Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .map(|(name, item)| (name, put_strings(item, redacted)))
                .collect(),
        ),
        other => other,
    }
}

/// What this session has spent on the network, and against what.
fn budget_of(session: &Session) -> Value {
    let allowance = session.broker.budget();
    json!({
        "spent": allowance.spent,
        "max_requests": allowance.limits.max_requests,
        "max_wire_bytes": allowance.limits.max_wire_bytes,
    })
}

/// Whether a reply carries a breakdown of where its time went
/// (`H5I_BROWSER_TIMING`, read once).
fn timing_wanted() -> bool {
    static WANTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *WANTED.get_or_init(|| std::env::var_os("H5I_BROWSER_TIMING").is_some())
}

fn control_verb(session: &mut Session, request: &Value) -> Value {
    let name = request.get("verb").and_then(Value::as_str).unwrap_or("");
    let Some(verb) = Verb::from_name(name) else {
        // Counted before it is refused. A name nobody has is a request somebody
        // expected to work, and the count is the only record of that which does
        // not depend on them saying so.
        let key = one_line(name);
        if session.unknown_verbs.len() < MAX_UNKNOWN_VERBS
            || session.unknown_verbs.contains_key(&key)
        {
            *session.unknown_verbs.entry(key).or_insert(0) += 1;
        }
        return VerbError::unknown_verb(name).reply();
    };
    control_verb_inner(session, request, verb).0
}

/// A wire string, on one line and bounded, for a reply or a count key.
fn one_line(text: &str) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(80).collect()
}

fn control_verb_inner(session: &mut Session, request: &Value, verb: Verb) -> (Value, bool) {
    match verb {
        Verb::Status => (
            json!({
                "ok": true,
                "engine": "h5i-proxy",
                // How many, never which.
                "cookies": session.broker.cookie_count(),
                // Verbs callers asked for that this session does not have.
                "unknown_verbs": session.unknown_verbs,
                // What this session has spent on the network, and against what.
                "budget": budget_of(session),
                // The message store's health. A nonzero `errors` means the
                // stored evidence has a hole in it.
                "capture": session.broker.capture(),
            }),
            false,
        ),

        Verb::Resend => {
            let from = request.get("from").and_then(Value::as_u64);
            // A sequence number in this session's store, or a whole request
            // carried from another session (`--as`). `null` means absent.
            let given = request
                .get("request")
                .filter(|value| !value.is_null())
                .cloned();
            let Some(from) = from.or(given.is_some().then_some(0)) else {
                return (
                    json!({
                        "ok": false,
                        "code": "bad-request",
                        "message": "resend needs `from`: the sequence number of a stored request, as \
                                    `requests` lists them",
                    }),
                    false,
                );
            };
            let create = request.get("create").and_then(Value::as_bool).unwrap_or(false);
            // The allowance, started again because the agent asked
            // (`--reset-budget`): a long walk is many sends on purpose.
            if request
                .get("reset_budget")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                session.broker.reset_budget();
            }
            // Raw request bytes arrive base64-encoded; `repeat` is capped below.
            let raw_target = request
                .get("raw_target")
                .and_then(Value::as_str)
                .map(str::to_string);
            // Refused, never dropped: falling back to `None` sent an ordinary
            // framed request and reported its answer as the raw send's.
            let raw_request = match request.get("raw_request") {
                None | Some(Value::Null) => None,
                Some(value) => {
                    let decoded = value.as_str().and_then(|b| {
                        use base64::Engine as _;
                        base64::engine::general_purpose::STANDARD.decode(b).ok()
                    });
                    match decoded {
                        Some(bytes) => Some(bytes),
                        None => {
                            return (
                                json!({
                                    "ok": false,
                                    "code": "bad-raw",
                                    "message": "the raw request's bytes did not survive the \
                                                hop, and sending a framed request instead \
                                                would answer a different question",
                                }),
                                false,
                            );
                        }
                    }
                }
            };
            let plan = crate::broker::Sends {
                count: request
                    .get("repeat")
                    .and_then(Value::as_u64)
                    .unwrap_or(1)
                    .clamp(1, 1000) as u32,
                together: request
                    .get("together")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                no_follow: request
                    .get("no_follow")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                raw_target,
                raw_request,
                raw_headers: request
                    .get("raw_headers")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                // Two shapes, one walk. `{target, values}` is what `--set-each`
                // sends, and `{steps}` is the general form an experiment
                // expands to, where one send sets more than one target.
                each: request.get("each").and_then(|each| {
                    let walk = match each.get("steps").and_then(Value::as_array) {
                        Some(steps) => crate::broker::Each {
                            steps: steps
                                .iter()
                                .filter_map(|step| {
                                    let set: Vec<String> = step
                                        .get("set")?
                                        .as_array()?
                                        .iter()
                                        .filter_map(|v| v.as_str().map(str::to_string))
                                        .collect();
                                    (!set.is_empty()).then(|| crate::broker::Step {
                                        label: step
                                            .get("label")
                                            .and_then(Value::as_str)
                                            .unwrap_or_default()
                                            .to_string(),
                                        set,
                                    })
                                })
                                .collect(),
                        },
                        None => {
                            let target = each.get("target")?.as_str()?.to_string();
                            let values: Vec<String> = each
                                .get("values")?
                                .as_array()?
                                .iter()
                                .filter_map(|v| v.as_str().map(str::to_string))
                                .collect();
                            crate::broker::Each::over(&target, values)
                        }
                    };
                    (!walk.steps.is_empty()).then_some(walk)
                }),
                rate: request.get("rate").and_then(Value::as_f64),
            };
            let strings = |key: &str| -> Vec<String> {
                request
                    .get(key)
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| item.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let mut edits = Vec::new();
            for spec in strings("set") {
                match crate::edits::parse_set(&spec) {
                    Ok(edit) => edits.push(edit),
                    Err(e) => {
                        return (
                            json!({
                                "ok": false,
                                "code": "bad-edit",
                                "target": e.target,
                                "message": e.message,
                            }),
                            false,
                        );
                    }
                }
            }
            // Then the byte-valued ones, in the order they were given. After
            // the text edits rather than interleaved with them: two flags
            // cannot express one order, and saying which wins beats guessing.
            for pair in request
                .get("set_file")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default()
            {
                let target = pair.get(0).and_then(Value::as_str).unwrap_or_default();
                let encoded = pair.get(1).and_then(Value::as_str).unwrap_or_default();
                let bytes = match base64::engine::general_purpose::STANDARD.decode(encoded) {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        return (
                            json!({
                                "ok": false,
                                "code": "bad-edit",
                                "target": target,
                                "message": format!("its bytes did not survive the hop: {e}"),
                            }),
                            false,
                        );
                    }
                };
                match crate::edits::parse_set_bytes(target, bytes) {
                    Ok(edit) => edits.push(edit),
                    Err(e) => {
                        return (
                            json!({
                                "ok": false,
                                "code": "bad-edit",
                                "target": e.target,
                                "message": e.message,
                            }),
                            false,
                        );
                    }
                }
            }
            for spec in strings("unset") {
                match crate::edits::parse_unset(&spec) {
                    Ok(edit) => edits.push(edit),
                    Err(e) => {
                        return (
                            json!({
                                "ok": false,
                                "code": "bad-edit",
                                "target": e.target,
                                "message": e.message,
                            }),
                            false,
                        );
                    }
                }
            }

            // A composed request's body is base64 in the JSON. Each piece is
            // refused rather than defaulted when it does not read.
            let composed = match given {
                None => None,
                Some(value) => {
                    let text = |key: &str| {
                        value.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
                    };
                    let refuse = |what: &str| {
                        (
                            json!({
                                "ok": false,
                                "code": "bad-request",
                                "message": format!(
                                    "the request handed to `resend` {what}, and sending \
                                     something else in its place would answer a different \
                                     question"
                                ),
                            }),
                            false,
                        )
                    };
                    let body = match value.get("body_base64") {
                        None | Some(Value::Null) => Vec::new(),
                        Some(encoded) => {
                            let decoded = encoded.as_str().and_then(|b| {
                                use base64::Engine as _;
                                base64::engine::general_purpose::STANDARD.decode(b).ok()
                            });
                            match decoded {
                                Some(bytes) => bytes,
                                None => return refuse("has a body that did not survive the hop"),
                            }
                        }
                    };
                    let mut headers: Vec<(String, String)> = Vec::new();
                    match value.get("headers") {
                        None | Some(Value::Null) => {}
                        Some(Value::Array(items)) => {
                            for pair in items {
                                let read = pair.as_array().and_then(|pair| {
                                    Some((
                                        pair.first()?.as_str()?.to_string(),
                                        pair.get(1)?.as_str()?.to_string(),
                                    ))
                                });
                                match read {
                                    Some(pair) => headers.push(pair),
                                    None => {
                                        return refuse("has a header that is not a name and a value")
                                    }
                                }
                            }
                        }
                        Some(_) => return refuse("has headers that are not a list"),
                    }
                    let method = text("method");
                    if method.trim().is_empty() {
                        return refuse("names no method");
                    }
                    Some((method, text("url"), headers, body))
                }
            };

            let outcome = match composed {
                Some((method, target, headers, body)) => match url::Url::parse(&target) {
                    Err(e) => Err(crate::broker::SendError::new(
                        "bad-request",
                        format!("{target:?} is not a URL: {e}"),
                    )),
                    Ok(url) => session.broker.send_given(
                        crate::broker::Given {
                            method,
                            url,
                            headers,
                            body,
                        },
                        &edits,
                        create,
                        plan,
                    ),
                },
                None => session
                    .broker
                    .send_edited(from, &edits, create, plan),
            };
            match outcome {
                Err(why) => (
                    json!({"ok": false, "code": why.code, "message": why.message}),
                    false,
                ),
                Ok(edited) => {
                    let outcome = &edited.outcome;
                    // Include bounded previews; `show --raw` retains full bytes.
                    let body_preview = crate::broker::body_preview(&outcome.body);
                    let mut samples = serde_json::to_value(&edited.samples)
                        .expect("timing samples serialize");
                    if let Some(items) = samples.as_array_mut() {
                        for item in items {
                            if let Some(seq) = item.get("seq").and_then(Value::as_u64) {
                                item["request_id"] = Value::String(format!("req_{seq}"));
                                item["response_id"] = Value::String(format!("res_{seq}"));
                            }
                        }
                    }
                    (
                        json!({
                            "ok": outcome.error.is_none(),
                            // The new receipt, which is also where the replay's
                            // own request and response are stored. A replay is
                            // replayable.
                            "seq": edited.seq,
                            "request_id": edited.seq.map(|seq| format!("req_{seq}")),
                            "response_id": edited.seq.map(|seq| format!("res_{seq}")),
                            "applied": edited.applied,
                            "sent": edited.sent,
                            "samples": samples,
                            "response": {
                                "status": outcome.status,
                                "url": outcome.final_url.to_string(),
                                "headers": outcome.headers,
                                "bytes": outcome.body.len(),
                                "body_preview": body_preview,
                                "body_truncated": outcome.body.len() > crate::broker::BODY_PREVIEW_BYTES,
                                "error": outcome.error,
                            },
                        }),
                        false,
                    )
                }
            }
        }

        Verb::Socket => {
            let Some(url) = request.get("url").and_then(Value::as_str) else {
                return (
                    VerbError::bad_request(
                        "`socket` needs a `url`: the `ws://` or `wss://` endpoint to open",
                    )
                    .reply(),
                    false,
                );
            };
            let parsed = match url::Url::parse(url) {
                Ok(parsed) => parsed,
                Err(error) => {
                    return (
                        VerbError::bad_request(format!("`{url}` is not a URL: {error}")).reply(),
                        false,
                    );
                }
            };
            if !matches!(parsed.scheme(), "ws" | "wss") {
                return (
                    VerbError::bad_request(format!(
                        "`socket` opens `ws://` and `wss://`, and `{}` is neither. An \
                         `http://` endpoint is what `resend` is for",
                        parsed.scheme()
                    ))
                    .reply(),
                    false,
                );
            }

            let frames: Vec<String> = request
                .get("send")
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter().filter_map(Value::as_str).map(str::to_string).collect()
                })
                .unwrap_or_default();
            // Bounded: a socket that never answers is a result, not a hang.
            let listen = std::time::Duration::from_millis(
                request.get("wait_ms").and_then(Value::as_u64).unwrap_or(5_000).clamp(100, 60_000),
            );

            let channel = match session.broker.open_socket(&parsed, None) {
                Ok(channel) => channel,
                Err(error) => {
                    return (
                        VerbError::refused(format!("the socket did not open: {error}")).reply(),
                        false,
                    );
                }
            };
            for frame in &frames {
                if let Err(error) = channel.send(frame) {
                    channel.close();
                    return (
                        VerbError::refused(format!("the frame could not be sent: {error}"))
                            .reply(),
                        false,
                    );
                }
            }

            // Everything the peer said, up to the deadline. Drained in a loop
            // rather than once at the end, because a server that answers in
            // three frames should be reported as three.
            let mut received: Vec<Value> = Vec::new();
            let mut closed: Option<String> = None;
            let deadline = std::time::Instant::now() + listen;
            while std::time::Instant::now() < deadline {
                for event in channel.drain() {
                    match event {
                        crate::wsclient::Event::Open => {}
                        crate::wsclient::Event::Message(text) => {
                            received.push(json!({"text": text}));
                        }
                        crate::wsclient::Event::Named { name, data } => {
                            received.push(json!({"event": name, "text": data}));
                        }
                        crate::wsclient::Event::Closed(why) => closed = Some(why),
                        crate::wsclient::Event::Failed(why) => {
                            closed = Some(format!("the transport failed: {why}"));
                        }
                    }
                }
                if closed.is_some() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            channel.close();

            let mut reply = json!({
                "ok": true,
                "url": parsed.to_string(),
                "sent": frames.len(),
                "received": received,
            });
            if let Some(why) = closed {
                reply["closed"] = json!(why);
            }
            (reply, false)
        }

        // One gRPC call. The plugin frames the request and reads the reply; the
        // session only carries the bytes, under its policy.
        Verb::Grpc => {
            let Some(url) = request.get("url").and_then(Value::as_str) else {
                return (
                    VerbError::bad_request(
                        "`grpc` needs a `url`: the http(s) endpoint, e.g. http://host:50051",
                    )
                    .reply(),
                    false,
                );
            };
            let Some(path) = request.get("path").and_then(Value::as_str) else {
                return (
                    VerbError::bad_request("`grpc` needs a `path`: `/package.Service/Method`")
                        .reply(),
                    false,
                );
            };
            let body = match request.get("request_frame_b64").and_then(Value::as_str) {
                Some(b64) => {
                    use base64::Engine as _;
                    match base64::engine::general_purpose::STANDARD.decode(b64) {
                        Ok(bytes) => bytes,
                        Err(e) => {
                            return (
                                VerbError::bad_request(format!(
                                    "`request_frame_b64` is not base64: {e}"
                                ))
                                .reply(),
                                false,
                            );
                        }
                    }
                }
                None => Vec::new(),
            };
            let metadata: Vec<(String, String)> = request
                .get("metadata")
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter_map(|row| {
                            let pair = row.as_array()?;
                            Some((
                                pair.first()?.as_str()?.to_string(),
                                pair.get(1)?.as_str()?.to_string(),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let insecure = request.get("insecure").and_then(Value::as_bool).unwrap_or(false);
            let timeout = request
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .map(std::time::Duration::from_millis);
            let authority = request.get("authority").and_then(Value::as_str).map(str::to_string);

            let outcome = session.broker.grpc_call(crate::net::GrpcAsk {
                url: url.to_string(),
                path: path.to_string(),
                authority,
                body,
                metadata,
                insecure,
                timeout,
            });

            if let Some(reason) = outcome.denied {
                return (VerbError::refused(reason).reply(), false);
            }
            if let Some(error) = outcome.error {
                return (
                    VerbError::new(crate::verbs::Code::Internal, error).reply(),
                    false,
                );
            }
            use base64::Engine as _;
            let body_b64 = base64::engine::general_purpose::STANDARD.encode(&outcome.body);
            let mut reply = json!({
                "ok": true,
                "url": url,
                "path": path,
                "http_status": outcome.http_status,
                "grpc_status": outcome.grpc_status,
                "grpc_message": outcome.grpc_message,
                "response_frame_b64": body_b64,
                "request_id": outcome.seq.map(|seq| format!("req_{seq}")),
                "response_id": outcome.seq.map(|seq| format!("res_{seq}")),
            });
            if let Some(seq) = outcome.seq {
                reply["seq"] = json!(seq);
            }
            (reply, false)
        }

        Verb::Requests => {
            // `since`: only what happened after the caller's last look.
            let since = request.get("since").and_then(Value::as_u64);
            let mut rows = session.broker.records_since(since);

            // Filters are fields of the record, never a query language.
            let want_text = |key: &str| {
                request
                    .get(key)
                    .and_then(Value::as_str)
                    .map(str::to_ascii_lowercase)
            };
            let method = want_text("method");
            let initiator = want_text("initiator");
            let url_contains = want_text("url_contains");
            let status = request.get("status").and_then(Value::as_u64);
            let denied_only = request
                .get("denied_only")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let limit = request.get("limit").and_then(Value::as_u64);
            // A limit narrows as surely as a filter does, and says so.
            let mut narrowed = method.is_some()
                || initiator.is_some()
                || url_contains.is_some()
                || status.is_some()
                || denied_only;
            if narrowed {
                rows.retain(|row| {
                    if let Some(method) = &method
                        && !row.method.eq_ignore_ascii_case(method)
                    {
                        return false;
                    }
                    if let Some(url) = &url_contains
                        && !row.url.to_ascii_lowercase().contains(url)
                    {
                        return false;
                    }
                    if let Some(initiator) = &initiator {
                        let name = serde_json::to_value(row.initiator)
                            .ok()
                            .and_then(|v| v.as_str().map(str::to_string))
                            .unwrap_or_default();
                        if !name.eq_ignore_ascii_case(initiator) {
                            return false;
                        }
                    }
                    // A status filter selects response rows only.
                    if let Some(status) = status
                        && row.status != Some(status as u16)
                    {
                        return false;
                    }
                    if denied_only && row.allowed {
                        return false;
                    }
                    true
                });
            }
            // Last, so a limit means "the newest N of what I asked for" rather
            // than "the newest N, then filtered", which would silently answer
            // from a window the caller did not choose.
            if let Some(limit) = limit
                && rows.len() as u64 > limit
            {
                rows.drain(..rows.len() - limit as usize);
                narrowed = true;
            }
            // Counts over the whole log, not the window: "nothing was refused"
            // is a claim about the session.
            let summary = session.broker.log_summary();

            // Advertise only IDs backed by a healthy capture store.
            let capture_healthy = session
                .broker
                .capture()
                .is_some_and(|health| health.errors == 0);
            let addressable = |r: &crate::receipt::RequestRecord| {
                capture_healthy
                    && r.allowed
                    && (r.phase == crate::receipt::Phase::Request || r.status.is_some())
            };
            let message_id = |r: &crate::receipt::RequestRecord| match r.phase {
                crate::receipt::Phase::Request => format!("req_{}", r.seq),
                crate::receipt::Phase::Response => format!("res_{}", r.seq),
            };
            let text = rows
                .iter()
                .map(|r| {
                    let id = if addressable(r) {
                        message_id(r)
                    } else {
                        "—".into()
                    };
                    format!("{id:<10} {}", r.render())
                })
                .collect::<Vec<_>>()
                .join("\n");

            let addressed: Vec<Value> = rows
                .iter()
                .map(|row| {
                    let mut value = serde_json::to_value(row).expect("request record serializes");
                    value["addressable"] = Value::Bool(addressable(row));
                    value["id"] = if addressable(row) {
                        Value::String(message_id(row))
                    } else {
                        Value::Null
                    };
                    value
                })
                .collect();

            (
                json!({
                    "ok": true,
                    "requests": addressed,
                    // The cursor to pass back as `since`; see
                    // [`crate::broker::LogSummary::cursor`].
                    "cursor": summary.cursor,
                    "shown": rows.len(),
                    // Whether what came back is the window or a slice of it, so
                    // "two rows" is never read as "this session made two
                    // requests".
                    "narrowed": narrowed,
                    "total": summary.total,
                    "denied": summary.denied,
                    "text": text,
                }),
                false,
            )
        }

        Verb::Env => {
            let names = session.broker.secret_names();
            (
                json!({
                    "ok": true,
                    "names": names,
                    "prefix": crate::secrets::PREFIX,
                    "message": if names.is_empty() {
                        format!(
                            "no credentials are available to this session. Set one in the \
                             environment you open the session from, named `{}<SITE>_<FIELD>`, \
                             and type it into a field as `${}<SITE>_<FIELD>`.",
                            crate::secrets::PREFIX,
                            crate::secrets::PREFIX
                        )
                    } else {
                        format!(
                            "{} credential(s). Names only — no verb returns a value. Use one \
                             by typing its placeholder into a field.",
                            names.len()
                        )
                    },
                }),
                false,
            )
        }
    }
}

fn local_port(listener: &TcpListener) -> Result<u16, H5iError> {
    Ok(listener
        .local_addr()
        .map_err(|e| H5iError::Metadata(format!("could not read the bound address: {e}")))?
        .port())
}

fn write_port_file(path: &Path, port: u16) -> Result<(), H5iError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| H5iError::with_path(e, parent))?;
    }
    std::fs::write(path, port.to_string()).map_err(|e| H5iError::with_path(e, path))?;
    // The address of an unauthenticated channel: 0600, not the umask's 0644.
    restrict_to_owner(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::LocalBroker;
    use crate::policy::Policy;
    use crate::receipt::MemorySink;

    fn session() -> Session {
        let broker = LocalBroker::new(Policy::new().allow("app.test"), Arc::new(MemorySink::new()), None)
            .expect("broker builds");
        Session {
            broker,
            actions: None,
            unknown_verbs: BTreeMap::new(),
        }
    }

    #[test]
    fn status_answers_and_an_unknown_verb_is_counted() {
        let mut session = session();
        let reply = recorded_verb(&mut session, &json!({"verb": "status"}));
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(reply["engine"], "h5i-proxy");
        assert_eq!(reply["cookies"], 0);

        let reply = recorded_verb(&mut session, &json!({"verb": "snapshot"}));
        assert_eq!(reply["ok"], false);
        assert_eq!(reply["code"], "unknown-verb");
        assert!(
            reply["error"].as_str().unwrap().contains("agent-browser"),
            "a page verb names where the page is: {reply}"
        );
        let reply = recorded_verb(&mut session, &json!({"verb": "status"}));
        assert_eq!(reply["unknown_verbs"]["snapshot"], 1);
    }

    /// Every piece of a composed request was defaulted when it could not be
    /// read: an undecodable body went out empty, a missing method became a GET,
    /// and an undecodable `raw_request` fell through to the framed sender.
    #[test]
    fn a_composed_request_that_cannot_be_read_is_refused_rather_than_defaulted() {
        let mut session = session();
        let resend = |body: Value| {
            let mut request = json!({"verb": "resend", "from": 0});
            request["request"] = body;
            request
        };

        let reply = control_verb(
            &mut session,
            &resend(json!({
                "method": "POST",
                "url": "https://app.test/a",
                "headers": [],
                "body_base64": "not base64 !!",
            })),
        );
        assert_eq!(reply["ok"], false, "{reply}");
        assert_eq!(reply["code"], "bad-request");
        assert!(reply["message"].as_str().unwrap_or_default().contains("body"), "{reply}");

        let reply = control_verb(
            &mut session,
            &resend(json!({"url": "https://app.test/a", "headers": [], "body_base64": ""})),
        );
        assert_eq!(reply["ok"], false, "a request with no method: {reply}");
        assert!(reply["message"].as_str().unwrap_or_default().contains("method"), "{reply}");

        let reply = control_verb(
            &mut session,
            &resend(json!({
                "method": "GET",
                "url": "https://app.test/a",
                "headers": [["only-a-name"]],
            })),
        );
        assert_eq!(reply["ok"], false, "a half-written header: {reply}");
        assert!(reply["message"].as_str().unwrap_or_default().contains("header"), "{reply}");

        let reply = control_verb(
            &mut session,
            &json!({"verb": "resend", "from": 0, "raw_request": "not base64 !!"}),
        );
        assert_eq!(reply["ok"], false, "{reply}");
        assert_eq!(reply["code"], "bad-raw");
    }

    #[test]
    fn a_socket_that_is_not_a_socket_url_is_refused() {
        let mut session = session();
        let reply = control_verb(&mut session, &json!({"verb": "socket", "url": "https://app.test/"}));
        assert_eq!(reply["ok"], false);
        assert_eq!(reply["code"], "bad-request");
    }

    #[test]
    fn a_verb_that_cannot_be_recorded_does_not_happen() {
        // No record, no action.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("actions.jsonl");
        let mut session = session();
        session.actions = Some(ActionLog::unwritable_for_test(&path).expect("log"));

        let reply = recorded_verb(&mut session, &json!({"verb": "requests"}));
        assert_eq!(reply["ok"], false);
        assert!(
            reply["error"].as_str().unwrap().contains("could not be recorded"),
            "the agent is told why: {reply:?}"
        );
    }

    #[test]
    fn the_channel_answers_one_line_per_request() {
        let broker = LocalBroker::new(Policy::new(), Arc::new(MemorySink::new()), None)
            .expect("broker builds");
        let dir = tempfile::tempdir().expect("tempdir");
        let control_file = dir.path().join("control");
        let file = control_file.clone();
        thread::spawn(move || {
            serve(
                broker,
                ServeOptions {
                    control_file: Some(file),
                    ..Default::default()
                },
            )
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let port = loop {
            if let Ok(port) = read_port_file(&control_file) {
                break port;
            }
            assert!(std::time::Instant::now() < deadline, "the port was never advertised");
            thread::sleep(std::time::Duration::from_millis(10));
        };
        let reply = ask(port, &json!({"verb": "status"})).expect("answers");
        assert_eq!(reply["ok"], true, "{reply}");
        let reply = ask(port, &json!({"verb": "requests"})).expect("answers");
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(reply["total"], 0);
    }

    /// Whoever can connect to the control socket *is* the agent: they can send
    /// requests with this session's cookies and credentials.
    #[cfg(unix)]
    #[test]
    fn the_control_socket_is_connectable_only_by_its_owner() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("session.sock");
        // The laxest umask a process can inherit, so this proves the mode is
        // set rather than merely inherited from a tidy environment.
        let previous = unsafe { umask_for_test(0) };
        let listener = bind_control_socket(&path);
        unsafe { umask_for_test(previous) };
        listener.expect("the socket binds");

        let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the control socket is {mode:o}, not 0600");
    }

    /// And the port file, for the same reason one step removed: it holds the
    /// address of a channel that has no authentication of its own.
    #[cfg(unix)]
    #[test]
    fn a_port_file_is_readable_only_by_its_owner() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("session.control");
        let previous = unsafe { umask_for_test(0) };
        let written = write_port_file(&path, 4321);
        unsafe { umask_for_test(previous) };
        written.expect("the file is written");

        let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the port file is {mode:o}, not 0600");
        assert_eq!(read_port_file(&path).expect("reads"), 4321);
    }

    /// `umask`, without taking a dependency on `libc` for one call.
    #[cfg(unix)]
    unsafe fn umask_for_test(mask: u32) -> u32 {
        unsafe extern "C" {
            fn umask(mask: u32) -> u32;
        }
        unsafe { umask(mask) }
    }
}
