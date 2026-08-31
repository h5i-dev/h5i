//! `h5i-core`'s placement trait, implemented over the runner protocol.
//! This is the only place the two halves meet. `h5i-core` owns the box
//! lifecycle and knows nothing about SSH or frames; `h5i-runner` owns the
//! protocol and knows nothing about manifests. The binary is above both, which
//! is where a dependency between them would otherwise have to go, and where it
//! would eventually become a cycle, since a later milestone will want the
//! worker reaching for receipts and export.

use h5i_core::error::H5iError;
use h5i_core::placement::{
    RemoteCreateSpec, RemoteCreated, RemoteExec, RemoteExecResult, RemoteExported, RemoteRunner,
};
use h5i_runner::config::RunnerRecord;
use h5i_runner::proto::{
    CreateRequest, ExecRequest, LeaseSpec, ResourceLimits, SourceKind, SourceSpec,
};
use h5i_runner::{Client, source};

/// A paired runner, as the lifecycle engine sees it.
pub struct PairedRunner {
    record: RunnerRecord,
    client: Client,
    /// The bundle is built into here and lives as long as this does, so the
    /// file is still there when `create` streams it and gone afterwards
    /// whatever happens.
    scratch: tempfile::TempDir,
}

impl PairedRunner {
    pub fn open(name: &str) -> anyhow::Result<Self> {
        let record = h5i_runner::config::load(name)?;
        let client = super::runner::client_for(&record)?;
        Ok(Self {
            record,
            client,
            scratch: tempfile::tempdir()?,
        })
    }

    /// Refuse early, with the capability named, when the runner does not offer
    /// what this box wants.
    ///
    /// The worker refuses too, and its refusal is the enforcement (ROADMAP.md
    /// R7). This one exists so the message arrives before a bundle is built and
    /// sent, and so it can name what the runner *does* offer, which the client
    /// knows from its cached probe and the worker would have to be asked for.
    pub fn check_supports(&self, isolation: &str) -> anyhow::Result<()> {
        let Some(caps) = self.record.cached_capabilities() else {
            // Never probed. Not an error: the worker still checks, and refusing
            // here would turn a stale cache into a broken command.
            return Ok(());
        };
        if caps.isolation.iter().any(|t| t == isolation) {
            return Ok(());
        }
        anyhow::bail!(
            "runner `{}` does not offer the `{isolation}` tier — it offers {}.\n\
             A capability it lacks is a refusal, not a weaker box. Run \
             `h5i runner probe {}` to see what it can do now, or pick another tier.",
            self.record.name,
            if caps.isolation.is_empty() {
                "none".to_string()
            } else {
                caps.isolation.join(", ")
            },
            self.record.name
        )
    }
}

impl PairedRunner {
    /// Refuse to talk to a machine that is not the one the box was built on.
    ///
    /// The manifest pins a host-key hash, and this compares it against the
    /// runner that answers to that *name* now. A name re-paired to different
    /// hardware would otherwise send a box's commands to a machine that has
    /// never seen it, which is exactly the failure that made identity
    /// cryptographic in the first place (design-runner.md R6).
    pub fn check_identity(&self, m: &h5i_core::env::EnvManifest) -> anyhow::Result<()> {
        let Some(recorded) = &m.runner_id else {
            return Ok(());
        };
        if &self.record.runner_id == recorded {
            return Ok(());
        }
        anyhow::bail!(
            "runner `{}` is not the machine `{}` was created on.\n\
             Its host key has changed, or the name now points at different hardware. \
             A box is bound to a machine, not to a name, so this is refused rather than \
             sent somewhere it does not belong.",
            self.record.name,
            m.id
        )
    }
}

impl RemoteRunner for PairedRunner {
    fn name(&self) -> &str {
        &self.record.name
    }

    fn create(&self, spec: &RemoteCreateSpec<'_>) -> Result<RemoteCreated, H5iError> {
        // The bundle, built from the repository this side owns. `build_bundle`
        // leaves no ref and no branch behind in it.
        let (source_spec, bundle) = match &spec.source {
            Some(src) => {
                let out = self.scratch.path().join("source.bundle");
                let bundle = source::build_bundle(src.repo, src.base_commit, &out)
                    .map_err(|e| H5iError::Metadata(format!("could not bundle the source: {e}")))?;
                (
                    SourceSpec {
                        kind: SourceKind::GitBundle,
                        bytes: bundle.bytes,
                        sha256: bundle.sha256.clone(),
                        base_commit: Some(bundle.base_commit.clone()),
                    },
                    Some(bundle),
                )
            }
            None => (
                SourceSpec {
                    kind: SourceKind::Empty,
                    bytes: 0,
                    sha256: String::new(),
                    base_commit: None,
                },
                None,
            ),
        };

        let request = CreateRequest {
            box_id: spec.box_id.to_string(),
            // A fresh attempt id per call, so a retry after a crash cannot be
            // confused with the attempt that crashed. Idempotency is carried by
            // the request digest, not by this.
            operation_id: format!("op-{}", &request_digest(spec, &source_spec)[..16]),
            request_digest: request_digest(spec, &source_spec),
            isolation: spec.isolation.to_string(),
            image: spec.image.map(str::to_string),
            limits: ResourceLimits::default(),
            lease: LeaseSpec::default(),
            policy_digest: spec.policy_digest.to_string(),
            policy: spec.policy_json.clone(),
            source: source_spec,
        };

        let result = self
            .client
            .create(&request, bundle.as_ref())
            .map_err(|e| H5iError::Metadata(format!("runner `{}`: {e}", self.record.name)))?;

        Ok(RemoteCreated {
            runner_id: self.record.runner_id.clone(),
            runner: self.record.name.clone(),
            workspace: result.workspace,
            policy_digest: result.policy_digest,
            existing: result.existing,
        })
    }

