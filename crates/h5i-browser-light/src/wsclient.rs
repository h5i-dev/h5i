//! A WebSocket client, for the one case this engine is uniquely good at.
//!
//! The argument for building this is narrower than "pages use WebSocket". It is
//! that **this engine's stated advantage is reach** (ROADMAP §B11.3): a cloud
//! browser cannot open `localhost:3000`, a staging host, or anything behind a
//! VPN, and for a coding agent that is most of what it needs to look at. A dev
//! server's hot-module-reload channel is a WebSocket. So the place this engine
//! alone can reach was also the place it rendered a half-built page, and
//! §B11.5.9's "a live application shows nothing without them" is not a general
//! gap so much as a hole in the middle of the case we win.
//!
//! ## What is built, and what is refused by name
//!
//! **`ws://` only.** `wss://` needs a raw TLS stream, which `reqwest` does not
//! hand out, so it would mean taking `rustls` and a root store as direct
//! dependencies. It is refused with the reason rather than failing obscurely.
//! For the case above this costs nothing: a dev server's HMR socket is `ws://`.
//!
//! **Loopback only, whenever an egress proxy is configured.** This is the
//! important one. A WebSocket is a raw `TcpStream`, so it does not go through
//! the proxy that `reqwest` was configured with — and inside a box that proxy is
//! how the sandbox's own allowlist stays in the path. Rather than quietly open a
//! hole in it, a non-loopback socket is refused whenever `$H5I_EGRESS_PROXY` is
//! set. Loopback is exempt because the proxy already excludes it
//! (`NoProxy::from_string("localhost,127.0.0.1,::1")` in [`crate::net::Broker`]),
//! so nothing is being bypassed that was ever in the path.
//!
//! ## Every frame is receipted
//!
//! The engine's central claim is that the receipt is not an observation of the
//! network, it *is* the network. A socket open for ten minutes carrying four
//! hundred messages could be honoured two ways: receipt the handshake only, and
//! quietly stop covering everything after it — which is exactly the CONNECT-gate
//! blindness this engine exists to remove — or receipt every frame.
//!
//! Every frame. Each one is written as an ordinary request/response pair with
//! `WS-SEND` or `WS-RECV` as its method, so the console, `h5i box watch` and the
//! export bundle all show socket traffic **with no changes to any of them** and
//! no new phase for an old reader to skip. It costs a sink write per frame,
//! which is the price of the guarantee rather than an oversight.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex};

use h5i_error::H5iError;
use url::Url;

use crate::net::Broker;
use crate::ws::{self, Incoming};

/// Longest a handshake may take.
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How many undelivered messages a socket may hold.
///
/// A **bounded** channel, which is a stronger statement than the cap this used
/// to apply on the way out. The old shape trimmed an unbounded queue when the
/// page happened to drain it — but the page only drains at a settle, and a
/// resident session is idle between verbs by design, so a chatty socket grew
/// without limit in exactly the case the cap was written for.
///
/// Bounded, the reader thread blocks instead, TCP back-pressures the server, and
/// nothing is silently lost. That also removes a drop counter which, because it
/// only ever increased, made every later drain report an error event — and an
/// event delivered every round is a page the settle loop can never call idle.
const MAX_QUEUED: usize = 512;

/// What a socket hands the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Open,
    Message(String),
    /// A message carrying an event name, which only server-sent events have.
    ///
    /// A separate variant rather than a marker inside the payload. The first
    /// attempt packed the name into the string as a first line and let the
    /// prelude guess which messages had one, so a plain multi-line
    /// `data: one\ndata: two` was read as an event *named* `one` carrying
    /// `two`, and the page's `onmessage` never fired.
    Named { name: String, data: String },
    /// The peer or the transport ended it. Carries a reason for the page's
    /// `close` event, and for the console.
    Closed(String),
    /// Something went wrong that the page should see as an `error`.
    Failed(String),
}

/// One open socket.
pub struct Socket {
    /// The write half. `None` once closed.
    out: Mutex<Option<TcpStream>>,
    rx: Mutex<Receiver<Event>>,
    /// The receipt sequence the handshake was recorded under, so frames can be
    /// attributed to the connection that carried them.
    pub seq: u64,
    pub url: Url,
    broker: Arc<Broker>,
    closed: Mutex<bool>,
}

