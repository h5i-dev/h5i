//! The terminal viewer: watch a box's browser, and take over, without leaving
//! the terminal.
//!
//! # What this is, structurally
//!
//! The web viewer ([`crate::view`]) binds a loopback port, gates it with a
//! per-box token, and serves a page the human opens in *their own browser*.
//! That works, and it has one awkward property: the thing doing the watching is
//! the most credential-laden program on the host.
//!
//! This viewer removes that. It is a client of the same stream, in the same
//! process as the CLI the human already ran:
//!
//! ```text
//!   h5i box view --term
//!     └── connect_in_netns(box pid, stream port)   no listener, no token
//!           └── WebSocket client ── JPEG frames ──► decode ──► Kitty graphics
//!                                 ◄── input ────── control lock ◄── terminal
//! ```
//!
//! Three consequences follow, and they are the reason this is worth building
//! rather than a nicer front end for the existing forward:
//!
//! * **Nothing is bound.** The forward has to listen somewhere, and a loopback
//!   listener is reachable by every process on the host, which is why it needs
//!   a token in the first place. Here the socket comes back over `SCM_RIGHTS`
//!   from a fork that entered the box's namespaces, and it is a file descriptor
//!   this process holds. There is nothing for another process to connect to, so
//!   there is nothing to authenticate.
//! * **The trusted path runs the other way too.** The box supplies compressed
//!   pixels inside a WebSocket message and nothing else. Every escape sequence
//!   the terminal receives is generated here (see [`kitty`]), so a box cannot
//!   reach the host's PTY even in principle — no OSC 52 clipboard write, no
//!   window-title rewrite, no graphics-protocol file read.
//! * **The status row cannot be painted over.** The page is an image below row
//!   two; row one is [`status`]. A page cannot lie about which origin it is,
//!   which is a claim browser chrome has never quite been able to make.
//!
//! # What it is not
//!
//! It is not a boundary of its own. It watches a box at whatever tier that box
//! is running, and shrinking the software that does the watching does not make
//! a shared kernel unshared. The honest claim is a smaller trusted computing
//! base for *watching*, plus a status line and a mode model that only a
//! terminal makes possible.

// Portable: these parse, encode and decode, and none of them touch a terminal.
pub mod image;
pub mod input;
pub mod kitty;
pub mod proto;
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

/// Rows the viewer keeps for itself: the status line and a blank separator.
#[cfg(unix)]
const CHROME_ROWS: u16 = 2;

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
    let pid = crate::view::box_pid(&opts.env_dir).ok_or_else(|| {
        H5iError::Metadata(
            "this box is not running, so there is no browser to watch. \
             Start a session (`h5i box shell <name>`) and try again."
                .into(),
        )
    })?;
    let port = crate::view::stream_port(&opts.env_dir).ok_or_else(|| {
        H5iError::Metadata(
            "the box's browser is not streaming. Inside the box, run \
             `agent-browser stream enable`, then try again."
                .into(),
        )
    })?;

    let mut guard = term::Guard::enter(stdin).map_err(H5iError::Io)?;
    if !opts.assume_graphics {
        probe_graphics(stdin)?;
    }

    // The socket is a descriptor this process owns, handed back from a fork
    // that entered the box's namespaces. Nothing is listening anywhere.
    let mut socket = crate::view::connect_in_netns(pid, port)?;
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
        let mut app = App::new(&opts, &mut guard);
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

/// Ask the terminal whether it can draw images, and refuse politely if not.
#[cfg(unix)]
fn probe_graphics(fd: std::os::fd::RawFd) -> Result<(), H5iError> {
    let mut out = std::io::stdout();
    let _ = out.write_all(kitty::probe_sequence().as_bytes());
    let _ = out.flush();

    let mut seen = Vec::new();
    let deadline = Instant::now() + PROBE_TIMEOUT;
    while Instant::now() < deadline {
        if !term::wait_readable(fd, Duration::from_millis(50)) {
            continue;
        }
        if term::read_available(fd, &mut seen).unwrap_or(0) == 0 {
            break;
        }
        match kitty::classify_probe(&seen) {
            kitty::Support::Yes => return Ok(()),
            kitty::Support::No => return Err(unsupported()),
            kitty::Support::Undecided => {}
        }
    }
    // A terminal that answered neither question. Treating silence as support
    // would fill someone's screen with base64.
    Err(unsupported())
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
    fn new(opts: &Options, guard: &'a mut term::Guard) -> App<'a> {
        let size = guard.size();
        App {
            env_dir: opts.env_dir.clone(),
            guard,
            placer: kitty::Placer::new(),
            mode: Mode::View,
            last_frame: None,
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
    fn pump(&mut self, rx: mpsc::Receiver<Ev>, writer: &mut impl Write) -> Option<String> {
        self.draw_status();
        loop {
            match rx.recv_timeout(TICK) {
                Ok(Ev::Net(msg)) => {
                    if let Some(err) = self.on_net(msg, writer) {
                        return Some(err);
                    }
                }
                Ok(Ev::NetDone(why)) => return why,
                Ok(Ev::Input(bytes)) => {
                    self.pending.extend_from_slice(&bytes);
                    if self.on_input(false, writer) {
                        return None;
                    }
                }
                Ok(Ev::InputDone) => return None,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // Quiet input: a lone ESC is now the Escape key rather than
                    // the start of a sequence nobody finished.
                    if self.on_input(true, writer) {
                        return None;
                    }
                    self.on_tick();
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return None,
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
                    Some(proto::ServerMessage::ConsoleError(_) | proto::ServerMessage::PageError(_)) => {
                        self.errors = self.errors.saturating_add(1);
                        self.draw_status();
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
        let rows = self.size.rows.saturating_sub(CHROME_ROWS).max(1);
        let fit = image::fit(
            frame.width,
            frame.height,
            self.size.cols.max(1),
            rows,
            self.size.cell_w,
            self.size.cell_h,
        );
        let scaled = image::downscale(&frame, fit.pixel_width, fit.pixel_height);

        let at = kitty::Placement {
            row: CHROME_ROWS + 1,
            col: 1,
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
        let size = self.guard.size();
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
        if let Event::Key(k) = &ev {
            if k.code == KeyCode::Char(']') && k.modifiers & proto::modifiers::CTRL != 0 {
                self.leave_interact();
                return;
            }
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
        assert_eq!(CHROME_ROWS, 2, "row one is the status line, row two separates it");
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
