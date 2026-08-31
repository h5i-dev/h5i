//! The P2P handshake, as bytes.
//!
//! One fixed-size frame each way, before anything else happens on a stream:
//!
//! ```text
//! joiner → sharer   "H5IS" | version | 64 ASCII hex   (69 bytes)
//! sharer → joiner   status                            (1 byte)
//! ```
//!
//! The magic has a second spelling, `H5IP`, meaning "I am only checking this
//! ticket". Same size, same version, same secret; what differs is what the sharer
//! does with it. A `H5IS` greeting ends with a socket into the box; a `H5IP` one
//! is answered from the grant table alone and touches nothing. That distinction
//! is why it is a separate frame rather than a convention: without it the check
//! cost a connection to the dev server, a slot out of the share's 64, and, if
//! that dev server did not close on end-of-input, a pump parked for the life of
//! the joiner, holding the slot.
//!
//! Fixed size on purpose. A length prefix is a number an attacker chooses, and
//! this frame is the first thing an unauthenticated peer sends. The cheapest way
//! not to have a length-handling bug is not to have a length. The QUIC stream
//! underneath is already encrypted and authenticated by iroh, so this frame is
//! not protecting the secret in transit; it decides whether this peer gets a
//! socket into the box.
//!
//! It lives in its own module, free of the P2P dependency, so the format can be
//! tested in a build that has no iroh in it at all.

/// Application-layer protocol negotiation. Both ends must agree on this exact
/// string or QUIC drops the connection before either speaks, a free first
/// filter against anything that wandered onto the endpoint.
/// Bumped when the greeting changed. The `H5IP` spelling below is a frame an
/// older sharer reads as junk and answers `REPLY_DENIED` to, which the joiner
/// would render as "the sharer refused this ticket … ask for a new one", and the
/// new ticket would fail identically, forever. ALPN is the one place where two
/// versions can fail to agree *before* either says anything, so a skew ends as a
/// connection that will not open rather than as advice nobody can act on.
pub const ALPN: &[u8] = b"h5i/share/2";

const MAGIC: &[u8; 4] = b"H5IS";
/// The same greeting, asking only whether the ticket is good.
const MAGIC_PROBE: &[u8; 4] = b"H5IP";
const VERSION: u8 = 1;

/// What a greeting is asking for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// Give me a socket into the box.
    Stream,
    /// Only tell me whether this ticket is good.
    Probe,
}

/// Hex characters in a share secret ([`crate::ticket::SECRET_BYTES`] bytes).
const SECRET_HEX: usize = crate::ticket::SECRET_BYTES * 2;

/// The whole greeting: magic, version, secret.
pub const HELLO_LEN: usize = MAGIC.len() + 1 + SECRET_HEX;

/// The sharer's one-byte answer.
pub const REPLY_OK: u8 = 0;
/// One value for every refusal. The peer learns that it was not let in and
/// nothing about why. Unknown, expired and revoked are the sharer's business.
pub const REPLY_DENIED: u8 = 1;
/// The ticket was fine; the share is already carrying as many connections as it
/// will. Separate from [`REPLY_DENIED`] because the two call for opposite
/// reactions (try again, versus ask for a new ticket) and telling a peer to
/// go and get a fresh ticket when the real answer is "wait a moment" is the
/// kind of error message that wastes two people's afternoon.
pub const REPLY_BUSY: u8 = 2;
/// The ticket was fine and the box had nothing listening on the shared port.
/// Distinct again for the same reason: "the dev server is not up" is the
/// sharer's problem to fix, and a peer told "your ticket was refused" will go
/// and ask for a new one that works no better.
pub const REPLY_UNREACHABLE: u8 = 3;
/// The ticket was fine and h5i itself could not reach the box. The dialer is
/// gone, retired, or the namespace has no loopback. Distinct from
/// [`REPLY_UNREACHABLE`] for the third time in the same argument: that one is a
/// sentence about the sharer's dev server, and this is a sentence about h5i.
/// The receipt learned to tell them apart a few rounds ago; the wire could not,
/// so the joiner's browser still said "whoever shared it needs to start their
/// dev server" while the sharer's terminal said the dialer had died.
pub const REPLY_ROUTE_BROKEN: u8 = 4;
/// The share has ended. The ticket was never weighed, because there is no
/// longer a grant table to weigh it against: `share stop` removes it, and a
/// connection can arrive in the moment between that and the process exiting.
/// Folded into [`REPLY_DENIED`] it told the visitor their invite had been
/// revoked, which is a sentence about a decision somebody made about *them*.
pub const REPLY_SHARE_OVER: u8 = 5;
/// The sharer could not read its own grant table. Full disk, no descriptors,
/// a file it cannot parse. The one refusal that is nobody's ticket's fault and
/// that a fresh ticket will not fix; as [`REPLY_DENIED`] it sent the visitor
/// back to ask for a new invite that would fail in exactly the same way.
pub const REPLY_SHARER_FAULT: u8 = 6;

/// Build the greeting a joiner sends.
pub fn encode_hello(secret: &str) -> Option<[u8; HELLO_LEN]> {
    encode(secret, Intent::Stream)
}

/// Build the greeting that only asks whether the ticket is good.
pub fn encode_probe(secret: &str) -> Option<[u8; HELLO_LEN]> {
    encode(secret, Intent::Probe)
}

fn encode(secret: &str, intent: Intent) -> Option<[u8; HELLO_LEN]> {
    if secret.len() != SECRET_HEX || !secret.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut buf = [0u8; HELLO_LEN];
    buf[..4].copy_from_slice(match intent {
        Intent::Stream => MAGIC,
        Intent::Probe => MAGIC_PROBE,
    });
    buf[4] = VERSION;
    buf[5..].copy_from_slice(secret.as_bytes());
    Some(buf)
}

/// Read a greeting. `None` for anything that is not one, which is the only
/// answer a caller needs, since every rejection here ends the stream.
pub fn decode_hello(buf: &[u8; HELLO_LEN]) -> Option<(Intent, String)> {
    let intent = if &buf[..4] == MAGIC {
        Intent::Stream
    } else if &buf[..4] == MAGIC_PROBE {
        Intent::Probe
    } else {
        return None;
    };
    if buf[4] != VERSION {
        return None;
    }
    let secret = std::str::from_utf8(&buf[5..]).ok()?;
    // Checked again on the way in, because this side is the one that will
    // compare it: a caller must never reach the grant table with bytes that
    // are not a well-formed secret.
    if !secret.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some((intent, secret.to_string()))
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
        assert_eq!(
            decode_hello(&hello),
            Some((Intent::Stream, secret().to_string()))
        );

        // And the probe spelling, which differs only in what the sharer does
        // with it: same length, same version, same secret.
        let probe = encode_probe(&secret()).expect("encode");
        assert_eq!(probe.len(), HELLO_LEN);
        assert_eq!(probe[4..], hello[4..], "only the magic may differ");
        assert_eq!(
            decode_hello(&probe),
            Some((Intent::Probe, secret().to_string()))
        );
    }

    #[test]
    fn a_probe_with_the_wrong_version_is_junk_too() {
        // The version check is shared by both magics, which is easy to say and
        // worth a test: the whole enumeration of what an unauthenticated peer
        // can send lives in this module, and a branch nobody exercises is a
        // branch that gets refactored apart.
        let mut probe = encode_probe(&secret()).expect("encode");
        probe[4] = VERSION.wrapping_add(1);
        assert_eq!(decode_hello(&probe), None);
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
