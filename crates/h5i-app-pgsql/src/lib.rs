//! The SQL h5i-app runs on PostgreSQL.
//!
//! [`compile`] turns an `h5i_app_sql::Stmt` into a [`Query`] over a small SQL
//! subset ([`Sql`]); [`render`] prints it. Extracted to Lean (`proofs/`): a
//! compiled statement matches `H5iAppLib.Sql.exec` on the tenant's rows and
//! leaves other tenants alone.
//!
//! Aeneas subset: no `?`, no `String`, no iterator adapters.

use h5i_app_sql::{Stmt, Val};

/// A column type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Int,
    Bool,
    Text,
    Bytes,
}

/// A column of a tenant table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    pub name: Vec<u8>,
    pub kind: Kind,
    pub nullable: bool,
}

/// A tenant table: `tenant_id`, then `columns`. Key: `tenant_id` and the first `key_len` columns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    pub name: Vec<u8>,
    pub columns: Vec<Column>,
    pub key_len: u32,
}

/// A column definition in `CREATE TABLE`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColDef {
    pub name: Vec<u8>,
    pub kind: Kind,
    pub not_null: bool,
}

/// A `WHERE` condition; `$param` counts from 1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cond {
    /// `"col" = $param`
    Eq { col: Vec<u8>, param: u32 },
    /// `"col" IS NOT DISTINCT FROM $param`
    Same { col: Vec<u8>, param: u32 },
}

/// A statement of the subset. Names are printed quoted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sql {
    /// `CREATE TABLE IF NOT EXISTS "table" ("c" KIND [NOT NULL], ..., PRIMARY KEY ("k", ...))`
    Create { table: Vec<u8>, cols: Vec<ColDef>, key: Vec<Vec<u8>> },
    /// `SELECT "c", ... FROM "table" [WHERE cond AND ...] [ORDER BY "o", ...]`
    Select { table: Vec<u8>, cols: Vec<Vec<u8>>, conds: Vec<Cond>, order: Vec<Vec<u8>> },
    /// `INSERT ... ON CONFLICT ("k", ...) DO UPDATE SET "u" = EXCLUDED."u", ...` (`DO NOTHING` if `update` is empty)
    Insert { table: Vec<u8>, cols: Vec<Vec<u8>>, conflict: Vec<Vec<u8>>, update: Vec<Vec<u8>> },
    /// `DELETE FROM "table" [WHERE cond AND ...]`
    Delete { table: Vec<u8>, conds: Vec<Cond> },
}

/// A statement and the values of its parameters, `$1` first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    pub sql: Sql,
    pub params: Vec<Val>,
}

/// `tenant_id`, the first column of every table.
pub fn tenant_col() -> Vec<u8> {
    let mut v = Vec::new();
    push_lit(&mut v, b"tenant_id");
    v
}

fn push_lit(out: &mut Vec<u8>, s: &[u8]) {
    let mut i = 0;
    while i < s.len() {
        out.push(s[i]);
        i += 1;
    }
}

/// Quoted, PostgreSQL keeps it as written: nonempty, no NUL, at most 63 bytes.
fn name_ok(n: &Vec<u8>) -> bool {
    if n.len() == 0 || n.len() > 63 {
        return false;
    }
    let mut i = 0;
    while i < n.len() {
        if n[i] == 0 {
            return false;
        }
        i += 1;
    }
    true
}

/// `names[i]` differs from every later name.
fn distinct_from(names: &Vec<Vec<u8>>, i: usize) -> bool {
    let mut j = i + 1;
    while j < names.len() {
        if names[i] == names[j] {
            return false;
        }
        j += 1;
    }
    true
}

fn all_distinct(names: &Vec<Vec<u8>>) -> bool {
    let mut i = 0;
    while i < names.len() {
        if !distinct_from(names, i) {
            return false;
        }
        i += 1;
    }
    true
}

fn all_ok(names: &Vec<Vec<u8>>) -> bool {
    let mut i = 0;
    while i < names.len() {
        if !name_ok(&names[i]) {
            return false;
        }
        i += 1;
    }
    true
}

/// Every column's name, `tenant_id` first.
fn col_names(t: &Table) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    out.push(tenant_col());
    let mut i = 0;
    while i < t.columns.len() {
        out.push(t.columns[i].name.clone());
        i += 1;
    }
    out
}

