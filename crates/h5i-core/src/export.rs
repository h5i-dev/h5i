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
//!
//! `report.md` carries a **What the browser saw** section built from the
//! per-run browser evidence ([`crate::browser`]) rather than from the agent's
//! account of its own testing — the case it exists for is a report that says
//! "verified in the browser" over a page that threw an uncaught exception.
//! Screenshots join the bundle with the viewer (roadmap M5).

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
/// Escape the markdown a box can steer.
///
/// `sanitize_display` strips control characters and bidi marks, which is the
/// right job for a terminal, but this text lands in `report.md` — a document a
/// reviewer reads as evidence. A recorded command containing a backtick closes
/// the code span around it, so `` x` — **reviewed and approved** ` `` renders
/// as authoritative prose next to the run it describes. Newlines are already
/// flattened, so no new table rows can be forged; this closes the inline half.
fn md_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '`' | '|' | '[' | ']' | '(' | ')' | '*' | '_' | '<' | '>' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A command, as a Markdown code span inside a table cell.
///
/// Not [`md_escape`] plus backticks, which is what this was. Backslash escapes
/// are not processed inside a code span, so every command containing a
/// parenthesis rendered to the reviewer with the backslashes still in it —
/// `h5i box share demo \(port 3000, 1 peer\(s\), 20s\)`. The one character a
/// table cell genuinely needs escaped is the pipe, which GFM splits on before
/// any inline parsing and which is honoured inside code spans for exactly that
/// reason.
///
/// A backtick in the content is handled the way CommonMark intends: a fence
/// longer than the longest run inside it, padded with spaces when the content
/// starts or ends with one.
fn md_code(s: &str) -> String {
    let escaped = s.replace('|', "\\|");
    let longest = escaped
        .split(|c| c != '`')
        .map(|run| run.len())
        .max()
        .unwrap_or(0);
    let fence = "`".repeat(longest + 1);
    let pad = if escaped.starts_with('`') || escaped.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{fence}{pad}{escaped}{pad}{fence}")
}

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

    // Asked before the freeze, purely so the answer is the true one. A share
    // needs a live process inside the box, that process holds `run.lock`, and
    // `propose` below takes the same lock — so an export attempted during a
    // share has always failed with "environment is busy — another `h5i box
    // run`/`shell` or lifecycle op holds it". True, and not the fact the
    // operator needs: what is holding it is the session their share is
    // standing on, and stopping the share is the thing to do about it.
    if let Some(sh) = crate::share_record::read_live(&m.dir(h5i_root)) {
        return Err(H5iError::Metadata(format!(
            "{} is being shared right now by pid {} — the export has to freeze the box into a \
             commit, and it cannot do that under somebody who is looking at it. Stop the share \
             first (`h5i box share stop {}`), then export: the ingress receipt is written when \
             the share ends, so exporting afterwards is also the only way to get the account of \
             who connected.",
            m.id, sh.pid, m.slug
        )));
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
    // Read straight off disk rather than plumbed in: this crate is below
    // `h5i-share`, and the one fact needed here — is a live process serving
    // this box right now — is a two-line read of a file that sits beside the
    // receipt log.
    let live_share = crate::share_record::read_live(&m.dir(h5i_root)).map(|s| s.pid.to_string());
    std::fs::write(
        &report_path,
        report(m, &summary, &records, &brief, live_share.as_deref()).as_bytes(),
    )
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
    live_share: Option<&str>,
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
                "| {} | {} | {} | {} |\n",
                r.timestamp,
                r.source,
                r.exit_code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".into()),
                md_code(&crate::redact::sanitize_display(r.cmd.as_deref().unwrap_or(""))),
            ));
        }
    }

    // What the page said, gathered separately from what the agent said about
    // it. A UI change whose report claims "verified in the browser" and whose
    // evidence section lists an uncaught TypeError is the case this exists for,
    // so it goes above the agent-authored proposal rather than below it.
    let browser: Vec<(&crate::receipt::ExecRecord, &crate::receipt::BrowserEvidence)> = records
        .iter()
        .filter_map(|r| r.browser.as_ref().map(|b| (r, b)))
        .collect();
    if !browser.is_empty() {
        out.push_str("\n## What the browser saw\n\n");
        let findings: Vec<_> = browser.iter().filter(|(_, b)| !b.is_clean()).collect();
        if findings.is_empty() {
            out.push_str(&format!(
                "{} browser command(s) ran and the page reported no console errors, \
                 no uncaught exceptions and no failed requests.\n",
                browser.len()
            ));
        } else {
            out.push_str(
                "Observed in the box's own browser, not reported by the agent. \
                 Each line is a console error, an uncaught exception, or a request \
                 that failed.\n\n",
            );
            for (r, b) in findings {
                out.push_str(&format!(
                    "- {} ({})\n",
                    md_code(&crate::redact::sanitize_display(
                        b.verb.as_deref().unwrap_or("browser")
                    )),
                    r.timestamp
                ));
                for line in b
                    .errors
                    .iter()
                    .chain(b.console.iter())
                    .chain(b.failed_requests.iter())
                {
                    out.push_str(&format!(
                        "  - {}\n",
                        md_escape(&crate::redact::sanitize_display(line))
                    ));
                }
                if b.truncated {
                    out.push_str("  - _(more findings than the per-record cap; list truncated)_\n");
                }
            }
        }
        // "Nothing was looked at" is a different claim from "nothing was
        // wrong", and a reviewer has to be able to tell them apart.
        let blind = browser.iter().filter(|(_, b)| b.unavailable).count();
        if blind > 0 {
            out.push_str(&format!(
                "\n_{blind} browser command(s) ran with no browser available to observe, \
                 so nothing was collected for them._\n"
            ));
        }
    }

    // Who was at the controls. A patch produced with a human driving the
    // browser is a different artifact from one an agent produced alone, and a
    // reviewer should not have to infer which this was.
    let viewer: Vec<_> = records.iter().filter(|r| r.source == "viewer").collect();
    if !viewer.is_empty() {
        out.push_str("\n## Viewer sessions\n\n");
        out.push_str(
            "Observed by h5i's own forward, not by anything in the box.\n\n\
             | when | session |\n|---|---|\n",
        );
        for r in &viewer {
            out.push_str(&format!(
                "| {} | {} |\n",
                r.timestamp,
                md_escape(&crate::redact::sanitize_display(r.cmd.as_deref().unwrap_or("")))
            ));
        }
        if viewer
            .iter()
            .any(|r| r.cmd.as_deref().unwrap_or("").contains("human took control"))
        {
            out.push_str(
                "\n**A human took control of the browser during this box's life.** \
                 Some of what the agent reports having verified may have been done by hand.\n",
            );
        }
    }

    // Who was let *in*. Every other lane in an export is about what left the
    // box; this is the one path that let a second person reach inside it while
    // it was running, and a reviewer who cannot see it is reviewing a different
    // artifact from the one that exists. It was rendered as an ordinary row in
    // "What ran" — `h5i box share demo (port 3000, 1 peer(s), 600s)` — which
    // reads as a command the box happened to execute.
    // A share that is *still running* has written no receipt — it writes one
    // when it ends — so an export taken during a demo was silent about the box
    // having been opened to somebody, which is the moment somebody is most
    // likely to take one. The manual says an export "should not be silent
    // about which one it came from"; taken mid-share, it was exactly that.
    if live_share.is_some() {
        out.push_str("\n## Shared with someone, right now\n\n");
        out.push_str(
            "**This box was being shared when this export was taken.** Somebody outside was \
             able to reach a server inside it while this patch was being produced. The \
             receipt for that session is written when the share ends, so it is not in this \
             bundle — export again afterwards if you need the account of who connected.\n",
        );
    }

    let shares: Vec<_> = records.iter().filter(|r| r.source == "share").collect();
    if !shares.is_empty() {
        out.push_str("\n## Shared with someone\n\n");
        out.push_str(
            "This box was opened to another person while it was running. h5i owned both ends \
             of the bridge, so this is host-observed evidence and the box supplied none of \
             it.\n\n| when | session |\n|---|---|\n",
        );
        for r in &shares {
            out.push_str(&format!(
                "| {} | {} |\n",
                r.timestamp,
                md_escape(&crate::redact::sanitize_display(r.cmd.as_deref().unwrap_or("")))
            ));
        }
        if shares
            .iter()
            .any(|r| r.cmd.as_deref().unwrap_or("").contains("tunnel"))
        {
            out.push_str(
                "\n**At least one of these went through a Cloudflare quick tunnel**, which \
                 terminates TLS — so that traffic was not end to end encrypted.\n",
            );
        }
        out.push_str(
            "\nThe full account of each — who connected, over what path, for how long, and \
             what was refused — is the receipt payload named by `raw_oid` in \
             `receipt.json`.\n",
        );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::receipt::{BrowserEvidence, ExecRecord};

    #[test]
    fn the_mid_share_section_is_reachable_only_in_the_gap_it_describes() {
        // Kept, and worth being exact about why. A live share needs a live
        // process inside the box; that process holds `run.lock`; and `export`
        // freezes through `propose`, which takes it — so the ordinary
        // mid-share export never gets this far, and now fails with a message
        // that names the share instead of the lock. What remains reachable is
        // the second between the box session ending and the share process
        // noticing and exiting: the lock is free, the record is still live.
        // That is a real second, and it is the one this section is for.
        let m = manifest();
        let s = summary();
        let body = report(&m, &s, &[], "", Some("4321"));
        assert!(body.contains("Shared with someone, right now"), "{body}");
        assert!(body.contains("export again afterwards"), "{body}");

        let quiet = report(&m, &s, &[], "", None);
        assert!(!quiet.contains("right now"), "{quiet}");
    }

    #[test]
    fn a_command_in_a_table_reaches_the_reviewer_without_its_escapes() {
        // Backslash escapes are not processed inside a code span, so escaping
        // the content and then wrapping it in backticks put the backslashes on
        // the reviewer's screen. Every share receipt says "(port 3000, 1
        // peer(s), 20s)", so every export of a shared box carried four of
        // them: `h5i box share demo \(port 3000, 1 peer\(s\), 20s\)`.
        let cell = md_code("h5i box share demo (port 3000, 1 peer(s), 20s)");
        assert_eq!(cell, "`h5i box share demo (port 3000, 1 peer(s), 20s)`");
        assert!(!cell.contains('\\'));

        // The pipe is the exception, because GFM splits a row on pipes before
        // it parses anything inline — so that one really does need escaping,
        // code span or not.
        assert_eq!(md_code("a | b"), "`a \\| b`");

        // A backtick in the command gets a longer fence rather than a broken
        // cell, and content that starts or ends with one gets the padding
        // CommonMark asks for.
        // Padded on both sides, which is what CommonMark strips: one space
        // either end of a code span whose content touches a backtick.
        assert_eq!(md_code("echo `date`"), "`` echo `date` ``");
        assert_eq!(md_code("`x`"), "`` `x` ``");
        assert_eq!(md_code("a ``b`` c"), "```a ``b`` c```");

        // And the plain-cell escaper is still the right tool for a plain cell.
        assert_eq!(md_escape("a (b)"), "a \\(b\\)");
    }

    fn manifest() -> EnvManifest {
        EnvManifest {
            id: "env/tester/ui".into(),
            agent: "tester".into(),
            slug: "ui".into(),
            base_commit: "a".repeat(40),
            base_tree: "b".repeat(40),
            parent_branch: "main".into(),
            branch: "refs/heads/h5i/env/tester/ui".into(),
            source: "repo".into(),
            profile: "browser".into(),
            policy_digest: "d".repeat(64),
            isolation_claim: "supervised".into(),
            backend: "worktree".into(),
            created_at: "2026-08-05T00:00:00.000000Z".into(),
            updated_at: "2026-08-05T00:00:00.000000Z".into(),
            status: "proposed".into(),
            captures: Vec::new(),
            service_digest: None,
            persona_digest: None,
            pr: None,
            pr_head_ref: None,
        }
    }

    fn summary() -> ExportSummary {
        ExportSummary {
            env_id: "env/tester/ui".into(),
            dir: PathBuf::from("/out"),
            files_changed: 1,
            insertions: 2,
            deletions: 0,
            patch_bytes: 10,
            receipts: 1,
            egress_denied: 0,
            redactions: Vec::new(),
        }
    }

    fn record(browser: Option<BrowserEvidence>) -> ExecRecord {
        ExecRecord {
            id: "0123456789abcdef".into(),
            timestamp: "2026-08-05T00:00:01.000000Z".into(),
            env_id: "env/tester/ui".into(),
            policy_digest: None,
            source: "host-env-run".into(),
            cmd: Some("agent-browser click @e2".into()),
            cwd: None,
            exit_code: Some(0),
            timed_out: false,
            wall_ms: None,
            cpu_ms: None,
            max_rss_kb: None,
            git_tree: None,
            files: Vec::new(),
            egress: None,
            browser,
            redactions: Vec::new(),
            raw_oid: "sha256:0".into(),
            raw_size: 0,
            raw_lines: 0,
            raw_truncated: false,
        }
    }

    #[test]
    fn the_report_shows_what_the_page_said_not_what_the_agent_said() {
        let ev = BrowserEvidence {
            verb: Some("click".into()),
            console: vec!["[error] widget failed to mount".into()],
            errors: vec!["TypeError: cannot read 'boom' of null".into()],
            failed_requests: vec!["500 POST /api/save".into()],
            ..Default::default()
        };
        let text = report(&manifest(), &summary(), &[record(Some(ev))], "brief", None);

        assert!(text.contains("## What the browser saw"), "{text}");
        assert!(text.contains("TypeError: cannot read 'boom' of null"), "{text}");
        assert!(text.contains("widget failed to mount"), "{text}");
        assert!(text.contains("500 POST /api/save"), "{text}");
        // Above the agent's own proposal: a reviewer should meet the observed
        // failures before the account that may not mention them.
        assert!(
            text.find("What the browser saw") < text.find("## Proposal"),
            "{text}"
        );
    }

    #[test]
    fn a_clean_page_and_an_unobserved_one_read_differently() {
        let clean = BrowserEvidence {
            verb: Some("snapshot".into()),
            ..Default::default()
        };
        let text = report(&manifest(), &summary(), &[record(Some(clean))], "brief", None);
        assert!(text.contains("no console errors"), "{text}");

        // The distinction that matters: this one was never looked at, and the
        // report must not let it read as a page that came back clean.
        let blind = BrowserEvidence {
            verb: Some("click".into()),
            unavailable: true,
            ..Default::default()
        };
        let text = report(&manifest(), &summary(), &[record(Some(blind))], "brief", None);
        assert!(text.contains("no browser available to observe"), "{text}");
    }

    #[test]
    fn an_export_taken_during_a_share_says_the_share_is_still_open() {
        // A live share has written no receipt yet, so an export taken during a
        // demo said nothing at all about the box having been opened to
        // somebody — and during a demo is when somebody takes one.
        let text = report(&manifest(), &summary(), &[record(None)], "brief", Some("4242"));
        assert!(text.contains("Shared with someone, right now"), "{text}");
        assert!(text.contains("written when the share ends"), "{text}");

        // And a box nobody is sharing does not grow the section.
        let quiet = report(&manifest(), &summary(), &[record(None)], "brief", None);
        assert!(!quiet.contains("right now"), "{quiet}");
    }

    #[test]
    fn a_box_that_was_shared_with_somebody_says_so() {
        // The one path that lets a second person reach *into* a running box,
        // rendered as an ordinary row in "What ran" — which reads as a command
        // the box happened to execute rather than as a door that was opened.
        let mut r = record(None);
        r.source = "share".into();
        r.cmd = Some("h5i box share demo --tunnel (port 3000, 1 peer(s), 600s)".into());
        let text = report(&manifest(), &summary(), &[r], "brief", None);
        assert!(text.contains("## Shared with someone"), "{text}");
        assert!(text.contains("terminates TLS"), "{text}");
        assert!(text.contains("raw_oid"), "{text}");

        // And a box nobody shared does not grow the section.
        let text = report(&manifest(), &summary(), &[record(None)], "brief", None);
        assert!(!text.contains("## Shared with someone"), "{text}");
    }

    #[test]
    fn a_human_at_the_controls_is_called_out() {
        let mut r = record(None);
        r.source = "viewer".into();
        r.cmd = Some("h5i box view (human took control, 42s)".into());
        let text = report(&manifest(), &summary(), &[r], "brief", None);

        assert!(text.contains("## Viewer sessions"), "{text}");
        // The load-bearing sentence: some of what the agent claims to have
        // verified may have been done by hand, and the reviewer has to know.
        assert!(text.contains("A human took control"), "{text}");

        // A session where nobody took over is listed but not flagged.
        let mut watched = record(None);
        watched.source = "viewer".into();
        watched.cmd = Some("h5i box view (agent, 42s)".into());
        let text = report(&manifest(), &summary(), &[watched], "brief", None);
        assert!(text.contains("## Viewer sessions"), "{text}");
        assert!(!text.contains("A human took control"), "{text}");
    }

    #[test]
    fn a_run_that_never_touched_a_browser_gets_no_section() {
        // Most boxes are not browser boxes; they should not carry an empty
        // heading implying an inspection that never happened.
        let text = report(&manifest(), &summary(), &[record(None)], "brief", None);
        assert!(!text.contains("What the browser saw"), "{text}");
    }
}
