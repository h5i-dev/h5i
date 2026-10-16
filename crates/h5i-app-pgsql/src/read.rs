//! SQL for the declarative reads of `h5i_app_sql` (`Read`, `Pred`).
//!
//! Not extracted to Lean: `compile_read` is checked by differential tests
//! (`crates/h5i-app-pg/src/reads_test.rs`) against `h5i_app_sql::eval`, which
//! mirrors `H5iAppLib.Query`. The checks below are what makes the SQL mean
//! what the Lean says: `IN` only between `NOT NULL` columns (so SQL's `NULL`
//! rules never apply), windows only on tables whose key columns are all
//! integers or booleans (so the key order is not a collation's), and the
//! result always sorted by key ascending.

use super::{has_kind, push_lit, push_name, tenant_col, valid, Kind, Table};
use h5i_app_sql::{Pred, Read, Val, Window};

/// A read's text and its parameters, `$1` (the tenant) first. Every value,
/// limit and offset is a parameter, so the text depends only on the read's
/// shape and on whether its window has a limit and a non-zero offset: at
/// most four texts per shape, whatever the numbers. `OFFSET 0` is left out
/// on purpose: with an `OFFSET` parameter PostgreSQL estimates a generic plan
/// as dearer than the custom ones and never reuses it, re-planning on every
/// execution of the prepared statement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadQuery {
    pub text: Vec<u8>,
    pub params: Vec<Val>,
}

struct Out<'a> {
    tables: &'a Vec<Table>,
    text: Vec<u8>,
    params: Vec<Val>,
}

fn windowed(w: &Window) -> bool {
    w.offset > 0 || w.limit.is_some()
}

/// A column that never holds `NULL`: a key column or a non-nullable one.
fn not_null(t: &Table, i: usize) -> bool {
    i < t.key_len as usize || !t.columns[i].nullable
}

fn ordered_key(t: &Table) -> bool {
    t.key_len > 0
        && t.columns[..t.key_len as usize].iter().all(|c| matches!(c.kind, Kind::Int | Kind::Bool))
}

fn int_param(n: u64) -> Val {
    Val::Int(n.min(i64::MAX as u64) as i64)
}

impl Out<'_> {
    fn lit(&mut self, s: &str) {
        push_lit(&mut self.text, s.as_bytes());
    }

    fn param(&mut self, v: Val) {
        self.params.push(v);
        let n = self.params.len();
        self.lit(&format!("${n}"));
    }

    fn col(&mut self, alias: usize, name: &Vec<u8>) {
        self.lit(&format!("q{alias}."));
        push_name(&mut self.text, name);
    }

    /// `SELECT <cols> FROM "t" AS q<alias> WHERE tenant AND <pred> [ORDER BY key [LIMIT/OFFSET]]`.
    fn select(&mut self, table: u32, pred: &Pred, w: &Window, cols: &[usize], alias: usize, order: bool) -> Option<()> {
        let t = self.tables.get(table as usize)?;
        if windowed(w) && !ordered_key(t) {
            return None;
        }
        self.lit("SELECT ");
        for (n, &i) in cols.iter().enumerate() {
            if n > 0 {
                self.lit(", ");
            }
            self.col(alias, &t.columns.get(i)?.name.clone());
        }
        self.lit(" FROM ");
        push_name(&mut self.text, &t.name);
        self.lit(&format!(" AS q{alias} WHERE "));
        self.col(alias, &tenant_col());
        self.lit(" = $1 AND ");
        self.pred(t, pred, alias)?;
        if order || windowed(w) {
            self.order_by(t, alias, w.desc && windowed(w));
        }
        if windowed(w) {
            if let Some(n) = w.limit {
                self.lit(" LIMIT ");
                self.param(int_param(n));
            }
            if w.offset > 0 {
                self.lit(" OFFSET ");
                self.param(int_param(w.offset));
            }
        }
        Some(())
    }

    fn order_by(&mut self, t: &Table, alias: usize, desc: bool) {
        self.lit(" ORDER BY ");
        for i in 0..t.key_len as usize {
            if i > 0 {
                self.lit(", ");
            }
            self.col(alias, &t.columns[i].name.clone());
            if desc {
                self.lit(" DESC");
            }
        }
    }

    fn pred(&mut self, t: &Table, p: &Pred, alias: usize) -> Option<()> {
        match p {
            Pred::All => self.lit("TRUE"),
            Pred::Eq { col, val } => {
                let c = t.columns.get(*col as usize)?;
                if !has_kind(val, c.kind) {
                    return None;
                }
                self.col(alias, &c.name.clone());
                // The same on a column that is never NULL, and `=` can use an index.
                self.lit(if not_null(t, *col as usize) { " = " } else { " IS NOT DISTINCT FROM " });
                self.param(val.clone());
            }
            Pred::In { col, table, pred, of, window } => {
                let i = *col as usize;
                let c = t.columns.get(i)?;
                let sub = self.tables.get(*table as usize)?;
                let j = *of as usize;
                let d = sub.columns.get(j)?;
                if c.kind != d.kind || !not_null(t, i) || !not_null(sub, j) {
                    return None;
                }
                self.col(alias, &c.name.clone());
                self.lit(" IN (");
                self.select(*table, pred, window, &[j], alias + 1, false)?;
                self.lit(")");
            }
            Pred::And(a, b) | Pred::Or(a, b) => {
                let op = if matches!(p, Pred::And(..)) { " AND " } else { " OR " };
                self.lit("(");
                self.pred(t, a, alias)?;
                self.lit(op);
                self.pred(t, b, alias)?;
                self.lit(")");
            }
        }
        Some(())
    }
}

