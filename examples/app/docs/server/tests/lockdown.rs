mod common;

use common::*;
use docs_kernel as k;
use docs_server::{principal, DocsApp, DocsStore};
use tokio_postgres::{error::SqlState, NoTls};
use h5i_app_pg::{lockdown, pool, Engine, EngineConfig};

async fn connect(url: &str) -> tokio_postgres::Client {
    let (c, conn) = tokio_postgres::connect(url, NoTls).await.unwrap();
    tokio::spawn(conn);
    c
}

/// Same server as `url`, other database; `user` logs in with password = name.
fn with_db(url: &str, user: Option<&str>, db: &str) -> String {
    let (creds, rest) = url.trim_start_matches("postgres://").split_once('@').unwrap();
    let host = rest.rsplit_once('/').unwrap().0;
    let creds = user.map(|u| format!("{u}:{u}")).unwrap_or(creds.to_string());
    format!("postgres://{creds}@{host}/{db}")
}

/// Only the engine role can write h5i-app tables after `lockdown`; another login role cannot.
#[tokio::test]
async fn only_engine_role_can_write() {
    let Ok(url) = std::env::var("H5I_APP_TEST_DATABASE_URL") else {
        eprintln!("H5I_APP_TEST_DATABASE_URL not set; skipping");
        return;
    };
    let db = "h5i_lockdown_test";
    let root = connect(&url).await;
    root.batch_execute(&format!("DROP DATABASE IF EXISTS {db} WITH (FORCE)")).await.unwrap();
    root.batch_execute(&format!("CREATE DATABASE {db}")).await.unwrap();
    for role in ["h5i_engine", "h5i_other"] {
        root.batch_execute(&format!(
            "DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{role}') \
             THEN CREATE ROLE {role} LOGIN PASSWORD '{role}'; END IF; END $$"
        ))
        .await
        .unwrap();
    }

    // Admin creates the schema and locks it down; twice, to check idempotence.
    let admin_url = with_db(&url, None, db);
    let admin_engine = Engine::<DocsApp, DocsStore>::new(pool(&admin_url, 2).unwrap(), EngineConfig::default());
    admin_engine.install_schema().await.unwrap();
    for _ in 0..2 {
        lockdown::<DocsApp, DocsStore>(&admin_url, "h5i_owner", "h5i_engine").await.unwrap();
    }

    // The engine role works end to end.
    let engine = Engine::<DocsApp, DocsStore>::new(pool(&with_db(&url, Some("h5i_engine"), db), 2).unwrap(), EngineConfig::default());
    let t = fresh_tenant();
    let (a, b) = (principal(t, 1), principal(t, 2));
    let run = |p: k::Principal, c: k::Command| {
        let engine = &engine;
        async move { engine.execute(&p, &c).await.unwrap().unwrap() }
    };
    let k::Reply::Created(p) = run(a, k::Command::CreateProject { name: b"p".to_vec() }).await else { panic!() };
    // Publishing with a webhook also inserts into the outbox as the engine role.
    run(a, k::Command::SetMember { project: p, user: 2, role: k::Role::Owner }).await;
    run(a, k::Command::SetWebhook { project: p, dest: Some(1) }).await;
    let k::Reply::Created(d) = run(a, k::Command::CreateDocument { project: p, title: vec![], body: vec![] }).await else {
        panic!()
    };
    run(a, k::Command::Submit { doc: d }).await;
    run(b, k::Command::Approve { doc: d }).await;
    run(a, k::Command::Publish { doc: d }).await;

    // Any other role is refused, for writes and reads.
    let other = connect(&with_db(&url, Some("h5i_other"), db)).await;
    for sql in [
        "INSERT INTO projects (tenant_id, id, name) VALUES (1, 1, 'x')",
        "UPDATE documents SET body = 'x'",
        "DELETE FROM members",
        "SELECT * FROM h5i_idempotency",
        "SELECT * FROM h5i_outbox",
    ] {
        let err = other.batch_execute(sql).await.unwrap_err();
        assert_eq!(err.code(), Some(&SqlState::INSUFFICIENT_PRIVILEGE), "{sql}");
    }

    // A bad role name is rejected before any SQL runs.
    assert!(lockdown::<DocsApp, DocsStore>(&admin_url, "x; DROP TABLE projects", "h5i_engine").await.is_err());

}
