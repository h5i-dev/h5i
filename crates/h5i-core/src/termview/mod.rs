//! The terminal viewer: watch a box's browser, and take over, without leaving
//! the terminal.
//!
//! The web viewer ([`crate::view`]) binds a loopback port, gates it with a
//! per-box token, and serves a page the human opens in *their own browser*,
//! which makes the watching program the most credential-laden one on the host.
//! This viewer is instead a client of the same stream, in the same process as
//! the CLI the human already ran:
//!
//! ```text
//!   h5i box view --term
//!     └── connect_in_netns(box pid, stream port)   no listener, no token
//!           └── WebSocket client ── JPEG frames ──► decode ──► Kitty graphics
//!                                 ◄── input ────── control lock ◄── terminal
//! ```
//!
//! Three consequences make this worth building rather than a nicer front end
//! for the existing forward:
//!
//! * **Nothing is bound.** A loopback listener is reachable by every process on
//!   the host, which is why the forward needs a token. Here the socket comes
//!   back over `SCM_RIGHTS` from a fork that entered the box's namespaces, so
//!   it is a descriptor this process holds. Nothing to connect to, nothing to
//!   authenticate.
//! * **The trusted path runs the other way too.** The box supplies compressed
//!   pixels inside a WebSocket message and nothing else. Every escape sequence
//!   the terminal receives is generated here (see [`kitty`]), so a box cannot
//!   reach the host's PTY even in principle: no OSC 52 clipboard write, no
//!   window-title rewrite, no graphics-protocol file read.
//! * **The status row cannot be painted over.** The page is an image below row
//!   two; row one is [`status`]. A page cannot lie about which origin it is, a
//!   claim browser chrome has never quite been able to make.
//!
//! It is not a boundary of its own. It watches a box at whatever tier that box
//! runs, and shrinking the watcher does not make a shared kernel unshared. The
//! claim is a smaller trusted computing base for *watching*, plus a status line
//! and a mode model only a terminal makes possible.

// Portable: these parse, encode and decode, and none of them touch a terminal.
pub mod image;
pub mod input;
pub mod kitty;
pub mod proto;
pub mod panes;
pub mod status;
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

/// What a viewer needs to know to attach to one box.
pub struct Options {
    pub env_dir: PathBuf,
    pub env_id: String,
    pub policy_digest: String,
    /// A short account of what the box may reach, for the status line.
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
    /// someone how to start a stream — the viewer itself is engine-agnostic,
    /// and adding this must not become the start of engine-specific rendering.
    pub engine: Option<String>,
}

