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
use std::thread;

use base64::Engine as _;
use h5i_error::H5iError;
use serde_json::{json, Value};
use url::Url;

use crate::engine::{Page, PageFactory};
use crate::receipt::ActionLog;
use crate::ws::{self, Incoming};

/// How far a key scrolls, as a fraction of the viewport.
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
    fn login_refusal(verb: &str) -> Value {
        json!({
            "ok": false,
            "error": format!(
                "`{verb}` is refused while this session is in login mode: a credential typed \
                 into a page the agent can read has been given to the agent. The live view \
                 still streams — the person typing has to see the page — so this refuses the \
                 control path and not an agent that attaches to the viewer socket itself. End \
                 login mode with `session login --off` and reads resume, with whatever session \
                 the login established still in the jar."
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
                    .unwrap_or_else(|_| error_reply("the session ended before it answered"))
            }
            Err(error) => error_reply(&format!("not JSON: {error}")),
        };
        writeln!(writer, "{answer}").map_err(H5iError::Io)?;
        writer.flush().map_err(H5iError::Io)?;
    }
    Ok(())
}

fn error_reply(message: &str) -> Value {
    json!({"ok": false, "error": message})
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
        return control_verb(session, request);
    };

    let seq = match log.begin(verb, target.as_deref()) {
        Ok(seq) => seq,
        // No record, no action. The agent is told why in the same shape as any
        // other refusal, so this does not read as the verb having half-happened.
        Err(error) => {
            return (
                error_reply(&format!(
                    "refusing to act: the action could not be recorded: {error}"
                )),
                false,
            )
        }
    };

    let (answer, changed) = control_verb(session, request);

    let ok = answer.get("ok").and_then(Value::as_bool).unwrap_or(false);
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
    (answer, changed)
}

