//! Just enough RFC 6455 to be a server.
//!
//! Hand-rolled rather than pulled in, for the same reason h5i-core hand-rolled
//! its client: the surface actually used is one handshake, four opcodes and no
//! extensions, and a WebSocket crate would bring an async runtime this engine
//! does not otherwise need.
//!
//! Two rules the h5i viewers depend on, both easy to get wrong:
//!
//! - A server never masks. h5i-core's client fails the connection outright on a
//!   masked server frame, so masking here is a disconnect, not a compatibility
//!   quirk.
//! - Nothing follows the handshake's blank line except the first frame byte. A
//!   stray newline is read as the start of a frame and the viewer hangs with no
//!   error, which is exactly the CRLF bug the web viewer already hit once.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;

use base64::Engine as _;
use h5i_error::H5iError;
use sha1::{Digest, Sha1};

const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Cap on one reassembled message. Client messages here are small JSON
/// controls; anything larger is a bug or an attack, not a config frame.
const MAX_MESSAGE: usize = 1 << 20;

/// Cap on the whole upgrade request. `read_line` grows its `String` until it
/// meets a newline, so a client that opens a socket and sends bytes without one
/// is an unbounded allocation on this side, and the handshake happens before
/// anything about the peer has been established.
const MAX_HANDSHAKE: u64 = 32 * 1024;

/// The largest payload a control frame may carry (RFC 6455 §5.5).
const MAX_CONTROL_PAYLOAD: usize = 125;

#[derive(Debug, PartialEq, Eq)]
pub enum Incoming {
    Text(String),
    Ping(Vec<u8>),
    Pong,
    Close,
}

/// Derive `Sec-WebSocket-Accept` from the client's key.
pub fn accept_key(client_key: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(client_key.as_bytes());
    hasher.update(WS_GUID.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(hasher.finalize())
}

/// Read the request head and answer the upgrade.
///
/// Returns the requested path, which the caller may want; the h5i viewers
/// always use `/`, and the web forward rewrites the request line to strip its
/// `?token=`, so a server that insisted on a query string would break it.
pub fn accept(stream: &mut TcpStream) -> Result<String, H5iError> {
    // `.take`, so the head is bounded as a whole rather than per line: a client
    // dribbling one endless header line grows a `String` on this side without
    // ever reaching a newline, and every `read_line` below would happily follow
    // it. Past the cap the reads return EOF and the handshake fails for want of
    // a key, which is the right answer for a peer that never sent a head.
    let mut reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|e| H5iError::Metadata(format!("could not clone the socket: {e}")))?
            .take(MAX_HANDSHAKE),
    );

    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .map_err(|e| H5iError::Metadata(format!("could not read the request line: {e}")))?;

    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .to_string();

    let mut key = None;
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .map_err(|e| H5iError::Metadata(format!("could not read a header: {e}")))?;
        if read == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("sec-websocket-key")
        {
            key = Some(value.trim().to_string());
        }
    }

    let key = key.ok_or_else(|| {
        H5iError::Metadata("the client sent no Sec-WebSocket-Key".to_string())
    })?;

    // Exactly one terminating CRLF pair, and not a byte more.
    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Accept: {}\r\n\r\n",
        accept_key(&key)
    );
    stream
        .write_all(response.as_bytes())
        .map_err(H5iError::Io)?;
    stream.flush().map_err(H5iError::Io)?;

    Ok(path)
}

/// Write one unmasked text frame.
pub fn send_text(stream: &mut TcpStream, payload: &str) -> Result<(), H5iError> {
    send_frame(stream, 0x1, payload.as_bytes())
}

pub fn send_pong(stream: &mut TcpStream, payload: &[u8]) -> Result<(), H5iError> {
    send_frame(stream, 0xA, payload)
}

fn send_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8]) -> Result<(), H5iError> {
    let mut header = Vec::with_capacity(10);
    header.push(0x80 | opcode); // FIN + opcode

    let len = payload.len();
    if len < 126 {
        header.push(len as u8);
    } else if len <= u16::MAX as usize {
        header.push(126);
        header.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        header.push(127);
        header.extend_from_slice(&(len as u64).to_be_bytes());
    }

    stream.write_all(&header).map_err(H5iError::Io)?;
    stream.write_all(payload).map_err(H5iError::Io)?;
    stream.flush().map_err(H5iError::Io)?;
    Ok(())
}

