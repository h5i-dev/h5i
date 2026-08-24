//! The three ways this tool says the same thing: to a person, to a program,
//! and to `.h5i/env.toml`.

use crate::advise::{Advice, AllowReach, Destination, Suggestion, sanitize};

/// Longest command shown inline. The point is to recognise which run wanted a
/// destination, not to reproduce it.
const CMD_WIDTH: usize = 64;

pub struct Style {
    pub color: bool,
}

impl Style {
    fn bold(&self, s: &str) -> String {
        if self.color {
            format!("\u{1b}[1m{s}\u{1b}[0m")
        } else {
            s.to_string()
        }
    }
    fn dim(&self, s: &str) -> String {
        if self.color {
            format!("\u{1b}[2m{s}\u{1b}[0m")
        } else {
            s.to_string()
        }
    }
    fn cyan(&self, s: &str) -> String {
        if self.color {
            format!("\u{1b}[36m{s}\u{1b}[0m")
        } else {
            s.to_string()
        }
    }
}

/// The report a human reads.
pub fn text(a: &Advice, style: &Style) -> String {
    let mut out = String::new();
    out.push_str(&style.dim(&format!("receipt: {}", a.origin)));
    out.push('\n');
    out.push_str(&style.dim(&box_line(a)));
    out.push('\n');

    if a.is_empty() {
        out.push('\n');
        out.push_str(&if a.runs_seen == 0 {
            "No egress verdicts in this receipt — nothing here was steered at the network.\n"
                .to_string()
        } else {
            format!(
                "Nothing was refused across {} run(s) with egress verdicts. \
                 The allowlist did not get in the way.\n",
                a.runs_seen
            )
        });
        out.push_str(&warnings(a, style));
        return out;
    }

    out.push_str(&format!(
        "\n{} refused across {} of {} run(s) with egress verdicts, at {} destination(s):\n\n",
        a.total_denied,
        a.runs_refused,
        a.runs_seen,
        a.destinations.len()
    ));

    let width = a
        .destinations
        .iter()
        .map(|d| d.label().chars().count())
        .max()
        .unwrap_or(0)
        .min(48);

    for d in &a.destinations {
        let label = d.label();
        out.push_str(&format!(
            "{:<width$}  {:>4} refused   in {}\n",
            style.bold(&label),
            d.denied,
            runs_phrase(d),
            // The padding has to account for the escape bytes the label carries.
            width = width + (style.bold(&label).len() - label.len())
        ));
        if d.allowed > 0 {
            out.push_str(&style.dim(&format!(
                "    also reached {} time(s) — the same name is already allowed somewhere\n",
                d.allowed
            )));
        }
        if let Some(cmd) = &d.example_cmd {
            out.push_str(&style.dim(&format!("    while running: {}\n", truncate(cmd))));
        }
        match &d.suggestion {
            Suggestion::Allow { rule, why } => {
                out.push_str(&style.dim(&format!("    {why}\n")));
                out.push_str(&format!(
                    "    {}\n",
                    style.cyan(&format!("h5i box allow {rule}"))
                ));
            }
            Suggestion::None { why } => {
                out.push_str(&format!("    no suggestion: {why}\n"));
            }
        }
        out.push('\n');
    }

    out.push_str(&footer(a, style));
    out.push_str(&warnings(a, style));
    out
}

fn box_line(a: &Advice) -> String {
    let mut parts = Vec::new();
    if let Some(env) = &a.env_id {
        parts.push(format!("box {env}"));
    }
    if let Some(p) = &a.profile {
        parts.push(format!("profile {p}"));
    }
    match &a.isolation_claim {
        Some(c) => parts.push(format!("{c} tier")),
        None => parts.push("tier not recorded".into()),
    }
    parts.join(" · ")
}

fn runs_phrase(d: &Destination) -> String {
    let n = d.runs.len();
    let noun = if n == 1 { "run" } else { "runs" };
    if d.runs.is_empty() {
        return "an unrecorded run".into();
    }
    format!("{n} {noun} ({})", d.runs.join(", "))
}