/// The `SELECT` returning `q`'s rows of `tenant` (no `tenant_id`) in
/// ascending key order, or with `count` the number of them. `None` on an
/// invalid schema, a missing table or column, a value of the wrong type, `IN`
/// on a nullable column, or a window on a table without an integer key.
pub fn compile_read(tables: &Vec<Table>, tenant: i64, q: &Read, count: bool) -> Option<ReadQuery> {
    if !valid(tables) {
        return None;
    }
    let t = tables.get(q.table as usize)?;
    let all: Vec<usize> = (0..t.columns.len()).collect();
    let mut out = Out { tables, text: Vec::new(), params: vec![Val::Int(tenant)] };
    let desc = q.window.desc && windowed(&q.window);
    if count {
        out.lit("SELECT count(*) FROM (");
        out.select(q.table, &q.pred, &q.window, &all, 0, false)?;
        out.lit(") AS c");
    } else if desc {
        // The window counts from the newest key; the rows come back oldest first.
        out.lit("SELECT * FROM (");
        out.select(q.table, &q.pred, &q.window, &all, 0, false)?;
        out.lit(") AS w ORDER BY ");
        for i in 0..t.key_len as usize {
            if i > 0 {
                out.lit(", ");
            }
            push_name(&mut out.text, &t.columns[i].name);
        }
    } else {
        out.select(q.table, &q.pred, &q.window, &all, 0, t.key_len > 0)?;
    }
    Some(ReadQuery { text: out.text, params: out.params })
}

