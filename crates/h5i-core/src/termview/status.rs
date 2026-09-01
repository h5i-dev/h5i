//! The status line: the one row on the screen the page can never reach.

use crate::control::Holder;
use super::cells;
use crate::redact::sanitize_display;

/// What the human is allowed to do with the page right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Watching. Keystrokes and clicks are the viewer's, not the page's, and
    /// the terminal keeps its own mouse (selection, scrollback).
    View,
    /// Driving with the pointer. Input goes to the page, and only the
    /// control-lock holder's input is sent at all.
    Interact,
    /// The hint overlay is up. Keys narrow the labels.
    Hint,
    /// Typing into a field a hint named. Unlike INTERACT there is one
    /// destination and the viewer knows what it is.
    Insert,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::View => "VIEW",
            Mode::Interact => "INTERACT",
            Mode::Hint => "HINT",
            Mode::Insert => "INSERT",
        }
    }

    /// Whether this mode holds the control lock. Asked of the mode, so a new one
    /// cannot leave it held by forgetting to be listed.
    ///
    /// HINT is included even though an overlay is a read: asking for one is an
    /// intent to act, and the labels describe the page *as it is now*.
    pub fn holds_control(self) -> bool {
        matches!(self, Mode::Interact | Mode::Insert | Mode::Hint)
    }

    /// Whether keystrokes in this mode reach the page. HINT is where this parts
    /// from [`Mode::holds_control`]: it holds the lock and sends nothing.
    pub fn types_into_the_page(self) -> bool {
        matches!(self, Mode::Interact | Mode::Insert)
    }
}

/// Everything the status line reports. All of it is the viewer's own state or
/// host-side state; none of it is taken on the box's word except the URL, which
/// is sanitized on the way in.
#[derive(Debug, Clone)]
pub struct Status {
    /// What is being watched: a box id, or a browser session id.
    pub subject: String,
    pub mode: Mode,
    pub holder: Holder,
    /// The page's current URL, as the box last reported it.
    pub url: Option<String>,
    /// A short account of what the box may reach, from the enforced policy.
    pub egress: String,
    /// Console errors and uncaught exceptions seen this session.
    pub errors: u32,
    /// Whether the box's browser is still streaming.
    pub streaming: bool,
    /// One line of the viewer talking to the human. It gives way only to the
    /// mode and the lock: a refusal that shows on wide terminals only is one
    /// nobody can rely on.
    pub notice: Option<String>,
}

/// SGR: reverse video, so the row reads as chrome rather than as page content.
const BAR_ON: &str = "\x1b[7m";
const BAR_OFF: &str = "\x1b[0m";

/// Render row one, padded to exactly `width` columns.
///
/// Padding to the full width is not cosmetic. Anything shorter leaves cells the
/// previous frame drew, and the whole claim of this row is that nothing from
/// the page is in it.
pub fn render(s: &Status, width: u16) -> String {
    let width = width.max(1) as usize;
    let body = compose(s, width);
    // Measured in cells, not characters: the URL is page-chosen and a
    // double-width glyph would otherwise wrap the row onto the page (see
    // [`super::cells`]).
    format!("{BAR_ON}{}{BAR_OFF}", cells::fit(&body, width))
}

/// One field of the bar, and what it is worth when the row runs out of room.
struct Seg {
    /// What to drop first. `0` is never dropped; higher goes sooner.
    prio: u8,
    text: String,
    /// The original URL, for a field that shortens instead of vanishing.
    raw: Option<String>,
}

/// A URL shorter than this says nothing useful, so below it the field goes
/// entirely rather than shrinking to an ellipsis and two characters.
const MIN_URL: usize = 8;

/// The URL's place in the pecking order. Fields worth less than this are
/// dropped whole before the URL is allowed to shorten at all.
const URL_PRIO: u8 = 2;

