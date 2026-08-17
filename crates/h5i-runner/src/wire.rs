//! The frame, as bytes.
//!
//! One shape for everything that crosses the runner boundary:
//!
//! ```text
//! [u32 big-endian length][u8 type][payload …]
//!  ^ counts the bytes after itself: the type byte plus the payload
//! ```
//!
//! So a frame is never shorter than one body byte, the length a peer declares
//! is checked before a single byte of body is read, and a reader that has
//! consumed the length knows exactly how much more to expect. JSON rides in the
//! payload for control messages and raw bytes ride there for stdio; this module
//! knows about neither, and does not know what any type code means. That is
//! [`crate::proto`]'s business, which is why an unknown type code is not an
//! error here: framing stays correct even when meaning does not, and a peer
//! that sends a frame we cannot interpret is a peer we can still stay in sync
//! with long enough to refuse it politely.
//!
//! It carries no transport, like `h5i-share`'s `wire` module and for the same
//! reason: the format is testable over an in-memory pipe, in a build with no
//! SSH and no child process anywhere near it.
//!
//! **Limits are per RPC, not just per frame** (ROADMAP.md R5). A 1 MiB cap on
//! one frame bounds one message and nothing else — a peer can send frames
//! forever. [`FrameReader`] therefore carries running totals and fails the RPC
//! the moment either is exceeded, so every caller inherits a bound instead of
//! having to remember one.

use std::io::{Read, Write};

use thiserror::Error;

/// Bytes of length prefix. Not configurable; it is the format.
const LEN_PREFIX: usize = 4;

/// The largest frame body (type byte + payload) this codec will read or write,
/// unless a caller narrows it further. Big enough for any control message and
/// for a useful stdio chunk, small enough that a hostile length is refused
/// before anything is allocated for it.
pub const MAX_FRAME: usize = 1024 * 1024;

/// What a peer can do to us at the framing layer, enumerated so that every case
/// has a name a test can assert on and an operator can read.
#[derive(Debug, Error)]
pub enum WireError {
    #[error("runner transport I/O: {0}")]
    Io(#[from] std::io::Error),

    /// The peer declared a body larger than the cap. **Nothing was read for
    /// it**: the declared length is refused on sight, so a peer cannot make us
    /// allocate or block on bytes it never intends to send. The session is over
    /// — we are no longer synchronised with the stream and must not guess.
    #[error("frame declares {declared} bytes, over the {max} byte cap — refusing to read it")]
    Oversized { declared: u64, max: usize },

    /// A frame that declares zero bytes has no room for a type byte. Same
    /// consequence as [`WireError::Oversized`]: the peer is not speaking this
    /// protocol and the session ends.
    #[error("frame declares no body, so it carries no type byte")]
    Empty,

    /// The stream ended partway through a frame: a half-written length prefix,
    /// or a body shorter than its own declaration. Distinct from a clean end of
    /// stream, which is not an error and which [`FrameReader::read`] reports as
    /// `Ok(None)`.
    #[error("stream ended {got} bytes into a {expected} byte {part} — the peer went away mid-frame")]
    Truncated {
        part: &'static str,
        expected: usize,
        got: usize,
    },

    /// The RPC's total byte budget is spent. Not a framing fault: every frame
    /// was well formed and there were too many of them.
    #[error("this exchange has read {read} bytes, over its {limit} byte budget")]
    TotalBytes { read: u64, limit: u64 },

    /// The RPC's frame-count budget is spent.
    #[error("this exchange has read {read} frames, over its {limit} frame budget")]
    TotalFrames { read: u64, limit: u64 },
}

/// One frame, after framing and before meaning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The type code, uninterpreted. [`crate::proto::FrameKind`] gives it a name
    /// — or declines to, which is how an unknown frame type is handled.
    pub kind: u8,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn new(kind: u8, payload: Vec<u8>) -> Self {
        Self { kind, payload }
    }
}

/// What one exchange is allowed to consume.
///
/// Every field is a *receiver* bound. A sender's declared size is a claim, and
/// a claim is not a budget: the receiver clamps to its own numbers and aborts
/// the moment a claim is exceeded, which is the same discipline the exec
/// timeout follows (ROADMAP.md R5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Cap on a single frame body. Never above [`MAX_FRAME`]; a caller may
    /// narrow it, and [`Limits::narrowed`] is how, so that no caller can widen
    /// the format's own ceiling by constructing the struct literally.
    max_frame: usize,
    /// Cap on the sum of every body in this exchange.
    pub max_total_bytes: u64,
    /// Cap on the number of frames in this exchange.
    pub max_frames: u64,
}

