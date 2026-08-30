//! Pixels, from the box to the console (roadmap-history.md M11a).
//!
//! The evidence panes answer *what did the agent do*; this answers *what did the
//! page look like while it did it*.
//!
//! **This does not make the console a remote control.** `h5i ui`'s guarantee is
//! that every route is a `GET` ([`crate::server`]), and the relay does not spend
//! it, being one-directional by construction: the console connects out into the
//! box and reads, so nothing new listens and the box gains no reachability. The
//! only upstream messages are `config` and `ack`, the pacing the stream server
//! needs, and no code path runs from an HTTP request to a message on this
//! socket. Typing still goes through [`crate::view`]'s forward with its own
//! per-box token and the control lock. Takeover has one door, and it is not this
//! one.
//!
//! **A frame is box-claimed.** It is the box's rendering of its own page,
//! arriving as pixels the box chose, so the console shows what the box reports
//! and the reader decides what that is worth. Nothing derived from a frame
//! reaches the trusted status row.
//!
//! One caveat this module cannot fix: `h5i-browser-light` has no resident
//! session, so a live view served by it shows the page *it* was started on
//! rather than the one the agent is driving. Chromium's agent-browser is one
//! daemon owning one session, so its frames really are the agent's page. The
//! console reports which case it is in rather than letting a picture imply the
//! stronger one.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::termview::{proto, ws};

/// Frames per second the console asks the box for.
///
/// Deliberately low. The console is a monitoring surface: it re-fetches the
/// newest frame when the sequence number changes, so anything faster than a
/// human notices is bytes across a namespace boundary for nothing. The engine
/// is change-driven anyway — a still page sends nothing at any cap.
const MAX_FPS: u32 = 4;

/// Largest frame the relay will hold. The WebSocket reader already refuses
/// anything over its own 16 MiB message cap; this is the narrower question of
/// what is worth keeping in the console's memory, one per box.
const MAX_FRAME_BYTES: usize = 4 << 20;

/// How long a dead relay waits before the next poll may start another.
///
/// Without it, a box with no live view would have the console forking, entering
/// namespaces, and failing to connect once per request for as long as the tab
/// is open.
const RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(5);

/// The newest frame the box has sent.
#[derive(Debug, Clone)]
pub struct Frame {
    /// The stream's own sequence number, which is what lets a viewer re-fetch
    /// only on change instead of on a clock.
    pub seq: u64,
    pub jpeg: Vec<u8>,
}

/// A running reader of one box's live view.
///
/// Shaped like [`crate::browser_proxy::MediatorHandle`] — a stop flag and a
/// `Drop` that joins — because the console already knows how to hold something
/// for the life of a session and let it go at the end.
pub struct FrameRelay {
    latest: Arc<Mutex<Option<Frame>>>,
    /// Set when the reader has stopped for any reason. The next poll consults
    /// it to decide whether to start a replacement.
    finished: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
    /// Why the last attempt ended, for the pane to show instead of a blank
    /// rectangle. A viewer that cannot say why it has no picture is indistinct
    /// from one that is broken.
    error: Arc<Mutex<Option<String>>>,
    started: std::time::Instant,
}

impl FrameRelay {
    /// Connect to the box's live view and start reading frames.
    ///
    /// Returns immediately; the connection happens on the reader thread,
    /// because entering a namespace involves a fork and a round trip and the
    /// console must not block a request on it.
    pub fn start(pid: u32, port: u16, pid_ns: std::ffi::OsString) -> Self {
        let latest: Arc<Mutex<Option<Frame>>> = Arc::new(Mutex::new(None));
        let finished = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        let join = {
            let latest = latest.clone();
            let finished = finished.clone();
            let stop = stop.clone();
            let error = error.clone();
            std::thread::spawn(move || {
                if let Err(e) = pump(pid, port, &pid_ns, &latest, &stop)
                    && let Ok(mut slot) = error.lock()
                {
                    *slot = Some(e.to_string());
                }
                finished.store(true, Ordering::SeqCst);
            })
        };

        Self {
            latest,
            finished,
            stop,
            join: Some(join),
            error,
            started: std::time::Instant::now(),
        }
    }

