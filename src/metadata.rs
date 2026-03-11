use git2::Oid;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct H5iCommitRecord {
    pub git_oid: String,
    pub parent_oid: Option<String>,
    pub ai_metadata: Option<AiMetadata>,
    pub test_metrics: Option<TestMetrics>,
    /// ファイルパス -> 外部から提供された AST (S式) のハッシュ
    pub ast_hashes: Option<HashMap<String, String>>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AiMetadata {
    pub model_name: String,
    pub prompt_hash: String,
    pub agent_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TestMetrics {
    pub test_suite_hash: String,
    pub coverage: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CommitProvenance {
    pub commit_oid: String,
    pub ai_metadata: Option<AiMetadata>,
    pub test_metrics: Option<TestMetrics>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}
