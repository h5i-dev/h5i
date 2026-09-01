//! `h5i box watch`: the box's policy decisions, one line each, as they happen.

use std::io::Write as _;

use console::style;
use h5i_core::browser_events::{BoxStream, ConsoleLevel, EventKind, Grade, Lane, ViewerEvent};
use h5i_core::ui::SUCCESS;

/// Ring size for the watcher's own copy of the stream.
///
/// The same cap the console uses. It bounds memory for a watcher left running
/// overnight; anything evicted is counted rather than hidden, and the counter
/// is printed when it moves.
const STREAM_CAP: usize = 4000;

/// How often to re-read the box's logs.
///
/// The console polls at 1s from a browser. A terminal watcher is cheaper to
/// serve and closer to the thing being watched, so it polls faster: the point
/// of the surface is that a refusal appears while the person is still looking
/// at the command that caused it.
const POLL: std::time::Duration = std::time::Duration::from_millis(400);

/// Print a line, and do not die if nobody is reading.
///
/// `println!` panics on `EPIPE` because Rust ignores `SIGPIPE`, so
/// `h5i box watch mybox | head -3` would unwind out of the poll loop instead of
/// stopping. The same guard `h5i box share` carries, for the same reason.
macro_rules! say {
    ($($arg:tt)*) => {{
        let _ = writeln!(std::io::stdout(), $($arg)*);
    }};
}

/// Stream one box's decisions until the process is killed.
pub fn run(
    h5i_root: &std::path::Path,
    m: &h5i_core::env::EnvManifest,
    deny_only: bool,
    json: bool,
) -> anyhow::Result<()> {
    if !json {
        header(h5i_root, m, deny_only);
    }

    // Held across iterations on purpose. A `BoxStream` rebuilt per poll re-reads
    // every source from byte zero and renumbers what it has already shown, which
    // is the defect `browser_events` documents on the type itself.
    let mut stream = BoxStream::new(STREAM_CAP);
    let mut cursor = 0u64;
    let mut dropped = 0u64;

    loop {
        stream.poll(h5i_root, m);
        let log = stream.log();

        for event in log.since(cursor) {
            if deny_only && !is_refusal(event) {
                continue;
            }
            if json {
                // The same envelope the console and `--json` consumers already
                // parse, serialised verbatim: three readers, one wire shape.
                match serde_json::to_string(event) {
                    Ok(line) => say!("{line}"),
                    Err(e) => {
                        let _ = writeln!(std::io::stderr(), "h5i box watch: {e}");
                    }
                }
            } else {
                say!("{}", render(event));
            }
        }

        cursor = log.cursor();

        // Counted rather than hidden. A watcher that silently skipped rows
        // would be the one surface here that lies by omission.
        let now_dropped = log.dropped();
        if now_dropped > dropped && !json {
            say!(
                "{}",
                style(format!(
                    "          … {} older rows dropped (ring is {STREAM_CAP})",
                    now_dropped - dropped
                ))
                .dim()
            );
            dropped = now_dropped;
        }
        std::thread::sleep(POLL);
    }
}

