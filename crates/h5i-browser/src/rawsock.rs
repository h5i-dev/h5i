//! Plain and TLS sockets shared by WebSocket and raw HTTP clients.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use url::Url;

/// How long a connect attempt waits before moving to the next address.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a TLS reader holds the shared connection lock while idle.
pub(crate) const TLS_READ_SLICE: Duration = Duration::from_millis(100);

/// A plain or TLS socket.
pub(crate) struct Wire {
    sock: TcpStream,
    /// `None` for a plain socket. Shared with a reader thread when one exists.
    tls: Option<Arc<Mutex<rustls::ClientConnection>>>,
}

impl Wire {
    pub(crate) fn plain(sock: TcpStream) -> Self {
        Self { sock, tls: None }
    }

    pub(crate) fn tls(sock: TcpStream, conn: rustls::ClientConnection) -> Self {
        Self {
            sock,
            tls: Some(Arc::new(Mutex::new(conn))),
        }
    }

    pub(crate) fn is_tls(&self) -> bool {
        self.tls.is_some()
    }

    /// Clone the socket for a reader thread.
    pub(crate) fn try_clone(&self) -> std::io::Result<Wire> {
        Ok(Wire {
            sock: self.sock.try_clone()?,
            tls: self.tls.clone(),
        })
    }

    /// Write and flush all bytes.
    pub(crate) fn write_all(&self, data: &[u8]) -> std::io::Result<()> {
        match &self.tls {
            None => {
                let mut sock = &self.sock;
                sock.write_all(data)?;
                sock.flush()
            }
            Some(conn) => {
                let mut conn = conn
                    .lock()
                    .map_err(|_| std::io::Error::other("the tls connection lock is poisoned"))?;
                let mut sock = &self.sock;
                let mut stream = rustls::Stream::new(&mut *conn, &mut sock);
                stream.write_all(data)?;
                stream.flush()
            }
        }
    }

    pub(crate) fn shutdown(&self) {
        let _ = self.sock.shutdown(std::net::Shutdown::Both);
    }

    pub(crate) fn set_read_timeout(&self, timeout: Option<Duration>) {
        let _ = self.sock.set_read_timeout(timeout);
    }
}

impl Read for Wire {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let Some(conn) = self.tls.clone() else {
            return (&self.sock).read(buf);
        };
        loop {
            {
                let mut conn = conn
                    .lock()
                    .map_err(|_| std::io::Error::other("the tls connection lock is poisoned"))?;
                let mut sock = &self.sock;
                let mut stream = rustls::Stream::new(&mut *conn, &mut sock);
                match stream.read(buf) {
                    Ok(read) => return Ok(read),
                    // Release the lock after an idle slice so writers can use it.
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) => {}
                    Err(error) => return Err(error),
                }
            }
            std::thread::yield_now();
        }
    }
}

/// Return the cached TLS configuration with bundled Mozilla roots.
pub(crate) fn tls_config() -> Arc<rustls::ClientConfig> {
    use std::sync::OnceLock;
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let roots = rustls::RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            };
            Arc::new(
                rustls::ClientConfig::builder()
                    .with_root_certificates(roots)
                    .with_no_client_auth(),
            )
        })
        .clone()
}

/// Connect to the first address that answers.
pub(crate) fn connect_to_any(addrs: &[SocketAddr]) -> std::io::Result<TcpStream> {
    let mut last = None;
    for addr in addrs {
        match TcpStream::connect_timeout(addr, CONNECT_TIMEOUT) {
            Ok(sock) => return Ok(sock),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::AddrNotAvailable,
            "the host resolved to no addresses",
        )
    }))
}

