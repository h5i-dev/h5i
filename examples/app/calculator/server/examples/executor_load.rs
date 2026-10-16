//! Direct executor microbenchmark; no HTTP. A write includes snapshot enrichment.
use calculator_kernel as k;
use calculator_server::executor_demo::{Calculator, CalculatorStore, LOADS};
use h5i_app_core::{Executor,TenantId};
use h5i_app_pg::{Engine, EngineConfig, pool};
use std::{sync::{Arc,atomic::Ordering},time::{Duration,Instant}};
async fn measure<E: Executor<Calculator,Error=h5i_app_pg::DbError> + 'static>(e:Arc<E>, org:u64, write:bool, concurrency:usize) {
    // Warm the full tenant once; the measurement concerns steady-state maintenance.
    e.snapshot_for(&k::Principal {org,user:1},&k::Command::Get).await.unwrap();
    let loads = LOADS.load(Ordering::Relaxed);
    let start=Instant::now();
    let end=start+Duration::from_secs(3);
    let mut tasks=Vec::new();
    for worker in 0..concurrency {
        let e=e.clone();
        tasks.push(tokio::spawn(async move {
            let a=k::Principal {org,user:1+worker as u64};
            let mut n=0;
            while Instant::now()<end {
                let c=if write {k::Command::Set{value:n}} else {k::Command::Get};
                e.execute(&a,&c).await.unwrap().unwrap();
                if write { e.snapshot_for(&a,&c).await.unwrap(); }
                n+=1;
            }
            n
        }));
    }
    let mut n=0;
    for t in tasks {n+=t.await.unwrap();}
    println!("{} c={} req/s={:.1} requests={} loads={}",if write {"set+snapshot"} else {"get"},concurrency,n as f64/start.elapsed().as_secs_f64(),n,LOADS.load(Ordering::Relaxed)-loads);
}
#[tokio::main(flavor="multi_thread",worker_threads=16)]
async fn main() {
    let url=std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pg=Arc::new(Engine::<Calculator,CalculatorStore>::new(pool(&h5i_app_pg::with_schema(&url,"calculator_load").unwrap(),32).unwrap(),EngineConfig::default()));
    pg.install_schema().await.unwrap();
    let org=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64 % (1<<50);
    // Disjoint tenants: the PG baseline must never write the RAM-owned tenant.
    for tenant in [org, org+1] { for user in 1..=64 {pg.execute(&k::Principal{org:tenant,user},&k::Command::Set{value:user}).await.unwrap().unwrap();} }
    let ram=Arc::new(h5i_app_mem::MemoryPgEngine::new(pg.clone()));
    for write in [false,true] {for c in [1,16,64] {
        print!("pg ");measure(pg.clone(),org,write,c).await;
        print!("ram ");measure(ram.clone(),org+1,write,c).await;
        assert_eq!(ram.snapshot_for(&k::Principal{org:org+1,user:1},&k::Command::Get).await.unwrap(),pg.snapshot(TenantId(org+1)).await.unwrap());
    }}
}
