//! `EventSource`: a server-sent event stream.
//!
//! The other long-lived connection, and the one that fits this engine's HTTP
//! stack better than a WebSocket does — it *is* an HTTP response, just one that
//! never ends. So unlike [`crate::wsclient`] it goes through the same client,
//! the same proxy and the same TLS as everything else, which means `https://`
//! works and there is no loopback restriction to argue about.
//!
//! ## Why it needed a second exit from the broker
//!
//! [`crate::broker::Broker::send_from`] reads a whole body before it returns
//! (`read_capped`), and writes one response record with a final byte count.
//! That is the right shape for a document and the wrong shape for a stream: an
//! event stream never completes, so it would hit the response cap or the
//! client timeout, whichever came first, and be reported as an error.
//!
//! So there is a second path — [`crate::net::LocalBroker::begin_event_stream`] —
//! which shares the front half exactly (policy, then the decision record,
//! *then* the wire) and hands back the response to be read incrementally.
//! Sharing the front half is the point: a stream is authorised and receipted by
//! the same code as everything else, so there is one place where the
//! fail-closed rule lives.
//!
//! Each event is then receipted as it arrives, for the reason the socket client
//! gives at length: receipting the handshake alone would let this engine's
//! central claim quietly stop covering the bytes that follow it.

use std::io::{BufReader, Read};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex};

use url::Url;

use crate::net::LocalBroker;
use crate::wsclient::{Direction, Event};

/// Cap on one event's data, so a server cannot grow this without bound.
const MAX_EVENT_BYTES: usize = 1 << 20;

/// How many undelivered events this may hold, matching the socket client's —
/// and bounded for the same reason. See [`crate::wsclient`].
const MAX_QUEUED: usize = 512;

/// One open event stream.
pub struct EventStream {
    rx: Mutex<Receiver<Event>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    pub url: Url,
    closed: Mutex<bool>,
}

impl EventStream {
    /// Open one, or say why not.
    pub fn open(broker: Arc<LocalBroker>, url: &Url, document: Option<&Url>) -> Result<Self, String> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(format!(
                "{url} is not an event-stream address: `EventSource` takes http or https."
            ));
        }

        // Policy, then the record, then the wire — the same order and the same
        // code as every other request here.
        let response = broker.begin_event_stream(url, document)?;

        let (tx, rx) = std::sync::mpsc::sync_channel(MAX_QUEUED);
        let _ = tx.send(Event::Open);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_for_thread = stop.clone();
        let broker_for_thread = broker.clone();
        let url_for_thread = url.clone();

        std::thread::Builder::new()
            .name("h5i-sse".to_string())
            .spawn(move || {
                read_loop(
                    response,
                    tx,
                    broker_for_thread,
                    url_for_thread,
                    stop_for_thread,
                )
            })
            .map_err(|e| format!("could not start the event-stream reader: {e}"))?;

        Ok(EventStream {
            rx: Mutex::new(rx),
            stop,
            url: url.clone(),
            closed: Mutex::new(false),
        })
    }

    /// Everything that has arrived since the last drain.
    pub fn drain(&self) -> Vec<Event> {
        let mut out = Vec::new();
        if let Ok(rx) = self.rx.lock() {
            while let Ok(event) = rx.try_recv() {
                out.push(event);
            }
        }
        out
    }

    /// Ask the reader to stop.
    ///
    /// Cooperative rather than a socket shutdown: the reader owns the response
    /// and checks this between events. A blocking read already in progress
    /// finishes first, which is the cost of not reaching into `reqwest`'s
    /// internals to close a connection out from under it.
    pub fn close(&self) {
        if let Ok(mut closed) = self.closed.lock() {
            if *closed {
                return;
            }
            *closed = true;
        }
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn is_closed(&self) -> bool {
        self.closed.lock().map(|c| *c).unwrap_or(true)
    }
}

/// An event stream is read-only, and says so rather than quietly dropping what
/// a page tried to send. `EventSource` has no `send` in any browser; a page
/// calling one is a page with a bug, and an error is what tells it so.
impl crate::broker::Channel for EventStream {
    fn send(&self, _text: &str) -> Result<(), String> {
        Err("an event stream is read-only: there is nothing to send on".to_string())
    }

