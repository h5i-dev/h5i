//! The manifest-to-pattern bridge (design review item 5): a TOML file that
//! parameterizes a *blessed Rust pattern*, so most users need not compile a
//! score binary. Held to one firm line — a manifest supplies **parameters**
//! (which agents, how many rounds, the verifier command, the gate), never
//! control flow. The moment a workflow needs a conditional it is a score, not
//! a manifest; this stays Keras-layer-1.5, not a second orchestration language.
//!
//! ```toml
//! pattern = "ensemble"          # the only pattern with a CLI driver today
//! task = "implement `h5i pull` mirroring `h5i push`"
//! rounds = 2
//! verify_cmd = "cargo test -q"
//! isolation = "container"       # optional verifier tier
//! gate = true                   # durable approval before apply
//!
//! # optional: enroll agents by runtime if the team has none yet
//! [[agents]]
//! name = "claude"
//! runtime = "claude"
//! [[agents]]
//! name = "codex"
//! runtime = "codex"
//! ```

use h5i_core::error::H5iError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamManifest {
    /// Blessed pattern name. Only `ensemble` is CLI-drivable today; an unknown
    /// value is refused (a manifest never invents a pattern).
    #[serde(default = "default_pattern")]
    pub pattern: String,
    /// The task text (or `task_file`, resolved by the caller).
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default)]
    pub task_file: Option<String>,
    #[serde(default = "default_rounds")]
    pub rounds: u32,
    #[serde(default)]
    pub verify_cmd: Option<String>,
    #[serde(default)]
    pub isolation: Option<String>,
    #[serde(default)]
    pub apply: bool,
    #[serde(default)]
    pub gate: bool,
    /// Optional roster to enroll if the team has none yet.
    #[serde(default)]
    pub agents: Vec<ManifestAgent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestAgent {
    pub name: String,
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// Enroll an existing env instead of creating one.
    #[serde(default)]
    pub env: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
}

fn default_pattern() -> String {
    "ensemble".into()
}
fn default_rounds() -> u32 {
    1
}

/// Patterns a manifest may name — the guardrail that keeps a manifest from
/// growing into a workflow language. Extend deliberately as patterns gain CLI
/// drivers.
const KNOWN_PATTERNS: &[&str] = &["ensemble"];

impl TeamManifest {
    pub fn parse(toml_src: &str) -> Result<Self, H5iError> {
        let m: TeamManifest = toml::from_str(toml_src)
            .map_err(|e| H5iError::Metadata(format!("invalid team manifest: {e}")))?;
        if !KNOWN_PATTERNS.contains(&m.pattern.as_str()) {
            return Err(H5iError::Metadata(format!(
                "team manifest: unknown pattern '{}' (known: {}). A manifest names a blessed \
                 pattern; it cannot define control flow — write a score for that.",
                m.pattern,
                KNOWN_PATTERNS.join(", ")
            )));
        }
        if m.task.is_some() && m.task_file.is_some() {
            return Err(H5iError::Metadata(
                "team manifest: set task or task_file, not both".into(),
            ));
        }
        Ok(m)
    }

    /// Resolve the task text, reading `task_file` relative to `base_dir` when
    /// set. A `flag_task` (from `--task`/`--task-file` on the CLI) overrides.
    pub fn resolve_task(
        &self,
        base_dir: &std::path::Path,
        flag_task: Option<String>,
    ) -> Result<String, H5iError> {
        if let Some(t) = flag_task {
            return Ok(t);
        }
        if let Some(t) = &self.task {
            return Ok(t.clone());
        }
        if let Some(f) = &self.task_file {
            let path = base_dir.join(f);
            return std::fs::read_to_string(&path)
                .map_err(|e| H5iError::with_path(e, &path));
        }
        Err(H5iError::Metadata(
            "team manifest: no task, task_file, or --task given".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_manifest() {
        let m = TeamManifest::parse(
            r#"
            pattern = "ensemble"
            task = "do the thing"
            rounds = 3
            verify_cmd = "cargo test -q"
            isolation = "container"
            gate = true

            [[agents]]
            name = "claude"
            runtime = "claude"

            [[agents]]
            name = "codex"
            runtime = "codex"
            "#,
        )
        .unwrap();
        assert_eq!(m.pattern, "ensemble");
        assert_eq!(m.rounds, 3);
        assert_eq!(m.verify_cmd.as_deref(), Some("cargo test -q"));
        assert!(m.gate);
        assert_eq!(m.agents.len(), 2);
        assert_eq!(m.agents[1].runtime.as_deref(), Some("codex"));
    }

    #[test]
    fn defaults_are_sane() {
        let m = TeamManifest::parse("task = \"x\"").unwrap();
        assert_eq!(m.pattern, "ensemble");
        assert_eq!(m.rounds, 1);
        assert!(!m.gate);
        assert!(m.agents.is_empty());
    }

    #[test]
    fn rejects_unknown_pattern() {
        let err = TeamManifest::parse("pattern = \"magentic\"\ntask = \"x\"")
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown pattern 'magentic'"), "{err}");
        assert!(err.contains("write a score"), "{err}");
    }

    #[test]
    fn rejects_task_and_task_file() {
        assert!(TeamManifest::parse("task = \"a\"\ntask_file = \"b.md\"").is_err());
    }
}
