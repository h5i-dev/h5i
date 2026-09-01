//! Where a box runs, as far as the lifecycle engine needs to know.

use std::path::Path;

use crate::error::H5iError;

/// The source a remote box is built from.
#[derive(Debug, Clone, Copy)]
pub struct RemoteSource<'a> {
    /// The repository to bundle. Opened read-only; building a bundle must not
    /// leave a ref or a branch behind in it.
    pub repo: &'a Path,
    /// The commit the box is pinned to.
    pub base_commit: &'a str,
}

/// What a runner needs in order to build a box.
///
/// The policy travels as its serialised value *and* its digest, together,
/// because the digest is the check that the box was built under the policy this
/// side resolved (R7). Splitting them across two calls would be an invitation
/// to compute one from something other than the other.
#[derive(Debug, Clone)]
pub struct RemoteCreateSpec<'a> {
    /// The box's name on the runner. Not the manifest id: that contains `/`,
    /// and this becomes a directory on a machine we do not own.
    pub box_id: &'a str,
    pub isolation: &'a str,
    pub image: Option<&'a str>,
    pub policy_json: serde_json::Value,
    pub policy_digest: &'a str,
    /// `None` for a box with no source.
    pub source: Option<RemoteSource<'a>>,
}

/// What the runner made.
#[derive(Debug, Clone)]
pub struct RemoteCreated {
    /// The runner's cryptographic identity: the SHA-256 of its pinned SSH host
    /// key. This is what the manifest records, because a name can be re-pointed
    /// at other hardware and a host key cannot (R6).
    pub runner_id: String,
    /// The runner's display name, for humans and command lines.
    pub runner: String,
    /// Where the box's workspace is on the runner, for diagnosis.
    pub workspace: String,
    /// The digest of the policy the runner actually enforced. The caller
    /// refuses the box unless this matches what it sent.
    pub policy_digest: String,
    /// True when the runner already had this box and returned it rather than
    /// building a second one.
    pub existing: bool,
}

/// One command to run in a remote box.
#[derive(Debug, Clone, Copy)]
pub struct RemoteExec<'a> {
    pub box_id: &'a str,
    pub argv: &'a [String],
    /// Relative to the box's workspace on the runner.
    pub cwd: Option<&'a str>,
    pub env: &'a [(String, String)],
    pub timeout_secs: Option<u64>,
}

/// What a remote command did.
///
/// The same facts a local run produces, so the receipt written from it is the
/// same shape. The difference is the lane it is filed under, not the evidence
/// itself (design-runner.md R10).
#[derive(Debug, Clone)]
pub struct RemoteExecResult {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub wall_ms: u64,
    pub cpu_ms: u64,
    pub max_rss_kb: Option<i64>,
    /// What the runner's own egress proxy saw. Observed outside the box, by an
    /// h5i we authenticated, on hardware we do not control.
    pub egress: Option<h5i_sandbox::sandbox_policy::EgressSummary>,
    /// Where it ran, on the runner.
    pub cwd: String,
    /// True when the runner cut the captured output.
    pub output_truncated: bool,
}

/// What a box has become, as the runner described it.
#[derive(Debug, Clone)]
pub struct RemoteExported {
    pub tip_commit: String,
    /// The tree the bundle claims to carry. Checked against what was actually
    /// fetched before anything is trusted.
    pub tip_tree: String,
    /// False when nothing was done in the box. Distinguished from a failure,
    /// because from a distance an empty patch and a broken export look alike.
    pub has_changes: bool,
}

/// A machine that can hold boxes for this one.
pub trait RemoteRunner {
    /// The runner's display name, for messages.
    fn name(&self) -> &str;

