//! A WebSocket client, cut down to what a viewer needs.
//!
//! The box's stream server is a WebSocket server, so reaching it means being a
//! WebSocket client. There is no way around the protocol. What there *is* a way
//! around is taking a full async stack for it: this viewer opens exactly one
//! connection, to one server, over a socket the host already holds, and never
//! negotiates an extension or a subprotocol. That is a few hundred lines of
//! RFC 6455 rather than a dependency, and it keeps the code between an untrusted
//! box and the host's terminal small enough to read in one sitting.
//!
//! Everything the box sends is untrusted input. The reader therefore treats
//! every length as hostile until it is under a cap, refuses reserved opcodes,
//! and never lets a fragmented message grow without bound.
//!
//! What is deliberately *not* implemented: `permessage-deflate` (never
//! negotiated, so never received), and server-to-client masking (RFC 6455
//! forbids it, and a masked server frame is a protocol violation we reject
//! rather than accommodate).

use std::io::{Read, Write};

use base64::Engine as _;
use sha1::{Digest, Sha1};

use crate::error::H5iError;

/// The GUID RFC 6455 concatenates with the client key. Fixed by the standard.
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Cap on one reassembled message. Frames are base64 JPEG, so a 4K viewport at
/// high quality is a few hundred KiB; 16 MiB is far above anything legitimate
/// and far below anything that would hurt the host.
const MAX_MESSAGE: usize = 16 << 20;

pub mod opcode {
    pub const CONTINUATION: u8 = 0x0;
    pub const TEXT: u8 = 0x1;
    pub const BINARY: u8 = 0x2;
    pub const CLOSE: u8 = 0x8;
    pub const PING: u8 = 0x9;
    pub const PONG: u8 = 0xA;
}

// ─── handshake ──────────────────────────────────────────────────────────────

/// A fresh `Sec-WebSocket-Key`: 16 random bytes, base64.
pub fn new_key() -> String {
    let raw: [u8; 16] = std::array::from_fn(|_| fastrand::u8(..));
    base64::engine::general_purpose::STANDARD.encode(raw)
}

/// The `Sec-WebSocket-Accept` a conforming server must return for `key`.
pub fn expected_accept(key: &str) -> String {
    let mut h = Sha1::new();
    h.update(key.as_bytes());
    h.update(WS_GUID.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(h.finalize())
}

/// The opening handshake.
///
/// No `Origin` header is sent, and that is deliberate rather than lazy: the
/// stream server allows a missing origin (it is how a command-line client
/// connects) and refuses any origin that is not loopback. Sending one would be
/// inventing a web page that does not exist.
pub fn request(host: &str, path: &str, key: &str) -> String {
    format!(
        "GET {path} HTTP/1.1\r\n\
         Host: {host}\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Key: {key}\r\n\
         Sec-WebSocket-Version: 13\r\n\
         \r\n"
    )
}

/// Check the server's response to [`request`].
///
/// Verifying the accept hash is not ceremony. It is the one thing that proves
/// the bytes now on this socket come from something that understood a WebSocket
/// handshake, rather than from an HTTP server that answered `200` and is about
/// to hand us a page, which is precisely the failure that would otherwise show
/// up as garbled frames much later, in a decoder.
pub fn verify_response(head: &str, key: &str) -> Result<(), H5iError> {
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap_or_default();
    if !status.contains("101") {
        return Err(H5iError::Metadata(format!(
            "the box's browser stream refused the connection: {}",
            status.trim()
        )));
    }
    let expected = expected_accept(key);
    let got = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("sec-websocket-accept"))
        .map(|(_, v)| v.trim().to_string());
    match got {
        Some(v) if v == expected => Ok(()),
        Some(_) => Err(H5iError::Metadata(
            "the box's browser stream answered with a bad WebSocket accept key — \
             something other than the stream server is on that port"
                .into(),
        )),
        None => Err(H5iError::Metadata(
            "the box's browser stream did not complete a WebSocket handshake".into(),
        )),
    }
}

