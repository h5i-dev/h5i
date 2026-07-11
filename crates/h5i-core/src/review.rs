use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Default threshold for the legacy aggregate `score`.
/// We keep this at 0.5 (was 0.25) so the long tail of shape-only commits no
/// longer pollutes `h5i audit review` and the PR comment.
pub const REVIEW_THRESHOLD: f32 = 0.5;

/// Threshold used to flag a commit in the PR comment 🚩. Compared against
/// `ReviewPoint::quality_score` (Quality-tier rules only) — shape rules
/// don't contribute, so the flag only fires on genuine quality signals.
pub const PR_QUALITY_THRESHOLD: f32 = 0.25;

/// Whether a rule measures **real risk** (Quality) or just the **shape** of
/// the diff (informational).
///
/// - **Quality** triggers are the ones that should drive review attention:
///   credential leaks, code-execution sinks, sensitive-file edits, blind
///   edits, test regressions, duplicated code, missing prompt provenance, …
/// - **Shape** triggers are correlated with "this looks like an AI-session
///   commit" rather than risk: large diffs, wide impact, polyglot changes,
///   bursts after a quiet period. We surface them only when paired with a
///   Quality signal — `LARGE_DIFF` alone is noise; `LARGE_DIFF + BLIND_EDIT`
///   is real.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Quality,
    Shape,
}

/// A single deterministic rule that fired for a commit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewTrigger {
    /// Short machine-readable rule identifier, e.g. `"LARGE_DIFF"`.
    pub rule_id: String,
    /// Weight this trigger adds to the overall score (0.0–1.0 range).
    pub weight: f32,
    /// Human-readable explanation of why this rule fired.
    pub detail: String,
    /// Quality (real risk) vs Shape (informational). Defaults to Shape so
    /// older serialized records without this field stay conservative.
    #[serde(default = "default_tier")]
    pub tier: Tier,
}

fn default_tier() -> Tier {
    Tier::Shape
}

/// A commit identified as a suggested review point by one or more deterministic rules.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewPoint {
    pub commit_oid: String,
    pub short_oid: String,
    pub message: String,
    pub author: String,
    pub timestamp: DateTime<Utc>,
    /// Legacy aggregate priority score in [0.0, 1.0]. Sum of all trigger
    /// weights (Quality + Shape), clamped. Higher means more review-worthy.
    pub score: f32,
    /// Score from Quality-tier triggers only. The PR comment flags 🚩
    /// against this score (default threshold [`PR_QUALITY_THRESHOLD`]).
    #[serde(default)]
    pub quality_score: f32,
    /// Score from Shape-tier triggers only. Surfaced as supplementary
    /// "shape signals" but never the sole reason for flagging a commit.
    #[serde(default)]
    pub shape_score: f32,
    /// Individual rules that fired and contributed to the score.
    pub triggers: Vec<ReviewTrigger>,
}

impl ReviewPoint {
    /// Quality-tier triggers only — what the PR 🚩 should display as
    /// "review signals".
    pub fn quality_triggers(&self) -> impl Iterator<Item = &ReviewTrigger> {
        self.triggers.iter().filter(|t| t.tier == Tier::Quality)
    }

    /// Shape-tier triggers — informational, shown only as context.
    pub fn shape_triggers(&self) -> impl Iterator<Item = &ReviewTrigger> {
        self.triggers.iter().filter(|t| t.tier == Tier::Shape)
    }

    /// True when the commit deserves a PR-comment flag: it must have at
    /// least one Quality-tier signal with aggregate weight ≥ threshold.
    pub fn should_flag_in_pr(&self) -> bool {
        self.quality_score >= PR_QUALITY_THRESHOLD
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn trigger(rule_id: &str, weight: f32, tier: Tier) -> ReviewTrigger {
        ReviewTrigger {
            rule_id: rule_id.to_string(),
            weight,
            detail: format!("{rule_id} detail"),
            tier,
        }
    }

    fn review_point(
        triggers: Vec<ReviewTrigger>,
        quality_score: f32,
        shape_score: f32,
    ) -> ReviewPoint {
        ReviewPoint {
            commit_oid: "abcdef1234567890".to_string(),
            short_oid: "abcdef12".to_string(),
            message: "test commit".to_string(),
            author: "test author".to_string(),
            timestamp: Utc.timestamp_opt(0, 0).single().unwrap(),
            score: (quality_score + shape_score).min(1.0),
            quality_score,
            shape_score,
            triggers,
        }
    }

    #[test]
    fn quality_triggers_returns_only_quality_tier_rules() {
        let point = review_point(
            vec![
                trigger("BLIND_EDIT", 0.4, Tier::Quality),
                trigger("LARGE_DIFF", 0.2, Tier::Shape),
                trigger("TEST_REGRESSION", 0.5, Tier::Quality),
            ],
            0.9,
            0.2,
        );

        let ids: Vec<&str> = point
            .quality_triggers()
            .map(|trigger| trigger.rule_id.as_str())
            .collect();

        assert_eq!(ids, vec!["BLIND_EDIT", "TEST_REGRESSION"]);
    }

    #[test]
    fn shape_triggers_returns_only_shape_tier_rules() {
        let point = review_point(
            vec![
                trigger("POLYGLOT_CHANGE", 0.15, Tier::Shape),
                trigger("SECRET_TOUCH", 0.8, Tier::Quality),
                trigger("LARGE_DIFF", 0.2, Tier::Shape),
            ],
            0.8,
            0.35,
        );

        let ids: Vec<&str> = point
            .shape_triggers()
            .map(|trigger| trigger.rule_id.as_str())
            .collect();

        assert_eq!(ids, vec!["POLYGLOT_CHANGE", "LARGE_DIFF"]);
    }

    #[test]
    fn should_flag_in_pr_uses_quality_threshold() {
        let point = review_point(
            vec![trigger("BLIND_EDIT", PR_QUALITY_THRESHOLD, Tier::Quality)],
            PR_QUALITY_THRESHOLD,
            0.0,
        );

        assert!(point.should_flag_in_pr());
    }

    #[test]
    fn should_flag_in_pr_ignores_shape_only_score() {
        let point = review_point(
            vec![trigger("LARGE_DIFF", 1.0, Tier::Shape)],
            PR_QUALITY_THRESHOLD - 0.01,
            1.0,
        );

        assert!(!point.should_flag_in_pr());
    }

    #[test]
    fn review_threshold_constants_document_current_cutoffs() {
        assert_eq!(REVIEW_THRESHOLD, 0.5);
        assert_eq!(PR_QUALITY_THRESHOLD, 0.25);
    }
}
