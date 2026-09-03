//! A WebSocket client, for the one case this engine is uniquely good at.

use std::io::{BufReader, Read};
use std::net::TcpStream;
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex};

use h5i_error::H5iError;
use url::Url;

use crate::net::LocalBroker;
use crate::ws::{self, Incoming};

/// Shared plain and TLS socket transport.
use crate::rawsock::{connect_to_any, is_loopback, tls_config, Wire, TLS_READ_SLICE};

/// The most one line of the server's handshake response may be, and the most
/// lines there may be.
///
/// `read_line` grows a `String` until it meets a newline, so a server that
/// answers `101` and then sends bytes without one is an unbounded allocation on
/// this side, and a server that sends short header lines forever is a loop that
/// never ends. The server half of this protocol bounds its own handshake; the
/// client half read the answer of whatever it had just dialled with no bound at
/// all, which is the wrong way round.
const MAX_HANDSHAKE_LINE: usize = 8 * 1024;
const MAX_HANDSHAKE_LINES: usize = 128;

/// Longest a handshake may take.
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How many undelivered messages a socket may hold.
const MAX_QUEUED: usize = 512;

/// What a socket hands the page.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    out: Mutex<Option<Wire>>,
    rx: Mutex<Receiver<Event>>,
    /// The receipt sequence the handshake was recorded under, so frames can be
    /// attributed to the connection that carried them.
    pub seq: u64,
    pub url: Url,
    broker: Arc<LocalBroker>,
    closed: Mutex<bool>,
}