/// Return whether the URL names a loopback host.
pub(crate) fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(name)) => {
            name.eq_ignore_ascii_case("localhost") || name.eq_ignore_ascii_case("localhost.")
        }
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// Dial a URL using pinned, policy-approved addresses when provided.
///
/// Reusing pinned addresses prevents DNS rebinding. TLS validates the URL's host.
pub(crate) fn dial(url: &Url, approved: Option<Vec<SocketAddr>>) -> Result<Wire, String> {
    let secure = match url.scheme() {
        "http" => false,
        "https" => true,
        other => return Err(format!("{other}:// is not a scheme the raw sender speaks")),
    };
    let host = url.host_str().ok_or_else(|| format!("{url} has no host"))?;
    let port = url.port_or_known_default().unwrap_or(if secure { 443 } else { 80 });

    let sock = match approved {
        Some(addrs) => {
            connect_to_any(&addrs).map_err(|e| format!("could not reach {host}:{port}: {e}"))?
        }
        None => TcpStream::connect((host, port))
            .map_err(|e| format!("could not reach {host}:{port}: {e}"))?,
    };

    if secure {
        let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
            .map_err(|_| format!("`{host}` is not a name a certificate can be checked against"))?;
        let conn = rustls::ClientConnection::new(tls_config(), server_name)
            .map_err(|e| format!("could not start TLS with {host}: {e}"))?;
        Ok(Wire::tls(sock, conn))
    } else {
        Ok(Wire::plain(sock))
    }
}

/// One HTTP/1.1 response, read off a raw socket.
pub(crate) struct RawResponse {
    pub status: Option<u16>,
    pub headers: Vec<(String, String)>,
    /// The body, decoded from chunked framing when needed.
    pub body: Vec<u8>,
    /// Bytes that had already arrived past the end of this response.
    ///
    /// A response's framing says where it stops, and a socket does not: the
    /// next message can be in the same read. Kept rather than truncated away,
    /// because for a raw send those bytes are the interesting half.
    pub leftover: Vec<u8>,
}

/// Anything the connection still had to say once one response was complete.
///
/// A well-behaved server has nothing: one request, one response. Two responses
/// to one request is the signature of a desync, and it is the *second* one that
/// carries the answer — the smuggled request's. Reading only the first would
/// make `--raw-request` a way to perform a request-smuggling attack and not see
/// its result, which is most of the point of performing one.
///
/// Bounded and short: this is a server that has already answered, so the wait
/// is for bytes that are either there or are not.
pub(crate) fn read_whatever_follows(wire: &mut Wire, already: Vec<u8>, cap: usize) -> Vec<u8> {
    let restore = std::time::Duration::from_secs(30);
    wire.set_read_timeout(Some(std::time::Duration::from_millis(500)));
    let mut rest = already;
    let mut chunk = [0u8; 8192];
    while rest.len() < cap {
        match wire.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => rest.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
    }
    wire.set_read_timeout(Some(restore));
    rest.truncate(cap);
    rest
}

/// Read one bounded HTTP/1.1 response using its length, chunks, or connection close.
pub(crate) fn read_http_response(wire: &mut Wire, cap: usize) -> Result<RawResponse, String> {
    let mut buf: Vec<u8> = Vec::with_capacity(8192);
    let mut chunk = [0u8; 8192];

    // Read the head through its terminating blank line.
    let head_end = loop {
        if let Some(at) = find_subslice(&buf, b"\r\n\r\n") {
            break at + 4;
        }
        if buf.len() > cap {
            return Err(format!("response head exceeded the {cap} byte cap without ending"));
        }
        match wire.read(&mut chunk) {
            Ok(0) => return Err("the connection closed before the response head was complete".into()),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if is_timeout(&e) => {
                return Err("timed out reading the response head".into());
            }
            Err(e) => return Err(format!("reading the response failed: {e}")),
        }
    };

    let (status, headers) = parse_head(&buf[..head_end]);
    let mut rest = buf.split_off(head_end); // whatever body bytes already arrived

    // Read the body using the declared framing.
    let te_chunked = headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("transfer-encoding")
            && value.to_ascii_lowercase().split(',').any(|v| v.trim() == "chunked")
    });
    let content_length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok());

    let (body, leftover) = if te_chunked {
        read_chunked(wire, &mut rest, &mut chunk, cap)?
    } else if let Some(len) = content_length {
        read_exact_len(wire, rest, &mut chunk, len.min(cap))?
    } else {
        // Without framing, read until the connection closes or the cap is
        // reached. Nothing can follow a response that ends at the close.
        (read_to_close(wire, rest, &mut chunk, cap)?, Vec::new())
    };

    Ok(RawResponse {
        status,
        headers,
        body,
        leftover,
    })
}

