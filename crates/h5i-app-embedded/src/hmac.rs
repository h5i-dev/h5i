//! [`HmacAuth`]: requests signed with a pre-shared key, for links others can
//! read (Wi-Fi, Ethernet). The key never crosses the link, and a captured
//! request cannot be replayed. Trusted, tested.
//!
//! ```text
//! Authorization: H5i-HMAC <boot>:<counter>:<mac>
//!
//! boot     8 lowercase hex digits: the device's boot id
//! counter  decimal, greater than every counter accepted since that boot
//! mac      64 lowercase hex digits: HMAC-SHA256(key, message)
//! message  "h5i-hmac\n" boot "\n" counter "\n" method "\n" target "\n" body
//! ```
//!
//! The target is the request target as sent (path and query). A refusal is a
//! 401 whose `WWW-Authenticate: H5i-HMAC boot=<boot>,last=<counter>` tells
//! the client the current boot id and the last accepted counter. A client
//! that keeps no state can use the time in microseconds as its counter.
//!
//! The boot id must differ from one boot to the next (take it from a
//! hardware RNG), or requests from an earlier boot verify again. One key
//! serves one client at a time: two clients sharing it race on the counter.
//!
//! What this does not give: confidentiality (requests and replies are in
//! the clear) and authenticity of replies. For those, terminate TLS in front
//! of the device.

use core::cell::Cell;

use critical_section::Mutex;
use h5i_app_core::Kernel;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::kernel::Authenticator;
use crate::{Request, Response};

pub const SCHEME: &[u8] = b"H5i-HMAC";

/// Verifies [`SCHEME`] requests and gives one fixed principal.
pub struct HmacAuth<P> {
    key: &'static [u8],
    principal: P,
    boot: u32,
    /// The last accepted counter.
    last: Mutex<Cell<u64>>,
}

impl<P> HmacAuth<P> {
    /// `boot` must differ on every boot; see the module documentation.
    pub const fn new(key: &'static [u8], principal: P, boot: u32) -> Self {
        HmacAuth { key, principal, boot, last: Mutex::new(Cell::new(0)) }
    }

    pub fn boot(&self) -> u32 {
        self.boot
    }

    fn challenge(&self) -> Response {
        let mut v = [0u8; 64];
        let mut n = 0;
        for &b in b"H5i-HMAC boot=" {
            v[n] = b;
            n += 1;
        }
        n += hex8(self.boot, &mut v[n..]);
        for &b in b",last=" {
            v[n] = b;
            n += 1;
        }
        let last = critical_section::with(|cs| self.last.borrow(cs).get());
        n += dec(last, &mut v[n..]);
        Response::status(401).header_copy(b"WWW-Authenticate", &v[..n])
    }
}

impl<K: Kernel> Authenticator<K> for HmacAuth<K::Principal> {
    #[allow(clippy::result_large_err)]
    fn authenticate(&self, req: &Request<'_>) -> Result<K::Principal, Response> {
        let Some(cred) = req.header(b"authorization").and_then(|v| v.strip_prefix(b"H5i-HMAC ")) else {
            return Err(self.challenge());
        };
        let Some((boot, counter, mac)) = split3(cred) else {
            return Err(self.challenge());
        };
        let mut want_boot = [0u8; 8];
        hex8(self.boot, &mut want_boot);
        let Some(n) = parse_counter(counter) else {
            return Err(self.challenge());
        };
        let mut tag = [0u8; 32];
        if boot != want_boot || !unhex(mac, &mut tag) {
            return Err(self.challenge());
        }
        let m = message(self.key, boot, counter, req.method_bytes(), req.target(), req.body);
        if m.verify_slice(&tag).is_err() {
            return Err(self.challenge());
        }
        // Verified: accept only a counter above the last one, atomically.
        let fresh = critical_section::with(|cs| {
            let last = self.last.borrow(cs);
            if n > last.get() {
                last.set(n);
                true
            } else {
                false
            }
        });
        if fresh { Ok(self.principal.clone()) } else { Err(self.challenge()) }
    }
}

