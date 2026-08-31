//! Who a runner is.
//!
//! A name is a label, and a label can be re-pointed at different hardware
//! tomorrow. `runner_id` is the SHA-256 of the runner's SSH *host key*, the thing
//! SSH already authenticates on every connection and the thing our pinned
//! `known_hosts` already refuses to let change silently. Binding a box to that
//! binds it to a machine (ROADMAP.md R6, and the decision it closed in R13).
//!
//! A reinstalled machine with a fresh host key is a fresh identity. That is
//! correct, not a bug: it really is a different trust anchor, whatever its label
//! says.
//!
//! Both spellings of the hash come from here so they cannot drift:
//! [`HostKey::fingerprint`] is the `SHA256:…` string `ssh-keygen -lf` prints, for
//! a human to compare out of band, and [`HostKey::runner_id`] is the hex form
//! that goes in a manifest.

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Longest base64 host key we will decode. Ed25519 keys are ~68 bytes and RSA
/// keys under a kilobyte; this is a bound on what a `ssh-keyscan` we did not
/// write can hand us, not a judgement about key sizes.
const MAX_KEY_B64: usize = 16 * 1024;

#[derive(Debug, Error)]
pub enum IdentityError {
    #[error("could not read an SSH host key from `{0}`")]
    NotAHostKey(String),

    #[error("host key algorithm `{0}` is not one OpenSSH would emit")]
    Algorithm(String),

    #[error("host key is {0} base64 bytes, which is not a key")]
    Size(usize),

    #[error("host key base64 does not decode")]
    Base64,

    #[error(
        "the runner presented a {presented} host key and this runner is pinned to {pinned} — \
         either the machine was reinstalled, or this is not the machine you paired with"
    )]
    Mismatch { pinned: String, presented: String },
}

/// One host key, as OpenSSH writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKey {
    /// `ssh-ed25519`, `ecdsa-sha2-nistp256`, `ssh-rsa`, …
    pub algorithm: String,
    /// The base64 payload, exactly as it appeared.
    pub base64: String,
    /// The decoded key blob, which is what gets hashed. Hashing the *blob*
    /// rather than the text is what makes the fingerprint here equal to the one
    /// `ssh-keygen -lf` prints, so a user can verify it out of band without
    /// trusting us to have invented a compatible scheme.
    pub blob: Vec<u8>,
}

impl HostKey {
    /// Parse `<algorithm> <base64> [comment]`, with or without a leading host
    /// pattern. That covers both shapes this code meets: a `known_hosts` line
    /// (`pi.local ssh-ed25519 AAAA…`) and an `ssh-keyscan` line, which is the
    /// same thing.
    pub fn parse(line: &str) -> Result<Self, IdentityError> {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return Err(IdentityError::NotAHostKey(line.to_string()));
        }
        // A `@cert-authority`/`@revoked` marker is a different kind of entry and
        // must not be read as a key we can pin.
        if line.starts_with('@') {
            return Err(IdentityError::NotAHostKey(line.to_string()));
        }

        let fields: Vec<&str> = line.split_whitespace().collect();
        // Either `algo b64 [comment]` or `host algo b64 [comment]`.
        let (algorithm, b64) = match fields.as_slice() {
            [a, b, ..] if is_key_algorithm(a) => (*a, *b),
            [_host, a, b, ..] if is_key_algorithm(a) => (*a, *b),
            _ => return Err(IdentityError::NotAHostKey(line.to_string())),
        };

        if !is_key_algorithm(algorithm) {
            return Err(IdentityError::Algorithm(algorithm.to_string()));
        }
        if b64.len() > MAX_KEY_B64 || b64.len() < 16 {
            return Err(IdentityError::Size(b64.len()));
        }
        let blob = STANDARD
            .decode(b64.as_bytes())
            .map_err(|_| IdentityError::Base64)?;

