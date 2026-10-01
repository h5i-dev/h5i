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
