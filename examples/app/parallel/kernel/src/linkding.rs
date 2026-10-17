//! Bounded, allocation-free port of linkding's postfix AST evaluator.
//! Node words: kind, text offset, text length, left, right (all u32).
pub const MAX_NODES: usize = 256;
pub const WIDTH: usize = 128;
pub const TAGS: usize = 8;
pub fn fold(c: u8) -> u8 {
    if c >= b'A' && c <= b'Z' { c + 32 } else { c }
}
pub fn hash(bytes: &[u8]) -> u64 {
    let mut h = 14695981039346656037u64;
    let mut i = 0;
    while i < bytes.len() {
        h = (h ^ fold(bytes[i]) as u64).wrapping_mul(1099511628211);
        i += 1;
    }
    h
}
fn contains(hay: &[u8], needle: &[u8]) -> bool {
    if needle.len() > hay.len() {
        return false;
    }
    let mut i = 0;
    while i <= hay.len() - needle.len() {
        let mut j = 0;
        let mut ok = true;
        while j < needle.len() {
            if fold(hay[i + j]) != fold(needle[j]) {
                ok = false;
            }
            j += 1;
        }
        if ok {
            return true;
        }
        i += 1;
    }
    false
}
fn equal(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && contains(a, b)
}
/// Returns 0/1 for a match, 2 for a capacity or malformed-layout error.
pub fn evaluate(
    fields: &[u8],
    lengths: &[u32],
    tags: &[u64],
    meta: &[u32],
    nodes: &[u32],
    text: &[u8],
    root: usize,
    flags: u32,
) -> u32 {
    if fields.len() != 512
        || lengths.len() != 4
        || tags.len() != TAGS
        || meta.len() != 3
        || meta[2] > TAGS as u32
        || nodes.len() % 5 != 0
        || nodes.len() / 5 > MAX_NODES
    {
        return 2;
    }
    let mut f = 0;
    while f < 4 {
        if lengths[f] > WIDTH as u32 {
            return 2;
        }
        f += 1;
    }
    if flags & 8 != 0 {
        return 2;
    }
    if flags & 1 != 0 {
        return 0;
    }
    if flags & 2 != 0 {
        return 1;
    }
    let mut vals = [false; MAX_NODES];
    let mut i = 0;
    while i < nodes.len() / 5 {
        let k = nodes[i * 5];
        let off = nodes[i * 5 + 1] as usize;
        let len = nodes[i * 5 + 2] as usize;
        let l = nodes[i * 5 + 3] as usize;
        let r = nodes[i * 5 + 4] as usize;
        if off > text.len() || len > text.len() - off {
            return 2;
        }
        let word = &text[off..off + len];
        let mut value = false;
        if k == 1 || k == 2 {
            if k == 1 {
                let mut j = 0;
                while j < 4 {
                    if contains(&fields[j * WIDTH..j * WIDTH + lengths[j] as usize], word) {
                        value = true;
                    }
                    j += 1;
                }
            }
            if k == 2 || flags & 4 != 0 {
                let h = hash(word);
                let mut j = 0;
                while j < meta[2] as usize {
                    if tags[j] == h {
                        value = true;
                    }
                    j += 1;
                }
            }
        } else if k == 3 {
            value = if equal(word, b"unread") {
                meta[0] != 0
            } else if equal(word, b"untagged") {
                meta[1] != 0
            } else {
                true
            };
        } else if k == 4 || k == 5 {
            if l >= i || r >= i {
                return 2;
            }
            value = if k == 4 {
                vals[l] && vals[r]
            } else {
                vals[l] || vals[r]
            };
        } else if k == 6 {
            if l >= i {
                return 2;
            }
            value = !vals[l];
        } else {
            return 2;
        }
        vals[i] = value;
        i += 1;
    }
    if root >= i {
        return 2;
    }
    vals[root] as u32
}