    fn drain(&self) -> Vec<Event> {
        EventStream::drain(self)
    }

    fn close(&self) {
        EventStream::close(self)
    }
}

impl Drop for EventStream {
    fn drop(&mut self) {
        self.close();
    }
}

/// Parse the `text/event-stream` format until the response ends.
///
/// Deliberately the useful subset: `data:` lines accumulate, a blank line
/// dispatches, `event:` names the type and `id:`/`retry:` are read and ignored.
/// Reconnection is **not** implemented, and that is a decision rather than an
/// omission: an engine that silently re-dialled would be making requests the
/// agent never asked for and the receipt would show them arriving from nowhere.
/// A stream that ends fires `error` and stays ended.
fn read_loop(
    response: reqwest::blocking::Response,
    tx: SyncSender<Event>,
    broker: Arc<LocalBroker>,
    url: Url,
    stop: Arc<std::sync::atomic::AtomicBool>,
) {
    let mut reader = BufReader::new(response);
    let mut data = String::new();
    let mut kind = String::new();
    // Whether a `data` field was seen at all, which is not the same as whether
    // it had content. `data:\n\n` and `event: ping\ndata:\n\n` are ordinary
    // keep-alive shapes, and the spec dispatches on the blank line whenever a
    // data field appeared. Guarding on `!data.is_empty()` swallowed them.
    let mut saw_data = false;

    loop {
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        let mut line = String::new();
        match read_line_capped(&mut reader, &mut line) {
            Ok(0) => {
                let _ = tx.send(Event::Closed("the server closed the stream".to_string()));
                return;
            }
            Ok(_) => {}
            Err(error) => {
                let _ = tx.send(Event::Closed(format!("{error}")));
                return;
            }
        }

        let line = line.trim_end_matches(['\r', '\n']);

        if line.is_empty() {
            // Blank line: dispatch what has accumulated.
            if saw_data {
                if let Err(e) = broker.record_socket_frame(
                    &url,
                    Direction::StreamReceive,
                    data.len() as u64,
                )
                {
                    let _ = tx.send(Event::Failed(format!(
                        "an event arrived that could not be receipted, so it was not delivered: {e}"
                    )));
                    return;
                }
                // The name travels as its own field rather than packed into
                // the payload. Packing it meant the prelude had to guess which
                // messages carried one, and a plain multi-line
                // `data: one\ndata: two` was read as an event named `one`.
                let event = if kind.is_empty() || kind == "message" {
                    Event::Message(std::mem::take(&mut data))
                } else {
                    Event::Named {
                        name: std::mem::take(&mut kind),
                        data: std::mem::take(&mut data),
                    }
                };
                if tx.send(event).is_err() {
                    return;
                }
            }
            data.clear();
            kind.clear();
            saw_data = false;
            continue;
        }

        // A comment. Servers send these as keep-alives.
        if line.starts_with(':') {
            continue;
        }

        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "data" => {
                saw_data = true;
                if data.len() + value.len() > MAX_EVENT_BYTES {
                    let _ = tx.send(Event::Failed(
                        "an event exceeded the size this engine holds for one".to_string(),
                    ));
                    return;
                }
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value);
            }
            "event" => kind = value.to_string(),
            // Read and ignored: `id` matters for reconnection, which is not
            // built, and pretending to honour it would be a claim about
            // behaviour that does not exist.
            "id" | "retry" => {}
            _ => {}
        }
    }
}

