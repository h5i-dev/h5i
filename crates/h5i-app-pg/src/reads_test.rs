//! Differential test of `h5i_app_pgsql::compile_read`: random reads over
//! random tables, run on PostgreSQL through `load_read` and `count_read`,
//! must return what `h5i_app_sql::eval` (the Lean definitions of
//! `H5iAppLib.Query`, clause by clause) says. Needs
//! H5I_APP_TEST_DATABASE_URL; skips otherwise.

use crate::{count_read, create_tables, load_read, load_reads, pool, store_writes, Tx};
use h5i_app_core::TenantId;
use h5i_app_pgsql::{Column, Kind, Table};
use h5i_app_sql::eval::{self, cases};
use h5i_app_sql::Write;

fn schema() -> Vec<Table> {
    cases::shapes()
        .iter()
        .map(|s| Table {
            name: s.name.as_bytes().to_vec(),
            columns: s
                .cols
                .iter()
                .map(|c| Column {
                    name: c.0.as_bytes().to_vec(),
                    kind: if c.1 == cases::Kind::Int { Kind::Int } else { Kind::Bytes },
                    nullable: c.2,
                })
                .collect(),
            key_len: s.key_len as u32,
        })
        .collect()
}

#[tokio::test]
async fn reads_match_the_reference() {
    let Ok(url) = std::env::var("H5I_APP_TEST_DATABASE_URL") else {
        eprintln!("H5I_APP_TEST_DATABASE_URL not set; skipping");
        return;
    };
    let url = crate::with_schema(&url, "h5i_reads_test").unwrap();
    let p = pool(&url, 2).unwrap();
    let schema = schema();
    let mut client = p.0.get().await.unwrap();
    {
        let tx = client.transaction().await.unwrap();
        tx.batch_execute("CREATE SCHEMA IF NOT EXISTS h5i_reads_test").await.unwrap();
        for t in &schema {
            tx.batch_execute(&format!("DROP TABLE IF EXISTS \"{}\"", String::from_utf8_lossy(&t.name))).await.unwrap();
        }
        for ddl in create_tables(&schema).unwrap() {
            tx.batch_execute(&ddl).await.unwrap();
        }
        tx.commit().await.unwrap();
    }
    let mut r = cases::Rng(42);
    let (mut ran, mut rejected, mut nonempty) = (0, 0, 0);
    for case in 0..400u64 {
        let tenant = TenantId(case + 1);
        let tabs = cases::tables(&mut r);
        let noise = cases::tables(&mut r);
        let tx = client.transaction().await.unwrap();
        let t = Tx(&tx, &p.1);
        // The case's rows, and other rows in the next tenant that must not show.
        for (tid, rows) in [(tenant, &tabs), (TenantId(case + 100_000), &noise)] {
            let mut ws = Vec::new();
            for (i, tab) in rows.iter().enumerate() {
                for row in tab {
                    ws.push(Write::Put { table: i as u32, key_len: schema[i].key_len, row: row.clone() });
                }
            }
            store_writes(&t, tid, &schema, &ws).await.unwrap();
        }
        let mut batch = Vec::new();
        for _ in 0..8 {
            let q = cases::read(&mut r);
            if h5i_app_pgsql::compile_read(&schema, 1, &q, false).is_none() {
                rejected += 1;
                continue;
            }
            batch.push(q.clone());
            let want = eval::rows(&tabs, &q);
            let got = load_read(&t, tenant, &schema, &q).await.unwrap();
            assert_eq!(got, want, "case {case}: {q:?}\ntables: {tabs:?}");
            let n = count_read(&t, tenant, &schema, &q).await.unwrap();
            assert_eq!(n, want.len() as u64, "count, case {case}: {q:?}");
            ran += 1;
            nonempty += usize::from(!want.is_empty());
        }
        // The same reads pipelined, as `schema_load_plan` runs a plan.
        let refs: Vec<&h5i_app_sql::Read> = batch.iter().collect();
        let got = load_reads(&t, tenant, &schema, &refs).await.unwrap();
        let want: Vec<_> = batch.iter().map(|q| eval::rows(&tabs, q)).collect();
        assert_eq!(got, want, "pipelined, case {case}");
        tx.rollback().await.unwrap();
    }
    eprintln!("{ran} reads compared ({nonempty} nonempty), {rejected} rejected by compile_read");
    assert!(ran > 2000 && nonempty > ran / 3, "too few reads compiled or matching: {ran}, {nonempty}");
}

