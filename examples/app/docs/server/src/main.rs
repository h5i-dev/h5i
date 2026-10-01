use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use docs_kernel::Command;
use docs_server::{principal, DocsApp, DocsStore};
use h5i_app_http::{rpc_router, Actor, HmacAuth, H5iApp};
use h5i_app_pg::{pool, Engine, EngineConfig};
use std::sync::Arc;

type App = H5iApp<DocsApp, DocsStore>;

// Ordinary axum handlers. They can only reach data through the kernel.
async fn list_documents(State(app): State<App>, actor: Actor<DocsApp>, Path(project): Path<u64>, h: HeaderMap) -> Response {
    app.respond(&actor, Command::ListDocuments { project }, &h).await
}

async fn get_document(State(app): State<App>, actor: Actor<DocsApp>, Path(doc): Path<u64>, h: HeaderMap) -> Response {
    app.respond(&actor, Command::GetDocument { doc }, &h).await
}

async fn approve(State(app): State<App>, actor: Actor<DocsApp>, Path(doc): Path<u64>, h: HeaderMap) -> Response {
    app.respond(&actor, Command::Approve { doc }, &h).await
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let url = std::env::var("DATABASE_URL")?;
    let secret = std::env::var("H5I_APP_SECRET")?;
    let addr = std::env::var("LISTEN").unwrap_or_else(|_| "127.0.0.1:8080".into());

    let auth = HmacAuth::<DocsApp>::new(secret, principal);
    if let Ok(spec) = std::env::var("H5I_APP_ISSUE") {
        // Dev helper: H5I_APP_ISSUE=<org>:<user> prints a one-day token and exits.
        let (o, u) = spec.split_once(':').ok_or("H5I_APP_ISSUE=<org>:<user>")?;
        println!("{}", auth.issue(o.parse()?, u.parse()?, 86_400));
        return Ok(());
    }

    let engine = Arc::new(Engine::<DocsApp, DocsStore>::new(pool(&url, 16)?, EngineConfig::default()));
    engine.install_schema().await?;
    let app = H5iApp::new(engine, auth);

    let router = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/projects/{id}/documents", get(list_documents))
        .route("/documents/{id}", get(get_document))
        .route("/documents/{id}/approve", post(approve))
        .with_state(app.clone())
        .merge(rpc_router(app));

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(%addr, "listening");
    axum::serve(listener, router).await?;
    Ok(())
}
