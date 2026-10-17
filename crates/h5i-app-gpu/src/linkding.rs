//! Host parser/packing; original predicate remains the independent oracle.
use crate::linkding_original::{self as original, BookmarkFacts};
#[path = "../../../examples/app/parallel/kernel/src/linkding.rs"]
pub mod shared;
#[derive(Clone)]
pub struct Query {
    pub nodes: Vec<u32>,
    pub text: Vec<u8>,
    pub root: usize,
    pub flags: u32,
}
impl Query {
    pub fn new(s: &str, lax: bool) -> Self {
        let p = original::parse_search(&s.as_bytes().to_vec());
        let mut q = Self {
            nodes: vec![],
            text: vec![],
            root: p.root,
            flags: p.failed as u32 | ((p.empty as u32) << 1) | ((lax as u32) << 2),
        };
        if p.nodes.len() > shared::MAX_NODES {
            q.flags |= 8;
            return q;
        }
        for n in p.nodes {
            q.nodes.extend([
                n.kind as u32,
                q.text.len() as u32,
                n.text.len() as u32,
                n.left as u32,
                n.right as u32,
            ]);
            q.text.extend(n.text);
        }
        q
    }
}
#[derive(Clone, Default)]
pub struct Rows {
    pub fields: Vec<u8>,
    pub lengths: Vec<u32>,
    pub tags: Vec<u64>,
    pub meta: Vec<u32>,
}
impl Rows {
    pub fn push(&mut self, b: &BookmarkFacts) {
        let mut bad = b.tag_names.len() > 8;
        for f in [&b.title, &b.description, &b.notes, &b.url] {
            bad |= f.len() > 128;
            self.lengths.push(f.len() as u32);
            let mut buf = [0; 128];
            let n = f.len().min(128);
            buf[..n].copy_from_slice(&f[..n]);
            self.fields.extend(buf);
        }
        let mut tags = [0; 8];
        for (i, t) in b.tag_names.iter().take(8).enumerate() {
            tags[i] = shared::hash(t);
        }
        self.tags.extend(tags);
        self.meta.extend([
            b.unread as u32,
            b.untagged as u32,
            if bad { 9 } else { b.tag_names.len() as u32 },
        ]);
    }
    pub fn len(&self) -> usize {
        self.meta.len() / 3
    }
    pub fn at(&self, q: &Query, i: usize) -> u32 {
        if i >= 1_000_000
            || i >= self.fields.len() / 512
            || i >= self.lengths.len() / 4
            || i >= self.tags.len() / 8
            || i >= self.meta.len() / 3
        {
            return 2;
        }
        shared::evaluate(
            &self.fields[i * 512..(i + 1) * 512],
            &self.lengths[i * 4..(i + 1) * 4],
            &self.tags[i * 8..(i + 1) * 8],
            &self.meta[i * 3..(i + 1) * 3],
            &q.nodes,
            &q.text,
            q.root,
            q.flags,
        )
    }
    pub fn cpu(&self, q: &Query, threads: usize) -> Vec<u32> {
        let mut out = vec![0; self.len()];
        let chunk = self.len().div_ceil(threads.max(1)).max(1);
        std::thread::scope(|scope| {
            for (part, dst) in out.chunks_mut(chunk).enumerate() {
                scope.spawn(move || {
                    for (j, v) in dst.iter_mut().enumerate() {
                        *v = self.at(q, part * chunk + j);
                    }
                });
            }
        });
        out
    }
}
pub fn fixture(i: usize) -> BookmarkFacts {
    BookmarkFacts {
        title: format!(
            "Bookmark {i} {}",
            if i % 50 == 0 { "alpha" } else { "ordinary" }
        )
        .into_bytes(),
        description: b"A saved page about beta gamma delta".to_vec(),
        notes: b"epsilon zeta eta theta".to_vec(),
        url: format!("https://example.com/item/{i}").into_bytes(),
        unread: i % 2 == 0,
        untagged: i % 3 == 0,
        tag_names: if i % 3 == 0 {
            vec![]
        } else {
            vec![b"Rust".to_vec(), b"alpha".to_vec()]
        },
    }
}

/// Exact search columns/flags/tags from linkding bench/seed.sql; scaled beyond 100k.
pub fn benchmark_fixture(i: usize) -> BookmarkFacts {
    BookmarkFacts {
        title: format!("{} {i}", if i % 50 == 0 { "alpha" } else { "bookmark" }).into_bytes(),
        description: vec![],
        notes: vec![],
        url: format!("https://example.com/b/{i}").into_bytes(),
        unread: i % 7 == 0,
        untagged: false,
        tag_names: vec![
            format!("tag{}", (i - 1) % 2000).into_bytes(),
            format!("tag{}", i % 2000).into_bytes(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn layout_and_hash_order() {
        let mut b = fixture(1);
        b.title = vec![b'A'; 128];
        b.notes = vec![0, b'Z', 0];
        let mut r = Rows::default();
        r.push(&b);
        assert_eq!(&r.fields[..128], &b.title);
        assert_eq!(&r.fields[256..259], &b.notes);
        assert_eq!(
            r.lengths,
            [128, b.description.len() as u32, 3, b.url.len() as u32]
        );
        assert_eq!(r.tags[0], shared::hash(b"rust"));
        assert_eq!(r.tags[1], shared::hash(b"ALPHA"));
        let q = Query::new("#rust", false);
        assert_eq!(r.at(&q, 0), 1);
        r.tags.swap(0, 1);
        assert_eq!(r.at(&q, 0), 1);
    }
    #[test]
    fn field_boundary_and_capacity() {
        let mut b = fixture(1);
        b.title = b"a".to_vec();
        b.description = b"lpha".to_vec();
        b.notes.clear();
        b.url.clear();
        b.tag_names.clear();
        let mut r = Rows::default();
        r.push(&b);
        assert_eq!(r.at(&Query::new("alpha", false), 0), 0);
        let max = Query::new(&("not ".to_owned() + &vec!["a"; 128].join(" and ")), false);
        assert_eq!(max.nodes.len() / 5, 256);
        assert_eq!(max.flags & 8, 0);
        let over = Query::new(&vec!["a"; 129].join(" and "), false);
        assert_eq!(r.at(&over, 0), 2);
        r.lengths[0] = 129;
        assert_eq!(r.at(&Query::new("", false), 0), 2);
    }
}
