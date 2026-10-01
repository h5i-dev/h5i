mod common;

use common::*;
use docs_kernel as k;
use docs_server::{effect_payload, principal};
use h5i_app_core::TenantId;
use h5i_app_pg::outbox::{Deliver, Delivery, DispatchConfig};
use h5i_app_pg::EngineConfig;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio_postgres::NoTls;

// A dispatcher claims every tenant's rows, so these tests run one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Records deliveries for one tenant; can fail the first attempt.
#[derive(Clone, Default)]
struct Recorder {
    tenant: u64,
    fail_first: bool,
    seen: Arc<Mutex<Vec<(String, Delivery)>>>,
}

impl Deliver<String> for Recorder {
    async fn deliver(&self, endpoint: &String, d: &Delivery) -> Result<(), String> {
        if d.tenant == TenantId(self.tenant) {
            self.seen.lock().unwrap().push((endpoint.clone(), d.clone()));
            if self.fail_first && d.attempt == 1 {
                return Err("receiver down".into());
            }
        }
        Ok(())
    }
}

/// Publish an approved document in a fresh tenant whose project uses `dest`.
async fn published(pg: &PgEngine, dest: Option<u64>) -> (u64, k::Effect) {
    let t = fresh_tenant();
    let (a, b) = (principal(t, 1), principal(t, 2));
    let run = |p: k::Principal, c: k::Command| async move { pg.execute(&p, &c).await.unwrap().unwrap() };
    let k::Reply::Created(p) = run(a, k::Command::CreateProject { name: vec![] }).await else { panic!() };
    run(a, k::Command::SetMember { project: p, user: 2, role: k::Role::Owner }).await;
    if dest.is_some() {
        run(a, k::Command::SetWebhook { project: p, dest }).await;
    }
    let k::Reply::Created(d) = run(a, k::Command::CreateDocument { project: p, title: vec![], body: vec![] }).await else {
        panic!()
    };
    run(a, k::Command::Submit { doc: d }).await;
    run(b, k::Command::Approve { doc: d }).await;
    let k::Reply::Version(v) = run(a, k::Command::Publish { doc: d }).await else { panic!() };
    (t, k::Effect { dest: dest.unwrap_or(0), project: p, doc: d, version: v })
}

/// (delivered, dead, attempts) for each outbox row of a tenant.
async fn rows(t: u64) -> Vec<(bool, bool, i32)> {
    let url = std::env::var("H5I_APP_TEST_DATABASE_URL").unwrap();
    let (c, conn) = tokio_postgres::connect(&url, NoTls).await.unwrap();
    tokio::spawn(conn);
    c.query("SELECT delivered_at IS NOT NULL, dead, attempts FROM h5i_outbox WHERE tenant_id = $1 ORDER BY id", &[&(t as i64)])
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2)))
        .collect()
}

fn registry() -> HashMap<u64, String> {
    HashMap::from([(7, "https://hooks.example/7".to_string())])
}

macro_rules! engine_or_skip {
    () => {
        match engine(EngineConfig::default()).await {
            Some(e) => e,
            None => {
                eprintln!("H5I_APP_TEST_DATABASE_URL not set; skipping");
                return;
            }
        }
    };
}

#[tokio::test]
async fn publish_delivers_once_to_the_registered_endpoint() {
    let _serial = SERIAL.lock().await;
    let pg = engine_or_skip!();
    let (t, effect) = published(&pg, Some(7)).await;
    assert_eq!(rows(t).await, vec![(false, false, 0)]);

    let rec = Recorder { tenant: t, ..Default::default() };
    let disp = pg.dispatcher(registry(), rec.clone(), DispatchConfig::default());
    disp.run_once().await.unwrap();
    disp.run_once().await.unwrap();

    let seen = rec.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "delivered exactly once");
    assert_eq!(seen[0].0, "https://hooks.example/7");
    assert_eq!(seen[0].1.payload, effect_payload(&effect));
    assert_eq!(rows(t).await, vec![(true, false, 1)]);
}

#[tokio::test]
async fn failed_delivery_is_retried_with_the_same_key() {
    let _serial = SERIAL.lock().await;
    let pg = engine_or_skip!();
    let (t, _) = published(&pg, Some(7)).await;
    let rec = Recorder { tenant: t, fail_first: true, ..Default::default() };
    let disp = pg.dispatcher(registry(), rec.clone(), DispatchConfig::default());
    disp.run_once().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    disp.run_once().await.unwrap();

    let seen = rec.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].1.key, seen[1].1.key, "receivers can drop the duplicate");
    assert_eq!((seen[0].1.attempt, seen[1].1.attempt), (1, 2));
    assert_eq!(rows(t).await, vec![(true, false, 2)]);
}

/// An id the operator did not register is never sent anywhere.
#[tokio::test]
async fn unknown_destination_is_never_contacted() {
    let _serial = SERIAL.lock().await;
    let pg = engine_or_skip!();
    let (t, _) = published(&pg, Some(99)).await;
    let rec = Recorder { tenant: t, ..Default::default() };
    pg.dispatcher(registry(), rec.clone(), DispatchConfig::default()).run_once().await.unwrap();
    assert!(rec.seen.lock().unwrap().is_empty());
    assert_eq!(rows(t).await, vec![(false, true, 1)]);
}

#[tokio::test]
async fn no_webhook_no_effect() {
    let _serial = SERIAL.lock().await;
    let pg = engine_or_skip!();
    let (t, _) = published(&pg, None).await;
    assert!(rows(t).await.is_empty());
}
