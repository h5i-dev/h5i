//! `h5i status` — attention triage in the terminal.
//!
//! Renders the same [`h5i_core::attention::AttentionReport`] the web
//! workbench serves at `/api/attention`, so CLI and web give one answer to
//! "what needs me?". `--json` prints the shared projection verbatim;
//! `--explain <id>` opens one item's evidence (the Why? drawer);
//! `--mark-seen [<id>…]` records this identity's acknowledgement cursor.
//! Acknowledged conditions remain visible until their backing state resolves;
//! the cursor only suppresses new-item counts and notifications.

use crate::*;
use h5i_core::attention::{self, Authority, AttentionItem, Priority};

fn glyph(p: Priority) -> &'static str {
    match p {
        Priority::Critical => "⛔",
        Priority::Decision => "?",
        Priority::Communication => "✉",
        Priority::Active => "●",
        Priority::Info => "·",
    }
}

fn authority_tag(a: Authority) -> &'static str {
    match a {
        Authority::Enforced => "enforced",
        Authority::Verified => "verified",
        Authority::Observed => "observed",
        Authority::Reported => "reported",
        Authority::Inferred => "inferred",
        Authority::Unknown => "unknown",
    }
}

fn render_item(item: &AttentionItem, detail: bool) {
    let seen = if item.seen_at.is_some() { style("(seen) ").dim().to_string() } else { String::new() };
    println!(
        "  {} {}{}  {}",
        glyph(item.priority),
        seen,
        style(&item.title).bold(),
        style(&item.id).dim()
    );
    for reason in item.reasons.iter().take(if detail { usize::MAX } else { 1 }) {
        println!("      {}", reason);
    }
    let tags: Vec<String> = item
        .evidence
        .iter()
        .map(|e| {
            if detail {
                format!(
                    "[{}] {}:{}{}",
                    authority_tag(e.authority),
                    e.kind,
                    e.id,
                    e.note.as_deref().map(|n| format!(" — {n}")).unwrap_or_default()
                )
            } else {
                authority_tag(e.authority).to_string()
            }
        })
        .collect();
    if !tags.is_empty() {
        if detail {
            println!("      {}", style("evidence").underlined());
            for t in &tags {
                println!("        {}", t);
            }
        } else {
            let mut uniq = tags.clone();
            uniq.dedup();
            println!("      {}", style(uniq.join(" · ")).cyan());
        }
    }
    for cmd in item.commands.iter().take(if detail { usize::MAX } else { 1 }) {
        println!("      $ {}", style(cmd).green());
    }
}

pub fn run(
    json: bool,
    explain: Option<String>,
    mark_seen: bool,
    only: Vec<String>,
    identity: Option<String>,
) -> anyhow::Result<()> {
    let repo = H5iRepository::open(".")?;
    let report = attention::report(&repo, identity.as_deref());

    if let Some(id) = explain {
        let Some(item) = attention::find(&report, &id) else {
            anyhow::bail!(
                "no attention item '{id}' — list current ids with `h5i status`"
            );
        };
        println!();
        render_item(item, true);
        println!();
        return Ok(());
    }

    if mark_seen {
        let filter = (!only.is_empty()).then_some(only.as_slice());
        let marked = attention::mark_seen(&repo.h5i_root, &report.identity, &report.items, filter)?;
        println!(
            "{} marked {} item(s) seen for '{}'",
            SUCCESS, marked, report.identity
        );
        return Ok(());
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    // Every item in the live projection is an unresolved condition. Seen is
    // acknowledgement state, not resolution, so it must never remove an item
    // from the default status view.
    let visible: Vec<&AttentionItem> = report.items.iter().collect();
    if visible.is_empty() {
        println!(
            "{} nothing needs you — {} work item(s), all quiet",
            SUCCESS,
            report.work_items.len()
        );
        return Ok(());
    }

    let needs_you: Vec<_> = visible
        .iter()
        .filter(|i| {
            matches!(i.priority, Priority::Critical | Priority::Decision | Priority::Communication)
        })
        .collect();
    let active: Vec<_> = visible.iter().filter(|i| i.priority == Priority::Active).collect();
    let info: Vec<_> = visible.iter().filter(|i| i.priority == Priority::Info).collect();

    println!();
    if !needs_you.is_empty() {
        println!("{}", style(format!("NEEDS YOU ({})", needs_you.len())).red().bold());
        for item in &needs_you {
            render_item(item, false);
        }
        println!();
    }
    if !active.is_empty() {
        println!("{}", style(format!("ACTIVE ({})", active.len())).bold());
        for item in &active {
            render_item(item, false);
        }
        println!();
    }
    if !info.is_empty() {
        println!("{}", style(format!("INFO ({})", info.len())).dim().bold());
        for item in &info {
            render_item(item, false);
        }
        println!();
    }
    println!(
        "{}",
        style(format!(
            "identity {} · explain: h5i status --explain <id> · acknowledge: h5i status --mark-seen",
            report.identity
        ))
        .dim()
    );
    Ok(())
}
