//! Render the recorded orchestration DAG from a run's event log — the
//! define-by-run principle's payoff: the graph is a *view over the journal*,
//! derived after (or during) execution, never a prerequisite for it.

use h5i_core::team::TeamEvent;
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// One-line-per-event text view of a run, orchestration events annotated.
pub fn render_trace(run_id: &str, events: &[TeamEvent]) -> String {
    let steps = events
        .iter()
        .filter(|e| e.kind == "orch_step")
        .count();
    let mut out = format!(
        "orchestra trace — run '{run_id}' ({} events, {steps} journaled steps)\n",
        events.len()
    );
    for ev in events {
        let ts = short_ts(&ev.ts);
        let line = match ev.kind.as_str() {
            "orch_score" => {
                let digest = ev
                    .payload
                    .get("digest")
                    .and_then(|v| v.as_str())
                    .map(|d| format!(" digest {}", &d[..d.len().min(12)]))
                    .unwrap_or_default();
                format!("▶ score start{digest}")
            }
            "orch_step" => {
                let key = payload_str(ev, "key").unwrap_or("?");
                let size = ev
                    .payload
                    .get("result")
                    .map(|r| r.to_string().len())
                    .unwrap_or(0);
                let cost = ev
                    .payload
                    .get("duration_ms")
                    .and_then(|v| v.as_u64())
                    .map(|ms| format!(", {:.1}s", ms as f64 / 1000.0))
                    .unwrap_or_default();
                format!("◆ step {key} ({size}B result{cost})")
            }
            "orch_note" => format!("✎ note: {}", payload_str(ev, "text").unwrap_or("")),
            "orch_gate_asked" => format!(
                "⧖ gate asked → {}: {}",
                payload_str(ev, "to").unwrap_or("?"),
                payload_str(ev, "question").unwrap_or("")
            ),
            "agent_reply" => format!(
                "⤷ data reply from {}",
                payload_str(ev, "agent_id").unwrap_or("?")
            ),
            "materials_granted" => format!(
                "⊞ materials granted to {}",
                payload_str(ev, "worker").unwrap_or("?")
            ),
            "created" => "● run created".to_string(),
            "agent_added" => format!(
                "＋ agent {} → {}",
                payload_str(ev, "agent_id").unwrap_or("?"),
                payload_str(ev, "env_id").unwrap_or("?")
            ),
            "dispatched" => "→ dispatched".to_string(),
            "submitted" => format!(
                "⇑ submitted {} by {}",
                payload_str(ev, "id").unwrap_or("?"),
                payload_str(ev, "owner_agent").unwrap_or("?")
            ),
            "frozen" => "■ round frozen".to_string(),
            "review_granted" => format!(
                "⊙ review granted {} → {}",
                payload_str(ev, "reviewer").unwrap_or("?"),
                payload_str(ev, "target").unwrap_or("?")
            ),
            "review_submitted" => format!(
                "✉ review {} → {}",
                payload_str(ev, "reviewer").unwrap_or("?"),
                payload_str(ev, "target").unwrap_or("?")
            ),
            "verified" => format!(
                "⚖ verified {} (tests_passed={})",
                payload_str(ev, "submission_id").unwrap_or("?"),
                ev.payload
                    .get("tests_passed")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            ),
            "verdict" => format!(
                "★ verdict: {}",
                payload_str(ev, "selected_submission").unwrap_or("(none)")
            ),
            "no_verdict" => "☆ no verdict".to_string(),
            "applied" => format!(
                "✓ applied {}",
                payload_str(ev, "submission_id").unwrap_or("?")
            ),
            other => other.to_string(),
        };
        let phase = match (&ev.phase_before, &ev.phase_after) {
            (Some(b), Some(a)) if b != a => format!("  [{b} → {a}]"),
            _ => String::new(),
        };
        let _ = writeln!(out, "  {ts} {line} ({}){phase}", ev.actor);
    }
    out
}

/// Graphviz dot view: journaled steps chained per label (one cluster per
/// label lane), with the run's phase transitions as a timeline spine.
pub fn render_trace_dot(run_id: &str, events: &[TeamEvent]) -> String {
    let mut lanes: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for ev in events.iter().filter(|e| e.kind == "orch_step") {
        if let (Some(key), Some(label)) = (payload_str(ev, "key"), payload_str(ev, "label")) {
            lanes.entry(label.to_string()).or_default().push(key.to_string());
        }
    }
    let mut phases: Vec<String> = Vec::new();
    for ev in events {
        if let Some(after) = &ev.phase_after {
            let node = format!("{}\\n{after}", ev.kind);
            if phases.last() != Some(&node) {
                phases.push(node);
            }
        }
    }

    let mut out = String::new();
    let _ = writeln!(out, "digraph \"h5i-orchestra-{run_id}\" {{");
    let _ = writeln!(out, "  rankdir=LR; node [shape=box, fontsize=10];");
    for (i, (label, keys)) in lanes.iter().enumerate() {
        let _ = writeln!(out, "  subgraph cluster_{i} {{ label=\"{label}\";");
        for key in keys {
            let _ = writeln!(out, "    \"{key}\";");
        }
        for pair in keys.windows(2) {
            let _ = writeln!(out, "    \"{}\" -> \"{}\";", pair[0], pair[1]);
        }
        let _ = writeln!(out, "  }}");
    }
    if !phases.is_empty() {
        let _ = writeln!(out, "  node [shape=ellipse, style=dashed];");
        for node in &phases {
            let _ = writeln!(out, "  \"{node}\";");
        }
        for pair in phases.windows(2) {
            let _ = writeln!(out, "  \"{}\" -> \"{}\" [style=dashed];", pair[0], pair[1]);
        }
    }
    let _ = writeln!(out, "}}");
    out
}

fn payload_str<'e>(ev: &'e TeamEvent, field: &str) -> Option<&'e str> {
    ev.payload.get(field).and_then(|v| v.as_str())
}

fn short_ts(ts: &str) -> &str {
    // RFC3339 with micros: keep `HH:MM:SS` for scanability.
    ts.get(11..19).unwrap_or(ts)
}