fn message(key: &[u8], boot: &[u8], counter: &[u8], method: &[u8], target: &[u8], body: &[u8]) -> Hmac<Sha256> {
    // HMAC takes keys of any length.
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("any key length");
    for part in [&b"h5i-hmac"[..], boot, counter, method, target] {
        m.update(part);
        m.update(b"\n");
    }
    m.update(body);
    m
}

/// The `Authorization` value for a request, written into `out` (at least
/// 103 bytes): what a client sends. Its length.
pub fn authorization(
    key: &[u8],
    boot: u32,
    counter: u64,
    method: &[u8],
    target: &[u8],
    body: &[u8],
    out: &mut [u8],
) -> usize {
    let mut b = [0u8; 8];
    hex8(boot, &mut b);
    let mut c = [0u8; 20];
    let cn = dec(counter, &mut c);
    let tag = message(key, &b, &c[..cn], method, target, body).finalize().into_bytes();
    let mut n = 0;
    let mut put = |s: &[u8]| {
        out[n..n + s.len()].copy_from_slice(s);
        n += s.len();
    };
    put(b"H5i-HMAC ");
    put(&b);
    put(b":");
    put(&c[..cn]);
    put(b":");
    for byte in tag {
        put(&[HEX[(byte >> 4) as usize], HEX[(byte & 15) as usize]]);
    }
    n
}

const HEX: &[u8; 16] = b"0123456789abcdef";

fn hex8(v: u32, out: &mut [u8]) -> usize {
    for i in 0..8 {
        out[i] = HEX[((v >> (28 - 4 * i)) & 15) as usize];
    }
    8
}

fn dec(mut v: u64, out: &mut [u8]) -> usize {
    let mut d = [0u8; 20];
    let mut k = 0;
    loop {
        d[k] = b'0' + (v % 10) as u8;
        k += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    for i in 0..k {
        out[i] = d[k - 1 - i];
    }
    k
}

fn split3(s: &[u8]) -> Option<(&[u8], &[u8], &[u8])> {
    let mut it = s.splitn(3, |&c| c == b':');
    Some((it.next()?, it.next()?, it.next()?))
}

/// Decimal, no leading zeros, above 0, within `u64`.
fn parse_counter(s: &[u8]) -> Option<u64> {
    if s.is_empty() || s.len() > 20 || s[0] == b'0' || !s.iter().all(u8::is_ascii_digit) {
        return None;
    }
    s.iter().try_fold(0u64, |v, &c| v.checked_mul(10)?.checked_add((c - b'0') as u64))
}

/// 64 lowercase hex digits into 32 bytes.
fn unhex(s: &[u8], out: &mut [u8; 32]) -> bool {
    if s.len() != 64 {
        return false;
    }
    for i in 0..32 {
        let h = |c: u8| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        };
        match (h(s[2 * i]), h(s[2 * i + 1])) {
            (Some(a), Some(b)) => out[i] = a * 16 + b,
            _ => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_answer() {
        // python3 -c 'import hmac,hashlib; print(hmac.new(b"k", b"h5i-hmac\n0000002a\n7\nPUT\n/mode?x=1\nheat", hashlib.sha256).hexdigest())'
        let mut out = [0u8; 128];
        let n = authorization(b"k", 42, 7, b"PUT", b"/mode?x=1", b"heat", &mut out);
        let s = core::str::from_utf8(&out[..n]).unwrap();
        assert_eq!(s, "H5i-HMAC 0000002a:7:07c1dcab9d2859a99d8764a6e4e796221d387b5639834469256e8d6929c5e86b");
    }

    #[test]
    fn counters() {
        assert_eq!(parse_counter(b"1"), Some(1));
        assert_eq!(parse_counter(b"18446744073709551615"), Some(u64::MAX));
        assert_eq!(parse_counter(b"18446744073709551616"), None);
        assert_eq!(parse_counter(b"0"), None);
        assert_eq!(parse_counter(b"01"), None);
        assert_eq!(parse_counter(b""), None);
        assert_eq!(parse_counter(b"1a"), None);
        let mut out = [0u8; 20];
        let n = dec(u64::MAX, &mut out);
        assert_eq!(&out[..n], b"18446744073709551615");
        let mut h = [0u8; 8];
        hex8(0xdeadbeef, &mut h);
        assert_eq!(&h, b"deadbeef");
    }
}
