//! The stream protocol, as agent-browser actually speaks it.
//!
//! Both directions are JSON text frames over one WebSocket. This module is the
//! single place that knows their shape, so the viewer's event loop deals in typed
//! values and a change upstream lands in one file.
//!
//! Two things here are pinned to observed behaviour rather than to a
//! specification, and both matter:
//!
//! * Input messages are `input_mouse` / `input_keyboard` / `input_touch`.
//!   agent-browser's dispatcher matches those three names and falls through to
//!   `_ => {}` for everything else, so a message named after the DOM event
//!   (`mousedown`, `keydown`) is *accepted by the socket and silently
//!   discarded*. There is no error, no log line, and no way for a client to
//!   notice: the connection stays healthy and the page simply never moves. h5i's
//!   own web viewer shipped with exactly that bug (fixed alongside this module),
//!   which is why the mapping is tested here rather than trusted.
//! * An absent string field is omitted, never sent as `null`. Upstream builds its
//!   CDP parameters by copying string fields through only when they parse as
//!   strings, and CDP rejects the whole command when one is null. The caller
//!   there discards the error, so a single null drops the keystroke quietly.
//!   [`KeyEvent::to_json`] omits, and a test holds it to that.

use serde_json::{json, Value};

// ─── modifiers ──────────────────────────────────────────────────────────────

/// CDP's modifier bitmask. Not a guess: these are the values
/// `Input.dispatchKeyEvent` documents, and the dashboard sends the same.
pub mod modifiers {
    pub const ALT: u32 = 1;
    pub const CTRL: u32 = 2;
    pub const META: u32 = 4;
    pub const SHIFT: u32 = 8;
}

// ─── client → box ───────────────────────────────────────────────────────────

/// Which mouse event this is, in CDP's spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Pressed,
    Released,
    Moved,
    Wheel,
}

impl MouseKind {
    fn as_str(self) -> &'static str {
        match self {
            MouseKind::Pressed => "mousePressed",
            MouseKind::Released => "mouseReleased",
            MouseKind::Moved => "mouseMoved",
            MouseKind::Wheel => "mouseWheel",
        }
    }
}

/// Which button, in CDP's spelling. CDP wants a *string* here; a numeric DOM
/// button index is not accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    None,
    Left,
    Middle,
    Right,
}

impl Button {
    fn as_str(self) -> &'static str {
        match self {
            Button::None => "none",
            Button::Left => "left",
            Button::Middle => "middle",
            Button::Right => "right",
        }
    }
}

/// One mouse event bound for the page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MouseEvent {
    pub kind: MouseKind,
    /// Viewport pixels, not terminal cells. The mapping is [`super::input`]'s
    /// job, so nothing downstream has to know the terminal exists.
    pub x: i32,
    pub y: i32,
    pub button: Button,
    pub click_count: u32,
    pub delta_x: f64,
    pub delta_y: f64,
    pub modifiers: u32,
}

impl MouseEvent {
    pub fn to_json(self) -> Value {
        json!({
            "type": "input_mouse",
            "eventType": self.kind.as_str(),
            "x": self.x,
            "y": self.y,
            "button": self.button.as_str(),
            "clickCount": self.click_count,
            "deltaX": self.delta_x,
            "deltaY": self.delta_y,
            "modifiers": self.modifiers,
        })
    }
}

/// Key down or key up. A terminal delivers presses only, so the viewer
/// synthesizes the pair. See [`super::input`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    Down,
    Up,
}

impl KeyKind {
    fn as_str(self) -> &'static str {
        match self {
            KeyKind::Down => "keyDown",
            KeyKind::Up => "keyUp",
        }
    }
}

/// One key event bound for the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEvent {
    pub kind: KeyKind,
    /// DOM `key`: `"a"`, `"Enter"`, `"ArrowLeft"`.
    pub key: String,
    /// DOM `code`: `"KeyA"`, `"Enter"`, `"ArrowLeft"`. Empty means "unknown",
    /// and is omitted rather than sent blank.
    pub code: String,
    /// The character this keystroke inserts, on `keyDown` only. `None` for a
    /// key that types nothing (an arrow, a modifier) and for every `keyUp`:
    /// sending `text` on key-up types the character twice.
    pub text: Option<String>,
    pub windows_virtual_key_code: u32,
    pub modifiers: u32,
}

impl KeyEvent {
    pub fn to_json(&self) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("type".into(), json!("input_keyboard"));
        m.insert("eventType".into(), json!(self.kind.as_str()));
        m.insert("key".into(), json!(self.key));
        // Omitted when unknown. Upstream copies string fields through only when
        // they parse as strings, so a null here is not "no code". It is a
        // rejected CDP command and a lost keystroke.
        if !self.code.is_empty() {
            m.insert("code".into(), json!(self.code));
        }
        if let Some(text) = &self.text {
            m.insert("text".into(), json!(text));
        }
        m.insert(
            "windowsVirtualKeyCode".into(),
            json!(self.windows_virtual_key_code),
        );
        m.insert("modifiers".into(), json!(self.modifiers));
        Value::Object(m)
    }
}

