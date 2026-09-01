//! Developer mode: the page is not the only thing worth looking at.

use super::cells;
use crate::redact::sanitize_display;

/// A rectangle in terminal cells, 1-indexed like the escape sequences that
/// consume it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub row: u16,
    pub col: u16,
    pub cols: u16,
    pub rows: u16,
}

/// Where each part of the screen goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// The rendered page.
    pub page: Rect,
    /// Console errors and page exceptions. `None` when the terminal is too
    /// small to carry a pane without starving the page.
    pub log: Option<Rect>,
}

/// Rows the status line reserves at the top.
pub const CHROME_ROWS: u16 = 2;

/// Below this many rows there is no room for both a page and a log, and a
/// two-line page is worse than no developer mode.
const MIN_ROWS_FOR_LOG: u16 = 16;

/// Fraction of the height the log takes when it is shown.
const LOG_SHARE: u16 = 3;

/// Split the screen.
///
/// Developer mode stacks rather than side-by-side: terminal cells are much
/// taller than wide, so a vertical split costs the page more of what it needs
/// (width) than a horizontal one costs (height), and console lines are long.
pub fn layout(cols: u16, rows: u16, developer: bool) -> Layout {
    let usable = rows.saturating_sub(CHROME_ROWS).max(1);
    let full_page = Rect {
        row: CHROME_ROWS + 1,
        col: 1,
        cols,
        rows: usable,
    };

    if !developer || rows < MIN_ROWS_FOR_LOG {
        return Layout {
            page: full_page,
            log: None,
        };
    }

    let log_rows = (usable / LOG_SHARE).max(3);
    let page_rows = usable.saturating_sub(log_rows).max(1);

    Layout {
        page: Rect {
            row: CHROME_ROWS + 1,
            col: 1,
            cols,
            rows: page_rows,
        },
        log: Some(Rect {
            row: CHROME_ROWS + 1 + page_rows,
            col: 1,
            cols,
            rows: log_rows,
        }),
    }
}

/// One line destined for a pane, with the severity that decides its colour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    pub kind: LogKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogKind {
    /// `console.error` from the page.
    Console,
    /// An uncaught exception.
    PageError,
    /// A note from h5i itself (a refused action, a takeover).
    Note,
}

impl LogKind {
    fn tag(&self) -> &'static str {
        match self {
            LogKind::Console => "console",
            LogKind::PageError => "error  ",
            LogKind::Note => "h5i    ",
        }
    }
}

impl LogLine {
    pub fn console(text: impl Into<String>) -> Self {
        Self {
            kind: LogKind::Console,
            text: text.into(),
        }
    }
    pub fn page_error(text: impl Into<String>) -> Self {
        Self {
            kind: LogKind::PageError,
            text: text.into(),
        }
    }
    pub fn note(text: impl Into<String>) -> Self {
        Self {
            kind: LogKind::Note,
            text: text.into(),
        }
    }
}

/// A bounded log. Keeps the newest lines and forgets the rest, because a page
/// in a failure loop can produce console output faster than anyone reads it
/// and an unbounded buffer in a viewer is a memory leak with a pretty face.
#[derive(Debug, Default)]
pub struct LogBuffer {
    lines: std::collections::VecDeque<LogLine>,
    dropped: usize,
    cap: usize,
}

impl LogBuffer {
    pub fn new(cap: usize) -> Self {
        Self {
            lines: std::collections::VecDeque::new(),
            dropped: 0,
            cap: cap.max(1),
        }
    }

