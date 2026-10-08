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
//! The patch comes from the same mediated commit `propose` runs
//! ([`env::DiffSource::Proposed`]), so the `$WORK` allowlist invariants hold
//! for anything reaching this directory.
//!
//! `report.md` builds its "What the browser saw" section from per-run browser
//! evidence rather than the agent's account of its own testing, for the case
//! where a report says "verified in the browser" over a page that threw an
//! uncaught exception.

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
    /// Boxes whose effective filesystem grants overlapped this one's, as the
    /// newest run/shell receipt recorded them (`env/<id> via <path>`).
    /// Latest-record semantics, matching the console. A reviewer applying
    /// this bundle should know the box did not run alone.
    pub fs_overlap: Vec<String>,
    /// Browser sessions that ran inside this box, with their timelines written
    /// beside the patch. Empty when none did.
    pub browser_sessions: Vec<ExportedSession>,
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
    /// One entry per browser session placed in this box. The rows themselves
    /// are in `browser/<id>.json`: a session that loaded a busy page is
    /// thousands of them, and `receipt.json` has to stay a document a person
    /// can open.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    browser_sessions: Vec<ExportedSession>,
}

/// Freeze the box and write the bundle to `out`.
fn md_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '`' | '|' | '[' | ']' | '(' | ')' | '*' | '_' | '<' | '>' | '\\'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A command, as a Markdown code span inside a table cell.
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

    // Same freeze as `propose`: the mediated commit is what makes the diff
    // trustworthy, so export never reads a live worktree directly.
    let brief = env::propose(repo, h5i_root, m)?;

    // From the commit `propose` just made, not from the live worktree. The
    // worktree is what `env::diff` reads by default, and reading it here meant
    // `patch.diff` had passed no part of the output gate and need not match the
    // commit `receipt.json` attests to — while `report.md` ends by telling a
    // reviewer to `git apply --3way patch.diff`.
    let patch = env::diff_from(repo, h5i_root, m, false, env::DiffSource::Proposed)?;
    let (files_changed, insertions, deletions) =
        env::diffstat_numbers(repo, h5i_root, m, env::DiffSource::Proposed).unwrap_or((0, 0, 0));

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
    let fs_overlap: Vec<String> = records
        .iter()
        .rfind(|r| matches!(r.source.as_str(), "host-env-run" | "host-env-shell"))
        .map(|r| r.fs_overlap.clone())
        .unwrap_or_default();

    std::fs::create_dir_all(out).map_err(|e| H5iError::with_path(e, out))?;
    let patch_path = out.join("patch.diff");
    std::fs::write(&patch_path, patch.as_bytes())
        .map_err(|e| H5iError::with_path(e, &patch_path))?;

    // Every browser session that was placed in this box, with its whole
    // timeline. One file per session rather than a section of `receipt.json`,
    // because a session that loaded a busy page is thousands of rows and the
    // receipt has to stay a document somebody reads.
    //
    // The bundle stands alone or it is not evidence, so these are copied in
    // rather than pointed at: a session's own directory lives under the user's
    // state root and does not travel with the export.
    let sessions = export_browser_audits(m, out);

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
        browser_sessions: sessions.clone(),
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
        fs_overlap,
        browser_sessions: sessions.clone(),
    };

    let report_path = out.join("report.md");
    std::fs::write(
        &report_path,
        report(
            m,
            &summary,
            &records,
            &brief,
            &sessions,
        )
        .as_bytes(),
    )
    .map_err(|e| H5iError::with_path(e, &report_path))?;

    Ok(summary)
}

/// The human half of the bundle: what this box was, what it changed, and every command it ran.
#[derive(Debug, Clone, Serialize)]
pub struct ExportedSession {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub url: String,
    pub state: String,
    /// `engine-claimed` or `host-observed`. The claim this session's network
    /// record can actually support, carried into the export so a reviewer does
    /// not have to reconstruct it from the isolation tier.
    pub lane: String,
    pub events: usize,
    /// Which of the audit's sources could be read. A source that could not is
    /// the difference between a quiet session and one nobody watched.
    pub sources: crate::browser_session::Sources,
    /// The file inside the bundle, relative to its root.
    pub file: String,
}