/// The body, and whatever had already arrived behind it.
fn read_exact_len(
    wire: &mut Wire,
    mut have: Vec<u8>,
    chunk: &mut [u8],
    want: usize,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    while have.len() < want {
        match wire.read(chunk) {
            Ok(0) => break,
            Ok(n) => have.extend_from_slice(&chunk[..n]),
            Err(e) if is_timeout(&e) => break,
            Err(e) => return Err(format!("reading the response body failed: {e}")),
        }
    }
    let leftover = have.split_off(want.min(have.len()));
    Ok((have, leftover))
}

fn read_to_close(
    wire: &mut Wire,
    mut have: Vec<u8>,
    chunk: &mut [u8],
    cap: usize,
) -> Result<Vec<u8>, String> {
    while have.len() < cap {
        match wire.read(chunk) {
            Ok(0) => break,
            Ok(n) => have.extend_from_slice(&chunk[..n]),
            Err(e) if is_timeout(&e) => break,
            Err(e) => return Err(format!("reading the response body failed: {e}")),
        }
    }
    have.truncate(cap);
    Ok(have)
}

/// Decode a chunked body, reading more as needed.
fn read_chunked(
    wire: &mut Wire,
    have: &mut Vec<u8>,
    chunk: &mut [u8],
    cap: usize,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let mut out: Vec<u8> = Vec::new();
    let mut cursor = 0usize;
    loop {
        // Parse the hexadecimal size and ignore chunk extensions.
        let line_end = loop {
            if let Some(rel) = find_subslice(&have[cursor..], b"\r\n") {
                break cursor + rel;
            }
            if have.len() > cap {
                return Err(format!("chunked response exceeded the {cap} byte cap"));
            }
            if !fill(wire, have, chunk)? {
                return Err("the connection closed inside a chunk size line".into());
            }
        };
        let size_field = &have[cursor..line_end];
        let size_text = std::str::from_utf8(size_field)
            .map_err(|_| "a chunk size was not text".to_string())?;
        let size_hex = size_text.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_hex, 16)
            .map_err(|_| format!("`{size_hex}` is not a chunk size"))?;
        // A chunk cannot be bigger than the whole response is allowed to be,
        // and a size that says otherwise is not a body to read: it is
        // arithmetic to break. `cursor + size + 2` wraps in a release build, so
        // a size near `usize::MAX` makes the fill loop below decide it already
        // has enough and then slice a range that runs backwards, which panics.
        // Refused where the number is read, so every sum after this point is
        // bounded by the cap.
        if size > cap {
            return Err(format!("chunked response exceeded the {cap} byte cap"));
        }
        cursor = line_end + 2;
        if size == 0 {
            break; // the last chunk; trailers, if any, are ignored
        }
        // Read the chunk and its trailing CRLF.
        while have.len() < cursor + size + 2 {
            if out.len() + size > cap {
                return Err(format!("chunked response exceeded the {cap} byte cap"));
            }
            if !fill(wire, have, chunk)? {
                return Err("the connection closed inside a chunk body".into());
            }
        }
        out.extend_from_slice(&have[cursor..cursor + size]);
        cursor += size + 2;
        if out.len() > cap {
            return Err(format!("chunked response exceeded the {cap} byte cap"));
        }
    }
    // The last chunk is followed by the trailer section's blank line. Step over
    // it when it is already here; what remains belongs to the next message.
    if have.get(cursor..cursor + 2) == Some(b"\r\n") {
        cursor += 2;
    }
    let leftover = have.get(cursor..).unwrap_or(&[]).to_vec();
    Ok((out, leftover))
}

/// Read one more read's worth into `have`. `false` on a clean close.
fn fill(wire: &mut Wire, have: &mut Vec<u8>, chunk: &mut [u8]) -> Result<bool, String> {
    match wire.read(chunk) {
        Ok(0) => Ok(false),
        Ok(n) => {
            have.extend_from_slice(&chunk[..n]);
            Ok(true)
        }
        Err(e) if is_timeout(&e) => Ok(false),
        Err(e) => Err(format!("reading the response body failed: {e}")),
    }
}