impl Limits {
    /// The format's ceiling, with generous totals: for a streaming exchange
    /// whose real bound is a deadline rather than a size.
    pub const fn permissive() -> Self {
        Self {
            max_frame: MAX_FRAME,
            max_total_bytes: 4 * 1024 * 1024 * 1024,
            max_frames: 1_000_000,
        }
    }

    /// A small exchange: a handshake, a probe, a refusal. Sized so that a peer
    /// which decides to talk forever is cut off long before it costs anything.
    pub const fn control() -> Self {
        Self {
            max_frame: 256 * 1024,
            max_total_bytes: 4 * 1024 * 1024,
            max_frames: 256,
        }
    }

    /// Narrow an existing budget. Every field takes the smaller of the two, so
    /// this can only tighten — a caller cannot widen the format's ceiling, and
    /// nesting one budget inside another is always safe.
    pub fn narrowed(self, max_frame: usize, max_total_bytes: u64, max_frames: u64) -> Self {
        Self {
            max_frame: self.max_frame.min(max_frame).min(MAX_FRAME),
            max_total_bytes: self.max_total_bytes.min(max_total_bytes),
            max_frames: self.max_frames.min(max_frames),
        }
    }

    pub fn max_frame(&self) -> usize {
        self.max_frame.min(MAX_FRAME)
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self::permissive()
    }
}

/// Read frames from a stream, under a budget.
pub struct FrameReader<R> {
    inner: R,
    limits: Limits,
    bytes: u64,
    frames: u64,
}

impl<R: Read> FrameReader<R> {
    pub fn new(inner: R, limits: Limits) -> Self {
        Self {
            inner,
            limits,
            bytes: 0,
            frames: 0,
        }
    }

    /// Bytes of frame body consumed so far.
    pub fn bytes_read(&self) -> u64 {
        self.bytes
    }

    /// Frames consumed so far.
    pub fn frames_read(&self) -> u64 {
        self.frames
    }

    /// The next frame, or `Ok(None)` when the peer closed the stream cleanly at
    /// a frame boundary — which is how an exchange ends and is not an error.
    ///
    /// Every error this returns is terminal for the session. Once a length has
    /// been read and refused, or a body has ended early, the reader's position
    /// in the stream is unknown and the only correct move is to stop.
    pub fn read(&mut self) -> Result<Option<Frame>, WireError> {
        let mut len_buf = [0u8; LEN_PREFIX];
        match fill(&mut self.inner, &mut len_buf)? {
            0 => return Ok(None),
            n if n < LEN_PREFIX => {
                return Err(WireError::Truncated {
                    part: "length prefix",
                    expected: LEN_PREFIX,
                    got: n,
                });
            }
            _ => {}
        }

        let declared = u32::from_be_bytes(len_buf) as u64;
        if declared == 0 {
            return Err(WireError::Empty);
        }
        let max_frame = self.limits.max_frame() as u64;
        if declared > max_frame {
            return Err(WireError::Oversized {
                declared,
                max: max_frame as usize,
            });
        }

        // Budgets are charged against the *declared* size, before the body is
        // read. Charging afterwards would let a peer spend the whole budget in
        // one frame that we have already paid to receive.
        self.frames += 1;
        if self.frames > self.limits.max_frames {
            return Err(WireError::TotalFrames {
                read: self.frames,
                limit: self.limits.max_frames,
            });
        }
        self.bytes += declared;
        if self.bytes > self.limits.max_total_bytes {
            return Err(WireError::TotalBytes {
                read: self.bytes,
                limit: self.limits.max_total_bytes,
            });
        }

        let body_len = declared as usize;
        let mut body = vec![0u8; body_len];
        let got = fill(&mut self.inner, &mut body)?;
        if got < body_len {
            return Err(WireError::Truncated {
                part: "frame body",
                expected: body_len,
                got,
            });
        }

        let kind = body[0];
        body.remove(0);
        Ok(Some(Frame::new(kind, body)))
    }

    /// Give the underlying reader back, for a caller that owns what happens to
    /// the stream after the exchange.
    pub fn into_inner(self) -> R {
        self.inner
    }
}

/// Write frames to a stream.
pub struct FrameWriter<W> {
    inner: W,
    max_frame: usize,
}

impl<W: Write> FrameWriter<W> {
    pub fn new(inner: W, limits: Limits) -> Self {
        Self {
            inner,
            max_frame: limits.max_frame(),
        }
    }