/// Read up to the end of an HTTP header block, and no further.
///
/// Stopping exactly at the blank line matters: the next byte on the socket is
/// the first byte of a WebSocket frame, and reading it into the header buffer
/// would desynchronize the frame parser for the life of the connection. This is
/// the same class of bug the forward hit with a stray CRLF, from the other side.
pub fn read_head(r: &mut impl Read) -> std::io::Result<String> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while buf.len() < 16 * 1024 {
        if r.read(&mut byte)? == 0 {
            break;
        }
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

// ─── framing ────────────────────────────────────────────────────────────────

/// Encode one client frame. Always masked, as RFC 6455 requires of a client.
pub fn encode_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 14);
    out.push(0x80 | opcode); // FIN, never fragmented: our messages are small.
    let len = payload.len();
    if len < 126 {
        out.push(0x80 | len as u8);
    } else if len <= u16::MAX as usize {
        out.push(0x80 | 126);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(0x80 | 127);
        out.extend_from_slice(&(len as u64).to_be_bytes());
    }
    let mask: [u8; 4] = std::array::from_fn(|_| fastrand::u8(..));
    out.extend_from_slice(&mask);
    out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    out
}

/// A message reassembled from one or more frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    Close,
}

/// Reads messages from the box, reassembling fragments.
///
/// Fragmentation state has to live across calls, because a control frame may
/// arrive *between* the fragments of a message and must be delivered without
/// disturbing the partial one. That is why this is a struct and not a function.
pub struct Reader<R> {
    inner: R,
    /// Bytes of a message still being assembled.
    partial: Vec<u8>,
    /// The opcode the fragmented message started with, if one is in progress.
    partial_opcode: Option<u8>,
}

impl<R: Read> Reader<R> {
    pub fn new(inner: R) -> Self {
        Reader {
            inner,
            partial: Vec::new(),
            partial_opcode: None,
        }
    }

    /// The next complete message, or `None` at a clean end of stream.
    pub fn next_message(&mut self) -> std::io::Result<Option<Incoming>> {
        loop {
            let Some(frame) = self.read_frame()? else {
                return Ok(None);
            };
            let is_control = frame.opcode & 0x8 != 0;

            if is_control {
                // Control frames are never fragmented and never join a
                // message in progress.
                return Ok(Some(match frame.opcode {
                    opcode::PING => Incoming::Ping(frame.payload),
                    opcode::PONG => Incoming::Pong(frame.payload),
                    opcode::CLOSE => Incoming::Close,
                    other => return Err(protocol_error(&format!("reserved opcode {other:#x}"))),
                }));
            }

            match frame.opcode {
                opcode::CONTINUATION => {
                    if self.partial_opcode.is_none() {
                        return Err(protocol_error("continuation with nothing to continue"));
                    }
                }
                opcode::TEXT | opcode::BINARY => {
                    if self.partial_opcode.is_some() {
                        return Err(protocol_error("a new message interrupted a fragmented one"));
                    }
                    self.partial_opcode = Some(frame.opcode);
                }
                other => return Err(protocol_error(&format!("reserved opcode {other:#x}"))),
            }

            // Checked before extending, so a hostile box cannot make us hold
            // more than the cap even across many small fragments.
            if self.partial.len() + frame.payload.len() > MAX_MESSAGE {
                return Err(protocol_error("message over the size cap"));
            }
            self.partial.extend_from_slice(&frame.payload);

            if !frame.fin {
                continue;
            }
            let bytes = std::mem::take(&mut self.partial);
            return Ok(Some(match self.partial_opcode.take() {
                Some(opcode::TEXT) => Incoming::Text(String::from_utf8_lossy(&bytes).into_owned()),
                _ => Incoming::Binary(bytes),
            }));
        }
    }

