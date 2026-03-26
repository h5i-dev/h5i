use serde::{Deserialize, Serialize};

use crate::claude::AnthropicClient;
use crate::error::H5iError;
use crate::metadata::CommitSummary;

// ── Output structure ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct CommitCluster {
    pub label: String,          // e.g. "authentication", "testing", etc.
    pub commits: Vec<String>,   // commit OIDs
}

// ── Step 1: generate intents for all commits ──────────────────────────────────

pub fn enrich_with_intents(
    client: &AnthropicClient,
    commits: &[CommitSummary],
) -> Result<Vec<(String, String)>, H5iError> {
    // (oid, intent)
    let mut result = Vec::new();

    for c in commits {
        let short_oid = &c.oid[..7];
        let intent = client.generate_intent(
            short_oid,
            &c.message,
            c.prompt.as_deref(),
        )?;
        result.push((c.oid.clone(), intent));
    }

    Ok(result)
}

// ── Step 2: cluster commits by purpose using AI ───────────────────────────────

pub fn cluster_commits(
    client: &AnthropicClient,
    commits_with_intent: &[(String, String)],
) -> Result<Vec<CommitCluster>, H5iError> {
    let system = "You group git commits by their functional purpose in the codebase. \
Return JSON as an array of objects: {\"label\": string, \"commits\": [oid,...]}. \
Group commits into meaningful sections like 'auth', 'tests', 'refactor', etc. \
Use each commit exactly once.";

    let list = commits_with_intent
        .iter()
        .enumerate()
        .map(|(i, (oid, intent))| {
            format!("{}. OID: {}\n   Intent: {}", i + 1, oid, intent)
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let user = format!("Group these commits:\n\n{list}");

    let request = serde_json::json!({
        "model": client.model(),
        "max_tokens": 512,
        "system": system,
        "messages": [
            { "role": "user", "content": user }
        ]
    });

    let response = reqwest::blocking::Client::new()
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", &client.api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&request)
        .send()
        .map_err(|e| H5iError::Metadata(format!("Claude API request failed: {e}")))?;

    let value: serde_json::Value = response
        .json()
        .map_err(|e| H5iError::Metadata(format!("Parse error: {e}")))?;

    let text = value["content"][0]["text"]
        .as_str()
        .unwrap_or("[]");

    let clusters: Vec<CommitCluster> =
        serde_json::from_str(text)
            .map_err(|e| H5iError::Metadata(format!("Invalid JSON from Claude: {e}")))?;

    Ok(clusters)
}
