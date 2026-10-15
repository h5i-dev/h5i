//! Plans the statements a write set runs, independent of SQL text.
//!
//! Extracted to Lean (see `proofs/`). Aeneas subset: no `?`, no `String`,
//! no iterator adapters; loops are `while` over indices.

/// A column value. Text is kept as UTF-8 bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Val {
    Int(i64),
    Bool(bool),
    Text(Vec<u8>),
    Bytes(Vec<u8>),
    Null,
}

/// How a kernel field is stored and read back. Integers keep their bits: a
/// `u64` above `i64::MAX` is stored as a negative `BIGINT`. `from_val` must
/// undo `to_val`; the kernel proofs check this for each field type used.
pub trait Column: Sized {
    fn to_val(&self) -> Val;
    fn from_val(v: &Val) -> Option<Self>;
}

impl Column for u64 {
    fn to_val(&self) -> Val {
        Val::Int(*self as i64)
    }
    fn from_val(v: &Val) -> Option<u64> {
        match v {
            Val::Int(i) => Some(*i as u64),
            _ => None,
        }
    }
}

impl Column for u32 {
    fn to_val(&self) -> Val {
        Val::Int(*self as i64)
    }
    fn from_val(v: &Val) -> Option<u32> {
        match v {
            Val::Int(i) => {
                if *i >= 0 && *i <= u32::MAX as i64 {
                    Some(*i as u32)
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

impl Column for bool {
    fn to_val(&self) -> Val {
        Val::Bool(*self)
    }
    fn from_val(v: &Val) -> Option<bool> {
        match v {
            Val::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

impl Column for Vec<u8> {
    fn to_val(&self) -> Val {
        Val::Bytes(self.clone())
    }
    fn from_val(v: &Val) -> Option<Vec<u8>> {
        match v {
            Val::Bytes(b) => Some(b.clone()),
            _ => None,
        }
    }
}

/// `None` is SQL `NULL`, so `T` must never encode to `NULL` itself.
impl<T: Column> Column for Option<T> {
    fn to_val(&self) -> Val {
        match self {
            Some(x) => x.to_val(),
            None => Val::Null,
        }
    }
    fn from_val(v: &Val) -> Option<Option<T>> {
        match v {
            Val::Null => Some(None),
            _ => match T::from_val(v) {
                Some(x) => Some(Some(x)),
                None => None,
            },
        }
    }
}

/// A column's value in a fresh tenant's singleton row (a table without key
/// columns, such as a counter, that has no row until its first write).
pub trait Zero {
    fn zero() -> Self;
}

impl Zero for u64 {
    fn zero() -> u64 {
        0
    }
}

impl Zero for u32 {
    fn zero() -> u32 {
        0
    }
}

impl Zero for bool {
    fn zero() -> bool {
        false
    }
}

impl Zero for Vec<u8> {
    fn zero() -> Vec<u8> {
        Vec::new()
    }
}

impl<T> Zero for Option<T> {
    fn zero() -> Option<T> {
        None
    }
}

/// `a == b`, written out so Aeneas needs no derived `PartialEq`.
pub fn val_eq(a: &Val, b: &Val) -> bool {
    match a {
        Val::Int(x) => match b {
            Val::Int(y) => *x == *y,
            _ => false,
        },
        Val::Bool(x) => match b {
            Val::Bool(y) => *x == *y,
            _ => false,
        },
        Val::Text(x) => match b {
            Val::Text(y) => *x == *y,
            _ => false,
        },
        Val::Bytes(x) => match b {
            Val::Bytes(y) => *x == *y,
            _ => false,
        },
        Val::Null => match b {
            Val::Null => true,
            _ => false,
        },
    }
}

/// Column `col` of `row` holds `val`.
pub fn has_col(row: &Vec<Val>, col: u32, val: &Val) -> bool {
    let i = col as usize;
    if i < row.len() {
        val_eq(&row[i], val)
    } else {
        false
    }
}

/// One row-level write on a tenant's table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Write {
    /// Insert or replace the row whose first `key_len` columns are its key.
    Put { table: u32, key_len: u32, row: Vec<Val> },
    /// Remove the row with this key, if any.
    Del { table: u32, key: Vec<Val> },
    /// Remove every row whose column `col` holds `val`.
    DelWhere { table: u32, col: u32, val: Val },
}

/// A statement. `Upsert` stores `key ++ rest` at `key`. The store runs
/// `DeleteWhere` as a `SELECT` of the matching rows and a keyed delete of each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stmt {
    Upsert { table: u32, key: Vec<Val>, rest: Vec<Val> },
    Delete { table: u32, key: Vec<Val> },
    DeleteWhere { table: u32, col: u32, val: Val },
}

/// The first `n` values of `row` (all of them if the row is shorter).
pub fn prefix(row: &Vec<Val>, n: usize) -> Vec<Val> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < row.len() && i < n {
        out.push(row[i].clone());
        i += 1;
    }
    out
}

/// The values of `row` after the first `n`.
pub fn suffix(row: &Vec<Val>, n: usize) -> Vec<Val> {
    let mut out = Vec::new();
    let mut i = n;
    while i < row.len() {
        out.push(row[i].clone());
        i += 1;
    }
    out
}

pub fn plan_one(w: &Write) -> Stmt {
    match w {
        Write::Put { table, key_len, row } => {
            let n = *key_len as usize;
            Stmt::Upsert { table: *table, key: prefix(row, n), rest: suffix(row, n) }
        }
        Write::Del { table, key } => Stmt::Delete { table: *table, key: key.clone() },
        Write::DelWhere { table, col, val } => Stmt::DeleteWhere { table: *table, col: *col, val: val.clone() },
    }
}

/// One statement per write, in order.
pub fn plan(ws: &Vec<Write>) -> Vec<Stmt> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < ws.len() {
        out.push(plan_one(&ws[i]));
        i += 1;
    }
    out
}

/// Which part of a read's matching rows it keeps: `offset` rows skipped, then
/// at most `limit` (all if `None`), counted in ascending key order, or
/// descending if `desc`. The rows come back in ascending key order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub desc: bool,
    pub offset: u64,
    pub limit: Option<u64>,
}

/// Every matching row.
pub fn whole() -> Window {
    Window { desc: false, offset: 0, limit: None }
}

/// Which rows of a table a read keeps. `H5iAppLib.Query.Pred` is its meaning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pred {
    /// Every row.
    All,
    /// Column `col` holds `val` (`IS NOT DISTINCT FROM`).
    Eq { col: u32, val: Val },
    /// Column `col` is column `of` of a row the read of `table` by `pred` and
    /// `window` returns (`IN (SELECT ...)`). Both columns must be `NOT NULL`.
    In { col: u32, table: u32, pred: Box<Pred>, of: u32, window: Window },
    And(Box<Pred>, Box<Pred>),
    Or(Box<Pred>, Box<Pred>),
}

/// A read of one table of the tenant. `H5iAppLib.Query.Read.rows` is its
/// meaning; `h5i_app_pgsql::compile_read` its SQL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Read {
    pub table: u32,
    pub pred: Pred,
    pub window: Window,
}

/// The whole table.
pub fn read_all(table: u32) -> Read {
    Read { table, pred: Pred::All, window: whole() }
}

/// The rows whose column `col` holds `val`.
pub fn read_eq(table: u32, col: u32, val: Val) -> Read {
    Read { table, pred: Pred::Eq { col, val }, window: whole() }
}

/// Reference evaluator: what `H5iAppLib.Query` says a read returns, over the
/// tenant's tables (`tabs[t]`, rows in ascending key order). Not extracted;
/// it mirrors the Lean definitions clause by clause, and the differential
/// tests compare it with both PostgreSQL and the Lean definitions.
pub mod eval {
    use super::{Pred, Read, Val, Window};