/// Write one `browser/<id>.json` per session placed in this box.
///
/// Best effort, and silent when there are none: a box with no browser session
/// in it should not grow an empty directory to explain. What is *not* silent is
/// a session whose logs could not be read. That travels inside the audit as
/// `unavailable`, because "no rows" and "no log" are different findings.
fn export_browser_audits(m: &EnvManifest, out: &Path) -> Vec<ExportedSession> {
    let Ok(root) = crate::browser_session::root() else {
        return Vec::new();
    };
    let Ok(all) = crate::browser_session::list(&root) else {
        return Vec::new();
    };
    let mine: Vec<_> = all
        .into_iter()
        .filter(|s| s.placement.box_name() == Some(m.slug.as_str()))
        .collect();
    if mine.is_empty() {
        return Vec::new();
    }

    let dir = out.join("browser");
    if std::fs::create_dir_all(&dir).is_err() {
        return Vec::new();
    }

    let mut exported = Vec::new();
    for session in mine {
        let audit = crate::browser_session::audit(&root, &session);
        let file = format!("browser/{}.json", session.id);
        let Ok(body) = serde_json::to_vec_pretty(&audit) else {
            continue;
        };
        if std::fs::write(out.join(&file), body).is_err() {
            continue;
        }
        exported.push(ExportedSession {
            id: session.id.clone(),
            name: session.name.clone(),
            url: session.url.clone(),
            state: session.state.as_str().to_string(),
            lane: session.lane.as_str().to_string(),
            events: audit.events.len(),
            sources: audit.sources.clone(),
            file,
        });
    }
    exported
}