/// Ask the stream for ack pacing at a frame-rate ceiling.
///
/// Ack pacing is the right mode for a terminal, not merely a tuning knob. Under
/// push pacing the box streams frames as fast as it renders them; a terminal that
/// draws more slowly falls steadily behind, because "latest frame wins" only
/// holds *above* the socket, and anything already handed to the transport is
/// delivered in order. Under ack pacing the box keeps at most one frame in
/// flight, so the viewer's own draw rate sets the pace and it can never
/// accumulate a backlog it will have to skip.
pub fn config_ack_pacing(max_fps: u32) -> Value {
    json!({"type": "config", "maxFps": max_fps, "pacing": "ack"})
}

/// Acknowledge a rendered frame, releasing the next one.
pub fn ack(seq: u64) -> Value {
    json!({"type": "ack", "seq": seq})
}

// ─── box → client ───────────────────────────────────────────────────────────

/// What the box said, as far as the viewer cares.
///
/// Unrecognized messages become [`ServerMessage::Other`] rather than an error:
/// the stream carries traffic for a dashboard we are not, and a new message
/// type upstream must not take the viewer down.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerMessage {
    /// A rendered viewport. `data` is base64 JPEG, left encoded because the
    /// decode is the expensive part and the render loop decides when to pay it.
    Frame { seq: Option<u64>, data: String },
    /// The box's own account of the session.
    Status {
        connected: bool,
        screencasting: bool,
        viewport_width: u32,
        viewport_height: u32,
        engine: Option<String>,
    },
    /// The active tab navigated.
    Url(String),
    /// The tab list changed; carries the active tab's URL when there is one.
    Tabs { active_url: Option<String> },
    /// A console entry, kept only when it is bad news (see [`parse`]).
    ConsoleError(String),
    /// An uncaught exception in the page.
    PageError(String),
    /// Anything else on the stream.
    Other,
}

