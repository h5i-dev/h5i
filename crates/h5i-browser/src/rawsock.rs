//! The raw socket transport, shared by the WebSocket client and the raw HTTP
//! sender.
//!
//! Both of them go around `reqwest` for the same reason: they need to put bytes
//! on the wire that a parsed `Url` and a built request cannot express. A
//! WebSocket needs the connection kept open after the handshake; a raw HTTP send
//! needs a request-target the URL standard would rewrite before it existed (a
//! `.%2e/` traversal, a smuggled framing header). Neither has a home inside the
//! client, and both need exactly the same thing underneath: a plain or TLS
//! socket, dialled at an address the policy already approved.
//!
//! [`Wire`] is that socket. It was `wsclient.rs`'s alone; the raw HTTP path is
//! the second caller that made it worth its own module rather than a second
//! copy of the TLS bookkeeping.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use url::Url;

/// How long a connect attempt waits before moving to the next address.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a TLS read waits before releasing the connection lock.
///
/// Short enough that a writer never feels it, long enough not to spin. Only TLS
/// sockets set a read timeout at all: a plain socket has two independent handles
/// and needs none.
pub(crate) const TLS_READ_SLICE: Duration = Duration::from_millis(100);

/// The bytes underneath a socket, plain or encrypted.
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

    /// A second handle on the same connection, for a reader thread.
    pub(crate) fn try_clone(&self) -> std::io::Result<Wire> {
        Ok(Wire {
            sock: self.sock.try_clone()?,
            tls: self.tls.clone(),
        })
    }

    /// Write, and make sure it has actually left.
    ///
    /// `&self` rather than `&mut self` because both halves of a WebSocket hold
    /// one of these, and a frame written from the page's thread must not need
    /// exclusive ownership of the connection.
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
                    // The slice expired with nothing to show for it. The lock is
                    // dropped here, which is the whole point of the timeout, and
                    // then we ask again.
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

/// The trust store, built once.
///
/// Mozilla's roots, compiled in, for the same reason the public suffix list is:
/// a decision about who to trust should not depend on the network being
/// reachable or on a file the box may not have.
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

/// Whether a URL names a loopback host, spelled as a name or an address.
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

/// Dial a socket for a URL, at addresses the policy already approved.
///
/// `approved` is the pinned address set: the ones `Policy::check_address` said
/// yes to, handed down rather than resolved again here, so this client does not
/// reopen the rebinding window the pinning resolver closes for `reqwest`. `None`
/// means nothing is pinned (a bare host with no egress proxy), and the name is
/// resolved by `TcpStream::connect` as a last resort.
///
/// The TLS name is checked against the certificate, so an address literal in a
/// `https` URL is refused here rather than connected to without validation.
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
    /// The decoded body: de-chunked when the response was chunked, and the exact
    /// bytes otherwise.
    pub body: Vec<u8>,
}

/// Read one HTTP/1.1 response, honouring the framing the server chose.
///
/// A raw send cannot lean on `reqwest` to know where the message ends, so this
/// reads the framing itself: `Content-Length`, then `Transfer-Encoding:
/// chunked`, then a close as the length of last resort. `cap` bounds the whole
/// read, so a server that never stops talking cannot become this process's
/// memory ceiling. The caller sets the socket's read timeout, which is what ends
/// a read that is waiting on bytes that are not coming.
pub(crate) fn read_http_response(wire: &mut Wire, cap: usize) -> Result<RawResponse, String> {
    let mut buf: Vec<u8> = Vec::with_capacity(8192);
    let mut chunk = [0u8; 8192];

    // 1. The head: read until the blank line that ends it.
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

    // 2. The body, framed the way the headers say it is.
    let te_chunked = headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("transfer-encoding")
            && value.to_ascii_lowercase().split(',').any(|v| v.trim() == "chunked")
    });
    let content_length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok());

    let body = if te_chunked {
        read_chunked(wire, &mut rest, &mut chunk, cap)?
    } else if let Some(len) = content_length {
        read_exact_len(wire, rest, &mut chunk, len.min(cap))?
    } else {
        // No framing at all: the message ends when the connection does. Read to
        // the cap or the close, whichever comes first.
        read_to_close(wire, rest, &mut chunk, cap)?
    };

    Ok(RawResponse {
        status,
        headers,
        body,
    })
}

fn read_exact_len(
    wire: &mut Wire,
    mut have: Vec<u8>,
    chunk: &mut [u8],
    want: usize,
) -> Result<Vec<u8>, String> {
    while have.len() < want {
        match wire.read(chunk) {
            Ok(0) => break,
            Ok(n) => have.extend_from_slice(&chunk[..n]),
            Err(e) if is_timeout(&e) => break,
            Err(e) => return Err(format!("reading the response body failed: {e}")),
        }
    }
    have.truncate(want);
    Ok(have)
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
) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::new();
    let mut cursor = 0usize;
    loop {
        // The size line: hex digits up to a CRLF, ignoring any chunk extension.
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
        cursor = line_end + 2;
        if size == 0 {
            break; // the last chunk; trailers, if any, are ignored
        }
        // The chunk data plus its trailing CRLF.
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
    Ok(out)
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
}
