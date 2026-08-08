//! The live view: the engine as something a human can watch.
//!
//! This speaks the wire format h5i's existing viewers already use, so
//! `h5i box view` and `h5i box view --term` work against this engine with no
//! change on their side: base64 JPEG frames in a JSON envelope, a `status`
//! message carrying the viewport so mouse coordinates mean something, and
//! `config`/`ack` pacing from the client.
//!
//! # Zero frames per second at rest
//!
//! The loop is driven by client messages, not by a timer. Tier 1 has no script,
//! so nothing changes on its own: a frame is produced when something *did*
//! change — a scroll that moved, a navigation that landed — and at rest the
//! process is idle rather than encoding identical JPEGs thirty times a second.
//! That falls out of the structure rather than being a special case, and it is
//! most of why an engine like this is cheap to leave open.
//!
//! `pacing: "ack"` (what the terminal viewer asks for) is satisfied for free by
//! the same shape: at most one frame is sent per client message, so the ack for
//! frame N arrives before anything could send frame N+1.

use std::io::BufReader;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use h5i_error::H5iError;
use serde_json::{json, Value};
use url::Url;

use crate::engine::{Page, PageFactory};
use crate::ws::{self, Incoming};

/// How far a key scrolls, as a fraction of the viewport.
const PAGE_SCROLL: f64 = 0.9;
const LINE_SCROLL: f64 = 64.0;

pub struct ServeOptions {
    pub addr: String,
    pub quality: u8,
    /// Where to advertise the port. h5i's viewers find a stream by scanning
    /// `<env>/tmp/agent-browser/*.stream`, so writing one here is what makes
    /// this engine discoverable without changing the viewer.
    pub stream_file: Option<PathBuf>,
    /// Serve one viewer and exit, which is what the tests and a one-shot
    /// demo want.
    pub once: bool,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            addr: "127.0.0.1:0".to_string(),
            quality: 80,
            stream_file: None,
            once: false,
        }
    }
}

/// A page plus the viewer state that surrounds it.
struct Session {
    factory: PageFactory,
    page: Page,
    quality: u8,
    seq: u64,
}

impl Session {
    fn viewport(&self) -> (u32, u32) {
        let options = self.factory.options();
        (options.width, options.height)
    }

    fn status_message(&self) -> Value {
        let (width, height) = self.viewport();
        json!({
            "type": "status",
            "connected": true,
            "screencasting": true,
            "viewportWidth": width,
            "viewportHeight": height,
            // Named honestly. A viewer that assumes Chromium semantics from
            // this string should find out here rather than from a missing
            // feature later.
            "engine": "h5i-browser-light",
        })
    }

    fn url_message(&self) -> Value {
        json!({"type": "url", "url": self.page.url().to_string()})
    }

    fn frame_message(&mut self) -> Result<Value, H5iError> {
        let jpeg = self.page.screenshot_jpeg(self.quality)?;
        self.seq += 1;
        Ok(json!({
            "type": "frame",
            "seq": self.seq,
            "data": base64::engine::general_purpose::STANDARD.encode(&jpeg),
        }))
    }

    /// Follow a link, replacing the page. A failed navigation leaves the
    /// current page in place and reports itself, because a viewer that goes
    /// blank on a denied link is indistinguishable from one that crashed.
    fn navigate(&mut self, url: &Url) -> Result<Vec<Value>, H5iError> {
        match self.factory.open(url) {
            Ok(page) => {
                self.page = page;
                Ok(vec![self.url_message()])
            }
            Err(error) => Ok(vec![json!({
                "type": "page_error",
                "text": format!("navigation to {url} failed: {error}"),
            })]),
        }
    }
}

/// Serve the live view until the viewer disconnects.
pub fn serve(factory: PageFactory, page: Page, options: ServeOptions) -> Result<(), H5iError> {
    let listener = TcpListener::bind(&options.addr)
        .map_err(|e| H5iError::Metadata(format!("could not bind {}: {e}", options.addr)))?;
    let port = listener
        .local_addr()
        .map_err(|e| H5iError::Metadata(format!("could not read the bound address: {e}")))?
        .port();

    if let Some(path) = &options.stream_file {
        write_stream_file(path, port)?;
    }
    eprintln!("h5i-browser-light: live view on 127.0.0.1:{port}");

    let mut session = Session {
        factory,
        page,
        quality: options.quality,
        seq: 0,
    };

    for incoming in listener.incoming() {
        let stream = incoming.map_err(H5iError::Io)?;
        if let Err(error) = serve_one(&mut session, stream) {
            // A viewer that closed its tab is not an error worth exiting on.
            eprintln!("h5i-browser-light: viewer disconnected: {error}");
        }
        if options.once {
            break;
        }
    }

    if let Some(path) = &options.stream_file {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

fn write_stream_file(path: &Path, port: u16) -> Result<(), H5iError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| H5iError::with_path(e, parent))?;
        }
    }
    std::fs::write(path, port.to_string()).map_err(|e| H5iError::with_path(e, path))
}

