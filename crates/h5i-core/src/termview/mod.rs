//! The terminal viewer: watch a box's browser, and take over, without leaving
//! the terminal.
//!
//! The web viewer ([`crate::view`]) binds a loopback port, gates it with a token
//! and serves a page the human opens in *their own browser*, which makes the
//! watching program the most credential-laden one on the host. This viewer is a
//! client of the same stream, in the process the human already ran:
//!
//! ```text
//!   h5i box view --term
//!     └── connect_in_netns(box pid, stream port)   no listener, no token
//!           └── WebSocket client ── JPEG frames ──► decode ──► Kitty graphics
//!                                 ◄── input ────── control lock ◄── terminal
//! ```
//!
//! Nothing is bound. The socket comes back over `SCM_RIGHTS` from a fork that
//! entered the box's namespaces, so it is a descriptor this process holds. The
//! trusted path runs both ways: the box supplies compressed pixels and nothing
//! else, and every escape sequence is generated here (see [`kitty`]), so a box
//! cannot reach the host's PTY even in principle. The status row cannot be
//! painted over, the page being an image below row two while row one is
//! [`status`].
//!
//! It is not a boundary of its own: it watches a box at whatever tier that box
//! runs.

// Portable: these parse, encode and decode, and none of them touch a terminal.
pub mod image;
pub mod input;
pub mod kitty;
pub mod proto;
pub mod overlay;
pub mod panes;
pub mod status;
pub mod vim;
pub mod ws;

// Raw mode, `termios` and `TIOCGWINSZ` have no Windows equivalent worth
// pretending to. The crate is kept building there (see `view.rs`), so the
// terminal half is gated here and [`run`] has a stub that says so, rather than
// the module vanishing and taking its callers' type names with it.
#[cfg(unix)]
pub mod term;

use std::path::PathBuf;

use crate::error::H5iError;

#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::sync::mpsc;
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
use crate::control::{self, Holder};
#[cfg(unix)]
use status::{Mode, Status};

/// How long to wait for the terminal to answer the graphics probe.
#[cfg(unix)]
const PROBE_TIMEOUT: Duration = Duration::from_millis(600);

/// The clock the render loop runs on when nothing else is happening: it
/// refreshes the status line, notices a resize, and picks up a control-lock
/// change made by another process.
#[cfg(unix)]
const TICK: Duration = Duration::from_millis(250);

// Rows the viewer keeps for itself (status line + separator) live in `panes`,
// which is also what computes the split. Two copies of "how many rows the
// chrome takes" is a screen that overlaps by one row the first time either
// changes.

/// Where the browser being watched is, and how this process reaches its frames.
///
/// The two arms are not two transports for one thing. They are two different
/// security stories, and the viewer has to be able to say which one it is in
/// rather than presenting them identically:
///
/// * A boxed session's stream port is advertised inside the box's own `/tmp`
///   and reached through a fork that enters the box's namespaces, so nothing is
///   bound on the host and no token is minted. What the page may reach is
///   enforced outside the engine, and the status line can say so.
/// * A host session's engine is on this machine's loopback, because there is no
///   namespace between the two. There is also no boundary: the engine's own
///   confinement is what is holding it, and an egress claim here rests on the
///   engine's word. The status line says that too. The alternative, showing the
///   same chrome for both, would be the viewer implying containment that is not
///   there.
#[derive(Debug, Clone)]
pub enum Attach {
    /// A browser inside an h5i box.
    Boxed,
    /// A browser session on this machine, whose engine wrote its stream port
    /// into this file.
    Host { stream_file: PathBuf },
}

/// What a viewer needs to know to attach to one browser.
pub struct Options {
    /// Where `control.json` lives and where the viewer receipt is filed: a
    /// box's env directory, or a browser session's own directory.
    pub state_dir: PathBuf,
    /// What is being watched, for the status line and the receipt.
    pub subject: String,
    pub policy_digest: String,
    /// How to reach the frames.
    pub attach: Attach,
    /// The command the human actually typed, for the receipt.
    pub command: String,
    /// A short account of what the page may reach, for the status line.
    pub egress: String,
    /// Frame-rate ceiling asked of the box.
    pub max_fps: u32,
    /// Skip the graphics probe and render anyway.
    ///
    /// An escape hatch, not a feature: detection is a heuristic about someone
    /// else's terminal, and being wrong about it must not be the end of the
    /// road for a user who knows better than we do.
    pub assume_graphics: bool,
    /// The engine this box is pinned to, when it has one. Read only to tell
    /// someone how to start a stream. The viewer itself is engine-agnostic,
    /// and adding this must not become the start of engine-specific rendering.
    pub engine: Option<String>,
}

/// Find this browser's stream, with a sentence a human can act on if it is not
/// there.
///
/// The resolution itself lives in [`crate::view::Route`], shared with the web
/// forward. What is here is the *refusals*, which differ between the two ways
/// of attaching in a way the shared type should not have to know about: a box
/// that is not running is a different problem, with a different fix, from a
/// session that never became resident.
#[cfg(unix)]
fn resolve_route(opts: &Options) -> Result<crate::view::Route, H5iError> {
    match &opts.attach {
        Attach::Boxed => {
            let (pid, pid_ns) = crate::view::box_pid_ns(&opts.state_dir).ok_or_else(|| {
                H5iError::Metadata(
                    "this box is not running, so there is no browser to watch. \
                     Start a session (`h5i box shell <name>`) and try again."
                        .into(),
                )
            })?;
            let port = crate::view::stream_port(&opts.state_dir)
                .ok_or_else(|| H5iError::Metadata(not_streaming_hint(opts.engine.as_deref())))?;
            Ok(crate::view::Route::Boxed { pid, pid_ns, port })
        }
        Attach::Host { stream_file } => {
            let port = crate::view::session_stream_port(stream_file).ok_or_else(|| {
                H5iError::Metadata(
                    "this session is not serving a live view, so there is nothing to watch. \
                     Only a resident session does: open one with `h5i browser open <url>` \
                     and try again."
                        .into(),
                )
            })?;
            Ok(crate::view::Route::Host { port })
        }
    }
}

/// What to tell someone whose box has no `.stream` file yet.
///
/// The advice is engine-specific because the command is: an `h5i-light` box has
/// no agent-browser daemon to enable streaming on, and telling its owner to run
/// `agent-browser stream enable` sends them to a CLI that will fail on a missing
/// socket directory before it reaches the question they asked.
///
/// Unix-gated with the `run` that calls it, following this file's rule: the
/// non-unix `run` is a stub that refuses before it could need advice about
/// streaming.
#[cfg(unix)]
fn not_streaming_hint(engine: Option<&str>) -> String {
    match engine {
        Some("h5i-light") => "the box's browser is not streaming. Inside the box, run \
                              `h5i browser open <url>`, then try again."
            .into(),
        _ => "the box's browser is not streaming. Inside the box, run \
              `agent-browser stream enable`, then try again."
            .into(),
    }
}

