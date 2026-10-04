//! A `Vec<T>` used as a set: membership, subset, equality as sets.
//!
//! Generic over `T: PartialEq`, so the same functions serve ids (`u64`),
//! names (`Vec<u8>`) and enums. Order and duplicates do not matter to any
//! of them.

/// `s.contains(x)`.
pub fn contains<T: PartialEq>(s: &Vec<T>, x: &T) -> bool {
    let mut i = 0;
    while i < s.len() {
        if s[i] == *x {
            return true;
        }
        i += 1;
    }
    false
}

/// Every element of `a` is in `b`.
pub fn subset<T: PartialEq>(a: &Vec<T>, b: &Vec<T>) -> bool {
    let mut i = 0;
    while i < a.len() {
        if !contains(b, &a[i]) {
            return false;
        }
        i += 1;
    }
    true
}

/// Same elements, ignoring order and duplicates.
pub fn set_eq<T: PartialEq>(a: &Vec<T>, b: &Vec<T>) -> bool {
    subset(a, b) && subset(b, a)
}

/// Some element of `a` is in `b`.
pub fn intersects<T: PartialEq>(a: &Vec<T>, b: &Vec<T>) -> bool {
    let mut i = 0;
    while i < a.len() {
        if contains(b, &a[i]) {
            return true;
        }
        i += 1;
    }
    false
}

/// `s.iter().position(|y| y == x)`.
pub fn position<T: PartialEq>(s: &Vec<T>, x: &T) -> Option<usize> {
    let mut i = 0;
    while i < s.len() {
        if s[i] == *x {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Add `x` unless it is already there.
pub fn insert<T: PartialEq + Clone>(s: &Vec<T>, x: &T) -> Vec<T> {
    let mut out = s.clone();
    if !contains(s, x) {
        out.push(x.clone());
    }
    out
}

/// Every element except those equal to `x`.
pub fn remove<T: PartialEq + Clone>(s: &Vec<T>, x: &T) -> Vec<T> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        if s[i] != *x {
            out.push(s[i].clone());
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn sets() -> Vec<Vec<u64>> {
        vec![vec![], vec![1], vec![2], vec![1, 2], vec![2, 1, 1], vec![3, 1], vec![1, 2, 3]]
    }

    #[test]
    fn matches_btreeset() {
        for a in sets() {
            let sa: BTreeSet<u64> = a.iter().copied().collect();
            for b in sets() {
                let sb: BTreeSet<u64> = b.iter().copied().collect();
                assert_eq!(subset(&a, &b), sa.is_subset(&sb));
                assert_eq!(set_eq(&a, &b), sa == sb);
                assert_eq!(intersects(&a, &b), !sa.is_disjoint(&sb));
            }
            for x in 0..4u64 {
                assert_eq!(contains(&a, &x), sa.contains(&x));
                assert_eq!(position(&a, &x), a.iter().position(|y| *y == x));
                let mut si = sa.clone();
                si.insert(x);
                assert_eq!(insert(&a, &x).into_iter().collect::<BTreeSet<_>>(), si);
                assert_eq!(remove(&a, &x), a.iter().copied().filter(|y| *y != x).collect::<Vec<_>>());
            }
        }
    }
}
