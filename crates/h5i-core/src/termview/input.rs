//! Terminal input, turned into events a browser understands.

use super::proto::{self, Button, KeyEvent, KeyKind, MouseEvent, MouseKind};

/// The key the viewer keeps for itself: `Ctrl-]` (ASCII GS, 0x1d).
pub const ESCAPE_KEY: u8 = 0x1d;

// ─── what a terminal can tell us ────────────────────────────────────────────

/// A key, before it is given browser spelling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyCode {
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Delete,
    Insert,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Function(u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    pub code: KeyCode,
    /// CDP's modifier bitmask (see [`proto::modifiers`]).
    pub modifiers: u32,
}

/// What the mouse did, in terminal cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Press(Button),
    Release(Button),
    Move,
    Wheel { up: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mouse {
    pub action: MouseAction,
    /// 1-based, as the terminal reports them.
    pub col: u16,
    pub row: u16,
    pub modifiers: u32,
}

/// One thing the human did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Key(Key),
    Mouse(Mouse),
    /// A bracketed paste, delivered whole. Control bytes inside it are text,
    /// never viewer commands, which is the reason bracketed paste is enabled
    /// at all.
    Paste(String),
}

// ─── parsing ────────────────────────────────────────────────────────────────

/// Drain every complete event from the front of `buf`.
///
/// Incomplete sequences are left in place for the next read. `flush` says the
/// input has gone quiet: a lone `ESC` that has stopped growing is the Escape
/// key rather than the start of a sequence, and this is the only way to tell
/// the two apart, which is why the caller passes it after a timeout rather
/// than this module guessing.
pub fn parse(buf: &mut Vec<u8>, flush: bool) -> Vec<Event> {
    let mut events = Vec::new();
    loop {
        match parse_one(buf, flush) {
            Parsed::Event(ev, used) => {
                buf.drain(..used);
                events.push(ev);
            }
            Parsed::Skip(used) => {
                buf.drain(..used);
            }
            Parsed::Incomplete => break,
        }
    }
    events
}

enum Parsed {
    Event(Event, usize),
    /// Consumed bytes that carry no event (an unrecognized sequence).
    Skip(usize),
    Incomplete,
}

fn parse_one(buf: &[u8], flush: bool) -> Parsed {
    let Some(&first) = buf.first() else {
        return Parsed::Incomplete;
    };
    if first != 0x1b {
        return parse_plain(buf);
    }
    if buf.len() == 1 {
        // Either the Escape key or the beginning of a sequence. Only silence
        // distinguishes them.
        return if flush {
            Parsed::Event(Event::Key(plain(KeyCode::Escape)), 1)
        } else {
            Parsed::Incomplete
        };
    }
    match buf[1] {
        b'[' => parse_csi(buf, flush),
        // SS3: what a terminal in application-cursor mode sends for arrows and
        // F1 to F4. Many do, so ignoring it would break the arrow keys on them.
        b'O' => {
            if buf.len() < 3 {
                return Parsed::Incomplete;
            }
            let code = match buf[2] {
                b'A' => KeyCode::Up,
                b'B' => KeyCode::Down,
                b'C' => KeyCode::Right,
                b'D' => KeyCode::Left,
                b'H' => KeyCode::Home,
                b'F' => KeyCode::End,
                b'P' => KeyCode::Function(1),
                b'Q' => KeyCode::Function(2),
                b'R' => KeyCode::Function(3),
                b'S' => KeyCode::Function(4),
                _ => return Parsed::Skip(3),
            };
            Parsed::Event(Event::Key(plain(code)), 3)
        }
        // ESC followed by a key is Alt+key on most terminals.
        _ => match parse_plain(&buf[1..]) {
            Parsed::Event(Event::Key(mut k), used) => {
                k.modifiers |= proto::modifiers::ALT;
                Parsed::Event(Event::Key(k), used + 1)
            }
            Parsed::Event(ev, used) => Parsed::Event(ev, used + 1),
            Parsed::Skip(used) => Parsed::Skip(used + 1),
            Parsed::Incomplete => Parsed::Incomplete,
        },
    }
}