/// Starts with `h5i_`, the prefix of the engine's own tables.
fn reserved(n: &Vec<u8>) -> bool {
    n.len() >= 4 && n[0] == 104 && n[1] == 53 && n[2] == 105 && n[3] == 95
}

/// 1..1599 columns (PostgreSQL caps at 1600 with `tenant_id`), distinct good names.
fn table_ok(t: &Table) -> bool {
    if !name_ok(&t.name) || reserved(&t.name) || t.columns.len() == 0 || t.columns.len() >= 1600 {
        return false;
    }
    let names = col_names(t);
    (t.key_len as usize) <= t.columns.len() && all_ok(&names) && all_distinct(&names)
}

/// Tables well formed with distinct names; no `tenant_id` column, no `h5i_` table.
pub fn valid(tables: &Vec<Table>) -> bool {
    let mut names = Vec::new();
    let mut i = 0;
    while i < tables.len() {
        if !table_ok(&tables[i]) {
            return false;
        }
        names.push(tables[i].name.clone());
        i += 1;
    }
    all_distinct(&names)
}

fn kind_eq(a: Kind, b: Kind) -> bool {
    match a {
        Kind::Int => match b {
            Kind::Int => true,
            _ => false,
        },
        Kind::Bool => match b {
            Kind::Bool => true,
            _ => false,
        },
        Kind::Text => match b {
            Kind::Text => true,
            _ => false,
        },
        Kind::Bytes => match b {
            Kind::Bytes => true,
            _ => false,
        },
    }
}

/// `v` has type `k` (`NULL` has every type).
fn has_kind(v: &Val, k: Kind) -> bool {
    match v {
        Val::Int(_) => kind_eq(k, Kind::Int),
        Val::Bool(_) => kind_eq(k, Kind::Bool),
        Val::Text(_) => kind_eq(k, Kind::Text),
        Val::Bytes(_) => kind_eq(k, Kind::Bytes),
        Val::Null => true,
    }
}

fn is_null(v: &Val) -> bool {
    match v {
        Val::Null => true,
        _ => false,
    }
}

/// Column `i` of `t` may hold `v`; `NULL` only in a nullable non-key column.
fn fits(t: &Table, i: usize, v: &Val) -> bool {
    let c = &t.columns[i];
    has_kind(v, c.kind) && (!is_null(v) || (c.nullable && i >= t.key_len as usize))
}

/// `CREATE TABLE` for `t`.
pub fn create(t: &Table) -> Sql {
    let mut cols = Vec::new();
    cols.push(ColDef { name: tenant_col(), kind: Kind::Int, not_null: true });
    let mut key = Vec::new();
    key.push(tenant_col());
    let n = t.key_len as usize;
    let mut i = 0;
    while i < t.columns.len() {
        cols.push(ColDef { name: t.columns[i].name.clone(), kind: t.columns[i].kind, not_null: !t.columns[i].nullable || i < n });
        if i < n {
            key.push(t.columns[i].name.clone());
        }
        i += 1;
    }
    Sql::Create { table: t.name.clone(), cols, key }
}

/// The names of columns `lo..hi`.
fn names_between(t: &Table, lo: usize, hi: usize) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = lo;
    while i < hi && i < t.columns.len() {
        out.push(t.columns[i].name.clone());
        i += 1;
    }
    out
}

/// The tenant's rows of `table` (no `tenant_id`), optionally filtered by
/// `col IS NOT DISTINCT FROM val`. `None` on a bad schema, table, column, or type.
pub fn select(tables: &Vec<Table>, tenant: i64, table: u32, filter: Option<(u32, Val)>) -> Option<Query> {
    if !valid(tables) || table as usize >= tables.len() {
        return None;
    }
    let t = &tables[table as usize];
    let mut conds = Vec::new();
    conds.push(Cond::Eq { col: tenant_col(), param: 1 });
    let mut params = Vec::new();
    params.push(Val::Int(tenant));
    match filter {
        None => {}
        Some((col, val)) => {
            let i = col as usize;
            if i >= t.columns.len() || !has_kind(&val, t.columns[i].kind) {
                return None;
            }
            conds.push(Cond::Same { col: t.columns[i].name.clone(), param: 2 });
            params.push(val);
        }
    }
    let sql = Sql::Select {
        table: t.name.clone(),
        cols: names_between(t, 0, t.columns.len()),
        conds,
        order: names_between(t, 0, t.key_len as usize),
    };
    Some(Query { sql, params })
}

