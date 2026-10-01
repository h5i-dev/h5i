use std::sync::Arc;
use wastebin_server::{config, purge, router, App, Shell, WastebinEngine};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let url = std::env::var("DATABASE_URL")?;
    let secret = std::env::var("WASTEBIN_SIGNING_KEY")?;
    let addr = std::env::var("LISTEN").unwrap_or_else(|_| "127.0.0.1:8088".into());

    let engine = Arc::new(WastebinEngine::new(h5i_app_pg::pool(&h5i_app_pg::with_schema(&url, "wastebin")?, 8)?, config()));
    engine.install_schema().await?;

    // `wastebin-server purge` deletes expired pastes and exits.
    if std::env::args().nth(1).as_deref() == Some("purge") {
        purge(&engine).await?;
        return Ok(());
    }

    let app = App { engine, shell: Arc::new(Shell::new(secret.into_bytes())) };
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(%addr, "listening");
    axum::serve(listener, router(app)).await?;
    Ok(())
}