/// A CSI sequence: `ESC [ <params> <final>`.
fn parse_csi(buf: &[u8], flush: bool) -> Parsed {
    // SGR mouse reporting is a CSI sequence with a `<` prefix and a final byte
    // that carries meaning (`M` press, `m` release), so it is handled first.
    if buf.len() >= 3 && buf[2] == b'<' {
        return parse_sgr_mouse(buf);
    }
    // Bracketed paste opens with `ESC [ 2 0 0 ~` and runs to its closing
    // marker. The body may contain anything at all, including escape bytes.
    if buf.starts_with(b"\x1b[200~") {
        return parse_paste(buf, flush);
    }

    let Some(end) = buf.iter().position(|b| (0x40..=0x7e).contains(b) && *b != b'[') else {
        return Parsed::Incomplete;
    };
    let params = &buf[2..end];
    let final_byte = buf[end];
    let used = end + 1;

    // `ESC [ <n> ; <m> <final>`: `n` selects the key, `m - 1` is a modifier
    // bitmask. Either may be absent.
    let mut parts = params.split(|b| *b == b';');
    let n: u16 = parts
        .next()
        .and_then(|p| std::str::from_utf8(p).ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let modifiers = parts
        .next()
        .and_then(|p| std::str::from_utf8(p).ok())
        .and_then(|s| s.parse::<u32>().ok())
        .map(csi_modifiers)
        .unwrap_or(0);

    let code = match final_byte {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        b'Z' => {
            // Shift-Tab has its own final byte and no modifier parameter.
            return Parsed::Event(
                Event::Key(Key {
                    code: KeyCode::Tab,
                    modifiers: proto::modifiers::SHIFT,
                }),
                used,
            );
        }
        b'~' => match n {
            1 | 7 => KeyCode::Home,
            2 => KeyCode::Insert,
            3 => KeyCode::Delete,
            4 | 8 => KeyCode::End,
            5 => KeyCode::PageUp,
            6 => KeyCode::PageDown,
            11..=15 => KeyCode::Function((n - 10) as u8),
            17..=21 => KeyCode::Function((n - 11) as u8),
            23..=26 => KeyCode::Function((n - 12) as u8),
            _ => return Parsed::Skip(used),
        },
        _ => return Parsed::Skip(used),
    };
    Parsed::Event(Event::Key(Key { code, modifiers }), used)
}

/// `ESC [ 2 0 0 ~ <text> ESC [ 2 0 1 ~`
fn parse_paste(buf: &[u8], flush: bool) -> Parsed {
    const OPEN: usize = 6;
    let close = b"\x1b[201~";
    match buf
        .windows(close.len())
        .position(|w| w == close)
        .filter(|at| *at >= OPEN)
    {
        Some(at) => {
            let text = String::from_utf8_lossy(&buf[OPEN..at]).into_owned();
            Parsed::Event(Event::Paste(text), at + close.len())
        }
        // A paste whose end never arrives would otherwise wedge the parser and
        // freeze every later keystroke. On a quiet input stream, take what is
        // there and move on.
        None if flush => {
            let text = String::from_utf8_lossy(&buf[OPEN..]).into_owned();
            Parsed::Event(Event::Paste(text), buf.len())
        }
        None => Parsed::Incomplete,
    }
}

/// `ESC [ < b ; col ; row M|m`
fn parse_sgr_mouse(buf: &[u8]) -> Parsed {
    let Some(end) = buf
        .iter()
        .position(|b| *b == b'M' || *b == b'm')
        .filter(|i| *i >= 3)
    else {
        return Parsed::Incomplete;
    };
    let pressed = buf[end] == b'M';
    let used = end + 1;
    let Ok(text) = std::str::from_utf8(&buf[3..end]) else {
        return Parsed::Skip(used);
    };
    let nums: Vec<u32> = text.split(';').filter_map(|p| p.parse().ok()).collect();
    let [b, col, row] = nums[..] else {
        return Parsed::Skip(used);
    };

    let mut modifiers = 0;
    if b & 4 != 0 {
        modifiers |= proto::modifiers::SHIFT;
    }
    if b & 8 != 0 {
        modifiers |= proto::modifiers::ALT;
    }
    if b & 16 != 0 {
        modifiers |= proto::modifiers::CTRL;
    }

    let action = if b & 64 != 0 {
        // Wheel. The low bits pick the direction; horizontal wheels report 2
        // and 3 and are mapped to vertical rather than dropped, because a
        // trackpad's sideways drift is common and a dropped event is a viewer
        // that feels stuck.
        MouseAction::Wheel { up: b & 3 == 0 }
    } else if b & 32 != 0 {
        MouseAction::Move
    } else {
        let button = match b & 3 {
            0 => Button::Left,
            1 => Button::Middle,
            2 => Button::Right,
            _ => Button::None,
        };
        if pressed {
            MouseAction::Press(button)
        } else {
            MouseAction::Release(button)
        }
    };

    Parsed::Event(
        Event::Mouse(Mouse {
            action,
            col: col.min(u16::MAX as u32) as u16,
            row: row.min(u16::MAX as u32) as u16,
            modifiers,
        }),
        used,
    )
}

/// Bytes with no escape prefix: text, control characters, and `Ctrl-<letter>`.
fn parse_plain(buf: &[u8]) -> Parsed {
    let b = buf[0];
    let key = |code| Parsed::Event(Event::Key(plain(code)), 1);
    match b {
        b'\r' | b'\n' => key(KeyCode::Enter),
        b'\t' => key(KeyCode::Tab),
        0x7f | 0x08 => key(KeyCode::Backspace),
        0x00 => Parsed::Event(
            Event::Key(Key {
                code: KeyCode::Char(' '),
                modifiers: proto::modifiers::CTRL,
            }),
            1,
        ),
        // Ctrl-A through Ctrl-Z, minus the three handled above.
        0x01..=0x1a => Parsed::Event(
            Event::Key(Key {
                code: KeyCode::Char((b'a' + b - 1) as char),
                modifiers: proto::modifiers::CTRL,
            }),
            1,
        ),
        // The rest of the C0 range, `Ctrl-[` through `Ctrl-_`. `Ctrl-[` is ESC
        // and never reaches here.
        0x1c..=0x1f => Parsed::Event(
            Event::Key(Key {
                code: KeyCode::Char((b'\\' + b - 0x1c) as char),
                modifiers: proto::modifiers::CTRL,
            }),
            1,
        ),
        _ => match utf8(buf) {
            Some((c, used)) => Parsed::Event(Event::Key(plain(KeyCode::Char(c))), used),
            None => Parsed::Incomplete,
        },
    }
}

/// Decode one UTF-8 character, or report that it has not all arrived.
///
/// A multi-byte character split across two reads is ordinary, it happens on
/// every paste of non-ASCII text, so a partial sequence must wait rather than
/// be replaced with a replacement character the page would then receive.
fn utf8(buf: &[u8]) -> Option<(char, usize)> {
    let len = match buf[0] {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        // A stray continuation byte is not the start of anything. Consuming it
        // as one character keeps the parser moving instead of wedging on it.
        _ => return Some((char::REPLACEMENT_CHARACTER, 1)),
    };
    if buf.len() < len {
        return None;
    }
    match std::str::from_utf8(&buf[..len]) {
        Ok(s) => s.chars().next().map(|c| (c, len)),
        Err(_) => Some((char::REPLACEMENT_CHARACTER, 1)),
    }
}

fn plain(code: KeyCode) -> Key {
    Key { code, modifiers: 0 }
}

/// `ESC [ 1 ; <m> A` encodes the modifiers as `m - 1`, with its own bit order.
fn csi_modifiers(m: u32) -> u32 {
    let bits = m.saturating_sub(1);
    let mut out = 0;
    if bits & 1 != 0 {
        out |= proto::modifiers::SHIFT;
    }
    if bits & 2 != 0 {
        out |= proto::modifiers::ALT;
    }
    if bits & 4 != 0 {
        out |= proto::modifiers::CTRL;
    }
    if bits & 8 != 0 {
        out |= proto::modifiers::META;
    }
    out
}

// ─── giving it browser spelling ─────────────────────────────────────────────

/// The DOM `key`, DOM `code`, and Windows virtual key code for one key.
///
/// An empty `code` means "this key's physical position is not known", which
/// [`KeyEvent::to_json`] turns into an omitted field rather than a null. That
/// happens for punctuation, where the physical code depends on a keyboard
/// layout the terminal never told us about, and guessing `Semicolon` for `;`
/// on an AZERTY keyboard would put the wrong key in the page's event.
fn spelling(code: &KeyCode) -> (String, String, u32) {
    match code {
        KeyCode::Char(c) => {
            let key = c.to_string();
            let (dom_code, vk) = match c {
                'a'..='z' => (
                    format!("Key{}", c.to_ascii_uppercase()),
                    (*c as u32) - 32,
                ),
                'A'..='Z' => (format!("Key{c}"), *c as u32),
                '0'..='9' => (format!("Digit{c}"), *c as u32),
                ' ' => ("Space".to_string(), 32),
                _ => (String::new(), 0),
            };
            (key, dom_code, vk)
        }
        KeyCode::Enter => ("Enter".into(), "Enter".into(), 13),
        KeyCode::Tab => ("Tab".into(), "Tab".into(), 9),
        KeyCode::Backspace => ("Backspace".into(), "Backspace".into(), 8),
        KeyCode::Escape => ("Escape".into(), "Escape".into(), 27),
        KeyCode::Delete => ("Delete".into(), "Delete".into(), 46),
        KeyCode::Insert => ("Insert".into(), "Insert".into(), 45),
        KeyCode::Up => ("ArrowUp".into(), "ArrowUp".into(), 38),
        KeyCode::Down => ("ArrowDown".into(), "ArrowDown".into(), 40),
        KeyCode::Left => ("ArrowLeft".into(), "ArrowLeft".into(), 37),
        KeyCode::Right => ("ArrowRight".into(), "ArrowRight".into(), 39),
        KeyCode::Home => ("Home".into(), "Home".into(), 36),
        KeyCode::End => ("End".into(), "End".into(), 35),
        KeyCode::PageUp => ("PageUp".into(), "PageUp".into(), 33),
        KeyCode::PageDown => ("PageDown".into(), "PageDown".into(), 34),
        KeyCode::Function(n) => (format!("F{n}"), format!("F{n}"), 111 + *n as u32),
    }
}

/// The text a keystroke inserts, if any.
fn text_for(key: &Key) -> Option<String> {
    // A modified keystroke is a command, not typing: `Ctrl-A` selects all, it
    // does not type "a". Shift is the exception, because shifted characters
    // arrive already shifted from the terminal.
    let commanding = proto::modifiers::CTRL | proto::modifiers::ALT | proto::modifiers::META;
    if key.modifiers & commanding != 0 {
        return None;
    }
    match &key.code {
        KeyCode::Char(c) => Some(c.to_string()),
        KeyCode::Enter => Some("\r".into()),
        KeyCode::Tab => Some("\t".into()),
        _ => None,
    }
}

/// The `keyDown` / `keyUp` pair for one keystroke.
///
/// Always a pair: a page that only ever sees key-downs never fires `keyup`
/// handlers, and a surprising number of forms and editors are wired to those.
pub fn key_events(key: &Key) -> [KeyEvent; 2] {
    let (dom_key, dom_code, vk) = spelling(&key.code);
    let text = text_for(key);
    let down = KeyEvent {
        kind: KeyKind::Down,
        key: dom_key,
        code: dom_code,
        text,
        windows_virtual_key_code: vk,
        modifiers: key.modifiers,
    };
    let up = KeyEvent {
        kind: KeyKind::Up,
        // `text` is dropped on the way up, or the character is typed twice.
        text: None,
        ..down.clone()
    };
    [down, up]
}

/// Pasted text, as the key events that type it.
///
/// Each character is sent as its own keystroke with `text` set, which is what
/// makes a React-controlled input see it. Sending the string as a single event
/// is faster and does not work: the page's per-character handlers never run.
pub fn paste_events(text: &str) -> Vec<KeyEvent> {
    text.chars()
        .flat_map(|c| {
            let code = if c == '\n' || c == '\r' {
                KeyCode::Enter
            } else {
                KeyCode::Char(c)
            };
            key_events(&plain(code))
        })
        .collect()
}

/// Where the image sits on screen, and what it is showing.
///
/// Both halves are needed to place a click: the cell box says which cells the
/// page occupies, the viewport says what those cells are showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mapping {
    /// 1-based top-left cell of the image.
    pub col: u16,
    pub row: u16,
    pub cols: u16,
    pub rows: u16,
    /// The page's own viewport size, in CSS pixels.
    pub viewport_width: u32,
    pub viewport_height: u32,
}