/// What is being watched, and what is not.
///
/// Printed before the first row because silence is ambiguous: a box with no
/// browser log and a box whose agent has not browsed yet look identical from
/// the outside, and only one of them will ever produce a line.
fn header(h5i_root: &std::path::Path, m: &h5i_core::env::EnvManifest, deny_only: bool) {
    say!("{} watching {}", SUCCESS, m.id);

    let env_dir = m.dir(h5i_root);
    let mut sources: Vec<&str> = Vec::new();
    if h5i_core::env::browser_request_log(h5i_root, m).is_some() {
        sources.push("the engine's request log (box-claimed, fail-closed)");
    }
    if h5i_core::env::browser_action_log(h5i_root, m).is_some() {
        sources.push("the engine's action log (box-claimed, best-effort)");
    }
    if h5i_core::browser_proxy::actions_log(&env_dir).exists() {
        sources.push("the mediator's actions (host-observed, best-effort)");
    }
    sources.push("receipt evidence (box-claimed, best-effort)");

    for source in &sources {
        say!("{}", style(format!("         + {source}")).dim());
    }
    if sources.len() == 1 {
        // The only source is the post-run receipt drain, so nothing will appear
        // live. Said plainly rather than left to be inferred from an empty
        // screen for the next ten minutes.
        say!(
            "{}",
            style(
                "         this box has no live browser log h5i will read. Only our own \
                 engine writes one; a tier that keeps /tmp inside its image puts it out of \
                 reach; and where a box shares the host's /tmp, h5i declines to attribute an \
                 unqualified world-writable file to it. Rows will appear after a run, not \
                 during one."
            )
            .yellow()
        );
    }
    if deny_only {
        say!("{}", style("         showing refusals only").dim());
    }
    say!("{}", style("         stop     Ctrl-C").dim());
    say!("");
}

/// Whether this row is a policy saying no.
///
/// Three kinds qualify, and a denied request contributes *two* of them: the
/// request row carries the method and URL, the paired `PolicyVerdict` carries
/// the reason. Showing one and not the other would drop half of what a reader
/// needs, so `--deny-only` keeps the pair and the `<- #id` link reads it as one.
fn is_refusal(event: &ViewerEvent) -> bool {
    match &event.kind {
        EventKind::Request { allowed, .. } => !allowed,
        EventKind::PolicyVerdict { .. } => true,
        EventKind::AgentAction { forwarded, .. } => !forwarded,
        _ => false,
    }
}

/// One row.
///
/// Fixed columns on the left so the eye can run down them, free text on the
/// right where a URL's length is not the format's problem. The provenance
/// columns come before the verb rather than after the text, because they
/// qualify everything to their right and a reader who stops scanning early
/// should have already passed them.
fn render(event: &ViewerEvent) -> String {
    let time = clock(&event.observed_at);
    let lane = match event.lane {
        Lane::HostObserved => "host",
        Lane::BoxClaimed => "box ",
    };
    let grade = match event.grade {
        Grade::FailClosed => "fail-closed",
        Grade::BestEffort => "best-effort",
    };

    let (kind, verdict, text) = columns(event);
    let link = event
        .caused_by
        .map(|id| format!("   (<- #{id})"))
        .unwrap_or_default();

    let head = style(format!("{time}  {lane} {grade}  {kind:<8}")).dim();
    let body = format!("{verdict:<6} {text}{link}");

    let body = match &event.kind {
        EventKind::Request { allowed: false, .. } => style(body).red().to_string(),
        EventKind::PolicyVerdict { .. } => style(body).red().to_string(),
        EventKind::AgentAction {
            forwarded: false, ..
        } => style(body).red().to_string(),
        EventKind::Console { level, .. } => match level {
            ConsoleLevel::Error | ConsoleLevel::PageError => style(body).red().to_string(),
            ConsoleLevel::Warning => style(body).yellow().to_string(),
        },
        EventKind::Response { error: Some(_), .. } => style(body).yellow().to_string(),
        EventKind::SessionReset { .. } => style(body).yellow().to_string(),
        _ => body,
    };

    format!("{head}{body}")
}

