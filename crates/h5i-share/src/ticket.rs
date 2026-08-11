//! The invite ticket: the whole access model for a P2P share.
//!
//! A ticket names one box, one port, one grant and one expiry, and carries the
//! secret that authorizes a peer plus whatever the transport needs in order to
//! find the sharer. Possession is authorization — there is no account on either
//! side, and no directory anywhere that has to agree the two of you know each
//! other.
//!
//! Three properties are deliberate:
//!
//! * **The secret is in the ticket, not on disk.** The sharer keeps only its
//!   SHA-256 ([`crate::session`]), so a stolen box directory does not admit
//!   anyone. The consequence is that a ticket is printed once and cannot be
//!   reprinted; mint another with `h5i box share grant`.
//! * **The addressing is opaque here.** `addr` is whatever the transport put
//!   there, carried as JSON. That keeps this module free of the P2P dependency
//!   so the encoding can be tested, and reviewed, without one.
//! * **Decoding is bounded before it allocates.** A ticket arrives by paste
//!   from wherever, so the length cap is checked on the encoded text, before
//!   base64 and before serde see any of it.

use base64::Engine as _;
use h5i_error::H5iError;
use serde::{Deserialize, Serialize};

/// Version tag and separator. The version is outside the payload so a future
/// format can be refused by looking at the first five characters rather than by
/// parsing something we do not understand yet.
pub const PREFIX: &str = "h5i1_";

/// 256 bits from the OS CSPRNG. A share secret gates a path into a running box
/// from off this machine, which is a stronger claim than the loopback viewer
/// token's 128 bits, and doubling it costs nothing.
pub const SECRET_BYTES: usize = 32;

/// Refuse absurd input before decoding it. A real ticket is a few hundred bytes;
/// this leaves room for a transport that lists many candidate addresses and
/// still says no to a paste that is trying to be a denial of service.
const MAX_ENCODED: usize = 8 * 1024;

/// What a peer needs in order to reach one port of one box, once.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ticket {
    /// Format version. Checked on decode; a mismatch is refused rather than
    /// guessed at.
    pub v: u8,
    /// The box this ticket reaches, for the joiner to see before connecting.
    pub box_id: String,
    /// The port inside the box. Informational for the joiner — the sharer's
    /// bridge is pinned to one port and does not take direction from the wire.
    pub port: u16,
    /// Which grant this is, so `h5i box share revoke <box> <grant>` can name it.
    pub grant: String,
    /// Unix seconds. Enforced by the sharer; shown by the joiner.
    pub expires_at: i64,
    /// Hex, [`SECRET_BYTES`] bytes.
    pub secret: String,
    /// The transport's own addressing, verbatim. Opaque to this module.
    pub addr: serde_json::Value,
}

impl Ticket {
    /// `h5i1_<base64url, no padding>` of the JSON body.
    pub fn encode(&self) -> Result<String, H5iError> {
        let json = serde_json::to_vec(self)?;
        Ok(format!(
            "{PREFIX}{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
        ))
    }

    /// Parse a ticket a human pasted.
    ///
    /// Everything here is attacker-supplied in the case that matters — someone
    /// sent you a ticket — so each step refuses rather than repairs, and the
    /// error says which step failed without echoing the input back.
    pub fn decode(s: &str) -> Result<Ticket, H5iError> {
        let s = s.trim();
        if s.len() > MAX_ENCODED {
            return Err(H5iError::Metadata(format!(
                "this ticket is {} bytes, past the {MAX_ENCODED}-byte limit — that is not a \
                 ticket h5i minted",
                s.len()
            )));
        }
        let body = s.strip_prefix(PREFIX).ok_or_else(|| {
            H5iError::Metadata(format!(
                "that does not look like an h5i ticket: it should start with `{PREFIX}`"
            ))
        })?;
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(body)
            .map_err(|_| {
                H5iError::Metadata(
                    "this ticket is not valid base64 — it was probably truncated or line-wrapped \
                     on the way here"
                        .into(),
                )
            })?;
        let t: Ticket = serde_json::from_slice(&raw).map_err(|e| {
            H5iError::Metadata(format!(
                "this ticket is not in a format h5i understands: {e}"
            ))
        })?;
        if t.v != 1 {
            return Err(H5iError::Metadata(format!(
                "this ticket is version {}, and this h5i speaks version 1 — upgrade whichever \
                 side is older",
                t.v
            )));
        }
        // The secret's shape is checked here rather than at use, so a malformed
        // ticket fails while the message can still say what is wrong with it.
        if t.secret.len() != SECRET_BYTES * 2 || !t.secret.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(H5iError::Metadata(
                "this ticket's secret is malformed — it is not the right length or not hex".into(),
            ));
        }
        Ok(t)
    }

    /// Seconds until expiry, or `None` once it has passed.
    pub fn remaining(&self, now: i64) -> Option<i64> {
        (self.expires_at > now).then_some(self.expires_at - now)
    }
}