/// The render loop's own clock.
///
/// A type rather than two local variables so the tick cannot be attached to the
/// socket again by accident, which is what it was: the tick was whatever
/// `recv_timeout` *expiring* meant, so a box sending frames faster than [`TICK`]
/// suppressed it entirely. At the default 10 fps a frame lands every 100 ms and
/// the 250 ms timeout never elapses, so the status line stopped refreshing, a
/// resize went unnoticed and a lone Escape was never flushed.
#[cfg(unix)]
struct Ticker {
    /// When the next tick is owed.
    next: Instant,
    /// When the keyboard was last heard from. Tracked apart from the tick
    /// because "the input has gone quiet" is a claim about the keyboard, and
    /// letting the frame rate answer it is how the bug above got in.
    last_input: Instant,
}

#[cfg(unix)]
impl Ticker {
    fn start(now: Instant) -> Ticker {
        Ticker {
            next: now + TICK,
            last_input: now,
        }
    }

    /// How long the loop may block waiting for the next event.
    fn wait(&self, now: Instant) -> Duration {
        self.next.saturating_duration_since(now)
    }

    fn saw_input(&mut self, now: Instant) {
        self.last_input = now;
    }

    /// `Some(quiet)` when a tick is owed. `quiet` says the keyboard has been
    /// still for a whole tick, which is the only way to tell a lone `ESC` from
    /// the start of a sequence nobody finished.
    ///
    /// The next tick is scheduled from *now* rather than from the deadline that
    /// just passed, so a loop that falls behind ticks less often instead of
    /// owing a burst it then has to catch up on.
    fn due(&mut self, now: Instant) -> Option<bool> {
        if now < self.next {
            return None;
        }
        self.next = now + TICK;
        Some(now.duration_since(self.last_input) >= TICK)
    }
}

/// Everything the render loop is waiting on, from whichever thread saw it.
#[cfg(unix)]
enum Ev {
    /// A message from the box.
    Net(ws::Incoming),
    /// The box's stream ended, or failed.
    NetDone(Option<String>),
    /// Bytes from the terminal.
    Input(Vec<u8>),
    /// The terminal's input ended.
    InputDone,
}

/// Attach to a box and run until the human leaves.
#[cfg(unix)]
pub fn run(opts: Options) -> Result<(), H5iError> {
    let stdin = term::stdin_fd();
    if !term::is_tty(stdin) || !term::is_tty(std::io::stdout().as_raw_fd()) {
        return Err(H5iError::Metadata(
            "the terminal viewer needs a terminal on both stdin and stdout. \
             In a pipe or a script, use `h5i box view` and open the URL it prints."
                .into(),
        ));
    }

    // Everything that can fail with an explanation is resolved before the
    // terminal is touched, so a failure prints a sentence on a normal screen
    // rather than on an alternate one that is about to be torn down. That
    // includes finding the stream, which is the step that fails most often and
    // whose message differs the most between the two ways of attaching.
    let route = resolve_route(&opts)?;

    let mut guard = term::Guard::enter(stdin).map_err(H5iError::Io)?;
    // Skipping the probe means skipping the answer about compression too. Raw
    // is the conservative choice and the only honest one: the flag exists for
    // the case where we were wrong about this terminal, so it must not then
    // assume something else about it. Frames cost about six times as much.
    let encoding = if opts.assume_graphics {
        kitty::Encoding::Raw
    } else {
        probe_graphics(stdin)?
    };

    let mut socket = route.connect()?;
    let port = route.port();
    let key = ws::new_key();
    socket
        .write_all(ws::request(&format!("127.0.0.1:{port}"), "/", &key).as_bytes())
        .map_err(H5iError::Io)?;
    socket.flush().map_err(H5iError::Io)?;
    let head = ws::read_head(&mut socket).map_err(H5iError::Io)?;
    ws::verify_response(&head, &key)?;

    let mut writer = socket.try_clone().map_err(H5iError::Io)?;
    ws::send_text(&mut writer, &proto::config_ack_pacing(opts.max_fps).to_string())
        .map_err(H5iError::Io)?;

    let (tx, rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));

    let net_tx = tx.clone();
    let net = std::thread::spawn(move || {
        let mut reader = ws::Reader::new(socket);
        loop {
            match reader.next_message() {
                Ok(Some(msg)) => {
                    if net_tx.send(Ev::Net(msg)).is_err() {
                        return;
                    }
                }
                Ok(None) => {
                    let _ = net_tx.send(Ev::NetDone(None));
                    return;
                }
                Err(e) => {
                    let _ = net_tx.send(Ev::NetDone(Some(e.to_string())));
                    return;
                }
            }
        }
    });

    let input_stop = Arc::clone(&stop);
    let input_tx = tx.clone();
    std::thread::spawn(move || {
        while !input_stop.load(Ordering::Relaxed) {
            // A bounded wait, so the thread notices the stop flag rather than
            // sitting in a read that only the next keystroke would end.
            if !term::wait_readable(stdin, TICK) {
                continue;
            }
            let mut buf = Vec::new();
            match term::read_available(stdin, &mut buf) {
                Ok(0) => {
                    let _ = input_tx.send(Ev::InputDone);
                    return;
                }
                Ok(_) => {
                    if input_tx.send(Ev::Input(buf)).is_err() {
                        return;
                    }
                }
                Err(_) => {
                    let _ = input_tx.send(Ev::InputDone);
                    return;
                }
            }
        }
    });
    drop(tx);

    let opened = chrono::Utc::now();
    let holder_at_open = control::read(&opts.state_dir).holder;
    // Scoped so the app releases its borrow of the terminal guard before the
    // guard itself is dropped and the terminal is restored.
    let (outcome, bytes_in, input_sent) = {
        let mut app = App::new(&opts, &mut guard, encoding);
        let outcome = app.pump(rx, &mut writer);

        // Leave the page as we found it. A viewer that exits still holding the
        // lock leaves the agent refusing to act, with nothing on screen to
        // explain why.
        // Asked of the mode rather than matched here, so a mode added later
        // cannot leave the lock held by forgetting to be listed. INSERT drives
        // the page too, and a viewer that exited from it still holding the lock
        // would leave the agent refusing to act with nothing on screen to
        // explain why.
        if app.mode.holds_control() {
            let _ = control::release(&opts.state_dir);
        }
        let _ = writer.write_all(&app.placer.clear());
        (outcome, app.bytes_in, app.input_sent)
    };
    let _ = ws::send_close(&mut writer);
    stop.store(true, Ordering::Relaxed);
    let _ = writer.shutdown(std::net::Shutdown::Both);
    let _ = net.join();
    drop(guard);

    crate::view::record_session(
        &crate::view::Session {
            env_dir: opts.state_dir.clone(),
            env_id: opts.subject.clone(),
            policy_digest: opts.policy_digest.clone(),
            transport: crate::view::Transport::Terminal,
            command: opts.command.clone(),
        },
        opened,
        holder_at_open,
        bytes_in,
        &crate::view::Pump {
            input_frames: input_sent,
            error: outcome.clone(),
        },
    );

    match outcome {
        // The box's browser shutting down is how this ends most of the time.
        // It is not a failure and must not print like one.
        None => Ok(()),
        Some(e) => Err(H5iError::Metadata(format!(
            "the box's browser stream ended: {e}"
        ))),
    }
}

