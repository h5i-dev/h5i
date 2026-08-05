//! The output gate: the only path from a box back to the host.
//!
//! A box has no write access to anything outside itself. What leaves it leaves
//! here, as an inspectable bundle a human reads before applying:
//!
//! ```text
//! <out>/
//!   patch.diff    the tree diff against the pinned base, path-validated
//!   report.md     what the box was, what it changed, what it ran
//!   receipt.json  every observed execution, with the enforced policy digest
//! ```
//!
//! The patch is produced by the same mediated commit that `propose` runs, so
//! the `$WORK` allowlist invariants (no symlink escape, no nested `.git`, no
//! agent-introduced gitlink) hold for anything that reaches this directory.
//! Screenshots join the bundle when the browser layer lands (roadmap M4).

use git2::Repository;
use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::env::{self, EnvManifest};
use crate::error::H5iError;

/// What an export produced, for the caller to render.
#[derive(Debug, Clone, Serialize)]
pub struct ExportSummary {
    pub env_id: String,
    pub dir: PathBuf,
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub patch_bytes: u64,
    pub receipts: usize,
    /// Denied egress attempts across every receipt. Non-zero is worth a look
    /// before applying: the box tried to reach something the policy refused.
    pub egress_denied: u64,
    /// Distinct secret-redaction rules that fired while recording.
    pub redactions: Vec<String>,
}

/// The machine-readable half of the bundle.
#[derive(Debug, Serialize)]
struct ReceiptBundle<'a> {
    env_id: &'a str,
    agent: &'a str,
    profile: &'a str,
    isolation_claim: &'a str,
    policy_digest: &'a str,
    base_commit: &'a str,
    parent_branch: &'a str,
    branch: &'a str,
    exported_at: String,
    records: Vec<crate::receipt::ExecRecord>,
}

/// Freeze the box and write the bundle to `out`.
///
/// Refuses rather than overwrites: an existing non-empty directory is an error
/// unless `force`, because an export is evidence and silently replacing one is
/// how evidence goes missing.
pub fn export(
    repo: &Repository,
    h5i_root: &Path,
    m: &mut EnvManifest,
    out: &Path,
    force: bool,
) -> Result<ExportSummary, H5iError> {
    if out.exists() {
        let empty = std::fs::read_dir(out)
            .map(|mut d| d.next().is_none())
            .unwrap_or(false);
        if !empty && !force {
            return Err(H5iError::Metadata(format!(
                "{} already exists and is not empty — pass --force to replace it",
                out.display()
            )));
        }
    }

    // Same freeze as `propose`: the mediated commit is what makes the diff
    // trustworthy, so export never reads a live worktree directly.
    let brief = env::propose(repo, h5i_root, m)?;

    let patch = env::diff(repo, h5i_root, m, false)?;
    let (files_changed, insertions, deletions) =
        env::diffstat_numbers(repo, h5i_root, m).unwrap_or((0, 0, 0));

    let records = crate::receipt::list(&env::env_dir(h5i_root, &m.agent, &m.slug))?;
    let egress_denied: u64 = records
        .iter()
        .filter_map(|r| r.egress.as_ref())
        .map(|e| e.denied)
        .sum();
    let mut redactions: Vec<String> = records
        .iter()
        .flat_map(|r| r.redactions.iter().cloned())
        .collect();
    redactions.sort();
    redactions.dedup();

    std::fs::create_dir_all(out).map_err(|e| H5iError::with_path(e, out))?;
    let patch_path = out.join("patch.diff");
    std::fs::write(&patch_path, patch.as_bytes()).map_err(|e| H5iError::with_path(e, &patch_path))?;

    let bundle = ReceiptBundle {
        env_id: &m.id,
        agent: &m.agent,
        profile: &m.profile,
        isolation_claim: &m.isolation_claim,
        policy_digest: &m.policy_digest,
        base_commit: &m.base_commit,
        parent_branch: &m.parent_branch,
        branch: &m.branch,
        exported_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        records: records.clone(),
    };
    let receipt_path = out.join("receipt.json");
    std::fs::write(&receipt_path, serde_json::to_vec_pretty(&bundle)?)
        .map_err(|e| H5iError::with_path(e, &receipt_path))?;

    let summary = ExportSummary {
        env_id: m.id.clone(),
        dir: out.to_path_buf(),
        files_changed,
        insertions,
        deletions,
        patch_bytes: patch.len() as u64,
        receipts: records.len(),
        egress_denied,
        redactions,
    };

    let report_path = out.join("report.md");
    std::fs::write(&report_path, report(m, &summary, &records, &brief).as_bytes())
        .map_err(|e| H5iError::with_path(e, &report_path))?;

    Ok(summary)
}

/// The human half of the bundle: what this box was, what it changed, and every
/// command it ran. Written from the identity-validated manifest and the
/// receipts, never from anything the box wrote into `$WORK`.
fn report(
    m: &EnvManifest,
    s: &ExportSummary,
    records: &[crate::receipt::ExecRecord],
    brief: &str,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Export: {}\n\n", m.id));
    out.push_str(&format!("- base: `{}` (from `{}`)\n", m.base_commit, m.parent_branch));
    out.push_str(&format!("- branch: `{}`\n", m.branch));
    out.push_str(&format!("- profile: `{}`\n", m.profile));
    out.push_str(&format!(
        "- isolation enforced: `{}`\n",
        m.isolation_claim
    ));
    out.push_str(&format!("- policy digest: `{}`\n", m.policy_digest));
    out.push_str(&format!(
        "- changes: {} file(s), +{} -{}\n",
        s.files_changed, s.insertions, s.deletions
    ));
    if s.egress_denied > 0 {
        out.push_str(&format!(
            "- **egress denied: {}** — the box tried to reach hosts the policy refused\n",
            s.egress_denied
        ));
    }
    if !s.redactions.is_empty() {
        out.push_str(&format!(
            "- secrets redacted while recording: {}\n",
            s.redactions.join(", ")
        ));
    }

    out.push_str("\n## What ran\n\n");
    if records.is_empty() {
        out.push_str("_No commands were recorded in this box._\n");
    } else {
        out.push_str("| when | lane | exit | command |\n|---|---|---|---|\n");
        for r in records {
            out.push_str(&format!(
                "| {} | {} | {} | `{}` |\n",
                r.timestamp,
                r.source,
                r.exit_code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".into()),
                crate::redact::sanitize_display(r.cmd.as_deref().unwrap_or("")).replace('|', "\\|"),
            ));
        }
    }

    out.push_str("\n## Proposal\n\n```\n");
    out.push_str(brief);
    out.push_str("\n```\n");
    out.push_str(
        "\n## Applying\n\nReview `patch.diff`, then apply it in the target repository:\n\n\
         ```bash\ngit apply --3way patch.diff\n```\n",
    );
    out
}