/// The kind, the verdict and the free text for one event.
///
/// Split out so the colour decision above and the layout here read against the
/// same tuple, and so a new `EventKind` shows up as a missing arm rather than
/// as a row that renders blank.
fn columns(event: &ViewerEvent) -> (&'static str, String, String) {
    match &event.kind {
        EventKind::Navigated { url } => ("nav", String::new(), url.clone()),

        EventKind::Request {
            seq,
            method,
            url,
            initiator,
            allowed,
            denied_reason,
        } => {
            let verdict = if *allowed { "allow" } else { "DENY" };
            let mut text = format!("{method} {url}  #{seq} {}", initiator.as_str());
            if let Some(reason) = denied_reason {
                text.push_str(&format!("  {reason}"));
            }
            (" request", verdict.to_string(), text)
        }

        EventKind::Response {
            seq,
            status,
            bytes,
            duration_ms,
            error,
        } => {
            let verdict = status
                .map(|s| s.to_string())
                .unwrap_or_else(|| "---".to_string());
            let mut parts: Vec<String> = Vec::new();
            if let Some(b) = bytes {
                parts.push(bytes_human(*b));
            }
            if let Some(ms) = duration_ms {
                parts.push(format!("{ms}ms"));
            }
            if let Some(e) = error {
                parts.push(e.clone());
            }
            let text = format!("#{seq} {}", parts.join(", "));
            ("response", verdict, text)
        }

        EventKind::Console { level, text } => {
            ("console", level.as_str().to_string(), text.clone())
        }

        EventKind::AgentAction { action, forwarded } => {
            let verdict = if *forwarded { "" } else { "REFUSED" };
            ("action", verdict.to_string(), action.clone())
        }

        EventKind::PolicyVerdict { subject, reason } => (
            "policy",
            String::new(),
            format!("{subject}: {reason}"),
        ),

        EventKind::SessionReset { source } => (
            "reset",
            String::new(),
            format!("{source} restarted — ids continue, the log did not"),
        ),

        EventKind::Control { holder, note } => (
            "control",
            holder.clone(),
            note.clone().unwrap_or_default(),
        ),

        // The lane's own row. `name` in the verdict column rather than in the
        // text, because a reader scanning an audit for "did anything but the
        // engine touch the network" is scanning that column.
        EventKind::Helper {
            name,
            argv,
            status,
            note,
        } => {
            let verdict = match status {
                Some(0) | None => name.clone(),
                Some(code) => format!("{name} exit {code}"),
            };
            let mut text = argv.join(" ");
            if let Some(note) = note {
                text.push_str(&format!(" — {note}"));
            }
            ("helper", verdict, text)
        }

        EventKind::Lifecycle { state, reason } => (
            "session",
            state.clone(),
            reason.clone().unwrap_or_default(),
        ),
    }
}

/// `HH:MM:SS` out of an RFC3339 stamp, or the whole thing if it is not one.
///
/// The date is dropped rather than abbreviated: a watcher is looking at now,
/// and a reader who needs the date has `--json`, which carries it in full.
fn clock(observed_at: &str) -> String {
    observed_at
        .split('T')
        .nth(1)
        .and_then(|rest| rest.split('.').next())
        .filter(|hms| hms.len() == 8)
        .unwrap_or(observed_at)
        .to_string()
}