    pub fn cut<T: Clone>(offset: u64, limit: Option<u64>, l: &[T]) -> Vec<T> {
        let start = (offset.min(l.len() as u64)) as usize;
        let rest = &l[start..];
        match limit {
            None => rest.to_vec(),
            Some(n) => rest[..(n.min(rest.len() as u64) as usize)].to_vec(),
        }
    }

    pub fn window<T: Clone>(w: &Window, l: &[T]) -> Vec<T> {
        if w.desc {
            let rev: Vec<T> = l.iter().rev().cloned().collect();
            let mut out = cut(w.offset, w.limit, &rev);
            out.reverse();
            out
        } else {
            cut(w.offset, w.limit, l)
        }
    }

    pub fn holds(tabs: &[Vec<Vec<Val>>], p: &Pred, row: &[Val]) -> bool {
        match p {
            Pred::All => true,
            Pred::Eq { col, val } => row.get(*col as usize) == Some(val),
            Pred::In { col, table, pred, of, window: w } => match row.get(*col as usize) {
                None => false,
                Some(x) => {
                    let rows = select(tabs, *table, pred, w);
                    rows.iter().any(|y| y.get(*of as usize) == Some(x))
                }
            },
            Pred::And(a, b) => holds(tabs, a, row) && holds(tabs, b, row),
            Pred::Or(a, b) => holds(tabs, a, row) || holds(tabs, b, row),
        }
    }

