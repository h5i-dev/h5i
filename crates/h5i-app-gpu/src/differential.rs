//! Reusable conformance suite: compares both output columns exactly.
use crate::{
    arena::{MAX_ROWS, SearchArena},
    batch::*,
};
use parallel_kernel::Bookmark;
pub fn fixture(n: usize, random: bool) -> Batch {
    let mut b = Batch {
        text: vec![0; n * 512],
        patterns: vec![0; n * 64],
        displays: vec![0; n * 64],
        tags: vec![3; n],
        members: vec![5; n],
        powers: vec![50; n],
        terms: vec![0; 128],
        count: 8,
        any: false,
        negated: 0,
        required: 1,
        minimum: 1,
        maximum: 10,
        enabled: true,
        require_display: true,
        n,
    };
    for t in 0..8 {
        b.terms[t * 16..t * 16 + 12].copy_from_slice(b"nonmatchword");
    }
    let mut seed = 0x123456789abcdefu64;
    for i in 0..n {
        for j in 0..512 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            b.text[i * 512 + j] = if random {
                b'a' + (seed % 26) as u8
            } else {
                b'a' + (j % 23) as u8
            };
        }
        if random {
            let text=b"hello Alice, a notification message with enough content to exercise wildcard matching";
            if i % 3 == 0 {
                b.text[i * 512..(i + 1) * 512].fill(0);
                b.text[i * 512..i * 512 + text.len()].copy_from_slice(text);
            }
            b.tags[i] = seed & 7;
            b.members[i] = seed % 14;
            b.powers[i] = seed % 70;
        }
        b.patterns[i * 64..i * 64 + 23].copy_from_slice(b"h*notification*matching");
        b.displays[i * 64..i * 64 + 5].copy_from_slice(b"Alice");
        if random {
            let pattern = &mut b.patterns[i * 64..i * 64 + 64];
            match i % 6 {
                0 => pattern.fill(b'*'),
                1 => pattern.fill(b'?'),
                2 => {
                    pattern.fill(0);
                    pattern[..3].copy_from_slice(b"a?*");
                }
                3 => pattern.fill(0),
                _ => {}
            }
            if i % 2 == 1 {
                b.displays[i * 64..i * 64 + 64].fill(0);
                b.displays[i * 64] = b'a';
            }
        }
    }
    b
}
pub fn check<B: Backend>(backend: &mut B) -> Result<(), B::Error> {
    for n in [0, 1, 127, 128, 129, 1025] {
        let mut b = fixture(n, true);
        b.terms.fill(0);
        b.terms[0] = b'a';
        b.terms[16] = b'e';
        b.terms[32] = b'z';
        for decision in [Decision::Search, Decision::Push] {
            for k in 0..5 {
                b.any = k % 2 == 1;
                b.negated = k;
                b.count = if k == 4 { 9 } else { k as usize };
                b.required = k as u64;
                b.enabled = k != 3;
                b.require_display = k % 2 == 0;
                assert_eq!(backend.run(decision, &b)?, Cpu.run(decision, &b).unwrap());
            }
            // Appending a row with missing inputs must write an explicit error.
            b.n = n + 1;
            assert_eq!(backend.run(decision, &b)?, Cpu.run(decision, &b).unwrap());
            b.n = n;
        }
    }
    // Exact maximum and first row beyond it; use cheap decisions here.
    let mut boundary = fixture(MAX_ROWS + 1, false);
    boundary.count = 0;
    boundary.required = 0;
    boundary.enabled = false;
    for decision in [Decision::Search, Decision::Push] {
        let out = backend.run(decision, &boundary)?;
        assert_eq!(out, Cpu.run(decision, &boundary).unwrap());
        assert_eq!(out.errors[MAX_ROWS - 1], 0);
        assert_eq!(out.errors[MAX_ROWS], 1);
    }
    drop(boundary);
    let mut malformed = fixture(3, true);
    malformed.terms.truncate(127);
    malformed.patterns.truncate(65);
    malformed.displays.truncate(63);
    for decision in [Decision::Search, Decision::Push] {
        assert_eq!(
            backend.run(decision, &malformed)?,
            Cpu.run(decision, &malformed).unwrap()
        );
    }
    let b = fixture(257, true);
    for decision in [Decision::Search, Decision::Push] {
        let whole = backend.run(decision, &b)?;
        let mut joined = Output {
            values: vec![],
            errors: vec![],
        };
        for (start, end) in [(0, 128), (128, 257)] {
            let mut part = b.clone();
            part.n = end - start;
            part.text = b.text[start * 512..end * 512].to_vec();
            part.patterns = b.patterns[start * 64..end * 64].to_vec();
            part.displays = b.displays[start * 64..end * 64].to_vec();
            part.tags = b.tags[start..end].to_vec();
            part.members = b.members[start..end].to_vec();
            part.powers = b.powers[start..end].to_vec();
            let out = backend.run(decision, &part)?;
            joined.values.extend(out.values);
            joined.errors.extend(out.errors);
        }
        assert_eq!(whole, joined);
    }
    let rows = vec![
        Bookmark {
            id: 91,
            text: vec![b'x'; 512],
            tags: 7,
        },
        Bookmark {
            id: 2,
            text: b"a\0b".to_vec(),
            tags: 1,
        },
    ];
    let soa = SearchArena::pack(&rows, 2).unwrap();
    assert_eq!(soa.unpack(), rows);
    assert_eq!(soa.ids, vec![91, 2]);
    assert!(SearchArena::pack(&rows, 1).is_err());
    assert!(SearchArena::pack(&rows, MAX_ROWS + 1).is_err());
    Ok(())
}
#[cfg(test)]
mod tests {
    #[test]
    fn cpu_backend_conformance() {
        super::check(&mut super::Cpu).unwrap();
    }
}
