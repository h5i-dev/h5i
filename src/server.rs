use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    extract::{Query, State},
    response::{Html, Json},
    routing::get,
    Router,
};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

use crate::metadata::IntegrityReport;
use crate::repository::H5iRepository;

// ── Shared state ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct AppState {
    pub repo_path: PathBuf,
}

// ── API response types ────────────────────────────────────────────────────────

#[derive(Serialize, Default)]
pub struct EnrichedCommit {
    pub git_oid: String,
    pub short_oid: String,
    pub message: String,
    pub author: String,
    pub timestamp: String,
    pub ai_model: Option<String>,
    pub ai_agent: Option<String>,
    pub ai_prompt: Option<String>,
    pub ai_tokens: Option<usize>,
    pub test_coverage: Option<f64>,
    pub ast_file_count: usize,
    pub has_crdt: bool,
}

// ── Query params ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct LogQuery {
    pub limit: Option<usize>,
}

#[derive(Deserialize)]
pub struct IntegrityQuery {
    pub message: Option<String>,
    pub prompt: Option<String>,
}

// ── Handlers ──────────────────────────────────────────────────────────────────

async fn index() -> Html<&'static str> {
    Html(FRONTEND_HTML)
}

async fn api_repo(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let path = state.repo_path.clone();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<serde_json::Value> {
        let repo = H5iRepository::open(&path)?;
        let git = repo.git();

        let branch = git
            .head()
            .ok()
            .and_then(|h| h.shorthand().map(String::from))
            .unwrap_or_else(|| "HEAD".to_string());

        let name = git
            .workdir()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        let records = repo.get_log(2000)?;
        let total = records.len();
        let ai = records.iter().filter(|r| r.ai_metadata.is_some()).count();

        Ok(serde_json::json!({
            "name": name,
            "branch": branch,
            "total_commits": total,
            "ai_commits": ai,
        }))
    })
    .await;

    Json(result.unwrap_or_else(|_| Ok(serde_json::json!({}))).unwrap_or_default())
}

async fn api_commits(
    State(state): State<Arc<AppState>>,
    Query(params): Query<LogQuery>,
) -> Json<Vec<EnrichedCommit>> {
    let path = state.repo_path.clone();
    let limit = params.limit.unwrap_or(100);

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<EnrichedCommit>> {
        let repo = H5iRepository::open(&path)?;
        let records = repo.get_log(limit)?;
        let mut enriched = Vec::new();

        for record in records {
            let oid = git2::Oid::from_str(&record.git_oid)?;
            let commit = repo.git().find_commit(oid)?;

            let message = commit.message().unwrap_or("").trim().to_string();
            let author = commit.author().name().unwrap_or("Unknown").to_string();
            let short_oid = record.git_oid[..8.min(record.git_oid.len())].to_string();
            let timestamp = record.timestamp.to_rfc3339();

            let (ai_model, ai_agent, ai_prompt, ai_tokens) =
                if let Some(ai) = &record.ai_metadata {
                    let tokens = ai.usage.as_ref().map(|u| u.total_tokens);
                    (
                        Some(ai.model_name.clone()),
                        Some(ai.agent_id.clone()),
                        Some(ai.prompt.clone()),
                        tokens,
                    )
                } else {
                    (None, None, None, None)
                };

            let test_coverage = record.test_metrics.as_ref().map(|t| t.coverage);
            let ast_file_count = record.ast_hashes.as_ref().map(|h| h.len()).unwrap_or(0);
            let has_crdt = record
                .crdt_states
                .as_ref()
                .map(|s| !s.is_empty())
                .unwrap_or(false);

            enriched.push(EnrichedCommit {
                git_oid: record.git_oid,
                short_oid,
                message,
                author,
                timestamp,
                ai_model,
                ai_agent,
                ai_prompt,
                ai_tokens,
                test_coverage,
                ast_file_count,
                has_crdt,
            });
        }

        Ok(enriched)
    })
    .await;

    Json(result.unwrap_or_else(|_| Ok(vec![])).unwrap_or_default())
}

async fn api_integrity(
    State(state): State<Arc<AppState>>,
    Query(params): Query<IntegrityQuery>,
) -> Json<IntegrityReport> {
    let path = state.repo_path.clone();
    let message = params.message.unwrap_or_default();
    let prompt = params.prompt;

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<IntegrityReport> {
        let repo = H5iRepository::open(&path)?;
        Ok(repo.verify_integrity(prompt.as_deref(), &message)?)
    })
    .await;

    Json(
        result
            .unwrap_or_else(|_| {
                Ok(IntegrityReport {
                    level: crate::metadata::IntegrityLevel::Valid,
                    score: 1.0,
                    findings: vec![],
                })
            })
            .unwrap_or(IntegrityReport {
                level: crate::metadata::IntegrityLevel::Valid,
                score: 1.0,
                findings: vec![],
            }),
    )
}