    fn select(tabs: &[Vec<Vec<Val>>], table: u32, pred: &Pred, w: &Window) -> Vec<Vec<Val>> {
        let empty = Vec::new();
        let t = tabs.get(table as usize).unwrap_or(&empty);
        let kept: Vec<Vec<Val>> = t.iter().filter(|r| holds(tabs, pred, r)).cloned().collect();
        window(w, &kept)
    }

    /// `Read.rows`.
    pub fn rows(tabs: &[Vec<Vec<Val>>], q: &Read) -> Vec<Vec<Val>> {
        select(tabs, q.table, &q.pred, &q.window)
    }

    /// Random reads over a fixed small schema, for the differential tests
    /// (`h5i-app-pg`'s against PostgreSQL, `H5iAppLibTests/QueryCases.lean`
    /// against the Lean definitions).
    pub mod cases {
        use super::super::{Pred, Read, Val, Window};

        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Kind {
            Int,
            Bytes,
        }

        /// A table: name, columns (name, kind, nullable), key length.
        pub struct Shape {
            pub name: &'static str,
            pub cols: Vec<(&'static str, Kind, bool)>,
            pub key_len: usize,
        }

        /// `dt_a` (integer key, a nullable column), `dt_b` (a pair key),
        /// `dt_c` (a key with bytes, so no windows).
        pub fn shapes() -> Vec<Shape> {
            vec![
                Shape {
                    name: "dt_a",
                    cols: vec![("id", Kind::Int, false), ("k", Kind::Int, false), ("s", Kind::Bytes, false), ("n", Kind::Int, true)],
                    key_len: 1,
                },
                Shape { name: "dt_b", cols: vec![("a", Kind::Int, false), ("b", Kind::Int, false)], key_len: 2 },
                Shape { name: "dt_c", cols: vec![("x", Kind::Int, false), ("y", Kind::Bytes, false), ("z", Kind::Int, false)], key_len: 2 },
            ]
        }

        pub struct Rng(pub u64);

        impl Rng {
            pub fn below(&mut self, n: u64) -> u64 {
                self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (self.0 >> 33) % n
            }
        }

        fn value(r: &mut Rng, k: Kind, nullable: bool) -> Val {
            if nullable && r.below(4) == 0 {
                return Val::Null;
            }
            match k {
                Kind::Int => Val::Int(r.below(6) as i64),
                Kind::Bytes => Val::Bytes(vec![b'a' + r.below(3) as u8]),
            }
        }

        /// Rows of each table, unique keys, in ascending key order.
        pub fn tables(r: &mut Rng) -> Vec<Vec<Vec<Val>>> {
            shapes()
                .iter()
                .map(|sh| {
                    let n = r.below(8);
                    let mut rows: Vec<Vec<Val>> = Vec::new();
                    for _ in 0..n {
                        let row: Vec<Val> = sh.cols.iter().map(|c| value(r, c.1, c.2)).collect();
                        if !rows.iter().any(|x| x[..sh.key_len] == row[..sh.key_len]) {
                            rows.push(row);
                        }
                    }
                    rows.sort_by(|a, b| key_cmp(&a[..sh.key_len], &b[..sh.key_len]));
                    rows
                })
                .collect()
        }