/// Every value of `vs` fits the column at its position plus `off`.
fn all_fit(t: &Table, off: usize, vs: &Vec<Val>) -> bool {
    let mut i = 0;
    while i < vs.len() {
        if !fits(t, off + i, &vs[i]) {
            return false;
        }
        i += 1;
    }
    true
}

/// The query that runs `s` for `tenant`. `None` on a bad schema, a missing
/// table, or values that do not fit. `Delete` and `DeleteWhere` need key
/// columns: a keyless table holds one row, which is only replaced.
pub fn compile(tables: &Vec<Table>, tenant: i64, s: &Stmt) -> Option<Query> {
    if !valid(tables) {
        return None;
    }
    match s {
        Stmt::Upsert { table, key, rest } => {
            if *table as usize >= tables.len() {
                return None;
            }
            let t = &tables[*table as usize];
            let n = t.key_len as usize;
            if key.len() != n || rest.len() != t.columns.len() - n || !all_fit(t, 0, key) || !all_fit(t, n, rest) {
                return None;
            }
            let mut params = Vec::new();
            params.push(Val::Int(tenant));
            let mut i = 0;
            while i < key.len() {
                params.push(key[i].clone());
                i += 1;
            }
            let mut j = 0;
            while j < rest.len() {
                params.push(rest[j].clone());
                j += 1;
            }
            let mut conflict = Vec::new();
            conflict.push(tenant_col());
            let mut k = 0;
            while k < n {
                conflict.push(t.columns[k].name.clone());
                k += 1;
            }
            let sql = Sql::Insert {
                table: t.name.clone(),
                cols: col_names(t),
                conflict,
                update: names_between(t, n, t.columns.len()),
            };
            Some(Query { sql, params })
        }
        Stmt::Delete { table, key } => {
            if *table as usize >= tables.len() {
                return None;
            }
            let t = &tables[*table as usize];
            if t.key_len == 0 || key.len() != t.key_len as usize || !all_fit(t, 0, key) {
                return None;
            }
            let mut conds = Vec::new();
            conds.push(Cond::Eq { col: tenant_col(), param: 1 });
            let mut params = Vec::new();
            params.push(Val::Int(tenant));
            let mut i = 0;
            while i < key.len() {
                conds.push(Cond::Eq { col: t.columns[i].name.clone(), param: i as u32 + 2 });
                params.push(key[i].clone());
                i += 1;
            }
            Some(Query { sql: Sql::Delete { table: t.name.clone(), conds }, params })
        }
        Stmt::DeleteWhere { table, col, val } => {
            if *table as usize >= tables.len() {
                return None;
            }
            let t = &tables[*table as usize];
            let i = *col as usize;
            if t.key_len == 0 || i >= t.columns.len() || !has_kind(val, t.columns[i].kind) {
                return None;
            }
            let mut conds = Vec::new();
            conds.push(Cond::Eq { col: tenant_col(), param: 1 });
            conds.push(Cond::Same { col: t.columns[i].name.clone(), param: 2 });
            let mut params = Vec::new();
            params.push(Val::Int(tenant));
            params.push(val.clone());
            Some(Query { sql: Sql::Delete { table: t.name.clone(), conds }, params })
        }
    }
}

/// Appends `n` quoted: `"` around it, and each `"` inside doubled.
fn push_name(out: &mut Vec<u8>, n: &Vec<u8>) {
    out.push(34);
    let mut i = 0;
    while i < n.len() {
        if n[i] == 34 {
            out.push(34);
        }
        out.push(n[i]);
        i += 1;
    }
    out.push(34);
}

/// Appends quoted names separated by `, `.
fn push_names(out: &mut Vec<u8>, ns: &Vec<Vec<u8>>) {
    let mut i = 0;
    while i < ns.len() {
        if i > 0 {
            push_lit(out, b", ");
        }
        push_name(out, &ns[i]);
        i += 1;
    }
}

/// Appends `n` in decimal.
fn push_dec(out: &mut Vec<u8>, n: u32) {
    if n >= 10 {
        push_dec(out, n / 10);
    }
    out.push(48 + (n % 10) as u8);
}