    pub fn push(&mut self, line: LogLine) {
        if self.lines.len() == self.cap {
            self.lines.pop_front();
            self.dropped += 1;
        }
        self.lines.push_back(line);
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn dropped(&self) -> usize {
        self.dropped
    }

    /// The newest `n` lines, oldest first.
    pub fn tail(&self, n: usize) -> Vec<&LogLine> {
        let skip = self.lines.len().saturating_sub(n);
        self.lines.iter().skip(skip).collect()
    }
}

const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const RESET: &str = "\x1b[0m";

/// Render a pane's worth of rows, each exactly `cols` wide.
///
/// Every line is passed through [`sanitize_display`] first: this text comes
/// from the page, which is untrusted input, and a console message carrying
/// escape sequences would otherwise repaint the viewer's own chrome.
pub fn render_pane(log: &LogBuffer, cols: u16, rows: u16) -> Vec<String> {
    let cols = cols.max(8) as usize;
    let rows = rows.max(1) as usize;

    let mut out = Vec::with_capacity(rows);

    // A header, so the pane is not mistaken for page content.
    let dropped = log.dropped();
    let header = if dropped > 0 {
        format!(" console & errors  ({dropped} older line(s) dropped)")
    } else {
        " console & errors".to_string()
    };
    out.push(format!("{DIM}{}{RESET}", pad(&header, cols)));

    let body_rows = rows.saturating_sub(1);
    if log.is_empty() {
        out.push(format!(
            "{DIM}{}{RESET}",
            pad("   (nothing from the page yet)", cols)
        ));
        while out.len() < rows {
            out.push(pad("", cols));
        }
        return out;
    }

    for line in log.tail(body_rows) {
        let colour = match line.kind {
            LogKind::PageError => RED,
            LogKind::Console => YELLOW,
            LogKind::Note => DIM,
        };
        let text = sanitize_display(&line.text);
        let body = format!(" {} {}", line.kind.tag(), text);
        out.push(format!("{colour}{}{RESET}", pad(&body, cols)));
    }

    while out.len() < rows {
        out.push(pad("", cols));
    }
    out
}

/// Truncate or pad to exactly `cols` terminal cells.
///
/// Cells rather than characters, matching `status::render`: a console message
/// is page-written, and a row of double-width glyphs counted as characters is
/// twice as wide as the pane, so it wraps and repaints the viewer's chrome.
fn pad(text: &str, cols: usize) -> String {
    cells::fit(text, cols)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_developer_mode_the_page_gets_everything_below_the_status_line() {
        let l = layout(100, 30, false);
        assert_eq!(l.log, None);
        assert_eq!(l.page.row, CHROME_ROWS + 1);
        assert_eq!(l.page.rows, 30 - CHROME_ROWS);
        assert_eq!(l.page.cols, 100);
    }

    #[test]
    fn developer_mode_splits_without_overlapping_or_leaving_a_gap() {
        let l = layout(100, 30, true);
        let log = l.log.expect("a 30-row terminal has room");
        assert_eq!(
            log.row,
            l.page.row + l.page.rows,
            "the log must start exactly where the page ends"
        );
        assert_eq!(
            l.page.rows + log.rows,
            30 - CHROME_ROWS,
            "together they must fill the screen below the status line"
        );
    }

    #[test]
    fn a_short_terminal_keeps_the_whole_page_rather_than_two_useless_slivers() {
        for rows in [8, 12, 15] {
            let l = layout(80, rows, true);
            assert_eq!(l.log, None, "{rows} rows is too short to split");
            assert_eq!(l.page.rows, rows - CHROME_ROWS);
        }
    }

    #[test]
    fn every_rendered_row_is_exactly_the_pane_width() {
        let mut log = LogBuffer::new(10);
        log.push(LogLine::console("short"));
        log.push(LogLine::page_error(
            "a very long message that will certainly need truncating to fit inside the pane",
        ));

        for cols in [20u16, 40, 100] {
            for row in render_pane(&log, cols, 6) {
                let visible = strip_ansi(&row);
                assert_eq!(
                    visible.chars().count(),
                    cols as usize,
                    "row {visible:?} at width {cols}"
                );
            }
        }
    }

    #[test]
    fn the_pane_always_fills_its_height() {
        let log = LogBuffer::new(10);
        assert_eq!(render_pane(&log, 40, 7).len(), 7);
        let mut full = LogBuffer::new(50);
        for i in 0..50 {
            full.push(LogLine::console(format!("line {i}")));
        }
        assert_eq!(render_pane(&full, 40, 7).len(), 7);
    }

    #[test]
    fn page_text_cannot_repaint_the_viewer() {
        // Console output is untrusted input. Without sanitising, a page could
        // emit escape sequences and rewrite the status line above it.
        let mut log = LogBuffer::new(4);
        log.push(LogLine::console("\x1b[2J\x1b[1;1Hgotcha"));
        let rendered = render_pane(&log, 60, 4).join("");
        assert!(rendered.contains("gotcha"), "the text should survive");
        assert!(
            !rendered.contains("\x1b[2J"),
            "the clear-screen sequence must not: {rendered:?}"
        );
    }

    #[test]
    fn the_buffer_keeps_the_newest_lines_and_says_how_many_it_dropped() {
        let mut log = LogBuffer::new(3);
        for i in 0..6 {
            log.push(LogLine::console(format!("line {i}")));
        }
        let tail: Vec<_> = log.tail(3).iter().map(|l| l.text.clone()).collect();
        assert_eq!(tail, vec!["line 3", "line 4", "line 5"]);
        assert_eq!(log.dropped(), 3);

        let rendered = render_pane(&log, 60, 5).join("");
        assert!(rendered.contains("3 older line(s) dropped"), "{rendered}");
    }

    #[test]
    fn an_empty_log_says_so_rather_than_looking_broken() {
        let log = LogBuffer::new(5);
        let rendered = render_pane(&log, 50, 4).join("");
        assert!(rendered.contains("nothing from the page yet"), "{rendered}");
    }

    #[test]
    fn severities_are_visually_distinct() {
        let mut log = LogBuffer::new(4);
        log.push(LogLine::page_error("boom"));
        log.push(LogLine::console("warned"));
        log.push(LogLine::note("a human took control"));
        let rendered = render_pane(&log, 60, 5).join("");
        assert!(rendered.contains(RED), "page errors should stand out");
        assert!(rendered.contains(YELLOW));
        assert!(rendered.contains("h5i"), "h5i's own notes are labelled");
    }

    fn strip_ansi(input: &str) -> String {
        let mut out = String::new();
        let mut chars = input.chars();
        while let Some(ch) = chars.next() {
            if ch == '\x1b' {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(ch);
            }
        }
        out
    }
}
