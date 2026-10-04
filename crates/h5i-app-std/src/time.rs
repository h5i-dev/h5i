//! Expiry checks over whole seconds, in `u64` without overflow.
//!
//! The comparisons are the ones JWT validation and session checks make,
//! rearranged so no sum can wrap: `expired` is `exp + leeway < now`, never
//! `exp < now - leeway` underflowing.

/// `exp + leeway < now`: the deadline passed, even allowing `leeway`.
pub fn expired(exp: u64, now: u64, leeway: u64) -> bool {
    now > leeway && exp < now - leeway
}

/// `now + leeway < nbf`: not valid yet, even allowing `leeway`.
pub fn not_yet_valid(nbf: u64, now: u64, leeway: u64) -> bool {
    nbf > leeway && now < nbf - leeway
}

/// `now < start + ttl`: still inside a window of `ttl` seconds from `start`.
pub fn within(start: u64, now: u64, ttl: u64) -> bool {
    now < start || now - start < ttl
}

/// Microseconds to whole seconds, rounded up.
pub fn secs_ceil(micros: u64) -> u64 {
    let s = micros / 1_000_000;
    if micros % 1_000_000 == 0 { s } else { s + 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_wide_arithmetic() {
        let xs = [0u64, 1, 2, 59, 60, 61, 1_000_000, 1_000_001, u64::MAX - 1, u64::MAX];
        for a in xs {
            assert_eq!(secs_ceil(a) as u128, (a as u128).div_ceil(1_000_000));
            for b in xs {
                for c in xs {
                    let (a2, b2, c2) = (a as u128, b as u128, c as u128);
                    assert_eq!(expired(a, b, c), a2 + c2 < b2);
                    assert_eq!(not_yet_valid(a, b, c), b2 + c2 < a2);
                    assert_eq!(within(a, b, c), b2 < a2 + c2);
                }
            }
        }
    }
}
