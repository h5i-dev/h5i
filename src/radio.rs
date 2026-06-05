//! Agent Radio — a cinematic full-screen "mission control" view for
//! `h5i msg watch` on an interactive terminal.
//!
//! This is a *passive* dashboard: it never advances any agent's read cursor
//! (dedup is tracked in an in-memory set only) and it is purely additive over
//! the existing watch behaviour. `main.rs` only routes here when stdout is a
//! real TTY and the user did not ask for `--plain` / `--no-tui` / `--once`;
//! every non-interactive path keeps the stable line protocol untouched.
//!
//! Rendering is hand-rolled ANSI (no `crossterm`/`ratatui` dependency): each
//! frame is built as one string and written with a home-cursor + clear-to-EOL
//! "smooth redraw" so the screen updates without flicker. We deliberately do
//! not enter the alternate screen or raw mode — that keeps the terminal in a
//! clean state on `Ctrl+C` with no signal handler, and the final frame stays
//! in scrollback. Exit is `Ctrl+C`; backscroll lives in `h5i msg history`.
//!
//! The look is restrained tactical/SF (NORAD comms net): near-black, phosphor
//! green / amber / red accents, thin rules, stable per-agent colours, and a
//! brief accent pulse on freshly-arrived transmissions — no continuous motion
//! that would delay reading.

use std::collections::HashSet;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use console::{measure_text_width, style, truncate_str, Term};

use crate::msg::{self, Message};
use crate::repository::H5iRepository;

/// Animation frame cadence. Decoupled from the message poll so the clock,
/// LIVE indicator, and arrival accent stay smooth without hammering Git.
const FRAME_MS: u64 = 125;
/// How long a freshly-arrived transmission keeps its bright accent + banner.
const ACCENT_MS: u128 = 700;
/// Rows of fixed chrome around the feed (header, sub, roster rule, roster,
/// feed rule, proof, help).
const CHROME_ROWS: usize = 7;

/// Run the full-screen Agent Radio watch loop. Blocks until `Ctrl+C`.
///
/// `me` is the resolved identity (the conversation is scoped to messages this
/// agent sent, received, or that were broadcast); `None` (or `all`) watches the
/// whole channel. Both directions are shown — this is a conversation view, not
/// just an inbox.
pub fn run_watch(
    workdir: &Path,
    me: Option<String>,
    all: bool,
    interval: u64,
) -> anyhow::Result<()> {
    let me = if all { None } else { me };
    let interval = interval.max(1);
    let term = Term::stdout();

    // In-memory dedup. Everything present at launch is "already seen" so the
    // arrival accent only fires for transmissions that land *after* we start;
    // the initial history is still displayed, just without the banner.
    let mut seen: HashSet<String> = HashSet::new();
    let (mut msgs, mut stats) = fetch(workdir, me.as_deref()).unwrap_or_else(|_| {
        (
            Vec::new(),
            msg::Stats { total: 0, tip: None, tip_time: None },
        )
    });
    for m in &msgs {
        seen.insert(m.id.clone());
    }

    let mut accent_ids: HashSet<String> = HashSet::new();
    let mut accent_at: Option<Instant> = None;

    let start = Instant::now();
    let mut last_poll = Instant::now();
    let mut frame: u64 = 0;

    // One-time full clear so no stale scrollback bleeds into the first frame.
    print!("\x1b[2J\x1b[H");
    let _ = std::io::stdout().flush();

    loop {
        // ── poll Git on the slow cadence ────────────────────────────────────
        if last_poll.elapsed() >= Duration::from_secs(interval) {
            last_poll = Instant::now();
            if let Ok((fresh, fresh_stats)) = fetch(workdir, me.as_deref()) {
                let arrivals: Vec<String> = fresh
                    .iter()
                    .filter(|m| !seen.contains(&m.id))
                    .map(|m| m.id.clone())
                    .collect();
                if !arrivals.is_empty() {
                    for id in &arrivals {
                        seen.insert(id.clone());
                    }
                    accent_ids = arrivals.into_iter().collect();
                    accent_at = Some(Instant::now());
                }
                msgs = fresh;
                stats = fresh_stats;
            }
        }

        // Expire the arrival accent once its window passes.
        if accent_at.map(|t| t.elapsed().as_millis() > ACCENT_MS).unwrap_or(false) {
            accent_at = None;
            accent_ids.clear();
        }

        // ── render one frame ────────────────────────────────────────────────
        let (rows, cols) = term.size();
        let (rows, cols) = (rows as usize, cols.max(1) as usize);
        let accent_on = accent_at.is_some();

        // `view_ids` (the on-screen numbering) is intentionally NOT persisted.
        // This dashboard is strictly passive: it must not touch the shared
        // per-agent read state that `h5i msg reply <n>` and the Stop hook rely
        // on — doing so in another terminal would silently re-point `reply`
        // numbering. Reply / ack from `h5i msg inbox`, whose numbering is the
        // canonical one.
        let (frame_str, _view_ids) = render_frame(
            &msgs,
            &stats,
            me.as_deref(),
            rows,
            cols,
            frame,
            start.elapsed().as_millis(),
            interval,
            &accent_ids,
            accent_on,
        );
        print!("{frame_str}");
        let _ = std::io::stdout().flush();

        frame = frame.wrapping_add(1);
        std::thread::sleep(Duration::from_millis(FRAME_MS));
    }
}