    /// Build a box there.
    fn create(&self, spec: &RemoteCreateSpec<'_>) -> Result<RemoteCreated, H5iError>;

    /// Run something in one.
    fn exec(&self, exec: &RemoteExec<'_>) -> Result<RemoteExecResult, H5iError>;

    /// Fetch what a box has become: a thin bundle of `base..tip`, written into
    /// `into`, verified against the digest the runner described it with.
    fn export(&self, box_id: &str, into: &Path) -> Result<RemoteExported, H5iError>;

    /// Remove one. Removing something already gone is success, not an error:
    /// gone is the state the caller asked for, and a retry after a lost answer
    /// must not fail.
    fn destroy(&self, box_id: &str) -> Result<(), H5iError>;
}

/// The receipt lane a remote execution is filed under.
pub const RUNNER_OBSERVED_LANE: &str = "runner-observed";

/// The name a box goes by on the runner.
/// Derived rather than chosen, so the same box always lands on the same name
/// and a retry is idempotent. The short digest suffix is doing real work: agent
/// and slug both admit `-`, so `("a-b", "c")` and `("a", "b-c")` would
/// otherwise produce one name for two different boxes, and the runner would
/// refuse the second as a conflict.
pub fn remote_box_id(manifest_id: &str) -> String {
    let digest = crate::refstore::sha256_hex(manifest_id.as_bytes());
    let readable: String = manifest_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    // Bounded well under the protocol's limit, leaving room for the suffix.
    let readable: String = readable.chars().take(80).collect();
    // Sixteen hex, not eight. A collision cannot corrupt anything, the runner
    // refuses a second box under a taken name whose request digest differs,
    // but it *can* deny service to a box whose slug someone else chose, and a
    // 32-bit suffix is within reach of someone who can pick slugs. The extra
    // eight characters cost nothing anyone reads.
    format!("{readable}-{}", &digest[..16])
}

/// A runner that is not a machine, for tests.
///
/// The whole point of the trait is that the lifecycle engine can be exercised
/// without a transport: this records what it was asked for, answers with the
/// digest it was given, and opens nothing.
#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::sync::Mutex;

    pub struct FakeRunner {
        pub name: String,
        pub runner_id: String,
        /// Every spec this was asked to build, in order.
        pub created: Mutex<Vec<(String, String)>>,
        /// Every command it was asked to run.
        pub execed: Mutex<Vec<Vec<String>>>,
        /// When set, the digest to answer with instead of the one sent. For
        /// exercising the check that a runner enforced the policy it was given.
        pub lie_with_digest: Option<String>,
        pub fail_with: Option<String>,
    }

    impl FakeRunner {
        pub fn new(name: &str) -> Self {
            Self {
                name: name.into(),
                runner_id: "d".repeat(64),
                created: Mutex::new(Vec::new()),
                execed: Mutex::new(Vec::new()),
                lie_with_digest: None,
                fail_with: None,
            }
        }
    }

    impl RemoteRunner for FakeRunner {
        fn name(&self) -> &str {
            &self.name
        }

        fn create(&self, spec: &RemoteCreateSpec<'_>) -> Result<RemoteCreated, H5iError> {
            if let Some(msg) = &self.fail_with {
                return Err(H5iError::Metadata(msg.clone()));
            }
            self.created
                .lock()
                .unwrap()
                .push((spec.box_id.to_string(), spec.isolation.to_string()));
            Ok(RemoteCreated {
                runner_id: self.runner_id.clone(),
                runner: self.name.clone(),
                workspace: format!("/var/lib/h5i/runner/boxes/live/{}/work", spec.box_id),
                policy_digest: self
                    .lie_with_digest
                    .clone()
                    .unwrap_or_else(|| spec.policy_digest.to_string()),
                existing: false,
            })
        }

        fn exec(&self, exec: &RemoteExec<'_>) -> Result<RemoteExecResult, H5iError> {
            self.execed
                .lock()
                .unwrap()
                .push(exec.argv.to_vec());
            Ok(RemoteExecResult {
                stdout: b"from the runner\n".to_vec(),
                stderr: Vec::new(),
                exit_code: Some(0),
                timed_out: false,
                wall_ms: 12,
                cpu_ms: 7,
                max_rss_kb: Some(2048),
                egress: None,
                cwd: "/var/lib/h5i/runner/work".into(),
                output_truncated: false,
            })
        }

        fn export(&self, _box_id: &str, _into: &Path) -> Result<RemoteExported, H5iError> {
            Err(H5iError::Metadata(
                "this fake runner exports nothing; the export path is tested against a real \
                 worker"
                    .into(),
            ))
        }

        fn destroy(&self, _box_id: &str) -> Result<(), H5iError> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_manifest_id_becomes_a_name_a_runner_can_hold() {
        // The manifest id has slashes in it, and this becomes a directory on
        // another machine.
        let id = remote_box_id("env/claude/demo");
        assert!(!id.contains('/'));
        assert!(id.starts_with("env-claude-demo-"));
        assert!(
            id.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
    }

    #[test]
    fn the_same_box_always_gets_the_same_name() {
        // Idempotency depends on it: a retry that picked a new name would build
        // a second box rather than find the first.
        assert_eq!(remote_box_id("env/claude/demo"), remote_box_id("env/claude/demo"));
    }

    #[test]
    fn two_boxes_that_flatten_to_one_string_still_get_two_names() {
        // Agent and slug both admit `-`, so this pair collides on the readable
        // half and must not collide overall.
        let a = remote_box_id("env/claude-demo/x");
        let b = remote_box_id("env/claude/demo-x");
        assert_ne!(a, b, "a collision here would refuse a legitimate box");
    }

    #[test]
    fn the_suffix_is_wide_enough_that_a_collision_is_not_arranged() {
        // A collision denies service to whoever asks second; it cannot corrupt
        // anything, because the runner refuses a taken name whose request
        // digest differs. Still worth being out of reach.
        let id = remote_box_id("env/claude/demo");
        let suffix = id.rsplit('-').next().unwrap();
        assert_eq!(suffix.len(), 16, "64 bits, not 32");
        assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn a_long_id_stays_inside_the_protocols_limit() {
        let id = remote_box_id(&format!("env/agent/{}", "s".repeat(500)));
        assert!(id.len() <= 128, "got {} chars", id.len());
    }
}
