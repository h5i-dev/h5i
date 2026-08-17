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
    self, Capabilities, ErrorMsg, FrameKind, Hello, HelloAck, PROTOCOL_VERSION, ProtoError,
};
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
}

fn stderr_tail(stderr: &str) -> String {
    let t = stderr.trim();
    if t.is_empty() {
        String::new()
    } else {
        format!(":\n{t}")
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
    /// fails validation is an error rather than a stored value — R13.1's
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
        // — ssh refusing to authenticate, or no h5i on the far side — so the
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

    fn expect_message<T: for<'de> serde::Deserialize<'de>>(
        &mut self,
        kind: FrameKind,
        name: &'static str,
    ) -> Result<T, ClientError> {
        let channel = self.channel.as_ref().expect("open session");
        read_message(&mut self.reader, kind, name, &self.what, channel)
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
