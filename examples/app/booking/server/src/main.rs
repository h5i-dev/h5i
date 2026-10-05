use axum::routing::get;
use axum::Router;
use booking_server::{config, parse_registry, principal, BookingApp, BookingStore, Notifier};
use h5i_app_http::{rpc_router, HmacAuth, H5iApp};
use h5i_app_pg::outbox::DispatchConfig;
use h5i_app_pg::{pool, Engine};
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn,booking_server=info".into()))
        .init();
    let url = std::env::var("DATABASE_URL")?;
    let secret = std::env::var("H5I_APP_SECRET")?;
    let addr = std::env::var("LISTEN").unwrap_or_else(|_| "127.0.0.1:8080".into());
    // The operator's destinations, e.g. `7=log,8=http://127.0.0.1:9000/hooks`.
    let registry = parse_registry(&std::env::var("BOOKING_DESTINATIONS").unwrap_or_else(|_| "7=log".into()))?;

    let auth = HmacAuth::<BookingApp>::new(secret, principal);
    if let Ok(spec) = std::env::var("H5I_APP_ISSUE") {
        // H5I_APP_ISSUE=<org>:<user> prints a one-day token and exits.
        let (o, u) = spec.split_once(':').ok_or("H5I_APP_ISSUE=<org>:<user>")?;
        println!("{}", auth.issue(o.parse()?, u.parse()?, 86_400));
        return Ok(());
    }

    let engine = Arc::new(Engine::<BookingApp, BookingStore>::new(pool(&h5i_app_pg::with_schema(&url, "booking")?, 8)?, config()));
    engine.install_schema().await?;

    // Send committed notifications once a second.
    let dispatcher = engine.dispatcher(registry, Notifier::default(), DispatchConfig::default());
    tokio::spawn(async move {
        loop {
            match dispatcher.run_once().await {
                Ok(pass) if pass != Default::default() => tracing::info!(?pass, "outbox"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "outbox pass failed"),
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });

    let app = H5iApp::new(engine, auth);
    let router = Router::new().route("/healthz", get(|| async { "ok" })).merge(rpc_router(app));
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(%addr, "listening");
    axum::serve(listener, router).await?;
    Ok(())
}