/// What to tell someone whose box has no `.stream` file yet.
///
/// The advice is engine-specific because the command is: an `h5i-light` box has
/// no agent-browser daemon to enable streaming on, and telling its owner to run
/// `agent-browser stream enable` sends them to a CLI that will fail on a
/// missing socket directory before it ever reaches the question they asked. The
/// viewer stays engine-agnostic everywhere else; this is the one place the
/// difference is the user's problem rather than ours.
///
/// Unix-gated with the `run` that calls it, following this file's rule: the
/// non-unix `run` is a stub that refuses before it could ever need advice about
/// streaming, so an ungated helper here is dead code on Windows and `-D
/// warnings` is right to say so.
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
/// It is a type rather than two local variables so that the tick cannot be
/// attached to the socket again by accident, which is what it was: the tick was
/// whatever `recv_timeout` *expiring* meant, so a box sending frames faster than
/// [`TICK`] suppressed it entirely. At the default 10 fps a frame lands every
/// 100 ms and the 250 ms timeout never elapses, so the status line stopped
/// refreshing, a resize went unnoticed and a lone Escape was never flushed —
/// all of it on exactly the pages someone is watching because they are moving.
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
    // rather than on an alternate one that is about to be torn down.
    let (pid, pid_ns) = crate::view::box_pid_ns(&opts.env_dir).ok_or_else(|| {
        H5iError::Metadata(
            "this box is not running, so there is no browser to watch. \
             Start a session (`h5i box shell <name>`) and try again."
                .into(),
        )
    })?;
    let port = crate::view::stream_port(&opts.env_dir)
        .ok_or_else(|| H5iError::Metadata(not_streaming_hint(opts.engine.as_deref())))?;

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

    // The socket is a descriptor this process owns, handed back from a fork
    // that entered the box's namespaces. Nothing is listening anywhere.
    let mut socket = crate::view::connect_in_netns(pid, port, &pid_ns)?;
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
    let holder_at_open = control::read(&opts.env_dir).holder;
    // Scoped so the app releases its borrow of the terminal guard before the
    // guard itself is dropped and the terminal is restored.
    let (outcome, bytes_in, input_sent) = {
        let mut app = App::new(&opts, &mut guard, encoding);
        let outcome = app.pump(rx, &mut writer);

        // Leave the page as we found it. A viewer that exits still holding the
        // lock leaves the agent refusing to act, with nothing on screen to
        // explain why.
        if app.mode == Mode::Interact {
            let _ = control::release(&opts.env_dir);
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
            env_dir: opts.env_dir.clone(),
            env_id: opts.env_id.clone(),
            policy_digest: opts.policy_digest.clone(),
            transport: crate::view::Transport::Terminal,
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
/// first would decide compression before its answer had arrived — which is not
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
    box_id: String,
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
}

#[cfg(unix)]
impl<'a> App<'a> {
    fn new(opts: &Options, guard: &'a mut term::Guard, encoding: kitty::Encoding) -> App<'a> {
        let size = guard.size().or_fallback();
        App {
            env_dir: opts.env_dir.clone(),
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
            box_id: opts.env_id.clone(),
            errors: 0,
            streaming: true,
            size,
            bytes_in: 0,
            input_sent: 0,
            pending: Vec::new(),
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
                        ..
                    }) => {
                        self.streaming = screencasting;
                        self.viewport = (viewport_width, viewport_height);
                        self.draw_status();
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
        let scaled = image::downscale(&frame, fit.pixel_width, fit.pixel_height);

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
        // The lock can change under us — the agent's own tooling, or another
        // terminal — so it is read rather than remembered.
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
            box_id: self.box_id.clone(),
            mode: self.mode,
            holder: control::read(&self.env_dir).holder,
            url: self.url.clone(),
            egress: self.egress.clone(),
            errors: self.errors,
            streaming: self.streaming,
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
            Mode::View => self.on_view_key(ev),
            Mode::Interact => {
                self.on_interact(ev, writer);
                false
            }
        }
    }

    /// In VIEW the keyboard is the viewer's. Nothing reaches the page, which is
    /// what makes it safe to bind single letters.
    fn on_view_key(&mut self, ev: input::Event) -> bool {
        use input::{Event, KeyCode};
        let Event::Key(key) = ev else {
            return false;
        };
        let ctrl = key.modifiers & proto::modifiers::CTRL != 0;
        match key.code {
            KeyCode::Char('q') if !ctrl => true,
            // Ctrl-C is a keystroke here, not a signal — raw mode saw to that —
            // so leaving on it has to be arranged rather than assumed.
            KeyCode::Char('c') if ctrl => true,
            KeyCode::Char('i') if !ctrl => {
                self.enter_interact();
                false
            }
            // Developer mode. Safe as a bare letter here: nothing typed in
            // VIEW reaches the page.
            KeyCode::Char('d') if !ctrl => {
                self.developer = !self.developer;
                // The split moves the page, so the old placement has to go
                // before the new one is drawn or the two overlap.
                let _ = std::io::stdout().write_all(b"\x1b[2J");
                self.draw_status();
                if let Some(frame) = self.last_frame.clone() {
                    self.render(&frame);
                }
                self.redraw_log();
                false
            }
            _ => false,
        }
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
        // elapsed and the tick simply never ran — no status refresh, no resize
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
