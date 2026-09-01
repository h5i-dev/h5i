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
/// Two different security stories rather than two transports, and the status
/// line has to be able to tell them apart: a boxed session is enforced outside
/// the engine, a host session is not.
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
/// there. The resolution lives in [`crate::view::Route`]; what is here is the
/// refusals, which differ between the two ways of attaching.
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
        // Asked of the mode, so a mode added later cannot leave the lock held by
        // forgetting to be listed.
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

/// How long to wait for a reply before assuming it is not coming. An engine that
/// does not know the message answers nothing, and the web viewer's forward drops
/// input when the control lock is not the human's — without this, one lost reply
/// wedges typing for the session.
#[cfg(unix)]
const INSERT_TIMEOUT: Duration = Duration::from_secs(2);

/// Whether a redraw puts the image somewhere new.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Moved {
    /// Same placement, different content. No clear.
    Nothing,
    /// The page is about to occupy a different box, so the old one has to go.
    Layout,
}

/// The roles `gi` accepts as "a field to type into". A list rather than "the
/// first hint", because the first actionable thing on a page is rarely a field.
#[cfg(unix)]
const FIELD_ROLES: &[&str] = &["textbox", "searchbox", "combobox"];

/// One terminal keystroke, in the vocabulary a browser uses.
///
/// `text` is what gets inserted, reported rather than re-derived: the terminal
/// already read the byte. `None` for keys the viewer keeps for itself.
#[cfg(unix)]
fn dom_key(key: &input::Key) -> Option<proto::Typed> {
    use input::KeyCode;
    let ctrl = key.modifiers & proto::modifiers::CTRL != 0;
    let (name, text) = match key.code {
        KeyCode::Backspace => ("Backspace", None),
        KeyCode::Delete => ("Delete", None),
        KeyCode::Tab => ("Tab", None),
        KeyCode::Up => ("ArrowUp", None),
        KeyCode::Down => ("ArrowDown", None),
        KeyCode::Left => ("ArrowLeft", None),
        KeyCode::Right => ("ArrowRight", None),
        KeyCode::Home => ("Home", None),
        KeyCode::End => ("End", None),
        KeyCode::PageUp => ("PageUp", None),
        KeyCode::PageDown => ("PageDown", None),
        KeyCode::Insert | KeyCode::Function(_) | KeyCode::Escape | KeyCode::Enter => return None,
        KeyCode::Char(ch) => {
            // A chord types nothing. Enforced in the engine's table too; said
            // here so a viewer cannot send `Ctrl-S` as an `s`.
            let text = (!ctrl).then(|| ch.to_string());
            return Some(proto::Typed {
                key: ch.to_string(),
                text,
                modifiers: key.modifiers,
            });
        }
    };
    Some(proto::Typed {
        key: name.to_string(),
        text,
        modifiers: key.modifiers,
    })
}