/// `CREATE INDEX IF NOT EXISTS` on `tenant_id` and columns `cols` of table
/// `table`, so reads that filter on them need not scan the tenant. Indexes
/// change no result, only speed. `None` on a bad schema, table or column.
pub fn create_index(tables: &Vec<Table>, table: u32, cols: &[u32]) -> Option<Vec<u8>> {
    if !valid(tables) || cols.is_empty() {
        return None;
    }
    let t = tables.get(table as usize)?;
    let mut names = vec![tenant_col()];
    for &c in cols {
        names.push(t.columns.get(c as usize)?.name.clone());
    }
    // A name PostgreSQL keeps whole: the table's number and the columns'.
    let mut idx = format!("h5i_idx_{table}");
    for &c in cols {
        idx.push_str(&format!("_{c}"));
    }
    let mut out = Vec::new();
    push_lit(&mut out, b"CREATE INDEX IF NOT EXISTS ");
    push_name(&mut out, &[&t.name[..], b"_", idx.as_bytes()].concat().iter().take(63).copied().collect());
    push_lit(&mut out, b" ON ");
    push_name(&mut out, &t.name);
    push_lit(&mut out, b" (");
    for (i, n) in names.iter().enumerate() {
        if i > 0 {
            push_lit(&mut out, b", ");
        }
        push_name(&mut out, n);
    }
    push_lit(&mut out, b")");
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Column;
    use h5i_app_sql::whole;

    fn col(name: &str, kind: Kind, nullable: bool) -> Column {
        Column { name: name.as_bytes().to_vec(), kind, nullable }
    }

    fn schema() -> Vec<Table> {
        vec![
            Table {
                name: b"articles".to_vec(),
                columns: vec![col("id", Kind::Int, false), col("author", Kind::Int, false), col("slug", Kind::Bytes, false)],
                key_len: 1,
            },
            Table { name: b"tags".to_vec(), columns: vec![col("article", Kind::Int, false), col("tag", Kind::Bytes, false)], key_len: 2 },
            Table { name: b"notes".to_vec(), columns: vec![col("tag", Kind::Bytes, false), col("n", Kind::Int, true)], key_len: 1 },
        ]
    }

    fn text(q: &ReadQuery) -> String {
        String::from_utf8(q.text.clone()).unwrap()
    }

    #[test]
    fn renders_a_page_of_a_tag() {
        let q = Read {
            table: 0,
            pred: Pred::In {
                col: 0,
                table: 1,
                pred: Box::new(Pred::Eq { col: 1, val: Val::Bytes(b"rust".to_vec()) }),
                of: 0,
                window: whole(),
            },
            window: Window { desc: true, offset: 0, limit: Some(20) },
        };
        let c = compile_read(&schema(), 7, &q, false).unwrap();
        assert_eq!(
            text(&c),
            "SELECT * FROM (SELECT q0.\"id\", q0.\"author\", q0.\"slug\" FROM \"articles\" AS q0 WHERE q0.\"tenant_id\" = $1 \
             AND q0.\"id\" IN (SELECT q1.\"article\" FROM \"tags\" AS q1 WHERE q1.\"tenant_id\" = $1 AND \
             q1.\"tag\" = $2) ORDER BY q0.\"id\" DESC LIMIT $3) AS w ORDER BY \"id\""
        );
        assert_eq!(c.params, vec![Val::Int(7), Val::Bytes(b"rust".to_vec()), Val::Int(20)]);
        // Another page size, same text: one prepared statement serves both.
        let mut other = q.clone();
        other.window.limit = Some(50);
        assert_eq!(compile_read(&schema(), 9, &other, false).unwrap().text, c.text);
        let n = compile_read(&schema(), 7, &q, true).unwrap();
        assert!(text(&n).starts_with("SELECT count(*) FROM (SELECT q0."));
    }

    #[test]
    fn rejects_what_the_lean_does_not_mean() {
        let s = schema();
        // A window over a text key.
        let w = Read { table: 2, pred: Pred::All, window: Window { desc: false, offset: 1, limit: None } };
        assert!(compile_read(&s, 1, &w, false).is_none());
        // IN on a nullable column.
        let n = Read {
            table: 0,
            pred: Pred::In { col: 1, table: 2, pred: Box::new(Pred::All), of: 1, window: whole() },
            window: whole(),
        };
        assert!(compile_read(&s, 1, &n, false).is_none());
        // A value of the wrong type, a missing column.
        assert!(compile_read(&s, 1, &h5i_app_sql::read_eq(0, 2, Val::Int(1)), false).is_none());
        assert!(compile_read(&s, 1, &h5i_app_sql::read_eq(0, 9, Val::Int(1)), false).is_none());
        assert!(compile_read(&s, 1, &h5i_app_sql::read_all(0), false).is_some());
    }

    #[test]
    fn renders_an_index() {
        let i = create_index(&schema(), 0, &[2]).unwrap();
        assert_eq!(
            String::from_utf8(i).unwrap(),
            "CREATE INDEX IF NOT EXISTS \"articles_h5i_idx_0_2\" ON \"articles\" (\"tenant_id\", \"slug\")"
        );
        assert!(create_index(&schema(), 0, &[9]).is_none());
    }
}