impl Socket {
    /// Open one, or say why not.
    ///
    /// The policy check and the decision record happen *before* the TCP
    /// connect, the same order [`Broker::send_from`] uses and for the same
    /// reason: no receipt, no connection.
    pub fn open(broker: Arc<Broker>, url: &Url, document: Option<&Url>) -> Result<Socket, String> {
        if url.scheme() == "wss" {
            return Err(format!(
                "`wss://` is not built in this engine, so {url} cannot be opened. It needs a raw \
                 TLS stream, which the HTTP client here does not expose. `ws://` works, which \
                 covers a dev server's hot-reload channel — the case this engine is for."
            ));
        }
        if url.scheme() != "ws" {
            return Err(format!("{url} is not a WebSocket address"));
        }

        let loopback = is_loopback(url);
        if !loopback && broker.has_proxy() {
            return Err(format!(
                "{url} is refused: this session routes through an egress proxy, and a WebSocket \
                 is a raw socket that would not go through it. Opening one would step around \
                 the allowlist the proxy enforces. Loopback sockets are allowed, because the \
                 proxy does not carry those either."
            ));
        }

        // Policy first, and receipted before anything is dialled.
        let seq = broker.authorise_socket(url, document)?;

        let host = url.host_str().ok_or_else(|| format!("{url} has no host"))?;
        let port = url.port().unwrap_or(80);
        let stream = TcpStream::connect((host, port)).map_err(|e| {
            format!("could not reach {host}:{port}: {e}")
        })?;
        stream
            .set_read_timeout(Some(HANDSHAKE_TIMEOUT))
            .map_err(|e| e.to_string())?;

        // One reader for the handshake *and* the frames that follow it.
        //
        // This was a bug before it was a design note. A `BufReader` reads
        // ahead, so the one used for the handshake headers had already pulled
        // the server's first data frame into its buffer — and dropping it threw
        // that frame away. A server that greets on connect (which is what a
        // hot-reload channel does) looked like a server that never spoke.
        let reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
        let reader = handshake(reader, &stream, url, host, port)?;

        // Reads have no deadline once the handshake is done: a socket that is
        // quiet is not a socket that is broken, and a dev server's HMR channel
        // is quiet almost all the time.
        let _ = stream.set_read_timeout(None);

        let (tx, rx) = std::sync::mpsc::sync_channel(MAX_QUEUED);
        // Queued before the reader starts, so `open` reaches the page ahead of
        // anything the peer says. Capacity is far above one, so it cannot block.
        let _ = tx.send(Event::Open);
        let broker_for_thread = broker.clone();
        let url_for_thread = url.clone();

        // The same shape the fetch path already uses: a worker thread does the
        // blocking work and hands results back over a channel, because the page
        // and its realm are single-threaded and `!Send`.
        std::thread::Builder::new()
            .name(format!("h5i-ws-{seq}"))
            .spawn(move || read_loop(reader, tx, broker_for_thread, url_for_thread))
            .map_err(|e| format!("could not start the socket reader: {e}"))?;

        Ok(Socket {
            out: Mutex::new(Some(stream)),
            rx: Mutex::new(rx),
            seq,
            url: url.clone(),
            broker,
            closed: Mutex::new(false),
        })
    }

    /// Send one text frame.
    pub fn send(&self, text: &str) -> Result<(), String> {
        // Receipted before it goes, like everything else here.
        self.broker
            .record_socket_frame(&self.url, Direction::Send, text.len() as u64)
            .map_err(|e| format!("refusing to send: the receipt could not be written: {e}"))?;

        let mut guard = self.out.lock().map_err(|_| "socket lock poisoned")?;
        let Some(stream) = guard.as_mut() else {
            return Err("this socket is closed".to_string());
        };
        send_masked(stream, 0x1, text.as_bytes()).map_err(|e| e.to_string())
    }

    /// Everything that has arrived since the last drain.
    ///
    /// Non-blocking by construction: this is called from the settle loop and
    /// from verb boundaries, and a blocking read there would stall the page.
    pub fn drain(&self) -> Vec<Event> {
        let mut out = Vec::new();
        if let Ok(rx) = self.rx.lock() {
            while let Ok(event) = rx.try_recv() {
                out.push(event);
            }
        }
        out
    }

    pub fn close(&self) {
        if let Ok(mut closed) = self.closed.lock() {
            if *closed {
                return;
            }
            *closed = true;
        }
        if let Ok(mut guard) = self.out.lock()
            && let Some(stream) = guard.as_mut()
        {
            let _ = send_masked(stream, 0x8, &[]);
            let _ = stream.shutdown(std::net::Shutdown::Both);
            *guard = None;
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed.lock().map(|c| *c).unwrap_or(true)
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        self.close();
    }
}

/// Which way a frame went, for the receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Send,
    Receive,
    /// An event on a server-sent stream.
    ///
    /// Its own variant because the receipt has to say what actually crossed the
    /// wire. An `EventSource` opened as `SSE-OPEN` and then recorded every
    /// event as `WS-RECV`, which describes a WebSocket frame on a connection
    /// that is not one — a log that misdescribes the protocol, in an engine
    /// whose claim is that the log is the truth about the wire.
    StreamReceive,
}

