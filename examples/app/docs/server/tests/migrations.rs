mod common;

use common::*;
use docs_kernel as k;
use docs_server::{principal, DocsApp, DocsStore};
use h5i_app_core::TenantId;
use h5i_app_pg::migrate::Migration;
use h5i_app_pg::{pool, DbError, Engine, EngineConfig};
use tokio_postgres::NoTls;

/// The checker runs on every tenant in the database, so this test uses its own.
async fn fresh_engine(url: &str) -> (Engine<DocsApp, DocsStore>, tokio_postgres::Client) {
    let db = "h5i_migrate_test";
    let (root, conn) = tokio_postgres::connect(url, NoTls).await.unwrap();
    tokio::spawn(conn);
    root.batch_execute(&format!("DROP DATABASE IF EXISTS {db} WITH (FORCE)")).await.unwrap();
    root.batch_execute(&format!("CREATE DATABASE {db}")).await.unwrap();
    let url = format!("{}/{db}", url.rsplit_once('/').unwrap().0);
    let engine = Engine::new(pool(&url, 4).unwrap(), EngineConfig::default());
    engine.install_schema().await.unwrap();
    let (client, conn) = tokio_postgres::connect(&url, NoTls).await.unwrap();
    tokio::spawn(conn);
    (engine, client)
}

/// A project with two owners and an approved document.
async fn populate(e: &Engine<DocsApp, DocsStore>, t: u64) {
    let (a, b) = (principal(t, 1), principal(t, 2));
    let run = |p: k::Principal, c: k::Command| async move { e.execute(&p, &c).await.unwrap().unwrap() };
    let k::Reply::Created(p) = run(a, k::Command::CreateProject { name: b"p".to_vec() }).await else { panic!() };
    run(a, k::Command::SetMember { project: p, user: 2, role: k::Role::Owner }).await;
    let k::Reply::Created(d) = run(a, k::Command::CreateDocument { project: p, title: vec![], body: vec![] }).await else {
        panic!()
    };
    run(a, k::Command::Submit { doc: d }).await;
    run(b, k::Command::Approve { doc: d }).await;
}

async fn applied(c: &tokio_postgres::Client) -> Vec<String> {
    c.query("SELECT id FROM h5i_migrations ORDER BY applied_at, id", &[]).await.unwrap().iter().map(|r| r.get(0)).collect()
}

#[tokio::test]
async fn migrations_are_checked_before_commit() {
    let Ok(url) = std::env::var("H5I_APP_TEST_DATABASE_URL") else {
        eprintln!("H5I_APP_TEST_DATABASE_URL not set; skipping");
        return;
    };
    let (e, c) = fresh_engine(&url).await;
    let (t1, t2) = (fresh_tenant(), fresh_tenant());
    populate(&e, t1).await;
    populate(&e, t2).await;

    // A harmless migration applies once.
    let index = Migration { id: "001_members_by_user", sql: "CREATE INDEX members_by_user ON members (tenant_id, \"user\")" };
    let r = e.migrate(&[index], k::check_inv).await.unwrap();
    assert_eq!((r.applied, r.tenants_checked), (vec!["001_members_by_user"], 2));
    assert!(e.migrate(&[index], k::check_inv).await.unwrap().applied.is_empty());

    // Removing every owner breaks the invariants: rolled back, not recorded.
    let before = normalize(e.snapshot(TenantId(t1)).await.unwrap());
    let drop_owners = Migration { id: "002_drop_owners", sql: "DELETE FROM members WHERE role = 2" };
    let err = e.migrate(&[index, drop_owners], k::check_inv).await.unwrap_err();
    assert!(matches!(err, DbError::InvariantViolated { .. }), "{err}");
    assert_eq!(normalize(e.snapshot(TenantId(t1)).await.unwrap()), before);
    assert_eq!(applied(&c).await, vec!["001_members_by_user"]);

    // So does a quiet data fix that drops approvers of approved documents.
    let bad_fix = Migration { id: "003_clear_approvers", sql: "UPDATE documents SET approver = NULL" };
    assert!(matches!(e.migrate(&[bad_fix], k::check_inv).await, Err(DbError::InvariantViolated { .. })));
    assert_eq!(applied(&c).await, vec!["001_members_by_user"]);
}