fn serve_one(session: &mut Session, mut stream: TcpStream) -> Result<(), H5iError> {
    ws::accept(&mut stream)?;

    // The viewport has to arrive before frames, or the viewer maps mouse
    // coordinates against its 1280x720 default and clicks land elsewhere.
    ws::send_text(&mut stream, &session.status_message().to_string())?;
    ws::send_text(&mut stream, &session.url_message().to_string())?;
    let first = session.frame_message()?;
    ws::send_text(&mut stream, &first.to_string())?;

    let mut reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|e| H5iError::Metadata(format!("could not clone the socket: {e}")))?,
    );

    loop {
        match ws::read_message(&mut reader)? {
            Incoming::Close => return Ok(()),
            Incoming::Pong => continue,
            Incoming::Ping(payload) => {
                ws::send_pong(&mut stream, &payload)?;
            }
            Incoming::Text(text) => {
                let Ok(message) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                for outgoing in handle(session, &message)? {
                    ws::send_text(&mut stream, &outgoing.to_string())?;
                }
            }
        }
    }
}

/// Handle one client message, returning what to send back.
///
/// Split out from the socket so the protocol can be tested without one.
fn handle(session: &mut Session, message: &Value) -> Result<Vec<Value>, H5iError> {
    let kind = message.get("type").and_then(Value::as_str).unwrap_or("");
    let (_, viewport_height) = session.viewport();

    let changed = match kind {
        // The viewer announces its pacing; answering with the current frame
        // means it has something to draw immediately.
        "config" => true,

        // Under ack pacing this is the client's permission to send the next
        // frame. Nothing has changed, so nothing is sent — which is what keeps
        // a still page at zero frames per second instead of a busy loop.
        "ack" => false,

        "input_mouse" => match message.get("eventType").and_then(Value::as_str) {
            Some("mouseWheel") => {
                let delta = message
                    .get("deltaY")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                session.page.scroll_by(0.0, delta)
            }
            Some("mouseReleased") => {
                let x = message.get("x").and_then(Value::as_f64).unwrap_or(0.0) as f32;
                let y = message.get("y").and_then(Value::as_f64).unwrap_or(0.0) as f32;
                if let Some(url) = session.page.link_at(x, y) {
                    let mut out = session.navigate(&url)?;
                    out.push(session.frame_message()?);
                    return Ok(out);
                }
                false
            }
            _ => false,
        },

        // keyUp never scrolls: acting on both halves would double every press.
        "input_keyboard"
            if message.get("eventType").and_then(Value::as_str) == Some("keyDown") =>
        {
            let key = message.get("key").and_then(Value::as_str).unwrap_or("");
            match scroll_for_key(key, viewport_height as f64) {
                Some(delta) => session.page.scroll_by(0.0, delta),
                None => false,
            }
        }

        _ => false,
    };

    if changed {
        Ok(vec![session.frame_message()?])
    } else {
        Ok(Vec::new())
    }
}