/// Not available where there is no `termios` to put into raw mode.
#[cfg(not(unix))]
pub fn run(_opts: Options) -> Result<(), H5iError> {
    Err(H5iError::Metadata(
        "the terminal viewer needs a unix terminal. Use `h5i box view` and open the URL \
         it prints."
            .into(),
    ))
}

/// Ask the terminal whether it can draw images and whether it accepts deflated
/// pixels, and refuse politely if it cannot draw at all.
///
/// The read runs to the device-attributes reply rather than stopping at the
/// first graphics answer. Both questions are in flight, and returning on the
/// first would decide compression before its answer had arrived, which is not
/// a slow path, it is a permanently wrong one.
#[cfg(unix)]
fn probe_graphics(fd: std::os::fd::RawFd) -> Result<kitty::Encoding, H5iError> {
    let mut out = std::io::stdout();
    let _ = out.write_all(kitty::probe_sequence().as_bytes());
    let _ = out.flush();

    let mut seen = Vec::new();
    let deadline = Instant::now() + PROBE_TIMEOUT;
    while Instant::now() < deadline && !kitty::probe_done(&seen) {
        if !term::wait_readable(fd, Duration::from_millis(50)) {
            continue;
        }
        if term::read_available(fd, &mut seen).unwrap_or(0) == 0 {
            break;
        }
    }

    match kitty::classify_probe(&seen) {
        kitty::Support::Yes if kitty::accepts_zlib(&seen) => Ok(kitty::Encoding::Zlib),
        // It draws, it just will not inflate. Six times the bytes, and every
        // one of them arrives.
        kitty::Support::Yes => Ok(kitty::Encoding::Raw),
        // A terminal that said no, or that answered neither question. Treating
        // silence as support would fill someone's screen with base64.
        kitty::Support::No | kitty::Support::Undecided => Err(unsupported()),
    }
}

#[cfg(unix)]
fn unsupported() -> H5iError {
    H5iError::Metadata(
        "this terminal does not support the Kitty graphics protocol, so there is nowhere \
         to draw the page. Terminals that do include kitty, Ghostty, WezTerm and Konsole. \
         Otherwise use `h5i box view`, which serves the same stream to a browser. \
         If you know your terminal supports it and the probe is wrong, \
         pass `--assume-graphics`."
            .into(),
    )
}

/// The roles `gi` will accept as "a field to type into".
///
/// The roles [`crate::browser`]'s outline mints for text-bearing controls. A
/// list rather than "the first hint", because the first actionable thing on a
/// page is almost never a field and `gi` that focused a navigation link would
/// be a key nobody could use twice.
#[cfg(unix)]
const FIELD_ROLES: &[&str] = &["textbox", "searchbox", "combobox"];

/// Collapse a page-derived string to one line for the status row.
///
/// The row's whole claim is that nothing from the page can put a pixel in it,
/// and [`status`] sanitizes escape sequences on the way in. This is the other
/// half: a name carrying a newline would push the row off its line.
#[cfg(unix)]
fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The render loop's state.
#[cfg(unix)]
struct App<'a> {
    env_dir: PathBuf,
    guard: &'a mut term::Guard,
    placer: kitty::Placer,
    mode: Mode,
    /// The last frame, kept so a resize can redraw without waiting for the box
    /// to send another. A static page sends nothing at all.
    last_frame: Option<Vec<u8>>,
    /// Developer layout: page plus what the page said.
    developer: bool,
    /// Console errors and page exceptions, bounded.
    log: panes::LogBuffer,
    /// Where the page currently sits on screen, for mapping clicks back.
    mapping: Option<input::Mapping>,
    viewport: (u32, u32),
    url: Option<String>,
    egress: String,
    subject: String,
    errors: u32,
    streaming: bool,
    size: term::Size,
    /// Bytes of frame data received, for the receipt.
    bytes_in: u64,
    /// Input events actually forwarded to the page, for the receipt. Counted
    /// where they are sent, so a session that ends untidily still reports them.
    input_sent: u64,
    /// Terminal bytes not yet parsed into a complete event.
    pending: Vec<u8>,
    /// What the engine says its viewer lane offers, read off `status`.
    features: vim::Features,
    /// The key prefix already typed in VIEW (`g`, `y`), if any.
    prefix: String,
    /// The overlay, while it is up.
    hinting: Option<Hinting>,
    /// The field being typed into, while INSERT is on.
    inserting: Option<Inserting>,
    /// Whether the key list is showing.
    help: bool,
    /// One line of the viewer talking to the human, cleared on the next key.
    notice: Option<String>,
}

/// The overlay, from the moment it is asked for to the moment a label is
/// chosen or the human gives up.
#[cfg(unix)]
struct Hinting {
    /// What a chosen label is for. Decided before the overlay went up, which is
    /// what lets one set of labels serve `f`, `F` and `yf`.
    then: vim::HintThen,
    /// Set by `gi`, which wants a field rather than a choice: the overlay is
    /// asked for only because it is the thing that knows where the fields are,
    /// and the first one is taken without ever being drawn.
    auto_first: bool,
    items: Vec<proto::Hint>,
    typed: String,
    /// The viewport the rects were measured in. Kept with them, because a
    /// resize between the request and the draw would otherwise scale a chip by
    /// a viewport the rects never belonged to.
    viewport: (u32, u32),
}

#[cfg(unix)]
impl Hinting {
    /// Which items still match what has been typed.
    fn matching(&self) -> Vec<usize> {
        let labels: Vec<String> = self.items.iter().map(|h| h.label.clone()).collect();
        match vim::narrow(&labels, &self.typed) {
            vim::Match::One(i) => vec![i],
            vim::Match::Several(hits) => hits,
            vim::Match::None => Vec::new(),
        }
    }
}

/// The field INSERT is typing into.
#[cfg(unix)]
struct Inserting {
    reference: String,
    /// The whole value, resent on every keystroke. The engine's primitive is
    /// select-all-then-insert, so sending the buffer is idempotent and a
    /// dropped message cannot leave the field holding a scramble.
    text: String,
}

