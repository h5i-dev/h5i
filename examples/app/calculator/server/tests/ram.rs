use calculator_kernel as k;
use calculator_server::executor_demo::{Calculator, CalculatorStore, LOADS};
use h5i_app_core::{Executor, MemoryEngine};
use h5i_app_pg::{Engine, EngineConfig, pool};
use std::{sync::{Arc, atomic::Ordering}, time::{SystemTime, UNIX_EPOCH}};
#[tokio::test]
async fn committed_writes_keep_warm_cache_and_replays_do_not_apply_twice() {
    let Ok(url) = std::env::var("H5I_APP_TEST_DATABASE_URL") else { return; };
    let pg = Arc::new(Engine::<Calculator, CalculatorStore>::new(pool(&h5i_app_pg::with_schema(&url,"calculator_ram").unwrap(),4).unwrap(),EngineConfig::default()));
    pg.install_schema().await.unwrap();
    let ram = h5i_app_mem::MemoryPgEngine::new(pg.clone());
    let reference = MemoryEngine::<Calculator>::default();
    let org = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64 % (1<<50);
    let a = k::Principal { org, user: 1 };
    ram.execute(&a,&k::Command::Get).await.unwrap().unwrap();
    let before = LOADS.load(Ordering::Relaxed);
    for i in 0..100 {
        let c = k::Command::Set { value: i };
        assert_eq!(ram.execute(&a,&c).await.unwrap(),reference.execute(&a,&c));
        assert_eq!(ram.execute(&a,&k::Command::Get).await.unwrap(),reference.execute(&a,&k::Command::Get));
        assert_eq!(ram.snapshot_for(&a,&c).await.unwrap(),reference.snapshot(h5i_app_core::TenantId(org)));
    }
    // One transaction load per write; no cache refills for reads/enrichment.
    assert_eq!(LOADS.load(Ordering::Relaxed)-before,100);
    let c = k::Command::Apply { op:k::Op::Add, arg:7 };
    let first = ram.execute_idempotent::<CalculatorStore>(&a,"add",&c).await.unwrap();
    let replay = ram.execute_idempotent::<CalculatorStore>(&a,"add",&c).await.unwrap();
    assert_eq!(first,replay);
    assert_eq!(ram.execute(&a,&k::Command::Get).await.unwrap(),Ok(k::Reply::Value(106)));
    assert_eq!(ram.snapshot_for(&a,&c).await.unwrap(),pg.snapshot(h5i_app_core::TenantId(org)).await.unwrap());
    let refusal = k::Command::Apply {op:k::Op::Div,arg:0};
    let before = LOADS.load(Ordering::Relaxed);
    assert_eq!(ram.execute(&a,&refusal).await.unwrap(),Err(k::Error::DivByZero));
    assert_eq!(ram.execute(&a,&k::Command::Get).await.unwrap(),Ok(k::Reply::Value(106)));
    assert_eq!(LOADS.load(Ordering::Relaxed)-before,1);
    let ram = Arc::new(ram);
    let scoped = h5i_app_pg::with_schema(&url,"calculator_ram").unwrap();
    let (mut db, connection) = tokio_postgres::connect(&scoped,tokio_postgres::NoTls).await.unwrap();
    let connection = tokio::spawn(connection);
    let tx = db.transaction().await.unwrap();
    tx.batch_execute("LOCK TABLE memories IN ACCESS EXCLUSIVE MODE").await.unwrap();
    let attempts = pg.stats.attempts.load(Ordering::Relaxed);
    let writer = {let ram=ram.clone(); tokio::spawn(async move {ram.execute(&a,&k::Command::Set{value:200}).await})};
    tokio::time::timeout(std::time::Duration::from_secs(5),async {
        while pg.stats.attempts.load(Ordering::Relaxed)==attempts {tokio::task::yield_now().await;}
    }).await.unwrap();
    writer.abort(); assert!(writer.await.unwrap_err().is_cancelled());
    tx.rollback().await.unwrap();
    let reads=pg.stats.reads.load(Ordering::Relaxed);
    ram.execute(&a,&k::Command::Get).await.unwrap().unwrap();
    assert_eq!(pg.stats.reads.load(Ordering::Relaxed),reads+1);
    ram.execute(&a,&k::Command::Set{value:300}).await.unwrap().unwrap();
    assert_eq!(ram.execute(&a,&k::Command::Get).await.unwrap(),Ok(k::Reply::Value(300)));
    assert_eq!(pg.stats.reads.load(Ordering::Relaxed),reads+2,"later commit cannot revive cancelled cache");
    drop(db);connection.await.unwrap().unwrap();

}