impl Mapping {
    /// Map a terminal cell to a viewport pixel.
    pub fn to_viewport(&self, col: u16, row: u16) -> Option<(i32, i32)> {
        if col < self.col || row < self.row {
            return None;
        }
        let (dx, dy) = (col - self.col, row - self.row);
        if dx >= self.cols || dy >= self.rows {
            return None;
        }
        let x = (dx as u64 * 2 + 1) * self.viewport_width as u64 / (self.cols.max(1) as u64 * 2);
        let y = (dy as u64 * 2 + 1) * self.viewport_height as u64 / (self.rows.max(1) as u64 * 2);
        Some((
            x.min(self.viewport_width.saturating_sub(1) as u64) as i32,
            y.min(self.viewport_height.saturating_sub(1) as u64) as i32,
        ))
    }
}

/// One scroll notch, in CSS pixels. Chrome's own wheel notch is 100px and a
/// terminal reports one event per notch, so this matches a physical wheel.
const WHEEL_NOTCH: f64 = 100.0;

/// The page-bound event for one mouse report, if it lands on the page.
pub fn mouse_event(m: &Mouse, map: &Mapping) -> Option<MouseEvent> {
    let (x, y) = map.to_viewport(m.col, m.row)?;
    let base = MouseEvent {
        kind: MouseKind::Moved,
        x,
        y,
        button: Button::None,
        click_count: 0,
        delta_x: 0.0,
        delta_y: 0.0,
        modifiers: m.modifiers,
    };
    Some(match m.action {
        MouseAction::Press(button) => MouseEvent {
            kind: MouseKind::Pressed,
            button,
            // Chrome treats a press with clickCount 0 as not a click at all, so
            // a button that fires on `click` never fires.
            click_count: 1,
            ..base
        },
        MouseAction::Release(button) => MouseEvent {
            kind: MouseKind::Released,
            button,
            click_count: 1,
            ..base
        },
        MouseAction::Move => base,
        MouseAction::Wheel { up } => MouseEvent {
            kind: MouseKind::Wheel,
            // Negative scrolls the page up, matching a DOM wheel event.
            delta_y: if up { -WHEEL_NOTCH } else { WHEEL_NOTCH },
            ..base
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_all(bytes: &[u8]) -> Vec<Event> {
        let mut buf = bytes.to_vec();
        parse(&mut buf, true)
    }

    fn key(bytes: &[u8]) -> Key {
        match parse_all(bytes).into_iter().next() {
            Some(Event::Key(k)) => k,
            other => panic!("expected a key from {bytes:?}, got {other:?}"),
        }
    }

    #[test]
    fn typing_arrives_as_characters_that_carry_their_text() {
        assert_eq!(key(b"a").code, KeyCode::Char('a'));
        assert_eq!(key(b"Z").code, KeyCode::Char('Z'));
        assert_eq!(key(b" ").code, KeyCode::Char(' '));

        let [down, up] = key_events(&key(b"a"));
        assert_eq!(down.key, "a");
        assert_eq!(down.code, "KeyA");
        assert_eq!(down.text.as_deref(), Some("a"));
        assert_eq!(down.windows_virtual_key_code, 65, "'a' reports VK_A, not 97");
        // The pair, and only the down half types.
        assert_eq!(up.kind, KeyKind::Up);
        assert_eq!(up.text, None);

        // Uppercase reports the same virtual key as lowercase.
        assert_eq!(key_events(&key(b"A"))[0].windows_virtual_key_code, 65);
        assert_eq!(key_events(&key(b"7"))[0].code, "Digit7");
    }

    #[test]
    fn control_keys_command_rather_than_type() {
        let ctrl_a = key(b"\x01");
        assert_eq!(ctrl_a.code, KeyCode::Char('a'));
        assert_eq!(ctrl_a.modifiers, proto::modifiers::CTRL);
        // The whole point: Ctrl-A selects all, it does not type the letter a.
        assert_eq!(key_events(&ctrl_a)[0].text, None);

        // Enter and Tab do type, and their text is what CDP expects.
        assert_eq!(key_events(&key(b"\r"))[0].text.as_deref(), Some("\r"));
        assert_eq!(key_events(&key(b"\t"))[0].text.as_deref(), Some("\t"));
        assert_eq!(key_events(&key(b"\x7f"))[0].text, None);
        assert_eq!(key(b"\x7f").code, KeyCode::Backspace);
    }

    #[test]
    fn the_arrow_keys_work_in_both_encodings_a_terminal_may_use() {
        // Application-cursor mode is not exotic; several terminals use it by
        // default, and ignoring SS3 breaks the arrows on exactly those.
        for seq in [&b"\x1b[A"[..], &b"\x1bOA"[..]] {
            assert_eq!(key(seq).code, KeyCode::Up, "{seq:?}");
        }
        assert_eq!(key(b"\x1b[B").code, KeyCode::Down);
        assert_eq!(key(b"\x1b[C").code, KeyCode::Right);
        assert_eq!(key(b"\x1b[D").code, KeyCode::Left);
        assert_eq!(key(b"\x1b[3~").code, KeyCode::Delete);
        assert_eq!(key(b"\x1b[5~").code, KeyCode::PageUp);
        assert_eq!(key(b"\x1b[6~").code, KeyCode::PageDown);
        assert_eq!(key(b"\x1b[15~").code, KeyCode::Function(5));

        let [down, _] = key_events(&key(b"\x1b[A"));
        assert_eq!(down.key, "ArrowUp");
        assert_eq!(down.windows_virtual_key_code, 38);
        assert_eq!(down.text, None, "an arrow types nothing");
    }

    #[test]
    fn modifiers_survive_the_trip_into_cdps_bitmask() {
        // `ESC [ 1 ; 5 C` is Ctrl-Right: the parameter is the mask plus one,
        // and its bit order is not CDP's.
        let k = key(b"\x1b[1;5C");
        assert_eq!(k.code, KeyCode::Right);
        assert_eq!(k.modifiers, proto::modifiers::CTRL);
        assert_eq!(key(b"\x1b[1;2A").modifiers, proto::modifiers::SHIFT);
        assert_eq!(key(b"\x1b[1;3A").modifiers, proto::modifiers::ALT);
        assert_eq!(
            key(b"\x1b[1;7A").modifiers,
            proto::modifiers::CTRL | proto::modifiers::ALT
        );
        // Shift-Tab has a final byte of its own and no parameter.
        let shift_tab = key(b"\x1b[Z");
        assert_eq!(shift_tab.code, KeyCode::Tab);
        assert_eq!(shift_tab.modifiers, proto::modifiers::SHIFT);
        // ESC-prefixed keys are Alt on most terminals.
        assert_eq!(key(b"\x1bx").modifiers, proto::modifiers::ALT);
    }

    #[test]
    fn a_lone_escape_is_only_the_escape_key_once_the_input_goes_quiet() {
        // The genuinely awkward case: `ESC` is both a key and the start of
        // every sequence, and only silence tells them apart. Deciding too early
        // turns every arrow key into an Escape followed by garbage.
        let mut buf = b"\x1b".to_vec();
        assert!(parse(&mut buf, false).is_empty(), "must wait for more bytes");
        assert_eq!(buf, b"\x1b", "and leave the byte for the next read");

        let events = parse(&mut buf, true);
        assert_eq!(events, vec![Event::Key(plain(KeyCode::Escape))]);
        assert!(buf.is_empty());

        // The rest of the sequence arriving in a later read must still parse.
        let mut split = b"\x1b".to_vec();
        assert!(parse(&mut split, false).is_empty());
        split.extend_from_slice(b"[A");
        assert_eq!(parse(&mut split, false), vec![Event::Key(plain(KeyCode::Up))]);
    }

    #[test]
    fn a_multi_byte_character_split_across_reads_is_not_corrupted() {
        // Ordinary on any paste of non-ASCII text. Emitting a replacement
        // character here would type a wrong character into the page.
        let mut buf = vec![0xe3, 0x81]; // first two bytes of 'あ'
        assert!(parse(&mut buf, false).is_empty());
        buf.push(0x82);
        assert_eq!(parse(&mut buf, false), vec![Event::Key(plain(KeyCode::Char('あ')))]);
        assert!(buf.is_empty());
    }

    #[test]
    fn a_click_reports_its_button_and_a_wheel_reports_its_direction() {
        let m = |bytes: &[u8]| match parse_all(bytes).into_iter().next() {
            Some(Event::Mouse(m)) => m,
            other => panic!("expected a mouse event, got {other:?}"),
        };
        assert_eq!(m(b"\x1b[<0;10;5M").action, MouseAction::Press(Button::Left));
        assert_eq!(m(b"\x1b[<0;10;5m").action, MouseAction::Release(Button::Left));
        assert_eq!(m(b"\x1b[<2;1;1M").action, MouseAction::Press(Button::Right));
        assert_eq!(m(b"\x1b[<1;1;1M").action, MouseAction::Press(Button::Middle));
        assert_eq!(m(b"\x1b[<35;4;9M").action, MouseAction::Move);
        assert_eq!(m(b"\x1b[<64;1;1M").action, MouseAction::Wheel { up: true });
        assert_eq!(m(b"\x1b[<65;1;1M").action, MouseAction::Wheel { up: false });

        let click = m(b"\x1b[<0;10;5M");
        assert_eq!((click.col, click.row), (10, 5));
        // Modifier bits ride on the button byte.
        assert_eq!(m(b"\x1b[<16;1;1M").modifiers, proto::modifiers::CTRL);
        assert_eq!(m(b"\x1b[<4;1;1M").modifiers, proto::modifiers::SHIFT);

        // Coordinates past 223 are exactly why SGR mode is enabled; the legacy
        // encoding cannot express them.
        let far = m(b"\x1b[<0;500;300M");
        assert_eq!((far.col, far.row), (500, 300));
    }

    #[test]
    fn a_partial_mouse_report_waits_instead_of_being_misread() {
        let mut buf = b"\x1b[<0;10".to_vec();
        assert!(parse(&mut buf, false).is_empty());
        buf.extend_from_slice(b";5M");
        assert_eq!(parse(&mut buf, false).len(), 1);
    }

    fn mapping() -> Mapping {
        Mapping {
            col: 1,
            row: 3,
            cols: 80,
            rows: 24,
            viewport_width: 1280,
            viewport_height: 720,
        }
    }

    #[test]
    fn a_click_lands_where_the_human_pointed() {
        let map = mapping();
        // The cell's centre, not its corner: a corner biases every click up and
        // to the left by half a cell, which on a dense form is the difference
        // between the field and its label.
        let (x, y) = map.to_viewport(1, 3).unwrap();
        assert_eq!(x, 1280 / 160, "first cell's centre");
        assert_eq!(y, 720 / 48);

        // The middle of the image lands in the middle of the page.
        let (x, _) = map.to_viewport(41, 3).unwrap();
        assert!((x - 640).abs() <= 1280 / 80, "{x}");

        // The far corner stays inside the viewport rather than one past it.
        let (x, y) = map.to_viewport(80, 26).unwrap();
        assert!(x < 1280 && y < 720, "({x}, {y}) escaped the viewport");
    }

    #[test]
    fn a_click_on_the_status_line_never_reaches_the_page() {
        // The status line is the viewer's, and it is the one part of the screen
        // a page must never be able to receive input from, or to draw on.
        let map = mapping();
        assert_eq!(map.to_viewport(1, 1), None, "the status row");
        assert_eq!(map.to_viewport(1, 2), None, "the separator row");
        assert_eq!(map.to_viewport(81, 3), None, "past the right edge");
        assert_eq!(map.to_viewport(1, 27), None, "below the image");

        // And the event builder refuses it too, not just the mapping.
        let off = Mouse {
            action: MouseAction::Press(Button::Left),
            col: 1,
            row: 1,
            modifiers: 0,
        };
        assert!(mouse_event(&off, &map).is_none());
    }

    #[test]
    fn a_press_carries_a_click_count_or_the_page_never_sees_a_click() {
        let map = mapping();
        let press = mouse_event(
            &Mouse {
                action: MouseAction::Press(Button::Left),
                col: 10,
                row: 10,
                modifiers: 0,
            },
            &map,
        )
        .unwrap();
        assert_eq!(press.kind, MouseKind::Pressed);
        assert_eq!(press.button, Button::Left);
        // Chrome treats clickCount 0 as "not a click", so a button wired to
        // `click` would never fire. This is a silent failure, hence the test.
        assert_eq!(press.click_count, 1);

        let wheel = mouse_event(
            &Mouse {
                action: MouseAction::Wheel { up: true },
                col: 10,
                row: 10,
                modifiers: 0,
            },
            &map,
        )
        .unwrap();
        assert_eq!(wheel.kind, MouseKind::Wheel);
        assert!(wheel.delta_y < 0.0, "scrolling up is a negative delta");
        assert_eq!(wheel.button, Button::None);
    }

    #[test]
    fn a_paste_arrives_whole_and_its_control_bytes_are_text() {
        // Without bracketed paste, a `Ctrl-]` inside pasted text would be read
        // as the viewer's escape key and drop the human out mid-paste.
        let events = parse_all(b"\x1b[200~hi \x1d there\x1b[201~");
        assert_eq!(events, vec![Event::Paste("hi \x1d there".into())]);

        // Typed as individual keystrokes, because a page's per-character
        // handlers are what make a controlled input update.
        let keys = paste_events("ab");
        assert_eq!(keys.len(), 4, "a down and an up for each character");
        assert_eq!(keys[0].text.as_deref(), Some("a"));
        assert_eq!(keys[2].text.as_deref(), Some("b"));

        // A newline in pasted text submits, as it would if typed.
        assert_eq!(paste_events("\n")[0].key, "Enter");
    }

    #[test]
    fn an_unterminated_paste_does_not_wedge_every_later_keystroke() {
        let mut buf = b"\x1b[200~half".to_vec();
        assert!(parse(&mut buf, false).is_empty(), "wait while it may still arrive");
        assert_eq!(parse(&mut buf, true), vec![Event::Paste("half".into())]);
        assert!(buf.is_empty());
    }

    #[test]
    fn the_viewers_own_key_is_a_key_the_parser_reports_rather_than_swallows() {
        // Ctrl-] has to arrive as an ordinary event; the app layer is what
        // reserves it. A parser that dropped it would leave no way out.
        let k = key(&[ESCAPE_KEY]);
        assert_eq!(k.code, KeyCode::Char(']'));
        assert_eq!(k.modifiers, proto::modifiers::CTRL);
    }

    #[test]
    fn an_unrecognized_sequence_is_skipped_rather_than_forwarded_as_noise() {
        // Terminals emit status reports and other replies we did not ask for.
        // Typing them into the page would be worse than dropping them.
        let mut buf = b"\x1b[?62;1c".to_vec();
        let events = parse(&mut buf, true);
        assert!(events.is_empty(), "{events:?}");
        assert!(buf.is_empty(), "and the bytes are consumed, not left to jam");
    }

    #[test]
    fn a_burst_of_input_parses_in_order() {
        // Everything arrives in one read when a human types quickly.
        let events = parse_all(b"h\x1b[<0;5;5Mi\r");
        assert_eq!(
            events,
            vec![
                Event::Key(plain(KeyCode::Char('h'))),
                Event::Mouse(Mouse {
                    action: MouseAction::Press(Button::Left),
                    col: 5,
                    row: 5,
                    modifiers: 0
                }),
                Event::Key(plain(KeyCode::Char('i'))),
                Event::Key(plain(KeyCode::Enter)),
            ]
        );
    }
}
