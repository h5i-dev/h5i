//! A hash map in the Aeneas subset: `Vec<Vec<(K, V)>>` buckets.
//!
//! Each bucket is a `map`-style association list, and `key_hash(k) % n`
//! picks the bucket of key `k`. Lookups compare keys with `==`, so a hash
//! only decides where to look: any `key_hash`, colliding or not, gives the
//! same results, and a poor one only makes buckets longer. That is why
//! [`KeyHash`] carries no law, and why the specs in `proofs/StdSpecs.lean`
//! state every operation over the association list the buckets hold.
//!
//! The bucket count is fixed when the map is made: there is no rehash. Size
//! the map with [`HashMap::with_capacity`] from the rows it will hold; a map
//! that outgrows its buckets stays correct and gets slower.

/// The hash a [`HashMap`] buckets keys by. Unlike `std::hash::Hash` it has no
/// hasher and no random seed, so it extracts to a plain function.
///
/// For a composite key, use a struct with `#[derive(PartialEq)]` rather than
/// a tuple: Aeneas has no model of the tuple `==`, so no spec could compare
/// tuple keys. Its `key_hash` can combine the fields' hashes, e.g.
/// `self.a.key_hash().wrapping_mul(0x100000001b3) ^ self.b.key_hash()`.
pub trait KeyHash {
    fn key_hash(&self) -> u64;
}

impl KeyHash for u8 {
    fn key_hash(&self) -> u64 {
        *self as u64
    }
}

impl KeyHash for u32 {
    fn key_hash(&self) -> u64 {
        *self as u64
    }
}

impl KeyHash for u64 {
    fn key_hash(&self) -> u64 {
        *self
    }
}

impl KeyHash for usize {
    fn key_hash(&self) -> u64 {
        *self as u64
    }
}

impl KeyHash for bool {
    fn key_hash(&self) -> u64 {
        if *self { 1 } else { 0 }
    }
}

/// FNV-1a.
impl KeyHash for Vec<u8> {
    fn key_hash(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut i = 0;
        while i < self.len() {
            h = (h ^ self[i] as u64).wrapping_mul(0x0000_0100_0000_01b3);
            i += 1;
        }
        h
    }
}

/// The bucket count of [`HashMap::new`].
pub const DEFAULT_BUCKETS: usize = 16;

pub struct HashMap<K, V> {
    buckets: Vec<Vec<(K, V)>>,
    len: usize,
}

/// The bucket of `k` among `n`.
fn bucket_of<K: KeyHash>(n: usize, k: &K) -> usize {
    (k.key_hash() % n as u64) as usize
}

/// The index of the entry with key `k` in bucket `b`.
fn find<K: PartialEq, V>(b: &Vec<(K, V)>, k: &K) -> Option<usize> {
    let mut i = 0;
    while i < b.len() {
        if b[i].0 == *k {
            return Some(i);
        }
        i += 1;
    }
    None
}