/// Pull the full conversation log plus the channel's Git stats. The returned
/// messages are filtered to the viewer's conversation (sent + received +
/// broadcast) or the whole channel when `me` is `None`, chronological (oldest
/// first) like `history`. `Stats` is always the *global* ledger (real ref tip
/// OID + total) — the PROOF ticker must show genuine Git provenance, never a
/// scoped or message-id-derived stand-in.
fn fetch(workdir: &Path, me: Option<&str>) -> anyhow::Result<(Vec<Message>, msg::Stats)> {
    let repo = H5iRepository::open(workdir)?;
    let stats = msg::stats(repo.git());
    let all = msg::history(repo.git(), None, None, usize::MAX)?;
    let msgs = match me {
        Some(name) => all
            .into_iter()
            .filter(|m| m.from == name || m.to == name || m.to == msg::BROADCAST)
            .collect(),
        None => all,
    };
    Ok((msgs, stats))
}

// ── roster ──────────────────────────────────────────────────────────────────

struct Roster {
    name: String,
    is_self: bool,
    /// Seconds since this agent last *sent* a message, or `None` if never.
    last_send_age: Option<i64>,
}

/// Build the participant roster from the visible log: every distinct sender or
/// recipient (broadcast excluded as a "name"), plus the viewer. Activity is
/// derived from each agent's most recent *sent* message age.
fn build_roster(msgs: &[Message], me: Option<&str>) -> Vec<Roster> {
    use std::collections::BTreeMap;
    let now = chrono::Utc::now().timestamp();
    let mut last_send: BTreeMap<String, i64> = BTreeMap::new();
    let mut names: BTreeMap<String, ()> = BTreeMap::new();

    for m in msgs {
        if m.from != msg::BROADCAST {
            names.insert(m.from.clone(), ());
            if let Some(ts) = parse_ts(&m.ts) {
                let e = last_send.entry(m.from.clone()).or_insert(ts);
                if ts > *e {
                    *e = ts;
                }
            }
        }
        if m.to != msg::BROADCAST {
            names.insert(m.to.clone(), ());
        }
    }
    if let Some(name) = me {
        names.entry(name.to_string()).or_default();
    }

    names
        .into_keys()
        .map(|name| {
            let last_send_age = last_send.get(&name).map(|t| (now - *t).max(0));
            Roster {
                is_self: me == Some(name.as_str()),
                name,
                last_send_age,
            }
        })
        .collect()
}

/// Stable colour index for an agent name (FNV-1a → palette slot).
fn agent_color_idx(name: &str) -> usize {
    let mut h: u32 = 2166136261;
    for b in name.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(16777619);
    }
    (h as usize) % 6
}

