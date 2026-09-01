//! The invite ticket: the whole access model for a P2P share.

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
    /// The port inside the box. Informational for the joiner. The sharer's
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
    /// Everything here is attacker-supplied in the case that matters, someone
    /// sent you a ticket, so each step refuses rather than repairs, and the
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
        // Quotes come along when somebody copies out of a chat client, and
        // the refusal read as a lie: they can see `h5i1_` on their screen.
        let s = s.trim_matches(|c| c == '"' || c == '\'' || c == '`');
        // And so do line breaks. A ticket is one long line; email clients hard
        // wrap at 72 or 78 columns, terminals wrap on copy, and chat clients
        // insert breaks of their own, after which this said "probably truncated
        // or line-wrapped": an accurate diagnosis and a dead end for somebody
        // holding the whole ticket in two pieces. The base64 alphabet contains no
        // whitespace, so removing it cannot change which ticket this is; it can
        // only turn a refusal into the ticket the sender meant. Allocates only
        // when there is something to remove.
        let rejoined: String;
        let s = if s.contains(char::is_whitespace) {
            rejoined = s.chars().filter(|c| !c.is_whitespace()).collect();
            rejoined.as_str()
        } else {
            s
        };
        let body = s.strip_prefix(PREFIX).ok_or_else(|| {
            if s.starts_with("http://") || s.starts_with("https://") {
                return H5iError::Metadata(
                    "that is a tunnel link, not a ticket — open it in a browser. `h5i join` \
                     takes the `h5i1_…` ticket a peer-to-peer share prints."
                        .into(),
                );
            }
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
            // A cut-short ticket is the commoner of the two truncation cases,
            // a chat client eliding a long line, and it used to be the one
            // that got serde's cursor position while the line-wrapped case got
            // plain language.
            if e.is_eof() {
                return H5iError::Metadata(
                    "this ticket looks cut short — ask for the whole line, in one piece".into(),
                );
            }
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
    fn a_ticket_a_mail_client_wrapped_is_still_that_ticket() {
        // A ticket is one long line and the world is full of things that break
        // long lines: mail clients hard wrap at 72 or 78 columns, terminals wrap
        // on copy, chat clients insert their own. The refusal for that was
        // accurate, "probably truncated or line-wrapped", and a dead end for
        // somebody holding the whole ticket in two pieces. Whitespace is not in
        // the base64 alphabet, so removing it cannot turn one ticket into
        // another; it can only turn a refusal into the ticket the sender meant.
        let one_line = sample().encode().expect("encode");
        let mid = one_line.len() / 2;
        let wrapped = format!("{}\n{}", &one_line[..mid], &one_line[mid..]);
        assert_eq!(
            Ticket::decode(&wrapped).expect("a wrapped ticket").secret,
            sample().secret
        );

        // Every shape of the same accident: CRLF from a Windows mail client,
        // several breaks from a narrow column, a space where a terminal broke
        // it, and quotes around the lot from a chat client.
        let crlf = one_line
            .as_bytes()
            .chunks(40)
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect::<Vec<_>>()
            .join("\r\n");
        assert_eq!(
            Ticket::decode(&crlf).expect("a hard-wrapped ticket").port,
            3000
        );
        assert_eq!(
            Ticket::decode(&format!("\"{wrapped}\""))
                .expect("quoted and wrapped")
                .port,
            3000
        );
        assert_eq!(
            Ticket::decode(&format!("{}  {}", &one_line[..mid], &one_line[mid..]))
                .expect("a space where the break was")
                .grant,
            "a1b2c3d4"
        );

        // What must not change: two tickets pasted one after another are still
        // two tickets, not one long one that happens to decode.
        let two = format!("{one_line}\n{one_line}");
        assert!(Ticket::decode(&two).is_err(), "two tickets decoded as one");
    }

    #[test]
    fn the_three_ways_a_ticket_arrives_wrong_each_say_which() {
        // A cut-short ticket used to get serde's cursor position while the
        // line-wrapped one got plain language. Backwards, since eliding a
        // long line is the commoner accident.
        let good = super::Ticket {
            v: 1,
            box_id: "env/human/demo".into(),
            port: 3000,
            grant: "g1".into(),
            expires_at: 4_000_000_000,
            secret: "ab".repeat(SECRET_BYTES),
            addr: serde_json::json!({"id": "cd", "addrs": []}),
        }
        .encode()
        .expect("encode");

        let cut = &good[..good.len() / 2];
        let err = format!("{}", Ticket::decode(cut).expect_err("cut short"));
        assert!(
            err.contains("cut short") || err.contains("not valid base64"),
            "{err}"
        );

        // Quotes come along when somebody copies out of a chat client, and the
        // refusal read as a lie: they can see `h5i1_` on their screen.
        Ticket::decode(&format!("\"{good}\"")).expect("a quoted ticket still decodes");
        Ticket::decode(&format!("`{good}`")).expect("a backticked ticket still decodes");

        // And a tunnel link is not a ticket, but it is an easy thing to paste.
        let err = format!(
            "{}",
            Ticket::decode("https://odd-cat.trycloudflare.com/?h5i=abc").expect_err("a url")
        );
        assert!(err.contains("open it in a browser"), "{err}");
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
        // truncated sailing through decode and then being compared. A short
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