fn parse_head(head: &[u8]) -> (Option<u16>, Vec<(String, String)>) {
    let text = String::from_utf8_lossy(head);
    let mut lines = text.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok());
    let headers = lines
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_string(), value.trim().to_string()))
        })
        .collect();
    (status, headers)
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn is_timeout(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_head_is_parsed_into_status_and_headers() {
        let head = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 3\r\n\r\n";
        let (status, headers) = parse_head(head);
        assert_eq!(status, Some(200));
        assert!(headers
            .iter()
            .any(|(n, v)| n == "Content-Type" && v == "text/plain"));
        assert!(headers.iter().any(|(n, v)| n == "Content-Length" && v == "3"));
    }

    #[test]
    fn find_subslice_locates_the_header_terminator() {
        assert_eq!(find_subslice(b"ab\r\n\r\ncd", b"\r\n\r\n"), Some(2));
        assert_eq!(find_subslice(b"abcd", b"\r\n\r\n"), None);
    }

    /// Two responses to one request is the whole signal a desync produces, and
    /// the second one is the smuggled request's. A reader that stops at the end
    /// of the first response's framing and drops the rest of what it had
    /// already read would make a successful attack indistinguishable from a
    /// failed one.
    #[test]
    fn a_second_response_in_the_same_read_is_kept_rather_than_truncated() {
        let two = concat!(
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi",
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nflag!",
        );
        let response = read_from_a_server_that_says(two);
        assert_eq!(response.body, b"hi");
        assert!(
            String::from_utf8_lossy(&response.leftover).contains("flag!"),
            "{:?}",
            String::from_utf8_lossy(&response.leftover)
        );
    }

    /// The same, for the framing the benchmark's backend actually uses.
    #[test]
    fn a_chunked_response_hands_back_what_followed_its_terminator() {
        let two = concat!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nhi\r\n0\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nflag!",
        );
        let response = read_from_a_server_that_says(two);
        assert_eq!(response.body, b"hi");
        assert!(
            String::from_utf8_lossy(&response.leftover).starts_with("HTTP/1.1 200"),
            "{:?}",
            String::from_utf8_lossy(&response.leftover)
        );
    }

    /// A chunk size is a number a server chooses, and one near `usize::MAX`
    /// used to reach `&have[cursor..cursor + size]` with the sum wrapped past
    /// zero: a range running backwards, which panics. A hostile target could
    /// therefore end the session by answering a raw send.
    #[test]
    fn a_chunk_size_that_would_overflow_is_refused_rather_than_sliced() {
        let hostile = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nffffffffffffffee\r\n";
        let outcome = try_reading_from_a_server_that_says(hostile);
        assert!(
            outcome.is_err(),
            "a chunk larger than the cap has to be refused, got {:?}",
            outcome.map(|r| r.body)
        );
    }

    /// One connection, one write, whatever bytes the test names.
    fn read_from_a_server_that_says(bytes: &str) -> RawResponse {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let said = bytes.to_string();
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.write_all(said.as_bytes());
                let _ = stream.flush();
                // Held open, so the reader has to stop on the framing rather
                // than on the close.
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        });
        let sock =
            std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
        sock.set_read_timeout(Some(std::time::Duration::from_secs(5))).expect("timeout");
        let mut wire = Wire::plain(sock);
        let response = read_http_response(&mut wire, 1 << 20).expect("a response");
        let _ = server.join();
        response
    }

    /// The same server, for the cases where the refusal is the answer.
    fn try_reading_from_a_server_that_says(bytes: &str) -> Result<RawResponse, String> {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let said = bytes.to_string();
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.write_all(said.as_bytes());
                let _ = stream.flush();
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        });
        let sock = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
        sock.set_read_timeout(Some(std::time::Duration::from_secs(5))).expect("timeout");
        let mut wire = Wire::plain(sock);
        let response = read_http_response(&mut wire, 1 << 20);
        let _ = server.join();
        response
    }
}