/// Paint an agent's display name in its stable palette colour.
fn paint_agent(disp: &str, idx: usize, bold: bool) -> String {
    let s = style(disp.to_string());
    let s = match idx {
        0 => s.green(),
        1 => s.cyan(),
        2 => s.magenta(),
        3 => s.blue(),
        4 => s.yellow(),
        _ => s.red(),
    };
    if bold {
        s.bold().to_string()
    } else {
        s.to_string()
    }
}

/// Parse an RFC3339 timestamp to unix seconds (best-effort).
fn parse_ts(ts: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|t| t.timestamp())
}

/// Compact "14s" / "3m" / "2h" / "5d" age label.
fn age_label(secs: i64) -> String {
    let d = secs.max(0);
    if d < 60 {
        format!("{d}s")
    } else if d < 3600 {
        format!("{}m", d / 60)
    } else if d < 86_400 {
        format!("{}h", d / 3600)
    } else {
        format!("{}d", d / 86_400)
    }
}

// ── frame rendering ──────────────────────────────────────────────────────────

/// Build the complete frame string and the on-screen message numbering (the
/// ids of the messages actually shown, oldest→newest, for `reply <n>`).
#[allow(clippy::too_many_arguments)]
fn render_frame(
    msgs: &[Message],
    stats: &msg::Stats,
    me: Option<&str>,
    rows: usize,
    cols: usize,
    frame: u64,
    elapsed_ms: u128,
    interval: u64,
    accent_ids: &HashSet<String>,
    accent_on: bool,
) -> (String, Vec<String>) {
    let (lines, view_ids) = build_frame_lines(
        msgs, stats, me, rows, cols, frame, elapsed_ms, interval, accent_ids, accent_on,
    );

    // Assemble with a scroll-proof redraw: every line is placed with an
    // ABSOLUTE cursor move (`ESC[row;1H`) and cleared to EOL — we never emit a
    // newline, so the terminal can never scroll (the staircase/append bug).
    // A trailing `ESC[J` wipes anything below the frame (e.g. after a shrink),
    // and the cursor parks at the bottom-left so a `Ctrl+C` prompt lands
    // cleanly. No alt-screen / cursor-hide: exit always leaves a sane terminal.
    let mut out = String::with_capacity(cols * rows + 64);
    for (i, line) in lines.iter().enumerate() {
        out.push_str(&format!("\x1b[{};1H", i + 1));
        out.push_str(line);
        out.push_str("\x1b[K");
    }
    out.push_str("\x1b[J");
    out.push_str(&format!("\x1b[{};1H", rows.max(1)));
    (out, view_ids)
}

