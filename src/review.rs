use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Score threshold above which a commit is flagged as a suggested review point.
pub const REVIEW_THRESHOLD: f32 = 0.25;

/// A single deterministic rule that fired for a commit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewTrigger {
    /// Short machine-readable rule identifier, e.g. `"LARGE_DIFF"`.
    pub rule_id: String,
    /// Weight this trigger adds to the overall score (0.0–1.0 range).
    pub weight: f32,
    /// Human-readable explanation of why this rule fired.
    pub detail: String,
}

/// A commit identified as a suggested review point by one or more deterministic rules.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewPoint {
    pub commit_oid: String,
    pub short_oid: String,
    pub message: String,
    pub author: String,
    pub timestamp: DateTime<Utc>,
    /// Aggregate priority score in [0.0, 1.0]. Higher means more review-worthy.
    pub score: f32,
    /// Individual rules that fired and contributed to the score.
    pub triggers: Vec<ReviewTrigger>,
}
