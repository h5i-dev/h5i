//! The P2P handshake, as bytes.
//!
//! One fixed-size frame each way, before anything else happens on a stream:
//!
//! ```text
//! joiner → sharer   "H5IS" | version | 64 ASCII hex   (69 bytes)
//! sharer → joiner   status                            (1 byte)
//! ```
//!
//! Fixed size on purpose. A length prefix is a number an attacker chooses, and
//! this frame is the very first thing an unauthenticated peer sends — the
//! cheapest way not to have a length-handling bug is not to have a length. The
//! QUIC stream underneath is already encrypted and authenticated by iroh, so
//! this frame is not protecting the secret in transit; it is deciding whether
//! this peer gets a socket into the box.
//!
//! It lives in its own module, free of the P2P dependency, so the format can be
//! tested in a build that has no iroh in it at all.

/// Application-layer protocol negotiation. Both ends must agree on this exact
/// string or QUIC drops the connection before either speaks, which is a free
//  first filter against anything that wandered onto the endpoint.
pub const ALPN: &[u8] = b"h5i/share/1";

const MAGIC: &[u8; 4] = b"H5IS";
const VERSION: u8 = 1;

/// Hex characters in a share secret ([`crate::ticket::SECRET_BYTES`] bytes).
const SECRET_HEX: usize = crate::ticket::SECRET_BYTES * 2;

/// The whole greeting: magic, version, secret.
pub const HELLO_LEN: usize = MAGIC.len() + 1 + SECRET_HEX;

/// The sharer's one-byte answer.
pub const REPLY_OK: u8 = 0;
/// One value for every refusal. The peer learns that it was not let in and
/// nothing about why — unknown, expired and revoked are the sharer's business.
pub const REPLY_DENIED: u8 = 1;
/// The ticket was fine; the share is already carrying as many connections as it
/// will. Separate from [`REPLY_DENIED`] because the two call for opposite
/// reactions — try again, versus ask for a new ticket — and telling a peer to
/// go and get a fresh ticket when the real answer is "wait a moment" is the
/// kind of error message that wastes two people's afternoon.
pub const REPLY_BUSY: u8 = 2;
/// The ticket was fine and the box had nothing listening on the shared port.
/// Distinct again for the same reason: "the dev server is not up" is the
/// sharer's problem to fix, and a peer told "your ticket was refused" will go
/// and ask for a new one that works no better.
pub const REPLY_UNREACHABLE: u8 = 3;

/// Build the greeting a joiner sends.
pub fn encode_hello(secret: &str) -> Option<[u8; HELLO_LEN]> {
    if secret.len() != SECRET_HEX || !secret.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut buf = [0u8; HELLO_LEN];
    buf[..4].copy_from_slice(MAGIC);
    buf[4] = VERSION;
    buf[5..].copy_from_slice(secret.as_bytes());
    Some(buf)
}

/// Read a greeting. `None` for anything that is not one — which is the only
/// answer a caller needs, since every rejection here ends the stream.
pub fn decode_hello(buf: &[u8; HELLO_LEN]) -> Option<String> {
    if &buf[..4] != MAGIC || buf[4] != VERSION {
        return None;
    }
    let secret = std::str::from_utf8(&buf[5..]).ok()?;
    // Checked again on the way in, because this side is the one that will
    // compare it: a caller must never reach the grant table with bytes that
    // are not a well-formed secret.
    if !secret.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(secret.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret() -> String {
        "ab".repeat(crate::ticket::SECRET_BYTES)
    }

    #[test]
    fn a_greeting_survives_the_round_trip() {
        let hello = encode_hello(&secret()).expect("encode");
        assert_eq!(hello.len(), 69);
        assert_eq!(decode_hello(&hello).as_deref(), Some(secret().as_str()));
    }

    #[test]
    fn junk_is_not_mistaken_for_a_greeting() {
        // The first bytes an unauthenticated peer sends. Each of these must end
        // the stream rather than reach the grant table.
        let mut wrong_magic = encode_hello(&secret()).unwrap();
        wrong_magic[0] = b'X';
        assert!(decode_hello(&wrong_magic).is_none());

        let mut wrong_version = encode_hello(&secret()).unwrap();
        wrong_version[4] = 2;
        assert!(decode_hello(&wrong_version).is_none());

        let mut not_hex = encode_hello(&secret()).unwrap();
        not_hex[10] = b'!';
        assert!(decode_hello(&not_hex).is_none());

        assert!(decode_hello(&[0u8; HELLO_LEN]).is_none());
    }

    #[test]
    fn a_secret_of_the_wrong_shape_is_never_put_on_the_wire() {
        assert!(encode_hello("").is_none());
        assert!(encode_hello("abc").is_none());
        assert!(encode_hello(&"zz".repeat(crate::ticket::SECRET_BYTES)).is_none());
    }

    #[test]
    fn the_frame_is_the_same_size_whatever_it_carries() {
        // The reason there is no length prefix: an unauthenticated peer never
        // gets to choose how many bytes we read.
        let a = encode_hello(&secret()).unwrap();
        let b = encode_hello(&"0".repeat(64)).unwrap();
        assert_eq!(a.len(), b.len());
        assert_eq!(a.len(), HELLO_LEN);
    }
}