// ── Server entry point ────────────────────────────────────────────────────────

pub async fn serve(repo_path: PathBuf, port: u16) -> anyhow::Result<()> {
    let state = Arc::new(AppState { repo_path });

    let app = Router::new()
        .route("/", get(index))
        .route("/api/repo", get(api_repo))
        .route("/api/commits", get(api_commits))
        .route("/api/integrity", get(api_integrity))
        .with_state(state);

    let addr = format!("127.0.0.1:{}", port);
    let listener = TcpListener::bind(&addr).await?;
    println!("  h5i UI →  http://{}", addr);
    axum::serve(listener, app).await?;
    Ok(())
}

// ── Embedded frontend ─────────────────────────────────────────────────────────

pub const FRONTEND_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>h5i — 5D Git Dashboard</title>
<style>
/* === Reset === */
*, *::before, *::after { box-sizing: border-box; margin: 0; padding: 0; }
html { font-size: 14px; scroll-behavior: smooth; }
body {
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", "Noto Sans", Helvetica, Arial, sans-serif;
  background: #0d1117;
  color: #e6edf3;
  min-height: 100vh;
  line-height: 1.5;
}

/* === Header === */
.header {
  background: #161b22;
  border-bottom: 1px solid #30363d;
  padding: 0 24px;
  display: flex;
  align-items: center;
  gap: 12px;
  height: 56px;
  position: sticky;
  top: 0;
  z-index: 100;
  backdrop-filter: blur(8px);
}
.logo {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 17px;
  font-weight: 700;
  color: #e6edf3;
  text-decoration: none;
  letter-spacing: -0.02em;
}
.logo-icon {
  width: 30px;
  height: 30px;
  background: linear-gradient(135deg, #bc8cff 0%, #58a6ff 100%);
  border-radius: 7px;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 13px;
  font-weight: 800;
  color: #fff;
  box-shadow: 0 0 12px #bc8cff44;
}
.header-sep { color: #30363d; font-size: 22px; margin: 0 2px; }
.repo-name { color: #58a6ff; font-size: 15px; font-weight: 600; }
.branch-badge {
  background: #21262d;
  border: 1px solid #30363d;
  border-radius: 20px;
  padding: 2px 10px;
  font-size: 11px;
  color: #8b949e;
  font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
}
.header-spacer { flex: 1; }
.btn-refresh {
  background: transparent;
  border: 1px solid #30363d;
  color: #8b949e;
  padding: 5px 12px;
  border-radius: 6px;
  cursor: pointer;
  font-size: 12px;
  font-weight: 500;
  transition: all 0.15s;
  display: flex;
  align-items: center;
  gap: 5px;
}
.btn-refresh:hover { background: #21262d; border-color: #58a6ff; color: #e6edf3; }

/* === Stats Bar === */
.stats-bar {
  background: #0d1117;
  border-bottom: 1px solid #21262d;
  padding: 8px 24px;
  display: flex;
  gap: 0;
  align-items: center;
  overflow-x: auto;
}
.stat-item {
  display: flex;
  align-items: center;
  gap: 7px;
  padding: 4px 20px 4px 0;
  border-right: 1px solid #21262d;
  margin-right: 20px;
}
.stat-item:last-child { border-right: none; margin-right: 0; }
.stat-dot { width: 8px; height: 8px; border-radius: 50%; flex-shrink: 0; }
.dot-purple { background: #bc8cff; box-shadow: 0 0 6px #bc8cff88; }
.dot-green  { background: #3fb950; box-shadow: 0 0 6px #3fb95088; }
.dot-blue   { background: #58a6ff; box-shadow: 0 0 6px #58a6ff88; }
.dot-orange { background: #f0883e; box-shadow: 0 0 6px #f0883e88; }
.stat-label { color: #6e7681; font-size: 12px; }
.stat-value { color: #e6edf3; font-size: 13px; font-weight: 600; }

/* === Layout === */
.main {
  display: flex;
  max-width: 1280px;
  margin: 0 auto;
  width: 100%;
  padding: 24px;
  gap: 20px;
  min-height: calc(100vh - 112px);
}

/* === Sidebar === */
.sidebar { width: 210px; flex-shrink: 0; }
.sidebar-card {
  background: #161b22;
  border: 1px solid #30363d;
  border-radius: 8px;
  padding: 14px;
  margin-bottom: 12px;
}
.sidebar-title {
  font-size: 10px;
  font-weight: 700;
  color: #6e7681;
  text-transform: uppercase;
  letter-spacing: 0.1em;
  margin-bottom: 12px;
}
.sidebar-metric {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 5px 0;
  font-size: 12px;
  border-bottom: 1px solid #21262d;
}
.sidebar-metric:last-child { border-bottom: none; }
.sidebar-metric-label { color: #8b949e; }
.sidebar-metric-value { font-weight: 600; font-size: 13px; }
.dim-row {
  display: flex;
  align-items: center;
  gap: 7px;
  padding: 5px 0;
  font-size: 12px;
  border-bottom: 1px solid #21262d;
}
.dim-row:last-child { border-bottom: none; }
.dim-icon { font-size: 13px; width: 18px; text-align: center; }
.dim-label { color: #8b949e; flex: 1; }
.dim-tag {
  font-size: 10px;
  font-weight: 600;
  padding: 1px 7px;
  border-radius: 10px;
  letter-spacing: 0.03em;
}

/* === Content === */
.content { flex: 1; min-width: 0; }

/* === Tabs === */
.nav-tabs {
  display: flex;
  border-bottom: 1px solid #30363d;
  margin-bottom: 18px;
}
.nav-tab {
  padding: 8px 16px;
  font-size: 13px;
  color: #8b949e;
  cursor: pointer;
  border-bottom: 2px solid transparent;
  transition: all 0.15s;
  font-weight: 500;
  display: flex;
  align-items: center;
  gap: 6px;
}
.nav-tab:hover { color: #c9d1d9; }
.nav-tab.active { color: #e6edf3; border-bottom-color: #f78166; }
.tab-count {
  background: #30363d;
  color: #8b949e;
  font-size: 10px;
  padding: 1px 6px;
  border-radius: 10px;
  font-weight: 600;
}
.nav-tab.active .tab-count { background: #f7816622; color: #f78166; }

/* === Search / Filter === */
.search-row {
  display: flex;
  gap: 8px;
  margin-bottom: 16px;
  align-items: center;
}
.search-input {
  flex: 1;
  background: #161b22;
  border: 1px solid #30363d;
  color: #e6edf3;
  padding: 7px 12px;
  border-radius: 6px;
  font-size: 13px;
  outline: none;
  transition: border-color 0.15s;
}
.search-input:focus { border-color: #58a6ff; box-shadow: 0 0 0 3px #58a6ff15; }
.search-input::placeholder { color: #6e7681; }
.filter-pill {
  background: #161b22;
  border: 1px solid #30363d;
  color: #8b949e;
  padding: 5px 10px;
  border-radius: 6px;
  font-size: 12px;
  cursor: pointer;
  transition: all 0.15s;
  white-space: nowrap;
}
.filter-pill:hover { background: #21262d; border-color: #58a6ff; color: #e6edf3; }
.filter-pill.active { background: #1f6feb22; border-color: #1f6feb; color: #58a6ff; }

/* === Timeline === */
.timeline-wrap { position: relative; }
.timeline-line {
  position: absolute;
  left: 15px;
  top: 8px;
  bottom: 8px;
  width: 2px;
  background: linear-gradient(180deg, #bc8cff33 0%, #58a6ff22 100%);
  border-radius: 1px;
}
.commit-entry {
  position: relative;
  padding-left: 44px;
  margin-bottom: 10px;
}
.commit-dot {
  position: absolute;
  left: 7px;
  top: 15px;
  width: 18px;
  height: 18px;
  border-radius: 50%;
  border: 2px solid #bc8cff;
  background: #0d1117;
  z-index: 1;
  transition: transform 0.15s, box-shadow 0.15s;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 8px;
}
.commit-dot.ai-dot { border-color: #bc8cff; box-shadow: 0 0 0 0 #bc8cff44; }
.commit-dot.human-dot { border-color: #58a6ff; }
.commit-entry:hover .commit-dot { transform: scale(1.25); box-shadow: 0 0 0 4px #bc8cff22; }
.commit-card {
  background: #161b22;
  border: 1px solid #30363d;
  border-radius: 8px;
  padding: 13px 16px;
  cursor: pointer;
  transition: border-color 0.15s, background 0.15s, transform 0.1s;
}
.commit-card:hover {
  background: #1c2128;
  border-color: #388bfd44;
  transform: translateX(1px);
}
.commit-card.expanded { border-color: #388bfd66; }
.commit-head {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  margin-bottom: 7px;
}
.oid-chip {
  font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
  font-size: 11px;
  padding: 2px 7px;
  border-radius: 5px;
  flex-shrink: 0;
  margin-top: 1px;
}
.oid-ai { color: #d2a8ff; background: #bc8cff15; border: 1px solid #bc8cff30; }
.oid-human { color: #79c0ff; background: #58a6ff15; border: 1px solid #58a6ff30; }
.commit-msg {
  font-size: 14px;
  font-weight: 600;
  color: #e6edf3;
  flex: 1;
  line-height: 1.45;
  word-break: break-word;
}
.commit-byline {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
  font-size: 12px;
  color: #6e7681;
  margin-bottom: 9px;
}
.commit-author { color: #79c0ff; font-weight: 500; }
.commit-time::before { content: "·"; margin-right: 4px; }

/* === Badges === */
.badges { display: flex; flex-wrap: wrap; gap: 5px; margin-top: 5px; }
.badge {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 2px 8px;
  border-radius: 20px;
  font-size: 11px;
  font-weight: 500;
  border: 1px solid;
  white-space: nowrap;
}
.b-model  { color: #d2a8ff; background: #bc8cff0d; border-color: #bc8cff30; }
.b-agent  { color: #ffa657; background: #f0883e0d; border-color: #f0883e30; }
.b-test-ok{ color: #56d364; background: #3fb9500d; border-color: #3fb95030; }
.b-test-no{ color: #ff7b72; background: #f851490d; border-color: #f8514930; }
.b-ast    { color: #39d353; background: #39d3530d; border-color: #39d35330; }
.b-crdt   { color: #79c0ff; background: #58a6ff0d; border-color: #58a6ff30; }
.b-tok    { color: #6e7681; background: #6e76810d; border-color: #6e768130; }

/* === Expanded detail === */
.commit-detail {
  margin-top: 10px;
  border-top: 1px solid #21262d;
  padding-top: 10px;
  display: none;
}
.commit-detail.open { display: block; }
.detail-grid {
  display: grid;
  grid-template-columns: 110px 1fr;
  gap: 1px;
  background: #21262d;
  border: 1px solid #21262d;
  border-radius: 6px;
  overflow: hidden;
  font-size: 12px;
}
.dk, .dv {
  padding: 6px 10px;
  background: #0d1117;
}
.dk {
  color: #6e7681;
  font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
  font-size: 11px;
  word-break: break-all;
}
.dv { color: #c9d1d9; word-break: break-word; }
.dv.prompt { color: #d2a8ff; font-style: italic; }
.dv.mono {
  font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
  color: #bc8cff;
  font-size: 11px;
}

/* === Integrity Panel === */
.integrity-panel { display: none; }
.integrity-panel.on { display: block; }
.timeline-panel.off { display: none; }

.int-form {
  background: #161b22;
  border: 1px solid #30363d;
  border-radius: 8px;
  padding: 18px;
  margin-bottom: 14px;
}
.form-label {
  display: block;
  font-size: 11px;
  font-weight: 600;
  color: #8b949e;
  text-transform: uppercase;
  letter-spacing: 0.05em;
  margin-bottom: 5px;
}
.form-row { margin-bottom: 12px; }
.form-row:last-child { margin-bottom: 0; }
.form-input, .form-textarea {
  width: 100%;
  background: #0d1117;
  border: 1px solid #30363d;
  color: #e6edf3;
  padding: 8px 12px;
  border-radius: 6px;
  font-size: 13px;
  outline: none;
  transition: border-color 0.15s;
  font-family: inherit;
}
.form-input:focus, .form-textarea:focus { border-color: #58a6ff; box-shadow: 0 0 0 3px #58a6ff15; }
.form-textarea { resize: vertical; min-height: 72px; }
.btn-run {
  background: #1f6feb;
  color: #fff;
  border: none;
  padding: 8px 18px;
  border-radius: 6px;
  cursor: pointer;
  font-size: 13px;
  font-weight: 600;
  transition: background 0.15s;
  margin-top: 12px;
}
.btn-run:hover { background: #388bfd; }
.btn-run:disabled { opacity: 0.5; cursor: not-allowed; }

.int-result {
  background: #161b22;
  border: 1px solid #30363d;
  border-radius: 8px;
  padding: 18px;
}
.int-header {
  display: flex;
  align-items: center;
  gap: 14px;
  margin-bottom: 18px;
  padding-bottom: 14px;
  border-bottom: 1px solid #21262d;
}
.level-chip {
  padding: 4px 12px;
  border-radius: 20px;
  font-size: 12px;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.06em;
}
.lv-valid { background: #3fb95020; color: #3fb950; border: 1px solid #3fb95040; }
.lv-warning { background: #d2992220; color: #d29922; border: 1px solid #d2992240; }
.lv-violation { background: #f8514920; color: #f85149; border: 1px solid #f8514940; }
.score-big {
  font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
  font-size: 30px;
  font-weight: 700;
  line-height: 1;
}
.score-label { color: #6e7681; font-size: 12px; }

.finding {
  display: flex;
  gap: 10px;
  padding: 10px 0;
  border-bottom: 1px solid #21262d;
  align-items: flex-start;
}
.finding:last-child { border-bottom: none; }
.f-icon { font-size: 14px; flex-shrink: 0; margin-top: 1px; }
.f-rule {
  font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
  font-size: 11px;
  font-weight: 700;
  padding: 1px 7px;
  border-radius: 4px;
  display: inline-block;
  margin-bottom: 3px;
}
.rv { background: #f8514920; color: #f85149; }
.rw { background: #d2992220; color: #d29922; }
.ri { background: #58a6ff20; color: #58a6ff; }
.f-detail { font-size: 12px; color: #8b949e; }

/* === Empty / Loading states === */
.loading-state {
  text-align: center;
  padding: 56px 24px;
  color: #6e7681;
}
.spinner {
  display: inline-block;
  width: 26px;
  height: 26px;
  border: 2px solid #30363d;
  border-top-color: #58a6ff;
  border-radius: 50%;
  animation: spin 0.75s linear infinite;
  margin-bottom: 12px;
}
@keyframes spin { to { transform: rotate(360deg); } }
.empty-state {
  text-align: center;
  padding: 56px 24px;
  color: #6e7681;
  font-size: 14px;
}
.empty-icon { font-size: 32px; margin-bottom: 10px; }

/* === Scrollbar === */
::-webkit-scrollbar { width: 7px; height: 7px; }
::-webkit-scrollbar-track { background: transparent; }
::-webkit-scrollbar-thumb { background: #30363d; border-radius: 4px; }
::-webkit-scrollbar-thumb:hover { background: #484f58; }

/* === Animations === */
.commit-entry { animation: slideIn 0.2s ease both; }
@keyframes slideIn { from { opacity: 0; transform: translateX(-8px); } to { opacity: 1; transform: translateX(0); } }
</style>
</head>
<body>

<header class="header">
  <div class="logo">
    <div class="logo-icon">h5</div>
    h5i
  </div>
  <span class="header-sep">/</span>
  <span class="repo-name" id="repo-name">loading…</span>
  <span class="branch-badge" id="branch-badge">—</span>
  <div class="header-spacer"></div>
  <button class="btn-refresh" onclick="loadAll()">
    <svg width="13" height="13" viewBox="0 0 16 16" fill="currentColor">
      <path d="M8 3a5 5 0 1 0 4.546 2.914.5.5 0 0 1 .908-.417A6 6 0 1 1 8 2v1z"/>
      <path d="M8 4.466V.534a.25.25 0 0 1 .41-.192l2.36 1.966c.12.1.12.284 0 .384L8.41 4.658A.25.25 0 0 1 8 4.466z"/>
    </svg>
    Refresh
  </button>
</header>

<div class="stats-bar">
  <div class="stat-item">
    <div class="stat-dot dot-blue"></div>
    <span class="stat-label">Commits</span>
    <span class="stat-value" id="s-total">—</span>
  </div>
  <div class="stat-item">
    <div class="stat-dot dot-purple"></div>
    <span class="stat-label">AI-assisted</span>
    <span class="stat-value" id="s-ai">—</span>
  </div>
  <div class="stat-item">
    <div class="stat-dot dot-green"></div>
    <span class="stat-label">Loaded</span>
    <span class="stat-value" id="s-loaded">—</span>
  </div>
  <div class="stat-item">
    <div class="stat-dot dot-orange"></div>
    <span class="stat-label">AI ratio</span>
    <span class="stat-value" id="s-ratio">—</span>
  </div>
</div>

<div class="main">
  <aside class="sidebar">
    <div class="sidebar-card">
      <div class="sidebar-title">5 Dimensions</div>
      <div class="dim-row">
        <span class="dim-icon">⏱</span>
        <span class="dim-label">Temporal</span>
        <span class="dim-tag" style="background:#58a6ff15;color:#58a6ff;border:1px solid #58a6ff30">Git</span>
      </div>
      <div class="dim-row">
        <span class="dim-icon">🌳</span>
        <span class="dim-label">Structural</span>
        <span class="dim-tag" style="background:#39d35315;color:#39d353;border:1px solid #39d35330">AST</span>
      </div>
      <div class="dim-row">
        <span class="dim-icon">🧠</span>
        <span class="dim-label">Intentional</span>
        <span class="dim-tag" style="background:#bc8cff15;color:#bc8cff;border:1px solid #bc8cff30">AI</span>
      </div>
      <div class="dim-row">
        <span class="dim-icon">🧪</span>
        <span class="dim-label">Empirical</span>
        <span class="dim-tag" style="background:#f0883e15;color:#f0883e;border:1px solid #f0883e30">Tests</span>
      </div>
      <div class="dim-row">
        <span class="dim-icon">🔗</span>
        <span class="dim-label">Associative</span>
        <span class="dim-tag" style="background:#d2992215;color:#d29922;border:1px solid #d2992230">CRDT</span>
      </div>
    </div>

    <div class="sidebar-card">
      <div class="sidebar-title">Repository</div>
      <div class="sidebar-metric">
        <span class="sidebar-metric-label">Total commits</span>
        <span class="sidebar-metric-value" id="side-total">—</span>
      </div>
      <div class="sidebar-metric">
        <span class="sidebar-metric-label">AI commits</span>
        <span class="sidebar-metric-value" style="color:#bc8cff" id="side-ai">—</span>
      </div>
      <div class="sidebar-metric">
        <span class="sidebar-metric-label">Human commits</span>
        <span class="sidebar-metric-value" style="color:#58a6ff" id="side-human">—</span>
      </div>
      <div class="sidebar-metric">
        <span class="sidebar-metric-label">AI ratio</span>
        <span class="sidebar-metric-value" id="side-ratio">—</span>
      </div>
    </div>
  </aside>

  <div class="content">
    <div class="nav-tabs">
      <div class="nav-tab active" id="tab-timeline" onclick="switchTab('timeline')">
        ⎇ Timeline
        <span class="tab-count" id="tab-count">0</span>
      </div>
      <div class="nav-tab" id="tab-integrity" onclick="switchTab('integrity')">
        🛡 Integrity Check
      </div>
    </div>

    <!-- Timeline Panel -->
    <div class="timeline-panel" id="panel-timeline">
      <div class="search-row">
        <input
          class="search-input"
          type="text"
          id="search"
          placeholder="Filter by message, author, model, agent…"
          oninput="filter()"
        />
        <button class="filter-pill" id="pill-ai" onclick="toggleFilter('ai')">🤖 AI only</button>
        <button class="filter-pill" id="pill-test" onclick="toggleFilter('test')">🧪 With tests</button>
      </div>
      <div class="timeline-wrap">
        <div class="timeline-line"></div>
        <div id="timeline">
          <div class="loading-state"><div class="spinner"></div><div>Loading commits…</div></div>
        </div>
      </div>
    </div>

    <!-- Integrity Panel -->
    <div class="integrity-panel" id="panel-integrity">
      <div class="int-form">
        <div class="form-row">
          <label class="form-label">Commit message</label>
          <input class="form-input" id="int-msg" type="text" placeholder="e.g. feat: add OAuth login" />
        </div>
        <div class="form-row">
          <label class="form-label">AI prompt (optional)</label>
          <textarea class="form-textarea" id="int-prompt" placeholder="The prompt that generated these changes…"></textarea>
        </div>
        <button class="btn-run" id="btn-run" onclick="runIntegrity()">Run Integrity Check</button>
      </div>
      <div id="int-result"></div>
    </div>
  </div>
</div>

<script>
'use strict';

let allCommits = [];
let activeFilters = new Set();

async function loadAll() {
  await Promise.all([loadRepo(), loadCommits()]);
}

async function loadRepo() {
  try {
    const d = await fetch('/api/repo').then(r => r.json());
    setText('repo-name', d.name || 'unknown');
    setText('branch-badge', d.branch || 'HEAD');
    setText('s-total', d.total_commits ?? '—');
    setText('s-ai', d.ai_commits ?? '—');
    setText('side-total', d.total_commits ?? '—');
    setText('side-ai', d.ai_commits ?? '—');
    if (d.total_commits) {
      const human = d.total_commits - (d.ai_commits || 0);
      const ratio = Math.round((d.ai_commits / d.total_commits) * 100);
      setText('side-human', human);
      setText('side-ratio', ratio + '%');
      setText('s-ratio', ratio + '%');
    }
  } catch (e) { console.error(e); }
}

async function loadCommits() {
  const tl = id('timeline');
  tl.innerHTML = '<div class="loading-state"><div class="spinner"></div><div>Loading commits…</div></div>';
  try {
    allCommits = await fetch('/api/commits?limit=200').then(r => r.json());
    setText('s-loaded', allCommits.length);
    setText('tab-count', allCommits.length);
    render(allCommits);
  } catch (e) {
    tl.innerHTML = '<div class="empty-state"><div class="empty-icon">⚠️</div>Could not load commits.<br>Is this a valid h5i repository?</div>';
  }
}

function filter() {
  const q = id('search').value.toLowerCase();
  let list = allCommits;
  if (activeFilters.has('ai')) list = list.filter(c => c.ai_model);
  if (activeFilters.has('test')) list = list.filter(c => c.test_coverage != null);
  if (q) {
    list = list.filter(c =>
      c.message.toLowerCase().includes(q) ||
      c.author.toLowerCase().includes(q) ||
      c.short_oid.includes(q) ||
      (c.ai_model || '').toLowerCase().includes(q) ||
      (c.ai_agent || '').toLowerCase().includes(q)
    );
  }
  render(list);
}

function toggleFilter(key) {
  activeFilters.has(key) ? activeFilters.delete(key) : activeFilters.add(key);
  id('pill-' + key).classList.toggle('active', activeFilters.has(key));
  filter();
}

function render(commits) {
  const tl = id('timeline');
  if (!commits.length) {
    tl.innerHTML = '<div class="empty-state"><div class="empty-icon">🔍</div>No commits match the current filter.</div>';
    return;
  }
  tl.innerHTML = commits.map((c, i) => commitHTML(c, i)).join('');
}

function commitHTML(c, i) {
  const isAi = !!c.ai_model;
  const dotCls = isAi ? 'ai-dot' : 'human-dot';
  const oidCls = isAi ? 'oid-ai' : 'oid-human';

  // Badges
  const badges = [];
  if (c.ai_model) badges.push(b('b-model', '🤖', c.ai_model));
  if (c.ai_agent && c.ai_agent !== 'unknown') badges.push(b('b-agent', '⚡', c.ai_agent));
  if (c.test_coverage != null) {
    const pct = Math.round(c.test_coverage * 100);
    badges.push(b(pct >= 70 ? 'b-test-ok' : 'b-test-no', '🧪', pct + '%'));
  }
  if (c.ast_file_count > 0) badges.push(b('b-ast', '🌳', c.ast_file_count + ' AST'));
  if (c.has_crdt) badges.push(b('b-crdt', '🔗', 'CRDT'));
  if (c.ai_tokens != null) badges.push(b('b-tok', '◦', c.ai_tokens.toLocaleString() + ' tok'));

  // Detail rows
  const rows = [];
  if (c.ai_prompt) rows.push(dr('prompt', `<span class="dv prompt">&ldquo;${esc(c.ai_prompt.slice(0, 300))}${c.ai_prompt.length > 300 ? '…' : ''}&rdquo;</span>`, true));
  if (c.ai_model) rows.push(dr('model', esc(c.ai_model)));
  if (c.ai_agent) rows.push(dr('agent_id', esc(c.ai_agent)));
  if (c.ai_tokens != null) rows.push(dr('tokens', c.ai_tokens.toLocaleString()));
  if (c.test_coverage != null) rows.push(dr('coverage', (c.test_coverage * 100).toFixed(1) + '%'));
  rows.push(dr('commit', `<span class="dv mono">${esc(c.git_oid)}</span>`, true));

  const detailId = 'det-' + i;

  return `<div class="commit-entry" style="animation-delay:${Math.min(i * 0.03, 0.5)}s">
  <div class="commit-dot ${dotCls}">${isAi ? '🤖' : ''}</div>
  <div class="commit-card" id="card-${i}" onclick="toggleDetail(${i}, '${detailId}')">
    <div class="commit-head">
      <span class="oid-chip ${oidCls}">${esc(c.short_oid)}</span>
      <span class="commit-msg">${esc(c.message || '(no message)')}</span>
    </div>
    <div class="commit-byline">
      <span class="commit-author">${esc(c.author)}</span>
      <span class="commit-time">${timeAgo(c.timestamp)}</span>
      <span>${new Date(c.timestamp).toLocaleDateString(undefined, {year:'numeric',month:'short',day:'numeric'})}</span>
    </div>
    ${badges.length ? `<div class="badges">${badges.join('')}</div>` : ''}
    ${rows.length ? `<div class="commit-detail" id="${detailId}">
      <div class="detail-grid">${rows.join('')}</div>
    </div>` : ''}
  </div>
</div>`;
}

function b(cls, icon, text) {
  return `<span class="badge ${cls}">${icon} ${esc(text)}</span>`;
}

function dr(key, valHTML, raw = false) {
  return `<div class="dk">${esc(key)}</div><div class="dv">${raw ? valHTML : esc(valHTML)}</div>`;
}

function toggleDetail(i, detailId) {
  const det = id(detailId);
  const card = id('card-' + i);
  if (!det) return;
  const open = det.classList.toggle('open');
  card.classList.toggle('expanded', open);
}

function timeAgo(iso) {
  const s = Math.floor((Date.now() - new Date(iso).getTime()) / 1000);
  if (s < 60) return s + 's ago';
  if (s < 3600) return Math.floor(s / 60) + 'm ago';
  if (s < 86400) return Math.floor(s / 3600) + 'h ago';
  if (s < 86400 * 30) return Math.floor(s / 86400) + 'd ago';
  if (s < 86400 * 365) return Math.floor(s / 2592000) + 'mo ago';
  return Math.floor(s / 31536000) + 'y ago';
}

function scoreColor(s) {
  if (s >= 0.8) return '#3fb950';
  if (s >= 0.5) return '#d29922';
  return '#f85149';
}

function switchTab(tab) {
  id('tab-timeline').classList.toggle('active', tab === 'timeline');
  id('tab-integrity').classList.toggle('active', tab === 'integrity');
  id('panel-timeline').classList.toggle('off', tab !== 'timeline');
  id('panel-integrity').classList.toggle('on', tab === 'integrity');
}

async function runIntegrity() {
  const msg = id('int-msg').value;
  const prompt = id('int-prompt').value;
  const btn = id('btn-run');
  const out = id('int-result');

  btn.disabled = true;
  btn.textContent = 'Checking…';
  out.innerHTML = '<div class="loading-state"><div class="spinner"></div><div>Running integrity rules…</div></div>';

  try {
    const p = new URLSearchParams();
    if (msg) p.set('message', msg);
    if (prompt) p.set('prompt', prompt);
    const data = await fetch('/api/integrity?' + p).then(r => r.json());
    renderIntegrity(data, out);
  } catch (e) {
    out.innerHTML = '<div class="empty-state">Failed to run check.</div>';
  } finally {
    btn.disabled = false;
    btn.textContent = 'Run Integrity Check';
  }
}

function renderIntegrity(data, out) {
  const lvMap = { Valid: 'lv-valid', Warning: 'lv-warning', Violation: 'lv-violation' };
  const lvCls = lvMap[data.level] || 'lv-valid';
  const color = scoreColor(data.score);
  const pct = Math.round(data.score * 100);

  const findings = (data.findings || []).map(f => {
    const icon = f.severity === 'Violation' ? '✖' : f.severity === 'Warning' ? '⚠' : 'ℹ';
    const cls  = f.severity === 'Violation' ? 'rv' : f.severity === 'Warning' ? 'rw' : 'ri';
    return `<div class="finding">
  <span class="f-icon">${icon}</span>
  <div>
    <span class="f-rule ${cls}">${esc(f.rule_id)}</span>
    <div class="f-detail">${esc(f.detail)}</div>
  </div>
</div>`;
  }).join('');

  out.innerHTML = `<div class="int-result">
  <div class="int-header">
    <span class="level-chip ${lvCls}">${esc(data.level)}</span>
    <span class="score-big" style="color:${color}">${pct}<span style="font-size:16px;opacity:.5">%</span></span>
    <span class="score-label">integrity score</span>
  </div>
  ${findings || '<div style="color:#3fb950;font-size:14px;display:flex;align-items:center;gap:8px"><span style="font-size:20px">✓</span> All rules passed — no issues detected.</div>'}
</div>`;
}

// Utilities
function id(s) { return document.getElementById(s); }
function setText(s, v) { const el = id(s); if (el) el.textContent = v; }
function esc(s) {
  return String(s ?? '')
    .replace(/&/g, '&amp;').replace(/</g, '&lt;')
    .replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

// Boot
loadAll();
</script>
</body>
</html>"#;