/// `read_line` with a cap, because a server that never sends a newline is an
/// unbounded allocation on this side.
///
/// Accumulates **bytes** and decodes once at the end. Pushing each byte as a
/// `char` decoded the stream as Latin-1, so every non-ASCII payload arrived
/// mangled — `café` reaching the page as `cafÃ©` — and the byte count that goes
/// into the receipt was wrong along with it.
fn read_line_capped<R: Read>(
    reader: &mut BufReader<R>,
    out: &mut String,
) -> std::io::Result<usize> {
    let mut bytes: Vec<u8> = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        match reader.read(&mut byte)? {
            0 => break,
            _ => {
                bytes.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
                if bytes.len() > MAX_EVENT_BYTES {
                    return Err(std::io::Error::other(
                        "a line in this event stream exceeded the engine's cap",
                    ));
                }
            }
        }
    }
    let taken = bytes.len();
    out.push_str(&String::from_utf8_lossy(&bytes));
    Ok(taken)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// A server that sends a couple of events and then ends the response.
    fn sse_server(body: &'static str) -> (u16, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut discard = [0u8; 1024];
                let _ = stream.read(&mut discard);
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                         Content-Length: {}\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
                let _ = stream.flush();
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        });
        (port, handle)
    }

    fn broker_with(sink: Arc<crate::receipt::MemorySink>) -> Arc<LocalBroker> {
        LocalBroker::new(crate::policy::Policy::new(), sink, None).expect("broker")
    }

    #[test]
    fn events_arrive_as_messages_and_every_one_is_receipted() {
        let (port, server) = sse_server("data: first\n\ndata: second\n\n");
        let sink = Arc::new(crate::receipt::MemorySink::new());
        let broker = broker_with(sink.clone());
        let url = Url::parse(&format!("http://127.0.0.1:{port}/events")).unwrap();

        let stream = EventStream::open(broker, &url, None).expect("opened");

        let mut seen: Vec<String> = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while seen.len() < 2 && std::time::Instant::now() < deadline {
            for event in stream.drain() {
                if let Event::Message(text) = event {
                    seen.push(text);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        assert_eq!(seen, vec!["first".to_string(), "second".to_string()]);

        let records = sink.records();
        let methods: Vec<&str> = records.iter().map(|r| r.method.as_str()).collect();
        assert!(methods.contains(&"SSE-OPEN"), "{methods:?}");
        assert_eq!(
            methods.iter().filter(|m| **m == "SSE-RECV").count(),
            4,
            "one request and one response record per event, labelled as SSE: {methods:?}"
        );

        let _ = server.join();
    }

    #[test]
    fn multi_line_data_is_joined_with_newlines_as_the_format_says() {
        let (port, server) = sse_server("data: one\ndata: two\n\n");
        let sink = Arc::new(crate::receipt::MemorySink::new());
        let broker = broker_with(sink);
        let url = Url::parse(&format!("http://127.0.0.1:{port}/events")).unwrap();
        let stream = EventStream::open(broker, &url, None).expect("opened");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut got = None;
        while got.is_none() && std::time::Instant::now() < deadline {
            for event in stream.drain() {
                if let Event::Message(text) = event {
                    got = Some(text);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(got.as_deref(), Some("one\ntwo"));
        let _ = server.join();
    }

    #[test]
    fn a_comment_is_a_keepalive_and_not_an_event() {
        let (port, server) = sse_server(": keep alive\n\ndata: real\n\n");
        let sink = Arc::new(crate::receipt::MemorySink::new());
        let broker = broker_with(sink);
        let url = Url::parse(&format!("http://127.0.0.1:{port}/events")).unwrap();
        let stream = EventStream::open(broker, &url, None).expect("opened");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut messages: Vec<String> = Vec::new();
        while messages.is_empty() && std::time::Instant::now() < deadline {
            for event in stream.drain() {
                if let Event::Message(text) = event {
                    messages.push(text);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(messages, vec!["real".to_string()]);
        let _ = server.join();
    }

    #[test]
    fn multi_line_data_reaches_the_page_as_a_plain_message() {
        // The regression. The name used to travel packed into the payload as a
        // first line, and the JS side guessed which messages had one — so
        // `data: one\ndata: two` became an event *named* `one` carrying `two`,
        // and `onmessage` never fired at all.
        let (port, server) = sse_server("data: one\ndata: two\n\n");
        let sink = Arc::new(crate::receipt::MemorySink::new());
        let broker = broker_with(sink);
        let url = Url::parse(&format!("http://127.0.0.1:{port}/events")).unwrap();
        let stream = EventStream::open(broker, &url, None).expect("opened");

        let got = collect_one(&stream);
        assert_eq!(
            got,
            Some(Event::Message("one\ntwo".to_string())),
            "a plain multi-line message must not become a named event"
        );
        let _ = server.join();
    }

    #[test]
    fn a_named_event_carries_its_name_beside_the_data() {
        let (port, server) = sse_server("event: tick\ndata: payload\n\n");
        let sink = Arc::new(crate::receipt::MemorySink::new());
        let broker = broker_with(sink);
        let url = Url::parse(&format!("http://127.0.0.1:{port}/events")).unwrap();
        let stream = EventStream::open(broker, &url, None).expect("opened");

        assert_eq!(
            collect_one(&stream),
            Some(Event::Named {
                name: "tick".to_string(),
                data: "payload".to_string()
            })
        );
        let _ = server.join();
    }

    #[test]
    fn a_non_ascii_payload_survives_the_wire() {
        // The reader used to push each byte as a `char`, decoding the stream as
        // Latin-1, so every multi-byte character arrived mangled and the
        // receipted byte count was wrong with it.
        let (port, server) = sse_server("data: café → 日本語\n\n");
        let sink = Arc::new(crate::receipt::MemorySink::new());
        let broker = broker_with(sink);
        let url = Url::parse(&format!("http://127.0.0.1:{port}/events")).unwrap();
        let stream = EventStream::open(broker, &url, None).expect("opened");

        assert_eq!(
            collect_one(&stream),
            Some(Event::Message("café → 日本語".to_string()))
        );
        let _ = server.join();
    }

    #[test]
    fn an_event_with_an_empty_data_field_still_dispatches() {
        // `data:\n\n` and `event: ping\ndata:\n\n` are ordinary keep-alive
        // shapes. The spec dispatches on the blank line whenever a data field
        // appeared; guarding on non-empty content swallowed them entirely.
        let (port, server) = sse_server("event: ping\ndata:\n\n");
        let sink = Arc::new(crate::receipt::MemorySink::new());
        let broker = broker_with(sink);
        let url = Url::parse(&format!("http://127.0.0.1:{port}/events")).unwrap();
        let stream = EventStream::open(broker, &url, None).expect("opened");

        assert_eq!(
            collect_one(&stream),
            Some(Event::Named {
                name: "ping".to_string(),
                data: String::new()
            })
        );
        let _ = server.join();
    }

    #[test]
    fn a_stream_that_opened_is_receipted_at_both_phases() {
        // Every other path in the broker writes both halves. A request that
        // never completes leaves the console's request/response pairing showing
        // an open connection for the life of the session.
        let (port, server) = sse_server("data: x\n\n");
        let sink = Arc::new(crate::receipt::MemorySink::new());
        let broker = broker_with(sink.clone());
        let url = Url::parse(&format!("http://127.0.0.1:{port}/events")).unwrap();
        let stream = EventStream::open(broker, &url, None).expect("opened");
        let _ = collect_one(&stream);

        let opens: Vec<_> = sink
            .records()
            .into_iter()
            .filter(|r| r.method == "SSE-OPEN")
            .collect();
        assert_eq!(opens.len(), 2, "both phases: {opens:?}");
        assert!(opens.iter().any(|r| r.phase == crate::receipt::Phase::Request));
        let response = opens
            .iter()
            .find(|r| r.phase == crate::receipt::Phase::Response)
            .expect("a response half");
        assert_eq!(response.status, Some(200));
        let _ = server.join();
    }

    /// Wait for the first message-shaped event, or give up.
    fn collect_one(stream: &EventStream) -> Option<Event> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            for event in stream.drain() {
                if matches!(event, Event::Message(_) | Event::Named { .. }) {
                    return Some(event);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        None
    }

    #[test]
    fn a_scheme_that_is_not_http_is_refused_by_name() {
        let sink = Arc::new(crate::receipt::MemorySink::new());
        let broker = broker_with(sink);
        let url = Url::parse("ws://127.0.0.1:1/events").unwrap();
        match EventStream::open(broker, &url, None) {
            Err(error) => assert!(error.contains("http"), "{error}"),
            Ok(_) => panic!("ws is not an event-stream address"),
        }
    }
}