/// Build the frame as a vector of styled lines (each already clipped to
/// `cols` visible columns). Split out from `render_frame` so the layout is unit
/// testable without the terminal-control assembly.
#[allow(clippy::too_many_arguments)]
fn build_frame_lines(
    msgs: &[Message],
    stats: &msg::Stats,
    me: Option<&str>,
    rows: usize,
    cols: usize,
    frame: u64,
    elapsed_ms: u128,
    interval: u64,
    accent_ids: &HashSet<String>,
    accent_on: bool,
) -> (Vec<String>, Vec<String>) {
    // Tiny-terminal guard: don't try to lay out bands we can't fit.
    if rows < 8 || cols < 24 {
        return (vec![clip("h5i radio — terminal too small", cols)], Vec::new());
    }

    let blink = (elapsed_ms / 500).is_multiple_of(2); // ~2 Hz LIVE pulse
    let mut lines: Vec<String> = Vec::with_capacity(rows);

    // — header —
    let live = if blink {
        style("◉ LIVE").green().bold().to_string()
    } else {
        style("◉ LIVE").green().dim().to_string()
    };
    let clock = chrono::Utc::now().format("%H:%M:%S").to_string();
    let title = style("H5I AGENT RADIO").cyan().bold().to_string();
    let head = format!(
        "{} {}  {}  {}",
        rule_seg('─', 2),
        title,
        live,
        style(format!("{clock}Z")).dim(),
    );
    lines.push(rule_fill(&head, cols));

    // — sub-header: scope · ref · count · poll —
    let scope = match me {
        Some(name) => paint_agent(name, agent_color_idx(name), true),
        None => style("ALL CHANNELS").yellow().bold().to_string(),
    };
    let sub = format!(
        " {} {}   {} {}   {} {}   {} {}",
        style("NET").dim(),
        scope,
        style("REF").dim(),
        style(msg::MSG_REF).magenta(),
        style("VIEW").dim(),
        style(msgs.len()).bold(),
        style("POLL").dim(),
        style(format!("{interval}s")).dim(),
    );
    lines.push(clip(&sub, cols));

    // — roster band —
    let roster = build_roster(msgs, me);
    lines.push(rule_fill(
        &format!("{} {}", rule_seg('─', 2), style("ROSTER").dim().bold()),
        cols,
    ));
    lines.push(clip(&roster_line(&roster), cols));

    // — feed band —
    let banner = if accent_on {
        let n = accent_ids.len();
        let txt = if (elapsed_ms / 200).is_multiple_of(2) {
            style(format!("◢◤ {n} INCOMING ◢◤")).yellow().bold().to_string()
        } else {
            style(format!("◢◤ {n} INCOMING ◢◤")).red().bold().to_string()
        };
        format!("  {txt}")
    } else {
        String::new()
    };
    lines.push(rule_fill(
        &format!(
            "{} {}{}",
            rule_seg('─', 2),
            style("TRANSMISSIONS").dim().bold(),
            banner
        ),
        cols,
    ));

    // Feed body fills the space between chrome rows; 2 lines per message.
    let feed_rows = rows.saturating_sub(CHROME_ROWS);
    let cap = (feed_rows / 2).max(1);
    let start = msgs.len().saturating_sub(cap);
    let shown = &msgs[start..];
    let mut view_ids = Vec::with_capacity(shown.len());

    if shown.is_empty() {
        let dots = ".".repeat(((elapsed_ms / 400) % 4) as usize);
        lines.push(clip(
            &format!("   {}{}", style("AWAITING TRANSMISSIONS").dim(), style(dots).dim()),
            cols,
        ));
        // pad the rest of the feed area
        while lines.len() < rows.saturating_sub(2) {
            lines.push(String::new());
        }
    } else {
        for (i, m) in shown.iter().enumerate() {
            view_ids.push(m.id.clone());
            let fresh = accent_ids.contains(&m.id);
            let (h, b) = render_message(i + 1, m, me, fresh, accent_on, frame, cols);
            lines.push(h);
            lines.push(b);
        }
        while lines.len() < rows.saturating_sub(2) {
            lines.push(String::new());
        }
    }

    // — proof ticker — genuine Git provenance: the real ref commit OID and the
    // *global* ledger total from `msg::stats`, never a scoped/message-id proxy.
    let tip = stats.tip.as_deref().unwrap_or("none");
    let age = stats
        .tip_time
        .map(|t| format!(" · {} ago", age_label(chrono::Utc::now().timestamp() - t)))
        .unwrap_or_default();
    let proof = format!(
        " {} {}   {} {}   {} {}",
        style("▸ PROOF").green().bold(),
        style(msg::MSG_REF).magenta(),
        style("tip").dim(),
        style(format!("#{tip}")).magenta(),
        style("ledger").dim(),
        style(format!("{} signed · union-merge by id{age}", stats.total)).dim(),
    );
    lines.push(clip(&proof, cols));

    // — help / status strip —
    let scrolled = msgs.len() > shown.len();
    let more = if scrolled {
        style(format!("  · {} earlier in `h5i msg history`", msgs.len() - shown.len()))
            .dim()
            .to_string()
    } else {
        String::new()
    };
    let help = format!(
        " {} {}  {}  {}{}",
        style("●").green(),
        style("auto-follow").dim(),
        style("Ctrl+C exit").dim(),
        style("passive · cursors untouched").dim(),
        more,
    );
    lines.push(clip(&help, cols));

    // Exactly `rows` lines; clip any overflow.
    lines.truncate(rows);
    (lines, view_ids)
}

