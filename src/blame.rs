use crate::metadata::{AiMetadata, TestMetrics};

pub enum BlameMode {
    /// Traditional line-based blame (Standard)
    Line,
    /// AST hash-based blame (Structural Dimension)
    Ast,
}

pub struct BlameEntry {
    pub commit_oid: String,
    pub author_name: String,
    /// Metadata if the change was authored by an AI agent
    pub ai_metadata: Option<AiMetadata>,
    /// Test results associated with this specific commit
    pub test_metrics: Option<TestMetrics>,
}

pub struct H5iBlameEntry {
    pub line_number: usize,
    pub commit_id: String,
    /// Metadata if AI was involved in this line's creation/modification
    pub ai_metadata: Option<AiMetadata>,
    /// The test status recorded at the time of this entry
    pub test_passed: Option<bool>,
    /// Whether this entry was identified via AST-based tracking (Semantic)
    pub is_semantic: bool,
}

#[derive(Debug)]
pub struct BlameResult {
    pub line_number: usize,
    pub line_content: String,
    pub commit_id: String,
    /// Display name: "Human" or "AI:ModelName"
    pub agent_info: String,
    /// Indicates if a logical change occurred at the AST level
    pub is_semantic_change: bool,
    pub test_passed: Option<bool>,
}
