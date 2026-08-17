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

use chrono::Utc;

use crate::boxstore::{BoxRecord, BoxStore, StoreError};
use crate::host;
use crate::proto::{
    self, Capabilities, CreateRequest, CreateResult, DestroyRequest, DestroyResult,
    ErrorCode, ErrorMsg, FrameKind, GcRequest, GcResult, Hello, HelloAck, ListResult,
    MAX_SOURCE_BYTES, PROTOCOL_VERSION, ProtoError, SourceKind,
};
use crate::source::{self, SourceError};
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
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Source(#[from] SourceError),
}

/// The budget a source transfer runs under.
///
/// Its own, not the control session's: a bundle is megabytes and a handshake is
/// bytes, and one budget covering both would have to be as loose as the looser
/// of the two. [`FrameReader::begin_rpc`] is what makes it per-RPC.
fn transfer_limits() -> Limits {
    Limits::permissive().narrowed(
        crate::wire::MAX_FRAME,
        MAX_SOURCE_BYTES,
        // A frame carries up to a megabyte, so this bounds the count at roughly
        // twice what the byte budget alone allows — a peer sending a million
        // empty frames is stopped by this rather than by the byte total.
        (MAX_SOURCE_BYTES / (64 * 1024)) + 1024,
    )
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
    /// Whether this invocation has already swept.
    ///
    /// The reaper is whoever turns up next (ROADMAP.md R11): there is no daemon
    /// to watch a clock, so every invocation sweeps before doing its own work.
    /// Once per session, not once per frame.
    swept: bool,
}

