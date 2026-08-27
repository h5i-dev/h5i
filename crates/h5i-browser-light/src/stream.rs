//! The live view: the engine as something a human can watch.
//!
//! This speaks the wire format h5i's existing viewers already use, so
//! `h5i box view` and `h5i box view --term` work against this engine with no
//! change on their side: base64 JPEG frames in a JSON envelope, a `status`
//! message carrying the viewport so mouse coordinates mean something, and
//! `config`/`ack` pacing from the client.
//!
//! # Zero frames per second at rest
//!
//! The loop is driven by client messages, not by a timer. Tier 1 has no script,
//! so nothing changes on its own: a frame is produced when something *did*
//! change — a scroll that moved, a navigation that landed — and at rest the
//! process is idle rather than encoding identical JPEGs thirty times a second.
//! That falls out of the structure rather than being a special case, and it is
//! most of why an engine like this is cheap to leave open.
//!
//! `pacing: "ack"` (what the terminal viewer asks for) is satisfied by tracking
//! it per viewer: a viewer that owes an ack is marked dirty rather than sent a
//! second frame, and gets the newest one when its ack arrives.
//!
//! # One thread owns the page
//!
//! Everything here is arranged around a constraint the type system enforces
//! and no amount of design can wish away: **[`Page`] is not `Send`.** Blitz's
//! `BaseDocument` holds an `Arc<dyn HtmlParserProvider>` and a
//! `Box<dyn FontMetricsProvider>`, and neither is thread-safe, so there is no
//! `Arc<Mutex<Session>>` to be had — the obvious shape for "several viewers
//! plus a CLI share one page" does not compile.
//!
//! So the page has exactly one owner: [`run_session`], a loop on a single
//! thread. Viewers and control clients each get a thread that owns only its
//! socket, and they reach the page by sending it a [`Command`]. Replies and
//! frames travel back over channels, which carry only JSON and are `Send`.
//!
//! That constraint bought the right architecture. A session that several
//! things drive needs one serialisation point whether or not the DOM is
//! thread-safe, and this is it: there is no interleaving to reason about,
//! because a command is handled to completion before the next one starts.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread;

use base64::Engine as _;
use h5i_error::H5iError;
use serde_json::{json, Value};
use url::Url;

use crate::engine::{Page, PageFactory};
use crate::receipt::ActionLog;
use crate::verbs::{Verb, VerbError};
use crate::ws::{self, Incoming};

/// How far a key scrolls, as a fraction of the viewport.
/// How many distinct unknown verb names one session will remember.
///
/// The names come off the wire, so a caller looping over generated ones must
/// not be able to grow the map without limit. Past this the counts already
/// being kept still rise; new names are simply not added, which keeps the
/// useful signal — what real clients repeatedly ask for — and drops the noise.
const MAX_UNKNOWN_VERBS: usize = 64;

const PAGE_SCROLL: f64 = 0.9;
const LINE_SCROLL: f64 = 64.0;

pub struct ServeOptions {
    pub addr: String,
    pub quality: u8,
    /// Where to advertise the port. h5i's viewers find a stream by scanning
    /// `<env>/tmp/agent-browser/*.stream`, so writing one here is what makes
    /// this engine discoverable without changing the viewer.
    pub stream_file: Option<PathBuf>,
    /// Where to advertise the control port, which is what makes the session
    /// *resident*: a CLI that connects here drives the same page the viewers
    /// are watching, instead of rendering its own copy and exiting.
    ///
    /// Loopback TCP rather than a Unix socket, for two reasons. It is the same
    /// mechanism as the stream port, so there is one story about reachability
    /// rather than two; and it needs no `cfg(unix)`, which a Unix socket would
    /// bring to a crate that is otherwise portable. It grants nothing new
    /// either way: anything that can reach this port is already inside the box
    /// and could run the binary itself.
    pub control_file: Option<PathBuf>,
    /// Where to record the verbs an agent asks for. `None` on a bare host,
    /// where there is no console to feed.
    pub action_log: Option<PathBuf>,
    /// Serve one viewer and exit, which is what the tests and a one-shot
    /// demo want.
    pub once: bool,
    /// The in-memory half of the receipt sink, so the session can answer
    /// `requests` without reading back the file it just wrote.
    ///
    /// This is the same sink the fail-closed rule runs through, not a copy kept
    /// alongside it: what the verb reports is what the broker recorded, or the
    /// fetch did not happen.
    pub requests: Arc<crate::receipt::MemorySink>,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            addr: "127.0.0.1:0".to_string(),
            quality: 80,
            stream_file: None,
            control_file: None,
            action_log: None,
            once: false,
            requests: Arc::new(crate::receipt::MemorySink::new()),
        }
    }
}

/// What reaches the page's owning thread.
///
/// Every variant carries whatever the sender needs back, because the page
/// thread must never block waiting on a socket: it answers into a channel and
/// returns to the loop.
enum Command {
    /// A viewer connected. The reply channel is kept, not consumed.
    Join { id: u64, tx: Sender<Outgoing> },
    /// A viewer's socket ended, however it ended.
    Leave { id: u64 },
    /// One message from a viewer — input, pacing, or something to ignore.
    Viewer { id: u64, message: Value },
    /// One request from a control client, answered exactly once.
    Control { request: Value, reply: Sender<Value> },
}

/// What a connection thread writes to its own socket.
enum Outgoing {
    Text(String),
    Pong(Vec<u8>),
    Close,
}

/// A connected viewer, as the page thread sees it.
struct Viewer {
    tx: Sender<Outgoing>,
    /// The terminal viewer asks for `ack` pacing; the web viewer does not.
    ack_pacing: bool,
    /// A frame has been sent that this viewer has not acked yet.
    awaiting_ack: bool,
    /// The newest frame withheld while awaiting an ack. Held rather than
    /// queued: a viewer that fell behind wants the current page, not a replay
    /// of every scroll it missed.
    pending: Option<Value>,
}

impl Viewer {
    fn send(&self, message: &Value) {
        let _ = self.tx.send(Outgoing::Text(message.to_string()));
    }
}

/// A page plus the viewer state that surrounds it.
struct Session {
    factory: PageFactory,
    page: Page,
    quality: u8,
    seq: u64,
    /// Where the agent's own verbs are recorded, when h5i asked for one.
    ///
    /// Optional because the engine runs on a bare host too, where there is no
    /// console to feed and no claim to support. Inside a box it is set, and
    /// then it is a precondition: see [`crate::receipt::ActionLog::begin`].
    actions: Option<ActionLog>,

    /// The last reading served to a control client, so the next one can be
    /// expressed as its difference.
    ///
    /// Held per session rather than per client because there is one page: two
    /// clients asking for deltas against different baselines would be two
    /// answers about one thing.
    last_snapshot: Option<crate::snapshot::Snapshot>,

    /// Credentials this session may substitute on the way to the page.
    ///
    /// Read once at startup so a later `setenv` cannot widen what the agent can
    /// reach. See [`crate::secrets`] for why the namespace is narrower than
    /// `H5I_*`.
    secrets: crate::secrets::Secrets,

    /// The request log, live.
    ///
    /// See [`ServeOptions::requests`]. Held rather than reached through the
    /// broker because `Sink` is deliberately a one-method trait — `append` and
    /// nothing else — and widening it to be readable would weaken the thing
    /// that makes the guarantee simple to state.
    requests: Arc<crate::receipt::MemorySink>,

    /// The refs this session last handed to a control client.
    ///
    /// The other half of `last_snapshot`, kept separately because it answers a
    /// different question. `last_snapshot` is the baseline a delta is computed
    /// against; this is the evidence that an agent's `@e5` still means what the
    /// agent read. See [`resolve_ref`].
    served_refs: Option<Vec<crate::snapshot::RefEntry>>,
    /// Verb names callers asked for that this session does not have, counted.
    ///
    /// Free telemetry on the gap between what this engine offers and what the
    /// things driving it expect, and the only source of that fact which does
    /// not depend on somebody filing a report. Lightpanda keeps the same
    /// counter for CDP methods (`cdp_unknown_commands`) and it is the sharpest
    /// item in its metrics: the published conformance list says what is
    /// honestly absent, and this says which absences anyone actually hits.
    ///
    /// Names only, and only names that failed to resolve — a verb this session
    /// *has* is never counted, so nothing here describes what an agent did with
    /// the page. Reported by `status`.
    unknown_verbs: std::collections::BTreeMap<String, u64>,
    /// What this session has done, in a form that can be run again.
    ///
    /// In memory, like the cookie jar and for the same reason: it names the
    /// fields a login used, and a file is exactly where that should not
    /// accumulate on its own. `session script` hands it over when asked, and
    /// the caller decides whether to keep it.
    recording: crate::replay::Recording,

    /// Whether a human is typing a credential right now.
    ///
    /// While this is set every control verb that reads the page is refused —
    /// see [`Session::login_refusal`]. The viewer keeps streaming, because the
    /// human doing the typing has to see what they are typing, and that is the
    /// limit of the mode: the viewer socket is inside the box, where there is
    /// no privilege boundary, so an agent that goes looking can attach to it
    /// and watch the same frames. ROADMAP §5.10 specified withholding frames
    /// *and* snapshots; only the snapshot half is built, and the refusal text
    /// says so rather than implying the other half.
    login: bool,
}

impl Session {
    /// Why a read was refused while a human is logging in.
    ///
    /// The whole point of the mode: a credential typed into a page the agent
    /// can snapshot has been handed to the agent. Refusing the *read* is what
    /// makes "log in for me" a thing a person can reasonably do, and it is
    /// deliberately not a refusal of the session — the page still works, the
    /// jar still fills, and everything resumes when the human says so.
    ///
    /// The message says what is refused rather than that the page is unreadable.
    /// It is not: frames still go to the live view, by design, and the viewer
    /// socket is in the box. Claiming otherwise here would be the one thing
    /// this project says it does not do.
    fn login_refusal(verb: crate::verbs::Verb) -> Value {
        json!({
            "ok": false,
            "code": crate::verbs::Code::LoginMode.as_str(),
            "retryable": false,
            "error": format!(
                "`{}` is refused while this session is in login mode: a credential typed \
                 into a page the agent can read has been given to the agent. The live view \
                 still streams — the person typing has to see the page — so this refuses the \
                 control path and not an agent that attaches to the viewer socket itself. End \
                 login mode with `session login --off` and reads resume, with whatever session \
                 the login established still in the jar.",
                verb.name()
            ),
            "login": true,
            "frames_withheld": false,
        })
    }

    fn viewport(&self) -> (u32, u32) {
        let options = self.factory.options();
        (options.width, options.height)
    }

    fn status_message(&self) -> Value {
        let (width, height) = self.viewport();
        json!({
            "type": "status",
            "connected": true,
            "screencasting": true,
            "viewportWidth": width,
            "viewportHeight": height,
            // Named honestly. A viewer that assumes Chromium semantics from
            // this string should find out here rather than from a missing
            // feature later.
            "engine": "h5i-browser-light",
        })
    }

    fn url_message(&self) -> Value {
        json!({"type": "url", "url": self.page.url().to_string()})
    }

    fn frame_message(&mut self) -> Result<Value, H5iError> {
        let jpeg = self.page.screenshot_jpeg(self.quality)?;
        self.seq += 1;
        Ok(json!({
            "type": "frame",
            "seq": self.seq,
            "data": base64::engine::general_purpose::STANDARD.encode(&jpeg),
        }))
    }

    /// Follow a link, replacing the page. A failed navigation leaves the
    /// current page in place and reports itself, because a viewer that goes
    /// blank on a denied link is indistinguishable from one that crashed.
    fn navigate(&mut self, url: &Url) -> Result<Vec<Value>, H5iError> {
        match self.factory.open(url) {
            Ok(page) => {
                self.page = page;
                Ok(vec![self.url_message()])
            }
            Err(error) => Ok(vec![json!({
                "type": "page_error",
                "text": format!("navigation to {url} failed: {error}"),
            })]),
        }
    }
}

