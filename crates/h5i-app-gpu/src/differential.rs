//! Reusable conformance suite: compares both output columns exactly.
use crate::{
    arena::{MAX_ROWS, SearchArena},
    batch::*,
    layout::{MAX_TERMS, ROW_BYTES, RULE_BYTES, TERM_SLOT, TERMS_BYTES},
};
use parallel_kernel::Bookmark;
pub fn fixture(n: usize, random: bool) -> Batch {
    let mut b = Batch {
        text: vec![0; n * ROW_BYTES],
        patterns: vec![0; n * RULE_BYTES],
        displays: vec![0; n * RULE_BYTES],
        tags: vec![3; n],
        members: vec![5; n],
        powers: vec![50; n],
        terms: vec![0; TERMS_BYTES],
        count: MAX_TERMS,
        any: false,
        negated: 0,
        required: 1,
        minimum: 1,
        maximum: 10,
        enabled: true,
        require_display: true,
        n,
    };
    for t in 0..MAX_TERMS {
        b.terms[t * TERM_SLOT..t * TERM_SLOT + 12].copy_from_slice(b"nonmatchword");
    }
    let mut seed = 0x123456789abcdefu64;
    for i in 0..n {
        for j in 0..ROW_BYTES {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            b.text[i * ROW_BYTES + j] = if random {
                b'a' + (seed % 26) as u8
            } else {
                b'a' + (j % 23) as u8
            };
        }
        if random {
            let text=b"hello Alice, a notification message with enough content to exercise wildcard matching";
            if i % 3 == 0 {
                b.text[i * ROW_BYTES..(i + 1) * ROW_BYTES].fill(0);
                b.text[i * ROW_BYTES..i * ROW_BYTES + text.len()].copy_from_slice(text);
            }
            b.tags[i] = seed & 7;
            b.members[i] = seed % 14;
            b.powers[i] = seed % 70;
        }
        b.patterns[i * RULE_BYTES..i * RULE_BYTES + 23].copy_from_slice(b"h*notification*matching");
        b.displays[i * RULE_BYTES..i * RULE_BYTES + 5].copy_from_slice(b"Alice");
        if random {
            let pattern = &mut b.patterns[i * RULE_BYTES..i * RULE_BYTES + RULE_BYTES];
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
                b.displays[i * RULE_BYTES..i * RULE_BYTES + RULE_BYTES].fill(0);
                b.displays[i * RULE_BYTES] = b'a';
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
        b.terms[TERM_SLOT] = b'e';
        b.terms[2 * TERM_SLOT] = b'z';
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
    malformed.terms.truncate(TERMS_BYTES - 1);
    malformed.patterns.truncate(RULE_BYTES + 1);
    malformed.displays.truncate(RULE_BYTES - 1);
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
            part.text = b.text[start * ROW_BYTES..end * ROW_BYTES].to_vec();
            part.patterns = b.patterns[start * RULE_BYTES..end * RULE_BYTES].to_vec();
            part.displays = b.displays[start * RULE_BYTES..end * RULE_BYTES].to_vec();
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
            text: vec![b'x'; ROW_BYTES],
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
    let events = vec![
        parallel_kernel::Event {
            id: 8,
            content: b"hello Alice".to_vec(),
            display: b"Alice".to_vec(),
            members: 5,
            power: 50,
        },
        parallel_kernel::Event {
            id: 1,
            content: vec![b'x'; ROW_BYTES],
            display: vec![b'x'; RULE_BYTES],
            members: 1,
            power: 0,
        },
    ];
    let soa = crate::arena::PushArena::pack(&events, 2).unwrap();
    assert_eq!(soa.unpack(), events);
    assert_eq!(soa.ids, vec![8, 1]);
    assert!(crate::arena::PushArena::pack(&events, 1).is_err());
    let rule = parallel_kernel::Rule {
        id: 9,
        pattern: b"h*".to_vec(),
        minimum: 1,
        maximum: 10,
        required: 20,
        enabled: true,
        display: true,
    };
    let mut b = fixture(2, false);
    b.text = soa.content;
    b.displays = soa.display;
    b.members = soa.members;
    b.powers = soa.powers;
    b.minimum = rule.minimum;
    b.maximum = rule.maximum;
    b.required = rule.required;
    b.enabled = rule.enabled;
    b.require_display = rule.display;
    b.patterns.fill(0);
    for row in b.patterns.chunks_exact_mut(RULE_BYTES) {
        row[..rule.pattern.len()].copy_from_slice(&rule.pattern);
    }
    assert_eq!(
        backend.run(Decision::Push, &b)?,
        Cpu.run(Decision::Push, &b).unwrap()
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    #[test]
    fn cpu_backend_conformance() {
        super::check(&mut super::Cpu).unwrap();
    }
}