/// What the reader does next, which depends on whether `h5i box allow` reaches
/// this box's tier at all.
fn footer(a: &Advice, style: &Style) -> String {
    let suggested = a
        .destinations
        .iter()
        .filter(|d| matches!(d.suggestion, Suggestion::Allow { .. }))
        .count();
    let mut s = String::new();
    s.push_str(
        &style.dim(
            "Nothing above has been applied: these are lines to read, decide on, and paste.\n",
        ),
    );
    if suggested == 0 {
        return s;
    }
    match a.reach {
        AllowReach::Proxy => s.push_str(&style.dim(
            "`h5i box allow` takes effect at the next run, and only on a profile that already \
             sets `net.egress` — it never widens a deny-all one.\n",
        )),
        AllowReach::Policy => s.push_str(&style.dim(&format!(
            "`h5i box allow` does not reach a {} box: its allowlist is the policy. Re-run with \
             `--toml` for a `.h5i/env.toml` block, then create a box on the edited profile — a \
             box's policy is resolved when it is created.\n",
            a.isolation_claim.as_deref().unwrap_or("kernel-tier")
        ))),
        AllowReach::Unknown => s.push_str(&style.dim(
            "This receipt does not record the isolation tier. `h5i box allow` applies to the \
             proxy tiers (container, microvm); for a supervised or process box, re-run with \
             `--toml` and edit the profile instead.\n",
        )),
    }
    s
}

fn warnings(a: &Advice, style: &Style) -> String {
    if a.warnings.is_empty() {
        return String::new();
    }
    let mut s = String::from("\n");
    for w in &a.warnings {
        s.push_str(&style.dim(&format!("note: {w}\n")));
    }
    s
}

fn truncate(s: &str) -> String {
    if s.chars().count() <= CMD_WIDTH {
        return s.to_string();
    }
    let head: String = s.chars().take(CMD_WIDTH - 1).collect();
    format!("{head}…")
}

/// The same advice for a program to read.
pub fn json(a: &Advice, version: &str) -> String {
    let destinations: Vec<serde_json::Value> = a
        .destinations
        .iter()
        .map(|d| {
            // Both shapes carry every key — a consumer testing
            // `command === null` must not have to tell that apart from a key
            // that was simply left out.
            let suggestion = serde_json::json!({
                "kind": d.suggestion.kind(),
                "rule": d.suggestion.rule(),
                "command": d.suggestion.rule().map(|r| format!("h5i box allow {r}")),
                "why": d.suggestion.why(),
            });
            serde_json::json!({
                "host": d.host,
                "port": d.port,
                "denied": d.denied,
                "allowed": d.allowed,
                "runs": d.runs,
                "first_seen": d.first_seen,
                "last_seen": d.last_seen,
                "example_cmd": d.example_cmd,
                "suggestion": suggestion,
            })
        })
        .collect();
    let doc = serde_json::json!({
        "tool": "h5i-egress-advisor",
        "version": version,
        "schema": 1,
        "source": a.origin,
        "box": {
            "env_id": a.env_id,
            "profile": a.profile,
            "isolation_claim": a.isolation_claim,
            "allow_reach": match a.reach {
                AllowReach::Proxy => "proxy",
                AllowReach::Policy => "policy",
                AllowReach::Unknown => "unknown",
            },
        },
        "totals": {
            "destinations": a.destinations.len(),
            "denied": a.total_denied,
            "runs_with_egress": a.runs_seen,
            "runs_refused": a.runs_refused,
        },
        "destinations": destinations,
        "warnings": a.warnings,
    });
    format!(
        "{}\n",
        serde_json::to_string_pretty(&doc).unwrap_or_default()
    )
}

