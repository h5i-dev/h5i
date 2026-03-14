use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};

use crate::error::H5iError;
use crate::metadata::CommitSummary;

// ── Anthropic API types ───────────────────────────────────────────────────────

#[derive(Serialize)]
struct ApiRequest {
    model: String,
    max_tokens: u32,
    system: String,
    messages: Vec<ApiMessage>,
}

#[derive(Serialize)]
struct ApiMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ApiResponse {
    content: Vec<ContentBlock>,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

// ── Client ────────────────────────────────────────────────────────────────────

pub struct AnthropicClient {
    api_key: String,
    model: String,
    client: Client,
}

impl AnthropicClient {
    /// Constructs a client from environment variables.
    /// Returns `None` when `ANTHROPIC_API_KEY` is not set.
    pub fn from_env() -> Option<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY").ok()?;
        let model = std::env::var("H5I_SEARCH_MODEL")
            .unwrap_or_else(|_| "claude-haiku-4-5-20251001".to_string());
        Some(Self {
            api_key,
            model,
            client: Client::new(),
        })
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// Asks Claude which commit in `commits` best matches `intent`.
    ///
    /// Returns the full OID string of the matched commit, or `None` when
    /// Claude cannot find a clear match.
    pub fn find_matching_commit(
        &self,
        commits: &[CommitSummary],
        intent: &str,
    ) -> Result<Option<String>, H5iError> {
        let commit_list = commits
            .iter()
            .enumerate()
            .map(|(i, c)| {
                format!(
                    "{}. OID: {}\n   Message: {}\n   Prompt: {}\n   Agent: {}\n   Date: {}",
                    i + 1,
                    c.oid,
                    c.message.trim(),
                    c.prompt.as_deref().unwrap_or("(none)"),
                    c.agent_id.as_deref().unwrap_or("(human)"),
                    c.timestamp.format("%Y-%m-%d %H:%M UTC"),
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        let system = "You are a git assistant helping a developer find which commit to revert. \
            Given an intent description and a list of commits (with their OIDs, messages, \
            AI prompts, and dates), return ONLY the full 40-character commit OID of the \
            single best-matching commit. If no commit clearly matches the intent, return \
            exactly the word NONE. Output nothing else.";

        let user_content = format!(
            "Find the commit that best matches this intent: \"{intent}\"\n\nCommits:\n\n{commit_list}"
        );

        let request = ApiRequest {
            model: self.model.clone(),
            max_tokens: 64,
            system: system.to_string(),
            messages: vec![ApiMessage {
                role: "user".to_string(),
                content: user_content,
            }],
        };

        let response = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&request)
            .send()
            .map_err(|e| H5iError::Metadata(format!("Claude API request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            return Err(H5iError::Metadata(format!(
                "Claude API error {status}: {body}"
            )));
        }

        let api_resp: ApiResponse = response
            .json()
            .map_err(|e| H5iError::Metadata(format!("Failed to parse Claude API response: {e}")))?;

        let raw = api_resp
            .content
            .into_iter()
            .find(|b| b.kind == "text")
            .and_then(|b| b.text)
            .unwrap_or_default();

        let text = raw.trim();
        if text.eq_ignore_ascii_case("none") || text.is_empty() {
            return Ok(None);
        }

        // Validate the returned OID is actually in our list (full or prefix match).
        let matched = commits
            .iter()
            .find(|c| c.oid == text || c.oid.starts_with(text));
        Ok(matched.map(|c| c.oid.clone()))
    }
}

// ── Keyword fallback ──────────────────────────────────────────────────────────

/// Simple keyword search used when `ANTHROPIC_API_KEY` is not available.
/// Scores each commit by how many whitespace-separated terms from `intent`
/// appear in its message + prompt, and returns the highest-scoring one.
pub fn keyword_search<'a>(commits: &'a [CommitSummary], intent: &str) -> Option<&'a CommitSummary> {
    let terms: Vec<String> = intent
        .split_whitespace()
        .map(|t| t.to_lowercase())
        .collect();

    let score = |c: &CommitSummary| {
        let haystack = format!(
            "{} {}",
            c.message.to_lowercase(),
            c.prompt.as_deref().unwrap_or("").to_lowercase()
        );
        terms.iter().filter(|t| haystack.contains(t.as_str())).count()
    };

    commits.iter().filter(|c| score(c) > 0).max_by_key(|c| score(c))
}