/// Collapse a page-derived string to one line for the status row. [`status`]
/// strips escape sequences; this stops a newline pushing the row off its line.
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
    /// The scaled image the terminal is showing, so only the part that moved has
    /// to be sent. A resize changes its shape, which `damage` reports as "all of
    /// it".
    shown: Option<image::Rgb>,
    /// The same frame, already decoded. Most redraws are not caused by new
    /// pixels — a hint keystroke, a mode change — and re-decoding for those
    /// costs about 1.7ms to reproduce an identical buffer. Dropped whenever a
    /// new frame arrives, so it cannot go stale.
    decoded: Option<image::Rgb>,
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
///
/// A keystroke cannot be answered locally: nothing appears until the page is
/// laid out again and a frame encoded, around 40ms. Sent one at a time those
/// serialize, so at most one message is on the wire and every key struck while
/// it is away goes out together.
///
/// Batched, not coalesced: a keystroke is a delta, so dropping the ones in
/// between would lose characters.
#[cfg(unix)]
struct Inserting {
    reference: String,
    /// Keys struck since the last batch went out, in order.
    pending: Vec<proto::Typed>,
    /// What is on the wire and what is owed. See [`vim::Coalesce`], which is
    /// where the ordering lives and where it is tested.
    wire: vim::Coalesce,
    /// When the in-flight batch went out.
    ///
    /// A guard against a reply that never arrives rather than a guess about how
    /// slow an engine can be: an engine that does not know the message answers
    /// with nothing at all, and without this the first keystroke would wedge
    /// typing for the life of the session.
    sent_at: Instant,
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
            shown: None,
            decoded: None,
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
                self.on_tick(writer);
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
                        // What the keymap may bind, from the engine's own
                        // account. Re-read on every status.
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
                    Some(proto::ServerMessage::Acted { action, ok, error }) => {
                        // Released before the refusal is shown, so a rejected
                        // keystroke does not wedge the ones after it.
                        // `focus` releases the first batch, `input_keys` the rest.
                        if matches!(action.as_deref(), Some("input_keys" | "focus")) {
                            self.insert_landed(writer);
                        }
                        // Only refusals: a click that worked shows on the page.
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
        // New pixels, so whatever is cached describes the frame before this one.
        self.decoded = None;
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
        //
        // Decoded once per frame, not once per redraw: `on_frame` clears the
        // cache, so a hit means the same pixels.
        if self.decoded.is_none() {
            let Ok(frame) = image::decode(jpeg) else {
                return;
            };
            self.decoded = Some(frame);
        }
        let Some(frame) = self.decoded.take() else {
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

        // Only what changed, when only a little did. Sending the whole frame for
        // a keystroke cost about 40KB of deflated JPEG each time, a megabyte to
        // type one word, nearly all of it already on screen.
        let patch = self.shown.as_ref().and_then(|shown| {
            if self.placer.should_refresh() {
                // Patches have piled up; a whole frame releases them.
                return None;
            }
            let hurt = image::damage(shown, &scaled)?;
            // Cell-aligned, grown outwards so no changed pixel is left showing
            // the older frame.
            let cell_w = scaled.width / u32::from(fit.cols.max(1));
            let cell_h = scaled.height / u32::from(fit.rows.max(1));
            let hurt = hurt.to_cells(cell_w, cell_h, scaled.width, scaled.height);
            hurt.worth_patching(scaled.width, scaled.height)
                .then_some((hurt, cell_w, cell_h))
        });

        let bytes = match patch {
            // Identical to what is on screen: the cheapest frame is unsent.
            None if self.shown.as_ref().is_some_and(|shown| *shown == *scaled) => Vec::new(),
            None => self
                .placer
                .draw(&scaled.data, scaled.width, scaled.height, at),
            Some((hurt, cell_w, cell_h)) => {
                let piece = hurt.crop(&scaled);
                let where_ = kitty::Placement {
                    row: at.row + (hurt.y / cell_h.max(1)) as u16,
                    col: at.col + (hurt.x / cell_w.max(1)) as u16,
                    cols: (hurt.width / cell_w.max(1)).max(1) as u16,
                    rows: (hurt.height / cell_h.max(1)).max(1) as u16,
                };
                self.placer
                    .draw_patch(&piece.data, piece.width, piece.height, where_)
            }
        };

        if !bytes.is_empty() {
            let mut out = std::io::stdout();
            let _ = out.write_all(&bytes);
            let _ = out.flush();
        }
        // What the next frame is compared against. Kept whatever was sent: after
        // a patch the screen holds the new frame just as after a whole one.
        self.shown = Some(scaled.into_owned());
        self.decoded = Some(frame);
    }

    /// Where each still-matching label goes in the frame just decoded.
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

    /// The key list, drawn over the page as terminal text — unlike a hint chip
    /// it does not have to line up with anything, so it may sit where it fits.
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

    fn on_tick(&mut self, writer: &mut impl Write) {
        self.unwedge_insert(writer);
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

        // Ctrl-C is a keystroke here, not a signal — raw mode saw to that — so
        // leaving on it is arranged, before the keymap, so no prefix eats it.
        if ctrl && key.code == KeyCode::Char('c') {
            return true;
        }
        // Escape abandons a half-typed prefix and clears the notice. Nothing
        // else, so it is safe to press when unsure.
        if key.code == KeyCode::Escape {
            self.prefix.clear();
            self.say(None);
            return false;
        }

        // The arrows and page keys work whatever the keymap says, so someone who
        // has not read the key list can still move around.
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
        // A control chord is not a keymap letter: `Ctrl-D` must not scroll.
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
                // Shown: a waiting prefix looks like a viewer that has stopped.
                self.say(Some(format!("{key}…")));
                return false;
            }
            Action::Interact => self.enter_interact(),
            Action::Developer => self.toggle_developer(),
            Action::Help => {
                self.help = !self.help;
                // Layout, so the cells the list was written into are cleared
                // rather than left showing through when it is dismissed.
                self.repaint_with(Moved::Layout);
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
            // Said rather than swallowed: a silent key gets pressed harder.
            Action::Unsupported(why) => self.say(Some(why.to_string())),
            Action::Unbound => self.say(None),
        }
        false
    }