    fn exec(&self, exec: &RemoteExec<'_>) -> Result<RemoteExecResult, H5iError> {
        let request = ExecRequest {
            box_id: exec.box_id.to_string(),
            argv: exec.argv.to_vec(),
            cwd: exec.cwd.map(str::to_string),
            env: exec.env.to_vec(),
            timeout_secs: exec.timeout_secs,
        };
        let out = self
            .client
            .exec(&request)
            .map_err(|e| H5iError::Metadata(format!("runner `{}`: {e}", self.record.name)))?;

        Ok(RemoteExecResult {
            stdout: out.stdout,
            stderr: out.stderr,
            exit_code: out.exit.exit_code,
            timed_out: out.exit.timed_out,
            wall_ms: out.exit.wall_ms,
            cpu_ms: out.exit.cpu_ms,
            max_rss_kb: out.exit.max_rss_kb,
            // The egress summary crosses as opaque JSON and is parsed back into
            // the product's own type here. A summary that will not parse is
            // dropped rather than guessed at: absent evidence is honest, and
            // invented evidence is not.
            egress: out
                .exit
                .egress
                .and_then(|v| serde_json::from_value(v).ok()),
            cwd: out.started.cwd,
            output_truncated: out.exit.output_truncated,
        })
    }

    fn export(&self, box_id: &str, into: &std::path::Path) -> Result<RemoteExported, H5iError> {
        let described = self
            .client
            .export(box_id, into)
            .map_err(|e| H5iError::Metadata(format!("runner `{}`: {e}", self.record.name)))?;
        Ok(RemoteExported {
            tip_commit: described.tip_commit,
            tip_tree: described.tip_tree,
            has_changes: described.has_changes,
        })
    }

    fn destroy(&self, box_id: &str) -> Result<(), H5iError> {
        self.client
            .destroy(box_id, false)
            .map(|_| ())
            .map_err(|e| H5iError::Metadata(format!("runner `{}`: {e}", self.record.name)))
    }
}

/// The idempotency key: a digest over everything that decides what gets built.
/// Deliberately *not* over the whole request. `operation_id` names the attempt
/// and must not be in it, or every retry would look like a different box and
/// the idempotent case (design-runner.md R7) would never trigger. The lease is
/// out for the same reason: asking for a longer lease on the same box is not a
/// request for a different box.
fn request_digest(spec: &RemoteCreateSpec<'_>, source: &SourceSpec) -> String {
    let material = serde_json::json!({
        "box_id": spec.box_id,
        "isolation": spec.isolation,
        "image": spec.image,
        "policy_digest": spec.policy_digest,
        "source_kind": source.kind,
        "source_sha256": source.sha256,
        "base_commit": source.base_commit,
    });
    h5i_core::refstore::sha256_hex(material.to_string().as_bytes())
}

/// Remove a box from the runner it lives on, by manifest.
///
/// Best effort by design: a runner that cannot be reached must not stop a box
/// being removed from this side, or an unreachable machine would pin a record
/// here forever. What could not be done is returned for the caller to say.
pub fn destroy_remote(manifest: &h5i_core::env::EnvManifest) -> Option<String> {
    let name = manifest.runner.as_deref()?;
    let box_id = h5i_core::placement::remote_box_id(&manifest.id);

    let runner = match PairedRunner::open(name) {
        Ok(r) => r,
        Err(e) => {
            return Some(format!(
                "could not reach runner `{name}` to remove `{box_id}` there: {e}"
            ));
        }
    };

    // The identity, not the name.
    if let Err(e) = runner.check_identity(manifest) {
        return Some(format!("{e} — `{box_id}` was left alone"));
    }

    match runner.destroy(&box_id) {
        Ok(()) => None,
        Err(e) => Some(format!("could not remove `{box_id}` from `{name}`: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec<'a>(box_id: &'a str, digest: &'a str) -> RemoteCreateSpec<'a> {
        RemoteCreateSpec {
            box_id,
            isolation: "container",
            image: Some("debian"),
            policy_json: serde_json::json!({"profile": "agent"}),
            policy_digest: digest,
            source: None,
        }
    }

    fn source_of(sha: &str) -> SourceSpec {
        SourceSpec {
            kind: SourceKind::GitBundle,
            bytes: 10,
            sha256: sha.into(),
            base_commit: Some("deadbeef".into()),
        }
    }

    #[test]
    fn the_same_box_hashes_the_same_way_twice() {
        // Idempotency depends on it: a digest that varied per call would make
        // every retry look like a different box.
        let a = request_digest(&spec("demo", &"a".repeat(64)), &source_of("c"));
        let b = request_digest(&spec("demo", &"a".repeat(64)), &source_of("c"));
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn anything_that_changes_what_gets_built_changes_the_digest() {
        let base = request_digest(&spec("demo", &"a".repeat(64)), &source_of("c"));

        assert_ne!(
            base,
            request_digest(&spec("other", &"a".repeat(64)), &source_of("c")),
            "a different name is a different box"
        );
        assert_ne!(
            base,
            request_digest(&spec("demo", &"b".repeat(64)), &source_of("c")),
            "a different policy is a different box"
        );
        assert_ne!(
            base,
            request_digest(&spec("demo", &"a".repeat(64)), &source_of("d")),
            "different source bytes are a different box"
        );

        let digest = "a".repeat(64);
        let mut tier = spec("demo", &digest);
        tier.isolation = "process";
        assert_ne!(
            base,
            request_digest(&tier, &source_of("c")),
            "a different tier is a different box"
        );
    }
}
