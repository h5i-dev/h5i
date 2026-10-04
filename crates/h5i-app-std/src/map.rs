//! A `Vec<(K, V)>` used as a map. The first entry with a key wins, as
//! `insert` keeps one entry per key.

/// `m.get(k).cloned()`.
pub fn get<K: PartialEq, V: Clone>(m: &Vec<(K, V)>, k: &K) -> Option<V> {
    let mut i = 0;
    while i < m.len() {
        if m[i].0 == *k {
            return Some(m[i].1.clone());
        }
        i += 1;
    }
    None
}

/// `m.contains_key(k)`.
pub fn contains_key<K: PartialEq, V>(m: &Vec<(K, V)>, k: &K) -> bool {
    let mut i = 0;
    while i < m.len() {
        if m[i].0 == *k {
            return true;
        }
        i += 1;
    }
    false
}

/// `m.insert(k, v)`: replace the first entry with key `k`, or append one.
pub fn insert<K: PartialEq + Clone, V: Clone>(m: &Vec<(K, V)>, k: &K, v: &V) -> Vec<(K, V)> {
    let mut out = m.clone();
    let mut i = 0;
    while i < out.len() {
        if out[i].0 == *k {
            out[i] = (k.clone(), v.clone());
            return out;
        }
        i += 1;
    }
    out.push((k.clone(), v.clone()));
    out
}

/// `m.remove(k)`: drop every entry with key `k`.
pub fn remove<K: PartialEq + Clone, V: Clone>(m: &Vec<(K, V)>, k: &K) -> Vec<(K, V)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < m.len() {
        if m[i].0 != *k {
            out.push((m[i].0.clone(), m[i].1.clone()));
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn matches_btreemap() {
        let mut m: Vec<(u64, u64)> = Vec::new();
        let mut r: BTreeMap<u64, u64> = BTreeMap::new();
        let ops = [(1, 10, true), (2, 20, true), (1, 11, true), (2, 0, false), (3, 30, true), (1, 0, false)];
        for (k, v, put) in ops {
            if put {
                m = insert(&m, &k, &v);
                r.insert(k, v);
            } else {
                m = remove(&m, &k);
                r.remove(&k);
            }
            for q in 0..4u64 {
                assert_eq!(get(&m, &q), r.get(&q).copied());
                assert_eq!(contains_key(&m, &q), r.contains_key(&q));
            }
            assert_eq!(m.len(), r.len());
        }
    }
}