/// Render one message as (header line, body line).
fn render_message(
    n: usize,
    m: &Message,
    me: Option<&str>,
    fresh: bool,
    accent_on: bool,
    frame: u64,
    cols: usize,
) -> (String, String) {
    // Bold the viewer's own callsign so outgoing transmissions stand out.
    let mine = me == Some(m.from.as_str());
    let from_idx = agent_color_idx(&m.from);
    let from = paint_agent(&msg::sanitize_display(&m.from), from_idx, mine);
    let to = if m.to == msg::BROADCAST {
        style("ALL").yellow().bold().to_string()
    } else {
        paint_agent(&msg::sanitize_display(&m.to), agent_color_idx(&m.to), false)
    };
    // Pulse the arrow on a fresh arrival for a beat of motion.
    let arrow = if fresh && accent_on && frame.is_multiple_of(2) {
        style("━━▶").yellow().bold().to_string()
    } else {
        style("──▶").dim().to_string()
    };

    let marker = if fresh && accent_on {
        style("◢").yellow().bold().to_string()
    } else {
        style(" ").to_string()
    };

    let re = m
        .reply_to
        .as_deref()
        .map(|r| style(format!(" re #{}", short_id(&msg::sanitize_display(r)))).dim().to_string())
        .unwrap_or_default();

    let header = format!(
        "{} {} {}  {} {} {}  {}  {}{}{}",
        marker,
        style(format!("{n:>2}")).bold(),
        style(hhmm(&m.ts)).dim(),
        from,
        arrow,
        to,
        kind_badge(&m.effective_kind()),
        prio_badge(&m.priority),
        style(format!("#{}", short_id(&m.id))).dim(),
        re,
    );

    let body_raw = msg::sanitize_display(&m.body);
    let body_styled = if fresh && accent_on {
        // Bright reveal for the newly-arrived line.
        if frame.is_multiple_of(2) {
            style(body_raw).white().bold().to_string()
        } else {
            style(body_raw).yellow().bold().to_string()
        }
    } else {
        style(body_raw).dim().to_string()
    };
    let bar = if fresh && accent_on {
        style("   ┃ ").yellow().to_string()
    } else {
        "     ".to_string()
    };
    let body = format!("{bar}{body_styled}");

    (clip(&header, cols), clip(&body, cols))
}

/// One-line roster: ` ● claude ACTIVE · ◐ codex IDLE 4m · ○ reviewer 3d`.
fn roster_line(roster: &[Roster]) -> String {
    if roster.is_empty() {
        return format!("   {}", style("no agents yet").dim());
    }
    let parts: Vec<String> = roster
        .iter()
        .map(|r| {
            let idx = agent_color_idx(&r.name);
            let name = paint_agent(&msg::sanitize_display(&r.name), idx, r.is_self);
            let you = if r.is_self {
                style(" (you)").dim().to_string()
            } else {
                String::new()
            };
            let (glyph, status) = match r.last_send_age {
                Some(a) if a < 120 => (style("●").green().to_string(), style("ACTIVE").green().to_string()),
                Some(a) if a < 3600 => (
                    style("◐").yellow().to_string(),
                    style(format!("IDLE {}", age_label(a))).yellow().to_string(),
                ),
                Some(a) => (
                    style("○").dim().to_string(),
                    style(age_label(a)).dim().to_string(),
                ),
                None => (style("○").dim().to_string(), style("—").dim().to_string()),
            };
            format!("{glyph} {name}{you} {status}")
        })
        .collect();
    format!(" {}", parts.join(&style("  ·  ").dim().to_string()))
}

/// Colour an i5h kind label by semantics (NORAD palette).
fn kind_badge(kind: &str) -> String {
    let k = msg::sanitize_display(kind);
    match kind {
        "RISK" | "BLOCKED" | "REVIEW_REQUEST" => style(k).yellow().bold().to_string(),
        "DONE" | "ACK" => style(k).green().bold().to_string(),
        "DECLINE" | "FAILURE" => style(k).red().bold().to_string(),
        "BROADCAST" => style(k).yellow().to_string(),
        _ => style(k).cyan().to_string(),
    }
}

/// Red/amber priority marker (`!!` urgent, `!` high); nothing otherwise.
fn prio_badge(priority: &Option<String>) -> String {
    match priority.as_deref() {
        Some("urgent") => format!("{} ", style("!!").red().bold()),
        Some("high") => format!("{} ", style("!").yellow().bold()),
        _ => String::new(),
    }
}

