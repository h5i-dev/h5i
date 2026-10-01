use axum::routing::get;
use axum::Router;
use conduit_server::api::{router, App, ConduitAuth};
use conduit_server::{Conduit, ConduitStore};
use h5i_app_pg::{pool, Engine, EngineConfig};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let url = std::env::var("DATABASE_URL")?;
    let secret = std::env::var("H5I_APP_SECRET")?;
    let addr = std::env::var("LISTEN").unwrap_or_else(|_| "127.0.0.1:8080".into());

    let engine = Arc::new(Engine::<Conduit, ConduitStore>::new(pool(&h5i_app_pg::with_schema(&url, "conduit")?, 8)?, EngineConfig::default()));
    engine.install_schema().await?;
    let app = App { engine, auth: Arc::new(ConduitAuth::new(secret.clone())), secret: Arc::new(secret.into_bytes()) };
    let router = Router::new().route("/healthz", get(|| async { "ok" })).merge(router(app));

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(%addr, "listening");
    axum::serve(listener, router).await?;
    Ok(())
}
