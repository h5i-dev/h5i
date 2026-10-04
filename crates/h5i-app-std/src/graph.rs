//! Reachability over an edge list: role inheritance, nested groups.
//!
//! Upstream code often resolves these by recursion, which the Aeneas subset
//! lacks. `reachable` is the loop form, proven once: its result holds exactly
//! the nodes reachable from `start`.

use crate::set;

/// Every node reachable from `start` along `edges` (`(from, to)`), `start`
/// included, each once. Breadth-first: `out` is both the visited set and the
/// queue, so a cycle ends the search instead of looping.
pub fn reachable<T: PartialEq + Clone>(edges: &Vec<(T, T)>, start: &Vec<T>) -> Vec<T> {
    let mut out: Vec<T> = Vec::new();
    let mut i = 0;
    while i < start.len() {
        if !set::contains(&out, &start[i]) {
            out.push(start[i].clone());
        }
        i += 1;
    }
    let mut k = 0;
    while k < out.len() {
        let x = out[k].clone();
        let mut j = 0;
        while j < edges.len() {
            if edges[j].0 == x && !set::contains(&out, &edges[j].1) {
                out.push(edges[j].1.clone());
            }
            j += 1;
        }
        k += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Reachability by fixpoint iteration, for comparison.
    fn closure(edges: &[(u64, u64)], start: &[u64]) -> BTreeSet<u64> {
        let mut s: BTreeSet<u64> = start.iter().copied().collect();
        loop {
            let next: BTreeSet<u64> =
                s.iter().copied().chain(edges.iter().filter(|e| s.contains(&e.0)).map(|e| e.1)).collect();
            if next == s {
                return s;
            }
            s = next;
        }
    }

    #[test]
    fn matches_fixpoint() {
        let graphs: Vec<Vec<(u64, u64)>> = vec![
            vec![],
            vec![(1, 2), (2, 3)],
            vec![(1, 2), (2, 1)],
            vec![(1, 1)],
            vec![(1, 2), (1, 3), (3, 4), (4, 2), (5, 1)],
        ];
        let starts: Vec<Vec<u64>> = vec![vec![], vec![1], vec![2, 2], vec![5], vec![3, 1]];
        for g in &graphs {
            for s in &starts {
                let r = reachable(g, s);
                let set: BTreeSet<u64> = r.iter().copied().collect();
                assert_eq!(set, closure(g, s));
                assert_eq!(set.len(), r.len(), "no duplicates");
            }
        }
    }
}