fn push_param(out: &mut Vec<u8>, p: u32) {
    out.push(36);
    push_dec(out, p);
}

fn push_cond(out: &mut Vec<u8>, c: &Cond) {
    match c {
        Cond::Eq { col, param } => {
            push_name(out, col);
            push_lit(out, b" = ");
            push_param(out, *param);
        }
        Cond::Same { col, param } => {
            push_name(out, col);
            push_lit(out, b" IS NOT DISTINCT FROM ");
            push_param(out, *param);
        }
    }
}

fn push_where(out: &mut Vec<u8>, cs: &Vec<Cond>) {
    let mut i = 0;
    while i < cs.len() {
        if i == 0 {
            push_lit(out, b" WHERE ");
        } else {
            push_lit(out, b" AND ");
        }
        push_cond(out, &cs[i]);
        i += 1;
    }
}

fn push_kind(out: &mut Vec<u8>, k: Kind) {
    match k {
        Kind::Int => push_lit(out, b"BIGINT"),
        Kind::Bool => push_lit(out, b"BOOLEAN"),
        Kind::Text => push_lit(out, b"TEXT"),
        Kind::Bytes => push_lit(out, b"BYTEA"),
    }
}

fn push_coldefs(out: &mut Vec<u8>, cs: &Vec<ColDef>) {
    let mut i = 0;
    while i < cs.len() {
        push_name(out, &cs[i].name);
        out.push(32);
        push_kind(out, cs[i].kind);
        if cs[i].not_null {
            push_lit(out, b" NOT NULL");
        }
        push_lit(out, b", ");
        i += 1;
    }
}

/// `$1, $2, ..., $n`.
fn push_placeholders(out: &mut Vec<u8>, n: usize) {
    let mut i = 0;
    while i < n {
        if i > 0 {
            push_lit(out, b", ");
        }
        push_param(out, i as u32 + 1);
        i += 1;
    }
}

fn push_sets(out: &mut Vec<u8>, ns: &Vec<Vec<u8>>) {
    let mut i = 0;
    while i < ns.len() {
        if i > 0 {
            push_lit(out, b", ");
        }
        push_name(out, &ns[i]);
        push_lit(out, b" = EXCLUDED.");
        push_name(out, &ns[i]);
        i += 1;
    }
}

