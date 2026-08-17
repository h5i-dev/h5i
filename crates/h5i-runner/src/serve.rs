//! The worker: one process per RPC, speaking frames on its own stdio.
//!
//! This is what an SSH forced command runs. It is stateless across invocations
//! by design (ROADMAP.md R3) — box state lives in the container runtime and the
//! state dir, never in a resident daemon, because there is no daemon. Nothing
//! listens on any interface, of any kind, ever.
//!
//! **Nothing but frames may reach stdout.** A stray `println!` in this path is
//! not a cosmetic bug, it is a corrupt stream: the client would read the text
//! as a length prefix. Diagnostics go to stderr, which the client captures and
//! puts in its error messages.
//!
//! The loop is written as a small state machine with an explicit disposition
//! for every failure, because "what happens to the session when a peer sends
//! something wrong" is the question a protocol most often leaves to accident.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::host;
use crate::proto::{
    self, Capabilities, ErrorCode, ErrorMsg, FrameKind, Hello, HelloAck, PROTOCOL_VERSION,
    ProtoError,
};
use crate::wire::{FrameReader, FrameWriter, Limits, WireError};

/// Override for the worker's state directory. A test drives a worker whose
/// storage is a tempdir; nothing else should set it.
pub const STATE_DIR_ENV: &str = "H5I_RUNNER_STATE_DIR";

#[derive(Debug, Error)]
pub enum ServeError {
    #[error(transparent)]
    Wire(#[from] WireError),
    #[error(transparent)]
    Proto(#[from] ProtoError),
}

/// What the worker knows about itself for the length of one session.
pub struct Worker {
    state_dir: PathBuf,
    /// The version to report as h5i's.
    ///
    /// Supplied by the binary rather than taken from this crate, because
    /// `CARGO_PKG_VERSION` here is *this crate's* version and the operator
    /// asking "which h5i is over there" means the product's. Reporting `0.1.0`
    /// to someone running h5i 0.3.4 is a small lie in the one field whose whole
    /// job is answering that question.
    version: String,
    /// Cached for the session: the capability probe shells out to `podman info`
    /// and runs a functional exec self-test, which is not something to repeat
    /// per frame. One session is short enough that nothing meaningful drifts
    /// inside it.
    capabilities: Option<Capabilities>,
}

impl Worker {
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        Self {
            state_dir: state_dir.into(),
            // A default so the library is usable alone; the binary overrides it
            // with its own version, which is the one that means anything.
            version: env!("CARGO_PKG_VERSION").to_string(),
            capabilities: None,
        }
    }

    /// Report this version as h5i's. The binary calls this with its own.
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    /// The worker's own state directory.
    pub fn default_state_dir() -> PathBuf {
        if let Some(v) = std::env::var_os(STATE_DIR_ENV).filter(|v| !v.is_empty()) {
            return PathBuf::from(v);
        }
        let base = std::env::var_os("XDG_DATA_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share"))
            })
            .unwrap_or_else(|| PathBuf::from("/var/lib"));
        base.join("h5i").join("runner")
    }

    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    fn capabilities(&mut self) -> &Capabilities {
        self.capabilities
            .get_or_insert_with(|| host::capabilities(&self.state_dir))
    }
}

/// What a failure does to the session.
enum Disposition {
    /// Answer and keep reading. For a request that is well formed and names
    /// something this build cannot do: the client may sensibly ask something
    /// else on the same channel.
    Answer(ErrorMsg),
    /// Answer and stop. For anything that means the peer is not speaking this
    /// protocol — an unknown frame type, a frame out of order, a payload that
    /// is not what its type says. Continuing would be guessing.
    Fatal(ErrorMsg),
}