/// Read one complete message, reassembling continuation frames.
pub fn read_message(reader: &mut impl Read) -> Result<Incoming, H5iError> {
    let mut assembled: Vec<u8> = Vec::new();
    let mut message_opcode: Option<u8> = None;

    loop {
        let mut head = [0u8; 2];
        reader.read_exact(&mut head).map_err(H5iError::Io)?;

        let fin = head[0] & 0x80 != 0;
        let opcode = head[0] & 0x0F;
        let masked = head[1] & 0x80 != 0;
        // Carried as `u64` until it has been checked against the cap. Casting
        // first and checking after truncates on a 32-bit target. A declared
        // length of 2^32+1 became 1, the cap waved it through, and the reader
        // then sat one byte into a payload the peer is still sending, reading
        // its own body as frame headers.
        let mut len = (head[1] & 0x7F) as u64;

        if len == 126 {
            let mut ext = [0u8; 2];
            reader.read_exact(&mut ext).map_err(H5iError::Io)?;
            len = u16::from_be_bytes(ext) as u64;
        } else if len == 127 {
            let mut ext = [0u8; 8];
            reader.read_exact(&mut ext).map_err(H5iError::Io)?;
            len = u64::from_be_bytes(ext);
        }

        if len > MAX_MESSAGE as u64
            || (assembled.len() as u64).saturating_add(len) > MAX_MESSAGE as u64
        {
            return Err(H5iError::Metadata(format!(
                "client message exceeds the {MAX_MESSAGE} byte cap"
            )));
        }
        let len = len as usize;

        // A control frame is never fragmented and never carries more than 125
        // bytes (RFC 6455 §5.5), and a reserved opcode is not something to
        // guess at. All three used to be accepted: a `Ping` with FIN clear was
        // answered as a whole message and left the next read starting on a
        // continuation frame, and a reserved opcode was reassembled as if it
        // were text. The connection is failed instead, which is what the RFC
        // asks for and the only reading that keeps the stream in step.
        let is_control = opcode & 0x8 != 0;
        if is_control && (!fin || len > MAX_CONTROL_PAYLOAD) {
            return Err(H5iError::Metadata(
                "control frame is fragmented or oversized — refusing the connection".to_string(),
            ));
        }
        if !matches!(opcode, 0x0 | 0x1 | 0x2 | 0x8 | 0x9 | 0xA) {
            return Err(H5iError::Metadata(format!(
                "reserved websocket opcode {opcode:#x} — refusing the connection"
            )));
        }

        let mask = if masked {
            let mut key = [0u8; 4];
            reader.read_exact(&mut key).map_err(H5iError::Io)?;
            Some(key)
        } else {
            None
        };

        let mut payload = vec![0u8; len];
        reader.read_exact(&mut payload).map_err(H5iError::Io)?;
        if let Some(key) = mask {
            for (index, byte) in payload.iter_mut().enumerate() {
                *byte ^= key[index % 4];
            }
        }

        // Control frames are never fragmented and may interleave a fragmented
        // data message (RFC 6455 §5.4). At a message boundary we surface them so
        // the caller can pong; *between* fragments we must not, since returning
        // here would discard `assembled` and corrupt the message into its tail
        // alone. We cannot pong from inside this read, so an interleaved ping is
        // skipped and reassembly continues: preserving the in-flight message
        // beats answering one keep-alive, and a dropped ping is benign for these
        // short sessions. Close always surfaces, and discarding a partial message
        // on close is correct.
        let at_boundary = assembled.is_empty() && message_opcode.is_none();
        match opcode {
            0x8 => return Ok(Incoming::Close),
            0x9 if at_boundary => return Ok(Incoming::Ping(payload)),
            0xA if at_boundary => return Ok(Incoming::Pong),
            0x9 | 0xA => continue, // interleaved: skip without touching `assembled`
            0x0 => {} // continuation: keep the opcode we started with
            other => message_opcode = Some(other),
        }

        assembled.extend_from_slice(&payload);

        if fin {
            return match message_opcode {
                Some(0x1) | None => Ok(Incoming::Text(
                    String::from_utf8_lossy(&assembled).into_owned(),
                )),
                // Binary from these viewers would be a protocol error, but
                // decoding it as text loses nothing a caller can act on.
                Some(_) => Ok(Incoming::Text(
                    String::from_utf8_lossy(&assembled).into_owned(),
                )),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_key_matches_the_rfc_example() {
        // RFC 6455 section 1.3. Checking against the spec's own vector is the
        // only way to know the sha1/base64 wiring is right rather than merely
        // self-consistent.
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn server_frames_are_never_masked() {
        // h5i-core's client fails the connection on a masked server frame, so
        // this bit is a compatibility requirement, not a preference.
        let mut out = Vec::new();
        write_frame_for_test(&mut out, 0x1, b"hi");
        assert_eq!(out[0], 0x81, "FIN + text");
        assert_eq!(out[1] & 0x80, 0, "mask bit must be clear");
        assert_eq!(out[1] & 0x7F, 2, "payload length");
    }

    #[test]
    fn frame_length_encoding_switches_at_the_right_boundaries() {
        let mut small = Vec::new();
        write_frame_for_test(&mut small, 0x1, &[b'x'; 125]);
        assert_eq!(small[1], 125, "125 stays inline");

        let mut medium = Vec::new();
        write_frame_for_test(&mut medium, 0x1, &[b'x'; 126]);
        assert_eq!(medium[1], 126, "126 escapes to 16-bit");
        assert_eq!(u16::from_be_bytes([medium[2], medium[3]]), 126);

        let mut large = Vec::new();
        write_frame_for_test(&mut large, 0x1, &[b'x'; 70_000]);
        assert_eq!(large[1], 127, "over u16 escapes to 64-bit");
    }

    #[test]
    fn a_masked_client_text_frame_round_trips() {
        let payload = br#"{"type":"ack","seq":7}"#;
        let mask = [0x37, 0xfa, 0x21, 0x3d];
        let mut frame = vec![0x81, 0x80 | payload.len() as u8];
        frame.extend_from_slice(&mask);
        frame.extend(
            payload
                .iter()
                .enumerate()
                .map(|(i, b)| b ^ mask[i % 4]),
        );

        let message = read_message(&mut frame.as_slice()).expect("decodes");
        assert_eq!(
            message,
            Incoming::Text(r#"{"type":"ack","seq":7}"#.to_string())
        );
    }

    #[test]
    fn fragmented_messages_are_reassembled() {
        // The viewers send small controls, but a fragmented one must not be
        // read as two half-messages of invalid JSON.
        let mut frame = Vec::new();
        // Fragment 1: text opcode, FIN clear.
        frame.extend_from_slice(&[0x01, 4]);
        frame.extend_from_slice(b"{\"a\"");
        // Fragment 2: continuation opcode, FIN set.
        frame.extend_from_slice(&[0x80, 3]);
        frame.extend_from_slice(b":1}");

        let message = read_message(&mut frame.as_slice()).expect("decodes");
        assert_eq!(message, Incoming::Text("{\"a\":1}".to_string()));
    }

    #[test]
    fn a_ping_between_fragments_does_not_corrupt_the_message() {
        // RFC 6455 allows a control frame between fragments. It must not throw
        // away the fragment already assembled. The whole message must survive.
        let mut frame = Vec::new();
        // Fragment 1: text, FIN clear.
        frame.extend_from_slice(&[0x01, 4]);
        frame.extend_from_slice(b"{\"a\"");
        // An interleaved ping (FIN set, unmasked, tiny payload).
        frame.extend_from_slice(&[0x89, 2]);
        frame.extend_from_slice(b"hi");
        // Fragment 2: continuation, FIN set.
        frame.extend_from_slice(&[0x80, 3]);
        frame.extend_from_slice(b":1}");

        let message = read_message(&mut frame.as_slice()).expect("decodes");
        assert_eq!(
            message,
            Incoming::Text("{\"a\":1}".to_string()),
            "the ping must not have eaten the first fragment"
        );
    }

    #[test]
    fn a_ping_at_a_boundary_is_still_surfaced_so_it_can_be_ponged() {
        let frame = [0x89u8, 3, b'a', b'b', b'c'];
        assert_eq!(
            read_message(&mut frame.as_slice()).expect("decodes"),
            Incoming::Ping(b"abc".to_vec())
        );
    }

    /// RFC 6455 §5.5: a control frame is never fragmented and never carries
    /// more than 125 bytes. A fragmented `Ping` was answered as a whole message
    /// and left the next read starting on a continuation frame. The stream and
    /// the reader no longer agreeing about where a frame begins.
    #[test]
    fn a_control_frame_that_breaks_the_rules_fails_the_connection() {
        // Ping with FIN clear.
        let frame = [0x09u8, 2, b'h', b'i'];
        assert!(read_message(&mut frame.as_slice()).is_err());

        // Ping over 125 bytes (escaped to the 16-bit length form).
        let mut big = vec![0x89u8, 126, 0, 200];
        big.extend(std::iter::repeat_n(b'x', 200));
        assert!(read_message(&mut big.as_slice()).is_err());

        // A reserved opcode is refused rather than reassembled as text.
        let frame = [0x83u8, 1, b'x'];
        assert!(read_message(&mut frame.as_slice()).is_err());

        // And the legal shapes still pass.
        let frame = [0x89u8, 3, b'a', b'b', b'c'];
        assert!(read_message(&mut frame.as_slice()).is_ok());
    }

    /// A declared length is checked before it is narrowed to `usize`, so a
    /// 32-bit build cannot truncate `2^32 + 1` to `1` and then read one byte of
    /// a payload the peer is still sending.
    #[test]
    fn an_enormous_declared_length_is_refused_not_truncated() {
        let mut frame = vec![0x81u8, 127];
        frame.extend_from_slice(&(u64::MAX).to_be_bytes());
        assert!(read_message(&mut frame.as_slice()).is_err());

        let mut frame = vec![0x81u8, 127];
        frame.extend_from_slice(&(0x1_0000_0001u64).to_be_bytes());
        assert!(read_message(&mut frame.as_slice()).is_err());
    }

    #[test]
    fn a_close_frame_is_reported_rather_than_treated_as_data() {
        let frame = [0x88u8, 0x00];
        assert_eq!(
            read_message(&mut frame.as_slice()).expect("decodes"),
            Incoming::Close
        );
    }

    /// The framing half of `send_frame`, without needing a socket.
    fn write_frame_for_test(out: &mut Vec<u8>, opcode: u8, payload: &[u8]) {
        out.push(0x80 | opcode);
        let len = payload.len();
        if len < 126 {
            out.push(len as u8);
        } else if len <= u16::MAX as usize {
            out.push(126);
            out.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            out.push(127);
            out.extend_from_slice(&(len as u64).to_be_bytes());
        }
        out.extend_from_slice(payload);
    }
}
