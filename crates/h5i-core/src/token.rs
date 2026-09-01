//! Session tokens: the one place h5i mints a secret.

use crate::error::H5iError;

/// A fresh `n`-byte token, lowercase hex (so `2 * n` characters).
///
/// Fails rather than falling back to a weaker source: a host that cannot
/// produce entropy should get no listener at all, not a guessable one.
pub fn hex(n: usize) -> Result<String, H5iError> {
    let mut raw = vec![0u8; n];
    getrandom::fill(&mut raw).map_err(|e| {
        H5iError::Metadata(format!(
            "cannot mint a session token: no OS entropy ({e}) — refusing to fall back to a \
             guessable one (fail-closed)"
        ))
    })?;
    let mut out = String::with_capacity(n * 2);
    for b in raw {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mints_hex_of_the_requested_width() {
        let t = hex(16).expect("OS entropy");
        assert_eq!(t.len(), 32);
        assert!(t.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn every_token_is_distinct() {
        // Not a randomness test. A collision here means the source is wired up
        // wrong (a constant, a reused buffer), which is the failure that would
        // actually ship.
        let mut seen = std::collections::HashSet::new();
        for _ in 0..256 {
            assert!(seen.insert(hex(16).expect("OS entropy")), "repeated token");
        }
    }
}