/// First 8 chars of a 16-hex id for a compact provenance marker.
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// `HH:MM` of an RFC3339 timestamp (falls back to the raw value).
fn hhmm(ts: &str) -> String {
    ts.split('T')
        .nth(1)
        .and_then(|t| t.get(0..5))
        .unwrap_or(ts)
        .to_string()
}

// ── width-safe line primitives ───────────────────────────────────────────────

/// Clip a (possibly styled) line to `width` visible columns, escape-aware.
fn clip(s: &str, width: usize) -> String {
    truncate_str(s, width, "…").to_string()
}

/// A dim run of `n` rule glyphs.
fn rule_seg(ch: char, n: usize) -> String {
    style(ch.to_string().repeat(n)).dim().to_string()
}

/// Take a styled prefix and extend it with a dim `─` rule out to `width`.
fn rule_fill(prefix: &str, width: usize) -> String {
    let used = measure_text_width(prefix);
    if used >= width {
        return clip(prefix, width);
    }
    let pad = width - used - 1;
    format!("{prefix} {}", style("─".repeat(pad)).dim())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: &str, from: &str, to: &str, kind: &str, body: &str) -> Message {
        Message {
            id: id.to_string(),
            ts: "2026-06-01T21:38:33.000000Z".to_string(),
            from: from.to_string(),
            to: to.to_string(),
            body: body.to_string(),
            tag: None,
            version: 1,
            kind: Some(kind.to_string()),
            reply_to: None,
            thread_id: None,
            priority: None,
            status: None,
            branch: None,
            context_branch: None,
            focus: Vec::new(),
            risk: None,
            deadline: None,
            links: None,
            meta: None,
        }
    }

    /// Force `console` to emit ANSI so width-safety is tested against real
    /// escape sequences (CI has no TTY).
    fn forced_color() {
        console::set_colors_enabled(true);
    }

    fn test_stats(total: usize, tip: &str) -> msg::Stats {
        msg::Stats {
            total,
            tip: Some(tip.to_string()),
            tip_time: None,
        }
    }

    /// The styled frame lines (each already clipped to `cols`).
    fn frame(msgs: &[Message], me: Option<&str>, rows: usize, cols: usize) -> Vec<String> {
        let st = test_stats(msgs.len(), "deadbeef");
        let (lines, _) =
            build_frame_lines(msgs, &st, me, rows, cols, 0, 0, 5, &HashSet::new(), false);
        lines
    }

    /// The rendered escape sequence must never contain a newline — newlines are
    /// what made the redraw scroll/staircase on real terminals.
    #[test]
    fn rendered_frame_has_no_newlines() {
        forced_color();
        let msgs = vec![msg("aaaaaaaaaaaaaaaa", "claude", "codex", "ASK", "hello there")];
        let st = test_stats(1, "deadbeef");
        let (s, _) = render_frame(&msgs, &st, Some("claude"), 24, 100, 0, 0, 5, &HashSet::new(), false);
        assert!(!s.contains('\n'), "frame must use absolute positioning, not newlines");
        assert!(s.contains("\x1b[1;1H"), "frame must place lines with absolute cursor moves");
    }

    #[test]
    fn frame_is_width_safe_even_with_styling_and_long_bodies() {
        forced_color();
        let cols = 60;
        let long = "x".repeat(500);
        let msgs = vec![
            msg("aaaaaaaaaaaaaaaa", "claude", "codex", "ASK", &long),
            msg("bbbbbbbbbbbbbbbb", "codex", "all", "BROADCAST", "short"),
        ];
        for line in frame(&msgs, Some("claude"), 24, cols) {
            let visible = measure_text_width(&line);
            assert!(
                visible <= cols,
                "line exceeds {cols} cols (got {visible}): {line:?}"
            );
        }
    }

    #[test]
    fn empty_log_shows_awaiting_banner() {
        forced_color();
        let lines = frame(&[], Some("claude"), 24, 80);
        let joined = lines.join("\n");
        assert!(joined.contains("AWAITING TRANSMISSIONS"), "missing idle banner:\n{joined}");
        assert!(joined.contains("H5I AGENT RADIO"), "missing title:\n{joined}");
    }

    #[test]
    fn view_ids_track_the_visible_tail_oldest_first() {
        forced_color();
        // 50 messages, small terminal → only the newest few fit.
        let msgs: Vec<Message> = (0..50)
            .map(|i| msg(&format!("{i:016x}"), "claude", "codex", "ASK", "hi"))
            .collect();
        let st = test_stats(msgs.len(), "deadbeef");
        let (_, view) = render_frame(&msgs, &st, Some("claude"), 18, 80, 0, 0, 5, &HashSet::new(), false);
        assert!(!view.is_empty() && view.len() < msgs.len(), "should show a bounded tail");
        // Tail must be the newest messages, in chronological order.
        assert_eq!(view.last().unwrap(), &msgs.last().unwrap().id);
        let first_shown = &view[0];
        let idx = msgs.iter().position(|m| &m.id == first_shown).unwrap();
        assert_eq!(view, msgs[idx..].iter().map(|m| m.id.clone()).collect::<Vec<_>>());
    }

    #[test]
    fn tiny_terminal_degrades_without_panicking() {
        forced_color();
        let msgs = vec![msg("aaaaaaaaaaaaaaaa", "a", "b", "ASK", "hi")];
        let st = test_stats(msgs.len(), "deadbeef");
        let (s, view) = render_frame(&msgs, &st, None, 4, 20, 0, 0, 5, &HashSet::new(), false);
        assert!(view.is_empty());
        assert!(s.contains("radio")); // clipped notice still identifies the view
    }

    #[test]
    fn roster_marks_self_and_derives_activity() {
        let msgs = vec![
            msg("aaaaaaaaaaaaaaaa", "claude", "codex", "ASK", "hi"),
            msg("bbbbbbbbbbbbbbbb", "codex", "claude", "DONE", "ok"),
        ];
        let roster = build_roster(&msgs, Some("claude"));
        let me = roster.iter().find(|r| r.name == "claude").unwrap();
        assert!(me.is_self);
        // Both participants present; broadcast pseudo-recipient excluded.
        assert_eq!(roster.len(), 2);
        assert!(roster.iter().all(|r| r.name != msg::BROADCAST));
    }

    #[test]
    fn agent_colors_are_stable() {
        assert_eq!(agent_color_idx("claude"), agent_color_idx("claude"));
    }

    #[test]
    fn proof_uses_global_stats_not_scoped_view_or_message_id() {
        // Regression (Codex review): the PROOF ticker must show genuine Git
        // provenance — the real ref tip OID and the *global* ledger total —
        // not the newest visible message id or the identity-scoped count.
        let scoped = vec![
            msg("aaaaaaaaaaaaaaaa", "claude", "codex", "ASK", "hi"),
            msg("bbbbbbbbbbbbbbbb", "codex", "claude", "DONE", "ok"),
        ];
        // Global ledger is larger (7) and the ref tip is a real commit OID.
        let st = msg::Stats {
            total: 7,
            tip: Some("c0ffee12".to_string()),
            tip_time: None,
        };
        let (lines, _) =
            build_frame_lines(&scoped, &st, Some("claude"), 24, 100, 0, 0, 5, &HashSet::new(), false);
        let plain: Vec<String> = lines.iter().map(|l| console::strip_ansi_codes(l).to_string()).collect();
        // Isolate the PROOF ticker line (message ids legitimately appear in the
        // feed as per-message markers — the invariant is about the PROOF line).
        let proof = plain
            .iter()
            .find(|l| l.contains("PROOF"))
            .expect("a PROOF line");
        let plain = plain.join("\n");
        assert!(proof.contains("7 signed"), "PROOF must show global ledger total: {proof}");
        assert!(proof.contains("#c0ffee12"), "PROOF must show the real ref tip OID: {proof}");
        assert!(
            !proof.contains("#bbbbbbbb"),
            "PROOF must not pass a message id off as the Git ref tip: {proof}"
        );
        // The scoped view count is still surfaced separately (VIEW 2).
        assert!(plain.contains("VIEW 2"), "scoped view count should remain:\n{plain}");
    }
}