/// Serve the live view and the control channel until the session ends.
///
/// The calling thread becomes the page's owner ([`run_session`]); the two
/// listeners get accept threads of their own. That is the only arrangement
/// available, because `page` cannot be moved to another thread — see the
/// module docs.
pub fn serve(factory: PageFactory, page: Page, options: ServeOptions) -> Result<(), H5iError> {
    let viewers = TcpListener::bind(&options.addr)
        .map_err(|e| H5iError::Metadata(format!("could not bind {}: {e}", options.addr)))?;
    let port = local_port(&viewers)?;

    // Bound before anything is advertised, so a client that finds one file and
    // then the other never finds a port nobody is listening on.
    let control = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| H5iError::Metadata(format!("could not bind a control port: {e}")))?;
    let control_port = local_port(&control)?;

    if let Some(path) = &options.stream_file {
        write_port_file(path, port)?;
    }
    if let Some(path) = &options.control_file {
        write_port_file(path, control_port)?;
    }
    eprintln!("h5i-browser-light: live view on 127.0.0.1:{port}");
    eprintln!("h5i-browser-light: session control on 127.0.0.1:{control_port}");

    let (tx, rx) = channel::<Command>();

    let viewer_tx = tx.clone();
    let once = options.once;
    thread::spawn(move || accept_viewers(viewers, viewer_tx, once));
    thread::spawn(move || accept_control(control, tx));

    // Opened before the listeners are advertised: a session that cannot record
    // what it is asked to do should fail at startup, where someone is watching,
    // rather than on the agent's first verb.
    let actions = match &options.action_log {
        Some(path) => Some(ActionLog::create(path)?),
        None => None,
    };

    let session = Session {
        factory,
        page,
        quality: options.quality,
        seq: 0,
        actions,
        last_snapshot: None,
        served_refs: None,
        unknown_verbs: std::collections::BTreeMap::new(),
        recording: crate::replay::Recording::default(),
        requests: options.requests.clone(),
        secrets: crate::secrets::Secrets::from_env(),
        login: false,
    };
    run_session(session, rx, options.once);

    for path in [&options.stream_file, &options.control_file].into_iter().flatten() {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

fn local_port(listener: &TcpListener) -> Result<u16, H5iError> {
    Ok(listener
        .local_addr()
        .map_err(|e| H5iError::Metadata(format!("could not read the bound address: {e}")))?
        .port())
}

/// The page's owning thread: the one place a [`Session`] is touched.
///
/// Returns when every channel into it has closed, or — under `once` — when the
/// first viewer has come and gone.
fn run_session(mut session: Session, rx: Receiver<Command>, once: bool) {
    let mut viewers: HashMap<u64, Viewer> = HashMap::new();
    let mut had_viewer = false;

    while let Ok(command) = rx.recv() {
        match command {
            Command::Join { id, tx } => {
                let viewer = Viewer {
                    tx,
                    ack_pacing: false,
                    awaiting_ack: false,
                    pending: None,
                };
                // The viewport has to arrive before frames, or the viewer maps
                // mouse coordinates against its 1280x720 default and clicks
                // land elsewhere.
                viewer.send(&session.status_message());
                viewer.send(&session.url_message());
                viewers.insert(id, viewer);
                had_viewer = true;

                match session.frame_message() {
                    Ok(frame) => send_frame(viewers.get_mut(&id).expect("just inserted"), frame),
                    Err(error) => {
                        eprintln!("h5i-browser-light: could not render the first frame: {error}")
                    }
                }
            }

            Command::Leave { id } => {
                viewers.remove(&id);
                if once && had_viewer && viewers.is_empty() {
                    break;
                }
            }

            Command::Viewer { id, message } => {
                // An ack is about one viewer's pacing, so it is answered here
                // rather than in `handle`, which speaks for the whole session.
                if message.get("type").and_then(Value::as_str) == Some("ack") {
                    if let Some(viewer) = viewers.get_mut(&id) {
                        viewer.awaiting_ack = false;
                        if let Some(frame) = viewer.pending.take() {
                            send_frame(viewer, frame);
                        }
                    }
                    continue;
                }
                if message.get("type").and_then(Value::as_str) == Some("config")
                    && let Some(viewer) = viewers.get_mut(&id)
                {
                    viewer.ack_pacing =
                        message.get("pacing").and_then(Value::as_str) == Some("ack");
                }

                match handle(&mut session, &message) {
                    Ok(out) => dispatch(&mut viewers, id, out),
                    Err(error) => {
                        eprintln!("h5i-browser-light: {error}");
                    }
                }
            }

            Command::Control { request, reply } => {
                let (answer, changed) = recorded_verb(&mut session, &request);
                let _ = reply.send(answer);
                // A control verb that moved the page is exactly the case the
                // resident session exists for: every viewer sees what the
                // agent did, rather than the page the server happened to open.
                if changed {
                    broadcast_change(&mut session, &mut viewers);
                }
            }
        }
    }
}

/// Route what `handle` produced.
///
/// A frame is a fact about the session, so it reaches every viewer; anything
/// else answers the viewer that asked. The distinction matters for `config`,
/// whose frame is a courtesy to one arriving client and would be an unasked-for
/// frame for everybody else.
fn dispatch(viewers: &mut HashMap<u64, Viewer>, actor: u64, out: Vec<Value>) {
    for message in out {
        if message.get("type").and_then(Value::as_str) == Some("frame") {
            for viewer in viewers.values_mut() {
                send_frame(viewer, message.clone());
            }
        } else if let Some(viewer) = viewers.get(&actor) {
            viewer.send(&message);
        }
    }
}

/// Render one frame and give it to everyone watching.
fn broadcast_change(session: &mut Session, viewers: &mut HashMap<u64, Viewer>) {
    if viewers.is_empty() {
        // Nobody is watching, so nothing is encoded. The "zero frames per
        // second at rest" property has to survive the control channel, or a
        // headless agent driving the page pays for JPEGs no one sees.
        return;
    }
    let url = session.url_message();
    match session.frame_message() {
        Ok(frame) => {
            for viewer in viewers.values_mut() {
                viewer.send(&url);
                send_frame(viewer, frame.clone());
            }
        }
        Err(error) => eprintln!("h5i-browser-light: could not render a frame: {error}"),
    }
}

/// Send a frame, or hold it if this viewer still owes an ack.
fn send_frame(viewer: &mut Viewer, frame: Value) {
    if viewer.ack_pacing && viewer.awaiting_ack {
        viewer.pending = Some(frame);
        return;
    }
    viewer.send(&frame);
    if viewer.ack_pacing {
        viewer.awaiting_ack = true;
    }
}

/// Accept viewers until the listener dies, one thread each.
fn accept_viewers(listener: TcpListener, tx: Sender<Command>, once: bool) {
    let mut next_id = 0u64;
    for incoming in listener.incoming() {
        let Ok(stream) = incoming else { continue };
        next_id += 1;
        let id = next_id;
        let tx = tx.clone();
        thread::spawn(move || {
            if let Err(error) = serve_viewer(id, stream, &tx) {
                // A viewer that closed its tab is not an error worth reporting
                // as one; the session outlives it either way.
                eprintln!("h5i-browser-light: viewer {id} disconnected: {error}");
            }
            let _ = tx.send(Command::Leave { id });
        });
        if once {
            break;
        }
    }
}

/// One viewer: a reader on this thread, a writer on another.
///
/// Split because both directions are blocking and the page thread must never
/// wait on either. The writer thread is the only thing that touches the socket
/// for output, which is what makes "several actors, one socket" safe without a
/// lock around the stream.
fn serve_viewer(id: u64, mut stream: TcpStream, tx: &Sender<Command>) -> Result<(), H5iError> {
    ws::accept(&mut stream)?;

    let (out_tx, out_rx) = channel::<Outgoing>();
    let writer = stream
        .try_clone()
        .map_err(|e| H5iError::Metadata(format!("could not clone the socket: {e}")))?;
    let pump = thread::spawn(move || write_outgoing(writer, out_rx));

    tx.send(Command::Join { id, tx: out_tx.clone() })
        .map_err(|_| H5iError::Metadata("the session ended".into()))?;

    let mut reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|e| H5iError::Metadata(format!("could not clone the socket: {e}")))?,
    );

    let result = loop {
        match ws::read_message(&mut reader) {
            Ok(Incoming::Close) | Err(_) => break Ok(()),
            Ok(Incoming::Pong) => continue,
            Ok(Incoming::Ping(payload)) => {
                let _ = out_tx.send(Outgoing::Pong(payload));
            }
            Ok(Incoming::Text(text)) => {
                let Ok(message) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                if tx.send(Command::Viewer { id, message }).is_err() {
                    break Ok(());
                }
            }
        }
    };

    let _ = out_tx.send(Outgoing::Close);
    drop(out_tx);
    let _ = pump.join();
    result
}

/// The only writer on a viewer's socket.
fn write_outgoing(mut stream: TcpStream, rx: Receiver<Outgoing>) {
    while let Ok(message) = rx.recv() {
        let sent = match message {
            Outgoing::Text(text) => ws::send_text(&mut stream, &text),
            Outgoing::Pong(payload) => ws::send_pong(&mut stream, &payload),
            Outgoing::Close => break,
        };
        if sent.is_err() {
            break;
        }
    }
}

/// Accept control clients: one JSON request per line, one JSON reply per line.
///
/// Line-delimited rather than a framed protocol because the client is a CLI
/// that connects, asks one thing and leaves, and a format a human can produce
/// with `nc` is a format they can debug with `nc`.
fn accept_control(listener: TcpListener, tx: Sender<Command>) {
    for incoming in listener.incoming() {
        let Ok(stream) = incoming else { continue };
        let tx = tx.clone();
        thread::spawn(move || {
            if let Err(error) = serve_control(stream, &tx) {
                eprintln!("h5i-browser-light: control client: {error}");
            }
        });
    }
}

fn serve_control(stream: TcpStream, tx: &Sender<Command>) -> Result<(), H5iError> {
    let mut writer = stream
        .try_clone()
        .map_err(|e| H5iError::Metadata(format!("could not clone the socket: {e}")))?;
    let reader = BufReader::new(stream);

    for line in reader.lines() {
        let line = line.map_err(H5iError::Io)?;
        if line.trim().is_empty() {
            continue;
        }
        let answer = match serde_json::from_str::<Value>(&line) {
            Ok(request) => {
                let (reply_tx, reply_rx) = channel::<Value>();
                if tx.send(Command::Control { request, reply: reply_tx }).is_err() {
                    return Err(H5iError::Metadata("the session ended".into()));
                }
                reply_rx
                    .recv()
                    .unwrap_or_else(|_| {
                        VerbError::new(
                            crate::verbs::Code::Internal,
                            "the session ended before it answered",
                        )
                        .reply()
                    })
            }
            Err(error) => VerbError::bad_request(format!(
                "the control channel takes one JSON object per line; this was not JSON: {error}"
            ))
            .reply(),
        };
        writeln!(writer, "{answer}").map_err(H5iError::Io)?;
        writer.flush().map_err(H5iError::Io)?;
    }
    Ok(())
}


/// Read a port advertised in a file by [`write_port_file`].
pub fn read_port_file(path: &Path) -> Result<u16, H5iError> {
    let text = std::fs::read_to_string(path).map_err(|e| H5iError::with_path(e, path))?;
    text.trim()
        .parse()
        .map_err(|_| H5iError::Metadata(format!("{} does not hold a port", path.display())))
}

/// Ask a resident session one thing, and read its answer.
///
/// A connection per request. The session is a long-lived thing but a CLI verb
/// is not, and a pool would buy nothing on a loopback socket except a second
/// lifetime to get wrong.
pub fn ask(port: u16, request: &Value) -> Result<Value, H5iError> {
    let stream = TcpStream::connect(("127.0.0.1", port)).map_err(|e| {
        H5iError::Metadata(format!(
            "no session answering on 127.0.0.1:{port} ({e}). Start one with \
             `h5i-browser-light serve <url>`."
        ))
    })?;
    let mut writer = stream
        .try_clone()
        .map_err(|e| H5iError::Metadata(format!("could not clone the socket: {e}")))?;
    writeln!(writer, "{request}").map_err(H5iError::Io)?;
    writer.flush().map_err(H5iError::Io)?;

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

/// Add one step to the replay recording, if this verb belongs in one.
///
/// Called only after a verb succeeded, and only from [`recorded_verb`], so
/// there is one place where "did it" and "recorded it for replay" can drift.
///
/// The handle is rewritten on the way in. A caller that named a `@ref` gets its
/// **verified selector** looked up now, while the reading that minted it is
/// still the current one; a caller that named a selector already has the
/// durable form. Where no selector can be verified the step is dropped and
/// counted, because a handle that resolves elsewhere is worse than no handle.
fn record_step(session: &mut Session, request: &Value) {
    let Some(verb) = request
        .get("verb")
        .and_then(Value::as_str)
        .and_then(crate::verbs::Verb::from_name)
    else {
        return;
    };
    if !verb.is_recorded() {
        return;
    }

    // Where the session was when it started doing things, so a replay does not
    // have to be told separately.
    session
        .recording
        .start_at(session.page.url().as_ref());

    let mut step = crate::replay::Step {
        verb: verb.name().to_string(),
        ..Default::default()
    };

    if verb.needs_ref() {
        let Some(entry) = durable_handle(session, request) else {
            session.recording.drop_step();
            return;
        };
        step.selector = Some(entry.0);
        step.named = Some(entry.1);
    }

    match verb {
        crate::verbs::Verb::Navigate => {
            step.url = request
                .get("url")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        crate::verbs::Verb::Scroll => {
            step.by = request.get("by").and_then(Value::as_i64);
        }
        crate::verbs::Verb::Type => {
            // The text **as the caller wrote it**, which for a credential is
            // the placeholder. `request` is the pre-substitution request; the
            // resolved value never reaches this function and must not.
            step.text = request
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        _ => {}
    }

    session.recording.push(step);
}

/// The durable selector for whatever this request aimed at, and what it was
/// called.
///
/// `None` when there is no verified selector for it, which is a real outcome:
/// generated markup with no stable attribute and no unique ancestor chain does
/// not always yield one.
fn durable_handle(session: &Session, request: &Value) -> Option<(String, String)> {
    // A selector the caller supplied is already the durable form.
    if let Some(selector) = request.get("selector").and_then(Value::as_str) {
        return Some((selector.to_string(), String::new()));
    }
    let reference = request.get("ref").and_then(Value::as_str)?;
    let wanted = reference.trim_start_matches('@');
    let entry = session
        .served_refs
        .as_ref()?
        .iter()
        .find(|e| e.id == wanted)?;
    let dom = session.page.dom();
    let doc = dom.borrow();
    let selector = crate::selector::for_node(&doc, entry.node_id)?;
    Some((selector, crate::snapshot::one_line(&entry.name)))
}

/// Record a verb, run it, record how it went.
///
/// Wrapped around [`control_verb`] rather than folded into it so the verbs stay
/// testable without a file, and so there is exactly one place where "acted" and
/// "recorded" can drift apart — this one.
///
/// The pane this feeds says *agent actions*, and until now it could only ever
/// be filled by the mediated socket in front of agent-browser. There is no such
/// socket here — the engine is the browser — so before this the console showed
/// an empty pane for a session an agent was actively driving, which reads as
/// "the agent did nothing" and is the one thing a monitoring surface must never
/// say by accident.
fn recorded_verb(session: &mut Session, request: &Value) -> (Value, bool) {
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
        let (reply, changed) = control_verb(session, request);
        if reply.get("ok").and_then(Value::as_bool) == Some(true) {
            record_step(session, request);
        }
        return (redact_reply(&session.secrets, reply), changed);
    };

    let seq = match log.begin(verb, target.as_deref()) {
        Ok(seq) => seq,
        // No record, no action. The agent is told why in the same shape as any
        // other refusal, so this does not read as the verb having half-happened.
        Err(error) => {
            return (
                VerbError::refused(format!(
                    "refusing to act: the action could not be recorded: {error}"
                ))
                .reply(),
                false,
            )
        }
    };

    let (answer, changed) = control_verb(session, request);

    let ok = answer.get("ok").and_then(Value::as_bool).unwrap_or(false);
    if ok {
        record_step(session, request);
    }
    let url = answer.get("url").and_then(Value::as_str);
    let error = answer.get("error").and_then(Value::as_str);
    // Which receipts this verb produced, read back out of the reply it just
    // returned. The engine already stamped the link; this is what carries it
    // into the log the console reads.
    let caused: Vec<u64> = answer
        .get("requests")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.get("seq").and_then(Value::as_u64))
                .collect()
        })
        .unwrap_or_default();
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
    // Redacted after the action log is written, so the log keeps the engine's
    // own account and only what leaves for the agent is scrubbed.
    (redact_reply(&session.secrets, answer), changed)
}

/// Put credential placeholders back into anything on its way out.
///
/// [`crate::secrets`] describes this as the rule that anything written back out
/// goes through — and until now nothing called it, which made the module's own
/// claim false. It is applied here, at the one point every reply passes through,
/// rather than at each site that might echo something.
///
/// This is not only tidiness. A login form that reflects what was typed — into
/// a hidden field, a validation message, a page title — puts the value into the
/// DOM, and the next `snapshot` or `markdown` would carry it back to the agent
/// the indirection exists to keep it from. Scanning the reply catches that
/// wherever it comes from.
///
/// The cost is a string scan per reply against a handful of values, on a path
/// that has already done a policy check and a layout pass.
fn redact_reply(secrets: &crate::secrets::Secrets, value: Value) -> Value {
    match value {
        Value::String(text) => Value::String(secrets.redact(&text)),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| redact_reply(secrets, item))
                .collect(),
        ),
        Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .map(|(name, item)| (name, redact_reply(secrets, item)))
                .collect(),
        ),
        other => other,
    }
}

/// Handle one control request against the resident page.
///
/// Returns the reply and whether the page moved, because the caller is the only
/// thing that knows who else is watching.
fn control_verb(session: &mut Session, request: &Value) -> (Value, bool) {
    let name = request.get("verb").and_then(Value::as_str).unwrap_or("");
    let Some(verb) = crate::verbs::Verb::from_name(name) else {
        // Counted before it is refused. A name nobody has is a request somebody
        // expected to work, and the count is the only record of that which does
        // not depend on them saying so.
        //
        // Bounded, because the name came off the wire: a caller looping over
        // generated names must not be able to grow this without limit.
        if session.unknown_verbs.len() < MAX_UNKNOWN_VERBS
            || session.unknown_verbs.contains_key(name)
        {
            *session
                .unknown_verbs
                .entry(crate::snapshot::one_line(name))
                .or_insert(0) += 1;
        }
        return (VerbError::unknown_verb(name).reply(), false);
    };

    // Reads are refused while a human is typing a credential, and which verbs
    // those are is a property of the verb rather than a list kept here. See
    // `Verb::readable_during_login`: the allowlist used to be two string
    // literals, where a typo widened it silently.
    if session.login && !verb.readable_during_login() {
        return (Session::login_refusal(verb), false);
    }

    // A read verb given a `url` goes there first, so "look at this page" is one
    // round trip rather than a `navigate` whose reply an agent reads only to
    // send the next request. Which verbs may do this is a property of the verb
    // (`Verb::navigates_first`), not a check remembered at each call site.
    //
    // The navigation happens *before* the verb runs and reports its own
    // refusal, because a policy denial reached this way is the same fact it
    // would have been on its own and must not be dressed up as an empty page.
    let mut navigated = false;
    if verb.navigates_first()
        && let Some(target) = request.get("url").and_then(Value::as_str)
    {
        match navigate_to(session, target) {
            Ok(()) => navigated = true,
            Err(reply) => return (reply, false),
        }
    }

    let (mut reply, moved) = control_verb_inner(session, request, verb);
    if navigated {
        // Where it actually ended up, which a redirect may have changed. An
        // agent that fused the navigation into the read still gets the one
        // fact the separate `navigate` reply would have given it.
        if let Some(object) = reply.as_object_mut() {
            object
                .entry("url")
                .or_insert_with(|| json!(session.page.url().to_string()));
        }
    }
    (reply, moved || navigated)
}

