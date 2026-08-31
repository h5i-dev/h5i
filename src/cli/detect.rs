//! `h5i box detect`: the runtime-detection lane, from the outside.
//!
//! Three verbs, each answering a question a reviewer actually asks:
//!
//! * `probe`: *can this machine watch a box at all?* And when it cannot, the
//!   command that would change that. A security feature that reports
//!   "unavailable" and stops is a security feature nobody turns on.
//! * `rules` (*what does it look for?* The catalogue is data, not code paths,
//!   so it can be read without reading Rust) and so the answer to "would it
//!   have caught X" is checkable rather than assumed.
//! * `show`: *what did it see in this box?* Folded across the box's receipts,
//!   worst first.
//!
//! Every one of them is read-only. Nothing here loads a program, attaches
//! anything, or changes a policy.

use clap::Subcommand;
use console::style;

use h5i_core::bpf;
use h5i_core::ui::LOOKING;

#[derive(Subcommand)]
pub enum DetectCommands {
    /// What this host can do for runtime detection, and what to run if it
    /// cannot.
    Probe {
        #[arg(long)]
        json: bool,
    },

    /// The signature catalogue: every rule, its severity, and what it is for.
    Rules {
        /// Only this family (`net`, `secret`, `exec`, `priv`, `kernel`,
        /// `mount`) or this exact rule id.
        #[arg(long, value_name = "FAMILY|ID")]
        filter: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// What the kernel observed in a box, across all of its receipts.
    Show {
        /// Box name or id.
        name: String,
        /// Only detections at this severity or above (`info`, `notice`,
        /// `alert`).
        #[arg(long, default_value = "info")]
        min: String,
        #[arg(long)]
        json: bool,
    },
}

pub fn run(action: DetectCommands) -> anyhow::Result<()> {
    match action {
        DetectCommands::Probe { json } => probe(json),
        DetectCommands::Rules { filter, json } => rules(filter.as_deref(), json),
        DetectCommands::Show { name, min, json } => show(&name, &min, json),
    }
}

fn probe(json: bool) -> anyhow::Result<()> {
    let caps = bpf::probe_host();
    if json {
        println!("{}", serde_json::to_string_pretty(&caps)?);
        return Ok(());
    }
    println!("── Runtime detection (kernel-observed lane) ──");
    println!("  os           = {}", caps.os);
    println!(
        "  kernel       = {}",
        caps.kernel.as_deref().unwrap_or("(unknown)")
    );
    let yn = |b: bool| if b { "yes" } else { "no" };
    println!(
        "  ring buffer  = {}  (needs Linux {}.{}+)",
        yn(caps.ringbuf),
        bpf::probe::MIN_KERNEL.0,
        bpf::probe::MIN_KERNEL.1
    );
    println!("  probe object = {}", yn(caps.object));
    println!("  CAP_BPF      = {}", yn(caps.cap_bpf));
    println!("  CAP_PERFMON  = {}", yn(caps.cap_perfmon));
    // Both are reported and neither is required. They are here because anyone
    // who has used another eBPF tool will ask, and "we do not need it" is a
    // more useful answer than silence.
    println!(
        "  kernel BTF   = {}  {}",
        yn(caps.kernel_btf),
        style("(not required: this probe is CO-RE-free)").dim()
    );
    println!(
        "  tracefs      = {}  {}",
        yn(caps.tracefs),
        style("(not required: it only lets h5i verify tracepoint offsets)").dim()
    );
    println!();
    if caps.usable {
        println!(
            "{} this host can watch a box. Turn it on per profile:",
            style("✓").green()
        );
        println!("      [profile.agent.detect]");
        println!("      enabled = true");
    } else {
        println!(
            "{} {}",
            style("✗").red(),
            caps.detail.as_deref().unwrap_or("unavailable")
        );
        if let Some(fix) = &caps.fix {
            println!("\n  To enable it:\n      {fix}");
            println!(
                "\n  {}",
                style(
                    "h5i does not run that itself: granting a process capabilities is your \
                     decision, not a tool's."
                )
                .dim()
            );
        }
    }
    Ok(())
}

fn rules(filter: Option<&str>, json: bool) -> anyhow::Result<()> {
    let selected: Vec<&bpf::RuleSpec> = bpf::RULES
        .iter()
        .filter(|r| match filter {
            None => true,
            Some(f) => r.family == f || r.id == f,
        })
        .collect();

    if selected.is_empty() {
        anyhow::bail!(
            "no rule or family matches `{}` — families are: {}",
            filter.unwrap_or(""),
            families().join(", ")
        );
    }

    if json {
        let rows: Vec<_> = selected
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id,
                    "family": r.family,
                    "severity": r.severity.as_str(),
                    "title": r.title,
                    "detail": r.detail,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    println!("── Runtime detection signatures ──\n");
    let mut family = "";
    for r in &selected {
        if r.family != family {
            family = r.family;
            println!("{}", style(family).bold());
        }
        println!(
            "  {:<24} {}  {}",
            r.id,
            severity_tag(r.severity),
            style(r.title).dim()
        );
        println!("      {}", wrap(r.detail, 72, "      "));
    }
    println!(
        "\n{}",
        style(
            "Every rule is observation only. Nothing here can deny a syscall — confinement is \
             Landlock, seccomp, the network namespace and the egress proxy, and it stays there."
        )
        .dim()
    );
    Ok(())
}

fn show(name: &str, min: &str, json: bool) -> anyhow::Result<()> {
    let min = parse_severity(min)?;
    let repo = super::discover_repo("box detect show")?;
    let h5i_root = h5i_core::storage::h5i_root_for_repo(&repo)?;
    let m = h5i_core::env::find(&h5i_root, name)?;
    let records = h5i_core::receipt::list(&m.dir(&h5i_root))?;

    let watched: Vec<&h5i_core::receipt::ExecRecord> =
        records.iter().filter(|r| r.runtime.is_some()).collect();

    if json {
        let rows: Vec<_> = watched
            .iter()
            .map(|r| {
                serde_json::json!({
                    "capture": r.id,
                    "timestamp": r.timestamp,
                    "cmd": r.cmd,
                    "runtime": r.runtime,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    if watched.is_empty() {
        println!(
            "{} no receipt in {} carries a runtime block.",
            LOOKING, m.id
        );
        println!(
            "  {}",
            style(
                "Nothing was watched — which is not the same as nothing happening. Enable it \
                 with `[profile.<name>.detect] enabled = true` and check `h5i box detect probe`."
            )
            .dim()
        );
        return Ok(());
    }

    println!("── Runtime detection: {} ──\n", m.id);
    let mut total = 0u64;
    for r in &watched {
        let rt = r.runtime.as_ref().expect("filtered above");
        println!(
            "{}  {}",
            style(&r.timestamp).dim(),
            h5i_core::redact::sanitize_display(r.cmd.as_deref().unwrap_or("(no command)"))
        );
        println!("    {}", h5i_core::redact::sanitize_display(&rt.summary()));
        for d in rt.detections.iter().filter(|d| d.severity >= min) {
            total += d.count;
            println!(
                "    {} {:<24} ×{}  {}",
                severity_tag(d.severity),
                d.rule,
                d.count,
                style(h5i_core::redact::sanitize_display(&d.title)).dim()
            );
            for ex in &d.examples {
                println!("        · {}", h5i_core::redact::sanitize_display(ex));
            }
            if d.examples_truncated {
                println!("        · {}", style("…").dim());
            }
        }
        println!();
    }
    // "recorded" and "watched" are different numbers, and conflating them is
    // the exact confusion this lane exists to remove: a run whose probe never
    // attached is on the log and was not watched.
    let observed = watched
        .iter()
        .filter(|r| r.runtime.as_ref().is_some_and(|rt| rt.observed()))
        .count();
    println!(
        "{} run(s) asked to be watched, {} actually {}, {} matched event(s) at {} or above.",
        watched.len(),
        observed,
        if observed == 1 { "was" } else { "were" },
        total,
        min.as_str()
    );
    // The sentence that keeps the number honest. An empty list means the
    // signatures did not fire, and the signatures are a finite list.
    println!(
        "{}",
        style("`h5i box detect rules` lists what was looked for; behaviour no rule models \
               produces no line here.")
        .dim()
    );
    Ok(())
}

fn families() -> Vec<&'static str> {
    let mut f: Vec<&'static str> = bpf::RULES.iter().map(|r| r.family).collect();
    f.sort_unstable();
    f.dedup();
    f
}

fn parse_severity(s: &str) -> anyhow::Result<bpf::Severity> {
    match s.trim().to_lowercase().as_str() {
        "info" => Ok(bpf::Severity::Info),
        "notice" => Ok(bpf::Severity::Notice),
        "alert" => Ok(bpf::Severity::Alert),
        other => anyhow::bail!("unknown severity `{other}` (expected info, notice or alert)"),
    }
}

fn severity_tag(s: bpf::Severity) -> String {
    match s {
        bpf::Severity::Alert => style("[alert] ").red().bold().to_string(),
        bpf::Severity::Notice => style("[notice]").yellow().to_string(),
        bpf::Severity::Info => style("[info]  ").dim().to_string(),
    }
}

/// Wrap `text` to `width`, indenting continuation lines with `pad`.
fn wrap(text: &str, width: usize, pad: &str) -> String {
    let mut out = String::new();
    let mut col = 0usize;
    for word in text.split_whitespace() {
        if col > 0 && col + 1 + word.len() > width {
            out.push('\n');
            out.push_str(pad);
            col = 0;
        } else if col > 0 {
            out.push(' ');
            col += 1;
        }
        out.push_str(word);
        col += word.len();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severities_parse_and_reject() {
        assert_eq!(parse_severity("alert").unwrap(), bpf::Severity::Alert);
        assert_eq!(parse_severity(" Notice ").unwrap(), bpf::Severity::Notice);
        assert!(parse_severity("critical").is_err());
    }

    #[test]
    fn every_family_is_reachable_by_name() {
        for f in families() {
            assert!(bpf::RULES.iter().any(|r| r.family == f));
        }
        assert!(families().contains(&"net"));
    }

    #[test]
    fn wrapping_keeps_every_word() {
        let text = "a much longer sentence than the width allows, wrapped";
        let wrapped = wrap(text, 12, "  ");
        let flat: Vec<&str> = wrapped.split_whitespace().collect();
        assert_eq!(flat, text.split_whitespace().collect::<Vec<_>>());
        assert!(wrapped.contains('\n'));
    }
}