    pub fn latest(&self) -> Option<Frame> {
        self.latest.lock().ok().and_then(|f| f.clone())
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|e| e.clone())
    }

    /// Should the caller replace this relay? True once the reader has stopped
    /// and the retry delay has passed.
    pub fn spent(&self) -> bool {
        self.finished.load(Ordering::SeqCst) && self.started.elapsed() >= RETRY_AFTER
    }

    /// Is a reader still attached? Distinct from "has a frame": a connected
    /// relay watching a still page holds nothing, and that is not a failure.
    pub fn connected(&self) -> bool {
        !self.finished.load(Ordering::SeqCst)
    }
}

impl Drop for FrameRelay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // The reader blocks on the socket, so the flag alone will not wake it.
        // It is a detached read on a socket owned by this process; letting the
        // thread go and closing the descriptor on process exit is the honest
        // trade against carrying a shutdown channel through a blocking read.
        if let Some(join) = self.join.take()
            && join.is_finished()
        {
            let _ = join.join();
        }
    }
}

/// Connect, handshake, and read frames until told to stop.
///
/// The message vocabulary here is the whole security argument, so it is short
/// enough to check by eye: `config` once, then `ack` per frame. No other write
/// exists in this function.
fn pump(
    pid: u32,
    port: u16,
    pid_ns: &std::ffi::OsStr,
    latest: &Arc<Mutex<Option<Frame>>>,
    stop: &Arc<AtomicBool>,
) -> Result<(), h5i_error::H5iError> {
    // The socket comes back from a fork that entered the box's user and network
    // namespaces — the same route `h5i box view --term` takes. Nothing binds,
    // nothing is punched through the namespace.
    let mut socket = crate::view::connect_in_netns(pid, port, pid_ns)?;

    let key = ws::new_key();
    socket
        .write_all(ws::request(&format!("127.0.0.1:{port}"), "/", &key).as_bytes())
        .map_err(h5i_error::H5iError::Io)?;
    socket.flush().map_err(h5i_error::H5iError::Io)?;
    let head = ws::read_head(&mut socket).map_err(h5i_error::H5iError::Io)?;
    ws::verify_response(&head, &key)?;

    let mut writer = socket.try_clone().map_err(h5i_error::H5iError::Io)?;
    // Ack pacing: the server waits for an ack before sending the next frame, so
    // a console that stops reading stops costing the box anything.
    ws::send_text(&mut writer, &proto::config_ack_pacing(MAX_FPS).to_string())
        .map_err(h5i_error::H5iError::Io)?;

    let mut reader = ws::Reader::new(socket);
    while !stop.load(Ordering::SeqCst) {
        let Some(message) = reader.next_message().map_err(h5i_error::H5iError::Io)? else {
            return Ok(()); // clean end of stream
        };
        match message {
            ws::Incoming::Text(text) => {
                let Some(proto::ServerMessage::Frame { seq, data }) = proto::parse(&text) else {
                    // Status, url, console and page-error messages all arrive
                    // here. They are the evidence panes' business, and those
                    // read them from the receipt lanes rather than from a
                    // socket that may not be connected — so they are skipped
                    // rather than half-handled in two places.
                    continue;
                };
                let seq = seq.unwrap_or(0);
                let Some(frame) = decode_frame(seq, &data) else {
                    continue;
                };
                if let Ok(mut slot) = latest.lock() {
                    *slot = Some(frame);
                }
                // Ack *after* storing, so the next frame cannot replace one the
                // console has not taken yet.
                if ws::send_text(&mut writer, &proto::ack(seq).to_string()).is_err() {
                    return Ok(());
                }
            }
            ws::Incoming::Ping(payload) => {
                let _ = ws::send_pong(&mut writer, &payload);
            }
            ws::Incoming::Close => return Ok(()),
            // A binary frame is not part of this protocol; ignoring it is
            // cheaper than deciding what a box meant by it.
            ws::Incoming::Binary(_) | ws::Incoming::Pong(_) => {}
        }
    }
    let _ = ws::send_close(&mut writer);
    Ok(())
}