/// Many read shapes through one connection with room for 8 statements: the
/// connection's cache and the server's prepared statements stay within the
/// bound, results stay those of the reference, and reads that differ only
/// in their numbers share one statement.
#[tokio::test]
async fn statement_cache_is_bounded() {
    let Ok(url) = std::env::var("H5I_APP_TEST_DATABASE_URL") else {
        eprintln!("H5I_APP_TEST_DATABASE_URL not set; skipping");
        return;
    };
    const CAP: usize = 8;
    let url = crate::with_schema(&url, "h5i_stmt_test").unwrap();
    let p = pool(&url, 1).unwrap();
    p.1.set_capacity(CAP);
    let schema = schema();
    let mut client = p.0.get().await.unwrap();
    {
        let tx = client.transaction().await.unwrap();
        tx.batch_execute("CREATE SCHEMA IF NOT EXISTS h5i_stmt_test").await.unwrap();
        for t in &schema {
            tx.batch_execute(&format!("DROP TABLE IF EXISTS \"{}\"", String::from_utf8_lossy(&t.name))).await.unwrap();
        }
        for ddl in create_tables(&schema).unwrap() {
            tx.batch_execute(&ddl).await.unwrap();
        }
        tx.commit().await.unwrap();
    }
    let mut r = cases::Rng(5);
    let tabs = cases::tables(&mut r);
    let tx = client.transaction().await.unwrap();
    let t = Tx(&tx, &p.1);
    let tenant = TenantId(1);
    let mut ws = Vec::new();
    for (i, tab) in tabs.iter().enumerate() {
        for row in tab {
            ws.push(Write::Put { table: i as u32, key_len: schema[i].key_len, row: row.clone() });
        }
    }
    store_writes(&t, tenant, &schema, &ws).await.unwrap();
    let server_count = async |tx: &deadpool_postgres::Transaction<'_>| -> i64 {
        tx.query_one("SELECT count(*) FROM pg_prepared_statements", &[]).await.unwrap().get(0)
    };
    let mut shapes = std::collections::HashSet::new();
    for _ in 0..300 {
        let q = cases::read(&mut r);
        let Some(c) = h5i_app_pgsql::compile_read(&schema, 1, &q, false) else { continue };
        shapes.insert(c.text);
        assert_eq!(load_read(&t, tenant, &schema, &q).await.unwrap(), eval::rows(&tabs, &q), "{q:?}");
        assert!(p.1.tracked(&tx.statement_cache) <= CAP);
        assert!(tx.statement_cache.size() <= CAP, "deadpool cache: {}", tx.statement_cache.size());
        // The counting query itself may be prepared while it runs.
        let n = server_count(&tx).await;
        assert!(n as usize <= CAP + 1, "{n} prepared statements on the server");
    }
    assert!(shapes.len() > 4 * CAP, "only {} distinct shapes", shapes.len());

    // Pages of one read: the text depends on the direction and on whether
    // there is a limit and a non-zero offset (at most 7 texts here, the
    // whole table included), never on the numbers themselves.
    let before = tx.statement_cache.size();
    let mut texts = std::collections::HashSet::new();
    for offset in 0..40u64 {
        for limit in [None, Some(0), Some(2)] {
            let q = h5i_app_sql::Read {
                table: 0,
                pred: h5i_app_sql::Pred::All,
                window: h5i_app_sql::Window { desc: offset % 2 == 0, offset, limit },
            };
            texts.insert(h5i_app_pgsql::compile_read(&schema, 1, &q, false).unwrap().text);
            assert_eq!(load_read(&t, tenant, &schema, &q).await.unwrap(), eval::rows(&tabs, &q));
        }
    }
    assert!(texts.len() <= 7, "{} texts for one read", texts.len());
    assert!(tx.statement_cache.size() <= before.max(CAP));
    tx.rollback().await.unwrap();
}