    fn read_frame(&mut self) -> std::io::Result<Option<Frame>> {
        let mut head = [0u8; 2];
        match self.inner.read_exact(&mut head) {
            Ok(()) => {}
            Err(e)
                if e.kind() == std::io::ErrorKind::UnexpectedEof
                    || e.kind() == std::io::ErrorKind::ConnectionReset =>
            {
                return Ok(None)
            }
            Err(e) => return Err(e),
        }
        let fin = head[0] & 0x80 != 0;
        // The three reserved bits are only meaningful under an extension, and
        // we negotiate none. Set means the peer is speaking something we did
        // not agree to, which is a protocol error rather than a bit to ignore.
        if head[0] & 0x70 != 0 {
            return Err(protocol_error("reserved bits set without an extension"));
        }
        let opcode = head[0] & 0x0f;
        let masked = head[1] & 0x80 != 0;
        // A server must not mask. One that does is not the server we think.
        if masked {
            return Err(protocol_error("the server masked a frame"));
        }

        let len = match head[1] & 0x7f {
            126 => {
                let mut ext = [0u8; 2];
                self.inner.read_exact(&mut ext)?;
                u16::from_be_bytes(ext) as usize
            }
            127 => {
                let mut ext = [0u8; 8];
                self.inner.read_exact(&mut ext)?;
                let n = u64::from_be_bytes(ext);
                // Compared before the cast, so a 64-bit length cannot wrap into
                // a small one on a 32-bit host.
                if n > MAX_MESSAGE as u64 {
                    return Err(protocol_error("frame over the size cap"));
                }
                n as usize
            }
            n => n as usize,
        };
        if len > MAX_MESSAGE {
            return Err(protocol_error("frame over the size cap"));
        }
        // Control frames carry at most 125 bytes and are never fragmented.
        if opcode & 0x8 != 0 && (len > 125 || !fin) {
            return Err(protocol_error("malformed control frame"));
        }

        let mut payload = vec![0u8; len];
        self.inner.read_exact(&mut payload)?;
        Ok(Some(Frame {
            fin,
            opcode,
            payload,
        }))
    }
}

struct Frame {
    fin: bool,
    opcode: u8,
    payload: Vec<u8>,
}

fn protocol_error(what: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("websocket protocol error: {what}"),
    )
}

/// Send one text message.
pub fn send_text(w: &mut impl Write, text: &str) -> std::io::Result<()> {
    w.write_all(&encode_frame(opcode::TEXT, text.as_bytes()))?;
    w.flush()
}

/// Send a pong carrying the ping's payload, as the protocol requires.
pub fn send_pong(w: &mut impl Write, payload: &[u8]) -> std::io::Result<()> {
    w.write_all(&encode_frame(opcode::PONG, payload))?;
    w.flush()
}