/// Parse one server text frame.
///
/// `None` only for text that is not JSON at all. Everything else resolves to a
/// variant, including messages this viewer has no use for.
pub fn parse(text: &str) -> Option<ServerMessage> {
    let v: Value = serde_json::from_str(text).ok()?;
    let ty = v.get("type")?.as_str()?;
    Some(match ty {
        "frame" => ServerMessage::Frame {
            seq: v.get("seq").and_then(Value::as_u64),
            data: v.get("data").and_then(Value::as_str).unwrap_or("").to_string(),
        },
        "status" => ServerMessage::Status {
            connected: v.get("connected").and_then(Value::as_bool).unwrap_or(false),
            screencasting: v
                .get("screencasting")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            // The defaults match the stream server's own, so a status message
            // that omits them does not resize the viewer to nothing.
            viewport_width: v
                .get("viewportWidth")
                .and_then(Value::as_u64)
                .unwrap_or(1280) as u32,
            viewport_height: v
                .get("viewportHeight")
                .and_then(Value::as_u64)
                .unwrap_or(720) as u32,
            engine: v
                .get("engine")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        },
        "url" => ServerMessage::Url(
            v.get("url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        ),
        "tabs" => ServerMessage::Tabs {
            active_url: v
                .get("tabs")
                .and_then(Value::as_array)
                .and_then(|tabs| {
                    tabs.iter()
                        .find(|t| t.get("active").and_then(Value::as_bool).unwrap_or(false))
                })
                .and_then(|t| t.get("url").and_then(Value::as_str))
                .map(str::to_string),
        },
        // Only errors are kept. A viewer that surfaced every `console.log`
        // would be a log tail with a picture attached; what earns space in a
        // status line is the thing that means the page is broken, and it is
        // the same signal `crate::browser` already puts in the receipt.
        "console" => {
            let level = v.get("level").and_then(Value::as_str).unwrap_or("");
            if level == "error" {
                ServerMessage::ConsoleError(
                    v.get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                )
            } else {
                ServerMessage::Other
            }
        }
        "page_error" => ServerMessage::PageError(
            v.get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        ),
        _ => ServerMessage::Other,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_messages_carry_the_names_the_box_actually_dispatches() {
        // The whole point of this test. agent-browser matches these three
        // strings and silently drops everything else, so a rename upstream,
        // or a well-meaning "use the DOM event name" here, produces a viewer
        // that connects, streams, accepts clicks and moves nothing.
        let m = MouseEvent {
            kind: MouseKind::Pressed,
            x: 10,
            y: 20,
            button: Button::Left,
            click_count: 1,
            delta_x: 0.0,
            delta_y: 0.0,
            modifiers: 0,
        }
        .to_json();
        assert_eq!(m["type"], "input_mouse");
        assert_eq!(m["eventType"], "mousePressed");
        // A *string* button. CDP ignores a numeric one, which is how a click
        // becomes a no-op that looks like a working connection.
        assert_eq!(m["button"], "left");

        let k = KeyEvent {
            kind: KeyKind::Down,
            key: "a".into(),
            code: "KeyA".into(),
            text: Some("a".into()),
            windows_virtual_key_code: 65,
            modifiers: 0,
        }
        .to_json();
        assert_eq!(k["type"], "input_keyboard");
        assert_eq!(k["eventType"], "keyDown");
    }

    #[test]
    fn an_unknown_string_field_is_omitted_rather_than_nulled() {
        // CDP rejects the command outright on a null string and upstream
        // discards the error, so a null `code` does not degrade the event. It
        // deletes it.
        let k = KeyEvent {
            kind: KeyKind::Up,
            key: "ArrowLeft".into(),
            code: String::new(),
            text: None,
            windows_virtual_key_code: 37,
            modifiers: 0,
        }
        .to_json();
        let obj = k.as_object().unwrap();
        assert!(!obj.contains_key("code"), "{k}");
        assert!(!obj.contains_key("text"), "{k}");
        assert!(!k.to_string().contains("null"), "{k}");
        // And what must be there, is.
        assert_eq!(k["key"], "ArrowLeft");
        assert_eq!(k["windowsVirtualKeyCode"], 37);
    }

    #[test]
    fn text_rides_on_key_down_only() {
        // `text` on key-up types the character a second time.
        let down = KeyEvent {
            kind: KeyKind::Down,
            key: "x".into(),
            code: "KeyX".into(),
            text: Some("x".into()),
            windows_virtual_key_code: 88,
            modifiers: 0,
        };
        let mut up = down.clone();
        up.kind = KeyKind::Up;
        up.text = None;
        assert_eq!(down.to_json()["text"], "x");
        assert!(!up.to_json().as_object().unwrap().contains_key("text"));
    }

    #[test]
    fn frames_and_status_parse_into_what_the_render_loop_needs() {
        let f = parse(r#"{"type":"frame","seq":7,"data":"AAAA"}"#).unwrap();
        assert_eq!(
            f,
            ServerMessage::Frame {
                seq: Some(7),
                data: "AAAA".into()
            }
        );

        let s = parse(
            r#"{"type":"status","connected":true,"screencasting":true,
                "viewportWidth":1024,"viewportHeight":768,"engine":"chromium"}"#,
        )
        .unwrap();
        assert_eq!(
            s,
            ServerMessage::Status {
                connected: true,
                screencasting: true,
                viewport_width: 1024,
                viewport_height: 768,
                engine: Some("chromium".into()),
            }
        );

        // A status with no dimensions must not resize the viewer to zero.
        let bare = parse(r#"{"type":"status","connected":false,"screencasting":false}"#).unwrap();
        assert_eq!(
            bare,
            ServerMessage::Status {
                connected: false,
                screencasting: false,
                viewport_width: 1280,
                viewport_height: 720,
                engine: None,
            }
        );
    }

    #[test]
    fn the_active_tabs_url_is_what_the_status_line_shows() {
        let t = parse(
            r#"{"type":"tabs","tabs":[
                 {"url":"http://a","active":false},
                 {"url":"http://localhost:3000","active":true}]}"#,
        )
        .unwrap();
        assert_eq!(
            t,
            ServerMessage::Tabs {
                active_url: Some("http://localhost:3000".into())
            }
        );
        // No active tab is not an error, it is simply nothing to display.
        let none = parse(r#"{"type":"tabs","tabs":[{"url":"http://a","active":false}]}"#).unwrap();
        assert_eq!(none, ServerMessage::Tabs { active_url: None });
    }

    #[test]
    fn only_console_errors_earn_a_place_in_the_status_line() {
        assert_eq!(
            parse(r#"{"type":"console","level":"error","text":"boom"}"#).unwrap(),
            ServerMessage::ConsoleError("boom".into())
        );
        assert_eq!(
            parse(r#"{"type":"console","level":"log","text":"chatter"}"#).unwrap(),
            ServerMessage::Other
        );
        assert_eq!(
            parse(r#"{"type":"page_error","text":"TypeError"}"#).unwrap(),
            ServerMessage::PageError("TypeError".into())
        );
    }

    #[test]
    fn an_unknown_message_is_ignored_rather_than_fatal() {
        // The stream carries dashboard traffic we are not the audience for. A
        // new type upstream must not take the viewer down.
        assert_eq!(
            parse(r#"{"type":"something_new","payload":1}"#).unwrap(),
            ServerMessage::Other
        );
        assert!(parse("not json").is_none());
        assert!(parse(r#"{"no":"type"}"#).is_none());
    }

    #[test]
    fn pacing_is_requested_as_ack_so_the_terminal_sets_the_rate() {
        let c = config_ack_pacing(12);
        assert_eq!(c["type"], "config");
        assert_eq!(c["maxFps"], 12);
        assert_eq!(c["pacing"], "ack");
        assert_eq!(ack(9)["seq"], 9);
    }
}