/// Serve one session to completion.
///
/// Generic over the streams rather than tied to stdio, which is what lets the
/// whole worker be driven from a test with two in-memory buffers, in a build
/// with no SSH and no child process anywhere in it.
pub fn serve<R: Read, W: Write>(
    reader: R,
    writer: W,
    worker: &mut Worker,
) -> Result<(), ServeError> {
    // A control session is small. The budget is the receiver's, and it bounds
    // a peer that decides to talk forever — the frame cap alone does not.
    let limits = Limits::control();
    let mut frames = FrameReader::new(reader, limits);
    let mut out = FrameWriter::new(writer, limits);

    let mut agreed: Option<u16> = None;
    let version = worker.version.clone();

    loop {
        let frame = match frames.read() {
            Ok(Some(f)) => f,
            // The client closed cleanly. That is how every exchange ends.
            Ok(None) => return Ok(()),
            Err(e) => {
                // A framing failure leaves us out of step with the stream, so
                // there is nothing safe to send: any bytes we write would be
                // read at an offset the peer does not expect. Report it on
                // stderr, which the client captures, and stop.
                eprintln!("h5i runner: {e}");
                return Err(e.into());
            }
        };

        let Some(kind) = FrameKind::from_u8(frame.kind) else {
            let msg = ErrorMsg::new(
                ErrorCode::UnknownFrame,
                format!(
                    "frame type 0x{:02X} is not one this h5i understands — \
                     the other side is probably newer than this runner",
                    frame.kind
                ),
            );
            let _ = respond(&mut out, &msg);
            return Err(ProtoError::UnknownFrame(frame.kind).into());
        };

        // A worker that is sent its own reply types is not talking to a client.
        if !kind.is_client_to_worker() {
            let msg = ErrorMsg::new(
                ErrorCode::Sequence,
                format!("{} is a reply, and this side does not make requests", kind.as_str()),
            );
            let _ = respond(&mut out, &msg);
            return Err(ProtoError::Unexpected {
                expected: "a request",
                got: kind.as_str(),
            }
            .into());
        }

        let result = match (agreed, kind) {
            // KEEPALIVE is legal at any point and means nothing on its own.
            (_, FrameKind::KeepAlive) => continue,

            (None, FrameKind::Hello) => match handle_hello(&frame.payload, &version) {
                Ok((ack, protocol)) => {
                    agreed = Some(protocol);
                    write_msg(&mut out, FrameKind::HelloAck, &ack)?;
                    continue;
                }
                Err(e) => Err(Disposition::Fatal(ErrorMsg::new(e.code(), e.to_string()))),
            },

            // Nothing may precede the handshake: until it lands we do not know
            // what protocol the peer speaks, so we cannot know what its bytes
            // mean.
            (None, other) => Err(Disposition::Fatal(ErrorMsg::new(
                ErrorCode::Sequence,
                format!(
                    "{} arrived before HELLO — the handshake comes first",
                    other.as_str()
                ),
            ))),

            (Some(_), FrameKind::Hello) => Err(Disposition::Fatal(ErrorMsg::new(
                ErrorCode::Sequence,
                "HELLO arrived twice on one channel",
            ))),

            (Some(_), FrameKind::Probe) => {
                let caps = worker.capabilities().clone();
                write_msg(&mut out, FrameKind::Capabilities, &caps)?;
                continue;
            }

            // Declared on the wire, not yet built. Answered rather than fatal:
            // the client may ask something else on the same channel, and a
            // clear "that milestone has not landed" beats a closed pipe.
            (Some(_), other) => Err(Disposition::Answer(ErrorMsg::new(
                ErrorCode::Unimplemented,
                format!(
                    "{} is not implemented by this runner yet — \
                     R13.1 built pair and probe; create, exec and export follow",
                    other.as_str()
                ),
            ))),
        };

        match result {
            Ok(()) => {}
            Err(Disposition::Answer(msg)) => respond(&mut out, &msg)?,
            Err(Disposition::Fatal(msg)) => {
                let code = msg.code;
                let text = msg.message.clone();
                respond(&mut out, &msg)?;
                return Err(ProtoError::Refused {
                    code,
                    message: text,
                    log_tail: None,
                }
                .into());
            }
        }
    }
}