    /// Ask the engine for the overlay. The *reply* is what enters HINT, so an
    /// engine that ignores the message leaves the viewer where it was.
    fn ask_for_hints(&mut self, then: vim::HintThen, auto_first: bool, writer: &mut impl Write) {
        // Taken before the request goes out: the labels describe the page as it
        // is now, and an agent navigating in between would make them all stale.
        let _ = control::take(&self.env_dir);
        self.hinting = Some(Hinting {
            then,
            auto_first,
            items: Vec::new(),
            typed: String::new(),
            viewport: self.viewport,
        });
        // Narrowed when the human is about to type.
        self.send(writer, &proto::hints(then == vim::HintThen::Insert));
    }

    /// The overlay arrived. Draw it, or use it and put it away.
    fn on_hints(&mut self, items: Vec<proto::Hint>, viewport: (u32, u32), writer: &mut impl Write) {
        let Some(hinting) = self.hinting.as_mut() else {
            // An overlay nobody asked for, or one that arrived after the human
            // gave up.
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

        // `gi` wanted a field, not a choice.
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

    /// In HINT the keys narrow the labels and nothing reaches the page. The lock
    /// is held anyway: see [`Mode::holds_control`].
    fn on_hint_key(&mut self, ev: input::Event, writer: &mut impl Write) {
        use input::{Event, KeyCode};
        let Event::Key(key) = ev else {
            return;
        };
        let ctrl = key.modifiers & proto::modifiers::CTRL != 0;
        // Both ways out, before the label matcher: `c` is in the hint alphabet,
        // so the modifier separates "get me out" from "narrow to c".
        if key.code == KeyCode::Escape || (ctrl && key.code == KeyCode::Char('c')) {
            self.leave_hint();
            return;
        }
        match key.code {
            KeyCode::Backspace => {
                let changed = match self.hinting.as_mut() {
                    Some(hinting) => hinting.typed.pop().is_some(),
                    None => false,
                };
                // Backspace at an empty prefix changes nothing on screen.
                if changed {
                    self.repaint();
                }
            }
            KeyCode::Char(ch) if !ctrl => {
                let Some(hinting) = self.hinting.as_mut() else {
                    self.leave_hint();
                    return;
                };
                // Tried before it is kept, or the overlay sticks behind a prefix
                // no label starts with.
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
                // The lock this overlay took is given back after the click.
                self.mode = Mode::View;
                // Through the verb layer, so the receipt names a role and an
                // accessible name rather than a coordinate.
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
    fn enter_insert(&mut self, reference: String, writer: &mut impl Write) {
        let _ = control::take(&self.env_dir);
        self.inserting = Some(Inserting {
            reference: reference.clone(),
            pending: Vec::new(),
            // The focus below is already on the wire and releases the first batch.
            wire: {
                let mut wire = vim::Coalesce::default();
                wire.typed();
                wire
            },
            sent_at: Instant::now(),
        });
        self.mode = Mode::Insert;
        self.say(Some("typing — Enter to submit, Esc to stop".into()));
        // The caret goes to the end and what is there stays: emptying it would
        // assume anyone focusing a field meant to replace it.
        self.send(writer, &proto::focus(&reference));
        self.repaint();
    }

    /// In INSERT every key goes to the field, as a real key.
    ///
    /// The keystroke itself rather than the value it would produce, so the caret
    /// moves, `Backspace` deletes where it is, and a page listening for typing
    /// hears it. None of that is expressible by sending a whole value.
    fn on_insert_key(&mut self, ev: input::Event, writer: &mut impl Write) {
        use input::{Event, KeyCode};
        let typed = match ev {
            // A paste is text, whole, control bytes included. It rides as one
            // key carrying all of it, inserted at the caret in one edit.
            Event::Paste(pasted) => proto::Typed {
                key: "Paste".into(),
                text: Some(pasted),
                modifiers: 0,
            },
            // The terminal keeps the mouse in INSERT: a click is its own
            // selection, not the page's.
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
                    // Submitting is the viewer's decision: a human who pressed
                    // `Enter` is done with the field.
                    KeyCode::Enter => {
                        if let Some(inserting) = self.inserting.as_ref() {
                            let reference = inserting.reference.clone();
                            self.send(writer, &proto::act_press(&reference, "Enter"));
                        }
                        self.leave_insert();
                        return;
                    }
                    _ => match dom_key(&key) {
                        Some(typed) => typed,
                        None => return,
                    },
                }
            }
        };

        // Re-read, not assumed: another process can take the lock mid-word, and
        // the keystrokes must stop the moment it does.
        if control::read(&self.env_dir).holder != Holder::Human {
            self.leave_insert();
            return;
        }
        let Some(inserting) = self.inserting.as_mut() else {
            self.leave_insert();
            return;
        };
        inserting.pending.push(typed);
        self.flush_insert(writer);
    }

    /// Put the pending keys on the wire, if the last batch has landed.
    fn flush_insert(&mut self, writer: &mut impl Write) {
        let Some(inserting) = self.inserting.as_mut() else {
            return;
        };
        if inserting.pending.is_empty() || !inserting.wire.typed() {
            return;
        }
        inserting.sent_at = Instant::now();
        // Taken, not copied: a batch that left a copy would type twice.
        let batch = std::mem::take(&mut inserting.pending);
        self.send(writer, &proto::keys(&batch));
    }

    /// Release a batch that is never going to be answered.
    fn unwedge_insert(&mut self, writer: &mut impl Write) {
        let stuck = self
            .inserting
            .as_ref()
            .is_some_and(|i| i.wire.waiting() && i.sent_at.elapsed() > INSERT_TIMEOUT);
        if !stuck {
            return;
        }
        let Some(inserting) = self.inserting.as_mut() else {
            return;
        };
        // `timed_out` first, unconditionally: skipping it on an empty queue
        // leaves the wire marked busy for a batch that is never coming.
        let released = inserting.wire.timed_out();
        if !released || inserting.pending.is_empty() {
            return;
        }
        // Sent from here, not left for the next keystroke: someone who typed a
        // word and stopped would be looking at a field that never caught up.
        // Only what has *not* been sent goes out — repeating a batch that may
        // already have arrived would type the word twice.
        inserting.sent_at = Instant::now();
        let batch = std::mem::take(&mut inserting.pending);
        self.send(writer, &proto::keys(&batch));
    }

    /// A batch came back. Send what was typed while it was away.
    fn insert_landed(&mut self, writer: &mut impl Write) {
        let Some(inserting) = self.inserting.as_mut() else {
            return;
        };
        if !inserting.wire.landed() {
            return;
        }
        // `landed` already marked the replacement in flight, so this writes it
        // rather than going back through `flush_insert`, which would hold it.
        inserting.sent_at = Instant::now();
        let batch = std::mem::take(&mut inserting.pending);
        self.send(writer, &proto::keys(&batch));
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

    /// Turn a scroll intent into events any engine understands: wheel deltas and
    /// the Home/End keys, never a message of ours.
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

    /// Put text on the clipboard with OSC 52. It cannot be confirmed — the write
    /// has no reply and many terminals disable it — so the notice says what was
    /// tried rather than claiming success.
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

    /// Say one line to the human, on the row the page can never reach. Draws as
    /// well as stores, so a refusal appears now rather than on the next tick.
    fn say(&mut self, notice: Option<String>) {
        if self.notice == notice {
            return;
        }
        self.notice = notice;
        self.draw_status();
    }

    fn toggle_developer(&mut self) {
        self.developer = !self.developer;
        // The split moves the page, so the old placement has to go before the
        // new one is drawn or the two overlap.
        self.repaint_with(Moved::Layout);
    }

    /// Redraw everything from the frame already in hand. A static page sends
    /// nothing, so a mode or layout change has to repaint from what is here.
    fn repaint(&mut self) {
        self.repaint_with(Moved::Nothing);
    }

    /// Repaint, saying whether the image is about to land somewhere else.
    ///
    /// A pane toggle resizes the page and the old placement has to go. Narrowing
    /// a hint label moves nothing, and clearing the screen for it makes every
    /// keystroke flash.
    fn repaint_with(&mut self, moved: Moved) {
        if moved == Moved::Layout {
            let _ = std::io::stdout().write_all(b"\x1b[2J");
            // The screen is blank now, so nothing on it can be patched against.
            // Without this the next frame would send only its difference from a
            // picture that has been wiped.
            self.shown = None;
        }
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
