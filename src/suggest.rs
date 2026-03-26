// suggest.rs

use h5i_core::repository::H5iRepository;
use h5i_core::metadata::CommitSummary;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::env;
use std::error::Error;

#[derive(Debug, Deserialize)]
struct ClaudeResponse {
    content: Vec<ContentBlock>,
}

#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

pub async fn suggest_next_step_claude(
    repo: &H5iRepository,
) -> Result<String, Box<dyn Error>> {
    // Collect all commits and build a simple synopsis
    let commits: Vec<CommitSummary> = repo
        .all_h5i_commits()?
        .into_iter()
        .map(|r| CommitSummary {
            oid: r.git_oid.clone(),
            message: r
                .ai_metadata
                .as_ref()
                .map(|m| m.prompt.clone())
                .flatten()
                .unwrap_or_else(|| r.git_oid.clone()),
            prompt: None,
            model: None,
            agent_id: None,
            timestamp: r.timestamp,
        })
        .collect();

    let list = commits
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{}. {}: {}", i + 1, c.oid, c.message))
        .collect::<Vec<_>>()
        .join("\n");

    // Construct Claude prompt
    let prompt = format!(
        "You are an expert Git/AI assistant. Given this commit history:\n{}\n\n\
Return a concise recommendation for the *next logical step* the developer should take \
(e.g., add tests, refactor, write docs, fix bug), and ONLY the suggestion text.",
        list
    );

    let api_key = env::var("CLAUDE_API_KEY")?;
    let client = reqwest::Client::new();

    let req_body = json!({
        "model": "claude-haiku-4-5-20251001",
        "max_tokens": 128,
        "system": "",
        "messages": [
            { "role": "user", "content": prompt }
        ]
    });

    let resp = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&req_body)
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(format!("Claude API returned {}", resp.status()).into());
    }

    let body: ClaudeResponse = resp.json().await?;
    let suggestion = body
        .content
        .into_iter()
        .find(|b| b.kind == "text")
        .and_then(|b| b.text)
        .unwrap_or_default();

    Ok(suggestion.trim().to_string())
}