/// A `[profile.X.net]` block for the boxes `h5i box allow` cannot reach.
///
/// Only the destinations that earned a suggestion go in the array. The rest are
/// listed underneath as comments, so nothing this tool saw disappears just
/// because it declined to recommend it.
pub fn toml(a: &Advice, profile_override: Option<&str>) -> String {
    let profile = profile_override
        .map(str::to_string)
        .or_else(|| a.profile.clone())
        .filter(|p| is_bare_key(p));
    let named = profile.is_some();
    let profile = profile.unwrap_or_else(|| "PROFILE".into());

    let mut s = String::new();
    s.push_str(&format!(
        "# h5i-egress-advisor — candidates from {}\n",
        comment(&a.origin)
    ));
    if a.is_empty() {
        s.push_str("# Nothing was refused. There is nothing to add.\n");
        return s;
    }
    s.push_str(&format!(
        "# {} refusal(s) at {} destination(s), across {} of {} run(s) with egress verdicts.\n",
        a.total_denied,
        a.destinations.len(),
        a.runs_refused,
        a.runs_seen
    ));
    s.push_str(
        "#\n# Every line here widens a boundary. Read it, delete what you do not need, and keep\n\
         # what is left narrow. This block sets `egress` for the profile rather than adding to\n\
         # it: merge it with what the profile already lists. A box's policy is resolved when the\n\
         # box is created, so create a new box after editing.\n",
    );
    if !named {
        s.push_str("#\n# This receipt did not name a usable profile — rename PROFILE below.\n");
    }
    s.push_str(&format!("\n[profile.{profile}.net]\negress = [\n"));
    let mut wrote_any = false;
    for d in &a.destinations {
        if let Suggestion::Allow { rule, .. } = &d.suggestion {
            wrote_any = true;
            s.push_str(&format!(
                "  \"{}\",  # {} refused in {}\n",
                escape(rule),
                d.denied,
                runs_phrase(d)
            ));
        }
    }
    if !wrote_any {
        s.push_str("  # nothing here earned a suggestion — see the notes below\n");
    }
    s.push_str("]\n");

    let declined: Vec<&Destination> = a
        .destinations
        .iter()
        .filter(|d| matches!(d.suggestion, Suggestion::None { .. }))
        .collect();
    if !declined.is_empty() {
        s.push_str("\n# Refused, and deliberately not suggested:\n");
        for d in declined {
            s.push_str(&format!(
                "#   {} — {} refused: {}\n",
                comment(&d.label()),
                d.denied,
                comment(d.suggestion.why())
            ));
        }
    }
    s
}

/// A TOML bare key: what can follow `[profile.` without quoting.
fn is_bare_key(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A basic-string body. Hosts are hostnames in practice; this is here so a
/// receipt that carries something else cannot break out of the quotes into the
/// file a user is about to trust.
fn escape(s: &str) -> String {
    sanitize(s)
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| match c {
            '"' => "\\\"".to_string(),
            '\\' => "\\\\".to_string(),
            c => c.to_string(),
        })
        .collect()
}