        fn key_cmp(a: &[Val], b: &[Val]) -> std::cmp::Ordering {
            for (x, y) in a.iter().zip(b) {
                let o = match (x, y) {
                    (Val::Int(p), Val::Int(q)) => p.cmp(q),
                    (Val::Bytes(p), Val::Bytes(q)) => p.cmp(q),
                    _ => std::cmp::Ordering::Equal,
                };
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
            }
            std::cmp::Ordering::Equal
        }

        fn window(r: &mut Rng, ordered: bool) -> Window {
            if !ordered || r.below(2) == 0 {
                return Window { desc: false, offset: 0, limit: None };
            }
            let limit = if r.below(3) == 0 { None } else { Some(r.below(4)) };
            Window { desc: r.below(2) == 0, offset: r.below(3), limit }
        }

        fn pred(r: &mut Rng, table: usize, depth: u32) -> Pred {
            let sh = &shapes()[table];
            let pick = if depth == 0 { r.below(2) } else { r.below(5) };
            match pick {
                0 => Pred::All,
                1 => {
                    let c = r.below(sh.cols.len() as u64) as usize;
                    Pred::Eq { col: c as u32, val: value(r, sh.cols[c].1, sh.cols[c].2) }
                }
                2 => {
                    // An integer, never-NULL column against one of another read.
                    let ints = |s: &Shape| -> Vec<usize> {
                        (0..s.cols.len()).filter(|&i| s.cols[i].1 == Kind::Int && !s.cols[i].2).collect()
                    };
                    let mine = ints(sh);
                    let t = r.below(3) as usize;
                    let theirs = ints(&shapes()[t]);
                    let col = mine[r.below(mine.len() as u64) as usize] as u32;
                    let of = theirs[r.below(theirs.len() as u64) as usize] as u32;
                    let w = window(r, t != 2);
                    Pred::In { col, table: t as u32, pred: Box::new(pred(r, t, depth - 1)), of, window: w }
                }
                3 => Pred::And(Box::new(pred(r, table, depth - 1)), Box::new(pred(r, table, depth - 1))),
                _ => Pred::Or(Box::new(pred(r, table, depth - 1)), Box::new(pred(r, table, depth - 1))),
            }
        }

        /// A random read. Windows only on the integer-keyed tables, as
        /// `compile_read` requires.
        pub fn read(r: &mut Rng) -> Read {
            let t = r.below(3) as usize;
            Read { table: t as u32, pred: pred(r, t, 2), window: window(r, t != 2) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_key_from_rest() {
        let row = vec![Val::Int(1), Val::Int(2), Val::Text(b"x".to_vec())];
        let ws = vec![Write::Put { table: 3, key_len: 2, row }, Write::Del { table: 3, key: vec![Val::Int(1)] }];
        assert_eq!(
            plan(&ws),
            vec![
                Stmt::Upsert { table: 3, key: vec![Val::Int(1), Val::Int(2)], rest: vec![Val::Text(b"x".to_vec())] },
                Stmt::Delete { table: 3, key: vec![Val::Int(1)] },
            ]
        );
    }

    #[test]
    fn compares_columns() {
        let row = vec![Val::Int(1), Val::Bytes(b"x".to_vec())];
        assert!(has_col(&row, 1, &Val::Bytes(b"x".to_vec())));
        assert!(!has_col(&row, 1, &Val::Text(b"x".to_vec())));
        assert!(!has_col(&row, 2, &Val::Null));
        let w = Write::DelWhere { table: 2, col: 1, val: Val::Int(7) };
        assert_eq!(plan(&vec![w]), vec![Stmt::DeleteWhere { table: 2, col: 1, val: Val::Int(7) }]);
    }
}