impl<K, V> HashMap<K, V> {
    /// An empty map with `DEFAULT_BUCKETS` buckets.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_BUCKETS)
    }

    /// An empty map with `n` buckets (one if `n` is 0).
    pub fn with_capacity(n: usize) -> Self {
        let nb = if n == 0 { 1 } else { n };
        let mut buckets = Vec::new();
        let mut i = 0;
        while i < nb {
            buckets.push(Vec::new());
            i += 1;
        }
        HashMap { buckets, len: 0 }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<K, V> Default for HashMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: PartialEq + KeyHash, V> HashMap<K, V> {
    /// `m.get(k)`.
    pub fn get(&self, k: &K) -> Option<&V> {
        let b = &self.buckets[bucket_of(self.buckets.len(), k)];
        match find(b, k) {
            Some(i) => Some(&b[i].1),
            None => None,
        }
    }

    /// `m.contains_key(k)`.
    pub fn contains_key(&self, k: &K) -> bool {
        find(&self.buckets[bucket_of(self.buckets.len(), k)], k).is_some()
    }

    /// `m.insert(k, v)`: the value `k` had, if any.
    pub fn insert(&mut self, k: K, v: V) -> Option<V> {
        let b = bucket_of(self.buckets.len(), &k);
        match find(&self.buckets[b], &k) {
            // Both arms move `k` and `v`: a value moved in one arm only gets a
            // drop flag, which Charon extracts differently in this crate and in
            // a kernel that includes it, and the specs must fit both.
            Some(i) => Some(std::mem::replace(&mut self.buckets[b][i], (k, v)).1),
            None => {
                self.buckets[b].push((k, v));
                self.len += 1;
                None
            }
        }
    }
}

impl<K: PartialEq + KeyHash + Clone, V: Clone> HashMap<K, V> {
    /// `m.remove(k)`: the value `k` had, if any.
    pub fn remove(&mut self, k: &K) -> Option<V> {
        let b = bucket_of(self.buckets.len(), k);
        match find(&self.buckets[b], k) {
            None => None,
            Some(i) => {
                let old = self.buckets[b][i].1.clone();
                let out = crate::map::remove(&self.buckets[b], k);
                self.buckets[b] = out;
                self.len -= 1;
                Some(old)
            }
        }
    }

    /// A map of `entries`, sized to them; a later entry for a key replaces
    /// an earlier one.
    pub fn from_vec(entries: &Vec<(K, V)>) -> Self {
        let mut m = Self::with_capacity(entries.len());
        let mut i = 0;
        while i < entries.len() {
            m.insert(entries[i].0.clone(), entries[i].1.clone());
            i += 1;
        }
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A hash that sends every key to one bucket: still correct.
    #[derive(Clone, PartialEq, Debug)]
    struct Collide(u64);
    impl KeyHash for Collide {
        fn key_hash(&self) -> u64 {
            7
        }
    }

    #[test]
    fn matches_btreemap() {
        for buckets in [0, 1, 3, 16] {
            let mut m: HashMap<u64, u64> = HashMap::with_capacity(buckets);
            let mut c: HashMap<Collide, u64> = HashMap::with_capacity(buckets);
            let mut r: BTreeMap<u64, u64> = BTreeMap::new();
            let ops = [(1, 10, true), (2, 20, true), (1, 11, true), (2, 0, false), (3, 30, true), (1, 0, false), (19, 5, true), (3, 0, false)];
            for (k, v, put) in ops {
                if put {
                    assert_eq!(m.insert(k, v), r.get(&k).copied());
                    assert_eq!(c.insert(Collide(k), v), r.get(&k).copied());
                    r.insert(k, v);
                } else {
                    assert_eq!(m.remove(&k), r.get(&k).copied());
                    assert_eq!(c.remove(&Collide(k)), r.get(&k).copied());
                    r.remove(&k);
                }
                for q in 0..20u64 {
                    assert_eq!(m.get(&q), r.get(&q));
                    assert_eq!(c.get(&Collide(q)), r.get(&q));
                    assert_eq!(m.contains_key(&q), r.contains_key(&q));
                }
                assert_eq!(m.len(), r.len());
                assert_eq!(c.len(), r.len());
            }
        }
    }

    #[test]
    fn from_vec_last_wins() {
        let m = HashMap::from_vec(&vec![(b"a".to_vec(), 1u64), (b"b".to_vec(), 2), (b"a".to_vec(), 3)]);
        assert_eq!(m.get(&b"a".to_vec()), Some(&3));
        assert_eq!(m.get(&b"b".to_vec()), Some(&2));
        assert_eq!(m.get(&b"c".to_vec()), None);
        assert_eq!(m.len(), 2);
    }

    #[derive(Clone, PartialEq)]
    struct Key {
        id: u64,
        name: Vec<u8>,
    }
    impl KeyHash for Key {
        fn key_hash(&self) -> u64 {
            self.id.key_hash().wrapping_mul(0x0000_0100_0000_01b3) ^ self.name.key_hash()
        }
    }

    #[test]
    fn struct_keys() {
        let key = |id, name: &[u8]| Key { id, name: name.to_vec() };
        let mut m: HashMap<Key, bool> = HashMap::new();
        m.insert(key(1, b"x"), true);
        m.insert(key(1, b"y"), false);
        assert_eq!(m.get(&key(1, b"x")), Some(&true));
        assert_eq!(m.get(&key(1, b"y")), Some(&false));
        assert_eq!(m.get(&key(2, b"x")), None);
    }
}