/// Bytes, in the shortest form that stays honest.
fn bytes_human(n: u64) -> String {
    const KB: f64 = 1024.0;
    let n_f = n as f64;
    if n < 1024 {
        format!("{n} B")
    } else if n_f < KB * KB {
        format!("{:.1} KB", n_f / KB)
    } else {
        format!("{:.1} MB", n_f / (KB * KB))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use h5i_core::browser_events::Initiator;

    /// Build a `ViewerEvent` without going through an ingest path.
    ///
    /// `Draft` is crate-private to `h5i-core`, so a consumer builds the public
    /// envelope directly, which is also the shape any other out-of-crate
    /// reader would have to use, so the test exercises the real surface.
    fn event(kind: EventKind, lane: Lane, grade: Grade) -> ViewerEvent {
        ViewerEvent {
            id: 7,
            observed_at: "2026-08-19T09:14:02.123456Z".to_string(),
            claimed_at: None,
            lane,
            grade,
            caused_by: None,
            kind,
        }
    }

    fn allowed_request() -> ViewerEvent {
        event(
            EventKind::Request {
                seq: 41,
                method: "GET".to_string(),
                url: "https://docs.rs/blitz/".to_string(),
                initiator: Initiator::Subresource,
                allowed: true,
                denied_reason: None,
            },
            Lane::BoxClaimed,
            Grade::FailClosed,
        )
    }

    fn denied_request() -> ViewerEvent {
        event(
            EventKind::Request {
                seq: 43,
                method: "GET".to_string(),
                url: "https://telemetry.example.com/collect".to_string(),
                initiator: Initiator::Subresource,
                allowed: false,
                denied_reason: Some("not in net.egress".to_string()),
            },
            Lane::BoxClaimed,
            Grade::FailClosed,
        )
    }

    #[test]
    fn every_row_carries_its_lane_and_grade_as_words() {
        // The rule this surface exists under: terse is not licence to drop the
        // qualifier. If a row can be read without learning who observed it and
        // how well, the format is asserting more than h5i knows.
        for e in [
            allowed_request(),
            denied_request(),
            event(
                EventKind::AgentAction {
                    action: "click @e3".to_string(),
                    forwarded: true,
                },
                Lane::HostObserved,
                Grade::BestEffort,
            ),
        ] {
            let row = console::strip_ansi_codes(&render(&e)).to_string();
            let lane = match e.lane {
                Lane::HostObserved => "host",
                Lane::BoxClaimed => "box",
            };
            assert!(row.contains(lane), "lane missing from {row:?}");
            assert!(
                row.contains(e.grade.as_str()),
                "grade missing from {row:?}"
            );
        }
    }

    #[test]
    fn deny_only_keeps_the_pair_that_makes_a_refusal_readable() {
        // The request row has the method and URL; the verdict row has the
        // reason. `--deny-only` keeps both or a reader gets half an answer.
        assert!(is_refusal(&denied_request()));
        assert!(is_refusal(&event(
            EventKind::PolicyVerdict {
                subject: "telemetry.example.com".to_string(),
                reason: "not in net.egress".to_string(),
            },
            Lane::BoxClaimed,
            Grade::FailClosed,
        )));
        assert!(is_refusal(&event(
            EventKind::AgentAction {
                action: "click @e3".to_string(),
                forwarded: false,
            },
            Lane::HostObserved,
            Grade::BestEffort,
        )));

        // And keeps nothing else: an allowed request and a 200 are not
        // refusals, however interesting they are.
        assert!(!is_refusal(&allowed_request()));
        assert!(!is_refusal(&event(
            EventKind::Response {
                seq: 41,
                status: Some(200),
                bytes: Some(12),
                duration_ms: Some(3),
                error: None,
            },
            Lane::BoxClaimed,
            Grade::FailClosed,
        )));
    }

    #[test]
    fn a_denied_request_says_deny_and_names_what_refused_it() {
        let row = console::strip_ansi_codes(&render(&denied_request())).to_string();
        assert!(row.contains("DENY"), "{row:?}");
        assert!(row.contains("telemetry.example.com"), "{row:?}");
        assert!(row.contains("not in net.egress"), "{row:?}");
    }

    #[test]
    fn the_clock_drops_the_date_and_keeps_the_whole_stamp_when_it_cannot() {
        assert_eq!(clock("2026-08-19T09:14:02.123456Z"), "09:14:02");
        // Not a stamp this function understands: hand it back whole rather than
        // print a confidently wrong slice of it.
        assert_eq!(clock("not a timestamp"), "not a timestamp");
        assert_eq!(clock("2026-08-19T09:14Z"), "2026-08-19T09:14Z");
    }

    #[test]
    fn a_caused_by_link_is_rendered_and_absence_is_not_invented() {
        let mut e = denied_request();
        e.caused_by = Some(43);
        assert!(console::strip_ansi_codes(&render(&e)).contains("(<- #43)"));

        e.caused_by = None;
        assert!(!console::strip_ansi_codes(&render(&e)).contains("<-"));
    }

    #[test]
    fn bytes_stay_honest_at_the_boundaries() {
        assert_eq!(bytes_human(0), "0 B");
        assert_eq!(bytes_human(1023), "1023 B");
        assert_eq!(bytes_human(1024), "1.0 KB");
        assert_eq!(bytes_human(1024 * 1024), "1.0 MB");
    }
}
