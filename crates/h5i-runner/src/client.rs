//! The control-plane side: opening a channel, shaking hands, asking one thing.
//!
//! Every method here is one channel and one RPC (ROADMAP.md R4), and each one
//! starts by proving the peer is there before it starts the longer clock for
//! the request. That is the two-clock discipline R5 asks for, made real by
//! [`Channel::rearm`] rather than asserted: a peer that never answers the
//! handshake is killed in seconds, and a peer that answers and then takes a
//! while to do the work is given the time the work needs.

use std::io::Read;
use std::process::{ChildStdin, ChildStdout};

use thiserror::Error;

use crate::proto::{
    self, Capabilities, CreateRequest, CreateResult, DestroyRequest, DestroyResult, ErrorMsg,
    ExecRequest, ExecStarted, ExitMsg, ExportRequest, ExportResult, FrameKind, GcRequest, GcResult,
    Hello, HelloAck, ListResult, PROTOCOL_VERSION, ProtoError,
};
use crate::source::Bundle;
use crate::transport::{Channel, Deadlines, Transport, TransportError};
use crate::wire::{FrameReader, FrameWriter, Limits, WireError};

#[derive(Debug, Error)]
pub enum ClientError {
    #[error(transparent)]
    Transport(#[from] TransportError),

    #[error(transparent)]
    Wire(#[from] WireError),

    #[error(transparent)]
    Proto(#[from] ProtoError),

    /// The peer stopped talking mid-exchange. Carries whatever it said on
    /// stderr, because for an SSH channel that is where the real diagnosis
    /// lives: "Permission denied", "Host key verification failed", "command not
    /// found" are all stderr, and none of them is a protocol event.
    #[error("{what} closed the connection before answering{}", stderr_tail(.stderr))]
    Closed { what: String, stderr: String },

    #[error("{what} did not answer in time")]
    TimedOut { what: String },

    /// The worker enforced a policy that is not the one this side resolved.
    ///
    /// Cheap to check and worth checking: it turns "the runner silently ran an
    /// older policy" from a possibility into a detected fault (ROADMAP.md R7).
    #[error(
        "the runner built the box under a different policy than the one resolved here — \
         expected {expected}, it enforced {enforced}. The box was not accepted."
    )]
    PolicyDigest { expected: String, enforced: String },

    #[error("could not read the source to send: {0}")]
    Source(#[from] crate::source::SourceError),
}

fn stderr_tail(stderr: &str) -> String {
    let t = stderr.trim();
    if t.is_empty() {
        String::new()
    } else {
        format!(":\n{t}")
    }
}

/// One command's worth of result.
#[derive(Debug, Clone)]
pub struct ExecOutput {
    pub started: ExecStarted,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit: ExitMsg,
}

impl ExecOutput {
    /// The conventional shell meaning: zero is success, and a command killed by
    /// a timeout is not.
    pub fn success(&self) -> bool {
        !self.exit.timed_out && self.exit.exit_code == Some(0)
    }
}

/// What a handshake plus a probe learned.
#[derive(Debug, Clone)]
pub struct Probed {
    pub ack: HelloAck,
    pub capabilities: Capabilities,
}

/// Talks to one runner.
pub struct Client {
    transport: Box<dyn Transport>,
    deadlines: Deadlines,
}

impl Client {
    pub fn new(transport: Box<dyn Transport>) -> Self {
        Self {
            transport,
            deadlines: Deadlines::default(),
        }
    }

    pub fn with_deadlines(mut self, deadlines: Deadlines) -> Self {
        self.deadlines = deadlines;
        self
    }

    pub fn describe(&self) -> String {
        self.transport.describe()
    }

    /// Open a channel and shake hands, and nothing else.
    ///
    /// This is what pairing runs first: it answers "is there an h5i over there,
    /// and can we agree on a protocol" before anything is written to disk.
    pub fn hello(&self) -> Result<HelloAck, ClientError> {
        let mut session = Session::open(&*self.transport, self.deadlines)?;
        let ack = session.ack.clone();
        session.close()?;
        Ok(ack)
    }

    /// Handshake, then ask what this machine can do right now.
    ///
    /// The capability report is validated here, on receipt, and a report that
    /// fails validation is an error rather than a stored value. R13.1's
    /// "hostile capability values are clamped or refused, never stored".
    pub fn probe(&self) -> Result<Probed, ClientError> {
        let mut session = Session::open(&*self.transport, self.deadlines)?;
        session.send(FrameKind::Probe, &())?;
        let caps: Capabilities = session.expect_message(FrameKind::Capabilities, "CAPABILITIES")?;
        let capabilities = caps.sanitized()?;
        let ack = session.ack.clone();
        session.close()?;
        Ok(Probed { ack, capabilities })
    }

    /// Make a box on the runner.
    ///
    /// `bundle` is `None` for an empty source. When it is `Some`, its bytes go
    /// out as `DATA` frames after the request and before `DATA_DONE`, on the
    /// same channel. The transfer is part of this RPC rather than a second one
    /// that could arrive without it.
    ///
    /// The policy digest the worker echoes is checked here, not merely logged:
    /// it is the one thing that says the box was built under the policy this
    /// side resolved (ROADMAP.md R7).
    pub fn create(
        &self,
        request: &CreateRequest,
        bundle: Option<&Bundle>,
    ) -> Result<CreateResult, ClientError> {
        let mut session = Session::open(&*self.transport, self.deadlines)?;
        session.send(FrameKind::CreateBox, request)?;

        if let Some(bundle) = bundle {
            session.send_file(&bundle.path)?;
        }

        let result: CreateResult = session.expect_message(FrameKind::CreateResult, "CREATE_RESULT")?;
        let result = result.sanitized()?;
        session.close()?;

        if result.policy_digest != request.policy_digest {
            return Err(ClientError::PolicyDigest {
                expected: request.policy_digest.clone(),
                enforced: result.policy_digest,
            });
        }
        Ok(result)
    }

    pub fn destroy(&self, box_id: &str, force: bool) -> Result<DestroyResult, ClientError> {
        let req = DestroyRequest {
            box_id: box_id.to_string(),
            force,
        };
        let mut session = Session::open(&*self.transport, self.deadlines)?;
        session.send(FrameKind::DestroyBox, &req)?;
        let result: DestroyResult =
            session.expect_message(FrameKind::DestroyResult, "DESTROY_RESULT")?;
        let result = result.sanitized()?;
        session.close()?;
        Ok(result)
    }

    pub fn list_boxes(&self) -> Result<ListResult, ClientError> {
        let mut session = Session::open(&*self.transport, self.deadlines)?;
        session.send(FrameKind::ListBoxes, &())?;
        let result: ListResult = session.expect_message(FrameKind::BoxList, "BOX_LIST")?;
        let result = result.sanitized()?;
        session.close()?;
        Ok(result)
    }

    /// Run a command in a box on the runner.
    ///
    /// Returns once the command has finished. The `EXEC_STARTED` frame is
    /// waited for first, which is what separates "it spawned" from "here is
    /// what it printed". A spawn failure is reported as one rather than as an
    /// empty result.
    pub fn exec(&self, request: &ExecRequest) -> Result<ExecOutput, ClientError> {
        let mut session = Session::open(&*self.transport, self.deadlines)?;
        session.send(FrameKind::Exec, request)?;

        let started: ExecStarted = session
            .expect_message::<ExecStarted>(FrameKind::ExecStarted, "EXEC_STARTED")?
            .sanitized()?;

        // The output's own budget. Without it the handshake's budget would
        // still be in force, and a command with a real amount of output would
        // be refused by its own client.
        session.reader.begin_rpc(crate::proto::exec_limits());

        // The run's own clock, from the moment it really started, and generous
        // enough to cover what the worker said it would allow. A client that
        // gave up before the worker's own timeout would report a hang for a
        // command that was going to finish.
        session.rearm(std::time::Duration::from_secs(started.timeout_secs.saturating_add(60)));

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit: ExitMsg = session.collect_until_exit(&mut stdout, &mut stderr)?;
        session.close()?;

        Ok(ExecOutput {
            started,
            stdout,
            stderr,
            exit,
        })
    }

    /// Fetch what a box has become, into `into`.
    ///
    /// The bundle is verified against the digest the worker described it with
    /// before this returns, so a caller never receives a path to bytes that
    /// were not checked.
    pub fn export(
        &self,
        box_id: &str,
        into: &std::path::Path,
    ) -> Result<ExportResult, ClientError> {
        let mut session = Session::open(&*self.transport, self.deadlines)?;
        session.send(
            FrameKind::ExportBox,
            &ExportRequest {
                box_id: box_id.to_string(),
            },
        )?;

        let described: ExportResult = session
            .expect_message::<ExportResult>(FrameKind::ExportDone, "EXPORT_RESULT")?
            .sanitized()?;

        // Its own budget, sized from what the worker just said it would send.
        session.begin_transfer();
        let mut rx = crate::source::Receiver::new(
            into.to_path_buf(),
            described.bytes,
            crate::proto::MAX_SOURCE_BYTES,
        )?;
        session.drain_data(&mut rx)?;
        rx.finish(&described.sha256)?;
        session.close()?;
        Ok(described)
    }

    pub fn gc(&self, all: bool) -> Result<GcResult, ClientError> {
        let mut session = Session::open(&*self.transport, self.deadlines)?;
        session.send(FrameKind::Gc, &GcRequest { all })?;
        let result: GcResult = session.expect_message(FrameKind::GcResult, "GC_RESULT")?;
        let result = result.sanitized()?;
        session.close()?;
        Ok(result)
    }
}

/// One channel, from handshake to close.
struct Session {
    what: String,
    channel: Option<Channel>,
    reader: FrameReader<ChildStdout>,
    /// `None` once the write half has been closed. Closing it is what tells the
    /// worker the exchange is over, so it is a state the session really has
    /// rather than something to fake with a dead handle.
    writer: Option<FrameWriter<ChildStdin>>,
    ack: HelloAck,
}

impl Session {
    fn open(transport: &dyn Transport, deadlines: Deadlines) -> Result<Self, ClientError> {
        let what = transport.describe();
        let mut channel = transport.connect()?;
        let Some((stdout, stdin)) = channel.take_io() else {
            // Only reachable if a Channel were handed out twice, which
            // `take_io` prevents; treated as a transport fault rather than a
            // panic because a CLI should not abort on an impossible state.
            channel.abandon();
            return Err(ClientError::Closed {
                what,
                stderr: "the channel had already been taken".into(),
            });
        };

        let limits = Limits::control();
        let mut reader = FrameReader::new(stdout, limits);
        let mut writer = FrameWriter::new(stdin, limits);

        let hello = Hello {
            protocol: PROTOCOL_VERSION,
            h5i_version: env!("CARGO_PKG_VERSION").to_string(),
        };
        // A write failure here is nearly always the peer having already exited
        // (ssh refusing to authenticate, or no h5i on the far side) so the
        // stderr tail is the whole diagnosis and a bare EPIPE is none of it.
        if writer.write(FrameKind::Hello.as_u8(), &proto::encode(&hello)?).is_err() {
            let stderr = channel.stderr_tail();
            channel.abandon();
            return Err(ClientError::Closed { what, stderr });
        }

        let ack: HelloAck = match read_message(
            &mut reader,
            FrameKind::HelloAck,
            "HELLO_ACK",
            &what,
            &channel,
        ) {
            Ok(ack) => ack,
            Err(e) => {
                channel.abandon();
                return Err(e);
            }
        };

        if let Err(e) = proto::agreed_protocol(PROTOCOL_VERSION, ack.protocol) {
            channel.abandon();
            return Err(e.into());
        }

        // The peer is there and speaks this protocol. Start the request clock.
        channel.rearm(deadlines.control);

        Ok(Self {
            what,
            channel: Some(channel),
            reader,
            writer: Some(writer),
            ack,
        })
    }

    fn send<T: serde::Serialize>(&mut self, kind: FrameKind, value: &T) -> Result<(), ClientError> {
        let payload = proto::encode(value)?;
        let writer = self.writer.as_mut().ok_or_else(|| ClientError::Closed {
            what: self.what.clone(),
            stderr: String::new(),
        })?;
        writer.write(kind.as_u8(), &payload)?;
        Ok(())
    }

    /// Stream a file out as `DATA` frames, then `DATA_DONE`.
    ///
    /// Chunked well under the frame cap so that one read is one frame and no
    /// buffer has to be resized; the receiver's budget is what actually bounds
    /// this, and it is checked on every chunk rather than at the end.
    fn send_file(&mut self, path: &std::path::Path) -> Result<(), ClientError> {
        use std::io::Read as _;
        // Comfortably under the frame cap, so a chunk plus its type byte is
        // never a frame the other side refuses. Equal to the cap would be an
        // off-by-one; a quarter of it leaves no room for one.
        const CHUNK: usize = 256 * 1024;

        let mut file = std::fs::File::open(path).map_err(|source| {
            ClientError::Source(crate::source::SourceError::Io {
                path: path.to_path_buf(),
                source,
            })
        })?;
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = file.read(&mut buf).map_err(|source| {
                ClientError::Source(crate::source::SourceError::Io {
                    path: path.to_path_buf(),
                    source,
                })
            })?;
            if n == 0 {
                break;
            }
            let writer = self.writer.as_mut().ok_or_else(|| ClientError::Closed {
                what: self.what.clone(),
                stderr: String::new(),
            })?;
            writer.write(FrameKind::Data.as_u8(), &buf[..n])?;
        }
        let writer = self.writer.as_mut().ok_or_else(|| ClientError::Closed {
            what: self.what.clone(),
            stderr: String::new(),
        })?;
        writer.write(FrameKind::DataDone.as_u8(), b"")?;
        Ok(())
    }

    fn expect_message<T: for<'de> serde::Deserialize<'de>>(
        &mut self,
        kind: FrameKind,
        name: &'static str,
    ) -> Result<T, ClientError> {
        let channel = self.channel.as_ref().expect("open session");
        read_message(&mut self.reader, kind, name, &self.what, channel)
    }

    /// Give this session's remaining work a new clock.
    fn rearm(&mut self, after: std::time::Duration) {
        if let Some(channel) = self.channel.as_mut() {
            channel.rearm(after);
        }
    }

    /// Read output frames until `EXIT`.
    ///
    /// Anything else in the stream is a protocol error rather than something to
    /// skip: a frame we were not expecting means we no longer know what the
    /// peer thinks it is doing.
    fn collect_until_exit(
        &mut self,
        stdout: &mut Vec<u8>,
        stderr: &mut Vec<u8>,
    ) -> Result<ExitMsg, ClientError> {
        loop {
            let frame = match self.reader.read() {
                Ok(Some(f)) => f,
                Ok(None) | Err(crate::wire::WireError::Io(_)) => {
                    let channel = self.channel.as_ref();
                    if channel.is_some_and(|c| c.timed_out()) {
                        return Err(ClientError::TimedOut {
                            what: self.what.clone(),
                        });
                    }
                    return Err(ClientError::Closed {
                        what: self.what.clone(),
                        stderr: channel.map(|c| c.stderr_tail()).unwrap_or_default(),
                    });
                }
                Err(e) => return Err(e.into()),
            };
            match FrameKind::from_u8(frame.kind) {
                Some(FrameKind::Stdout) => stdout.extend_from_slice(&frame.payload),
                Some(FrameKind::Stderr) => stderr.extend_from_slice(&frame.payload),
                Some(FrameKind::KeepAlive) => continue,
                Some(FrameKind::Exit) => {
                    let exit: ExitMsg = proto::decode("EXIT", &frame.payload)?;
                    return exit.sanitized().map_err(Into::into);
                }
                Some(FrameKind::Error) => {
                    let msg: ErrorMsg = proto::decode("ERROR", &frame.payload)?;
                    let msg = msg.sanitized();
                    return Err(ProtoError::Refused {
                        code: msg.code,
                        message: msg.message,
                        log_tail: msg.log_tail,
                    }
                    .into());
                }
                Some(other) => {
                    return Err(ProtoError::Unexpected {
                        expected: "STDOUT, STDERR or EXIT",
                        got: other.as_str(),
                    }
                    .into());
                }
                None => return Err(ProtoError::UnknownFrame(frame.kind).into()),
            }
        }
    }

    /// Start a fresh budget for a bulk transfer on this session.
    fn begin_transfer(&mut self) {
        self.reader
            .begin_rpc(Limits::bulk(crate::proto::MAX_SOURCE_BYTES));
    }

    /// Read `DATA` frames into a receiver until `DATA_DONE`.
    fn drain_data(&mut self, rx: &mut crate::source::Receiver) -> Result<(), ClientError> {
        loop {
            let frame = match self.reader.read() {
                Ok(Some(f)) => f,
                Ok(None) | Err(WireError::Io(_)) => {
                    let channel = self.channel.as_ref();
                    if channel.is_some_and(|c| c.timed_out()) {
                        return Err(ClientError::TimedOut {
                            what: self.what.clone(),
                        });
                    }
                    return Err(ClientError::Closed {
                        what: self.what.clone(),
                        stderr: channel.map(|c| c.stderr_tail()).unwrap_or_default(),
                    });
                }
                Err(e) => return Err(e.into()),
            };
            match FrameKind::from_u8(frame.kind) {
                Some(FrameKind::Data) => rx.chunk(&frame.payload)?,
                Some(FrameKind::DataDone) => return Ok(()),
                Some(FrameKind::KeepAlive) => continue,
                Some(FrameKind::Error) => {
                    let msg: ErrorMsg = proto::decode("ERROR", &frame.payload)?;
                    let msg = msg.sanitized();
                    return Err(ProtoError::Refused {
                        code: msg.code,
                        message: msg.message,
                        log_tail: msg.log_tail,
                    }
                    .into());
                }
                Some(other) => {
                    return Err(ProtoError::Unexpected {
                        expected: "DATA or DATA_DONE",
                        got: other.as_str(),
                    }
                    .into());
                }
                None => return Err(ProtoError::UnknownFrame(frame.kind).into()),
            }
        }
    }

    /// Close the write half so the worker sees end of input, then collect its
    /// exit status.
    ///
    /// The order is load-bearing: without dropping the writer first, `finish`
    /// waits for a peer that is waiting for us.
    fn close(&mut self) -> Result<(), ClientError> {
        let Some(channel) = self.channel.take() else {
            return Ok(());
        };
        drop(self.writer.take());
        channel.finish()?;
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // An early return anywhere above must not leave a child running.
        if let Some(channel) = self.channel.take() {
            channel.abandon();
        }
    }
}

/// Read frames until the expected one arrives, skipping keepalives and turning
/// a peer's `ERROR` into this side's error.
fn read_message<T: for<'de> serde::Deserialize<'de>, R: Read>(
    reader: &mut FrameReader<R>,
    want: FrameKind,
    name: &'static str,
    what: &str,
    channel: &Channel,
) -> Result<T, ClientError> {
    loop {
        let frame = match reader.read() {
            Ok(Some(f)) => f,
            Ok(None) | Err(WireError::Io(_)) => {
                // A dead child is the common case here, and the client cannot
                // tell "it never started" from "it exited" from the pipe alone.
                // The two clocks and the stderr tail are what make the
                // difference legible.
                if channel.timed_out() {
                    return Err(ClientError::TimedOut {
                        what: what.to_string(),
                    });
                }
                return Err(ClientError::Closed {
                    what: what.to_string(),
                    stderr: channel.stderr_tail(),
                });
            }
            Err(e) => return Err(e.into()),
        };

        let Some(kind) = FrameKind::from_u8(frame.kind) else {
            return Err(ProtoError::UnknownFrame(frame.kind).into());
        };

        match kind {
            FrameKind::KeepAlive => continue,
            FrameKind::Error => {
                let msg: ErrorMsg = proto::decode("ERROR", &frame.payload)?;
                let msg = msg.sanitized();
                return Err(ProtoError::Refused {
                    code: msg.code,
                    message: msg.message,
                    log_tail: msg.log_tail,
                }
                .into());
            }
            k if k == want => return Ok(proto::decode(name, &frame.payload)?),
            other => {
                return Err(ProtoError::Unexpected {
                    expected: name,
                    got: other.as_str(),
                }
                .into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::ChildProcessTransport;
    use std::time::Duration;

    /// A transport that runs a shell script instead of a worker, so a test can
    /// make the far side behave in ways a real worker never would.
    fn scripted(script: &str) -> ChildProcessTransport {
        ChildProcessTransport {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            env: vec![],
            deadlines: Deadlines {
                handshake: Duration::from_secs(5),
                control: Duration::from_secs(5),
            },
        }
    }

    #[test]
    fn a_peer_that_says_nothing_reports_what_it_printed_to_stderr() {
        // The SSH failure shape: the channel opens, the far side writes a
        // diagnosis to stderr and exits. Nothing about that is a protocol
        // event, and the stderr tail is the entire diagnosis.
        let client = Client::new(Box::new(scripted(
            "echo 'Permission denied (publickey).' >&2; exit 255",
        )));
        match client.hello() {
            Err(ClientError::Closed { stderr, .. }) => {
                assert!(
                    stderr.contains("Permission denied"),
                    "stderr was {stderr:?}"
                );
            }
            other => panic!("expected Closed, got {other:?}"),
        }
    }

    #[test]
    fn a_peer_that_never_answers_hits_the_handshake_clock() {
        // `exec`: a surviving shell would hold the pipe open past the kill.
        let client = Client::new(Box::new(scripted("exec sleep 60"))).with_deadlines(Deadlines {
            handshake: Duration::from_millis(300),
            control: Duration::from_secs(30),
        });
        match client.hello() {
            Err(ClientError::TimedOut { .. }) => {}
            other => panic!("expected TimedOut, got {other:?}"),
        }
    }

    #[test]
    fn a_peer_that_answers_junk_is_a_framing_failure_not_a_hang() {
        let client = Client::new(Box::new(scripted("printf 'not a frame at all'; sleep 0.1")));
        assert!(client.hello().is_err());
    }

    #[test]
    fn the_real_worker_answers_a_handshake_and_a_probe() {
        // Driving `serve` through two in-memory buffers is the unit test; this
        // is the same protocol over a real process boundary, which is what the
        // child-process transport exists for.
        let program = std::env::var("CARGO_BIN_EXE_h5i").ok();
        if program.is_none() {
            // The binary is only built for the root crate's own test targets.
            // The equivalent end-to-end test lives there; nothing to do here.
            return;
        }
        let t = ChildProcessTransport::serve_stdio(program.unwrap());
        let client = Client::new(Box::new(t));
        let probed = client.probe().expect("probe");
        assert_eq!(probed.ack.protocol, PROTOCOL_VERSION);
        assert_eq!(probed.capabilities.os, std::env::consts::OS);
    }
}