/// Go to a URL, resolved against the page the session is on.
///
/// Shared by `navigate` and by the read verbs that fuse one in, so the two
/// cannot resolve a relative URL differently or report a refusal differently.
fn navigate_to(session: &mut Session, target: &str) -> Result<(), Value> {
    // Resolved against the current page, so an agent may say `/docs` for the
    // same reason a person may click one.
    let resolved = match session.page.url().join(target) {
        Ok(url) => url,
        Err(error) => {
            return Err(
                VerbError::bad_request(format!("`{target}` is not a URL: {error}")).reply(),
            );
        }
    };
    match session.factory.open(&resolved) {
        Ok(page) => {
            session.page = page;
            // The refs the caller last read describe a page this session is no
            // longer on. Dropping them here is what makes a fused navigation
            // safe: without it a `@ref` from before would be checked against a
            // reading of a different document, and `same_target` could match by
            // coincidence — same ordinal, same role, same href — on a page the
            // agent has never seen.
            session.served_refs = None;
            Ok(())
        }
        // A refusal is an answer, not a crash: the allowlist saying no is the
        // engine working, and the agent needs to read it as one.
        Err(error) => Err(VerbError::refused(format!("{error}")).reply()),
    }
}

fn control_verb_inner(
    session: &mut Session,
    request: &Value,
    verb: crate::verbs::Verb,
) -> (Value, bool) {
    match verb {
        // What the session is, for a client that just connected.
        Verb::Status => (
            json!({
                "ok": true,
                "url": session.page.url().to_string(),
                "engine": "h5i-browser-light",
                // How many, never which. An agent can see that it is logged in
                // without being able to read the credential that makes it so,
                // which is what keeps a stolen snapshot worth less than a
                // stolen jar.
                "cookies": session.factory.broker().jar().len(),
                "login": session.login,
                "open_sockets": session.page.open_sockets(),
                // What was asked for and does not exist. Empty on almost every
                // session, and when it is not it is the most useful line here:
                // it names the gap between what this engine offers and what
                // whatever is driving it expected, without anyone having to
                // file a report.
                "unknown_verbs": session.unknown_verbs,
            }),
            false,
        ),

        // Hand the page to the human for as long as it takes to log in.
        //
        // §B10 of ROADMAP.md listed this as overdue rather than pending: it
        // was supposed to arrive with the cookie jar, because a jar is what
        // makes logging in worth doing and a readable page is what makes it
        // unsafe.
        Verb::Login => {
            let on = request.get("on").and_then(Value::as_bool).unwrap_or(true);
            session.login = on;
            // The baseline is dropped either way. A delta across a login would
            // describe the page a human just used, which is the one thing this
            // mode exists to keep out of the agent's hands.
            //
            // And the served refs with it. They carry the id, role and *name*
            // of every actionable element from the pre-login reading; leaving
            // them would let a ref minted before the login be honoured after
            // it, and would let a `stale-ref` message quote page state the
            // agent never read.
            session.last_snapshot = None;
            session.served_refs = None;
            (
                json!({
                    "ok": true,
                    "login": on,
                    "cookies": session.factory.broker().jar().len(),
                    "message": if on {
                        "login mode is on: the page is no longer readable by the agent, and the \
                         live view is yours. Type the credential there, then end this mode."
                    } else {
                        "login mode is off: the page is readable again, and whatever session \
                         the login established is in the jar. The count is all that is \
                         reported; no cookie value is readable through this engine."
                    },
                }),
                false,
            )
        }

        Verb::Snapshot => {
            let snapshot = session.page.snapshot();
            let wants_delta = request.get("delta").and_then(Value::as_bool).unwrap_or(false);

            // The difference, when one was asked for and there is a baseline to
            // difference against. Three hundred lines re-read after every click
            // — of which four are new — is the wrong shape for an agent loop.
            let delta = wants_delta
                .then(|| session.last_snapshot.as_ref().map(|prev| snapshot.delta(prev)))
                .flatten();

            // A live socket is the one thing here that makes two reads of one
            // page disagree without the agent having acted. Reported, because
            // the determinism this engine claims elsewhere genuinely does not
            // hold for a page holding one.
            let sockets = session.page.open_sockets();
            let mut reply = json!({
                "ok": true,
                "url": session.page.url().to_string(),
                // Stated rather than inferred: a caller that wants the
                // machine-readable form should not have to parse prose out
                // of the outline to learn the page had not finished.
                "settled": session.page.settled().map(|s| s.render()),
                "script": session.page.has_script(),
            });

            match delta {
                // A delta of a page that was replaced is as long as the page,
                // so the full outline is the smaller answer. Which one this is
                // is stated rather than left to be guessed from the shape.
                Some(delta) if !delta.replaced => {
                    reply["kind"] = json!("delta");
                    reply["text"] = json!(delta.render());
                    reply["delta"] = serde_json::to_value(&delta).unwrap_or(Value::Null);
                }
                Some(_) | None if wants_delta => {
                    reply["kind"] = json!("full");
                    reply["text"] = json!(snapshot.render());
                    reply["reason"] = json!(
                        "a delta was asked for; the page changed too much for one to be \
                         shorter than the outline, or there was no earlier reading to \
                         compare against"
                    );
                }
                _ => {
                    reply["kind"] = json!("full");
                    reply["text"] = json!(snapshot.render());
                }
            }

            // The durable handle beside the ordinal one.
            //
            // Computed here and *only* here: the action verbs take their own
            // internal captures to get a live node id, and paying for a
            // selector on each of those would put a per-ref tree walk on every
            // click. An agent reads a snapshot once per turn and acts several
            // times, so this is the right side of that trade.
            let selectors = {
                let dom = session.page.dom();
                let doc = dom.borrow();
                // One cache for the whole reading. Every candidate is verified
                // with a full-document query, and refs that sit near each other
                // share nearly all of their ancestor segments, so the same
                // query was being run once per ref that shared it. The cache
                // lives exactly as long as this borrow: the document cannot
                // change underneath it, which is the only thing that would make
                // a remembered answer wrong.
                let mut cache = crate::selector::Cache::new();
                snapshot
                    .refs
                    .iter()
                    .map(|entry| {
                        json!({
                            "id": entry.id,
                            "role": entry.role,
                            "name": entry.name,
                            // Absent rather than guessed when none could be
                            // verified: a selector that resolves elsewhere is
                            // worse than no selector, because it looks like a
                            // handle.
                            "selector": crate::selector::for_node_cached(
                                &doc, entry.node_id, &mut cache,
                            ),
                        })
                    })
                    .collect::<Vec<_>>()
            };
            reply["refs"] = json!(selectors);
            if sockets > 0 {
                reply["open_sockets"] = json!(sockets);
                reply["note"] = json!(format!(
                    "this page holds {sockets} open socket(s). Messages arrive on real time and \
                     are delivered when a verb runs, so two reads of this page can differ \
                     without you having done anything — unlike every other page here."
                ));
            }

            // What the agent is about to hold refs from. `resolve_ref` checks
            // the next `@ref` against this, which is the whole staleness story:
            // a ref is only honoured against the reading it was minted in.
            session.served_refs = Some(snapshot.refs.clone());
            session.last_snapshot = Some(snapshot);
            (reply, false)
        }

        // Scrolling is the one thing a viewer could do that an agent could not,
        // which made "look further down the page" a request only a human could
        // make. `moved` is reported rather than assumed: a scroll at the bottom
        // of a document changes nothing, and an agent that cannot tell will
        // loop asking for more page that does not exist.
        Verb::Scroll => {
            let by = request.get("by").and_then(Value::as_f64).unwrap_or(0.0);
            let moved = session.page.scroll_by(0.0, by);
            let (_, offset) = session.page.scroll_offset();
            (
                json!({
                    "ok": true,
                    "moved": moved,
                    "offset": offset,
                    "content_height": session.page.content_height(),
                }),
                moved,
            )
        }

        Verb::Navigate => {
            let Some(target) = request.get("url").and_then(Value::as_str) else {
                return (VerbError::bad_request("`navigate` needs a `url`.").reply(), false);
            };
            match navigate_to(session, target) {
                Ok(()) => (json!({"ok": true, "url": session.page.url().to_string()}), true),
                Err(reply) => (reply, false),
            }
        }

        // Typing and submitting are the pair that make a login reachable: a
        // session an agent cannot type into stops at the first form, so these
        // ship together or neither is worth having.
        Verb::Type => {
            let aim = match aim_of(request, Verb::Type) {
                Ok(aim) => aim,
                Err(e) => return (e.reply(), false),
            };
            let shown = aim.shown();
            let reference = shown.as_str();
            let Some(text) = request.get("text").and_then(Value::as_str) else {
                return (VerbError::bad_request("`type` needs `text`.").reply(), false);
            };
            // The one place a credential is resolved, guarded by the predicate
            // rather than by this call site remembering to ask.
            let resolved = if Verb::Type.substitutes_secrets() {
                session.secrets.substitute(text)
            } else {
                crate::secrets::Resolved {
                    text: text.to_string(),
                    used: Vec::new(),
                    missing: Vec::new(),
                }
            };
            let text = resolved.text.as_str();
            let snapshot = session.page.snapshot();
            let entry = match resolve_aim(session, &snapshot, &aim) {
                Ok(entry) => entry,
                Err(e) => return (e.reply(), false),
            };
            let node_id = entry.node_id;
            let role = entry.role.clone();
            if !session.page.type_into(node_id, text) {
                return (
                    VerbError::wrong_role(reference, &role, "a field to type into").reply(),
                    false,
                );
            }
            // Names, never values. A receipt that carried the credential would
            // be a credential in every export that receipt reaches, which is
            // the same rule the cookie jar follows by reporting a count.
            let mut reply = json!({"ok": true, "ref": reference});
            if resolved.substituted() {
                reply["used"] = json!(resolved.used);
            }
            if !resolved.missing.is_empty() {
                // Said rather than left to be discovered as a failed login that
                // looks like a wrong password.
                reply["unresolved"] = json!(resolved.missing);
                reply["note"] = json!(format!(
                    "{} placeholder(s) named nothing set in this session and were typed \
                     literally. `env` lists what is available.",
                    resolved.missing.len()
                ));
            }
            (reply, true)
        }

        Verb::Submit => {
            let aim = match aim_of(request, Verb::Submit) {
                Ok(aim) => aim,
                Err(e) => return (e.reply(), false),
            };
            let snapshot = session.page.snapshot();
            let entry = match resolve_aim(session, &snapshot, &aim) {
                Ok(entry) => entry,
                Err(e) => return (e.reply(), false),
            };
            let node_id = entry.node_id;
            let submission = match session.page.submit_form(node_id) {
                Ok(submission) => submission,
                Err(error) => return (VerbError::refused(format!("{error}")).reply(), false),
            };
            match session.factory.open_submission(&submission) {
                Ok(page) => {
                    session.page = page;
                    (
                        json!({
                            "ok": true,
                            "url": session.page.url().to_string(),
                            "method": submission.method,
                        }),
                        true,
                    )
                }
                Err(error) => (VerbError::refused(format!("{error}")).reply(), false),
            }
        }

        Verb::Click => {
            let aim = match aim_of(request, Verb::Click) {
                Ok(aim) => aim,
                Err(e) => return (e.reply(), false),
            };
            let shown = aim.shown();
            let reference = shown.as_str();
            let snapshot = session.page.snapshot();
            let entry = match resolve_aim(session, &snapshot, &aim) {
                Ok(entry) => entry,
                Err(e) => return (e.reply(), false),
            };
            let node_id = entry.node_id;
            let role = entry.role.clone();
            let href = entry.href.clone();

            // With script running, a click is an event before it is a
            // navigation. A page that handles the click and calls
            // `preventDefault` never wanted the href followed, and a button
            // with no href is only clickable at all because of its handler.
            if session.page.has_script()
                && let Some(caused) = session.page.dispatch_event(node_id, "click")
            {
                let settled = session
                    .page
                    .settled()
                    .map(|s| s.render())
                    .unwrap_or_default();
                if href.is_none() {
                    return (
                        json!({
                            "ok": true,
                            "ref": reference,
                            "settled": settled,
                            // The causal link, stamped by the one component
                            // that knows it: this click, these receipts.
                            "requests": caused,
                        }),
                        true,
                    );
                }
            }

            let Some(href) = href else {
                return (
                    VerbError::wrong_role(reference, &role, "something to follow").reply(),
                    false,
                );
            };
            let resolved = match session.page.url().join(&href) {
                Ok(url) => url,
                Err(error) => {
                    return (
                        VerbError::bad_request(format!("`{href}` is not a URL: {error}")).reply(),
                        false,
                    );
                }
            };
            match session.factory.open(&resolved) {
                Ok(page) => {
                    session.page = page;
                    (json!({"ok": true, "url": session.page.url().to_string()}), true)
                }
                Err(error) => (VerbError::refused(format!("{error}")).reply(), false),
            }
        }


        // The verb no other engine can offer honestly.
        //
        // Chromium's request list is an *observation* of the network made from
        // beside it, and it fails open: attach races, freshly created targets,
        // workers, buffer limits. Obscura's CDP `Network.*` events are batched
        // and emitted after navigation completes, reconstructed from a stored
        // list, so anything reading them live sees a compressed, out-of-time
        // picture. Lightpanda has no equivalent at all.
        //
        // Here the engine *is* the HTTP client, so this is not a report about
        // the network — it is the decision record the broker wrote before the
        // bytes moved. If it is not here, it did not happen.
        Verb::Requests => {
            let all = session.requests.records();

            // `since` lets an agent ask what happened after its last look, the
            // same shape `snapshot --delta` has and for the same reason: the
            // whole log re-read after every click is the wrong size for a loop.
            let since = request.get("since").and_then(Value::as_u64);
            let rows: Vec<&crate::receipt::RequestRecord> = all
                .iter()
                .filter(|r| since.is_none_or(|floor| r.seq > floor))
                .collect();

            // Counted over the *whole* log rather than the window, because
            // "nothing was refused" is a claim about the session and an agent
            // that only ever asks for windows should still be able to make it.
            let denied = all
                .iter()
                .filter(|r| r.phase == crate::receipt::Phase::Request && !r.allowed)
                .count();

            let text = rows
                .iter()
                .map(|r| r.render())
                .collect::<Vec<_>>()
                .join("\n");
            // The highest seq, not the last appended. Sequence numbers are
            // taken before the append, and a socket reader thread appends
            // concurrently with the page's own fetches — so append order and
            // seq order can differ, and `last()` would either re-show a row or
            // skip one permanently. A hole in a log this verb documents as
            // complete by construction is the worst of the two.
            let highest = all.iter().map(|r| r.seq).max();

            (
                json!({
                    "ok": true,
                    "requests": rows,
                    // The cursor to pass back as `since`. Named rather than
                    // left to be derived from the last row, which is absent
                    // when the window is empty.
                    "cursor": highest,
                    "shown": rows.len(),
                    "total": all.len(),
                    "denied": denied,
                    "text": text,
                }),
                false,
            )
        }

        // The wait an agent loop needs. Before this the only option on a
        // scripted page was to snapshot and hope.
        //
        // Neither reference engine can do the interesting half of this. Both
        // wait on a wall clock with hard-coded fudge — Lightpanda a 500ms
        // network-idle debounce, Obscura a 150ms quiet window, a 1s grace, a
        // 500ms tail and a 5s deadline that marks the page idle even when the
        // deadline is what ended it. Here the settle runs on a virtual clock,
        // so a page's `setTimeout(1000)` costs nothing and two runs of the same
        // page answer the same way.
        Verb::WaitFor => {
            let selector = request.get("selector").and_then(Value::as_str);
            let text = request.get("text").and_then(Value::as_str);
            let target = match (selector, text) {
                (Some(s), None) => crate::engine::WaitTarget::Selector(s.to_string()),
                (None, Some(t)) => crate::engine::WaitTarget::Text(t.to_string()),
                (Some(_), Some(_)) => {
                    return (
                        VerbError::bad_request(
                            "`wait_for` takes a `selector` or a `text`, not both: two conditions \
                             are two waits, and which one was met would not be reported.",
                        )
                        .reply(),
                        false,
                    );
                }
                (None, None) => {
                    return (
                        VerbError::bad_request("`wait_for` needs a `selector` or a `text`.")
                            .reply(),
                        false,
                    );
                }
            };

            let waited = session.page.wait_for(&target);
            // `changed` only when something actually happened: a condition
            // already true has moved nothing, and sending every attached viewer
            // a frame for it would undo the zero-frames-at-rest property.
            //
            // `waited.changed` is first because it is the only one that sees a
            // socket message: those arrive on real time, advancing neither the
            // virtual clock nor the timer count, so a wait satisfied by one
            // left every viewer showing the page from before it.
            let moved = waited.changed
                || waited.settled.elapsed_ms > 0
                || waited.settled.timers_run > 0;
            (
                json!({
                    "ok": true,
                    "met": waited.met,
                    // Four outcomes, not two. "Not found and the page has
                    // nothing left to run" is a different fact from "not found
                    // and it was still working", which is different again from
                    // "not found and the only thing left is a loop that will
                    // never converge". An agent branches differently on each.
                    "end": waited.end.as_str(),
                    "waited_ms": waited.settled.elapsed_ms,
                    "pending_timers": waited.settled.pending_timers,
                    "periodic_timers": waited.settled.periodic_timers,
                    "message": waited.render(),
                }),
                moved,
            )
        }

        Verb::WaitForScript => {
            let Some(expr) = request.get("expr").and_then(Value::as_str) else {
                return (
                    VerbError::bad_request("`wait_for_script` needs an `expr` to evaluate.")
                        .reply(),
                    false,
                );
            };
            let Some(waited) = session.page.wait_for_script(expr) else {
                return (VerbError::no_script(Verb::WaitForScript).reply(), false);
            };
            let moved = waited.changed
                || waited.settled.elapsed_ms > 0
                || waited.settled.timers_run > 0;
            (
                json!({
                    "ok": true,
                    "met": waited.met,
                    "end": waited.end.as_str(),
                    "waited_ms": waited.settled.elapsed_ms,
                    "pending_timers": waited.settled.pending_timers,
                    "periodic_timers": waited.settled.periodic_timers,
                    "message": waited.render(),
                }),
                moved,
            )
        }

        // Token economics. Three hundred lines of outline to find five titles
        // is the wrong shape, and a model asked to transcribe them out of prose
        // will occasionally invent one.
        Verb::Extract => {
            let Some(spec) = request.get("schema") else {
                return (
                    VerbError::bad_request(
                        "`extract` needs a `schema`: an object of field names to selectors, \
                         like {\"title\": \"h1\", \"links\": [\"a\"]}.",
                    )
                    .reply(),
                    false,
                );
            };
            let schema = match crate::extract::parse(spec) {
                Ok(schema) => schema,
                Err(e) => return (e.reply(), false),
            };
            let base = session.page.url().clone();
            let dom = session.page.dom();
            let result = {
                let doc = dom.borrow();
                crate::extract::run(&doc, &base, &schema)
            };
            match result {
                // In-band, so a model reads it and corrects itself. A schema
                // that matched nothing is the caller's to fix; answering it
                // with an object full of nulls would look like a result.
                Err(e) => (e.reply(), false),
                Ok(data) => (json!({"ok": true, "data": data}), false),
            }
        }

        Verb::Markdown => {
            let max_bytes = request
                .get("max_bytes")
                .and_then(Value::as_u64)
                .map(|n| n as usize)
                .unwrap_or(crate::markdown::DEFAULT_MAX_BYTES);
            let dom = session.page.dom();
            let rendered = {
                let doc = dom.borrow();
                crate::markdown::capture(&doc, max_bytes)
            };
            let url = session.page.url().to_string();
            (
                json!({
                    "ok": true,
                    "url": url,
                    "truncated": rendered.truncated,
                    // Fenced, because this is page content reaching something
                    // that is deciding what to do next.
                    "text": rendered.render(&url),
                }),
                false,
            )
        }

        // Discovery for the credential scheme, and the whole of it: an agent
        // learns that a credential exists and what to call it, which is
        // everything it needs to use one and nothing it needs to leak one.
        // What this session did, as something that can be run again.
        //
        // Made of selectors, not refs: an ordinal names a position in the
        // reading that minted it, so a script full of them replays into a
        // different page. See `crate::replay`.
        Verb::Script => (
            json!({
                "ok": true,
                "start_url": session.recording.start_url,
                "steps": session.recording.steps,
                // Named, because a script shorter than the session it came
                // from is a fact the person running it needs.
                "dropped": session.recording.dropped,
                "script": session.recording.render(),
            }),
            false,
        ),

        // What the page says about *itself*, in the formats it already
        // publishes for the purpose.
        //
        // The cheapest of the three reads by a wide margin: an outline is the
        // page's content and costs hundreds of lines, markdown is denser and
        // still prose, and this is a few hundred bytes the page has already
        // written down. A model asked to pull a headline out of prose will
        // occasionally invent one; handed `"headline": "…"` it will not.
        Verb::Structured => {
            let found = {
                let dom = session.page.dom();
                let doc = dom.borrow();
                crate::structured::capture(&doc, session.page.url())
            };
            // Fenced like every other page-derived reading, because that is
            // what it is: `og:title` is attacker-controlled on an attacker's
            // page exactly as a heading is.
            let mut reply = json!({
                "ok": true,
                "url": session.page.url().to_string(),
                "empty": found.is_empty(),
                "structured": found,
            });
            if found.is_empty() {
                // A result, not a failure, and said as one. Plenty of pages
                // publish no metadata, and "this page says nothing about
                // itself" is a different answer from "the read went wrong" —
                // reporting the first as the second is what ends a
                // self-correction loop instead of prompting it.
                reply["note"] = json!(
                    "this page publishes no metadata of its own. That is a fact about the \
                     page, not a failed read: use `markdown` or `snapshot` to see what it \
                     does contain."
                );
            }
            (reply, false)
        }

        Verb::Env => {
            let names = session.secrets.names();
            (
                json!({
                    "ok": true,
                    "names": names,
                    "prefix": crate::secrets::PREFIX,
                    "message": if names.is_empty() {
                        format!(
                            "no credentials are available to this session. Set one in the \
                             environment `serve` was started in, named `{}<SITE>_<FIELD>`, and \
                             type it into a field as `${}<SITE>_<FIELD>`.",
                            crate::secrets::PREFIX,
                            crate::secrets::PREFIX
                        )
                    } else {
                        format!(
                            "{} credential(s). Names only — no verb in this engine returns a \
                             value. Use one by typing its placeholder into a field.",
                            names.len()
                        )
                    },
                }),
                false,
            )
        }
    }
}