impl Direction {
    /// The method a frame is recorded under.
    ///
    /// Hyphenated so it cannot be mistaken for an HTTP verb by anything reading
    /// the log. A frame is not a request, but "a thing that crossed the wire,
    /// this size, in this direction" is exactly what a `RequestRecord` holds,
    /// and reusing it means every existing reader shows socket traffic without
    /// being taught anything.
    pub fn as_method(self) -> &'static str {
        match self {
            Direction::Send => "WS-SEND",
            Direction::Receive => "WS-RECV",
            Direction::StreamReceive => "SSE-RECV",
        }
    }
}

/// Read frames until the socket ends, receipting each one.
fn read_loop(
    mut reader: BufReader<TcpStream>,
    tx: SyncSender<Event>,
    broker: Arc<Broker>,
    url: Url,
) {
    loop {
        match ws::read_message(&mut reader) {
            Ok(Incoming::Text(text)) => {
                // No receipt, no delivery. The same rule the fetch path
                // follows: if the record cannot be written, the page does not
                // get to act on the bytes.
                if let Err(e) =
                    broker.record_socket_frame(&url, Direction::Receive, text.len() as u64)
                {
                    let _ = tx.send(Event::Failed(format!(
                        "a frame arrived that could not be receipted, so it was not delivered: {e}"
                    )));
                    return;
                }
                if tx.send(Event::Message(text)).is_err() {
                    return;
                }
            }
            Ok(Incoming::Close) => {
                let _ = tx.send(Event::Closed("the peer closed the socket".to_string()));
                return;
            }
            // Ping and pong are protocol chatter, not page-visible messages.
            Ok(Incoming::Ping(_)) | Ok(Incoming::Pong) => {}
            Err(error) => {
                let _ = tx.send(Event::Closed(format!("{error}")));
                return;
            }
        }
    }
}

/// The opening handshake.
///
/// Takes the reader and gives it back, because whatever it buffered past the
/// headers is the beginning of the frame stream and must not be dropped.
fn handshake(
    mut reader: BufReader<TcpStream>,
    stream: &TcpStream,
    url: &Url,
    host: &str,
    port: u16,
) -> Result<BufReader<TcpStream>, String> {
    let mut key_bytes = [0u8; 16];
    getrandom::getrandom(&mut key_bytes)
        .map_err(|e| format!("could not generate a handshake key: {e}"))?;
    let key = base64_encode(&key_bytes);

    let path = match url.query() {
        Some(query) => format!("{}?{}", url.path(), query),
        None => url.path().to_string(),
    };
    let request = format!(
        "GET {path} HTTP/1.1\r\n\
         Host: {host}:{port}\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Key: {key}\r\n\
         Sec-WebSocket-Version: 13\r\n\
         \r\n"
    );

    let mut writer = stream.try_clone().map_err(|e| e.to_string())?;
    writer
        .write_all(request.as_bytes())
        .map_err(|e| format!("could not send the handshake: {e}"))?;
    writer.flush().map_err(|e| e.to_string())?;

    let mut status = String::new();
    reader
        .read_line(&mut status)
        .map_err(|e| format!("no answer to the handshake: {e}"))?;
    if !status.contains("101") {
        return Err(format!(
            "the server did not upgrade the connection: {}",
            status.trim()
        ));
    }

    let expected = ws::accept_key(&key);
    let mut accepted = false;
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .map_err(|e| format!("the handshake headers ended early: {e}"))?;
        if read == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("sec-websocket-accept")
            && value.trim() == expected
        {
            accepted = true;
        }
    }
    if !accepted {
        // Checked rather than assumed. Without it, anything answering `101`
        // would be treated as a WebSocket peer, and the framing that follows
        // would be read out of whatever it sent.
        return Err(
            "the server's Sec-WebSocket-Accept did not match the key we sent, so this is not \
             the WebSocket endpoint we asked for"
                .to_string(),
        );
    }
    Ok(reader)
}