#[cfg(unix)]
impl<'a> App<'a> {
    fn new(opts: &Options, guard: &'a mut term::Guard, encoding: kitty::Encoding) -> App<'a> {
        let size = guard.size().or_fallback();
        App {
            env_dir: opts.state_dir.clone(),
            guard,
            placer: kitty::Placer::new(encoding),
            mode: Mode::View,
            last_frame: None,
            developer: false,
            // Enough to see a failure loop's shape without holding a page's
            // whole console in a viewer.
            log: panes::LogBuffer::new(200),
            mapping: None,
            viewport: (1280, 720),
            url: None,
            egress: opts.egress.clone(),
            subject: opts.subject.clone(),
            errors: 0,
            streaming: true,
            size,
            bytes_in: 0,
            input_sent: 0,
            pending: Vec::new(),
            features: vim::Features::default(),
            prefix: String::new(),
            hinting: None,
            inserting: None,
            help: false,
            notice: None,
        }
    }

    /// Run until the human leaves or the box stops streaming. Returns the
    /// reason the stream ended, if it ended badly.
    ///
    /// The tick runs on [`Ticker`]'s deadline rather than on the receive
    /// timeout, which is what keeps a page that never stops sending from
    /// freezing the status line and the resize check.
    fn pump(&mut self, rx: mpsc::Receiver<Ev>, writer: &mut impl Write) -> Option<String> {
        self.draw_status();
        let mut ticker = Ticker::start(Instant::now());
        loop {
            match rx.recv_timeout(ticker.wait(Instant::now())) {
                Ok(Ev::Net(msg)) => {
                    if let Some(err) = self.on_net(msg, writer) {
                        return Some(err);
                    }
                }
                Ok(Ev::NetDone(why)) => return why,
                Ok(Ev::Input(bytes)) => {
                    ticker.saw_input(Instant::now());
                    self.pending.extend_from_slice(&bytes);
                    if self.on_input(false, writer) {
                        return None;
                    }
                }
                Ok(Ev::InputDone) => return None,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return None,
            }

            if let Some(quiet) = ticker.due(Instant::now()) {
                if self.on_input(quiet, writer) {
                    return None;
                }
                self.on_tick();
            }
        }
    }

    fn on_net(&mut self, msg: ws::Incoming, writer: &mut impl Write) -> Option<String> {
        match msg {
            ws::Incoming::Text(text) => {
                self.bytes_in += text.len() as u64;
                match proto::parse(&text) {
                    Some(proto::ServerMessage::Frame { seq, data }) => {
                        self.on_frame(&data);
                        // Acknowledged after drawing, not on arrival: under ack
                        // pacing this is what makes the terminal's own draw
                        // rate set the pace, so frames can never pile up.
                        if let Some(seq) = seq {
                            let _ = ws::send_text(writer, &proto::ack(seq).to_string());
                        }
                    }
                    Some(proto::ServerMessage::Status {
                        screencasting,
                        viewport_width,
                        viewport_height,
                        features,
                        ..
                    }) => {
                        self.streaming = screencasting;
                        self.viewport = (viewport_width, viewport_height);
                        // What the keymap is allowed to bind, from the engine's
                        // own account rather than from its name. Re-read on
                        // every status: a session that gains a capability
                        // mid-run gains the keys with it.
                        self.features = vim::Features::from_iter(features);
                        self.draw_status();
                    }
                    Some(proto::ServerMessage::Hints {
                        viewport_width,
                        viewport_height,
                        items,
                    }) => {
                        self.on_hints(items, (viewport_width, viewport_height), writer);
                    }
                    Some(proto::ServerMessage::Acted { ok, error }) => {
                        // Only the refusals are shown. A click that worked
                        // announces itself by the page changing, and a viewer
                        // that said so as well would be narrating.
                        if !ok {
                            let why = error.unwrap_or_else(|| "refused".into());
                            self.say(Some(one_line(&why)));
                        }
                    }
                    Some(proto::ServerMessage::Url(url)) => {
                        self.url = Some(url);
                        self.draw_status();
                    }
                    Some(proto::ServerMessage::Tabs {
                        active_url: Some(url),
                    }) => {
                        self.url = Some(url);
                        self.draw_status();
                    }
                    Some(proto::ServerMessage::ConsoleError(text)) => {
                        self.errors = self.errors.saturating_add(1);
                        // Kept, not just counted: the count tells a supervisor
                        // something is wrong, the text tells them what.
                        self.log.push(panes::LogLine::console(text));
                        self.draw_status();
                        self.redraw_log();
                    }
                    Some(proto::ServerMessage::PageError(text)) => {
                        self.errors = self.errors.saturating_add(1);
                        self.log.push(panes::LogLine::page_error(text));
                        self.draw_status();
                        self.redraw_log();
                    }
                    _ => {}
                }
                None
            }
            // Binary frames are not part of this stream's contract, but a
            // future version could send raw JPEG. Treating them as a frame
            // costs nothing and fails safe if they are something else.
            ws::Incoming::Binary(bytes) => {
                self.bytes_in += bytes.len() as u64;
                self.render(&bytes);
                None
            }
            ws::Incoming::Ping(payload) => {
                let _ = ws::send_pong(writer, &payload);
                None
            }
            ws::Incoming::Pong(_) => None,
            ws::Incoming::Close => Some("the browser closed the stream".into()),
        }
    }

    fn on_frame(&mut self, base64_jpeg: &str) {
        use base64::Engine as _;
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(base64_jpeg) else {
            return;
        };
        self.render(&bytes);
        self.last_frame = Some(bytes);
    }

    /// Draw one frame. Writes only to the terminal: the socket is not involved,
    /// which is what keeps the frame's bytes and the input direction from ever
    /// interleaving on the same descriptor.
    fn render(&mut self, jpeg: &[u8]) {
        // A frame that will not decode is dropped, never fatal: the box can
        // produce one by crashing at the wrong moment, and a viewer that exits
        // on it is a viewer any flaky page can close.
        let Ok(frame) = image::decode(jpeg) else {
            return;
        };
        let regions = panes::layout(self.size.cols.max(1), self.size.rows, self.developer);
        let fit = image::fit(
            frame.width,
            frame.height,
            regions.page.cols.max(1),
            regions.page.rows.max(1),
            self.size.cell_w,
            self.size.cell_h,
        );
        let mut scaled = image::downscale(&frame, fit.pixel_width, fit.pixel_height);

        // Composited into the frame rather than written as terminal text over
        // it. See `overlay` for why: an image's relationship to cell
        // backgrounds is the one part of the graphics protocol terminals
        // genuinely disagree about, and a label that is sometimes invisible is
        // worse than none.
        if let Some(hinting) = self.hinting.as_ref()
            && self.mode == Mode::Hint
        {
            let chips = self.chips(hinting, scaled.width, scaled.height);
            if !chips.is_empty() {
                // `downscale` hands back a borrow when the frame is already the
                // right size, so drawing into it is the one path that has to
                // own its pixels. Paid only when there is an overlay to draw.
                let owned = scaled.to_mut();
                overlay::draw(&mut owned.data, owned.width, owned.height, &chips);
            }
        }

        let at = kitty::Placement {
            row: regions.page.row,
            col: regions.page.col,
            cols: fit.cols,
            rows: fit.rows,
        };
        // Where the page is on screen, so a click can be mapped back into it.
        // Derived from the placement that was actually drawn rather than
        // recomputed later, because the two drifting apart is a click that
        // lands somewhere plausible and wrong.
        self.mapping = Some(input::Mapping {
            col: at.col,
            row: at.row,
            cols: at.cols,
            rows: at.rows,
            viewport_width: self.viewport.0,
            viewport_height: self.viewport.1,
        });

        let bytes = self
            .placer
            .draw(&scaled.data, scaled.width, scaled.height, at);
        let mut out = std::io::stdout();
        let _ = out.write_all(&bytes);
        let _ = out.flush();
    }

    /// Where each still-matching label goes in the frame just decoded.
    ///
    /// Only the ones still matching. Leaving the rest up would make the overlay
    /// no easier to read after typing than before, which is the whole point of
    /// typing.
    fn chips(&self, hinting: &Hinting, width: u32, height: u32) -> Vec<overlay::Chip> {
        hinting
            .matching()
            .into_iter()
            .filter_map(|index| hinting.items.get(index))
            .map(|item| {
                overlay::place(
                    &item.label,
                    hinting.typed.chars().count(),
                    (item.x, item.y, item.width, item.height),
                    // The viewport the rects were measured in, not whatever the
                    // viewport is now: a resize between the request and the draw
                    // would otherwise scale a chip by a page it never described.
                    hinting.viewport,
                    (width, height),
                )
            })
            .collect()
    }

    /// The key list, drawn over the page.
    ///
    /// Terminal text rather than a composited overlay, because unlike a hint
    /// chip this does not have to line up with anything on the page: it is the
    /// viewer talking about itself, and it may sit wherever it fits.
    fn draw_help(&mut self) {
        if !self.help {
            return;
        }
        let rows: Vec<String> = vim::BINDINGS
            .iter()
            .map(|binding| {
                let available = binding.needs.is_none_or(|need| self.features.has(need));
                format!(
                    " {:<7} {}{} ",
                    binding.keys,
                    binding.what,
                    // An unavailable key is shown and marked rather than hidden.
                    // Hiding it leaves someone who has used this on another
                    // session wondering where the key went.
                    if available { "" } else { " (not on this engine)" },
                )
            })
            .collect();
        let width = rows.iter().map(|r| r.chars().count()).max().unwrap_or(0) as u16;
        let col = self.size.cols.saturating_sub(width + 1).max(1);

        let mut out = String::from("\x1b[s");
        for (index, row) in rows.iter().enumerate() {
            let row_at = panes::CHROME_ROWS + 1 + index as u16;
            if row_at > self.size.rows {
                break;
            }
            out.push_str(&kitty::cursor_to(row_at, col));
            // Reverse video, like the status line: it has to read as the
            // viewer's chrome rather than as something the page drew.
            out.push_str("\x1b[7m");
            out.push_str(&format!("{row:<width$}", width = width as usize));
            out.push_str("\x1b[0m");
        }
        out.push_str("\x1b[u");

        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(out.as_bytes());
        let _ = stdout.flush();
    }

    fn on_tick(&mut self) {
        let size = self.guard.size().or_fallback();
        if size != self.size {
            self.size = size;
            let mut out = std::io::stdout();
            let _ = out.write_all(b"\x1b[2J");
            let _ = out.flush();
            // Redraw from the frame we already have: a static page will not
            // send another one, and a resize that blanks the viewport until
            // something moves reads as a crash.
            if let Some(frame) = self.last_frame.take() {
                self.render(&frame);
                self.last_frame = Some(frame);
            }
        }
        // The lock can change under us (the agent's own tooling, or another
        // terminal) so it is read rather than remembered.
        self.draw_status();
    }

    /// Repaint the developer pane, if it is showing.
    ///
    /// Cursor position is saved and restored around it, the same way
    /// `draw_status` does: the page image is placed by absolute position, but
    /// anything else writing to the terminal would otherwise leave the cursor
    /// wherever it finished.
    fn redraw_log(&mut self) {
        if !self.developer {
            return;
        }
        let regions = panes::layout(self.size.cols.max(1), self.size.rows, true);
        let Some(rect) = regions.log else {
            return;
        };
        let rendered = panes::render_pane(&self.log, rect.cols, rect.rows);

        let mut out = String::from("\x1b[s");
        for (index, line) in rendered.iter().enumerate() {
            out.push_str(&kitty::cursor_to(rect.row + index as u16, rect.col));
            out.push_str(line);
        }
        out.push_str("\x1b[u");

        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(out.as_bytes());
        let _ = stdout.flush();
    }

    fn draw_status(&mut self) {
        let s = Status {
            subject: self.subject.clone(),
            mode: self.mode,
            holder: control::read(&self.env_dir).holder,
            url: self.url.clone(),
            egress: self.egress.clone(),
            errors: self.errors,
            streaming: self.streaming,
            notice: self.notice.clone(),
        };
        let mut out = std::io::stdout();
        // Saved and restored around the write so the image's placement cursor
        // is undisturbed; the frame is drawn relative to where it was left.
        let _ = write!(
            out,
            "\x1b[s{}{}\x1b[u",
            kitty::cursor_to(1, 1),
            status::render(&s, self.size.cols)
        );
        let _ = out.flush();
    }

    /// Handle whatever the human typed. Returns true when they are leaving.
    fn on_input(&mut self, quiet: bool, writer: &mut impl Write) -> bool {
        let events = input::parse(&mut self.pending, quiet);
        for ev in events {
            if self.on_event(ev, writer) {
                return true;
            }
        }
        false
    }

    fn on_event(&mut self, ev: input::Event, writer: &mut impl Write) -> bool {
        match self.mode {
            Mode::View => self.on_view_key(ev, writer),
            Mode::Hint => {
                self.on_hint_key(ev, writer);
                false
            }
            Mode::Insert => {
                self.on_insert_key(ev, writer);
                false
            }
            Mode::Interact => {
                self.on_interact(ev, writer);
                false
            }
        }
    }

    /// In VIEW the keyboard is the viewer's. Nothing reaches the page, which is
    /// what makes it safe to bind single letters, and what the whole keymap
    /// rests on.
    ///
    /// Returns true when the human is leaving.
    fn on_view_key(&mut self, ev: input::Event, writer: &mut impl Write) -> bool {
        use input::{Event, KeyCode};
        let Event::Key(key) = ev else {
            return false;
        };
        let ctrl = key.modifiers & proto::modifiers::CTRL != 0;

        // Ctrl-C is a keystroke here, not a signal; raw mode saw to that. So
        // leaving on it has to be arranged rather than assumed, and it is
        // checked before the keymap because a prefix must not swallow it.
        if ctrl && key.code == KeyCode::Char('c') {
            return true;
        }
        // Escape abandons a half-typed prefix and clears whatever the viewer
        // was last saying. Nothing else, so it is safe to press when unsure.
        if key.code == KeyCode::Escape {
            self.prefix.clear();
            self.say(None);
            return false;
        }

        // The arrows and page keys do what they say, whatever the keymap thinks
        // of the letters. Someone who has never read the key list should still
        // be able to move around the page.
        let arrow = match key.code {
            KeyCode::Down => Some(vim::Scroll::LineDown),
            KeyCode::Up => Some(vim::Scroll::LineUp),
            KeyCode::PageDown => Some(vim::Scroll::PageDown),
            KeyCode::PageUp => Some(vim::Scroll::PageUp),
            KeyCode::Home => Some(vim::Scroll::Top),
            KeyCode::End => Some(vim::Scroll::Bottom),
            _ => None,
        };
        if let Some(scroll) = arrow {
            self.scroll(scroll, writer);
            return false;
        }

        let KeyCode::Char(ch) = key.code else {
            return false;
        };
        // A control chord is not a keymap letter. Without this `Ctrl-D` would
        // scroll, which is nearly right and would make `Ctrl-D` for something
        // else impossible to add later.
        if ctrl {
            return false;
        }

        let prefix = std::mem::take(&mut self.prefix);
        let action = vim::resolve(ch, &prefix, &self.features);
        self.dispatch(action, ch, writer)
    }

    /// Carry out what the keymap decided. Returns true when the human is
    /// leaving.
    fn dispatch(&mut self, action: vim::Action, key: char, writer: &mut impl Write) -> bool {
        use vim::Action;
        match action {
            Action::Quit => return true,
            Action::Pending => {
                self.prefix.push(key);
                // Shown rather than kept quiet: a prefix that is waiting looks
                // exactly like a viewer that has stopped responding.
                self.say(Some(format!("{key}…")));
                return false;
            }
            Action::Interact => self.enter_interact(),
            Action::Developer => self.toggle_developer(),
            Action::Help => {
                self.help = !self.help;
                self.repaint();
            }
            Action::Scroll(scroll) => self.scroll(scroll, writer),
            Action::Reload => self.send(writer, &proto::reload()),
            Action::History(go) => self.send(writer, &proto::history(go)),
            Action::Hints(then) => self.ask_for_hints(then, false, writer),
            Action::InsertFirstField => {
                self.ask_for_hints(vim::HintThen::Insert, true, writer)
            }
            Action::YankUrl => {
                let url = self.url.clone().unwrap_or_default();
                self.yank(&url, "this page's URL");
            }
            // Said rather than swallowed. A key that does nothing and explains
            // nothing gets pressed harder.
            Action::Unsupported(why) => self.say(Some(why.to_string())),
            Action::Unbound => self.say(None),
        }
        false
    }

    /// Ask the engine for the overlay.
    ///
    /// The reply is what puts the viewer into HINT; nothing changes here, so a
    /// session whose engine ignores the message stays exactly where it was
    /// rather than sitting in a mode with no labels in it.
    fn ask_for_hints(&mut self, then: vim::HintThen, auto_first: bool, writer: &mut impl Write) {
        // Taken before the request goes out, not when a label is chosen. The
        // labels describe the page as it is at this moment, and an agent that
        // navigates between the ask and the answer would leave every one of
        // them pointing into a document that is gone.
        let _ = control::take(&self.env_dir);
        self.hinting = Some(Hinting {
            then,
            auto_first,
            items: Vec::new(),
            typed: String::new(),
            viewport: self.viewport,
        });
        self.send(writer, &proto::hints());
    }

    /// The overlay arrived. Draw it, or use it and put it away.
    fn on_hints(&mut self, items: Vec<proto::Hint>, viewport: (u32, u32), writer: &mut impl Write) {
        let Some(hinting) = self.hinting.as_mut() else {
            // An overlay nobody asked for, or one that arrived after the human
            // gave up. Dropped: acting on it would act on a page they are no
            // longer looking at the same way.
            return;
        };
        hinting.items = items;
        hinting.viewport = viewport;

        if hinting.items.is_empty() {
            self.hinting = None;
            let _ = control::release(&self.env_dir);
            self.say(Some("nothing on screen to act on".into()));
            return;
        }

        // `gi` wanted a field, not a choice. The overlay was only ever the
        // thing that knows where the fields are.
        if hinting.auto_first {
            let first = hinting
                .items
                .iter()
                .position(|item| FIELD_ROLES.contains(&item.role.as_str()));
            match first {
                Some(index) => {
                    let reference = hinting.items[index].reference.clone();
                    self.hinting = None;
                    self.enter_insert(reference, writer);
                }
                None => {
                    self.hinting = None;
                    let _ = control::release(&self.env_dir);
                    self.say(Some("no field on screen to type into".into()));
                }
            }
            return;
        }

        let prompt = hinting.then.prompt();
        self.mode = Mode::Hint;
        self.say(Some(format!("{prompt}: type a label, Esc to stop")));
        self.repaint();
    }

    /// In HINT the keys narrow the labels. Nothing reaches the page, but the
    /// lock is held all the same: see [`Mode::holds_control`] for why an overlay
    /// takes it.
    fn on_hint_key(&mut self, ev: input::Event, writer: &mut impl Write) {
        use input::{Event, KeyCode};
        let Event::Key(key) = ev else {
            return;
        };
        let ctrl = key.modifiers & proto::modifiers::CTRL != 0;
        // Both ways out, checked before the label matcher so that neither can
        // be swallowed as a keystroke. `c` is in the hint alphabet's reach, so
        // the modifier is what separates "get me out" from "narrow to c".
        if key.code == KeyCode::Escape || (ctrl && key.code == KeyCode::Char('c')) {
            self.leave_hint();
            return;
        }
        match key.code {
            KeyCode::Backspace => {
                if let Some(hinting) = self.hinting.as_mut() {
                    hinting.typed.pop();
                }
                self.repaint();
            }
            KeyCode::Char(ch) if !ctrl => {
                let Some(hinting) = self.hinting.as_mut() else {
                    self.leave_hint();
                    return;
                };
                // Tried before it is kept: a key that matches nothing must not
                // be added, or the overlay would be stuck behind a prefix no
                // label starts with and only Backspace would get it out.
                let mut attempt = hinting.typed.clone();
                attempt.push(ch);
                let labels: Vec<String> =
                    hinting.items.iter().map(|item| item.label.clone()).collect();
                match vim::narrow(&labels, &attempt) {
                    vim::Match::None => {
                        self.say(Some(format!("no label starts with `{attempt}`")));
                    }
                    vim::Match::Several(_) => {
                        hinting.typed = attempt;
                        self.repaint();
                    }
                    vim::Match::One(index) => {
                        let then = hinting.then;
                        let item = hinting.items[index].clone();
                        self.hinting = None;
                        self.act_on(then, item, writer);
                    }
                }
            }
            _ => {}
        }
    }

    /// Do what the overlay was put up for.
    fn act_on(&mut self, then: vim::HintThen, item: proto::Hint, writer: &mut impl Write) {
        match then {
            vim::HintThen::Click => {
                // The click goes out under the lock this overlay took, and the
                // lock is given back after it: a human who followed a link is
                // not still driving.
                self.mode = Mode::View;
                // Through the verb layer, which is the whole argument for hints
                // over a synthetic pointer: the receipt records which role and
                // which accessible name were activated, not a coordinate.
                self.send(writer, &proto::act(&item.reference, "click"));
                let _ = control::release(&self.env_dir);
                self.say(Some(format!("{} {}", item.role, one_line(&item.name))));
                self.repaint();
            }
            vim::HintThen::Insert => self.enter_insert(item.reference.clone(), writer),
            vim::HintThen::Yank => {
                self.mode = Mode::View;
                let _ = control::release(&self.env_dir);
                match &item.href {
                    Some(href) => self.yank(href, "link"),
                    None => self.say(Some(format!(
                        "a {} has no link to copy",
                        one_line(&item.role)
                    ))),
                }
                self.repaint();
            }
        }
    }

    /// Put the caret in a field and start typing into it.
    ///
    /// This *is* driving the page, so unlike HINT it takes the control lock,
    /// under the same rule `enter_interact` follows: reaching for the controls
    /// is taking them, because there is nowhere else to ask from.
    fn enter_insert(&mut self, reference: String, writer: &mut impl Write) {
        let _ = control::take(&self.env_dir);
        self.inserting = Some(Inserting {
            reference: reference.clone(),
            text: String::new(),
        });
        self.mode = Mode::Insert;
        self.say(Some("typing — Enter to submit, Esc to stop".into()));
        // Emptied on the way in, so what is typed replaces the field rather than
        // appending to whatever was there. The alternative is a viewer whose
        // first keystroke lands at an unknown offset in an unknown value.
        self.send(writer, &proto::insert(&reference, ""));
        self.repaint();
    }

    /// In INSERT every key is the field's text. One destination, known to the
    /// viewer, which is what makes this different from INTERACT.
    fn on_insert_key(&mut self, ev: input::Event, writer: &mut impl Write) {
        use input::{Event, KeyCode};
        let text = match ev {
            // A paste is text, whole, and its control bytes are text too. Same
            // rule the viewer already follows in INTERACT and for the same
            // reason bracketed paste is enabled at all.
            Event::Paste(pasted) => Some(pasted),
            // The terminal still has the mouse in INSERT: nothing here took it,
            // because there is one destination and it is already known. A click
            // is the terminal's own selection, not the page's.
            Event::Mouse(_) => return,
            Event::Key(key) => {
                let ctrl = key.modifiers & proto::modifiers::CTRL != 0;
                match key.code {
                    KeyCode::Escape => {
                        self.leave_insert();
                        return;
                    }
                    KeyCode::Char('c') if ctrl => {
                        self.leave_insert();
                        return;
                    }
                    KeyCode::Enter => {
                        if let Some(inserting) = self.inserting.as_ref() {
                            let reference = inserting.reference.clone();
                            self.send(writer, &proto::act_press(&reference, "Enter"));
                        }
                        self.leave_insert();
                        return;
                    }
                    KeyCode::Backspace => {
                        if let Some(inserting) = self.inserting.as_mut() {
                            inserting.text.pop();
                        }
                        None
                    }
                    KeyCode::Char(ch) if !ctrl => Some(ch.to_string()),
                    _ => return,
                }
            }
        };

        let Some(inserting) = self.inserting.as_mut() else {
            self.leave_insert();
            return;
        };
        if let Some(text) = text {
            inserting.text.push_str(&text);
        }
        let (reference, value) = (inserting.reference.clone(), inserting.text.clone());

        // Re-read rather than assumed, the same rule INTERACT follows: another
        // process can take the lock while a human is typing, and the keystrokes
        // must stop the moment it does.
        if control::read(&self.env_dir).holder != Holder::Human {
            self.leave_insert();
            return;
        }
        self.send(writer, &proto::insert(&reference, &value));
    }

    fn leave_hint(&mut self) {
        self.hinting = None;
        let _ = control::release(&self.env_dir);
        self.mode = Mode::View;
        self.say(None);
        self.repaint();
    }

    fn leave_insert(&mut self) {
        self.inserting = None;
        let _ = control::release(&self.env_dir);
        self.mode = Mode::View;
        self.say(None);
        self.repaint();
    }

    /// Turn a scroll intent into events the engine on the other end already
    /// understands.
    ///
    /// Wheel deltas and the Home/End keys, never a message of ours, which is
    /// what makes the keys a reader uses most work on a box running an engine
    /// that has never heard of any of this.
    fn scroll(&mut self, scroll: vim::Scroll, writer: &mut impl Write) {
        use vim::Scroll;
        // The engine's own line step. Named here rather than guessed, so a
        // `j` moves the page by what the engine calls a line.
        const LINE: f64 = 64.0;
        let page = self.viewport.1.max(1) as f64;
        let delta = match scroll {
            Scroll::LineDown => Some(LINE),
            Scroll::LineUp => Some(-LINE),
            Scroll::HalfDown => Some(page / 2.0),
            Scroll::HalfUp => Some(-page / 2.0),
            Scroll::PageDown => Some(page * 0.9),
            Scroll::PageUp => Some(-page * 0.9),
            // Home and End rather than an enormous wheel delta. An engine
            // clamps a scroll to the document, so a huge delta would work, but
            // "go to the top" is a thing the keyboard can say exactly and
            // saying it approximately is how a page with lazy loading ends up
            // somewhere near the top.
            Scroll::Top | Scroll::Bottom => None,
        };
        match delta {
            Some(delta) => self.send(writer, &proto::wheel(delta)),
            None => {
                let code = if scroll == Scroll::Top {
                    input::KeyCode::Home
                } else {
                    input::KeyCode::End
                };
                // Through the same encoder INTERACT uses, so an arrow sent from
                // the keymap and one typed by hand are the same event.
                let key = input::Key { code, modifiers: 0 };
                for event in input::key_events(&key) {
                    self.send(writer, &event.to_json());
                }
            }
        }
        self.say(None);
    }

    /// Put text on the human's clipboard with OSC 52.
    ///
    /// Said honestly, because it cannot be confirmed: OSC 52 is a write with no
    /// reply, and many terminals disable it. A viewer that reported "copied" and
    /// was silently ignored would be worse than one that says what it tried.
    fn yank(&mut self, text: &str, what: &str) {
        if text.is_empty() {
            self.say(Some(format!("no {what} to copy")));
            return;
        }
        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
        let mut out = std::io::stdout();
        // The payload is base64, so nothing page-derived reaches the terminal
        // as an escape sequence: this is the one place a URL is written to the
        // PTY unsanitized, and base64 is what makes it safe rather than a check.
        let _ = write!(out, "\x1b]52;c;{encoded}\x07");
        let _ = out.flush();
        self.say(Some(format!("sent {what} to the clipboard (OSC 52)")));
    }

    /// Say one line to the human, on the row the page can never reach.
    ///
    /// Draws as well as stores. Keeping the two together is what stops a
    /// refusal from being written into a field and then only appearing on the
    /// next tick, a quarter of a second later, by which time the human has
    /// pressed the key again.
    fn say(&mut self, notice: Option<String>) {
        if self.notice == notice {
            return;
        }
        self.notice = notice;
        self.draw_status();
    }

    fn toggle_developer(&mut self) {
        self.developer = !self.developer;
        self.repaint();
    }

    /// Redraw everything from the frame already in hand.
    ///
    /// A static page sends nothing, so anything that moves the page on screen,
    /// a split, an overlay, a mode change, has to repaint from what is already
    /// here or the viewport stays as it was until the page happens to change.
    fn repaint(&mut self) {
        // The split and the overlay both move the image, so the old placement
        // has to go before the new one is drawn or the two overlap.
        let _ = std::io::stdout().write_all(b"\x1b[2J");
        self.draw_status();
        if let Some(frame) = self.last_frame.take() {
            self.render(&frame);
            self.last_frame = Some(frame);
        }
        self.redraw_log();
        self.draw_help();
    }

    /// Reaching for the controls *is* taking them.
    ///
    /// The lock's own rule is that a human takes control rather than asking for
    /// it, and in a terminal viewer there is nowhere else to ask from: the
    /// terminal is busy being the viewer, so "run `h5i browser take` in another
    /// window" is advice with no window to follow it in.
    fn enter_interact(&mut self) {
        let _ = control::take(&self.env_dir);
        self.mode = Mode::Interact;
        self.guard.set_mouse(true);
        self.draw_status();
    }

    fn leave_interact(&mut self) {
        let _ = control::release(&self.env_dir);
        self.mode = Mode::View;
        // Give the mouse back to the terminal, so watching feels like a
        // terminal again: selection, scrollback, copy.
        self.guard.set_mouse(false);
        self.draw_status();
    }

    fn on_interact(&mut self, ev: input::Event, writer: &mut impl Write) {
        use input::{Event, KeyCode};
        // The one key the viewer keeps. Raw mode hands us everything else, so
        // without a reserved key there would be no way back out.
        if let Event::Key(k) = &ev
            && k.code == KeyCode::Char(']') && k.modifiers & proto::modifiers::CTRL != 0
        {
            self.leave_interact();
            return;
        }

        // The lock is re-read rather than assumed: another process can take it
        // while this viewer is in INTERACT, and input must stop the moment it
        // does. This is the same rule the web forward enforces on its own input
        // direction, applied to the one client that has no forward in front of
        // it.
        if control::read(&self.env_dir).holder != Holder::Human {
            return;
        }

        match ev {
            Event::Key(key) => {
                for e in input::key_events(&key) {
                    self.send(writer, &e.to_json());
                }
            }
            Event::Paste(text) => {
                for e in input::paste_events(&text) {
                    self.send(writer, &e.to_json());
                }
            }
            Event::Mouse(m) => {
                let Some(map) = self.mapping else { return };
                if let Some(e) = input::mouse_event(&m, &map) {
                    self.send(writer, &e.to_json());
                }
            }
        }
    }

    fn send(&mut self, writer: &mut impl Write, msg: &serde_json::Value) {
        if ws::send_text(writer, &msg.to_string()).is_ok() {
            // Counted here, at the point it actually went out, so a session
            // that ends untidily still reports what a human did. Recording it
            // any later loses exactly the case a reviewer cares about.
            self.input_sent += 1;
        }
    }
}

// Unix-gated with the code they cover: on a target with no terminal half there
// is nothing here to test.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn the_viewer_keeps_two_rows_for_itself() {
        // The page starts below the status line and its separator. If this ever
        // became 0 the page would be drawn over the one row it must never be
        // able to touch.
        assert_eq!(panes::CHROME_ROWS, 2, "row one is the status line, row two separates it");
    }

    #[test]
    fn a_page_that_never_stops_sending_cannot_starve_the_tick() {
        // The regression this is here for. Frames every 100 ms, which is what
        // the default `--fps 10` produces. With the tick hung off the receive
        // timeout, a 250 ms timeout next to a 100 ms frame interval never
        // elapsed and the tick simply never ran, no status refresh, no resize
        // check, for as long as the page kept moving.
        let t0 = Instant::now();
        let mut ticker = Ticker::start(t0);

        let mut ticks = 0;
        for n in 1..=10u32 {
            let now = t0 + Duration::from_millis(100 * u64::from(n));
            // The loop must never block past the next deadline either.
            assert!(ticker.wait(now) <= TICK, "wait overshoots the deadline");
            if ticker.due(now).is_some() {
                ticks += 1;
            }
        }
        // Ticks land on event boundaries and are rescheduled from where they
        // actually ran, so one second of 100 ms frames owes three or four of
        // them. The number is not the point. Zero was.
        assert!((3..=4).contains(&ticks), "one second of frames owed {ticks} ticks");
    }

    #[test]
    fn quiet_input_is_a_claim_about_the_keyboard_and_not_about_the_frame_rate() {
        let t0 = Instant::now();
        let mut ticker = Ticker::start(t0);

        // A keystroke at 200 ms. The tick at 250 ms is only 50 ms later, so a
        // lone ESC sitting in the buffer might still be the start of a sequence
        // whose tail has not arrived: it must not be flushed as Escape yet.
        ticker.saw_input(t0 + Duration::from_millis(200));
        assert_eq!(ticker.due(t0 + Duration::from_millis(250)), Some(false));

        // By the next tick the keyboard has been still for a whole one, so the
        // ESC is the Escape key.
        assert_eq!(ticker.due(t0 + Duration::from_millis(500)), Some(true));

        // And nothing is owed before the deadline.
        assert_eq!(ticker.due(t0 + Duration::from_millis(600)), None);
    }

    #[test]
    fn the_start_a_stream_hint_names_the_engine_the_box_actually_runs() {
        // An h5i-light box has no agent-browser daemon, so the old advice sent
        // its owner to a CLI that fails on a missing socket directory before it
        // can answer the question they asked.
        let light = not_streaming_hint(Some("h5i-light"));
        assert!(light.contains("h5i browser open"), "{light}");
        assert!(!light.contains("agent-browser"), "{light}");

        for other in [Some("chromium"), Some("lightpanda"), None] {
            let msg = not_streaming_hint(other);
            assert!(msg.contains("agent-browser stream enable"), "{msg}");
        }
    }

    #[test]
    fn an_unsupported_terminal_is_told_what_to_do_instead() {
        // The failure a first-time user is most likely to hit, so it names the
        // terminals that work, the command that always works, and the override.
        let msg = format!("{}", unsupported());
        assert!(msg.contains("h5i box view"), "{msg}");
        assert!(msg.contains("--assume-graphics"), "{msg}");
        assert!(msg.contains("Ghostty"), "{msg}");
    }
}
