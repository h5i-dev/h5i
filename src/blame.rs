use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct BlameResult {
    pub line_number: usize,
    pub line_content: String,
    pub commit_id: String,
    /// Display name: "Human" or "AI:ModelName"
    pub agent_info: String,
    pub test_passed: Option<bool>,
    /// The human prompt that triggered this commit (from h5i AI metadata).
    /// `None` for human commits or commits without recorded provenance.
    pub prompt: Option<String>,
}

/// One entry in the prompt ancestry chain for a specific file line.
#[derive(Debug, Serialize)]
pub struct AncestryEntry {
    pub commit_id: String,
    pub author: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Human prompt recorded for this commit, if any.
    pub prompt: Option<String>,
    /// AI agent identifier, if this was an AI commit.
    pub agent: Option<String>,
    /// The line content as it existed in this commit.
    pub line_content: String,
}