/// A fresh share secret: [`SECRET_BYTES`] bytes of OS entropy, hex.
///
/// Delegates to the same mint the viewer token uses, which fails closed on a
/// host with no entropy rather than falling back to something guessable.
pub fn mint_secret() -> Result<String, H5iError> {
    h5i_core::token::hex(SECRET_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Ticket {
        Ticket {
            v: 1,
            box_id: "env/agent/demo".into(),
            port: 3000,
            grant: "a1b2c3d4".into(),
            expires_at: 1_800_000_000,
            secret: "ab".repeat(SECRET_BYTES),
            addr: serde_json::json!({"id": "whatever", "addrs": []}),
        }
    }

    #[test]
    fn a_ticket_survives_the_round_trip() {
        let t = sample();
        let encoded = t.encode().expect("encode");
        assert!(encoded.starts_with(PREFIX));
        assert_eq!(Ticket::decode(&encoded).expect("decode"), t);
    }

    #[test]
    fn whitespace_from_a_chat_client_does_not_break_a_ticket() {
        // Tickets travel through Slack, email and terminals, all of which add
        // surrounding whitespace. Only the surrounding kind is forgiven.
        let encoded = sample().encode().expect("encode");
        let pasted = format!("  {encoded}\n");
        assert!(Ticket::decode(&pasted).is_ok());
    }

    #[test]
    fn something_that_is_not_a_ticket_is_refused_at_each_step() {
        // Each of these fails at a different point, and the point is that none
        // of them reaches serde with attacker-chosen bytes it might work on.
        assert!(Ticket::decode("https://example.test/").is_err());
        assert!(Ticket::decode(&format!("{PREFIX}not-base64!!")).is_err());
        assert!(Ticket::decode(&format!("{PREFIX}{}", "A".repeat(MAX_ENCODED))).is_err());
    }

    #[test]
    fn a_ticket_from_a_newer_h5i_is_refused_rather_than_guessed_at() {
        let mut t = sample();
        t.v = 2;
        let encoded = t.encode().expect("encode");
        let err = Ticket::decode(&encoded).expect_err("version 2 must be refused");
        assert!(format!("{err}").contains("version 2"));
    }

    #[test]
    fn a_secret_that_is_not_a_secret_is_caught_at_decode() {
        // The failure this prevents is a ticket whose secret is empty or
        // truncated sailing through decode and then being compared — a short
        // secret is a weak secret, and it should not get as far as the gate.
        for bad in [
            "",
            "zz",
            &"ab".repeat(SECRET_BYTES - 1),
            &"gg".repeat(SECRET_BYTES),
        ] {
            let mut t = sample();
            t.secret = bad.to_string();
            let encoded = t.encode().expect("encode");
            assert!(
                Ticket::decode(&encoded).is_err(),
                "a {}-char secret must be refused",
                bad.len()
            );
        }
    }

    #[test]
    fn expiry_is_a_question_the_ticket_can_answer() {
        let t = sample();
        assert_eq!(t.remaining(t.expires_at - 60), Some(60));
        assert_eq!(t.remaining(t.expires_at), None);
        assert_eq!(t.remaining(t.expires_at + 1), None);
    }

    #[test]
    fn two_minted_secrets_are_not_the_same_secret() {
        let a = mint_secret().expect("entropy");
        let b = mint_secret().expect("entropy");
        assert_eq!(a.len(), SECRET_BYTES * 2);
        assert_ne!(a, b);
    }
}