/// Turn a frame message's payload into bytes worth keeping.
///
/// `None` for anything that is not decodable base64, and for anything over
/// [`MAX_FRAME_BYTES`]: the console holds one frame per box for as long as the
/// tab is open, so a box that sends a 50 MB "frame" must not be able to make it
/// hold that. Its own function so the refusal is testable without a socket.
fn decode_frame(seq: u64, data: &str) -> Option<Frame> {
    use base64::Engine as _;
    let jpeg = base64::engine::general_purpose::STANDARD.decode(data).ok()?;
    (jpeg.len() <= MAX_FRAME_BYTES).then_some(Frame { seq, jpeg })
}

/// Where a box's live view is, if one is running: its pid and stream port.
///
/// `None` is the ordinary case — most boxes are not serving a view — so the
/// caller reports it as a state rather than as an error.
pub fn locate(env_dir: &std::path::Path) -> Option<(u32, u16, std::ffi::OsString)> {
    let port = crate::view::stream_port(env_dir)?;
    // The namespace comes back with the pid: a pid is not an identity, and the
    // reader below enters what this walk found rather than whatever holds the
    // number when it gets there. See `view::connect_in_netns`.
    let (pid, ns) = crate::view::box_pid_ns(env_dir)?;
    Some((pid, port, ns))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_relay_speaks_only_pacing_upstream() {
        // The console's read-only guarantee, checked against the source rather
        // than asserted in a comment: `pump` is the only function that writes to
        // the box, and the only payloads it can produce are the handshake, a
        // config, an ack, a pong and a close. If an `input_` message ever
        // appears in this file, the console has quietly become a remote control
        // and this test is the thing that notices.
        let source = include_str!("browser_frames.rs");
        // Skip this test's own body, or it would match the strings it checks for.
        let code = source.split("mod tests").next().unwrap_or("");
        for forbidden in ["input_mouse", "input_keyboard", "input_touch"] {
            assert!(
                !code.contains(forbidden),
                "the frame relay must never send `{forbidden}` — input belongs to \
                 the forward, which enforces the control lock"
            );
        }
        assert!(code.contains("config_ack_pacing"), "pacing is expected");
        assert!(code.contains("proto::ack"), "acks are expected");
    }

    #[test]
    fn a_frame_over_the_cap_is_dropped_rather_than_held() {
        // The console holds one frame per box for as long as a tab is open, so
        // an oversized one is refused rather than kept. Driven with real base64
        // through the real decoder: the earlier version of this test compared
        // two constants, which is a tautology and would have passed with the
        // check deleted.
        use base64::Engine as _;
        let encode =
            |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);

        let ok = encode(&[0xffu8, 0xd8, 0xff]); // a JPEG's opening bytes
        assert_eq!(
            decode_frame(7, &ok).map(|f| (f.seq, f.jpeg.len())),
            Some((7, 3)),
            "an ordinary frame is kept, with its sequence number"
        );

        let huge = encode(&vec![0u8; MAX_FRAME_BYTES + 1]);
        assert!(decode_frame(8, &huge).is_none(), "one byte over is refused");

        assert!(
            decode_frame(9, "not base64 at all !!!").is_none(),
            "and undecodable payloads never become a frame"
        );
    }

    #[test]
    fn a_box_with_no_live_view_locates_nothing() {
        let td = tempfile::TempDir::new().unwrap();
        assert!(
            locate(td.path()).is_none(),
            "no .stream file means no view, not an error"
        );
    }
}