/// Lay the fields out, giving up the least useful ones first.
///
/// The order things are surrendered in is a judgement about what this row is
/// *for*. Which session, and where the page is, are useful. Which mode the
/// viewer is in and who holds the control lock are the reason a human looks up
/// here at all, someone about to type a password needs to know whether their
/// keys are going to the page, so those never give way, whatever else has to.
fn compose(s: &Status, width: usize) -> String {
    let mut segs: Vec<Seg> = Vec::new();
    let seg = |prio, text: String| Seg {
        prio,
        text,
        raw: None,
    };

    segs.push(seg(3, format!("h5i:{}", sanitize_display(&s.subject))));
    // Never dropped.
    segs.push(seg(
        0,
        format!("{} control:{}", s.mode.as_str(), s.holder.as_str()),
    ));
    // Worth more than the URL: it answers something the human just did.
    if let Some(notice) = &s.notice {
        segs.push(seg(1, sanitize_display(notice)));
    }
    if let Some(url) = &s.url {
        let clean = sanitize_display(url);
        segs.push(Seg {
            prio: 2,
            text: clean.clone(),
            raw: Some(clean),
        });
    }
    if !s.egress.is_empty() {
        segs.push(seg(4, format!("net:{}", sanitize_display(&s.egress))));
    }
    if s.errors > 0 {
        segs.push(seg(1, format!("err:{}", s.errors)));
    }
    if !s.streaming {
        segs.push(seg(1, "[not streaming]".into()));
    }

    loop {
        let over = joined_len(&segs).saturating_sub(width);
        if over == 0 {
            break;
        }
        let Some(worst) = segs.iter().filter(|x| x.prio > 0).map(|x| x.prio).max() else {
            // Only the fields that never give way are left. `render` truncates
            // as a last resort; a terminal this narrow has no good answer.
            break;
        };
        let position_of = |p: u8, segs: &[Seg]| segs.iter().position(|x| x.prio == p);

        // Anything worth less than knowing where you are goes first, whole.
        if worst > URL_PRIO
            && let Some(i) = position_of(worst, &segs)
        {
            segs.remove(i);
            continue;
        }
        // Only once nothing cheaper is left does the URL start to give, and it
        // shortens rather than vanishing: an elided origin still answers the
        // question, a missing one does not.
        if let Some(i) = segs.iter().position(|x| x.raw.is_some()) {
            let now = cells::width(&segs[i].text);
            if now > MIN_URL {
                let target = now.saturating_sub(over).max(MIN_URL);
                let raw = segs[i].raw.clone().unwrap_or_default();
                segs[i].text = elide_url(&raw, target);
                continue;
            }
        }
        let Some(i) = position_of(worst, &segs) else {
            break;
        };
        segs.remove(i);
    }

    segs.into_iter()
        .map(|x| x.text)
        .collect::<Vec<_>>()
        .join(" ")
}

fn joined_len(segs: &[Seg]) -> usize {
    let text: usize = segs.iter().map(|s| cells::width(&s.text)).sum();
    text + segs.len().saturating_sub(1)
}

/// Shorten a URL to `room` characters, keeping the part that says *where you
/// are*.
pub fn elide_url(url: &str, room: usize) -> String {
    let url = sanitize_display(url);
    // Cells throughout: `room` counts terminal columns, and the string being
    // spent against it came from the page.
    if room == 0 {
        return String::new();
    }
    if cells::width(&url) <= room {
        return url;
    }
    let origin_len = origin_end(&url);
    let origin: String = url.chars().take(origin_len).collect();
    let origin_cells = cells::width(&origin);

    if origin_cells <= room {
        // The origin fits; spend what is left on the path, elided at the end
        // where losing characters is harmless.
        let rest: String = url.chars().skip(origin_len).collect();
        let left = room - origin_cells;
        if left <= 1 {
            return origin;
        }
        return format!("{origin}{}…", cells::head(&rest, left - 1));
    }

    // Even the origin is too long. Truncate from the *left*: the end of a host
    // is the part that identifies it, and a host cut from the right is the
    // spoof this function exists to prevent.
    format!("…{}", cells::tail(&origin, room - 1))
}

