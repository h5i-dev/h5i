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

    /// Asks Claude to produce a concise (≤12 word) intent sentence for a commit.
    ///
    /// Used by `intent-graph --mode analyze` to enrich commits that have no
    /// stored AI prompt.
    pub fn generate_intent(
        &self,
        short_oid: &str,
        message: &str,
        prompt: Option<&str>,
    ) -> Result<String, H5iError> {
        let system = "You are a git assistant summarising developer intent. \
            Given a commit message and an optional AI prompt, respond with a \
            single concise sentence (maximum 12 words) describing the intent \
            of the change. Output ONLY the sentence, nothing else.";

        let user_content = match prompt {
            Some(p) if !p.is_empty() => {
                format!("Commit: {short_oid}\nMessage: {message}\nPrompt: {p}")
            }
            _ => format!("Commit: {short_oid}\nMessage: {message}"),
        };

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

        let text = api_resp
            .content
            .into_iter()
            .find(|b| b.kind == "text")
            .and_then(|b| b.text)
            .unwrap_or_default();

        Ok(text.trim().to_string())
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_commit(oid: &str, message: &str, prompt: Option<&str>) -> CommitSummary {
        CommitSummary {
            oid: oid.to_string(),
            message: message.to_string(),
            prompt: prompt.map(|s| s.to_string()),
            model: None,
            agent_id: None,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn keyword_search_returns_best_match() {
        let commits = vec![
            make_commit("aaa", "add logging module", None),
            make_commit("bbb", "implement oauth login", Some("add GitHub login")),
            make_commit("ccc", "fix typo in README", None),
        ];
        let result = keyword_search(&commits, "oauth login");
        assert_eq!(result.map(|c| c.oid.as_str()), Some("bbb"));
    }

    #[test]
    fn keyword_search_returns_none_when_no_terms_match() {
        let commits = vec![
            make_commit("aaa", "add cache layer", None),
            make_commit("bbb", "refactor auth", None),
        ];
        assert!(keyword_search(&commits, "websocket streaming").is_none());
    }

    #[test]
    fn keyword_search_empty_commit_list() {
        assert!(keyword_search(&[], "anything").is_none());
    }

    #[test]
    fn keyword_search_is_case_insensitive() {
        let commits = vec![make_commit("aaa", "Add Rate Limiting", None)];
        let result = keyword_search(&commits, "rate limiting");
        assert!(result.is_some());
    }

    #[test]
    fn keyword_search_searches_prompt_field() {
        let commits = vec![
            make_commit("aaa", "refactor session module", Some("implement Redis session store")),
            make_commit("bbb", "update tests", None),
        ];
        let result = keyword_search(&commits, "redis");
        assert_eq!(result.map(|c| c.oid.as_str()), Some("aaa"));
    }

    #[test]
    fn keyword_search_higher_score_wins() {
        let commits = vec![
            make_commit("aaa", "fix auth token", None),           // 2 terms match
            make_commit("bbb", "fix auth token validation bug", None), // 3 terms match
        ];
        let result = keyword_search(&commits, "fix auth token");
        assert_eq!(result.map(|c| c.oid.as_str()), Some("bbb"));
    }
}