        Ok(Self {
            algorithm: algorithm.to_string(),
            base64: b64.to_string(),
            blob,
        })
    }

    /// The identity that goes in a manifest and a receipt: lowercase hex of the
    /// SHA-256 of the key blob.
    pub fn runner_id(&self) -> String {
        let digest = Sha256::digest(&self.blob);
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The same hash in the spelling `ssh-keygen -lf` prints, so a user can
    /// compare it against the machine itself over a channel that is not this
    /// one. Pairing is trust on first use; this string is how that trust gets
    /// checked by someone who cares to.
    pub fn fingerprint(&self) -> String {
        let digest = Sha256::digest(&self.blob);
        format!("SHA256:{}", STANDARD_NO_PAD.encode(digest))
    }

    /// The `known_hosts` line to pin this key against one host.
    ///
    /// The host pattern includes the port when it is not 22, in OpenSSH's
    /// bracket form, because that is how OpenSSH looks the entry up. A pinned
    /// key filed under the wrong pattern is a pin that silently never matches.
    pub fn known_hosts_line(&self, host: &str, port: Option<u16>) -> String {
        let pattern = match port {
            Some(p) if p != 22 => format!("[{host}]:{p}"),
            _ => host.to_string(),
        };
        format!("{pattern} {} {}\n", self.algorithm, self.base64)
    }
}

/// Is this one of the algorithms OpenSSH names a host *key* with?
///
/// Deliberately a list rather than a shape test: a field that merely *looks* like
/// an algorithm is how a comment or a stray token gets read as a key.
///
/// `rsa-sha2-256`/`rsa-sha2-512` are deliberately absent. They are *signature*
/// algorithm names and a `known_hosts` line's key type is always `ssh-rsa`, so
/// accepting them meant that if one ever appeared it would be written back into a
/// `known_hosts` line OpenSSH does not recognise as a key type: a pin that
/// silently never matches.
fn is_key_algorithm(s: &str) -> bool {
    matches!(
        s,
        "ssh-ed25519"
            | "ssh-rsa"
            | "ssh-dss"
            | "ecdsa-sha2-nistp256"
            | "ecdsa-sha2-nistp384"
            | "ecdsa-sha2-nistp521"
            | "sk-ssh-ed25519@openssh.com"
            | "sk-ecdsa-sha2-nistp256@openssh.com"
    )
}

/// Pick the key to pin out of `ssh-keyscan` output.
///
/// `ssh-keyscan` prints every key type the host offers. We prefer Ed25519:
/// it is what every current OpenSSH has, its blob is small, and preferring one
/// deterministically means `runner_id` does not depend on which line the
/// scanner happened to emit first.
pub fn preferred_host_key(scan_output: &str) -> Result<HostKey, IdentityError> {
    let keys: Vec<HostKey> = scan_output
        .lines()
        .filter_map(|l| HostKey::parse(l).ok())
        .collect();
    if keys.is_empty() {
        return Err(IdentityError::NotAHostKey(
            scan_output.lines().next().unwrap_or("").to_string(),
        ));
    }
    // Two keys of the *preferred* type that differ is one name pointing at two
    // machines, which is exactly the condition pairing must not paper over. The
    // type ranking below makes the choice deterministic across types; within a
    // type there is no right answer to pick.
    let mut preferred: Vec<&HostKey> = Vec::new();
    for k in &keys {
        if k.algorithm == "ssh-ed25519" {
            preferred.push(k);
        }
    }
    if preferred.len() > 1 && preferred.iter().any(|k| k.blob != preferred[0].blob) {
        return Err(IdentityError::NotAHostKey(
            "the scan returned two different ed25519 host keys — one name is answering as              two machines"
                .into(),
        ));
    }

    let rank = |a: &str| match a {
        "ssh-ed25519" => 0,
        "ecdsa-sha2-nistp256" | "ecdsa-sha2-nistp384" | "ecdsa-sha2-nistp521" => 1,
        "rsa-sha2-512" | "rsa-sha2-256" | "ssh-rsa" => 2,
        _ => 3,
    };
    Ok(keys
        .into_iter()
        .min_by_key(|k| rank(&k.algorithm))
        .expect("non-empty"))
}