/// Character index just past `scheme://host[:port]`.
fn origin_end(url: &str) -> usize {
    let chars: Vec<char> = url.chars().collect();
    let after_scheme = url
        .find("://")
        .map(|b| url[..b + 3].chars().count())
        .unwrap_or(0);
    chars
        .iter()
        .enumerate()
        .skip(after_scheme)
        .find(|(_, c)| **c == '/' || **c == '?' || **c == '#')
        .map(|(i, _)| i)
        .unwrap_or(chars.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> Status {
        Status {
            subject: "env/claude/fix-auth".into(),
            mode: Mode::View,
            holder: Holder::Agent,
            url: Some("http://localhost:3000/login".into()),
            egress: "localhost".into(),
            errors: 0,
            streaming: true,
            notice: None,
        }
    }

    /// The visible text, with the reverse-video wrapper removed.
    fn body(rendered: &str) -> String {
        rendered
            .trim_start_matches(BAR_ON)
            .trim_end_matches(BAR_OFF)
            .to_string()
    }

    #[test]
    fn the_row_is_exactly_as_wide_as_the_terminal() {
        // Anything shorter leaves cells the previous frame drew, and the claim
        // that the page cannot reach this row goes with them.
        for width in [20u16, 40, 80, 200] {
            let out = body(&render(&status(), width));
            assert_eq!(
                out.chars().count(),
                width as usize,
                "width {width} produced {out:?}"
            );
        }
        // Absurdly narrow is still well-formed rather than a panic.
        assert_eq!(body(&render(&status(), 1)).chars().count(), 1);
    }

    #[test]
    fn the_mode_and_the_lock_holder_survive_every_width() {
        // Someone about to type a password looks up here to find out whether
        // their keys are going to the page. That answer is the last thing the
        // row is allowed to drop, however narrow the pane, and a long box id
        // must not be able to push it off the end.
        let mut s = status();
        s.subject = "env/claude/a-very-long-branch-name-for-this-box".into();
        for width in [18u16, 24, 34, 60, 100] {
            let out = body(&render(&s, width));
            assert!(out.contains("VIEW"), "width {width}: {out:?}");
            assert!(out.contains("control:agent"), "width {width}: {out:?}");
        }

        // Roomier panes get the rest back, in order of usefulness.
        let wide = body(&render(&s, 120));
        assert!(wide.contains("h5i:env/claude/a-very-long"), "{wide:?}");
        assert!(wide.contains("localhost:3000"), "{wide:?}");
        assert!(wide.contains("net:localhost"), "{wide:?}");
    }

    #[test]
    fn the_url_shortens_before_anything_is_dropped() {
        // An elided URL still answers "where am I"; a missing one does not.
        // So `net:` goes before the URL does, and the URL shrinks before it
        // goes at all.
        let s = status();
        let mid = body(&render(&s, 52));
        assert!(mid.contains("localhost:3000"), "{mid:?}");
        assert!(!mid.contains("net:"), "net is worth less than the origin: {mid:?}");

        let mut s2 = status();
        s2.mode = Mode::Interact;
        s2.holder = Holder::Human;
        let out = body(&render(&s2, 80));
        assert!(out.contains("INTERACT"), "{out:?}");
        assert!(out.contains("control:human"), "{out:?}");
    }

    #[test]
    fn a_wide_url_cannot_wrap_the_row_onto_the_page() {
        // Characters and cells are not the same count. Eighty CJK codepoints
        // fit an eighty-character row and paint a hundred and sixty cells, so
        // the row wrapped and the page below it was repainted by the page's
        // own choice of URL.
        let mut s = status();
        s.url = Some(format!("http://x/{}", "日".repeat(200)));
        let row = body(&render(&s, 80));
        assert_eq!(cells::width(&row), 80, "{row:?}");
    }

    #[test]
    fn a_hostile_url_cannot_paint_outside_its_own_row() {
        // The URL is page-influenced text on its way to a terminal. An escape
        // sequence here would let a page move the cursor, clear the screen, or
        // rewrite the very row that is supposed to be reporting on it.
        let mut s = status();
        s.url = Some("http://x/\x1b[2J\x1b[1;1Hh5i:trusted control:human".into());
        let out = render(&s, 120);
        assert!(!out.contains('\x1b') || out.starts_with(BAR_ON), "{out:?}");
        let inner = body(&out);
        assert!(!inner.contains('\x1b'), "an escape survived into the bar: {inner:?}");

        // And a bidi override, which needs no escape byte at all to make the
        // origin read as a different host.
        s.url = Some("http://evil.test/\u{202E}gro.knab//:sptth".into());
        assert!(!body(&render(&s, 120)).contains('\u{202E}'));
    }

    #[test]
    fn a_long_url_loses_its_path_and_keeps_its_origin() {
        let url = "https://app.example.com/a/very/long/path/that/goes/on?q=1";
        let out = elide_url(url, 30);
        assert!(out.starts_with("https://app.example.com"), "{out:?}");
        assert!(out.chars().count() <= 30, "{out:?}");
        assert!(out.ends_with('…'), "{out:?}");
    }

    #[test]
    fn an_origin_too_long_to_fit_is_cut_from_the_left() {
        // The attack this rule exists for. Truncating from the right turns
        // `bank.example.evil.test` into `bank.example…`, which reads as the
        // bank. Cutting from the left keeps the end of the host, which is the
        // part that actually identifies it.
        let url = "https://bank.example.evil.test/login";
        let out = elide_url(url, 20);
        assert!(out.chars().count() <= 20, "{out:?}");
        assert!(out.contains("evil.test"), "the real host must survive: {out:?}");
        assert!(!out.contains("https://bank.example."), "{out:?}");
        assert!(out.starts_with('…'), "{out:?}");
    }

    #[test]
    fn a_url_that_fits_is_left_exactly_as_it_is() {
        let url = "http://localhost:3000/";
        assert_eq!(elide_url(url, 40), url);
        assert_eq!(elide_url(url, url.chars().count()), url);
    }

    #[test]
    fn the_origin_is_found_with_or_without_a_path() {
        assert_eq!(&"http://a.b:3000/x"[..origin_end("http://a.b:3000/x")], "http://a.b:3000");
        assert_eq!(&"http://a.b"[..origin_end("http://a.b")], "http://a.b");
        assert_eq!(&"http://a.b?q=1"[..origin_end("http://a.b?q=1")], "http://a.b");
        assert_eq!(&"http://a.b#f"[..origin_end("http://a.b#f")], "http://a.b");
        // about:blank and other scheme-only URLs have no authority at all.
        assert_eq!(&"about:blank"[..origin_end("about:blank")], "about:blank");
    }

    #[test]
    fn a_stalled_stream_and_page_errors_are_reported_rather_than_hidden() {
        // A frozen frame is indistinguishable from a static page, so the row
        // has to say which it is. Otherwise the honest failure mode of this
        // viewer is "everything looks fine".
        let mut s = status();
        s.streaming = false;
        s.errors = 3;
        let out = body(&render(&s, 120));
        assert!(out.contains("not streaming"), "{out:?}");
        assert!(out.contains("err:3"), "{out:?}");
    }

    /// A refusal must survive a narrow row, or nobody can rely on reading it.
    #[test]
    fn what_the_viewer_says_survives_a_narrow_row() {
        let mut s = status();
        s.notice = Some("no label starts with `z`".into());
        let row = render(&s, 60);
        assert!(row.contains("no label starts with"), "{row}");
        assert!(row.contains("VIEW"), "{row}");
    }

    /// And it is page-influenced text on its way to a PTY.
    #[test]
    fn a_notice_carrying_an_escape_sequence_cannot_repaint_the_row() {
        let mut s = status();
        s.notice = Some("link \u{1b}[2Jgone".into());
        let row = render(&s, 120);
        assert!(!row.contains("\u{1b}[2J"), "{row:?}");
    }

    /// Two questions, not one. HINT holds the lock and sends nothing.
    #[test]
    fn holding_the_lock_and_typing_into_the_page_are_different_questions() {
        for mode in [Mode::Interact, Mode::Insert] {
            assert!(mode.holds_control(), "{mode:?}");
            assert!(mode.types_into_the_page(), "{mode:?}");
        }
        assert!(Mode::Hint.holds_control());
        assert!(!Mode::Hint.types_into_the_page());
        assert!(!Mode::View.holds_control());
        assert!(!Mode::View.types_into_the_page());
    }
}