/// Write one masked frame.
///
/// [`crate::ws::send_frame`] is the *server* half and writes unmasked frames,
/// which is what RFC 6455 §5.1 requires of a server and forbids of a client. So
/// the direction that had no implementation is here, and the read direction is
/// shared: `ws::read_message` already branches on the MASK bit.
fn send_masked(stream: &mut TcpStream, opcode: u8, payload: &[u8]) -> Result<(), H5iError> {
    let mut header: Vec<u8> = Vec::with_capacity(14);
    header.push(0x80 | opcode);

    let len = payload.len();
    if len < 126 {
        header.push(0x80 | len as u8);
    } else if len <= u16::MAX as usize {
        header.push(0x80 | 126);
        header.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        header.push(0x80 | 127);
        header.extend_from_slice(&(len as u64).to_be_bytes());
    }

    let mut mask = [0u8; 4];
    getrandom::getrandom(&mut mask)
        .map_err(|e| H5iError::Internal(format!("could not generate a frame mask: {e}")))?;
    header.extend_from_slice(&mask);

    let masked: Vec<u8> = payload
        .iter()
        .enumerate()
        .map(|(i, byte)| byte ^ mask[i % 4])
        .collect();

    stream.write_all(&header).map_err(H5iError::Io)?;
    stream.write_all(&masked).map_err(H5iError::Io)?;
    stream.flush().map_err(H5iError::Io)?;
    Ok(())
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Whether this address is the machine we are already on.
fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(name)) => {
            name.eq_ignore_ascii_case("localhost") || name.eq_ignore_ascii_case("localhost.")
        }
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_is_recognised_in_every_spelling_that_matters() {
        for address in [
            "ws://localhost:5173/",
            "ws://127.0.0.1:5173/",
            "ws://[::1]:5173/",
        ] {
            assert!(
                is_loopback(&Url::parse(address).unwrap()),
                "{address} should be loopback"
            );
        }
        for address in ["ws://example.com/", "ws://10.0.0.1/", "ws://0.0.0.0/"] {
            assert!(
                !is_loopback(&Url::parse(address).unwrap()),
                "{address} should not be loopback"
            );
        }
    }

    #[test]
    fn wss_is_refused_by_name_rather_than_failing_obscurely() {
        let broker = Arc::new(
            crate::net::Broker::new(
                crate::policy::Policy::new(),
                Arc::new(crate::receipt::MemorySink::new()),
                None,
            )
            .expect("broker"),
        );
        let url = Url::parse("wss://example.com/socket").unwrap();
        let error = match Socket::open(broker, &url, None) {
            Err(error) => error,
            Ok(_) => panic!("wss should not have opened"),
        };
        assert!(error.contains("wss://"), "{error}");
        assert!(error.contains("not built"), "{error}");
        assert!(
            error.contains("ws://"),
            "it should say what does work: {error}"
        );
    }

    #[test]
    fn a_remote_socket_is_refused_while_a_proxy_is_in_the_path() {
        // The containment point. A WebSocket is a raw socket and does not go
        // through the proxy, so opening a remote one would step around the
        // allowlist the proxy enforces inside a box.
        let broker = Arc::new(
            crate::net::Broker::new(
                crate::policy::Policy::new().allow_all_of(&["example.com".to_string()]),
                Arc::new(crate::receipt::MemorySink::new()),
                Some("http://127.0.0.1:3128"),
            )
            .expect("broker"),
        );
        let url = Url::parse("ws://example.com/socket").unwrap();
        let error = match Socket::open(broker, &url, None) {
            Err(error) => error,
            Ok(_) => panic!("a remote socket behind a proxy should have been refused"),
        };
        assert!(error.contains("egress proxy"), "{error}");
        assert!(error.contains("Loopback"), "{error}");
    }

    #[test]
    fn the_direction_cannot_be_mistaken_for_an_http_verb() {
        assert_eq!(Direction::Send.as_method(), "WS-SEND");
        assert_eq!(Direction::Receive.as_method(), "WS-RECV");
        for method in [Direction::Send, Direction::Receive] {
            assert!(method.as_method().contains('-'));
        }
    }

    #[test]
    fn a_masked_frame_is_masked_and_round_trips_through_our_own_reader() {
        // The direction `ws.rs` had no implementation for. A client that sent
        // unmasked frames would be closed by any conforming server.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();

        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(stream);
            ws::read_message(&mut reader).expect("a readable message")
        });

        let mut client = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        send_masked(&mut client, 0x1, b"hello socket").expect("send");

        match server.join().expect("joined") {
            Incoming::Text(text) => assert_eq!(text, "hello socket"),
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn a_frame_over_125_bytes_uses_the_extended_length_and_still_round_trips() {
        let payload = "x".repeat(500);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let expected = payload.clone();

        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(stream);
            ws::read_message(&mut reader).expect("a readable message")
        });

        let mut client = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        send_masked(&mut client, 0x1, payload.as_bytes()).expect("send");

        match server.join().expect("joined") {
            Incoming::Text(text) => assert_eq!(text, expected),
            other => panic!("expected text, got {other:?}"),
        }
    }
}