impl Worker {
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        Self {
            state_dir: state_dir.into(),
            // A default so the library is usable alone; the binary overrides it
            // with its own version, which is the one that means anything.
            version: env!("CARGO_PKG_VERSION").to_string(),
            capabilities: None,
            swept: false,
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

    /// Every box on this runner.
    pub fn store(&self) -> BoxStore {
        BoxStore::new(self.state_dir.join("boxes"))
    }

    /// Reap what the leases say is over, once per session.
    ///
    /// Failures here are reported and swallowed: a sweep that cannot remove one
    /// box must not take down the request that happened to arrive next. What it
    /// could not do is on stderr, which the client captures.
    fn sweep_once(&mut self) {
        if self.swept {
            return;
        }
        self.swept = true;
        let store = self.store();
        let now = Utc::now();
        match store.reapable(now, false) {
            Ok(items) => {
                for item in items {
                    if let Some(name) = &item.container {
                        stop_container(name);
                    }
                    if let Err(e) = store.reap(&item) {
                        eprintln!("h5i runner: could not reap `{}`: {e}", item.box_id);
                    }
                }
            }
            Err(e) => eprintln!("h5i runner: could not read box state to sweep: {e}"),
        }
    }
}

/// Stop and remove a container, best effort.
///
/// Best effort on purpose: this runs on the reap path, and a container that
/// cannot be stopped must not stop the box record from being removed. The
/// alternative is a runner that accumulates records it will never clean up
/// because one runtime call keeps failing.
fn stop_container(name: &str) {
    let _ = std::process::Command::new("podman")
        .args(["rm", "-f", "--ignore", name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
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
    /// A framing failure partway through an RPC. Nothing is written back: we
    /// are out of step with the stream, so any bytes we sent would be read at
    /// an offset the peer does not expect.
    Wire(WireError),
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

            (Some(_), FrameKind::CreateBox) => {
                worker.sweep_once();
                match handle_create(&frame.payload, worker, &mut frames) {
                    Ok(result) => {
                        write_msg(&mut out, FrameKind::CreateResult, &result)?;
                        continue;
                    }
                    Err(d) => Err(d),
                }
            }

            (Some(_), FrameKind::DestroyBox) => {
                worker.sweep_once();
                match handle_destroy(&frame.payload, worker) {
                    Ok(result) => {
                        write_msg(&mut out, FrameKind::DestroyResult, &result)?;
                        continue;
                    }
                    Err(d) => Err(d),
                }
            }

            (Some(_), FrameKind::ListBoxes) => {
                worker.sweep_once();
                match worker.store().list(Utc::now()) {
                    Ok(boxes) => {
                        write_msg(&mut out, FrameKind::BoxList, &ListResult { boxes })?;
                        continue;
                    }
                    Err(e) => Err(Disposition::Answer(ErrorMsg::new(
                        ErrorCode::Internal,
                        e.to_string(),
                    ))),
                }
            }

            (Some(_), FrameKind::Gc) => match handle_gc(&frame.payload, worker) {
                Ok(result) => {
                    write_msg(&mut out, FrameKind::GcResult, &result)?;
                    continue;
                }
                Err(d) => Err(d),
            },

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
            Err(Disposition::Wire(e)) => {
                eprintln!("h5i runner: {e}");
                return Err(e.into());
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


/// Make a box, or hand back the one that is already there.
///
/// The order is the whole design (ROADMAP.md R7):
///
/// 1. Validate the request, so nothing peer-supplied becomes a path first.
/// 2. Check the tier against what this runner advertises — a capability it
///    lacks is a refusal naming the capability, never a quiet downgrade (R1).
/// 3. Decide the idempotent case *before* building anything: a matching
///    request digest returns the existing box, a differing one is a conflict.
/// 4. Rebuild the policy locally and check its digest. The client sent the
///    three fields a `ResolvedPolicy` serialises; every host path in it is
///    deliberately not among them, so this side reconstructs its own and the
///    digests still have to agree.
/// 5. Build under `creating/<operation_id>`, and rename. The rename is the
///    instant the box exists.
fn handle_create<R: Read>(
    payload: &[u8],
    worker: &mut Worker,
    frames: &mut FrameReader<R>,
) -> Result<CreateResult, Disposition> {
    let req: CreateRequest = proto::decode("CREATE_BOX", payload).map_err(fatal)?;
    req.validated().map_err(fatal)?;

    // What this runner will actually run. Asked before any work, so a refusal
    // costs nothing and names the reason.
    let caps = worker.capabilities().clone();
    if !caps.isolation.iter().any(|t| t == &req.isolation) {
        return Err(Disposition::Answer(ErrorMsg::new(
            ErrorCode::Unsupported,
            format!(
                "this runner does not offer the `{}` tier — it offers {}. \
                 A capability it lacks is a refusal, not a weaker box.",
                req.isolation,
                if caps.isolation.is_empty() {
                    "none".to_string()
                } else {
                    caps.isolation.join(", ")
                }
            ),
        )));
    }

    let store = worker.store();

    // The idempotent case, decided before anything is built.
    if let Some(existing) = store.find(&req.box_id).map_err(internal)? {
        if existing.request_digest == req.request_digest {
            let lease = store.lease(&req.box_id).map_err(internal)?;
            store.touch(&req.box_id, Utc::now()).map_err(internal)?;
            return Ok(CreateResult {
                box_id: existing.box_id.clone(),
                existing: true,
                policy_digest: existing.policy_digest.clone(),
                workspace: store.box_dir(&existing.box_id).join("work").display().to_string(),
                container: existing.container.clone(),
                lease_expires_at: lease
                    .map(|l| l.expires_at())
                    .unwrap_or_else(Utc::now),
            });
        }
        return Err(Disposition::Answer(ErrorMsg::new(
            ErrorCode::Unsupported,
            format!(
                "box `{}` already exists on this runner and was built from a different \
                 request — destroy it first, or use another name",
                req.box_id
            ),
        )));
    }

    // The policy, rebuilt here rather than trusted as sent. Only three fields
    // of a `ResolvedPolicy` serialise, and none of them is a host path, so a
    // digest computed on this side has to equal the one computed on that side.
    let policy_digest = policy_digest_of(&req.policy).map_err(|e| {
        Disposition::Answer(ErrorMsg::new(
            ErrorCode::Malformed,
            format!("the policy in this create is not one h5i can read: {e}"),
        ))
    })?;
    if policy_digest != req.policy_digest {
        return Err(Disposition::Answer(ErrorMsg::new(
            ErrorCode::Malformed,
            format!(
                "the policy digest does not match the policy: the request says \
                 {} and the policy hashes to {}",
                req.policy_digest, policy_digest
            ),
        )));
    }

    let creating = store.begin(&req.operation_id).map_err(internal)?;
    let work = creating.work_dir();

    // Build, and on any failure take the attempt's directory with us: a
    // half-built box wearing a real name is the state this design exists to
    // make impossible.
    let built = (|| -> Result<(), Disposition> {
        match req.source.kind {
            SourceKind::GitBundle => {
                let bundle = creating.dir().join("source.bundle");
                receive_source(frames, &req, bundle.clone())?;
                let base = req.source.base_commit.as_deref().unwrap_or_default();
                source::materialize(&bundle, &work, base).map_err(source_failed)?;
                // The bundle has done its job; keeping it doubles the box's
                // disk for nothing.
                let _ = std::fs::remove_file(&bundle);
            }
            SourceKind::Empty => {
                source::materialize_empty(&work).map_err(source_failed)?;
            }
        }
        Ok(())
    })();

    if let Err(d) = built {
        let _ = std::fs::remove_dir_all(creating.dir());
        return Err(d);
    }

    let now = Utc::now();
    let record = BoxRecord {
        box_id: req.box_id.clone(),
        operation_id: req.operation_id.clone(),
        request_digest: req.request_digest.clone(),
        isolation: req.isolation.clone(),
        image: req.image.clone(),
        limits: req.limits,
        policy_digest: policy_digest.clone(),
        // No container yet, and that is deliberate rather than unfinished: the
        // container tier has no warm form (every command is `podman run --rm`),
        // so a container is made when there is something to run in it. See the
        // note in R13.2.
        container: None,
        source_kind: req.source.kind,
        base_commit: req.source.base_commit.clone(),
        created_at: now,
    };

    let record = store
        .commit(creating, &record, req.lease, &req.policy, now)
        .map_err(|e| match e {
            StoreError::Conflict { box_id } => Disposition::Answer(ErrorMsg::new(
                ErrorCode::Unsupported,
                format!("box `{box_id}` was created by something else while this one built"),
            )),
            other => internal(other),
        })?;

    let lease = store.lease(&record.box_id).map_err(internal)?;
    Ok(CreateResult {
        box_id: record.box_id.clone(),
        existing: false,
        policy_digest,
        workspace: store.box_dir(&record.box_id).join("work").display().to_string(),
        container: None,
        lease_expires_at: lease.map(|l| l.expires_at()).unwrap_or(now),
    })
}

/// Read `DATA` frames until `DATA_DONE`, into a file that is verified before
/// anything unpacks it.
fn receive_source<R: Read>(
    frames: &mut FrameReader<R>,
    req: &CreateRequest,
    path: std::path::PathBuf,
) -> Result<(), Disposition> {
    // Its own budget: a bundle is megabytes and the handshake that preceded it
    // was bytes.
    frames.begin_rpc(transfer_limits());

    let mut rx = source::Receiver::new(path, req.source.bytes, MAX_SOURCE_BYTES)
        .map_err(source_failed)?;

    loop {
        let frame = match frames.read() {
            Ok(Some(f)) => f,
            Ok(None) => {
                return Err(fatal(ProtoError::Invalid(
                    "the source transfer ended before DATA_DONE".into(),
                )));
            }
            Err(e) => return Err(Disposition::Wire(e)),
        };
        match FrameKind::from_u8(frame.kind) {
            Some(FrameKind::Data) => rx.chunk(&frame.payload).map_err(source_failed)?,
            Some(FrameKind::DataDone) => break,
            Some(FrameKind::KeepAlive) => continue,
            Some(other) => {
                return Err(fatal(ProtoError::Unexpected {
                    expected: "DATA or DATA_DONE",
                    got: other.as_str(),
                }));
            }
            None => return Err(fatal(ProtoError::UnknownFrame(frame.kind))),
        }
    }

    rx.finish(&req.source.sha256).map_err(source_failed)?;
    Ok(())
}

fn handle_destroy(payload: &[u8], worker: &mut Worker) -> Result<DestroyResult, Disposition> {
    let req: DestroyRequest = proto::decode("DESTROY_BOX", payload).map_err(fatal)?;
    req.validated().map_err(fatal)?;

    let store = worker.store();
    let record = store.find(&req.box_id).map_err(internal)?;
    if let Some(name) = record.as_ref().and_then(|r| r.container.as_deref()) {
        stop_container(name);
    }
    let existed = store.remove(&req.box_id).map_err(internal)?;
    Ok(DestroyResult {
        box_id: req.box_id,
        existed,
        warnings: Vec::new(),
    })
}

fn handle_gc(payload: &[u8], worker: &mut Worker) -> Result<GcResult, Disposition> {
    // An absent payload is the default request, so `GC` with no body works.
    let req: GcRequest = if payload.is_empty() {
        GcRequest::default()
    } else {
        proto::decode("GC", payload).map_err(fatal)?
    };

    let store = worker.store();
    let now = Utc::now();
    let items = store.reapable(now, req.all).map_err(internal)?;

    let mut reaped = Vec::new();
    let mut kept = Vec::new();
    for item in items {
        if let Some(name) = &item.container {
            stop_container(name);
        }
        match store.reap(&item) {
            Ok(()) => reaped.push(item.box_id),
            // Said rather than skipped: a silent failure reads as "there was
            // nothing to do", and an un-reaped box beats an unrecoverable one
            // only if somebody is told (R11).
            Err(e) => kept.push(format!("{}: {e}", item.box_id)),
        }
    }
    Ok(GcResult { reaped, kept })
}

/// The digest a `ResolvedPolicy` would have, computed from what arrived.
fn policy_digest_of(policy: &serde_json::Value) -> Result<String, h5i_error::H5iError> {
    let resolved: h5i_sandbox::sandbox_policy::ResolvedPolicy =
        serde_json::from_value(policy.clone())
            .map_err(|e| h5i_error::H5iError::Metadata(e.to_string()))?;
    resolved.digest()
}

fn fatal(e: ProtoError) -> Disposition {
    Disposition::Fatal(ErrorMsg::new(e.code(), e.to_string()))
}

fn internal<E: std::fmt::Display>(e: E) -> Disposition {
    Disposition::Answer(ErrorMsg::new(ErrorCode::Internal, e.to_string()))
}

/// A source failure carries what the runner saw, because a transfer that fails
/// on another machine is otherwise undiagnosable from here.
fn source_failed(e: SourceError) -> Disposition {
    Disposition::Answer(ErrorMsg::new(ErrorCode::Internal, e.to_string()))
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
        // Deliberately a verb from a later milestone: CREATE_BOX is built now,
        // so using it here would test the wrong thing the day it landed.
        let (frames, result) = session(&[
            hello(),
            (FrameKind::Exec, b"{}".to_vec()),
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

    /// A real resolved policy and its real digest, so the digest check under
    /// test is the product's own rather than a stand-in.
    fn policy_and_digest() -> (serde_json::Value, String) {
        use h5i_sandbox::sandbox_policy::{IsolationClaim, Profile, ResolvedPolicy};
        let profile = Profile::builtin("probe", IsolationClaim::Container);
        let resolved = ResolvedPolicy::new(IsolationClaim::Container, profile);
        let digest = resolved.digest().expect("digest");
        (serde_json::to_value(&resolved).expect("json"), digest)
    }

    fn create_request(box_id: &str, op: &str, digest: &str) -> CreateRequest {
        let (policy, policy_digest) = policy_and_digest();
        CreateRequest {
            box_id: box_id.into(),
            operation_id: op.into(),
            request_digest: digest.into(),
            isolation: "container".into(),
            image: Some("debian".into()),
            limits: Default::default(),
            lease: Default::default(),
            policy_digest,
            policy,
            source: crate::proto::SourceSpec {
                kind: SourceKind::Empty,
                bytes: 0,
                sha256: String::new(),
                base_commit: None,
            },
        }
    }

    /// Drive a session against a worker whose state dir we keep, so a test can
    /// look at what landed on disk afterwards.
    fn session_in(
        dir: &std::path::Path,
        script: &[(FrameKind, Vec<u8>)],
    ) -> (Vec<Frame>, Result<(), ServeError>) {
        let mut input = Vec::new();
        {
            let mut w = FrameWriter::new(&mut input, Limits::permissive());
            for (kind, payload) in script {
                w.write(kind.as_u8(), payload).expect("script frame");
            }
        }
        let mut worker = Worker::new(dir);
        // Advertise the container tier regardless of what this host has, so the
        // capability gate under test is the gate and not the host's podman.
        worker.capabilities = Some(Capabilities {
            arch: "x86_64".into(),
            os: "linux".into(),
            memory_mb: 1024,
            workspace_mb: 1024,
            isolation: vec!["container".into()],
            container: true,
            kvm: false,
            persistent_boxes: true,
            own_egress: true,
            notes: vec![],
        });
        let mut output = Vec::new();
        let result = serve(input.as_slice(), &mut output, &mut worker);
        let mut frames = Vec::new();
        let mut r = FrameReader::new(output.as_slice(), Limits::permissive());
        while let Ok(Some(f)) = r.read() {
            frames.push(f);
        }
        (frames, result)
    }

    fn msg<T: serde::Serialize>(kind: FrameKind, v: &T) -> (FrameKind, Vec<u8>) {
        (kind, proto::encode(v).unwrap())
    }

    #[test]
    fn a_create_makes_a_box_and_a_second_identical_one_returns_it() {
        // The idempotent case (R7): a lost CREATE_RESULT costs a retry, not a
        // duplicate box.
        let dir = tempfile::tempdir().unwrap();
        let req = create_request("demo", "op1", &"a".repeat(64));

        let (frames, result) = session_in(
            dir.path(),
            &[hello(), msg(FrameKind::CreateBox, &req)],
        );
        assert!(result.is_ok(), "{result:?}");
        let created: CreateResult =
            proto::decode("CREATE_RESULT", &frames[1].payload).expect("result");
        assert!(!created.existing);
        assert_eq!(created.box_id, "demo");
        assert_eq!(created.policy_digest, req.policy_digest);

        // Same request again: the same box, named as already there.
        let (frames, result) = session_in(
            dir.path(),
            &[hello(), msg(FrameKind::CreateBox, &req)],
        );
        assert!(result.is_ok(), "{result:?}");
        let again: CreateResult =
            proto::decode("CREATE_RESULT", &frames[1].payload).expect("result");
        assert!(again.existing, "a re-send returns the existing box");
        assert_eq!(again.workspace, created.workspace);
    }

    #[test]
    fn a_different_request_under_a_taken_name_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let first = create_request("demo", "op1", &"a".repeat(64));
        let _ = session_in(dir.path(), &[hello(), msg(FrameKind::CreateBox, &first)]);

        let second = create_request("demo", "op2", &"b".repeat(64));
        let (frames, _) = session_in(dir.path(), &[hello(), msg(FrameKind::CreateBox, &second)]);
        let err = error_of(&frames[1]);
        assert_eq!(err.code, ErrorCode::Unsupported);
        assert!(err.message.contains("different request"), "{}", err.message);
    }

    #[test]
    fn a_tier_the_runner_does_not_advertise_is_refused_with_the_capability_named() {
        // R1's rule, at the point it actually bites.
        let dir = tempfile::tempdir().unwrap();
        let mut req = create_request("demo", "op1", &"a".repeat(64));
        req.isolation = "supervised".into();
        let (frames, _) = session_in(dir.path(), &[hello(), msg(FrameKind::CreateBox, &req)]);
        let err = error_of(&frames[1]);
        assert_eq!(err.code, ErrorCode::Unsupported);
        assert!(err.message.contains("supervised"), "{}", err.message);
        assert!(err.message.contains("container"), "and what it does offer");
    }

    #[test]
    fn a_policy_that_does_not_match_its_digest_is_refused() {
        // The check that turns "the worker silently enforced an older policy"
        // into a detected fault. Here the lie is in the other direction, which
        // is the same check.
        let dir = tempfile::tempdir().unwrap();
        let mut req = create_request("demo", "op1", &"a".repeat(64));
        req.policy_digest = "f".repeat(64);
        let (frames, _) = session_in(dir.path(), &[hello(), msg(FrameKind::CreateBox, &req)]);
        let err = error_of(&frames[1]);
        assert_eq!(err.code, ErrorCode::Malformed);
        assert!(err.message.contains("policy digest"), "{}", err.message);
    }

    #[test]
    fn a_create_that_fails_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let mut req = create_request("demo", "op1", &"a".repeat(64));
        req.policy_digest = "f".repeat(64);
        let _ = session_in(dir.path(), &[hello(), msg(FrameKind::CreateBox, &req)]);

        let store = crate::boxstore::BoxStore::new(dir.path().join("boxes"));
        assert!(store.list(Utc::now()).unwrap().is_empty(), "no box, no attempt");
    }

    #[test]
    fn destroy_removes_it_and_saying_so_twice_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let req = create_request("demo", "op1", &"a".repeat(64));
        let _ = session_in(dir.path(), &[hello(), msg(FrameKind::CreateBox, &req)]);

        let destroy = DestroyRequest {
            box_id: "demo".into(),
            force: false,
        };
        let (frames, _) = session_in(dir.path(), &[hello(), msg(FrameKind::DestroyBox, &destroy)]);
        let d: DestroyResult = proto::decode("DESTROY_RESULT", &frames[1].payload).unwrap();
        assert!(d.existed);

        // A retry after a lost answer must not fail: gone is the state asked for.
        let (frames, _) = session_in(dir.path(), &[hello(), msg(FrameKind::DestroyBox, &destroy)]);
        let d: DestroyResult = proto::decode("DESTROY_RESULT", &frames[1].payload).unwrap();
        assert!(!d.existed);
    }

    #[test]
    fn listing_shows_what_was_made() {
        let dir = tempfile::tempdir().unwrap();
        for (id, op, digest) in [("one", "op1", "1"), ("two", "op2", "2")] {
            let req = create_request(id, op, &digest.repeat(64));
            let _ = session_in(dir.path(), &[hello(), msg(FrameKind::CreateBox, &req)]);
        }
        let (frames, _) = session_in(dir.path(), &[hello(), (FrameKind::ListBoxes, vec![])]);
        let list: ListResult = proto::decode("BOX_LIST", &frames[1].payload).unwrap();
        assert_eq!(list.boxes.len(), 2);
        assert_eq!(list.boxes[0].box_id, "one");
        assert_eq!(list.boxes[1].box_id, "two");
        assert_eq!(list.boxes[0].isolation, "container");
    }

    #[test]
    fn gc_all_takes_live_boxes_and_gc_alone_leaves_them() {
        let dir = tempfile::tempdir().unwrap();
        let req = create_request("demo", "op1", &"a".repeat(64));
        let _ = session_in(dir.path(), &[hello(), msg(FrameKind::CreateBox, &req)]);

        let (frames, _) = session_in(
            dir.path(),
            &[hello(), msg(FrameKind::Gc, &GcRequest { all: false })],
        );
        let gc: GcResult = proto::decode("GC_RESULT", &frames[1].payload).unwrap();
        assert!(gc.reaped.is_empty(), "a live lease is not swept");

        let (frames, _) = session_in(
            dir.path(),
            &[hello(), msg(FrameKind::Gc, &GcRequest { all: true })],
        );
        let gc: GcResult = proto::decode("GC_RESULT", &frames[1].payload).unwrap();
        assert_eq!(gc.reaped, vec!["demo"]);
    }

    #[test]
    fn a_gc_with_no_payload_is_the_default_request() {
        let dir = tempfile::tempdir().unwrap();
        let (frames, result) = session_in(dir.path(), &[hello(), (FrameKind::Gc, vec![])]);
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(frames[1].kind, FrameKind::GcResult.as_u8());
    }

    #[test]
    fn a_source_transfer_is_verified_against_its_digest() {
        // The bundle is real: built by git, streamed as DATA frames, and
        // materialised on the far side. A digest that does not match must stop
        // it before git ever sees the file.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            vec!["init", "--quiet", "."],
            vec!["config", "user.email", "t@example.com"],
            vec!["config", "user.name", "T"],
        ] {
            std::process::Command::new("git").args(&args).current_dir(&repo).output().unwrap();
        }
        std::fs::write(repo.join("hello.txt"), b"from the host").unwrap();
        for args in [vec!["add", "-A"], vec!["commit", "--quiet", "-m", "one"]] {
            std::process::Command::new("git").args(&args).current_dir(&repo).output().unwrap();
        }
        let head = String::from_utf8(
            std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&repo)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();

        let bundle = crate::source::build_bundle(&repo, &head, &dir.path().join("s.bundle"))
            .expect("bundle");
        let bytes = std::fs::read(&bundle.path).unwrap();

        let mut req = create_request("demo", "op1", &"a".repeat(64));
        req.source = crate::proto::SourceSpec {
            kind: SourceKind::GitBundle,
            bytes: bundle.bytes,
            sha256: bundle.sha256.clone(),
            base_commit: Some(head.clone()),
        };

        // The happy path: the source lands and the box is on that commit.
        let (frames, result) = session_in(
            dir.path().join("state").as_path(),
            &[
                hello(),
                msg(FrameKind::CreateBox, &req),
                (FrameKind::Data, bytes.clone()),
                (FrameKind::DataDone, vec![]),
            ],
        );
        assert!(result.is_ok(), "{result:?}");
        let created: CreateResult =
            proto::decode("CREATE_RESULT", &frames[1].payload).expect("created");
        let work = std::path::Path::new(&created.workspace);
        assert_eq!(
            std::fs::read_to_string(work.join("hello.txt")).unwrap(),
            "from the host",
            "the source really arrived"
        );

        // And a digest that does not match stops it before git sees the file.
        let mut lying = req.clone();
        lying.box_id = "liar".into();
        lying.operation_id = "op2".into();
        lying.source.sha256 = "f".repeat(64);
        let (frames, _) = session_in(
            dir.path().join("state2").as_path(),
            &[
                hello(),
                msg(FrameKind::CreateBox, &lying),
                (FrameKind::Data, bytes),
                (FrameKind::DataDone, vec![]),
            ],
        );
        let err = error_of(&frames[1]);
        assert!(
            err.message.contains("digest"),
            "the refusal names the reason: {}",
            err.message
        );
    }

    #[test]
    fn a_transfer_that_ends_without_data_done_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut req = create_request("demo", "op1", &"a".repeat(64));
        req.source = crate::proto::SourceSpec {
            kind: SourceKind::GitBundle,
            bytes: 100,
            sha256: "a".repeat(64),
            base_commit: Some("deadbeef".into()),
        };
        let (_, result) = session_in(
            dir.path(),
            &[
                hello(),
                msg(FrameKind::CreateBox, &req),
                (FrameKind::Data, vec![0u8; 50]),
            ],
        );
        assert!(result.is_err(), "an unterminated transfer is not a success");
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