/// Send a close frame with the normal-closure status.
pub fn send_close(w: &mut impl Write) -> std::io::Result<()> {
    w.write_all(&encode_frame(opcode::CLOSE, &1000u16.to_be_bytes()))?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_accept_hash_matches_the_worked_example_in_rfc_6455() {
        // Section 1.3's example, which is the only way to know the sha1 and
        // base64 wiring is right rather than merely self-consistent.
        assert_eq!(
            expected_accept("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn a_server_that_is_not_a_stream_server_is_caught_at_the_handshake() {
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        let good = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
             Sec-WebSocket-Accept: {}\r\n\r\n",
            expected_accept(key)
        );
        assert!(verify_response(&good, key).is_ok());
        // Header names are case-insensitive on the wire.
        let odd_case = good.replace("Sec-WebSocket-Accept", "sec-websocket-accept");
        assert!(verify_response(&odd_case, key).is_ok());

        // An ordinary HTTP server answering 200 is the failure this catches:
        // without the check it becomes garbage in the JPEG decoder, much later.
        assert!(verify_response("HTTP/1.1 200 OK\r\n\r\n", key).is_err());
        // A 101 with the wrong hash is worse than a 200, and also refused.
        let wrong = "HTTP/1.1 101 Switching Protocols\r\nSec-WebSocket-Accept: nope\r\n\r\n";
        assert!(verify_response(wrong, key).is_err());
        // A 101 with no accept header at all.
        assert!(verify_response("HTTP/1.1 101 Switching\r\n\r\n", key).is_err());
    }

    #[test]
    fn the_request_omits_origin_so_the_stream_treats_us_as_a_cli_client() {
        let req = request("127.0.0.1:9222", "/", "KEY");
        assert!(req.starts_with("GET / HTTP/1.1\r\n"), "{req}");
        assert!(req.contains("Sec-WebSocket-Version: 13"), "{req}");
        assert!(req.contains("Sec-WebSocket-Key: KEY"), "{req}");
        // The stream server allows a missing Origin and refuses a non-loopback
        // one. Inventing a page that does not exist would be the wrong claim.
        assert!(!req.to_ascii_lowercase().contains("origin:"), "{req}");
        assert!(req.ends_with("\r\n\r\n"), "{req:?}");
    }

    #[test]
    fn the_head_reader_stops_at_the_blank_line() {
        // The byte after the header block is the first byte of a frame.
        // Swallowing it desynchronizes the parser forever.
        let raw = b"HTTP/1.1 101 x\r\nA: b\r\n\r\n\x81\x05hello";
        let mut cur = std::io::Cursor::new(&raw[..]);
        let head = read_head(&mut cur).unwrap();
        assert!(head.ends_with("\r\n\r\n"));
        let mut rest = Vec::new();
        cur.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, b"\x81\x05hello");
    }

    #[test]
    fn client_frames_are_masked_and_round_trip() {
        let f = encode_frame(opcode::TEXT, b"hi");
        assert_eq!(f[0], 0x81, "FIN plus text opcode");
        assert_eq!(f[1] & 0x80, 0x80, "a client frame must be masked");
        assert_eq!((f[1] & 0x7f) as usize, 2);
        let mask = &f[2..6];
        let unmasked: Vec<u8> = f[6..]
            .iter()
            .enumerate()
            .map(|(i, b)| b ^ mask[i % 4])
            .collect();
        assert_eq!(unmasked, b"hi");
    }

    #[test]
    fn frame_lengths_use_the_shortest_encoding_at_each_boundary() {
        assert_eq!(encode_frame(opcode::TEXT, &[0u8; 125])[1] & 0x7f, 125);
        assert_eq!(encode_frame(opcode::TEXT, &[0u8; 126])[1] & 0x7f, 126);
        assert_eq!(
            encode_frame(opcode::TEXT, &vec![0u8; 65535])[1] & 0x7f,
            126,
            "16-bit length still fits at u16::MAX"
        );
        assert_eq!(encode_frame(opcode::TEXT, &vec![0u8; 65536])[1] & 0x7f, 127);
    }

    /// One unmasked server frame.
    fn server_frame(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mut f = vec![if fin { 0x80 | opcode } else { opcode }];
        if payload.len() < 126 {
            f.push(payload.len() as u8);
        } else {
            f.push(126);
            f.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        }
        f.extend_from_slice(payload);
        f
    }

    #[test]
    fn a_fragmented_message_is_reassembled() {
        let mut wire = server_frame(false, opcode::TEXT, b"{\"ty");
        wire.extend(server_frame(false, opcode::CONTINUATION, b"pe\":\"f"));
        wire.extend(server_frame(true, opcode::CONTINUATION, b"rame\"}"));
        let mut r = Reader::new(&wire[..]);
        assert_eq!(
            r.next_message().unwrap(),
            Some(Incoming::Text("{\"type\":\"frame\"}".into()))
        );
        assert_eq!(r.next_message().unwrap(), None);
    }

    #[test]
    fn a_ping_between_fragments_does_not_corrupt_the_message() {
        // The case that makes this a struct rather than a function: a control
        // frame is delivered immediately, and the half-built message survives.
        let mut wire = server_frame(false, opcode::TEXT, b"ab");
        wire.extend(server_frame(true, opcode::PING, b"beat"));
        wire.extend(server_frame(true, opcode::CONTINUATION, b"cd"));
        let mut r = Reader::new(&wire[..]);
        assert_eq!(
            r.next_message().unwrap(),
            Some(Incoming::Ping(b"beat".to_vec()))
        );
        assert_eq!(
            r.next_message().unwrap(),
            Some(Incoming::Text("abcd".into()))
        );
    }

    #[test]
    fn a_long_frame_survives_the_extended_length_path() {
        // Real frames are base64 JPEG and always take this path.
        let payload = vec![b'x'; 4096];
        let wire = server_frame(true, opcode::TEXT, &payload);
        let mut r = Reader::new(&wire[..]);
        let Some(Incoming::Text(t)) = r.next_message().unwrap() else {
            panic!("expected text")
        };
        assert_eq!(t.len(), 4096);
    }

    #[test]
    fn hostile_framing_is_refused_rather_than_accommodated() {
        let cases: Vec<(&str, Vec<u8>)> = vec![
            // A masked server frame violates the RFC and means this is not the
            // server we think it is.
            ("masked server frame", {
                let mut f = vec![0x81, 0x82, 1, 2, 3, 4];
                f.extend_from_slice(b"\x00\x00");
                f
            }),
            // Reserved bits without a negotiated extension.
            ("reserved bits", vec![0xC1, 0x00]),
            // A continuation with nothing to continue.
            ("orphan continuation", server_frame(true, 0x0, b"x")),
            // A control frame that is fragmented, or oversized.
            ("fragmented control", server_frame(false, opcode::PING, b"x")),
            // A reserved opcode.
            ("reserved opcode", server_frame(true, 0x3, b"x")),
            // A 64-bit length past the cap: must be refused *before* it becomes
            // an allocation.
            ("absurd 64-bit length", {
                let mut f = vec![0x81, 127];
                f.extend_from_slice(&u64::MAX.to_be_bytes());
                f
            }),
        ];
        for (name, wire) in cases {
            let mut r = Reader::new(&wire[..]);
            assert!(r.next_message().is_err(), "{name} should be refused");
        }
    }

    #[test]
    fn a_new_message_may_not_interrupt_a_fragmented_one() {
        let mut wire = server_frame(false, opcode::TEXT, b"ab");
        wire.extend(server_frame(true, opcode::TEXT, b"cd"));
        let mut r = Reader::new(&wire[..]);
        assert!(r.next_message().is_err());
    }

    #[test]
    fn the_client_completes_a_real_handshake_and_talks_both_ways() {
        // The unit tests above check each half against a fixture. This one runs
        // the whole exchange over a socket, which is the only way to catch the
        // wiring faults that live between them. A key that is generated but
        // not the one sent, a header block that swallows the first frame byte,
        // a client frame the far side cannot unmask.
        use std::net::{TcpListener, TcpStream};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let head = read_head(&mut sock).unwrap();
            // Answer the key the client actually sent, not one we chose.
            let key = head
                .split("\r\n")
                .filter_map(|l| l.split_once(':'))
                .find(|(n, _)| n.trim().eq_ignore_ascii_case("sec-websocket-key"))
                .map(|(_, v)| v.trim().to_string())
                .expect("no client key");
            sock.write_all(
                format!(
                    "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
                     Connection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
                    expected_accept(&key)
                )
                .as_bytes(),
            )
            .unwrap();
            // A frame the client should read, sent unmasked as a server must.
            sock.write_all(&{
                let payload = br#"{"type":"frame","seq":1,"data":"AAAA"}"#;
                let mut f = vec![0x81, payload.len() as u8];
                f.extend_from_slice(payload);
                f
            })
            .unwrap();
            sock.flush().unwrap();

            // And one the client sends back. [`Reader`] cannot be used here:
            // it is a *client* reader and refuses a masked frame, which is
            // exactly what a conforming client must send. So the frame is
            // unmasked by hand, which is also the check that matters. That a
            // real server can recover what we wrote.
            let mut head = [0u8; 2];
            sock.read_exact(&mut head).unwrap();
            assert_eq!(head[0], 0x81, "FIN plus text");
            assert_eq!(head[1] & 0x80, 0x80, "a client frame must be masked");
            let len = (head[1] & 0x7f) as usize;
            let mut mask = [0u8; 4];
            sock.read_exact(&mut mask).unwrap();
            let mut payload = vec![0u8; len];
            sock.read_exact(&mut payload).unwrap();
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= mask[i % 4];
            }
            String::from_utf8(payload).unwrap()
        });

        let mut client = TcpStream::connect(addr).unwrap();
        let key = new_key();
        client
            .write_all(request(&addr.to_string(), "/", &key).as_bytes())
            .unwrap();
        client.flush().unwrap();
        let head = read_head(&mut client).unwrap();
        verify_response(&head, &key).expect("handshake should be accepted");

        let mut writer = client.try_clone().unwrap();
        let mut reader = Reader::new(client);
        assert_eq!(
            reader.next_message().unwrap(),
            Some(Incoming::Text(r#"{"type":"frame","seq":1,"data":"AAAA"}"#.into())),
            "the first frame after the header block must not be truncated"
        );

        send_text(&mut writer, r#"{"type":"ack","seq":1}"#).unwrap();
        assert_eq!(server.join().unwrap(), r#"{"type":"ack","seq":1}"#);
    }

    #[test]
    fn a_clean_end_of_stream_is_not_an_error() {
        // The ordinary shape when the box's browser shuts down. It must read as
        // "the session ended", not as a failure worth a stack of red text.
        let mut r = Reader::new(&b""[..]);
        assert_eq!(r.next_message().unwrap(), None);
        let closed = server_frame(true, opcode::CLOSE, b"");
        let mut r = Reader::new(&closed[..]);
        assert_eq!(r.next_message().unwrap(), Some(Incoming::Close));
    }
}
