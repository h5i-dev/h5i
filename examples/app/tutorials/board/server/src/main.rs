use axum::routing::get;
use axum::Router;
use board_server::{principal, Board, BoardStore};
use h5i_app_http::{rpc_router, HmacAuth, H5iApp};
use h5i_app_pg::{pool, Engine, EngineConfig};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let url = std::env::var("DATABASE_URL")?;
    let secret = std::env::var("H5I_APP_SECRET")?;
    let addr = std::env::var("LISTEN").unwrap_or_else(|_| "127.0.0.1:8080".into());

    let auth = HmacAuth::<Board>::new(secret, principal);
    if let Ok(spec) = std::env::var("H5I_APP_ISSUE") {
        // H5I_APP_ISSUE=<org>:<user> prints a one-day token and exits.
        let (o, u) = spec.split_once(':').ok_or("H5I_APP_ISSUE=<org>:<user>")?;
        println!("{}", auth.issue(o.parse()?, u.parse()?, 86_400));
        return Ok(());
    }

    let engine = Arc::new(Engine::<Board, BoardStore>::new(pool(&h5i_app_pg::with_schema(&url, "board")?, 8)?, EngineConfig::default()));
    engine.install_schema().await?;
    let app = H5iApp::new(engine, auth);
    let router = Router::new().route("/healthz", get(|| async { "ok" })).merge(rpc_router(app));

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(%addr, "listening");
    axum::serve(listener, router).await?;
    Ok(())
}