/// Comment text, with newlines already removed by [`sanitize`] — a `\n` here
/// would end the comment and turn recorded text into TOML.
fn comment(s: &str) -> String {
    sanitize(s).replace(['\n', '\r'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advise::advise;
    use crate::receipt::{BoxIdentity, EgressHost, EgressSummary, ExecRecord, Receipts};
    use std::path::PathBuf;

    fn advice(claim: Option<&str>, hosts: &[(&str, u16, u64)]) -> Advice {
        let rec = ExecRecord {
            id: "a3f1c2ff".into(),
            timestamp: "2026-08-24T10:00:00Z".into(),
            env_id: "env/claude/fix".into(),
            cmd: Some("npm install".into()),
            egress: Some(EgressSummary {
                denied: hosts.iter().map(|h| h.2).sum(),
                hosts: hosts
                    .iter()
                    .map(|(h, p, d)| EgressHost {
                        host: (*h).into(),
                        port: *p,
                        allowed: 0,
                        denied: *d,
                    })
                    .collect(),
                ..EgressSummary::default()
            }),
        };
        advise(
            &Receipts {
                records: vec![rec],
                identity: BoxIdentity {
                    env_id: Some("env/claude/fix".into()),
                    profile: Some("review".into()),
                    isolation_claim: claim.map(str::to_string),
                },
                origin: PathBuf::from("./review/receipt.json"),
                warnings: Vec::new(),
            },
            1,
        )
    }

    fn plain() -> Style {
        Style { color: false }
    }

    #[test]
    fn the_report_shows_the_command_but_never_runs_it() {
        let a = advice(Some("container"), &[("registry.npmjs.org", 443, 11)]);
        let t = text(&a, &plain());
        assert!(t.contains("registry.npmjs.org:443"));
        assert!(t.contains("11 refused"));
        assert!(t.contains("h5i box allow registry.npmjs.org"));
        assert!(t.contains("Nothing above has been applied"));
    }

    #[test]
    fn a_kernel_tier_box_is_pointed_at_toml_rather_than_box_allow() {
        let t = text(
            &advice(Some("supervised"), &[("x.example.com", 443, 2)]),
            &plain(),
        );
        assert!(t.contains("does not reach a supervised box"), "{t}");
        assert!(t.contains("--toml"));
    }

    #[test]
    fn a_clean_receipt_says_so() {
        let a = advice(Some("container"), &[]);
        let t = text(&a, &plain());
        assert!(t.contains("Nothing was refused"), "{t}");
    }

    #[test]
    fn the_toml_block_is_parseable_and_carries_only_suggestions() {
        let a = advice(
            Some("supervised"),
            &[
                ("registry.npmjs.org", 443, 11),
                ("telemetry.example.net", 443, 1),
            ],
        );
        let out = toml(&a, None);
        assert!(out.contains("[profile.review.net]"));
        assert!(out.contains("\"registry.npmjs.org\","));
        assert!(!out.contains("\"telemetry.example.net\""));
        assert!(out.contains("# Refused, and deliberately not suggested:"));
        // Every line is either a comment or part of the one table we emit.
        let body: Vec<&str> = out
            .lines()
            .filter(|l| !l.trim_start().starts_with('#') && !l.trim().is_empty())
            .collect();
        assert_eq!(body[0], "[profile.review.net]");
        assert_eq!(body[1], "egress = [");
        assert_eq!(body[body.len() - 1], "]");
    }

    #[test]
    fn an_unnamed_profile_becomes_a_placeholder_the_reader_must_edit() {
        let mut a = advice(Some("supervised"), &[("x.example.com", 443, 1)]);
        a.profile = None;
        let out = toml(&a, None);
        assert!(out.contains("[profile.PROFILE.net]"));
        assert!(out.contains("rename PROFILE"));
        assert!(toml(&a, Some("review")).contains("[profile.review.net]"));
    }

    /// A profile name that is not a bare key would produce broken TOML, and a
    /// crafted one could open a second table. It falls back instead.
    #[test]
    fn a_profile_name_that_is_not_a_bare_key_is_refused() {
        let mut a = advice(Some("supervised"), &[("x.example.com", 443, 1)]);
        a.profile = Some("review]\n[profile.other.net".into());
        assert!(toml(&a, None).contains("[profile.PROFILE.net]"));
    }

    #[test]
    fn json_is_stable_enough_to_script_against() {
        let a = advice(Some("container"), &[("registry.npmjs.org", 443, 11)]);
        let v: serde_json::Value = serde_json::from_str(&json(&a, "0.1.0")).unwrap();
        assert_eq!(v["schema"], 1);
        assert_eq!(v["box"]["allow_reach"], "proxy");
        assert_eq!(v["totals"]["denied"], 11);
        let d = &v["destinations"][0];
        assert_eq!(d["host"], "registry.npmjs.org");
        assert_eq!(d["port"], 443);
        assert_eq!(d["runs"][0], "a3f1c2");
        assert_eq!(d["suggestion"]["kind"], "allow");
        assert_eq!(
            d["suggestion"]["command"],
            "h5i box allow registry.npmjs.org"
        );
    }

    #[test]
    fn a_declined_destination_carries_a_null_command_rather_than_a_missing_key() {
        let a = advice(Some("container"), &[("telemetry.example.net", 443, 1)]);
        let v: serde_json::Value = serde_json::from_str(&json(&a, "0.1.0")).unwrap();
        let s = &v["destinations"][0]["suggestion"];
        assert_eq!(s["kind"], "no-suggestion");
        assert!(s["command"].is_null());
        assert!(s["why"].as_str().unwrap().contains("beacon"));
    }
}