/// How far a key should scroll, or `None` if it is not a scrolling key.
///
/// `f64::MIN`/`MAX` for Home/End lean on `scroll_by` clamping to the document
/// rather than duplicating the bounds here.
fn scroll_for_key(key: &str, viewport_height: f64) -> Option<f64> {
    match key {
        "PageDown" | " " | "Space" => Some(viewport_height * PAGE_SCROLL),
        "PageUp" => Some(-viewport_height * PAGE_SCROLL),
        "ArrowDown" => Some(LINE_SCROLL),
        "ArrowUp" => Some(-LINE_SCROLL),
        "End" => Some(f64::MAX / 4.0),
        "Home" => Some(f64::MIN / 4.0),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::Broker;
    use crate::policy::Policy;
    use crate::receipt::MemorySink;
    use std::sync::Arc;

    fn session_with(html: &str) -> Session {
        let broker = Arc::new(
            Broker::new(Policy::new(), Arc::new(MemorySink::new()), None).expect("broker"),
        );
        let fonts = crate::fonts::load(&[], &crate::fonts::default_font_dirs(), Some(4));
        let sources = fonts.sources.clone();
        let options = crate::engine::PageOptions {
            width: 400,
            height: 200,
            ..Default::default()
        };
        let factory = PageFactory::new(broker, sources, options);
        let page = factory.from_html(html, &Url::parse("https://example.com/").unwrap());
        Session {
            factory,
            page,
            quality: 70,
            seq: 0,
        }
    }

    fn tall_page() -> &'static str {
        "<!doctype html><body><div style='height:4000px'>\
         <p>top</p><a href='/next'>next</a></div></body>"
    }

    #[test]
    fn status_carries_the_viewport_so_clicks_land_where_they_were_aimed() {
        let session = session_with("<p>hi</p>");
        let status = session.status_message();
        assert_eq!(status["viewportWidth"], 400);
        assert_eq!(status["viewportHeight"], 200);
        assert_eq!(status["type"], "status");
        // Not "chromium": a viewer should not infer Chromium behaviour here.
        assert_eq!(status["engine"], "h5i-browser-light");
    }

    #[test]
    fn a_frame_is_base64_jpeg_with_a_monotonic_seq() {
        let mut session = session_with("<p>hi</p>");
        let first = session.frame_message().expect("frame");
        let second = session.frame_message().expect("frame");

        assert_eq!(first["type"], "frame");
        assert_eq!(first["seq"], 1);
        assert_eq!(second["seq"], 2, "seq must advance for ack pacing to work");

        let data = first["data"].as_str().expect("data is a string");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .expect("data is base64");
        assert_eq!(&bytes[..2], &[0xFF, 0xD8], "JPEG SOI marker");
    }

    #[test]
    fn an_ack_alone_produces_no_frame() {
        // The zero-fps-at-rest property, as a test: if acking ever produced a
        // frame, two viewers would ping-pong forever at full CPU.
        let mut session = session_with(tall_page());
        let out = handle(&mut session, &json!({"type": "ack", "seq": 1})).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn a_scroll_that_moves_sends_a_frame_and_one_that_cannot_does_not() {
        let mut session = session_with(tall_page());

        let scrolled = handle(
            &mut session,
            &json!({"type":"input_mouse","eventType":"mouseWheel","deltaY": 120.0}),
        )
        .unwrap();
        assert_eq!(scrolled.len(), 1, "a real scroll redraws");
        assert_eq!(session.page.scroll_offset().1, 120.0);

        // Already at the top: scrolling up moves nothing, so nothing is sent.
        let stuck = handle(
            &mut session,
            &json!({"type":"input_mouse","eventType":"mouseWheel","deltaY": -9999.0}),
        )
        .unwrap();
        assert_eq!(session.page.scroll_offset().1, 0.0);
        assert_eq!(stuck.len(), 1, "the first one moved to the top");

        let no_move = handle(
            &mut session,
            &json!({"type":"input_mouse","eventType":"mouseWheel","deltaY": -50.0}),
        )
        .unwrap();
        assert!(no_move.is_empty(), "a scroll with nowhere to go sends nothing");
    }

    #[test]
    fn scroll_is_clamped_to_the_document() {
        let mut session = session_with(tall_page());
        session.page.scroll_by(0.0, 100_000.0);
        let (_, y) = session.page.scroll_offset();
        assert!(y > 0.0);
        assert!(
            y <= session.page.content_height(),
            "scrolled past the end of the document: {y}"
        );
    }

    #[test]
    fn keys_scroll_by_the_amounts_a_reader_expects() {
        assert_eq!(scroll_for_key("PageDown", 1000.0), Some(900.0));
        assert_eq!(scroll_for_key("PageUp", 1000.0), Some(-900.0));
        assert_eq!(scroll_for_key("ArrowDown", 1000.0), Some(LINE_SCROLL));
        assert!(scroll_for_key("End", 1000.0).unwrap() > 0.0);
        assert!(scroll_for_key("Home", 1000.0).unwrap() < 0.0);
        // A key with no scrolling meaning must not redraw.
        assert_eq!(scroll_for_key("a", 1000.0), None);
    }

    #[test]
    fn an_unknown_message_type_is_ignored_rather_than_fatal() {
        // Both viewers send messages this engine does not implement; treating
        // one as an error would drop the connection mid-session.
        let mut session = session_with("<p>hi</p>");
        let out = handle(&mut session, &json!({"type": "hoverHighlight"})).unwrap();
        assert!(out.is_empty());
        let out = handle(&mut session, &json!({"nonsense": true})).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn a_denied_navigation_reports_itself_instead_of_going_blank() {
        // The link points off-origin and the policy allows nothing, so the
        // click must produce an error message and keep the current page.
        let mut session = session_with(
            "<!doctype html><body><a href='https://denied.test/x' \
             style='display:block;width:400px;height:200px'>go</a></body>",
        );
        let before = session.page.url().to_string();

        let out = handle(
            &mut session,
            &json!({"type":"input_mouse","eventType":"mouseReleased","x":10.0,"y":10.0,"button":"left"}),
        )
        .unwrap();

        let kinds: Vec<&str> = out
            .iter()
            .filter_map(|m| m.get("type").and_then(Value::as_str))
            .collect();
        assert!(kinds.contains(&"page_error"), "{kinds:?}");
        assert_eq!(session.page.url().to_string(), before, "page must survive");
    }

    #[test]
    fn the_stream_file_advertises_the_port_where_viewers_look_for_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("agent-browser").join("s1.stream");
        write_stream_file(&path, 45123).expect("writes");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "45123");
    }
}
