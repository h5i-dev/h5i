//! The runner protocol over a real process boundary.
//!
//! `h5i-runner`'s own tests drive the worker loop through two in-memory
//! buffers, which is where the framing and the state machine are pinned down.
//! This is the other half of R13.1's exit criterion: the same protocol against
//! the **real binary**, spawned as a child, with pipes and an exit status in
//! between — and still no sshd, no second machine and no network, which is what
//! makes it something CI can run.
//!
//! What the child-process transport is *for* is the failure half. A peer that
//! sends an oversized frame, or stops mid-message, or speaks a protocol from
//! the future, is trivial to arrange here and near-impossible to arrange
//! against a real runner on demand.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

use h5i_runner::proto::{
    self, Capabilities, ErrorCode, ErrorMsg, FrameKind, Hello, HelloAck, PROTOCOL_VERSION,
};
use h5i_runner::transport::ChildProcessTransport;
use h5i_runner::wire::{FrameReader, FrameWriter, Limits};
use h5i_runner::{Client, ClientError};

fn h5i() -> &'static str {
    env!("CARGO_BIN_EXE_h5i")
}

fn client() -> Client {
    Client::new(Box::new(ChildProcessTransport::serve_stdio(h5i())))
}

/// Speak to a real worker process by hand, so a test can send bytes no client
/// would ever send. Returns everything the worker wrote back, plus its status.
fn raw_exchange(input: &[u8]) -> (Vec<(u8, Vec<u8>)>, std::process::ExitStatus) {
    let mut child = Command::new(h5i())
        .args(["runner", "serve-stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn h5i runner serve-stdio");

    {
        let mut stdin = child.stdin.take().expect("stdin");
        // The worker may refuse and exit before reading all of this, which
        // makes the write fail with EPIPE. That is the behaviour under test,
        // not a test failure.
        let _ = stdin.write_all(input);
    }

    let mut buf = Vec::new();
    child
        .stdout
        .take()
        .expect("stdout")
        .read_to_end(&mut buf)
        .expect("read stdout");
    let status = child.wait().expect("wait");

    let mut frames = Vec::new();
    let mut r = FrameReader::new(buf.as_slice(), Limits::permissive());
    while let Ok(Some(f)) = r.read() {
        frames.push((f.kind, f.payload));
    }
    (frames, status)
}

fn hello_bytes(protocol: u16) -> Vec<u8> {
    let mut out = Vec::new();
    let h = Hello {
        protocol,
        h5i_version: "integration-test".into(),
    };
    FrameWriter::new(&mut out, Limits::permissive())
        .write(FrameKind::Hello.as_u8(), &proto::encode(&h).unwrap())
        .unwrap();
    out
}

fn error_in(frames: &[(u8, Vec<u8>)], at: usize) -> ErrorMsg {
    assert_eq!(
        frames[at].0,
        FrameKind::Error.as_u8(),
        "frame {at} should be an ERROR"
    );
    proto::decode::<ErrorMsg>("ERROR", &frames[at].1).expect("an error message")
}

#[test]
fn the_binary_answers_a_handshake_and_a_probe() {
    let probed = client().probe().expect("probe the real worker");

    assert_eq!(probed.ack.protocol, PROTOCOL_VERSION);
    assert_eq!(probed.ack.os, std::env::consts::OS);
    assert_eq!(probed.ack.arch, std::env::consts::ARCH);
    assert!(
        !probed.ack.h5i_version.is_empty(),
        "the worker names its own version"
    );

    // The capability report has already been through `sanitized` on the way in;
    // this is the same invariant from the other side of a real pipe.
    let caps = &probed.capabilities;
    assert_eq!(caps.arch, std::env::consts::ARCH);
    if caps.isolation.iter().any(|t| t == "container") {
        assert!(caps.container, "a tier claimed implies its runtime");
    }
}

#[test]
fn the_handshake_carries_no_identity() {
    // Identity is the client's to compute from the pinned host key (R5/R6). A
    // worker that asserted its own would be one a compromised runner could use
    // to impersonate another.
    let ack = client().hello().expect("hello");
    assert!(ack.runner_id_echo.is_none());
}

#[test]
fn a_probe_is_repeatable_and_each_call_is_its_own_process() {
    // One channel is one RPC, and a second probe is a second worker process.
    // If any state leaked between them this is where it would show.
    let c = client();
    let first = c.probe().expect("first");
    let second = c.probe().expect("second");
    assert_eq!(first.capabilities.arch, second.capabilities.arch);
    assert_eq!(first.ack.h5i_version, second.ack.h5i_version);
}

#[test]
fn the_worker_writes_frames_and_nothing_else_to_stdout() {
    // A stray `println!` anywhere in the worker path is not a cosmetic bug: the
    // client would read the text as a length prefix. This asserts the stream is
    // exactly the frames and not one byte more.
    let mut input = hello_bytes(PROTOCOL_VERSION);
    FrameWriter::new(&mut input, Limits::permissive())
        .write(FrameKind::Probe.as_u8(), b"")
        .unwrap();

    let (frames, status) = raw_exchange(&input);
    assert!(status.success(), "clean exchange exits zero");
    assert_eq!(frames.len(), 2, "exactly HELLO_ACK and CAPABILITIES");
    assert_eq!(frames[0].0, FrameKind::HelloAck.as_u8());
    assert_eq!(frames[1].0, FrameKind::Capabilities.as_u8());

    let _: HelloAck = proto::decode("HELLO_ACK", &frames[0].1).expect("parses");
    let _: Capabilities = proto::decode("CAPABILITIES", &frames[1].1).expect("parses");
}

#[test]
fn nothing_may_precede_the_handshake() {
    let mut input = Vec::new();
    FrameWriter::new(&mut input, Limits::permissive())
        .write(FrameKind::Probe.as_u8(), b"")
        .unwrap();

    let (frames, status) = raw_exchange(&input);
    assert_eq!(error_in(&frames, 0).code, ErrorCode::Sequence);
    assert_eq!(frames.len(), 1, "and the session is over");
    assert!(!status.success(), "a refused session is a non-zero exit");
}

#[test]
fn an_unknown_frame_type_is_refused_and_ends_the_session() {
    let mut input = hello_bytes(PROTOCOL_VERSION);
    {
        let mut w = FrameWriter::new(&mut input, Limits::permissive());
        w.write(0xEE, b"from a newer h5i").unwrap();
        w.write(FrameKind::Probe.as_u8(), b"").unwrap();
    }

    let (frames, status) = raw_exchange(&input);
    let err = error_in(&frames, 1);
    assert_eq!(err.code, ErrorCode::UnknownFrame);
    assert!(err.message.contains("0xEE"), "the code belongs in the message");
    assert_eq!(frames.len(), 2, "the frame after it is never processed");
    assert!(!status.success());
}

#[test]
fn an_oversized_declaration_is_refused_before_a_byte_of_it_is_read() {
    // The hostile shape: declare four gigabytes, send none of them. Nothing may
    // be allocated and nothing may block.
    let mut input = hello_bytes(PROTOCOL_VERSION);
    input.extend_from_slice(&u32::MAX.to_be_bytes());

    let (frames, status) = raw_exchange(&input);
    assert_eq!(frames.len(), 1, "only the handshake answer");
    assert!(!status.success());
}

#[test]
fn a_truncated_frame_ends_the_session_without_a_reply() {
    // Out of step with the stream, anything written back would be read at an
    // offset the peer does not expect — so the worker says nothing.
    let mut input = hello_bytes(PROTOCOL_VERSION);
    input.extend_from_slice(&64u32.to_be_bytes());
    input.extend_from_slice(b"only a few bytes, not 64");

    let (frames, status) = raw_exchange(&input);
    assert_eq!(frames.len(), 1, "only the handshake answer");
    assert!(!status.success());
}

#[test]
fn a_zero_length_frame_carries_no_type_byte_and_is_refused() {
    let mut input = hello_bytes(PROTOCOL_VERSION);
    input.extend_from_slice(&0u32.to_be_bytes());

    let (frames, status) = raw_exchange(&input);
    assert_eq!(frames.len(), 1);
    assert!(!status.success());
}

#[test]
fn the_cap_that_applies_is_the_sessions_own_and_both_sides_of_it_behave() {
    // A control session's budget is narrower than the format's ceiling, and it
    // is the narrower one that governs — the receiver's number always does.
    // The boundary is where a cap becomes an off-by-one, so both sides of it
    // are worth pinning.
    let cap = Limits::control().max_frame();

    // Exactly at the cap: legal framing, and a malformed HELLO. The refusal
    // must therefore be about the payload, not about the size.
    let mut input = Vec::new();
    FrameWriter::new(&mut input, Limits::permissive())
        .write(FrameKind::Hello.as_u8(), &vec![b'x'; cap - 1])
        .unwrap();
    let (frames, status) = raw_exchange(&input);
    assert_eq!(
        error_in(&frames, 0).code,
        ErrorCode::Malformed,
        "at the cap the frame is read, and its payload is what is wrong"
    );
    assert!(!status.success());

    // One byte over: refused at the framing layer, before the payload is read,
    // so there is nothing to say back.
    let mut input = Vec::new();
    FrameWriter::new(&mut input, Limits::permissive())
        .write(FrameKind::Hello.as_u8(), &vec![b'x'; cap])
        .unwrap();
    let (frames, status) = raw_exchange(&input);
    assert!(
        frames.is_empty(),
        "over the cap nothing is read and nothing is answered"
    );
    assert!(!status.success());
}

#[test]
fn a_protocol_from_the_future_meets_us_at_ours_and_one_too_old_is_named() {
    // The lower version governs, with no negotiation: a newer client is fine.
    let (frames, status) = raw_exchange(&hello_bytes(u16::MAX));
    assert_eq!(frames[0].0, FrameKind::HelloAck.as_u8());
    assert!(status.success());

    // And a version we cannot meet fails at the handshake, with the numbers in
    // the message — not later, mid-create, as a mysterious unknown frame.
    let (frames, status) = raw_exchange(&hello_bytes(0));
    assert_eq!(error_in(&frames, 0).code, ErrorCode::ProtocolVersion);
    assert!(!status.success());
}

#[test]
fn a_malformed_handshake_payload_is_refused_not_guessed_at() {
    let mut input = Vec::new();
    FrameWriter::new(&mut input, Limits::permissive())
        .write(FrameKind::Hello.as_u8(), b"{ not json at all")
        .unwrap();

    let (frames, _) = raw_exchange(&input);
    assert_eq!(error_in(&frames, 0).code, ErrorCode::Malformed);
}

#[test]
fn an_unbuilt_rpc_is_answered_and_the_channel_survives_it() {
    // "Not yet built" is a fact about this milestone, and a client meeting it
    // should get a sentence rather than a closed pipe.
    let mut input = hello_bytes(PROTOCOL_VERSION);
    {
        let mut w = FrameWriter::new(&mut input, Limits::permissive());
        w.write(FrameKind::CreateBox.as_u8(), b"{}").unwrap();
        w.write(FrameKind::Probe.as_u8(), b"").unwrap();
    }

    let (frames, status) = raw_exchange(&input);
    assert!(status.success(), "a refused RPC is not a failed session");
    assert_eq!(frames.len(), 3);
    let err = error_in(&frames, 1);
    assert_eq!(err.code, ErrorCode::Unimplemented);
    assert!(
        err.message.contains("CREATE_BOX"),
        "the refusal names the verb: {}",
        err.message
    );
    assert_eq!(
        frames[2].0,
        FrameKind::Capabilities.as_u8(),
        "the channel survived the refusal"
    );
}

#[test]
fn a_client_reports_a_peer_that_is_not_a_worker() {
    // What a misconfigured forced command looks like from here: the channel
    // opens, something else runs, and the diagnosis is on stderr rather than in
    // any protocol event.
    let t = ChildProcessTransport {
        program: "/bin/sh".into(),
        args: vec![
            "-c".into(),
            "echo 'h5i: command not found' >&2; exit 127".into(),
        ],
        env: vec![],
        deadlines: Default::default(),
    };
    match Client::new(Box::new(t)).probe() {
        Err(ClientError::Closed { stderr, .. }) => {
            assert!(stderr.contains("command not found"), "stderr was {stderr:?}");
        }
        other => panic!("expected Closed, got {other:?}"),
    }
}

#[test]
fn the_worker_uses_the_state_directory_it_is_pointed_at() {
    // The escape hatch a test needs, and the one a real worker never sets: its
    // storage must be somewhere a probe can measure without touching the
    // developer's own data directory.
    let dir = tempfile::tempdir().expect("tempdir");
    let t = ChildProcessTransport::serve_stdio(h5i())
        .with_env(h5i_runner::serve::STATE_DIR_ENV, dir.path().to_string_lossy());
    let probed = Client::new(Box::new(t)).probe().expect("probe");
    // The workspace figure is measured on that filesystem, so it must be a real
    // answer rather than the "could not measure" zero.
    assert!(
        probed.capabilities.workspace_mb > 0 || !probed.capabilities.notes.is_empty(),
        "an unmeasurable workspace must leave a note rather than a silent zero"
    );
}

/// The fingerprint we print must be the one OpenSSH prints.
///
/// Pairing is trust on first use, and the only check it ever gets is a human
/// comparing our `SHA256:…` against `ssh-keygen -lf` on the machine itself. If
/// those two strings are computed differently the check silently cannot pass,
/// and the advice in `h5i runner pair` is worse than none. So this generates a
/// real key with real OpenSSH and compares the two.
#[test]
fn our_fingerprint_is_the_one_ssh_keygen_prints() {
    if Command::new("ssh-keygen").arg("-h").output().is_err() {
        eprintln!("skipping: ssh-keygen is not installed");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let key = dir.path().join("host_key");

    let keygen = Command::new("ssh-keygen")
        .args(["-t", "ed25519", "-N", ""])
        .arg("-f")
        .arg(&key)
        .args(["-C", "h5i-test"])
        .output()
        .expect("run ssh-keygen");
    assert!(
        keygen.status.success(),
        "ssh-keygen failed: {}",
        String::from_utf8_lossy(&keygen.stderr)
    );

    let pub_text = std::fs::read_to_string(key.with_extension("pub")).expect("read .pub");
    let parsed = h5i_runner::HostKey::parse(&pub_text).expect("parse a real OpenSSH public key");
    assert_eq!(parsed.algorithm, "ssh-ed25519");

    // `ssh-keygen -lf` prints: <bits> SHA256:<base64> <comment> (ED25519)
    let listed = Command::new("ssh-keygen")
        .arg("-lf")
        .arg(key.with_extension("pub"))
        .output()
        .expect("run ssh-keygen -lf");
    let listed = String::from_utf8_lossy(&listed.stdout);
    let theirs = listed
        .split_whitespace()
        .find(|f| f.starts_with("SHA256:"))
        .expect("a SHA256 fingerprint in ssh-keygen output");

    assert_eq!(
        parsed.fingerprint(),
        theirs,
        "our fingerprint must be comparable with what a user reads on the runner"
    );

    // And the identity is that same hash in hex, so the two spellings can never
    // disagree about which machine this is.
    assert_eq!(parsed.runner_id().len(), 64);
    assert!(parsed.runner_id().chars().all(|c| c.is_ascii_hexdigit()));
}

/// The `authorized_keys` line, built from a real key, is one line and restricts.
#[test]
fn the_authorized_keys_line_is_built_from_a_real_key() {
    if Command::new("ssh-keygen").arg("-h").output().is_err() {
        eprintln!("skipping: ssh-keygen is not installed");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let key = dir.path().join("pair_key");
    let keygen = Command::new("ssh-keygen")
        .args(["-t", "ed25519", "-N", ""])
        .arg("-f")
        .arg(&key)
        .args(["-C", "h5i-runner-pi5"])
        .output()
        .expect("run ssh-keygen");
    assert!(keygen.status.success());

    let public = std::fs::read_to_string(key.with_extension("pub")).expect("read .pub");
    let line = h5i_runner::transport::authorized_keys_line(
        std::path::Path::new("/usr/local/bin/h5i"),
        &public,
    );

    assert!(line.starts_with("restrict,command=\"/usr/local/bin/h5i runner serve-stdio\" "));
    assert!(!line.contains('\n'), "one key is one line, always");
    assert!(line.ends_with("h5i-runner-pi5"), "the comment survives: {line}");

    // The field the installer greps for to avoid adding the same key twice is
    // the base64 blob, second from the end. Pin that shape here, because the
    // installer's `awk '{print $(NF-1)}'` depends on it.
    let fields: Vec<&str> = line.split_whitespace().collect();
    assert_eq!(
        fields[fields.len() - 2],
        public.split_whitespace().nth(1).unwrap(),
        "the blob is second from the end, which is what the installer greps for"
    );
}