/// Resolve a `@ref`, refusing one that no longer means what the agent read.
///
/// `click`, `type` and `submit` each need a *live* node id, so each takes a
/// fresh snapshot at action time. That much is right, and on its own it was the
/// bug: refs are minted by walk order (`snapshot.rs`, `e1` is "the first
/// actionable thing in this walk"), so if the page moved between the snapshot
/// the agent read and the one taken here, `@e5` resolves to a **different
/// element**, the action succeeds, and the reply says `ok`.
///
/// Nothing detected that. There was no memory-safety problem — the node id is
/// freshly minted, so the click landed on a real node — and that is exactly
/// what made it bad: a plausible wrong answer that looks like a right one is
/// the state this engine says it does not leave things in.
///
/// So the fresh capture is checked against the refs this session last *served*.
/// An identical entry — same id, same node, same role, same name — means the
/// reading the agent acted on still describes the page.
///
/// **What this proves, and what it does not.** It is an equality check on one
/// ref, not a proof that the document is unchanged: a page that mutates
/// something the walk does not record still passes, and two different elements
/// that agree on all four fields would too. What it catches is every case where
/// the *handle* has come to mean something else, which is the failure that was
/// silent before. It is not a claim that the page is the same page.
/// How a caller named the element an action is aimed at.
///
/// Two handle types, deliberately, and the difference is the whole of §B15.4.
/// A `@ref` is a position in the reading that minted it: cheap, checked against
/// that reading, and meaningless anywhere else. A selector is a handle that
/// survives the reading — and survives a *navigation*, which is what makes a
/// recorded session replayable at all.
#[derive(Debug, Clone)]
enum Aim {
    /// `@e3`, checked against the reading it came from.
    Ref(String),
    /// A CSS selector, resolved with `querySelector` semantics.
    Selector(String),
}

impl Aim {
    /// How to name this in an error message.
    fn shown(&self) -> String {
        match self {
            Aim::Ref(reference) => reference.clone(),
            Aim::Selector(selector) => format!("`{selector}`"),
        }
    }
}

/// Read whichever handle the caller used.
///
/// `ref` and `selector` are alternatives, not both: a request carrying each
/// would be a request whose author did not know which one they meant, and
/// picking one silently is how a replay ends up acting somewhere its script did
/// not say.
fn aim_of(request: &Value, verb: crate::verbs::Verb) -> Result<Aim, VerbError> {
    let reference = request.get("ref").and_then(Value::as_str);
    let selector = request.get("selector").and_then(Value::as_str);
    match (reference, selector) {
        (Some(_), Some(_)) => Err(VerbError::bad_request(format!(
            "`{}` takes either a `ref` or a `selector`, not both.",
            verb.name()
        ))),
        (Some(reference), None) => Ok(Aim::Ref(reference.to_string())),
        (None, Some(selector)) => Ok(Aim::Selector(selector.to_string())),
        (None, None) => Err(VerbError::bad_request(format!(
            "`{}` needs a `ref` from a snapshot, or a `selector`.",
            verb.name()
        ))),
    }
}

/// Resolve either handle to the element it names.
///
/// A ref goes through the staleness check, because an ordinal only means
/// anything against the reading that minted it. A selector does not, and does
/// not need to: it names whatever it matches *now*, by the same
/// `querySelector` rule the selector was verified under when it was minted, so
/// there is no earlier reading for it to have drifted from. That is precisely
/// why a recording is made of selectors.
fn resolve_aim(
    session: &Session,
    snapshot: &crate::snapshot::Snapshot,
    aim: &Aim,
) -> Result<crate::snapshot::RefEntry, VerbError> {
    match aim {
        Aim::Ref(reference) => resolve_ref(session, snapshot, reference),
        Aim::Selector(selector) => {
            let dom = session.page.dom();
            let doc = dom.borrow();
            let matched = crate::script::dom_api::matches_within(&doc, 0, selector);
            let Some(&node_id) = matched.first() else {
                return Err(VerbError::no_match(format!(
                    "`{selector}` matches nothing on this page. Take a `snapshot` to see what \
                     is there; its `refs` carry a verified selector for each element."
                )));
            };
            crate::snapshot::entry_for_node(&doc, node_id, selector).ok_or_else(|| {
                VerbError::wrong_role(
                    &format!("`{selector}`"),
                    "an element",
                    "something this engine offers as actionable — a link, a control or an image",
                )
            })
        }
    }
}

fn resolve_ref(
    session: &Session,
    snapshot: &crate::snapshot::Snapshot,
    reference: &str,
) -> Result<crate::snapshot::RefEntry, VerbError> {
    let Some(entry) = snapshot.resolve(reference) else {
        return Err(VerbError::no_such_ref(reference));
    };
    // No snapshot has been served, so the caller cannot have read this ref
    // anywhere. Distinguished from a stale one because the fix differs: this is
    // "take a snapshot", that is "take another".
    let Some(served) = session.served_refs.as_ref() else {
        return Err(VerbError::no_snapshot(reference));
    };
    let wanted = reference.trim_start_matches('@');
    match served.iter().find(|e| e.id == wanted) {
        Some(before) if same_target(before, entry) => Ok(entry.clone()),
        // Either it named something else in the served reading, or it was not
        // in it at all. Both mean the same thing to the caller and get the same
        // answer, which names what the ref points at *now* — the one piece of
        // evidence the session has and the agent does not.
        Some(_) | None => Err(VerbError::stale_ref(reference, &describe(entry))),
    }
}

/// Whether two readings of a ref name the same thing.
///
/// Compares identity, **not** `name`, and that distinction is the whole of it.
/// `accessible_name` reports a text input's *current value* (and a password
/// field's mask), so `type @e1 alice` changes `@e1`'s name from `username` to
/// `alice` without renumbering anything. A full `RefEntry` equality therefore
/// refused the second `type` on the same field — which is exactly the retry the
/// README documents ("`type` replaces the field rather than appending, so
/// retrying after a failed submit does not produce `alicealice`") and exactly
/// what the skill promises when it says typing renumbers nothing.
///
/// What identifies a ref is where it sits and what it is: the id it was served
/// under, the node it resolved to, its role, and — for a link — where it goes.
/// A page that changed any of those changed the thing the agent read.
fn same_target(before: &crate::snapshot::RefEntry, now: &crate::snapshot::RefEntry) -> bool {
    before.id == now.id
        && before.node_id == now.node_id
        && before.role == now.role
        && before.href == now.href
}

/// A ref entry as one line of prose, safe to put in an error message.
///
/// The name is page-derived, and an error message is read *outside* the
/// snapshot's fence. `one_line` is the same collapse the fence relies on, so a
/// page cannot smuggle a second line — or a forged fence marker — into a reply
/// by naming a button after one.
fn describe(entry: &crate::snapshot::RefEntry) -> String {
    let name = crate::snapshot::one_line(&entry.name);
    if name.is_empty() {
        format!("a {}", entry.role)
    } else {
        format!("a {} \"{}\"", entry.role, name)
    }
}

fn write_port_file(path: &Path, port: u16) -> Result<(), H5iError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| H5iError::with_path(e, parent))?;
    }
    std::fs::write(path, port.to_string()).map_err(|e| H5iError::with_path(e, path))
}

