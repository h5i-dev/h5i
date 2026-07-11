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

// ── Human-prompt sanitisation ───────────────────────────────────────────────

/// Decide whether a `UserPromptSubmit` payload is genuine human input and, if
/// so, return the cleaned human text.
///
/// Claude Code delivers more than typed prompts through this channel: a
/// completed background task (the `Agent`/Task tool) and other automated events
/// arrive as their **own synthetic "user" turns**, and harness context can be
/// wrapped in `<system-reminder>` blocks. None of that is human-authored, yet
/// the capture hook would otherwise record it as the prompt behind a commit
/// (this is exactly how a `<task-notification>` block leaked into a commit's
/// provenance).
///
/// Policy:
/// * A turn carrying the background-event banner or a `<task-notification>`
///   block is *entirely* machine-generated → `None` (skip the whole turn).
/// * A real human turn may still carry an injected `<system-reminder>` block →
///   strip those, keep the human text. Empty after stripping → `None`.
pub fn sanitize_human_prompt(raw: &str) -> Option<String> {
    // Standalone automated turns — never human input.
    if raw.contains("[SYSTEM NOTIFICATION - NOT USER INPUT]")
        || raw.contains("<task-notification")
    {
        return None;
    }
    // Defensive: drop any harness `<system-reminder>` context wrapped into the
    // turn, keeping only what the human actually wrote.
    let cleaned = strip_tag_block(raw, "system-reminder");
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.to_string())
    }
}

/// Remove every non-nested `<tag …>…</tag>` span (attributes allowed) from
/// `text`. An opening tag with no matching close drops the remainder, so a
/// truncated injected block can't leak through.
fn strip_tag_block(text: &str, tag: &str) -> String {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(&open) {
        // Confirm this is `<tag>` or `<tag …`, not a longer name like `<tagfoo`.
        let after = &rest[start + open.len()..];
        let is_tag = after.starts_with('>') || after.starts_with(char::is_whitespace);
        if !is_tag {
            let upto = start + open.len();
            out.push_str(&rest[..upto]);
            rest = &rest[upto..];
            continue;
        }
        out.push_str(&rest[..start]);
        match rest[start..].find(&close) {
            Some(rel) => rest = &rest[start + rel + close.len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
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
    fn sanitize_keeps_plain_human_prompt() {
        let p = "Can we make the prompt scoring mechanism faster?";
        assert_eq!(sanitize_human_prompt(p).as_deref(), Some(p));
    }

    #[test]
    fn sanitize_trims_whitespace() {
        assert_eq!(sanitize_human_prompt("  fix the retry loop \n").as_deref(), Some("fix the retry loop"));
        assert_eq!(sanitize_human_prompt("   ").as_deref(), None);
        assert_eq!(sanitize_human_prompt("").as_deref(), None);
    }

    #[test]
    fn sanitize_drops_background_task_notification_turn() {
        // The shape that actually leaked into commit provenance.
        let p = "<task-notification>\n\
                 <task-id>a2517b77a93d6c69f</task-id>\n\
                 <status>completed</status>\n\
                 <result>Found the scoring code…</result>\n\
                 </task-notification>";
        assert_eq!(sanitize_human_prompt(p), None);
    }

    #[test]
    fn sanitize_drops_system_notification_banner_turn() {
        let p = "[SYSTEM NOTIFICATION - NOT USER INPUT]\n\
                 This is an automated background-task event, NOT a message from the user.\n\
                 \n\
                 <task-notification>\n<status>completed</status>\n</task-notification>";
        assert_eq!(sanitize_human_prompt(p), None);
    }

    #[test]
    fn sanitize_strips_system_reminder_but_keeps_human_text() {
        let p = "Please fix the bug.\n\
                 <system-reminder>You are Claude. Follow CLAUDE.md exactly.</system-reminder>";
        assert_eq!(sanitize_human_prompt(p).as_deref(), Some("Please fix the bug."));
    }

    #[test]
    fn sanitize_system_reminder_only_is_dropped() {
        let p = "<system-reminder>injected context</system-reminder>";
        assert_eq!(sanitize_human_prompt(p), None);
    }

    #[test]
    fn sanitize_handles_multiple_and_unclosed_reminders() {
        // Two blocks plus human text between them.
        let p = "<system-reminder>a</system-reminder>real ask<system-reminder>b</system-reminder>";
        assert_eq!(sanitize_human_prompt(p).as_deref(), Some("real ask"));
        // An unclosed block drops the remainder (can't leak a partial injection).
        let p2 = "keep this <system-reminder>truncated injection with no close";
        assert_eq!(sanitize_human_prompt(p2).as_deref(), Some("keep this"));
    }

    #[test]
    fn sanitize_does_not_match_longer_tag_name() {
        // `<system-reminders>` (plural) is not the wrapper we strip.
        let p = "talk about <system-reminders> as a concept";
        assert_eq!(sanitize_human_prompt(p).as_deref(), Some("talk about <system-reminders> as a concept"));
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