    /// Write one frame and flush it.
    ///
    /// The whole frame is assembled in one buffer and handed to a single
    /// `write_all`. That is not an optimisation: two writers sharing a stream
    /// must never interleave half a frame, and the cheapest way to guarantee it
    /// is to never hold a partially written frame.
    pub fn write(&mut self, kind: u8, payload: &[u8]) -> Result<(), WireError> {
        let body_len = payload.len() + 1;
        if body_len > self.max_frame {
            return Err(WireError::Oversized {
                declared: body_len as u64,
                max: self.max_frame,
            });
        }
        let mut buf = Vec::with_capacity(LEN_PREFIX + body_len);
        buf.extend_from_slice(&(body_len as u32).to_be_bytes());
        buf.push(kind);
        buf.extend_from_slice(payload);
        self.inner.write_all(&buf)?;
        self.inner.flush()?;
        Ok(())
    }

    pub fn into_inner(self) -> W {
        self.inner
    }
}

/// Read until `buf` is full or the stream ends, returning how many bytes
/// landed. `read_exact` would do this but collapses "ended cleanly at zero"
/// and "ended three bytes in" into one error, and those are the two cases the
/// caller most needs to tell apart.
fn fill<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<usize, WireError> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(WireError::Io(e)),
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(kind: u8, payload: &[u8]) -> Frame {
        let mut buf = Vec::new();
        FrameWriter::new(&mut buf, Limits::permissive())
            .write(kind, payload)
            .expect("write");
        FrameReader::new(buf.as_slice(), Limits::permissive())
            .read()
            .expect("read")
            .expect("a frame")
    }

    #[test]
    fn a_frame_survives_the_round_trip() {
        let f = roundtrip(0x30, b"hello");
        assert_eq!(f.kind, 0x30);
        assert_eq!(f.payload, b"hello");
    }

    #[test]
    fn an_empty_payload_is_a_frame_too() {
        // Every signalling frame — DATA_DONE, CLOSE_STDIN, a bare PROBE — is
        // this shape, so the one-byte body is the common case and not an edge.
        let f = roundtrip(0x39, b"");
        assert_eq!(f.kind, 0x39);
        assert!(f.payload.is_empty());
        let mut buf = Vec::new();
        FrameWriter::new(&mut buf, Limits::permissive())
            .write(0x39, b"")
            .unwrap();
        assert_eq!(buf, vec![0, 0, 0, 1, 0x39]);
    }

    #[test]
    fn frames_come_back_in_order_and_the_stream_ends_cleanly() {
        let mut buf = Vec::new();
        {
            let mut w = FrameWriter::new(&mut buf, Limits::permissive());
            w.write(1, b"one").unwrap();
            w.write(2, b"two").unwrap();
        }
        let mut r = FrameReader::new(buf.as_slice(), Limits::permissive());
        assert_eq!(r.read().unwrap().unwrap().payload, b"one");
        assert_eq!(r.read().unwrap().unwrap().payload, b"two");
        // Clean end of stream is `None`, never an error: it is how every
        // exchange finishes.
        assert!(r.read().unwrap().is_none());
    }

    #[test]
    fn an_oversized_declaration_is_refused_before_a_byte_of_it_is_read() {
        // The hostile case: a peer declares four gigabytes and sends none of
        // them. Nothing may be allocated and nothing may block.
        let mut stream = Vec::new();
        stream.extend_from_slice(&u32::MAX.to_be_bytes());
        let mut r = FrameReader::new(stream.as_slice(), Limits::permissive());
        match r.read() {
            Err(WireError::Oversized { declared, max }) => {
                assert_eq!(declared, u32::MAX as u64);
                assert_eq!(max, MAX_FRAME);
            }
            other => panic!("expected Oversized, got {other:?}"),
        }
    }

    #[test]
    fn a_narrowed_budget_refuses_what_the_format_would_allow() {
        let limits = Limits::permissive().narrowed(64, 1024, 8);
        let mut buf = Vec::new();
        FrameWriter::new(&mut buf, Limits::permissive())
            .write(1, &[0u8; 128])
            .unwrap();
        let mut r = FrameReader::new(buf.as_slice(), limits);
        assert!(matches!(r.read(), Err(WireError::Oversized { .. })));

        // And narrowing only ever tightens, so a caller cannot widen the
        // format's own ceiling by asking for more.
        let widened = Limits::control().narrowed(usize::MAX, u64::MAX, u64::MAX);
        assert_eq!(widened.max_frame(), Limits::control().max_frame());
        assert_eq!(
            widened.max_total_bytes,
            Limits::control().max_total_bytes,
            "narrowing must not widen the total budget either"
        );
    }

    #[test]
    fn a_zero_length_frame_carries_no_type_byte() {
        let stream = 0u32.to_be_bytes().to_vec();
        let mut r = FrameReader::new(stream.as_slice(), Limits::permissive());
        assert!(matches!(r.read(), Err(WireError::Empty)));
    }

    #[test]
    fn a_truncated_length_prefix_is_not_a_clean_end() {
        let stream = vec![0u8, 0u8];
        let mut r = FrameReader::new(stream.as_slice(), Limits::permissive());
        match r.read() {
            Err(WireError::Truncated {
                part,
                expected,
                got,
            }) => {
                assert_eq!(part, "length prefix");
                assert_eq!((expected, got), (4, 2));
            }
            other => panic!("expected Truncated, got {other:?}"),
        }
    }

    #[test]
    fn a_body_shorter_than_its_declaration_is_truncated() {
        let mut stream = Vec::new();
        stream.extend_from_slice(&16u32.to_be_bytes());
        stream.extend_from_slice(b"only ten b");
        let mut r = FrameReader::new(stream.as_slice(), Limits::permissive());
        match r.read() {
            Err(WireError::Truncated {
                part,
                expected,
                got,
            }) => {
                assert_eq!(part, "frame body");
                assert_eq!((expected, got), (16, 10));
            }
            other => panic!("expected Truncated, got {other:?}"),
        }
    }

    #[test]
    fn the_total_byte_budget_stops_an_endless_peer() {
        // Every frame here is well formed and under the frame cap. The bound
        // that matters is the sum, which is the whole point of R5's per-RPC
        // limits: a 1 MiB frame cap alone bounds nothing.
        let limits = Limits::permissive().narrowed(MAX_FRAME, 40, 1000);
        let mut buf = Vec::new();
        {
            let mut w = FrameWriter::new(&mut buf, Limits::permissive());
            for _ in 0..10 {
                w.write(1, &[0u8; 9]).unwrap();
            }
        }
        let mut r = FrameReader::new(buf.as_slice(), limits);
        let mut ok = 0;
        loop {
            match r.read() {
                Ok(Some(_)) => ok += 1,
                Ok(None) => panic!("budget should have ended this before the stream did"),
                Err(WireError::TotalBytes { limit, .. }) => {
                    assert_eq!(limit, 40);
                    break;
                }
                Err(e) => panic!("expected TotalBytes, got {e:?}"),
            }
        }
        assert_eq!(ok, 4, "four ten-byte bodies fit in a forty byte budget");
    }

    #[test]
    fn the_frame_count_budget_stops_a_chatty_peer() {
        let limits = Limits::permissive().narrowed(MAX_FRAME, u64::MAX, 3);
        let mut buf = Vec::new();
        {
            let mut w = FrameWriter::new(&mut buf, Limits::permissive());
            for _ in 0..10 {
                w.write(1, b"").unwrap();
            }
        }
        let mut r = FrameReader::new(buf.as_slice(), limits);
        for _ in 0..3 {
            assert!(r.read().unwrap().is_some());
        }
        match r.read() {
            Err(WireError::TotalFrames { limit, .. }) => assert_eq!(limit, 3),
            other => panic!("expected TotalFrames, got {other:?}"),
        }
    }

    #[test]
    fn a_writer_refuses_to_emit_what_a_reader_would_refuse() {
        // The two caps are the same number on purpose: a frame this codec
        // writes must be a frame this codec can read, or the first oversized
        // payload becomes a peer's problem to diagnose instead of ours.
        let mut buf = Vec::new();
        let mut w = FrameWriter::new(&mut buf, Limits::permissive());
        let too_big = vec![0u8; MAX_FRAME];
        match w.write(1, &too_big) {
            Err(WireError::Oversized { declared, max }) => {
                assert_eq!(declared, MAX_FRAME as u64 + 1);
                assert_eq!(max, MAX_FRAME);
            }
            other => panic!("expected Oversized, got {other:?}"),
        }
        assert!(buf.is_empty(), "nothing may be written for a refused frame");
    }

    #[test]
    fn a_frame_at_exactly_the_cap_is_allowed() {
        // Off-by-one at the boundary is the classic way a cap becomes a bug.
        let payload = vec![7u8; MAX_FRAME - 1];
        let f = roundtrip(0x21, &payload);
        assert_eq!(f.payload.len(), MAX_FRAME - 1);
    }

    #[test]
    fn an_unknown_type_code_still_frames() {
        // Framing must survive meaning it does not have. The session ends on an
        // unknown *kind*, but that is proto's decision, made after this module
        // has kept the stream synchronised well enough to send a refusal.
        let f = roundtrip(0xEE, b"payload of a type we do not know");
        assert_eq!(f.kind, 0xEE);
    }

    #[test]
    fn payload_bytes_are_never_interpreted() {
        // Whatever a length prefix looks like inside a payload, it is payload.
        let mut nasty = Vec::new();
        nasty.extend_from_slice(&u32::MAX.to_be_bytes());
        nasty.extend_from_slice(b"\x00\x00\x00\x00");
        let f = roundtrip(0x32, &nasty);
        assert_eq!(f.payload, nasty);
    }
}