/// Handle one client message, returning what to send back.
///
/// Split out from the socket so the protocol can be tested without one.
fn handle(session: &mut Session, message: &Value) -> Result<Vec<Value>, H5iError> {
    let kind = message.get("type").and_then(Value::as_str).unwrap_or("");
    let (_, viewport_height) = session.viewport();

    let changed = match kind {
        // The viewer announces its pacing; answering with the current frame
        // means it has something to draw immediately.
        "config" => true,

        // Under ack pacing this is the client's permission to send the next
        // frame. Nothing has changed, so nothing is sent — which is what keeps
        // a still page at zero frames per second instead of a busy loop.
        "ack" => false,

        "input_mouse" => match message.get("eventType").and_then(Value::as_str) {
            Some("mouseWheel") => {
                let delta = message
                    .get("deltaY")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                session.page.scroll_by(0.0, delta)
            }
            Some("mouseReleased") => {
                let x = message.get("x").and_then(Value::as_f64).unwrap_or(0.0) as f32;
                let y = message.get("y").and_then(Value::as_f64).unwrap_or(0.0) as f32;
                if let Some(url) = session.page.link_at(x, y) {
                    let mut out = session.navigate(&url)?;
                    out.push(session.frame_message()?);
                    return Ok(out);
                }
                false
            }
            _ => false,
        },

        // keyUp never scrolls: acting on both halves would double every press.
        "input_keyboard"
            if message.get("eventType").and_then(Value::as_str) == Some("keyDown") =>
        {
            let key = message.get("key").and_then(Value::as_str).unwrap_or("");
            match scroll_for_key(key, viewport_height as f64) {
                Some(delta) => session.page.scroll_by(0.0, delta),
                None => false,
            }
        }

        _ => false,
    };

    if changed {
        Ok(vec![session.frame_message()?])
    } else {
        Ok(Vec::new())
    }
}