/// Serve on this process's own stdin and stdout: what the forced command runs.
pub fn serve_stdio(worker: &mut Worker) -> Result<(), ServeError> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    serve(stdin.lock(), stdout.lock(), worker)
}

fn handle_hello(payload: &[u8], version: &str) -> Result<(HelloAck, u16), ProtoError> {
    let hello: Hello = proto::decode("HELLO", payload)?;
    let protocol = proto::agreed_protocol(PROTOCOL_VERSION, hello.protocol)?;
    Ok((
        HelloAck {
            protocol: PROTOCOL_VERSION,
            h5i_version: version.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            os: std::env::consts::OS.to_string(),
            // Deliberately absent: identity is the client's to compute from the
            // host key it pinned, and a value we assert about ourselves is
            // exactly what pinning makes irrelevant (R5).
            runner_id_echo: None,
        },
        protocol,
    ))
}

fn write_msg<W: Write, T: serde::Serialize>(
    out: &mut FrameWriter<W>,
    kind: FrameKind,
    value: &T,
) -> Result<(), ServeError> {
    let payload = proto::encode(value)?;
    out.write(kind.as_u8(), &payload)?;
    Ok(())
}

fn respond<W: Write>(out: &mut FrameWriter<W>, msg: &ErrorMsg) -> Result<(), ServeError> {
    write_msg(out, FrameKind::Error, msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Frame;

    /// Drive a whole session from bytes to bytes. No process, no SSH: the
    /// worker's own loop, fed a script of frames.
    fn session(script: &[(FrameKind, Vec<u8>)]) -> (Vec<Frame>, Result<(), ServeError>) {
        let mut input = Vec::new();
        {
            let mut w = FrameWriter::new(&mut input, Limits::control());
            for (kind, payload) in script {
                w.write(kind.as_u8(), payload).expect("script frame");
            }
        }
        raw_session(&input)
    }

    fn raw_session(input: &[u8]) -> (Vec<Frame>, Result<(), ServeError>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut worker = Worker::new(dir.path());
        let mut output = Vec::new();
        let result = serve(input, &mut output, &mut worker);

        let mut frames = Vec::new();
        let mut r = FrameReader::new(output.as_slice(), Limits::permissive());
        while let Ok(Some(f)) = r.read() {
            frames.push(f);
        }
        (frames, result)
    }

    fn hello() -> (FrameKind, Vec<u8>) {
        let h = Hello {
            protocol: PROTOCOL_VERSION,
            h5i_version: "test".into(),
        };
        (FrameKind::Hello, proto::encode(&h).unwrap())
    }

    fn error_of(f: &Frame) -> ErrorMsg {
        assert_eq!(f.kind, FrameKind::Error.as_u8(), "expected an ERROR frame");
        proto::decode::<ErrorMsg>("ERROR", &f.payload).expect("an error message")
    }

    #[test]
    fn a_handshake_is_answered_and_the_session_ends_cleanly() {
        let (frames, result) = session(&[hello()]);
        assert!(result.is_ok(), "clean EOF is not an error: {result:?}");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].kind, FrameKind::HelloAck.as_u8());
        let ack: HelloAck = proto::decode("HELLO_ACK", &frames[0].payload).unwrap();
        assert_eq!(ack.protocol, PROTOCOL_VERSION);
        assert_eq!(ack.os, std::env::consts::OS);
    }

    #[test]
    fn the_handshake_carries_no_identity() {
        // Identity is the client's to compute from the pinned host key. A
        // worker that asserted its own would be a worker a compromised runner
        // could impersonate another with.
        let (frames, _) = session(&[hello()]);
        let ack: HelloAck = proto::decode("HELLO_ACK", &frames[0].payload).unwrap();
        assert!(ack.runner_id_echo.is_none());
    }

    #[test]
    fn a_probe_answers_with_capabilities() {
        let (frames, result) = session(&[hello(), (FrameKind::Probe, vec![])]);
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1].kind, FrameKind::Capabilities.as_u8());
        let caps: Capabilities = proto::decode("CAPABILITIES", &frames[1].payload).unwrap();
        assert_eq!(caps.arch, std::env::consts::ARCH);
    }

    #[test]
    fn nothing_may_precede_the_handshake() {
        // Until the handshake lands we do not know what protocol the peer
        // speaks, so we cannot know what its bytes mean.
        let (frames, result) = session(&[(FrameKind::Probe, vec![])]);
        assert!(result.is_err());
        assert_eq!(error_of(&frames[0]).code, ErrorCode::Sequence);
        assert_eq!(frames.len(), 1, "and the session is over");
    }

    #[test]
    fn a_second_handshake_ends_the_session() {
        let (frames, result) = session(&[hello(), hello()]);
        assert!(result.is_err());
        assert_eq!(frames.len(), 2);
        assert_eq!(error_of(&frames[1]).code, ErrorCode::Sequence);
    }

    #[test]
    fn a_reply_type_arriving_inbound_is_refused() {
        // A peer sending us CAPABILITIES is not a client.
        let (frames, result) = session(&[hello(), (FrameKind::Capabilities, b"{}".to_vec())]);
        assert!(result.is_err());
        assert_eq!(error_of(&frames[1]).code, ErrorCode::Sequence);
    }

    #[test]
    fn an_unknown_frame_type_ends_the_session_with_its_code_named() {
        let mut input = Vec::new();
        {
            let mut w = FrameWriter::new(&mut input, Limits::control());
            let (k, p) = hello();
            w.write(k.as_u8(), &p).unwrap();
            w.write(0xEE, b"from the future").unwrap();
            w.write(FrameKind::Probe.as_u8(), b"").unwrap();
        }
        let (frames, result) = raw_session(&input);
        assert!(matches!(result, Err(ServeError::Proto(ProtoError::UnknownFrame(0xEE)))));
        let err = error_of(&frames[1]);
        assert_eq!(err.code, ErrorCode::UnknownFrame);
        assert!(err.message.contains("0xEE"), "the code belongs in the message");
        assert_eq!(frames.len(), 2, "the frame after it is never processed");
    }

    #[test]
    fn a_malformed_handshake_payload_is_refused_not_guessed_at() {
        let (frames, result) = session(&[(FrameKind::Hello, b"{ not json".to_vec())]);
        assert!(result.is_err());
        assert_eq!(error_of(&frames[0]).code, ErrorCode::Malformed);
    }

    #[test]
    fn a_protocol_from_the_far_future_still_meets_us_at_ours() {
        // The lower version governs, so a newer client is not a failure.
        let h = Hello {
            protocol: u16::MAX,
            h5i_version: "from the future".into(),
        };
        let (frames, result) = session(&[(FrameKind::Hello, proto::encode(&h).unwrap())]);
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(frames[0].kind, FrameKind::HelloAck.as_u8());
    }

    #[test]
    fn a_protocol_too_old_to_meet_is_refused_at_the_handshake() {
        // Not later, in the middle of a create, as a mysterious unknown frame.
        let h = Hello {
            protocol: 0,
            h5i_version: "ancient".into(),
        };
        let (frames, result) = session(&[(FrameKind::Hello, proto::encode(&h).unwrap())]);
        assert!(result.is_err());
        assert_eq!(error_of(&frames[0]).code, ErrorCode::ProtocolVersion);
    }

    #[test]
    fn an_unbuilt_rpc_is_answered_and_the_channel_stays_open() {
        // The distinction that matters: "not yet built" is a fact about this
        // milestone, and the client may sensibly ask something else.
        let (frames, result) = session(&[
            hello(),
            (FrameKind::CreateBox, b"{}".to_vec()),
            (FrameKind::Probe, vec![]),
        ]);
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(frames.len(), 3);
        assert_eq!(error_of(&frames[1]).code, ErrorCode::Unimplemented);
        assert_eq!(
            frames[2].kind,
            FrameKind::Capabilities.as_u8(),
            "the channel survived the refusal"
        );
    }

    #[test]
    fn a_keepalive_is_legal_anywhere_and_means_nothing() {
        let (frames, result) = session(&[
            (FrameKind::KeepAlive, vec![]),
            hello(),
            (FrameKind::KeepAlive, vec![]),
            (FrameKind::Probe, vec![]),
        ]);
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(frames.len(), 2, "keepalives are answered with nothing");
    }

    #[test]
    fn a_truncated_stream_is_a_framing_failure_and_writes_nothing_back() {
        // Out of step with the stream, anything we wrote would be read at an
        // offset the peer does not expect.
        let mut input = Vec::new();
        {
            let mut w = FrameWriter::new(&mut input, Limits::control());
            let (k, p) = hello();
            w.write(k.as_u8(), &p).unwrap();
        }
        input.extend_from_slice(&[0, 0, 0, 64]); // a length with no body
        let (frames, result) = raw_session(&input);
        assert!(matches!(
            result,
            Err(ServeError::Wire(WireError::Truncated { .. }))
        ));
        assert_eq!(frames.len(), 1, "only the handshake answer");
    }

    #[test]
    fn an_oversized_declaration_is_refused_before_it_is_read() {
        let mut input = Vec::new();
        input.extend_from_slice(&u32::MAX.to_be_bytes());
        let (frames, result) = raw_session(&input);
        assert!(matches!(
            result,
            Err(ServeError::Wire(WireError::Oversized { .. }))
        ));
        assert!(frames.is_empty());
    }

    #[test]
    fn an_endless_peer_hits_the_session_budget() {
        // Every frame well formed, every frame under the cap, and far too many
        // of them. This is the case a per-frame limit alone does not cover.
        let mut input = Vec::new();
        {
            let mut w = FrameWriter::new(&mut input, Limits::permissive());
            let (k, p) = hello();
            w.write(k.as_u8(), &p).unwrap();
            for _ in 0..(Limits::control().max_frames + 10) {
                w.write(FrameKind::KeepAlive.as_u8(), b"").unwrap();
            }
        }
        let (_, result) = raw_session(&input);
        assert!(
            matches!(result, Err(ServeError::Wire(WireError::TotalFrames { .. }))),
            "expected the frame budget to end it, got {result:?}"
        );
    }

    #[test]
    fn the_version_reported_is_the_one_the_binary_supplied() {
        // `CARGO_PKG_VERSION` inside this crate is this crate's version, and
        // the field's whole job is answering "which h5i is over there".
        let dir = tempfile::tempdir().expect("tempdir");
        let mut worker = Worker::new(dir.path()).with_version("9.9.9-from-the-binary");
        let mut input = Vec::new();
        {
            let mut w = FrameWriter::new(&mut input, Limits::control());
            let (k, p) = hello();
            w.write(k.as_u8(), &p).unwrap();
        }
        let mut output = Vec::new();
        serve(input.as_slice(), &mut output, &mut worker).expect("serve");
        let mut r = FrameReader::new(output.as_slice(), Limits::permissive());
        let frame = r.read().unwrap().unwrap();
        let ack: HelloAck = proto::decode("HELLO_ACK", &frame.payload).unwrap();
        assert_eq!(ack.h5i_version, "9.9.9-from-the-binary");
    }

    #[test]
    fn the_state_dir_can_be_pointed_somewhere_for_a_test() {
        let dir = tempfile::tempdir().unwrap();
        let w = Worker::new(dir.path());
        assert_eq!(w.state_dir(), dir.path());
        // And the default is under the user's data dir, never the repo.
        let d = Worker::default_state_dir();
        assert!(d.ends_with("h5i/runner"), "{}", d.display());
    }
}