fn report(
    m: &EnvManifest,
    s: &ExportSummary,
    records: &[crate::receipt::ExecRecord],
    brief: &str,
    sessions: &[ExportedSession],
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Export: {}\n\n", m.id));
    out.push_str(&format!(
        "- base: `{}` (from `{}`)\n",
        m.base_commit, m.parent_branch
    ));
    out.push_str(&format!("- branch: `{}`\n", m.branch));
    out.push_str(&format!("- profile: `{}`\n", m.profile));
    out.push_str(&format!("- isolation enforced: `{}`\n", m.isolation_claim));
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
    if !s.fs_overlap.is_empty() {
        out.push_str(&format!(
            "- writable-path overlap with other boxes (last run): {}\n",
            s.fs_overlap.join("; ")
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
                md_code(&crate::redact::sanitize_display(
                    r.cmd.as_deref().unwrap_or("")
                )),
            ));
        }
    }

    // What the page said, gathered separately from what the agent said about
    // it. A UI change whose report claims "verified in the browser" and whose
    // evidence section lists an uncaught TypeError is the case this exists for,
    // so it goes above the agent-authored proposal rather than below it.
    let browser: Vec<(
        &crate::receipt::ExecRecord,
        &crate::receipt::BrowserEvidence,
    )> = records
        .iter()
        .filter_map(|r| r.browser.as_ref().map(|b| (r, b)))
        .collect();
    if !sessions.is_empty() {
        out.push_str("\n## Browser sessions\n\n");
        out.push_str(
            "Each session's whole timeline is beside this file: what the agent asked for, what \
             the engine decided about every fetch, who was driving, and how it ended. The \
             engine's rows are its own account of itself; the handovers and the endings are \
             h5i's, written from outside. **The two are never merged.**\n\n",
        );
        out.push_str("| session | lane | state | rows | sources | file |\n");
        out.push_str("|---|---|---|---|---|---|\n");
        for session in sessions {
            let name = session
                .name
                .as_deref()
                .map(|n| format!("`{}` ({})", md_escape(n), session.id))
                .unwrap_or_else(|| format!("`{}`", session.id));
            out.push_str(&format!(
                "| {name} | `{}` | {} | {} | actions {} · requests {} · control {} | `{}` |\n",
                session.lane,
                session.state,
                session.events,
                session.sources.actions.as_str(),
                session.sources.requests.as_str(),
                session.sources.control.as_str(),
                session.file,
            ));
        }
        // The thing a reviewer has to be told rather than left to infer, and
        // the one claim in this bundle that is checkable.
        let unverified = sessions.iter().any(|x| x.lane != "host-observed");
        if unverified {
            out.push_str(
                "\nA session marked `engine-claimed` is the browser's own account of what it \
                 fetched: fail-closed, complete, and still the browser describing itself. \
                 Nothing outside it corroborated the list.\n",
            );
        }
        let blind = sessions
            .iter()
            .filter(|x| {
                x.sources.actions == crate::browser_session::Availability::Unavailable
                    || x.sources.requests == crate::browser_session::Availability::Unavailable
            })
            .count();
        if blind > 0 {
            out.push_str(&format!(
                "\n**{blind} session(s) had a log this machine could not read.** An empty \
                 timeline there is not evidence of a quiet session.\n"
            ));
        }
        // A log that was read *in part* is a different fact from one that could
        // not be read at all, and reporting it as neither would make a timeline
        // that stops at the cap read as a session that went quiet.
        let cut = sessions
            .iter()
            .filter(|x| {
                x.sources.actions == crate::browser_session::Availability::Partial
                    || x.sources.requests == crate::browser_session::Availability::Partial
            })
            .count();
        if cut > 0 {
            out.push_str(&format!(
                "\n**{cut} session(s) had a log too large to read whole.** The timeline for \
                 those covers the start of the run and not the end of it.\n"
            ));
        }
        out.push('\n');
    }

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
                md_escape(&crate::redact::sanitize_display(
                    r.cmd.as_deref().unwrap_or("")
                ))
            ));
        }
        if viewer.iter().any(|r| {
            r.cmd
                .as_deref()
                .unwrap_or("")
                .contains("human took control")
        }) {
            out.push_str(
                "\n**A human took control of the browser during this box's life.** \
                 Some of what the agent reports having verified may have been done by hand.\n",
            );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::receipt::{BrowserEvidence, ExecRecord};

    #[test]
    fn a_command_in_a_table_reaches_the_reviewer_without_its_escapes() {
        // Backslash escapes are not processed inside a code span, so escaping
        // the content and then wrapping it in backticks put the backslashes on
        // the reviewer's screen: `echo \(port 3000, 1 peer\(s\), 20s\)`.
        let cell = md_code("echo (port 3000, 1 peer(s), 20s)");
        assert_eq!(cell, "`echo (port 3000, 1 peer(s), 20s)`");
        assert!(!cell.contains('\\'));

        // The pipe is the exception, because GFM splits a row on pipes before
        // it parses anything inline, so that one really does need escaping,
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
            effective_digest: None,
            fs_authority: None,
            isolation_claim: "supervised".into(),
            backend: "worktree".into(),
            created_at: "2026-08-05T00:00:00.000000Z".into(),
            updated_at: "2026-08-05T00:00:00.000000Z".into(),
            status: "proposed".into(),
            captures: Vec::new(),
            service_digest: None,
            pr: None,
            pr_head_ref: None,
        }
    }

    fn summary() -> ExportSummary {
        ExportSummary {
            browser_sessions: Vec::new(),
            env_id: "env/tester/ui".into(),
            dir: PathBuf::from("/out"),
            files_changed: 1,
            insertions: 2,
            deletions: 0,
            patch_bytes: 10,
            receipts: 1,
            egress_denied: 0,
            redactions: Vec::new(),
            fs_overlap: Vec::new(),
        }
    }

    fn record(browser: Option<BrowserEvidence>) -> ExecRecord {
        ExecRecord {
            id: "0123456789abcdef".into(),
            timestamp: "2026-08-05T00:00:01.000000Z".into(),
            env_id: "env/tester/ui".into(),
            policy_digest: None,
            effective_digest: None,
            fs_overlap: Vec::new(),
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
        let text = report(
            &manifest(),
            &summary(),
            &[record(Some(ev))],
            "brief",
            &[],
        );

        assert!(text.contains("## What the browser saw"), "{text}");
        assert!(
            text.contains("TypeError: cannot read 'boom' of null"),
            "{text}"
        );
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
        let text = report(
            &manifest(),
            &summary(),
            &[record(Some(clean))],
            "brief",
            &[],
        );
        assert!(text.contains("no console errors"), "{text}");

        // The distinction that matters: this one was never looked at, and the
        // report must not let it read as a page that came back clean.
        let blind = BrowserEvidence {
            verb: Some("click".into()),
            unavailable: true,
            ..Default::default()
        };
        let text = report(
            &manifest(),
            &summary(),
            &[record(Some(blind))],
            "brief",
            &[],
        );
        assert!(text.contains("no browser available to observe"), "{text}");
    }

    #[test]
    fn a_human_at_the_controls_is_called_out() {
        let mut r = record(None);
        r.source = "viewer".into();
        r.cmd = Some("h5i box view (human took control, 42s)".into());
        let text = report(
            &manifest(),
            &summary(),
            &[r],
            "brief",
            &[],
        );

        assert!(text.contains("## Viewer sessions"), "{text}");
        // The load-bearing sentence: some of what the agent claims to have
        // verified may have been done by hand, and the reviewer has to know.
        assert!(text.contains("A human took control"), "{text}");

        // A session where nobody took over is listed but not flagged.
        let mut watched = record(None);
        watched.source = "viewer".into();
        watched.cmd = Some("h5i box view (agent, 42s)".into());
        let text = report(
            &manifest(),
            &summary(),
            &[watched],
            "brief",
            &[],
        );
        assert!(text.contains("## Viewer sessions"), "{text}");
        assert!(!text.contains("A human took control"), "{text}");
    }

    #[test]
    fn a_run_that_never_touched_a_browser_gets_no_section() {
        // Most boxes are not browser boxes; they should not carry an empty
        // heading implying an inspection that never happened.
        let text = report(
            &manifest(),
            &summary(),
            &[record(None)],
            "brief",
            &[],
        );
        assert!(!text.contains("What the browser saw"), "{text}");
    }
}