/// How far a key should scroll, or `None` if it is not a scrolling key.
///
/// `f64::MIN`/`MAX` for Home/End lean on `scroll_by` clamping to the document
/// rather than duplicating the bounds here.
fn scroll_for_key(key: &str, viewport_height: f64) -> Option<f64> {
    match key {
        "PageDown" | " " | "Space" => Some(viewport_height * PAGE_SCROLL),
        "PageUp" => Some(-viewport_height * PAGE_SCROLL),
        "ArrowDown" => Some(LINE_SCROLL),
        "ArrowUp" => Some(-LINE_SCROLL),
        "End" => Some(f64::MAX / 4.0),
        "Home" => Some(f64::MIN / 4.0),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::Broker;
    use crate::policy::Policy;
    use crate::receipt::MemorySink;
    use std::sync::Arc;

    fn session_with(html: &str) -> Session {
        let requests = Arc::new(MemorySink::new());
        let broker =
            Arc::new(Broker::new(Policy::new(), requests.clone(), None).expect("broker"));
        let fonts = crate::fonts::load(&[], &crate::fonts::default_font_dirs(), Some(4));
        let sources = fonts.sources.clone();
        let options = crate::engine::PageOptions {
            width: 400,
            height: 200,
            ..Default::default()
        };
        let factory = PageFactory::new(broker, sources, options);
        let page = factory.from_html(html, &Url::parse("https://example.com/").unwrap());
        Session {
            factory,
            page,
            quality: 70,
            seq: 0,
            actions: None,
            last_snapshot: None,
            served_refs: None,
            unknown_verbs: std::collections::BTreeMap::new(),
        recording: crate::replay::Recording::default(),
            requests,
            secrets: crate::secrets::Secrets::default(),
            login: false,
        }
    }

    /// Serves one page per connection, so a fused navigation has somewhere to
    /// go. Loopback is reachable by default (it is the dev server), which is
    /// what makes this testable without loosening a policy.
    fn one_page_server(body: &'static str, hits: usize) -> u16 {
        use std::io::{BufRead, BufReader, Write};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for _ in 0..hits {
                let Ok((stream, _)) = listener.accept() else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                let _ = reader.read_line(&mut line);
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0
                        || header.trim().is_empty()
                    {
                        break;
                    }
                }
                let mut stream = stream;
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.flush();
            }
        });
        port
    }

    /// A read verb given a `url` goes there first. The turn this saves is the
    /// whole point: `navigate` then `markdown` is two passes through a model to
    /// answer one question.
    #[test]
    fn a_read_verb_can_navigate_first_and_says_where_it_landed() {
        let port = one_page_server("<html><body><h1>Arrived</h1></body></html>", 1);
        let mut session = session_with("<html><body><p>before</p></body></html>");

        let (reply, moved) = control_verb(
            &mut session,
            &json!({"verb": "markdown", "url": format!("http://127.0.0.1:{port}/")}),
        );

        assert_eq!(reply["ok"], true, "{reply:?}");
        assert!(
            reply["text"].as_str().unwrap_or_default().contains("Arrived"),
            "it read the page it was already on: {reply:?}"
        );
        assert!(
            reply["url"].as_str().unwrap_or_default().contains(&port.to_string()),
            "the reply must name where it ended up, or a redirect is silent: {reply:?}"
        );
        assert!(moved, "the viewers are looking at a different page now");
    }

    /// A policy refusal reached this way is the same fact it would have been on
    /// its own. Dressing it up as an empty page would be the plausible-wrong
    /// answer this engine refuses everywhere else.
    #[test]
    fn a_refused_fused_navigation_is_a_refusal_not_an_empty_read() {
        let mut session = session_with("<html><body><p>before</p></body></html>");
        let (reply, moved) = control_verb(
            &mut session,
            &json!({"verb": "markdown", "url": "https://blocked.example/"}),
        );

        assert_eq!(reply["ok"], false, "{reply:?}");
        assert_eq!(reply["code"], "refused", "{reply:?}");
        assert!(!moved);
        assert!(
            reply.get("text").is_none(),
            "a refusal must not also carry a reading: {reply:?}"
        );
    }

    /// The safety half. A ref names a position in the reading that minted it,
    /// so refs from before a navigation cannot be honoured after one — and a
    /// fused navigation is still a navigation. Without this a `@ref` could
    /// match by coincidence (same ordinal, same role, same href) on a document
    /// the agent has never seen.
    #[test]
    fn a_fused_navigation_drops_the_refs_it_served_before() {
        let port = one_page_server("<html><body><a href=\"/x\">Go</a></body></html>", 1);
        let mut session = session_with("<html><body><a href=\"/x\">Go</a></body></html>");

        let (first, _) = control_verb(&mut session, &json!({"verb": "snapshot"}));
        assert_eq!(first["ok"], true);
        assert!(session.served_refs.is_some(), "a reading was served");

        let (_, _) = control_verb(
            &mut session,
            &json!({"verb": "markdown", "url": format!("http://127.0.0.1:{port}/")}),
        );

        let (reply, _) = control_verb(&mut session, &json!({"verb": "click", "ref": "@e1"}));
        assert_eq!(reply["ok"], false, "{reply:?}");
        assert_eq!(
            reply["code"], "no-snapshot",
            "a ref from before the navigation must not be honoured after it: {reply:?}"
        );
    }

    /// Which verbs may fuse a navigation is a property of the verb, and the
    /// exhaustive match makes a new verb answer the question. This pins the
    /// answer for the ones that exist: reads yes, actions and waits no.
    #[test]
    fn only_read_verbs_navigate_first() {
        use crate::verbs::Verb;
        for verb in Verb::ALL {
            let expected = matches!(
                verb,
                Verb::Snapshot | Verb::Markdown | Verb::Extract | Verb::Structured
            );
            assert_eq!(
                verb.navigates_first(),
                expected,
                "{} fuses a navigation when it should not, or the reverse",
                verb.name()
            );
        }
    }

    /// The recording is made of selectors, not ordinals, and that is the whole
    /// of why it can be run again: `@e1` names the first actionable thing in
    /// the reading that minted it, which is a different element on a page with
    /// one more link near the top.
    #[test]
    fn a_recorded_step_carries_a_selector_rather_than_the_ref_it_was_named_by() {
        let mut session = session_with(
            "<html><body><form action=\"/go\">\
               <input name=\"user\">\
               <button id=\"send\">Send</button>\
             </form></body></html>",
        );

        let (snap, _) = recorded_verb(&mut session, &json!({"verb": "snapshot"}));
        assert_eq!(snap["ok"], true, "{snap:?}");

        let (typed, _) = recorded_verb(
            &mut session,
            &json!({"verb": "type", "ref": "@e1", "text": "alice"}),
        );
        assert_eq!(typed["ok"], true, "{typed:?}");

        let (script, _) = recorded_verb(&mut session, &json!({"verb": "script"}));
        assert_eq!(script["ok"], true, "{script:?}");

        let steps = script["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 1, "only the state change is recorded: {steps:?}");
        assert_eq!(steps[0]["verb"], "type");
        assert_eq!(steps[0]["text"], "alice");
        let selector = steps[0]["selector"].as_str().unwrap();
        assert!(
            selector.contains("user"),
            "the selector should name the field durably, not by position: {selector}"
        );
        assert!(
            steps[0].get("ref").is_none(),
            "an ordinal must not reach the recording: {:?}",
            steps[0]
        );
    }

    /// And the recorded form actually drives the engine: the selector a
    /// recording carries is one the action verbs accept.
    #[test]
    fn a_recorded_script_replays_through_the_same_verbs() {
        let page = "<html><body><form action=\"/go\">\
                      <input name=\"user\">\
                      <button id=\"send\">Send</button>\
                    </form></body></html>";
        let mut session = session_with(page);
        recorded_verb(&mut session, &json!({"verb": "snapshot"}));
        recorded_verb(
            &mut session,
            &json!({"verb": "type", "ref": "@e1", "text": "alice"}),
        );
        let (script, _) = recorded_verb(&mut session, &json!({"verb": "script"}));
        let steps: Vec<crate::replay::Step> =
            serde_json::from_value(script["steps"].clone()).unwrap();

        // A fresh session, which has served no refs at all. An ordinal would be
        // refused here; the selector is not.
        let mut replayed = session_with(page);
        for step in &steps {
            let (reply, _) = control_verb(&mut replayed, &step.request());
            assert_eq!(reply["ok"], true, "replaying {:?}: {reply:?}", step.render());
        }

        let (snap, _) = control_verb(&mut replayed, &json!({"verb": "snapshot"}));
        assert!(
            snap["text"].as_str().unwrap().contains("alice"),
            "the replay should have reached the same state: {snap:?}"
        );
    }

    /// Reads are not recorded, and neither is handing the page to a human:
    /// there is nobody there to take it on a replay.
    #[test]
    fn only_state_changing_verbs_are_recorded() {
        use crate::verbs::Verb;
        for verb in Verb::ALL {
            let expected = matches!(
                verb,
                Verb::Navigate | Verb::Scroll | Verb::Type | Verb::Submit | Verb::Click
            );
            assert_eq!(
                verb.is_recorded(),
                expected,
                "{} is recorded when it should not be, or the reverse",
                verb.name()
            );
        }
    }

    /// A selector is a durable handle, so it needs no staleness check — it
    /// names whatever it matches now. An ordinal from no reading at all is
    /// still refused, which is the property that must not have been loosened.
    #[test]
    fn a_selector_needs_no_prior_reading_but_a_ref_still_does() {
        let mut session = session_with(
            "<html><body><input id=\"user\" name=\"user\"></body></html>",
        );

        // An ordinal with no reading behind it is refused, and that must not
        // have been loosened by teaching the verbs about selectors.
        let (by_ref, _) = control_verb(
            &mut session,
            &json!({"verb": "type", "ref": "@e1", "text": "alice"}),
        );
        assert_eq!(by_ref["code"], "no-snapshot", "{by_ref:?}");

        // The same element named durably needs no earlier reading: a selector
        // names whatever it matches now, so there is nothing for it to have
        // drifted from.
        let (by_selector, _) = control_verb(
            &mut session,
            &json!({"verb": "type", "selector": "#user", "text": "alice"}),
        );
        assert_eq!(by_selector["ok"], true, "{by_selector:?}");
    }

    #[test]
    fn naming_an_element_two_ways_at_once_is_a_bad_request() {
        let mut session = session_with("<html><body><a id=\"go\" href=\"/x\">Go</a></body></html>");
        let (reply, _) = control_verb(
            &mut session,
            &json!({"verb": "click", "ref": "@e1", "selector": "#go"}),
        );
        assert_eq!(reply["code"], "bad-request", "{reply:?}");
    }

    /// A selector that matches nothing is the caller's to fix, so it is
    /// in-band and retryable rather than a protocol failure.
    #[test]
    fn a_selector_that_matches_nothing_says_so_as_a_correctable_error() {
        let mut session = session_with("<html><body><p>nothing here</p></body></html>");
        let (reply, _) =
            control_verb(&mut session, &json!({"verb": "click", "selector": "#missing"}));
        assert_eq!(reply["code"], "no-match", "{reply:?}");
        assert_eq!(reply["retryable"], true, "{reply:?}");
    }

    /// A session whose page runs its own scripts, for the verbs that behave
    /// differently once script is present.
    fn scripted_session_with(html: &str) -> Session {
        let requests = Arc::new(MemorySink::new());
        let broker =
            Arc::new(Broker::new(Policy::new(), requests.clone(), None).expect("broker"));
        let fonts = crate::fonts::load(&[], &crate::fonts::default_font_dirs(), Some(2));
        let options = crate::engine::PageOptions {
            width: 400,
            height: 200,
            script: true,
            ..Default::default()
        };
        let factory = PageFactory::new(broker, fonts.sources.clone(), options);
        let page = factory.from_html(html, &Url::parse("https://app.example/").unwrap());
        Session {
            factory,
            page,
            quality: 70,
            seq: 0,
            actions: None,
            last_snapshot: None,
            served_refs: None,
            unknown_verbs: std::collections::BTreeMap::new(),
        recording: crate::replay::Recording::default(),
            requests,
            secrets: crate::secrets::Secrets::default(),
            login: false,
        }
    }

    /// Take a snapshot **through the verb**, and hand back the refs it served.
    ///
    /// Reaching into `session.page.snapshot()` gets a ref the agent was never
    /// given, and the session now refuses to act on one of those — which is the
    /// point of `resolve_ref`. Tests drive the same loop an agent does.
    fn serve_refs(session: &mut Session) -> Vec<crate::snapshot::RefEntry> {
        let (reply, _) = control_verb(session, &json!({"verb": "snapshot"}));
        assert_eq!(reply["ok"], true, "{reply:?}");
        session
            .served_refs
            .clone()
            .expect("the snapshot verb records what it served")
    }

    fn tall_page() -> &'static str {
        "<!doctype html><body><div style='height:4000px'>\
         <p>top</p><a href='/next'>next</a></div></body>"
    }

    #[test]
    fn status_carries_the_viewport_so_clicks_land_where_they_were_aimed() {
        let session = session_with("<p>hi</p>");
        let status = session.status_message();
        assert_eq!(status["viewportWidth"], 400);
        assert_eq!(status["viewportHeight"], 200);
        assert_eq!(status["type"], "status");
        // Not "chromium": a viewer should not infer Chromium behaviour here.
        assert_eq!(status["engine"], "h5i-browser-light");
    }

    #[test]
    fn a_frame_is_base64_jpeg_with_a_monotonic_seq() {
        let mut session = session_with("<p>hi</p>");
        let first = session.frame_message().expect("frame");
        let second = session.frame_message().expect("frame");

        assert_eq!(first["type"], "frame");
        assert_eq!(first["seq"], 1);
        assert_eq!(second["seq"], 2, "seq must advance for ack pacing to work");

        let data = first["data"].as_str().expect("data is a string");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .expect("data is base64");
        assert_eq!(&bytes[..2], &[0xFF, 0xD8], "JPEG SOI marker");
    }

    #[test]
    fn an_ack_alone_produces_no_frame() {
        // The zero-fps-at-rest property, as a test: if acking ever produced a
        // frame, two viewers would ping-pong forever at full CPU.
        let mut session = session_with(tall_page());
        let out = handle(&mut session, &json!({"type": "ack", "seq": 1})).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn a_scroll_that_moves_sends_a_frame_and_one_that_cannot_does_not() {
        let mut session = session_with(tall_page());

        let scrolled = handle(
            &mut session,
            &json!({"type":"input_mouse","eventType":"mouseWheel","deltaY": 120.0}),
        )
        .unwrap();
        assert_eq!(scrolled.len(), 1, "a real scroll redraws");
        assert_eq!(session.page.scroll_offset().1, 120.0);

        // Already at the top: scrolling up moves nothing, so nothing is sent.
        let stuck = handle(
            &mut session,
            &json!({"type":"input_mouse","eventType":"mouseWheel","deltaY": -9999.0}),
        )
        .unwrap();
        assert_eq!(session.page.scroll_offset().1, 0.0);
        assert_eq!(stuck.len(), 1, "the first one moved to the top");

        let no_move = handle(
            &mut session,
            &json!({"type":"input_mouse","eventType":"mouseWheel","deltaY": -50.0}),
        )
        .unwrap();
        assert!(no_move.is_empty(), "a scroll with nowhere to go sends nothing");
    }

    #[test]
    fn scroll_is_clamped_to_the_document() {
        let mut session = session_with(tall_page());
        session.page.scroll_by(0.0, 100_000.0);
        let (_, y) = session.page.scroll_offset();
        assert!(y > 0.0);
        assert!(
            y <= session.page.content_height(),
            "scrolled past the end of the document: {y}"
        );
    }

    #[test]
    fn a_page_whose_css_pins_the_root_to_the_viewport_still_scrolls() {
        // The regression found by pointing this at Wikipedia. `html, body {
        // height: 100% }` sizes the root box to the viewport and lets the
        // article overflow it, so measuring the root's own box reported a long
        // page as exactly one screen and clamped every scroll to nothing. Every
        // local test page was unstyled, which is why they all passed.
        let mut session = session_with(
            "<!doctype html><html><head><style>html,body{height:100%;margin:0}</style></head>\
             <body><div style='height:4000px'>long</div></body></html>",
        );

        assert!(
            session.page.content_height() > 200.0,
            "the overflowing content counts toward the height: {}",
            session.page.content_height()
        );
        assert!(
            session.page.scroll_by(0.0, 300.0),
            "a page taller than the viewport must scroll"
        );
        let (_, y) = session.page.scroll_offset();
        assert!(y > 0.0, "scrolled to {y}");
    }

    #[test]
    fn keys_scroll_by_the_amounts_a_reader_expects() {
        assert_eq!(scroll_for_key("PageDown", 1000.0), Some(900.0));
        assert_eq!(scroll_for_key("PageUp", 1000.0), Some(-900.0));
        assert_eq!(scroll_for_key("ArrowDown", 1000.0), Some(LINE_SCROLL));
        assert!(scroll_for_key("End", 1000.0).unwrap() > 0.0);
        assert!(scroll_for_key("Home", 1000.0).unwrap() < 0.0);
        // A key with no scrolling meaning must not redraw.
        assert_eq!(scroll_for_key("a", 1000.0), None);
    }

    #[test]
    fn an_unknown_message_type_is_ignored_rather_than_fatal() {
        // Both viewers send messages this engine does not implement; treating
        // one as an error would drop the connection mid-session.
        let mut session = session_with("<p>hi</p>");
        let out = handle(&mut session, &json!({"type": "hoverHighlight"})).unwrap();
        assert!(out.is_empty());
        let out = handle(&mut session, &json!({"nonsense": true})).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn a_denied_navigation_reports_itself_instead_of_going_blank() {
        // The link points off-origin and the policy allows nothing, so the
        // click must produce an error message and keep the current page.
        let mut session = session_with(
            "<!doctype html><body><a href='https://denied.test/x' \
             style='display:block;width:400px;height:200px'>go</a></body>",
        );
        let before = session.page.url().to_string();

        let out = handle(
            &mut session,
            &json!({"type":"input_mouse","eventType":"mouseReleased","x":10.0,"y":10.0,"button":"left"}),
        )
        .unwrap();

        let kinds: Vec<&str> = out
            .iter()
            .filter_map(|m| m.get("type").and_then(Value::as_str))
            .collect();
        assert!(kinds.contains(&"page_error"), "{kinds:?}");
        assert_eq!(session.page.url().to_string(), before, "page must survive");
    }

    #[test]
    fn the_stream_file_advertises_the_port_where_viewers_look_for_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("agent-browser").join("s1.stream");
        write_port_file(&path, 45123).expect("writes");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "45123");
    }

    // ── the resident session ────────────────────────────────────────────────

    fn viewer(ack_pacing: bool) -> (Viewer, Receiver<Outgoing>) {
        let (tx, rx) = channel();
        (
            Viewer {
                tx,
                ack_pacing,
                awaiting_ack: false,
                pending: None,
            },
            rx,
        )
    }

    fn texts(rx: &Receiver<Outgoing>) -> Vec<Value> {
        let mut out = Vec::new();
        while let Ok(message) = rx.try_recv() {
            if let Outgoing::Text(text) = message {
                out.push(serde_json::from_str(&text).expect("json"));
            }
        }
        out
    }

    fn kinds(messages: &[Value]) -> Vec<String> {
        messages
            .iter()
            .filter_map(|m| m.get("type").and_then(Value::as_str))
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn a_frame_reaches_every_viewer_and_a_reply_reaches_only_the_asker() {
        // The bug this pins was found by driving a real box: the old serve
        // handled one connection at a time, so opening the console's page tab
        // silently blocked `h5i box view` instead of showing both the page.
        let mut viewers = HashMap::new();
        let (a, a_rx) = viewer(false);
        let (b, b_rx) = viewer(false);
        viewers.insert(1u64, a);
        viewers.insert(2u64, b);

        dispatch(
            &mut viewers,
            1,
            vec![
                json!({"type": "frame", "seq": 7, "data": ""}),
                json!({"type": "page_error", "text": "only the clicker cares"}),
            ],
        );

        assert_eq!(kinds(&texts(&a_rx)), vec!["frame", "page_error"]);
        assert_eq!(
            kinds(&texts(&b_rx)),
            vec!["frame"],
            "a second viewer sees the page move, not the other viewer's error"
        );
    }

    #[test]
    fn a_viewer_that_owes_an_ack_gets_the_newest_frame_not_a_backlog() {
        // Ack pacing used to fall out of "one frame per client message". With
        // several actors on one session that no longer holds, so the pacing is
        // tracked per viewer — and the held frame is the latest, because a
        // viewer that fell behind wants the current page, not a replay.
        let (mut v, rx) = viewer(true);

        send_frame(&mut v, json!({"type": "frame", "seq": 1}));
        assert!(v.awaiting_ack, "the first frame starts the wait");
        send_frame(&mut v, json!({"type": "frame", "seq": 2}));
        send_frame(&mut v, json!({"type": "frame", "seq": 3}));

        let sent = texts(&rx);
        assert_eq!(sent.len(), 1, "only the acked-for frame went out: {sent:?}");
        assert_eq!(sent[0]["seq"], 1);
        assert_eq!(
            v.pending.as_ref().and_then(|f| f["seq"].as_u64()),
            Some(3),
            "the held frame is the newest, not the oldest"
        );

        // The ack releases exactly one frame, and it is the newest.
        v.awaiting_ack = false;
        let pending = v.pending.take().expect("a frame was held");
        send_frame(&mut v, pending);
        let sent = texts(&rx);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["seq"], 3);
    }

    #[test]
    fn a_viewer_without_ack_pacing_is_never_throttled() {
        let (mut v, rx) = viewer(false);
        for seq in 1..=3 {
            send_frame(&mut v, json!({"type": "frame", "seq": seq}));
        }
        assert_eq!(texts(&rx).len(), 3);
        assert!(v.pending.is_none());
    }

    #[test]
    fn control_verbs_answer_and_say_whether_the_page_moved() {
        let mut session = session_with(tall_page());

        let (reply, changed) = control_verb(&mut session, &json!({"verb": "status"}));
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["engine"], "h5i-browser-light");
        assert!(!changed, "asking what the page is does not move it");

        let (reply, changed) = control_verb(&mut session, &json!({"verb": "snapshot"}));
        assert_eq!(reply["ok"], true);
        assert!(
            reply["text"]
                .as_str()
                .unwrap()
                .contains(crate::snapshot::CONTENT_BEGIN),
            "an agent reading through the control channel gets the same fenced \
             outline as one reading the CLI: {reply:?}"
        );
        assert!(!changed);
    }

    #[test]
    fn scroll_reports_whether_it_actually_moved() {
        let mut session = session_with(tall_page());

        let (reply, changed) = control_verb(&mut session, &json!({"verb": "scroll", "by": 300.0}));
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["moved"], true);
        assert!(changed, "a scroll that moved is a frame for every viewer");

        // At the top, scrolling up moves nothing — and an agent that cannot
        // tell the difference will loop asking for page that is not there.
        let (reply, changed) =
            control_verb(&mut session, &json!({"verb": "scroll", "by": -100000.0}));
        assert_eq!(reply["moved"], true, "it can still travel back to the top");
        assert!(changed);
        let (reply, changed) = control_verb(&mut session, &json!({"verb": "scroll", "by": -100.0}));
        assert_eq!(reply["moved"], false, "already at the top: {reply:?}");
        assert!(!changed, "a scroll that moved nothing encodes no frame");
    }

    #[test]
    fn every_verb_that_reaches_the_session_is_recorded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("browser-actions.jsonl");
        let mut session = session_with(tall_page());
        session.actions = Some(ActionLog::create(&path).expect("log"));

        recorded_verb(&mut session, &json!({"verb": "snapshot"}));
        recorded_verb(&mut session, &json!({"verb": "scroll", "by": 200.0}));
        recorded_verb(&mut session, &json!({"verb": "click", "ref": "e404"}));

        let text = std::fs::read_to_string(&path).expect("written");
        let results: Vec<&str> = text
            .lines()
            .filter(|l| l.contains("\"phase\":\"result\""))
            .collect();
        assert_eq!(results.len(), 3, "one outcome per verb:\n{text}");

        // A read is recorded as surely as a write. "The agent looked at this
        // page" is exactly the kind of thing a reviewer is auditing for.
        assert!(results[0].contains("\"verb\":\"snapshot\""), "{text}");
        // The target travels whatever its spelling: a url, a ref, a distance.
        assert!(results[1].contains("\"target\":\"200.0\""), "{text}");
        assert!(results[2].contains("\"ok\":false"), "{text}");
        assert!(results[2].contains("e404"), "{text}");
    }

    #[test]
    fn a_verb_that_cannot_be_recorded_does_not_happen() {
        // No record, no action. Proved by the page not moving, not merely by
        // the reply: an agent that scrolled invisibly would be the bug.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("actions.jsonl");
        let mut session = session_with(tall_page());
        session.actions = Some(ActionLog::unwritable_for_test(&path).expect("log"));

        let before = session.page.scroll_offset();
        let (reply, changed) = recorded_verb(&mut session, &json!({"verb": "scroll", "by": 400.0}));

        assert_eq!(reply["ok"], false);
        assert!(
            reply["error"].as_str().unwrap().contains("could not be recorded"),
            "the agent is told why: {reply:?}"
        );
        assert!(!changed);
        assert_eq!(session.page.scroll_offset(), before, "the page must not move");
    }

    #[test]
    fn a_session_without_a_log_still_works() {
        // The engine runs on a bare host too, where there is no console to feed
        // and no claim to support.
        let mut session = session_with(tall_page());
        assert!(session.actions.is_none());
        let (reply, changed) = recorded_verb(&mut session, &json!({"verb": "scroll", "by": 300.0}));
        assert_eq!(reply["ok"], true);
        assert!(changed);
    }

    #[test]
    fn typing_names_what_went_wrong_rather_than_failing_silently() {
        let mut session = session_with(
            "<!doctype html><body><a href='/next'>a link</a>             <form><input type='text' placeholder='name'></form></body>",
        );

        let (reply, _) = control_verb(&mut session, &json!({"verb": "type", "ref": "e1"}));
        assert_eq!(reply["ok"], false, "text is required: {reply:?}");

        let (reply, changed) = control_verb(
            &mut session,
            &json!({"verb": "type", "ref": "e404", "text": "x"}),
        );
        assert_eq!(reply["ok"], false);
        assert!(reply["error"].as_str().unwrap().contains("e404"));
        assert!(!changed);

        // The link is @e1; typing into it must say *why* it cannot be typed
        // into, not merely that it failed. Read the page first, the way an
        // agent does — a ref the session never served is refused before the
        // role is even looked at.
        let _ = serve_refs(&mut session);
        let (reply, _) = control_verb(
            &mut session,
            &json!({"verb": "type", "ref": "e1", "text": "x"}),
        );
        assert_eq!(reply["ok"], false);
        assert!(
            reply["error"].as_str().unwrap().contains("not a field"),
            "{reply:?}"
        );
    }

    #[test]
    fn a_status_reports_how_many_cookies_it_holds_and_never_which() {
        let mut session = session_with(tall_page());
        let (reply, _) = control_verb(&mut session, &json!({"verb": "status"}));
        assert_eq!(reply["cookies"], 0);
        // The shape of the answer is the guarantee: there is no field here a
        // value could travel in.
        let keys: Vec<&str> = reply.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert!(!keys.iter().any(|k| k.contains("value")), "{keys:?}");
    }

    #[test]
    fn an_unknown_verb_is_an_answer_rather_than_a_dropped_connection() {
        let mut session = session_with(tall_page());
        let (reply, changed) = control_verb(&mut session, &json!({"verb": "typewrite"}));
        assert_eq!(reply["ok"], false);
        assert!(
            reply["error"].as_str().unwrap().contains("typewrite"),
            "the answer names what was asked for: {reply:?}"
        );
        assert!(!changed);
    }

    #[test]
    fn a_control_navigation_the_policy_refuses_reports_itself_and_keeps_the_page() {
        // The same rule the viewer path already follows: a refusal is the
        // engine working, and the page an agent was on must survive being told
        // no — otherwise the next snapshot describes a blank it cannot explain.
        let mut session = session_with(tall_page());
        let before = session.page.url().to_string();

        let (reply, changed) =
            control_verb(&mut session, &json!({"verb": "navigate", "url": "https://denied.test/"}));

        assert_eq!(reply["ok"], false);
        assert!(!changed, "a refused navigation moved nothing");
        assert_eq!(session.page.url().to_string(), before);
    }

    #[test]
    fn a_control_click_needs_a_ref_that_exists_and_can_be_followed() {
        let mut session = session_with(tall_page());

        let (reply, _) = control_verb(&mut session, &json!({"verb": "click"}));
        assert_eq!(reply["ok"], false, "a click with no ref is refused");

        let (reply, changed) = control_verb(&mut session, &json!({"verb": "click", "ref": "e404"}));
        assert_eq!(reply["ok"], false);
        assert!(reply["error"].as_str().unwrap().contains("e404"));
        assert!(!changed);
    }

    #[test]
    fn a_click_on_a_scripted_page_runs_the_handler_rather_than_following_a_link() {
        // With script running, a click is an event before it is a navigation. A
        // button with no href is only clickable at all because of its handler.
        let mut session = scripted_session_with(
            "<html><body><button id='b'>Add</button><ul id='l'></ul><script>\
             document.querySelector('#b').addEventListener('click', () => { \
               const li = document.createElement('li'); li.textContent = 'added'; \
               document.querySelector('#l').appendChild(li); });\
             </script></body></html>",
        );

        let reference = serve_refs(&mut session)
            .iter()
            .find(|r| r.name == "Add")
            .expect("the button has a ref")
            .id
            .clone();

        let (reply, changed) =
            control_verb(&mut session, &json!({"verb": "click", "ref": reference}));

        assert_eq!(reply["ok"], true, "{reply:?}");
        assert!(changed, "the page moved, so viewers get a frame");
        assert!(
            session.page.snapshot().render().contains("added"),
            "the handler ran and the agent can see it:\n{}",
            session.page.snapshot().render()
        );
        assert!(reply["settled"].is_string(), "the reply says whether it finished");
    }

    #[test]
    fn a_click_reports_the_requests_it_caused() {
        // The causal link, stamped by the one component that knows it. Empty
        // here because the handler makes none, which is still the honest answer
        // rather than a missing field.
        let mut session = scripted_session_with(
            "<html><body><button id='b'>Go</button><script>\
             document.querySelector('#b').addEventListener('click', () => {});\
             </script></body></html>",
        );
        let reference = serve_refs(&mut session)[0].id.clone();
        let (reply, _) = control_verb(&mut session, &json!({"verb": "click", "ref": reference}));

        assert!(reply["requests"].is_array(), "{reply:?}");
        assert_eq!(reply["requests"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn a_snapshot_says_whether_script_ran_and_whether_the_page_settled() {
        let mut scripted = scripted_session_with("<html><body><p>hi</p></body></html>");
        let (reply, _) = control_verb(&mut scripted, &json!({"verb": "snapshot"}));
        assert_eq!(reply["script"], true);
        assert!(reply["settled"].is_string(), "{reply:?}");

        // And with script off, it says that too rather than leaving it unsaid.
        let mut plain = session_with("<html><body><p>hi</p></body></html>");
        let (reply, _) = control_verb(&mut plain, &json!({"verb": "snapshot"}));
        assert_eq!(reply["script"], false);
        assert!(reply["settled"].is_null());
    }

    #[test]
    fn a_link_click_still_navigates_when_no_handler_took_it() {
        // Script being on must not break the plain case: a link with an href
        // and no listener is still followed.
        let mut session = scripted_session_with(
            "<html><body><a href='https://denied.test/'>go</a></body></html>",
        );
        let reference = serve_refs(&mut session)[0].id.clone();
        let (reply, changed) =
            control_verb(&mut session, &json!({"verb": "click", "ref": reference}));

        // Refused by policy, which is the navigation path reporting itself
        // rather than the click being silently swallowed by the event path.
        assert_eq!(reply["ok"], false, "{reply:?}");
        assert!(!changed);
        assert!(reply["error"].as_str().unwrap().contains("denied.test"), "{reply:?}");
    }

    #[test]
    fn a_snapshot_carries_a_durable_handle_beside_the_ordinal_one() {
        // `@e1` is a position in this reading; the selector is a handle that
        // survives one. Both are reported, because they answer different
        // questions and neither replaces the other.
        let mut session = session_with(
            "<html><body><form><input name='user'>\
             <button id='go'>Sign in</button></form></body></html>",
        );
        let (reply, _) = control_verb(&mut session, &json!({"verb": "snapshot"}));
        assert_eq!(reply["ok"], true, "{reply:?}");

        let refs = reply["refs"].as_array().expect("refs in the reply");
        assert_eq!(refs.len(), 2, "{refs:?}");

        let button = refs
            .iter()
            .find(|r| r["name"] == "Sign in")
            .expect("the button");
        assert_eq!(button["selector"], "#go");
        // The ordinal is still there.
        assert!(button["id"].as_str().unwrap().starts_with('e'));

        let field = refs.iter().find(|r| r["name"] == "user").expect("the field");
        assert!(
            field["selector"]
                .as_str()
                .unwrap()
                .contains("[name=\"user\"]"),
            "{field:?}"
        );
    }

    #[test]
    fn a_credential_reaches_the_field_and_never_the_reply() {
        // The whole claim, in one test. The agent names a credential, the value
        // lands in the page, and nothing the agent can read ever holds it.
        let mut session = session_with(
            "<html><body><form action='/in' method='post'>\
             <input name='pass' type='password'></form></body></html>",
        );
        session.secrets =
            crate::secrets::Secrets::from_pairs(&[("H5I_SECRET_ACME_PASS", "hunter2")]);

        let refs = serve_refs(&mut session);
        let (reply, changed) = control_verb(
            &mut session,
            &json!({
                "verb": "type",
                "ref": refs[0].id.clone(),
                "text": "$H5I_SECRET_ACME_PASS",
            }),
        );

        assert_eq!(reply["ok"], true, "{reply:?}");
        assert!(changed);
        // The name, because a receipt has to be able to say a credential was
        // used. Never the value.
        assert_eq!(reply["used"], json!(["H5I_SECRET_ACME_PASS"]));
        let rendered = reply.to_string();
        assert!(
            !rendered.contains("hunter2"),
            "the value is in the reply: {rendered}"
        );

        // It really did reach the field.
        assert_eq!(
            session.page.field_value(refs[0].node_id).as_deref(),
            Some("hunter2")
        );

        // And a snapshot does not read it back out. The engine renders a
        // password field's value as its own placeholder, so this is the
        // existing behaviour rather than a new promise — asserted so it stays.
        let (snap, _) = control_verb(&mut session, &json!({"verb": "snapshot"}));
        assert!(
            !snap["text"].as_str().unwrap().contains("hunter2"),
            "{snap:?}"
        );
    }

    #[test]
    fn a_credential_a_page_reflects_back_is_redacted_on_the_way_out() {
        // `secrets` documents redaction as the rule that anything written back
        // out goes through, and nothing called it. It matters beyond tidiness:
        // a login form that reflects what was typed — a hidden field, a
        // validation message, a title — puts the value in the DOM, and the next
        // snapshot would hand it to the agent the indirection exists to keep it
        // from.
        let mut session = session_with(
            "<html><body><p id='echo'>nothing yet</p></body></html>",
        );
        session.secrets =
            crate::secrets::Secrets::from_pairs(&[("H5I_SECRET_ACME", "hunter2-secret")]);

        // Stand in for the page having reflected it: put the value where a
        // read verb will find it.
        let dom = session.page.dom();
        {
            let doc = dom.borrow();
            let node = doc.query_selector("#echo").ok().flatten();
            assert!(node.is_some(), "the fixture needs the node");
        }
        let reply = redact_reply(
            &session.secrets,
            json!({
                "ok": true,
                "text": "the form said hunter2-secret back",
                "nested": {"rows": ["hunter2-secret"]},
            }),
        );
        let rendered = reply.to_string();
        assert!(
            !rendered.contains("hunter2-secret"),
            "the value survived a reply: {rendered}"
        );
        assert!(
            rendered.contains("$H5I_SECRET_ACME"),
            "and it should say which credential it was: {rendered}"
        );
    }

    #[test]
    fn env_lists_names_and_the_engine_has_no_verb_that_returns_a_value() {
        let mut session = session_with("<html><body><p>hi</p></body></html>");
        session.secrets = crate::secrets::Secrets::from_pairs(&[
            ("H5I_SECRET_A", "aaaa-secret"),
            ("H5I_SECRET_B", "bbbb-secret"),
        ]);

        let (reply, changed) = control_verb(&mut session, &json!({"verb": "env"}));
        assert_eq!(reply["ok"], true, "{reply:?}");
        assert!(!changed);
        assert_eq!(reply["names"], json!(["H5I_SECRET_A", "H5I_SECRET_B"]));
        let rendered = reply.to_string();
        assert!(!rendered.contains("aaaa-secret"), "{rendered}");
        assert!(!rendered.contains("bbbb-secret"), "{rendered}");
    }

    #[test]
    fn env_is_refused_while_a_human_is_logging_in() {
        let mut session = session_with("<html><body><p>hi</p></body></html>");
        session.login = true;
        let (reply, _) = control_verb(&mut session, &json!({"verb": "env"}));
        assert_eq!(reply["code"], "login-mode", "{reply:?}");
    }

    #[test]
    fn a_placeholder_that_names_nothing_is_reported_rather_than_emptied() {
        let mut session = session_with(
            "<html><body><input name='u'></body></html>",
        );
        let refs = serve_refs(&mut session);
        let (reply, _) = control_verb(
            &mut session,
            &json!({
                "verb": "type",
                "ref": refs[0].id.clone(),
                "text": "$H5I_SECRET_TYPO",
            }),
        );
        assert_eq!(reply["ok"], true, "{reply:?}");
        assert_eq!(reply["unresolved"], json!(["H5I_SECRET_TYPO"]));
        // Typed literally, so the mistake is visible in the field rather than
        // arriving as a login failure that looks like a wrong password.
        assert_eq!(
            session.page.field_value(refs[0].node_id).as_deref(),
            Some("$H5I_SECRET_TYPO")
        );
    }

    #[test]
    fn waiting_for_something_already_there_costs_nothing() {
        let mut session = session_with("<html><body><p id='done'>ready</p></body></html>");
        let (reply, changed) = control_verb(
            &mut session,
            &json!({"verb": "wait_for", "selector": "#done"}),
        );
        assert_eq!(reply["ok"], true, "{reply:?}");
        assert_eq!(reply["met"], true);
        assert_eq!(reply["end"], "met");
        assert_eq!(reply["waited_ms"], 0);
        assert!(!changed, "nothing ran, so no viewer needs a frame");
    }

    #[test]
    fn a_page_that_cannot_change_says_so_instead_of_waiting() {
        // The answer worth having. This page runs no script, so the element is
        // never going to appear — and reporting that immediately is a different
        // and more useful fact than timing out.
        let mut session = session_with("<html><body><p>hi</p></body></html>");
        let started = std::time::Instant::now();
        let (reply, _) = control_verb(
            &mut session,
            &json!({"verb": "wait_for", "selector": "#never"}),
        );
        assert_eq!(reply["ok"], true, "{reply:?}");
        assert_eq!(reply["met"], false);
        assert_eq!(
            reply["end"], "quiescent",
            "not `budget`: nothing was still working, so this is settled, not cut off"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "it should not have spent a budget proving the obvious"
        );
        let message = reply["message"].as_str().unwrap();
        assert!(message.contains("nothing left to run"), "{message:?}");
    }

    #[test]
    fn waiting_for_text_reads_the_outline_a_reader_would_see() {
        // Not the raw tree: a match inside a `<script>` body is not something
        // the agent would ever have read.
        let mut session = session_with(
            "<html><body><script>var marker = 'appeared';</script><p>hi</p></body></html>",
        );
        let (reply, _) = control_verb(
            &mut session,
            &json!({"verb": "wait_for", "text": "appeared"}),
        );
        assert_eq!(reply["met"], false, "script text is not page text: {reply:?}");

        let (reply, _) = control_verb(&mut session, &json!({"verb": "wait_for", "text": "hi"}));
        assert_eq!(reply["met"], true, "{reply:?}");
    }

    #[test]
    fn a_wait_on_a_virtual_clock_is_free_and_repeatable() {
        // The property neither reference engine has, and it turns out to be
        // stronger than "the wait is cheap".
        //
        // The page arms a one second timer. Because the settle runs on a
        // *virtual* clock and runs to quiescence, that timer has already fired
        // by the time the session exists — so the wait does not wait at all, it
        // answers. Both reference engines would have spent a real second here,
        // or given up early on a wall-clock heuristic and reported a page that
        // had not finished.
        let page = "<html><body><div id='host'></div><script>\
                    setTimeout(() => { const p = document.createElement('p'); \
                    p.textContent = 'late'; document.querySelector('#host').appendChild(p); \
                    }, 1000);</script></body></html>";

        let started = std::time::Instant::now();
        let mut first = scripted_session_with(page);
        let (a, _) = control_verb(&mut first, &json!({"verb": "wait_for", "text": "late"}));
        let real = started.elapsed();

        assert_eq!(a["ok"], true, "{a:?}");
        assert_eq!(a["met"], true, "the timer fired and the node landed: {a:?}");
        assert_eq!(a["end"], "met");
        assert!(
            real < std::time::Duration::from_millis(900),
            "a page's own second of delay costs the agent nothing: {real:?}"
        );

        // And the answer does not depend on how the machine was feeling.
        let mut second = scripted_session_with(page);
        let (b, _) = control_verb(&mut second, &json!({"verb": "wait_for", "text": "late"}));
        assert_eq!(a["met"], b["met"]);
        assert_eq!(a["end"], b["end"]);
        assert_eq!(
            a["waited_ms"], b["waited_ms"],
            "two runs of one page answer identically"
        );
    }

    #[test]
    fn a_wait_answers_rather_than_waits_because_the_page_already_ran() {
        // Worth pinning as a property rather than leaving as an accident: the
        // settle runs a page to quiescence, so by the time any verb is served
        // there is nothing pending. `wait_for` is therefore a *definitive*
        // answer — found, or cannot appear — and not a sleep. An agent that
        // polls it in a loop is doing no good and this is why.
        let mut session = scripted_session_with(
            "<html><body><p>here</p><script>var n = 1;</script></body></html>",
        );
        for _ in 0..3 {
            let (reply, changed) = control_verb(
                &mut session,
                &json!({"verb": "wait_for", "selector": "#absent"}),
            );
            assert_eq!(reply["end"], "quiescent", "{reply:?}");
            assert_eq!(reply["waited_ms"], 0);
            assert!(!changed, "a wait that ran nothing moves no viewer");
        }
    }
    #[test]
    fn wait_for_script_needs_a_realm_and_says_so_as_a_routing_answer() {
        let mut session = session_with("<html><body><p>hi</p></body></html>");
        let (reply, _) = control_verb(
            &mut session,
            &json!({"verb": "wait_for_script", "expr": "true"}),
        );
        assert_eq!(reply["ok"], false, "{reply:?}");
        assert_eq!(reply["code"], "no-script");
        assert_eq!(
            reply["retryable"], false,
            "retrying will not grow a script engine"
        );

        let mut scripted = scripted_session_with(
            "<html><body><p>hi</p><script>var ready = true;</script></body></html>",
        );
        let (reply, _) = control_verb(
            &mut scripted,
            &json!({"verb": "wait_for_script", "expr": "globalThis.ready === true"}),
        );
        assert_eq!(reply["ok"], true, "{reply:?}");
        assert_eq!(reply["met"], true, "{reply:?}");
    }

    #[test]
    fn a_condition_that_throws_is_not_yet_rather_than_an_error() {
        // A page mid-build throws on the way to values it has not made, so a
        // throw has to count as "not satisfied" or no useful condition can be
        // written at all.
        let mut session = scripted_session_with(
            "<html><body><p>hi</p><script>var ok = 1;</script></body></html>",
        );
        let (reply, _) = control_verb(
            &mut session,
            &json!({"verb": "wait_for_script", "expr": "window.nothing.here.at.all"}),
        );
        assert_eq!(reply["ok"], true, "a throw is an answer, not a failure: {reply:?}");
        assert_eq!(reply["met"], false);
        assert_eq!(reply["end"], "quiescent");
    }

    #[test]
    fn wait_for_refuses_two_conditions_at_once() {
        let mut session = session_with("<html><body><p>hi</p></body></html>");
        let (reply, _) = control_verb(
            &mut session,
            &json!({"verb": "wait_for", "selector": "p", "text": "hi"}),
        );
        assert_eq!(reply["code"], "bad-request", "{reply:?}");

        let (reply, _) = control_verb(&mut session, &json!({"verb": "wait_for"}));
        assert_eq!(reply["code"], "bad-request", "{reply:?}");
    }

    #[test]
    fn the_request_log_is_readable_through_the_session_and_counts_refusals() {
        // The claim this verb rests on: what it reports is what the broker
        // recorded before the wire, not a reconstruction afterwards. So the
        // test drives a real refusal and reads it back through the verb.
        let mut session = session_with("<html><body><p>hi</p></body></html>");

        let (reply, changed) = control_verb(&mut session, &json!({"verb": "requests"}));
        assert_eq!(reply["ok"], true, "{reply:?}");
        assert!(!changed, "reading the log does not move the page");
        assert_eq!(reply["denied"], 0);
        assert_eq!(reply["total"], 0, "nothing has been fetched yet");
        assert!(reply["cursor"].is_null(), "no cursor before any request");

        // A navigation the policy refuses. The allowlist is empty and this is
        // not loopback, so the broker records the decision and no bytes move.
        let (reply, _) = control_verb(
            &mut session,
            &json!({"verb": "navigate", "url": "https://denied.test/"}),
        );
        assert_eq!(reply["ok"], false, "{reply:?}");
        assert_eq!(reply["code"], "refused");

        let (reply, _) = control_verb(&mut session, &json!({"verb": "requests"}));
        assert_eq!(reply["denied"], 1, "the refusal is in the log: {reply:?}");
        let rows = reply["requests"].as_array().expect("an array");
        assert!(!rows.is_empty());
        assert!(
            rows.iter().any(|r| r["url"]
                .as_str()
                .is_some_and(|u| u.contains("denied.test"))),
            "the refused URL is named: {reply:?}"
        );
        assert!(
            reply["text"].as_str().unwrap().contains("DENIED"),
            "{reply:?}"
        );

        // The cursor is the shape an agent loop needs: ask again with it and
        // get only what is new, which here is nothing.
        let cursor = reply["cursor"].as_u64().expect("a cursor once there are rows");
        let (again, _) = control_verb(
            &mut session,
            &json!({"verb": "requests", "since": cursor}),
        );
        assert_eq!(again["shown"], 0, "{again:?}");
        assert_eq!(
            again["denied"], 1,
            "counts describe the session, not the window: {again:?}"
        );
    }

    #[test]
    fn the_request_log_is_refused_while_a_human_is_logging_in() {
        // It names URLs a login flow visited. Engine-written, but still a
        // reading of where the page went.
        let mut session = session_with("<html><body><p>hi</p></body></html>");
        session.login = true;
        let (reply, _) = control_verb(&mut session, &json!({"verb": "requests"}));
        assert_eq!(reply["ok"], false, "{reply:?}");
        assert_eq!(reply["code"], "login-mode", "{reply:?}");
    }

    #[test]
    fn a_ref_from_a_reading_the_page_has_moved_on_from_is_refused() {
        // The defect this check exists for, reproduced.
        //
        // Refs are minted by walk order, and the action verbs take a *fresh*
        // snapshot to get a live node id. So when the page inserts an element
        // earlier in document order, every later ref shifts by one: the agent's
        // `@e2` now names what used to be `@e1`. Before this check the click
        // landed on that other element and the reply said `ok`.
        let mut session = scripted_session_with(
            "<html><body><div id='top'></div>\
             <button id='b'>Add</button>\
             <a href='/second'>second</a>\
             <script>document.querySelector('#b').addEventListener('click', () => {\
               const a = document.createElement('a');\
               a.setAttribute('href', '/first');\
               a.textContent = 'first';\
               document.querySelector('#top').appendChild(a);\
             });</script></body></html>",
        );

        let refs = serve_refs(&mut session);
        let button = refs
            .iter()
            .find(|r| r.name == "Add")
            .expect("the button has a ref")
            .clone();
        let second = refs
            .iter()
            .find(|r| r.name == "second")
            .expect("the link has a ref")
            .clone();

        // Click the button. Its handler inserts a link *above* it, so the walk
        // renumbers and `second`'s id now belongs to something else.
        let (reply, _) = control_verb(
            &mut session,
            &json!({"verb": "click", "ref": button.id.clone()}),
        );
        assert_eq!(reply["ok"], true, "{reply:?}");

        let after = session.page.snapshot();
        let now = after.resolve(&second.id).expect("the id still resolves");
        assert_ne!(
            now.node_id, second.node_id,
            "the fixture must actually renumber, or this test proves nothing"
        );

        // Acting on the ref the agent read is refused, by name, rather than
        // acting on whatever that id happens to mean now.
        let (reply, changed) = control_verb(
            &mut session,
            &json!({"verb": "click", "ref": second.id.clone()}),
        );
        assert_eq!(reply["ok"], false, "{reply:?}");
        assert_eq!(reply["code"], "stale-ref", "{reply:?}");
        assert_eq!(reply["retryable"], true);
        assert!(!changed, "a refused verb does not move the page");
        let text = reply["error"].as_str().unwrap();
        assert!(text.contains("snapshot"), "no recovery named: {text:?}");

        // And the loop an agent is supposed to run works: read again, act on
        // what that reading gave you.
        let fresh = serve_refs(&mut session);
        let second_again = fresh
            .iter()
            .find(|r| r.name == "second")
            .expect("still on the page");
        let (reply, _) = control_verb(
            &mut session,
            &json!({"verb": "click", "ref": second_again.id.clone()}),
        );
        assert_ne!(
            reply["code"], "stale-ref",
            "a ref from the current reading must be honoured: {reply:?}"
        );
    }

    #[test]
    fn a_ref_survives_everything_that_does_not_renumber_it() {
        // The check has to be precise or it is a nuisance: typing into one
        // field and then submitting is the login loop, and it must not require
        // a re-read between every step.
        let mut session = session_with(
            "<html><body><form action='/in' method='post'>\
             <input name='user'><input name='pass' type='password'>\
             <button type='submit'>Sign in</button></form></body></html>",
        );
        let refs = serve_refs(&mut session);
        assert_eq!(refs.len(), 3, "user, pass, submit");

        for (index, text) in [(0usize, "alice"), (1, "hunter2")] {
            let (reply, _) = control_verb(
                &mut session,
                &json!({"verb": "type", "ref": refs[index].id.clone(), "text": text}),
            );
            assert_eq!(reply["ok"], true, "step {index}: {reply:?}");
        }

        // Scrolling does not touch the DOM either.
        let (reply, _) = control_verb(&mut session, &json!({"verb": "scroll", "by": 50.0}));
        assert_eq!(reply["ok"], true, "{reply:?}");

        let (reply, _) = control_verb(
            &mut session,
            &json!({"verb": "submit", "ref": refs[2].id.clone()}),
        );
        assert_ne!(
            reply["code"], "stale-ref",
            "typing and scrolling do not renumber refs: {reply:?}"
        );
    }

    #[test]
    fn typing_into_one_ref_twice_is_the_documented_retry_and_must_work() {
        // README: "`type` replaces the field rather than appending, so retrying
        // after a failed submit does not produce `alicealice`." That retry
        // types into the *same* ref twice.
        let mut session = session_with(
            "<html><body><form><input name='user'></form></body></html>",
        );
        let refs = serve_refs(&mut session);
        let id = refs[0].id.clone();

        let (first, _) = control_verb(
            &mut session,
            &json!({"verb": "type", "ref": id.clone(), "text": "alice"}),
        );
        assert_eq!(first["ok"], true, "{first:?}");

        let (second, _) = control_verb(
            &mut session,
            &json!({"verb": "type", "ref": id, "text": "bob"}),
        );
        assert_eq!(
            second["ok"], true,
            "the documented retry was refused: {second:?}"
        );
    }

    #[test]
    fn acting_on_a_ref_before_reading_one_is_refused_as_its_own_thing() {
        // Distinguished from a stale ref because the fix differs: this is
        // "take a snapshot", that is "take another one".
        let mut session = session_with(tall_page());
        let (reply, changed) = control_verb(&mut session, &json!({"verb": "click", "ref": "e1"}));
        assert_eq!(reply["code"], "no-snapshot", "{reply:?}");
        assert!(!changed);
    }

    #[test]
    fn an_unknown_verb_is_named_and_the_known_ones_are_listed() {
        let mut session = session_with(tall_page());
        let (reply, changed) = control_verb(&mut session, &json!({"verb": "typewrite"}));
        assert_eq!(reply["code"], "unknown-verb", "{reply:?}");
        assert!(!changed);
        let text = reply["error"].as_str().unwrap();
        for verb in crate::verbs::Verb::ALL {
            assert!(text.contains(verb.name()), "{} not listed", verb.name());
        }
    }

    #[test]
    fn nothing_is_encoded_when_nobody_is_watching() {
        // The property that makes this engine cheap to leave open has to
        // survive the control channel: an agent driving a headless session must
        // not pay for JPEGs that no viewer will ever receive.
        let mut session = session_with(tall_page());
        let mut nobody = HashMap::new();
        let before = session.seq;
        broadcast_change(&mut session, &mut nobody);
        assert_eq!(session.seq, before, "no viewers, no frame");
    }
}

#[cfg(test)]
mod delta_and_login_tests {
    use super::*;

    fn page_session(html: &str) -> Session {
        let requests = std::sync::Arc::new(crate::receipt::MemorySink::new());
        let broker = std::sync::Arc::new(
            crate::net::Broker::new(crate::policy::Policy::new(), requests.clone(), None)
                .expect("broker"),
        );
        let fonts = crate::fonts::load(&[], &crate::fonts::default_font_dirs(), Some(2));
        let factory = crate::engine::PageFactory::new(
            broker,
            fonts.sources.clone(),
            crate::engine::PageOptions::default(),
        );
        let base = url::Url::parse("https://app.example/").unwrap();
        let page = factory.from_html(html, &base);
        Session {
            factory,
            page,
            quality: 70,
            seq: 0,
            actions: None,
            last_snapshot: None,
            served_refs: None,
            unknown_verbs: std::collections::BTreeMap::new(),
        recording: crate::replay::Recording::default(),
            requests,
            secrets: crate::secrets::Secrets::default(),
            login: false,
        }
    }

    /// Change the page the way the engine itself does, so the test exercises
    /// the real mutation path rather than a rebuilt document.
    fn replace_inner_html(session: &mut Session, selector: &str, html: &str) {
        let dom = session.page.dom();
        let id = {
            let doc = dom.borrow();
            doc.query_selector_all(selector)
                .ok()
                .and_then(|ids| ids.first().copied())
        };
        let id = id.unwrap_or_else(|| panic!("no node matched `{selector}`"));
        {
            let mut doc = dom.borrow_mut();
            doc.mutate().set_inner_html(id, html);
        }
        session.page.refresh();
    }

    #[test]
    fn the_first_snapshot_is_full_and_the_next_one_is_the_difference() {
        let mut session = page_session(
            "<html><body><p>one</p><p>two</p><p>three</p><p>four</p><p>five</p></body></html>",
        );

        // Nothing to difference against yet, so the outline is the answer and
        // the reply says so rather than sending an empty delta.
        let (first, _) = control_verb(&mut session, &json!({"verb": "snapshot", "delta": true}));
        assert_eq!(first["kind"], "full");
        assert!(first["reason"].is_string(), "{first:?}");

        // Nothing happened between the two reads.
        let (second, _) = control_verb(&mut session, &json!({"verb": "snapshot", "delta": true}));
        assert_eq!(second["kind"], "delta");
        assert!(
            second["text"].as_str().unwrap().contains("no change"),
            "an agent that changed nothing needs to be told so, not handed the page again: {}",
            second["text"]
        );
    }

    #[test]
    fn a_small_change_is_reported_as_a_small_change() {
        let mut session = page_session(
            "<html><body><ul><li>one</li><li>two</li><li>three</li><li>four</li>\
             <li>five</li><li>six</li><li>seven</li><li>eight</li></ul></body></html>",
        );
        control_verb(&mut session, &json!({"verb": "snapshot", "delta": true}));

        replace_inner_html(&mut session, "li:nth-child(2)", "TWO CHANGED");

        let (reply, _) = control_verb(&mut session, &json!({"verb": "snapshot", "delta": true}));
        assert_eq!(reply["kind"], "delta");
        let text = reply["text"].as_str().unwrap();
        assert!(text.contains("TWO CHANGED"), "{text}");
        assert!(!text.contains("seven"), "unchanged lines should not be re-sent: {text}");
        // And it is still fenced: every added line came from the page.
        assert!(text.contains(crate::snapshot::CONTENT_BEGIN), "{text}");
    }

    #[test]
    fn a_page_that_replaced_itself_gets_the_outline_not_a_difference() {
        let mut session = page_session(
            "<html><body><p>alpha</p><p>beta</p><p>gamma</p><p>delta</p></body></html>",
        );
        control_verb(&mut session, &json!({"verb": "snapshot", "delta": true}));

        replace_inner_html(&mut session, "body", "<p>wholly</p><p>different</p><p>now</p>");

        // A difference as long as the page is technically true and useless.
        let (reply, _) = control_verb(&mut session, &json!({"verb": "snapshot", "delta": true}));
        assert_eq!(reply["kind"], "full");
        assert!(reply["text"].as_str().unwrap().contains("wholly"));
    }

    /// The refusal is what a person reads to decide whether typing a password
    /// here is safe, so it has to name the half that is not enforced. Frames
    /// keep streaming by design, and the viewer socket is inside the box.
    #[test]
    fn the_login_refusal_does_not_claim_the_frames_are_withheld() {
        let refusal = Session::login_refusal(Verb::Snapshot);
        assert_eq!(refusal["login"], true);
        assert_eq!(
            refusal["frames_withheld"], false,
            "the mode must not imply it hides the live view"
        );
        let text = refusal["error"].as_str().expect("a reason");
        assert!(text.contains("live view still streams"), "{text}");
        assert!(text.contains("viewer socket"), "{text}");
    }

    #[test]
    fn login_mode_closes_the_page_to_the_agent_and_opens_again_on_request() {
        let mut session = page_session("<html><body><p>secret form</p></body></html>");

        let (on, _) = control_verb(&mut session, &json!({"verb": "login", "on": true}));
        assert_eq!(on["login"], true);

        // The page is not readable, and every way of reading it is refused —
        // not only the one an honest client would use.
        for verb in ["snapshot", "scroll", "click", "type", "submit", "navigate"] {
            let (reply, _) = control_verb(&mut session, &json!({"verb": verb}));
            assert_eq!(reply["ok"], false, "`{verb}` should be refused");
            assert_eq!(reply["login"], true, "and should say why: {reply:?}");
        }

        // Status still answers, or the mode could not be observed...
        let (status, _) = control_verb(&mut session, &json!({"verb": "status"}));
        assert_eq!(status["ok"], true);
        assert_eq!(status["login"], true);

        // ...and login still answers, or the mode could not be left.
        let (off, _) = control_verb(&mut session, &json!({"verb": "login", "on": false}));
        assert_eq!(off["login"], false);
        let (reply, _) = control_verb(&mut session, &json!({"verb": "snapshot"}));
        assert_eq!(reply["ok"], true);
        assert!(reply["text"].as_str().unwrap().contains("secret form"));
    }

    #[test]
    fn login_mode_reports_the_session_it_established_without_revealing_it() {
        let mut session = page_session("<html><body><p>x</p></body></html>");
        control_verb(&mut session, &json!({"verb": "login", "on": true}));
        let (off, _) = control_verb(&mut session, &json!({"verb": "login", "on": false}));

        // How many, never which — the same rule `status` follows.
        assert!(off["cookies"].is_number(), "{off:?}");
        let rendered = off.to_string();
        assert!(!rendered.contains("Set-Cookie"), "{rendered}");
    }

    #[test]
    fn a_delta_across_a_login_is_not_offered() {
        let mut session = page_session("<html><body><p>before</p></body></html>");
        control_verb(&mut session, &json!({"verb": "snapshot", "delta": true}));
        control_verb(&mut session, &json!({"verb": "login", "on": true}));
        control_verb(&mut session, &json!({"verb": "login", "on": false}));

        // The baseline is dropped, because a difference across a login would
        // describe the page the human just used.
        let (reply, _) = control_verb(&mut session, &json!({"verb": "snapshot", "delta": true}));
        assert_eq!(reply["kind"], "full");
    }
}