impl Socket {
    /// Open one, or say why not.
    ///
    /// The policy check and the decision record happen *before* the TCP
    /// connect, the same order [`crate::broker::Broker::send_from`] uses and for the same
    /// reason: no receipt, no connection.
    pub fn open(broker: Arc<LocalBroker>, url: &Url, document: Option<&Url>) -> Result<Socket, String> {
        let secure = match url.scheme() {
            "ws" => false,
            "wss" => true,
            _ => return Err(format!("{url} is not a WebSocket address")),
        };

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

        // What the page will name itself as. A document with no origin of its
        // own, a `file:` page, sends the literal `null`, which is what the
        // spec says and what a server has to allow deliberately rather than by
        // accident.
        let origin = document.map(|doc| match crate::cors::Origin::of(doc) {
            Some(origin) => origin.header(),
            None => "null".to_string(),
        });

        let host = url.host_str().ok_or_else(|| format!("{url} has no host"))?;
        let port = url.port().unwrap_or(if secure { 443 } else { 80 });
        // The addresses `authorise_socket` already checked, not a second
        // lookup. `Policy::check_address` decided about *these*; resolving the
        // name again here would reopen exactly the window the pinning resolver
        // closes for the HTTP client, and this is the one client that does its
        // own connecting. `None` only when nothing is pinned at all, an egress
        // proxy in the path, and a proxied session reaches loopback only.
        let sock = match broker.approved_addresses(host) {
            Some(addrs) => connect_to_any(&addrs)
                .map_err(|e| format!("could not reach {host}:{port}: {e}"))?,
            None => TcpStream::connect((host, port))
                .map_err(|e| format!("could not reach {host}:{port}: {e}"))?,
        };
        sock.set_read_timeout(Some(HANDSHAKE_TIMEOUT))
            .map_err(|e| e.to_string())?;

        let stream = if secure {
            // The name is checked against the certificate, so an address in the
            // URL is refused here rather than connected to without validation.
            let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
                .map_err(|_| format!("`{host}` is not a name a certificate can be checked against"))?;
            let conn = rustls::ClientConnection::new(tls_config(), server_name)
                .map_err(|e| format!("could not start TLS with {host}: {e}"))?;
            Wire::tls(sock, conn)
        } else {
            Wire::plain(sock)
        };

        // One reader for the handshake *and* the frames that follow it.
        //
        // This was a bug before it was a design note. A `BufReader` reads
        // ahead, so the one used for the handshake headers had already pulled
        // the server's first data frame into its buffer, and dropping it threw
        // that frame away. A server that greets on connect (which is what a
        // hot-reload channel does) looked like a server that never spoke.
        let reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
        let reader = handshake(reader, &stream, url, host, port, origin.as_deref())?;

        // Reads have no deadline once the handshake is done: a socket that is
        // quiet is not a socket that is broken, and a dev server's HMR channel
        // is quiet almost all the time.
        //
        // Except on TLS, where the timeout is not a deadline but the mechanism
        // that lets a writer take the connection lock. See [`Wire`].
        stream.set_read_timeout(if stream.is_tls() {
            Some(TLS_READ_SLICE)
        } else {
            None
        });

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

        let guard = self.out.lock().map_err(|_| "socket lock poisoned")?;
        let Some(stream) = guard.as_ref() else {
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
            && let Some(stream) = guard.as_ref()
        {
            let _ = send_masked(stream, 0x8, &[]);
            stream.shutdown();
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
    /// that is not one. A log that misdescribes the protocol, in an engine
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

impl crate::broker::Channel for Socket {
    fn send(&self, text: &str) -> Result<(), String> {
        Socket::send(self, text)
    }

    fn drain(&self) -> Vec<Event> {
        Socket::drain(self)
    }

    fn close(&self) {
        Socket::close(self)
    }
}

/// Read frames until the socket ends, receipting each one.
fn read_loop(
    mut reader: BufReader<Wire>,
    tx: SyncSender<Event>,
    broker: Arc<LocalBroker>,
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

/// `read_line` with a cap, because the peer chooses when the newline arrives.
///
/// Reads a byte at a time, which is what a `BufReader` makes cheap and what
/// keeps the reader's buffered bytes (the first frame of the stream, which
/// [`Socket::open`] documents must not be dropped) exactly where they were.
fn read_line_capped(reader: &mut BufReader<Wire>, out: &mut String) -> std::io::Result<usize> {
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
                if bytes.len() > MAX_HANDSHAKE_LINE {
                    return Err(std::io::Error::other(
                        "a line in the server's handshake exceeded the engine's cap",
                    ));
                }
            }
        }
    }
    let taken = bytes.len();
    out.push_str(&String::from_utf8_lossy(&bytes));
    Ok(taken)
}


/// The opening handshake.
///
/// Takes the reader and gives it back, because whatever it buffered past the
/// headers is the beginning of the frame stream and must not be dropped.
fn handshake(
    mut reader: BufReader<Wire>,
    stream: &Wire,
    url: &Url,
    host: &str,
    port: u16,
    origin: Option<&str>,
) -> Result<BufReader<Wire>, String> {
    let mut key_bytes = [0u8; 16];
    getrandom::getrandom(&mut key_bytes)
        .map_err(|e| format!("could not generate a handshake key: {e}"))?;
    let key = base64_encode(&key_bytes);

    let path = match url.query() {
        Some(query) => format!("{}?{}", url.path(), query),
        None => url.path().to_string(),
    };
    // The `Origin` a browser would send, and the reason it matters here: CORS
    // does not apply to a WebSocket, so `Origin` is the *only* thing a server
    // has to tell a page's socket from a program's. Omitting it made every
    // socket this engine opened on a page's behalf look like a non-browser
    // client, which is precisely the shape a cross-site WebSocket hijack takes.
    // The server's one defence, silently unarmed. Absent for a socket the
    // agent named itself, because there is no document behind that one.
    let origin_line = match origin {
        Some(origin) => format!("Origin: {origin}\r\n"),
        None => String::new(),
    };
    let request = format!(
        "GET {path} HTTP/1.1\r\n\
         Host: {host}:{port}\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         {origin_line}\
         Sec-WebSocket-Key: {key}\r\n\
         Sec-WebSocket-Version: 13\r\n\
         \r\n"
    );

    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("could not send the handshake: {e}"))?;

    let mut status = String::new();
    read_line_capped(&mut reader, &mut status)
        .map_err(|e| format!("no answer to the handshake: {e}"))?;
    if !status.contains("101") {
        return Err(format!(
            "the server did not upgrade the connection: {}",
            status.trim()
        ));
    }

    let expected = ws::accept_key(&key);
    let mut accepted = false;
    let mut lines = 0usize;
    loop {
        lines += 1;
        if lines > MAX_HANDSHAKE_LINES {
            return Err(format!(
                "the server sent more than {MAX_HANDSHAKE_LINES} handshake headers, which is \
                 not a handshake"
            ));
        }
        let mut line = String::new();
        let read = read_line_capped(&mut reader, &mut line)
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
fn send_masked(stream: &Wire, opcode: u8, payload: &[u8]) -> Result<(), H5iError> {
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

    // One write, not two. On a plain socket the difference is only a syscall;
    // on TLS each write is its own record, so splitting the header from the
    // payload would put every frame on the wire as two records and hand a
    // passive observer the frame boundaries for free.
    let mut frame = Vec::with_capacity(header.len() + masked.len());
    frame.extend_from_slice(&header);
    frame.extend_from_slice(&masked);
    stream.write_all(&frame).map_err(H5iError::Io)?;
    Ok(())
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
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

    /// `wss://` is a scheme this engine has, so it must fail on *policy* like
    /// any other address rather than on the scheme itself. The refusal that
    /// used to stand here was a capability gap dressed as an architectural
    /// property; what is left is the allowlist doing its job.
    #[test]
    fn wss_is_a_scheme_this_engine_has_and_is_judged_by_the_allowlist() {
        let broker = crate::net::LocalBroker::new(
                crate::policy::Policy::new(),
                Arc::new(crate::receipt::MemorySink::new()),
                None,
            )
            .expect("broker");
        let url = Url::parse("wss://example.com/socket").unwrap();
        let error = match Socket::open(broker, &url, None) {
            Err(error) => error,
            Ok(_) => panic!("an empty allowlist should have refused this"),
        };
        assert!(
            error.contains("denied by policy"),
            "the refusal should be the allowlist, not the scheme: {error}"
        );
        assert!(
            !error.contains("not built"),
            "`wss://` is built now: {error}"
        );
    }

    /// The scheme check still refuses what is genuinely not a socket address,
    /// and says which schemes are.
    #[test]
    fn a_non_socket_scheme_is_still_refused() {
        let broker = crate::net::LocalBroker::new(
                crate::policy::Policy::new(),
                Arc::new(crate::receipt::MemorySink::new()),
                None,
            )
            .expect("broker");
        let url = Url::parse("https://example.com/socket").unwrap();
        let error = match Socket::open(broker, &url, None) {
            Err(error) => error,
            Ok(_) => panic!("https is not a WebSocket address"),
        };
        assert!(error.contains("not a WebSocket address"), "{error}");
    }

    /// TLS changes the transport, not the containment rule. A remote socket
    /// behind an egress proxy is refused whether or not it is encrypted,
    /// because the objection was always that a raw socket does not go through
    /// the proxy.
    #[test]
    fn tls_does_not_buy_a_way_past_the_proxy_rule() {
        let broker = crate::net::LocalBroker::new(
                crate::policy::Policy::new().allow_all_of(&["example.com".to_string()]),
                Arc::new(crate::receipt::MemorySink::new()),
                Some("http://127.0.0.1:9"),
            )
            .expect("broker");
        let url = Url::parse("wss://example.com/socket").unwrap();
        let error = match Socket::open(broker, &url, None) {
            Err(error) => error,
            Ok(_) => panic!("a remote socket behind a proxy should be refused"),
        };
        assert!(error.contains("egress proxy"), "{error}");
    }

    /// The address behind the name is checked on this path too.
    ///
    /// `pin_addresses` used to return early for `ws`/`wss`, so
    /// `Policy::check_address`, the rebinding and private-space guard, never
    /// ran on the one client that does its own `TcpStream::connect`. The
    /// instrument mode makes the gap visible without a DNS server: it grants
    /// every remote *name* and deliberately grants no private address, so a
    /// socket to one must still be refused.
    #[test]
    fn a_socket_into_private_space_is_refused_even_when_every_name_is_granted() {
        let broker = crate::net::LocalBroker::new(
            crate::policy::Policy::new().set_any_remote(true),
            Arc::new(crate::receipt::MemorySink::new()),
            None,
        )
        .expect("broker");
        let url = Url::parse("ws://10.0.0.1:8080/socket").unwrap();
        let error = match Socket::open(broker, &url, None) {
            Err(error) => error,
            Ok(_) => panic!("a socket into RFC 1918 space should have been refused"),
        };
        assert!(
            error.contains("internal address") || error.contains("not in the allowlist"),
            "the address check should have refused this: {error}"
        );
    }

    /// The server half of this protocol bounds its own handshake; the client
    /// half read the answer of whatever it had just dialled with no bound at
    /// all, which is the wrong way round, because the answer is the part
    /// written by someone else. A server that answers `101` and then sends
    /// bytes without a newline is an unbounded allocation on this side.
    #[test]
    fn a_server_cannot_grow_this_side_with_a_handshake_that_never_ends() {
        use std::io::{Read as _, Write as _};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(b"HTTP/1.1 101 Switching Protocols\r\n");
                // One header line that never ends. Bounded on this side or the
                // reader grows a `String` for as long as the peer keeps typing.
                let filler = "x".repeat(4096);
                for _ in 0..64 {
                    if stream.write_all(filler.as_bytes()).is_err() {
                        return;
                    }
                }
                let _ = stream.flush();
            }
        });

        let broker = crate::net::LocalBroker::new(
            crate::policy::Policy::new(),
            Arc::new(crate::receipt::MemorySink::new()),
            None,
        )
        .expect("broker");
        let url = Url::parse(&format!("ws://127.0.0.1:{port}/hmr")).unwrap();
        let error = match Socket::open(broker, &url, None) {
            Err(error) => error,
            Ok(_) => panic!("a handshake that never ends must not open a socket"),
        };
        assert!(error.contains("cap"), "{error}");
        let _ = server.join();
    }

