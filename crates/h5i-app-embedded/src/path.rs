//! Route patterns: `/relay/{id}/state`. A `{...}` segment matches one
//! non-empty path segment; every other segment matches itself exactly.

use crate::MAX_PARAMS;

/// If `path` matches `pattern`, the number of parameters, with their spans
/// (into `path`) in `out`.
pub fn matches(pattern: &[u8], path: &[u8], out: &mut [(usize, usize); MAX_PARAMS]) -> Option<usize> {
    let mut pi = 0;
    let mut si = 0;
    let mut n = 0;
    loop {
        let pend = seg_end(pattern, pi);
        let send = seg_end(path, si);
        let pseg = &pattern[pi..pend];
        let sseg = &path[si..send];
        if pseg.len() >= 2 && pseg[0] == b'{' && pseg[pseg.len() - 1] == b'}' {
            if sseg.is_empty() || n == MAX_PARAMS {
                return None;
            }
            out[n] = (si, send);
            n += 1;
        } else if pseg != sseg {
            return None;
        }
        let pdone = pend == pattern.len();
        let sdone = send == path.len();
        if pdone || sdone {
            return if pdone && sdone { Some(n) } else { None };
        }
        pi = pend + 1;
        si = send + 1;
    }
}

fn seg_end(s: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < s.len() && s[i] != b'/' {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: &str, s: &str) -> Option<usize> {
        matches(p.as_bytes(), s.as_bytes(), &mut [(0, 0); MAX_PARAMS])
    }

    #[test]
    fn patterns() {
        assert_eq!(m("/", "/"), Some(0));
        assert_eq!(m("/status", "/status"), Some(0));
        assert_eq!(m("/status", "/status/"), None);
        assert_eq!(m("/status", "/stat"), None);
        assert_eq!(m("/relay/{id}", "/relay/3"), Some(1));
        assert_eq!(m("/relay/{id}", "/relay/"), None);
        assert_eq!(m("/relay/{id}", "/relay/3/x"), None);
        assert_eq!(m("/a/{x}/b/{y}", "/a/1/b/22"), Some(2));
        let mut out = [(0, 0); MAX_PARAMS];
        assert_eq!(matches(b"/a/{x}/b/{y}", b"/a/1/b/22", &mut out), Some(2));
        assert_eq!(out[..2], [(3, 4), (7, 9)]);
    }
}