/// The text of `s`, as sent to PostgreSQL.
pub fn render(s: &Sql) -> Vec<u8> {
    let mut out = Vec::new();
    match s {
        Sql::Create { table, cols, key } => {
            push_lit(&mut out, b"CREATE TABLE IF NOT EXISTS ");
            push_name(&mut out, table);
            push_lit(&mut out, b" (");
            push_coldefs(&mut out, cols);
            push_lit(&mut out, b"PRIMARY KEY (");
            push_names(&mut out, key);
            push_lit(&mut out, b"))");
        }
        Sql::Select { table, cols, conds, order } => {
            push_lit(&mut out, b"SELECT ");
            push_names(&mut out, cols);
            push_lit(&mut out, b" FROM ");
            push_name(&mut out, table);
            push_where(&mut out, conds);
            if order.len() > 0 {
                push_lit(&mut out, b" ORDER BY ");
                push_names(&mut out, order);
            }
        }
        Sql::Insert { table, cols, conflict, update } => {
            push_lit(&mut out, b"INSERT INTO ");
            push_name(&mut out, table);
            push_lit(&mut out, b" (");
            push_names(&mut out, cols);
            push_lit(&mut out, b") VALUES (");
            push_placeholders(&mut out, cols.len());
            push_lit(&mut out, b") ON CONFLICT (");
            push_names(&mut out, conflict);
            if update.len() > 0 {
                push_lit(&mut out, b") DO UPDATE SET ");
                push_sets(&mut out, update);
            } else {
                push_lit(&mut out, b") DO NOTHING");
            }
        }
        Sql::Delete { table, conds } => {
            push_lit(&mut out, b"DELETE FROM ");
            push_name(&mut out, table);
            push_where(&mut out, conds);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, kind: Kind, nullable: bool) -> Column {
        Column { name: name.as_bytes().to_vec(), kind, nullable }
    }

    fn members() -> Vec<Table> {
        vec![Table {
            name: b"members".to_vec(),
            columns: vec![col("project", Kind::Int, false), col("user", Kind::Int, false), col("note", Kind::Text, true)],
            key_len: 2,
        }]
    }

    fn text(s: &Sql) -> String {
        String::from_utf8(render(s)).unwrap()
    }

    #[test]
    fn renders_statements() {
        let ts = members();
        assert_eq!(
            text(&create(&ts[0])),
            "CREATE TABLE IF NOT EXISTS \"members\" (\"tenant_id\" BIGINT NOT NULL, \"project\" BIGINT NOT NULL, \
             \"user\" BIGINT NOT NULL, \"note\" TEXT, PRIMARY KEY (\"tenant_id\", \"project\", \"user\"))"
        );
        let up = Stmt::Upsert { table: 0, key: vec![Val::Int(1), Val::Int(2)], rest: vec![Val::Null] };
        let q = compile(&ts, 7, &up).unwrap();
        assert_eq!(
            text(&q.sql),
            "INSERT INTO \"members\" (\"tenant_id\", \"project\", \"user\", \"note\") VALUES ($1, $2, $3, $4) \
             ON CONFLICT (\"tenant_id\", \"project\", \"user\") DO UPDATE SET \"note\" = EXCLUDED.\"note\""
        );
        assert_eq!(q.params, vec![Val::Int(7), Val::Int(1), Val::Int(2), Val::Null]);
        let del = Stmt::Delete { table: 0, key: vec![Val::Int(1), Val::Int(2)] };
        assert_eq!(
            text(&compile(&ts, 7, &del).unwrap().sql),
            "DELETE FROM \"members\" WHERE \"tenant_id\" = $1 AND \"project\" = $2 AND \"user\" = $3"
        );
        let dw = Stmt::DeleteWhere { table: 0, col: 2, val: Val::Null };
        assert_eq!(
            text(&compile(&ts, 7, &dw).unwrap().sql),
            "DELETE FROM \"members\" WHERE \"tenant_id\" = $1 AND \"note\" IS NOT DISTINCT FROM $2"
        );
        assert_eq!(
            text(&select(&ts, 7, 0, Some((0, Val::Int(3)))).unwrap().sql),
            "SELECT \"project\", \"user\", \"note\" FROM \"members\" WHERE \"tenant_id\" = $1 \
             AND \"project\" IS NOT DISTINCT FROM $2 ORDER BY \"project\", \"user\""
        );
    }

    #[test]
    fn quotes_names() {
        let mut out = Vec::new();
        push_name(&mut out, &b"a\"b".to_vec());
        assert_eq!(out, b"\"a\"\"b\"".to_vec());
        let mut out = Vec::new();
        push_placeholders(&mut out, 12);
        assert!(out.ends_with(b"$10, $11, $12"));
    }

    #[test]
    fn rejects_bad_input() {
        let ts = members();
        // A NULL key, a wrong type, a short row, a missing table.
        assert!(compile(&ts, 7, &Stmt::Upsert { table: 0, key: vec![Val::Null, Val::Int(2)], rest: vec![Val::Null] }).is_none());
        assert!(compile(&ts, 7, &Stmt::Delete { table: 0, key: vec![Val::Bool(true), Val::Int(2)] }).is_none());
        assert!(compile(&ts, 7, &Stmt::Upsert { table: 0, key: vec![Val::Int(1), Val::Int(2)], rest: vec![] }).is_none());
        assert!(compile(&ts, 7, &Stmt::Delete { table: 1, key: vec![] }).is_none());
        // NULL only in a nullable column.
        assert!(compile(&ts, 7, &Stmt::Upsert { table: 0, key: vec![Val::Int(1), Val::Int(2)], rest: vec![Val::Text(b"x".to_vec())] }).is_some());
        // Schemas: a column named tenant_id, duplicate tables, an empty name.
        let mut bad = members();
        bad[0].columns[2].name = b"tenant_id".to_vec();
        assert!(!valid(&bad));
        let mut dup = members();
        dup.push(dup[0].clone());
        assert!(!valid(&dup));
        let mut empty = members();
        empty[0].columns[0].name = Vec::new();
        assert!(!valid(&empty));
        let mut framework = members();
        framework[0].name = b"h5i_clock".to_vec();
        assert!(!valid(&framework));
        assert!(valid(&members()));
    }
}