/// Check a key we just saw against the identity we recorded at pair time.
pub fn verify(pinned_runner_id: &str, presented: &HostKey) -> Result<(), IdentityError> {
    let got = presented.runner_id();
    if got != pinned_runner_id {
        return Err(IdentityError::Mismatch {
            pinned: short(pinned_runner_id),
            presented: short(&got),
        });
    }
    Ok(())
}

/// The first twelve hex characters, for a message. Enough to tell two machines
/// apart in a sentence; the full value is what is ever compared.
pub fn short(runner_id: &str) -> String {
    runner_id.chars().take(12).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // A real Ed25519 host key blob, base64 as OpenSSH writes it.
    const ED25519: &str =
        "AAAAC3NzaC1lZDI1NTE5AAAAIIxvZlbJ7kR3D8bxAqEhpNoTfPBhZFRRLKrjTaOMHGa1";

    fn key() -> HostKey {
        HostKey::parse(&format!("ssh-ed25519 {ED25519}")).expect("parse")
    }

    #[test]
    fn a_bare_key_and_a_known_hosts_line_parse_to_the_same_key() {
        // The two shapes this code meets are the same key, and an identity that
        // depended on which one we happened to read would be no identity.
        let bare = HostKey::parse(&format!("ssh-ed25519 {ED25519}")).unwrap();
        let scanned = HostKey::parse(&format!("pi.local ssh-ed25519 {ED25519}")).unwrap();
        let commented =
            HostKey::parse(&format!("ssh-ed25519 {ED25519} root@pi")).unwrap();
        assert_eq!(bare.runner_id(), scanned.runner_id());
        assert_eq!(bare.runner_id(), commented.runner_id());
        assert_eq!(bare.algorithm, "ssh-ed25519");
    }

    #[test]
    fn the_identity_is_the_hash_of_the_blob_not_of_the_text() {
        // Hashing the text would make the identity depend on the comment and on
        // which host pattern the line carried.
        let k = key();
        let id = k.runner_id();
        assert_eq!(id.len(), 64, "sha-256 in hex");
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));

        let expected: String = Sha256::digest(&k.blob)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(id, expected);
    }

    #[test]
    fn the_fingerprint_is_the_one_ssh_keygen_prints() {
        // Same hash, OpenSSH's spelling: base64 without padding, behind
        // `SHA256:`. A user comparing this against `ssh-keygen -lf` out of band
        // is the only check pairing's trust-on-first-use ever gets.
        let f = key().fingerprint();
        assert!(f.starts_with("SHA256:"));
        assert!(!f.ends_with('='), "OpenSSH prints it unpadded");
        let b64 = &f["SHA256:".len()..];
        assert_eq!(
            STANDARD_NO_PAD.decode(b64).unwrap().len(),
            32,
            "32 bytes of SHA-256"
        );
    }

    #[test]
    fn the_two_spellings_are_the_same_hash() {
        let k = key();
        let hex = k.runner_id();
        let from_fp = STANDARD_NO_PAD
            .decode(&k.fingerprint()["SHA256:".len()..])
            .unwrap();
        let fp_hex: String = from_fp.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, fp_hex);
    }

    #[test]
    fn junk_is_not_a_host_key() {
        assert!(HostKey::parse("").is_err());
        assert!(HostKey::parse("# a comment").is_err());
        assert!(HostKey::parse("pi.local").is_err());
        assert!(HostKey::parse("ssh-ed25519").is_err());
        assert!(HostKey::parse("ssh-ed25519 not-base64!!").is_err());
        assert!(
            HostKey::parse("not-an-algorithm AAAAC3NzaC1lZDI1NTE5AAAA").is_err(),
            "an unknown first field must not be read as an algorithm"
        );
    }

    #[test]
    fn a_certificate_authority_marker_is_not_a_key_to_pin() {
        // `@cert-authority` means "trust anything this CA signs", which is a
        // different and much wider statement than pinning one machine.
        let line = format!("@cert-authority *.local ssh-ed25519 {ED25519}");
        assert!(HostKey::parse(&line).is_err());
    }

    #[test]
    fn ed25519_wins_a_scan_that_offers_several() {
        // Deterministic, so `runner_id` does not depend on scanner output order.
        let scan = format!(
            "pi.local ssh-rsa {ED25519}\npi.local ssh-ed25519 {ED25519}\n\
             pi.local ecdsa-sha2-nistp256 {ED25519}\n"
        );
        assert_eq!(preferred_host_key(&scan).unwrap().algorithm, "ssh-ed25519");

        let reordered = format!(
            "pi.local ecdsa-sha2-nistp256 {ED25519}\npi.local ssh-ed25519 {ED25519}\n"
        );
        assert_eq!(
            preferred_host_key(&reordered).unwrap().algorithm,
            "ssh-ed25519"
        );
    }

    #[test]
    fn two_different_keys_of_the_preferred_type_are_refused() {
        // One name answering as two machines. Deterministically picking the
        // first would pin one of them and never say the other existed.
        let other = "AAAAC3NzaC1lZDI1NTE5AAAAIH0nQmVSy0MoLnEHwRuLPXCUZAJXSVZ3ExAmpleKeyAA";
        let scan = format!("h ssh-ed25519 {ED25519}\nh ssh-ed25519 {other}\n");
        assert!(preferred_host_key(&scan).is_err());

        // The same key twice is not a conflict. A scan may legitimately repeat.
        let dup = format!("h ssh-ed25519 {ED25519}\nh ssh-ed25519 {ED25519}\n");
        assert!(preferred_host_key(&dup).is_ok());
    }

    #[test]
    fn a_signature_algorithm_is_not_a_host_key_type() {
        // `rsa-sha2-512` names a signature, not a key. Accepting it would write
        // a known_hosts line OpenSSH does not read as a key type: a pin that
        // silently never matches.
        assert!(HostKey::parse(&format!("rsa-sha2-512 {ED25519}")).is_err());
        assert!(HostKey::parse(&format!("ssh-rsa {ED25519}")).is_ok());
    }

    #[test]
    fn a_scan_with_only_comments_is_not_a_key() {
        assert!(preferred_host_key("# pi.local:22 SSH-2.0-OpenSSH_9.6\n").is_err());
    }

    #[test]
    fn the_known_hosts_pattern_carries_a_non_default_port() {
        // OpenSSH looks up `[host]:port` when the port is not 22. Filed under
        // the bare host, the pin would silently never match.
        let k = key();
        assert!(k.known_hosts_line("pi.local", None).starts_with("pi.local ssh-ed25519 "));
        assert!(k.known_hosts_line("pi.local", Some(22)).starts_with("pi.local ssh-ed25519 "));
        assert!(
            k.known_hosts_line("pi.local", Some(2222))
                .starts_with("[pi.local]:2222 ssh-ed25519 ")
        );
        assert!(k.known_hosts_line("pi.local", None).ends_with('\n'));
    }

    #[test]
    fn a_different_key_is_a_different_machine() {
        let pinned = key().runner_id();
        let other = HostKey::parse(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIH0nQmVSy0MoLnEHwRuLPXCUZAJXSVZ3ExAmpleKeyAA",
        )
        .unwrap();
        match verify(&pinned, &other) {
            Err(IdentityError::Mismatch { .. }) => {}
            other => panic!("expected Mismatch, got {other:?}"),
        }
        assert!(verify(&pinned, &key()).is_ok());
    }
}
