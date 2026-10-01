use cratesio_server::{config, parse_teams, router, CratesAuth, CratesStore, Cratesio};
use h5i_app_pg::{pool, Engine};
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let secret = std::env::var("H5I_APP_SECRET")?;
    let addr = std::env::var("LISTEN").unwrap_or_else(|_| "127.0.0.1:8080".into());
    // H5I_APP_TEAMS=2:7,8 says user 2 is in GitHub teams 7 and 8.
    let teams = parse_teams(&std::env::var("H5I_APP_TEAMS").unwrap_or_default())?;
    // H5I_APP_DOWNLOADS=10:2500 says crate 10 has 2500 downloads.
    let downloads = parse_teams(&std::env::var("H5I_APP_DOWNLOADS").unwrap_or_default())?
        .into_iter()
        .map(|(k, v)| (k, v.first().copied().unwrap_or(0)))
        .collect::<HashMap<_, _>>();
    let registry = std::env::var("H5I_APP_REGISTRY").map(|r| r.parse()).unwrap_or(Ok(1))?;
    let auth = Arc::new(CratesAuth::new(&secret, registry, teams));

    if let Ok(spec) = std::env::var("H5I_APP_ISSUE") {
        // H5I_APP_ISSUE=github:<user> prints what the OAuth callback would hand
        // over for that user; H5I_APP_ISSUE=operator prints an operator token.
        match spec.split_once(':') {
            Some(("github", user)) => println!("{}", auth.github_login(user.parse()?)),
            _ if spec == "operator" => println!("{}", auth.operator()),
            _ => return Err("H5I_APP_ISSUE=github:<user> or H5I_APP_ISSUE=operator".into()),
        }
        return Ok(());
    }

    let url = std::env::var("DATABASE_URL")?;
    let engine = Arc::new(Engine::<Cratesio, CratesStore>::new(pool(&h5i_app_pg::with_schema(&url, "cratesio")?, 8)?, config()));
    engine.install_schema().await?;
    let app = router(engine, auth, Arc::new(downloads)).route("/healthz", axum::routing::get(|| async { "ok" }));

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(%addr, "listening");
    axum::serve(listener, app).await?;
    Ok(())
}
