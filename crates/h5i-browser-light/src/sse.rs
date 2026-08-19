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
//! [`crate::net::Broker::send_from`] reads a whole body before it returns
//! (`read_capped`), and writes one response record with a final byte count.
//! That is the right shape for a document and the wrong shape for a stream: an
//! event stream never completes, so it would hit the response cap or the
//! client timeout, whichever came first, and be reported as an error.
//!
//! So there is a second path — [`crate::net::Broker::open_event_stream`] —
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
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use url::Url;

use crate::net::Broker;
use crate::wsclient::{Direction, Event};

/// Cap on one event's data, so a server cannot grow this without bound.
const MAX_EVENT_BYTES: usize = 1 << 20;

/// Cap on undelivered events, matching the socket client's.
const MAX_QUEUED: usize = 512;

/// One open event stream.
pub struct EventStream {
    inbox: Mutex<Vec<Event>>,
    dropped: Mutex<usize>,
    rx: Mutex<Receiver<Event>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    pub url: Url,
    closed: Mutex<bool>,
}

impl EventStream {
    /// Open one, or say why not.
    pub fn open(broker: Arc<Broker>, url: &Url, document: Option<&Url>) -> Result<Self, String> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(format!(
                "{url} is not an event-stream address: `EventSource` takes http or https."
            ));
        }

        // Policy, then the record, then the wire — the same order and the same
        // code as every other request here.
        let response = broker.open_event_stream(url, document)?;

        let (tx, rx) = channel();
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
            inbox: Mutex::new(vec![Event::Open]),
            dropped: Mutex::new(0),
            rx: Mutex::new(rx),
            stop,
            url: url.clone(),
            closed: Mutex::new(false),
        })
    }

    /// Everything that has arrived since the last drain.
    pub fn drain(&self) -> Vec<Event> {
        if let Ok(rx) = self.rx.lock() {
            while let Ok(event) = rx.try_recv() {
                if let Ok(mut inbox) = self.inbox.lock() {
                    inbox.push(event);
                    if inbox.len() > MAX_QUEUED {
                        let over = inbox.len() - MAX_QUEUED;
                        inbox.drain(..over);
                        if let Ok(mut dropped) = self.dropped.lock() {
                            *dropped += over;
                        }
                    }
                }
            }
        }
        self.inbox
            .lock()
            .map(|mut inbox| std::mem::take(&mut *inbox))
            .unwrap_or_default()
    }

    pub fn dropped(&self) -> usize {
        self.dropped.lock().map(|d| *d).unwrap_or(0)
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
    tx: Sender<Event>,
    broker: Arc<Broker>,
    url: Url,
    stop: Arc<std::sync::atomic::AtomicBool>,
) {
    let mut reader = BufReader::new(response);
    let mut data = String::new();
    let mut kind = String::new();

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
            if !data.is_empty() {
                if let Err(e) =
                    broker.record_socket_frame(&url, Direction::Receive, data.len() as u64)
                {
                    let _ = tx.send(Event::Failed(format!(
                        "an event arrived that could not be receipted, so it was not delivered: {e}"
                    )));
                    return;
                }
                let payload = if kind.is_empty() || kind == "message" {
                    std::mem::take(&mut data)
                } else {
                    // The type is carried in-band because the drain protocol
                    // has one string per event; the prelude splits it back out.
                    format!("{kind}\n{}", std::mem::take(&mut data))
                };
                if tx.send(Event::Message(payload)).is_err() {
                    return;
                }
            }
            data.clear();
            kind.clear();
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
fn read_line_capped<R: Read>(
    reader: &mut BufReader<R>,
    out: &mut String,
) -> std::io::Result<usize> {
    let mut taken = 0usize;
    loop {
        let mut byte = [0u8; 1];
        match reader.read(&mut byte)? {
            0 => return Ok(taken),
            _ => {
                taken += 1;
                out.push(byte[0] as char);
                if byte[0] == b'\n' {
                    return Ok(taken);
                }
                if taken > MAX_EVENT_BYTES {
                    return Err(std::io::Error::other(
                        "a line in this event stream exceeded the engine's cap",
                    ));
                }
            }
        }
    }
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

    fn broker_with(sink: Arc<crate::receipt::MemorySink>) -> Arc<Broker> {
        Arc::new(
            Broker::new(crate::policy::Policy::new(), sink, None).expect("broker"),
        )
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
            methods.iter().filter(|m| **m == "WS-RECV").count(),
            4,
            "one request and one response record per event: {methods:?}"
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