/// Handle one control request against the resident page.
///
/// Returns the reply and whether the page moved, because the caller is the only
/// thing that knows who else is watching.
fn control_verb(session: &mut Session, request: &Value) -> (Value, bool) {
    let verb = request.get("verb").and_then(Value::as_str).unwrap_or("");

    // Reads are refused while a human is typing a credential. `status` and
    // `login` are not reads of the page: one reports the mode, the other is how
    // it ends, and refusing either would make the mode impossible to leave.
    if session.login && !matches!(verb, "status" | "login") {
        return (Session::login_refusal(verb), false);
    }

    match verb {
        // What the session is, for a client that just connected.
        "status" => (
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
            }),
            false,
        ),

        // Hand the page to the human for as long as it takes to log in.
        //
        // §B10 of ROADMAP.md listed this as overdue rather than pending: it
        // was supposed to arrive with the cookie jar, because a jar is what
        // makes logging in worth doing and a readable page is what makes it
        // unsafe.
        "login" => {
            let on = request.get("on").and_then(Value::as_bool).unwrap_or(true);
            session.login = on;
            // The baseline is dropped either way. A delta across a login would
            // describe the page a human just used, which is the one thing this
            // mode exists to keep out of the agent's hands.
            session.last_snapshot = None;
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

        "snapshot" => {
            let snapshot = session.page.snapshot();
            let wants_delta = request.get("delta").and_then(Value::as_bool).unwrap_or(false);

            // The difference, when one was asked for and there is a baseline to
            // difference against. Three hundred lines re-read after every click
            // — of which four are new — is the wrong shape for an agent loop.
            let delta = wants_delta
                .then(|| session.last_snapshot.as_ref().map(|prev| snapshot.delta(prev)))
                .flatten();

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

            session.last_snapshot = Some(snapshot);
            (reply, false)
        }

        // Scrolling is the one thing a viewer could do that an agent could not,
        // which made "look further down the page" a request only a human could
        // make. `moved` is reported rather than assumed: a scroll at the bottom
        // of a document changes nothing, and an agent that cannot tell will
        // loop asking for more page that does not exist.
        "scroll" => {
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

        "navigate" => {
            let Some(target) = request.get("url").and_then(Value::as_str) else {
                return (error_reply("navigate needs a `url`"), false);
            };
            // Resolved against the current page, so an agent may say
            // `/docs` for the same reason a person may click one.
            let resolved = match session.page.url().join(target) {
                Ok(url) => url,
                Err(error) => return (error_reply(&format!("`{target}` is not a URL: {error}")), false),
            };
            match session.factory.open(&resolved) {
                Ok(page) => {
                    session.page = page;
                    (json!({"ok": true, "url": session.page.url().to_string()}), true)
                }
                // A refusal is an answer, not a crash: the allowlist saying no
                // is the engine working, and the agent needs to read it as one.
                Err(error) => (error_reply(&format!("{error}")), false),
            }
        }

        // Typing and submitting are the pair that make a login reachable: a
        // session an agent cannot type into stops at the first form, so these
        // ship together or neither is worth having.
        "type" => {
            let Some(reference) = request.get("ref").and_then(Value::as_str) else {
                return (error_reply("type needs a `ref`"), false);
            };
            let Some(text) = request.get("text").and_then(Value::as_str) else {
                return (error_reply("type needs `text`"), false);
            };
            let snapshot = session.page.snapshot();
            let Some(entry) = snapshot.resolve(reference) else {
                return (
                    error_reply(&format!("no such ref `{reference}` on this page")),
                    false,
                );
            };
            let node_id = entry.node_id;
            let role = entry.role.clone();
            if !session.page.type_into(node_id, text) {
                return (
                    error_reply(&format!("`{reference}` is a {role}, not a field to type into")),
                    false,
                );
            }
            (json!({"ok": true, "ref": reference}), true)
        }

        "submit" => {
            let Some(reference) = request.get("ref").and_then(Value::as_str) else {
                return (error_reply("submit needs a `ref` inside the form"), false);
            };
            let snapshot = session.page.snapshot();
            let Some(entry) = snapshot.resolve(reference) else {
                return (
                    error_reply(&format!("no such ref `{reference}` on this page")),
                    false,
                );
            };
            let node_id = entry.node_id;
            let submission = match session.page.submit_form(node_id) {
                Ok(submission) => submission,
                Err(error) => return (error_reply(&format!("{error}")), false),
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
                Err(error) => (error_reply(&format!("{error}")), false),
            }
        }

        "click" => {
            let Some(reference) = request.get("ref").and_then(Value::as_str) else {
                return (error_reply("click needs a `ref`"), false);
            };
            let snapshot = session.page.snapshot();
            let Some(entry) = snapshot.resolve(reference) else {
                return (
                    error_reply(&format!("no such ref `{reference}` on this page")),
                    false,
                );
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
                    error_reply(&format!("`{reference}` is a {role} with nothing to follow")),
                    false,
                );
            };
            let resolved = match session.page.url().join(&href) {
                Ok(url) => url,
                Err(error) => return (error_reply(&format!("`{href}` is not a URL: {error}")), false),
            };
            match session.factory.open(&resolved) {
                Ok(page) => {
                    session.page = page;
                    (json!({"ok": true, "url": session.page.url().to_string()}), true)
                }
                Err(error) => (error_reply(&format!("{error}")), false),
            }
        }

        other => (
            error_reply(&format!(
                "`{other}` is not a verb this engine has (status, snapshot, navigate, \
                 scroll, type, submit, click)"
            )),
            false,
        ),
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
        let broker = Arc::new(
            Broker::new(Policy::new(), Arc::new(MemorySink::new()), None).expect("broker"),
        );
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
            login: false,
        }
    }

    /// A session whose page runs its own scripts, for the verbs that behave
    /// differently once script is present.
    fn scripted_session_with(html: &str) -> Session {
        let broker = Arc::new(
            Broker::new(Policy::new(), Arc::new(MemorySink::new()), None).expect("broker"),
        );
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
            login: false,
        }
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
        // into, not merely that it failed.
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

        let reference = session
            .page
            .snapshot()
            .refs
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
        let reference = session.page.snapshot().refs[0].id.clone();
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
        let reference = session.page.snapshot().refs[0].id.clone();
        let (reply, changed) =
            control_verb(&mut session, &json!({"verb": "click", "ref": reference}));

        // Refused by policy, which is the navigation path reporting itself
        // rather than the click being silently swallowed by the event path.
        assert_eq!(reply["ok"], false, "{reply:?}");
        assert!(!changed);
        assert!(reply["error"].as_str().unwrap().contains("denied.test"), "{reply:?}");
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
        let broker = std::sync::Arc::new(
            crate::net::Broker::new(
                crate::policy::Policy::new(),
                std::sync::Arc::new(crate::receipt::MemorySink::new()),
                None,
            )
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
        let refusal = Session::login_refusal("snapshot");
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