/// Exercise scalar membership through real SQL, including empty lookups under
/// OR, duplicate projected values from distinct compound keys, windows, and
/// another tenant's matching key. These must keep both rows and exact counts.
#[tokio::test]
async fn unique_membership_matches_the_reference() {
    use h5i_app_sql::{whole, Pred, Read, Val, Window};
    let Ok(url) = std::env::var("H5I_APP_TEST_DATABASE_URL") else {
        eprintln!("H5I_APP_TEST_DATABASE_URL not set; skipping");
        return;
    };
    let url = crate::with_schema(&url, "h5i_unique_read_test").unwrap();
    let p = pool(&url, 2).unwrap();
    let schema = schema();
    let mut client = p.0.get().await.unwrap();
    let tx = client.transaction().await.unwrap();
    tx.batch_execute("CREATE SCHEMA IF NOT EXISTS h5i_unique_read_test").await.unwrap();
    for t in &schema {
        tx.batch_execute(&format!("DROP TABLE IF EXISTS \"{}\"", String::from_utf8_lossy(&t.name))).await.unwrap();
    }
    for ddl in create_tables(&schema).unwrap() { tx.batch_execute(&ddl).await.unwrap(); }
    let t = Tx(&tx, &p.1);
    let tabs = vec![
        vec![vec![Val::Int(1), Val::Int(7), Val::Bytes(b"a".to_vec()), Val::Null],
             vec![Val::Int(2), Val::Int(8), Val::Bytes(b"b".to_vec()), Val::Int(9)]],
        vec![vec![Val::Int(1), Val::Int(7)], vec![Val::Int(1), Val::Int(8)], vec![Val::Int(2), Val::Int(7)]],
        vec![],
    ];
    for (tid, data) in [(TenantId(1), &tabs), (TenantId(2), &tabs)] {
        let mut ws = Vec::new();
        for (i, tab) in data.iter().enumerate() {
            for row in tab { ws.push(Write::Put { table: i as u32, key_len: schema[i].key_len, row: row.clone() }); }
        }
        store_writes(&t, tid, &schema, &ws).await.unwrap();
    }
    let eq = |col, n| Pred::Eq { col, val: Val::Int(n) };
    let preds = [eq(0, 1), eq(0, 99), eq(1, 7),
        Pred::And(Box::new(eq(0, 1)), Box::new(eq(1, 7))),
        Pred::Or(Box::new(eq(0, 1)), Box::new(eq(0, 2)))];
    let windows = [whole(), Window { desc: true, offset: 0, limit: Some(1) },
        Window { desc: false, offset: 1, limit: None }, Window { desc: false, offset: 0, limit: Some(0) }];
    let mut scalars = 0;
    for table in [0, 1] {
        for pred in &preds {
            for window in windows {
                let member = Pred::In { col: 1, table, pred: Box::new(pred.clone()), of: 1, window };
                for outer in [member.clone(), Pred::Or(Box::new(member.clone()), Box::new(eq(0, 2))),
                    Pred::And(Box::new(member.clone()), Box::new(eq(0, 1)))] {
                    let q = Read { table: 0, pred: outer, window: whole() };
                    let c = h5i_app_pgsql::compile_read(&schema, 1, &q, false).unwrap();
                    scalars += usize::from(String::from_utf8_lossy(&c.text).contains(" = (SELECT"));
                    let want = eval::rows(&tabs, &q);
                    assert_eq!(load_read(&t, TenantId(1), &schema, &q).await.unwrap(), want, "{q:?}");
                    assert_eq!(count_read(&t, TenantId(1), &schema, &q).await.unwrap(), want.len() as u64, "{q:?}");
                }
            }
        }
    }
    assert!(scalars >= 36, "unique lookups must exercise the scalar path");
    tx.rollback().await.unwrap();
}