    /// A page's socket names the page. CORS does not reach a WebSocket, so
    /// `Origin` is the only thing a server has to tell a page's socket from a
    /// program's, and this engine used to send none, which unarmed the one
    /// defence a WebSocket server has against a cross-site hijack.
    #[test]
    fn a_socket_opened_by_a_page_carries_the_page_origin() {
        use std::io::{Read as _, Write as _};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(String::new()));
        let recorder = seen.clone();
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 2048];
                let read = stream.read(&mut buf).unwrap_or(0);
                if let Ok(mut slot) = recorder.lock() {
                    *slot = String::from_utf8_lossy(&buf[..read]).to_string();
                }
                // Not a real upgrade: the handshake is expected to fail, and
                // what is under test is what went *out*.
                let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n");
                let _ = stream.flush();
            }
        });

        let broker = crate::net::LocalBroker::new(
            crate::policy::Policy::new(),
            Arc::new(crate::receipt::MemorySink::new()),
            None,
        )
        .expect("broker");
        let url = Url::parse(&format!("ws://127.0.0.1:{port}/hmr")).unwrap();
        let document = Url::parse("http://127.0.0.1:5173/index.html").unwrap();
        let _ = Socket::open(broker, &url, Some(&document));
        let _ = server.join();

        let request = seen.lock().map(|s| s.clone()).unwrap_or_default();
        assert!(
            request.contains("Origin: http://127.0.0.1:5173\r\n"),
            "the handshake should name the document that asked:\n{request}"
        );
    }

    /// And a socket the *agent* named carries none, because there is no
    /// document behind it to name.
    #[test]
    fn a_socket_the_agent_named_has_no_document_to_declare() {
        use std::io::{Read as _, Write as _};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(String::new()));
        let recorder = seen.clone();
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 2048];
                let read = stream.read(&mut buf).unwrap_or(0);
                if let Ok(mut slot) = recorder.lock() {
                    *slot = String::from_utf8_lossy(&buf[..read]).to_string();
                }
                let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n");
                let _ = stream.flush();
            }
        });

        let broker = crate::net::LocalBroker::new(
            crate::policy::Policy::new(),
            Arc::new(crate::receipt::MemorySink::new()),
            None,
        )
        .expect("broker");
        let url = Url::parse(&format!("ws://127.0.0.1:{port}/hmr")).unwrap();
        let _ = Socket::open(broker, &url, None);
        let _ = server.join();

        let request = seen.lock().map(|s| s.clone()).unwrap_or_default();
        assert!(!request.contains("Origin:"), "{request}");
    }

    #[test]
    fn a_remote_socket_is_refused_while_a_proxy_is_in_the_path() {
        // The containment point. A WebSocket is a raw socket and does not go
        // through the proxy, so opening a remote one would step around the
        // allowlist the proxy enforces inside a box.
        let broker = crate::net::LocalBroker::new(
                crate::policy::Policy::new().allow_all_of(&["example.com".to_string()]),
                Arc::new(crate::receipt::MemorySink::new()),
                Some("http://127.0.0.1:3128"),
            )
            .expect("broker");
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

        let client = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        send_masked(&Wire::plain(client), 0x1, b"hello socket").expect("send");

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

        let client = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        send_masked(&Wire::plain(client), 0x1, payload.as_bytes()).expect("send");

        match server.join().expect("joined") {
            Incoming::Text(text) => assert_eq!(text, expected),
            other => panic!("expected text, got {other:?}"),
        }
    }
}
