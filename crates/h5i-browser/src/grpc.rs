//! The gRPC transport: one HTTP/2 request, its response frames and trailers.
//!
//! Opaque to protobuf. It sends the framed body it is given and returns the
//! framed body it gets back, plus the `grpc-status` trailer that says whether
//! the call succeeded. Meaning is `h5i-grpc`'s job, out in the plugin; this is
//! only the socket, so it stays in the engine where the network belongs.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;

/// One call to make.
pub struct GrpcRequest {
    /// `http://host:50051` or `https://host:443`. The scheme picks h2c vs TLS.
    pub url: String,
    /// The `:path`, `/package.Service/Method`.
    pub path: String,
    /// Overrides the `:authority`; defaults to the URL's host and port.
    pub authority: Option<String>,
    /// The already-framed request body (one or more length-prefixed messages).
    pub body: Vec<u8>,
    /// Extra request headers: gRPC metadata.
    pub metadata: Vec<(String, String)>,
    /// Vetted addresses to dial, from the broker's address pinning. Empty falls
    /// back to a DNS lookup of the URL's host.
    pub addrs: Vec<std::net::SocketAddr>,
    /// Skip TLS certificate verification (for a self-signed target under test).
    pub insecure: bool,
    /// Give up after this long.
    pub timeout: Option<Duration>,
}

/// What the wire returned.
pub struct GrpcReply {
    pub http_status: u16,
    pub grpc_status: Option<i32>,
    pub grpc_message: Option<String>,
    /// The framed response body (several frames for a server-streaming reply).
    pub body: Vec<u8>,
}

/// Run one call to completion on a private single-thread runtime.
pub fn call_blocking(req: GrpcRequest) -> Result<GrpcReply, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("could not start the gRPC runtime: {e}"))?;
    let timeout = req.timeout;
    runtime.block_on(async move {
        match timeout {
            Some(limit) => tokio::time::timeout(limit, call(req))
                .await
                .map_err(|_| "the gRPC call timed out".to_string())?,
            None => call(req).await,
        }
    })
}

async fn call(req: GrpcRequest) -> Result<GrpcReply, String> {
    let uri: http::Uri = req
        .url
        .parse()
        .map_err(|e| format!("`{}` is not a URL: {e}", req.url))?;
    let scheme = uri.scheme_str().unwrap_or("http");
    let tls = match scheme {
        "https" | "grpcs" => true,
        "http" | "grpc" => false,
        other => return Err(format!("gRPC needs an http(s) URL, not `{other}`")),
    };
    let host = uri
        .host()
        .ok_or_else(|| format!("`{}` has no host", req.url))?
        .to_string();
    let port = uri.port_u16().unwrap_or(if tls { 443 } else { 80 });
    let authority = req.authority.clone().unwrap_or_else(|| format!("{host}:{port}"));

    let tcp = if req.addrs.is_empty() {
        tokio::net::TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|e| format!("could not connect to {host}:{port}: {e}"))?
    } else {
        connect_pinned(&req.addrs).await?
    };
    let _ = tcp.set_nodelay(true);

    if tls {
        let stream = tls_connect(tcp, &host, req.insecure).await?;
        drive(stream, &authority, tls, &req).await
    } else {
        drive(tcp, &authority, tls, &req).await
    }
}

/// Dial the first vetted address that accepts, so the connection lands on an IP
/// the broker already decided about.
async fn connect_pinned(
    addrs: &[std::net::SocketAddr],
) -> Result<tokio::net::TcpStream, String> {
    let mut last = String::from("no addresses to dial");
    for addr in addrs {
        match tokio::net::TcpStream::connect(addr).await {
            Ok(tcp) => return Ok(tcp),
            Err(e) => last = format!("could not connect to {addr}: {e}"),
        }
    }
    Err(last)
}

/// The HTTP/2 exchange, over whichever transport was set up.
async fn drive<T>(io: T, authority: &str, tls: bool, req: &GrpcRequest) -> Result<GrpcReply, String>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (send_request, connection) = h2::client::handshake(io)
        .await
        .map_err(|e| format!("the HTTP/2 handshake failed: {e}"))?;
    // The connection must be driven for the streams to make progress.
    tokio::spawn(async move {
        let _ = connection.await;
    });

    let scheme = if tls { "https" } else { "http" };
    let uri = format!("{scheme}://{authority}{}", req.path);
    let mut builder = http::Request::builder()
        .method(http::Method::POST)
        .uri(&uri)
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .header("grpc-encoding", "identity")
        .header("grpc-accept-encoding", "identity")
        .header("user-agent", "h5i-grpc/0.1");
    for (name, value) in &req.metadata {
        builder = builder.header(name.as_str(), value.as_str());
    }
    let request = builder
        .body(())
        .map_err(|e| format!("could not build the request: {e}"))?;

    let mut send_request = send_request
        .ready()
        .await
        .map_err(|e| format!("the connection was not ready: {e}"))?;
    let (response, mut stream) = send_request
        .send_request(request, false)
        .map_err(|e| format!("could not open the stream: {e}"))?;
    stream
        .send_data(Bytes::from(req.body.clone()), true)
        .map_err(|e| format!("could not send the request body: {e}"))?;

    let response = response
        .await
        .map_err(|e| format!("no response: {e}"))?;
    let http_status = response.status().as_u16();
    // A trailers-only error puts grpc-status in the headers.
    let mut grpc_status = header_i32(response.headers(), "grpc-status");
    let mut grpc_message = header_string(response.headers(), "grpc-message");

    let mut body = response.into_body();
    let mut collected = Vec::new();
    while let Some(chunk) = body.data().await {
        let chunk = chunk.map_err(|e| format!("reading the response failed: {e}"))?;
        collected.extend_from_slice(&chunk);
        let _ = body.flow_control().release_capacity(chunk.len());
    }
    if let Some(trailers) = body
        .trailers()
        .await
        .map_err(|e| format!("reading the trailers failed: {e}"))?
    {
        if let Some(status) = header_i32(&trailers, "grpc-status") {
            grpc_status = Some(status);
        }
        if let Some(message) = header_string(&trailers, "grpc-message") {
            grpc_message = Some(message);
        }
    }

    Ok(GrpcReply {
        http_status,
        grpc_status,
        grpc_message,
        body: collected,
    })
}

fn header_i32(headers: &http::HeaderMap, name: &str) -> Option<i32> {
    headers.get(name)?.to_str().ok()?.trim().parse().ok()
}

fn header_string(headers: &http::HeaderMap, name: &str) -> Option<String> {
    Some(headers.get(name)?.to_str().ok()?.to_string())
}

async fn tls_connect(
    tcp: tokio::net::TcpStream,
    host: &str,
    insecure: bool,
) -> Result<tokio_rustls::client::TlsStream<tokio::net::TcpStream>, String> {
    use tokio_rustls::TlsConnector;
    use tokio_rustls::rustls::{self, ClientConfig, RootCertStore};

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS setup failed: {e}"))?;
    let mut config = if insecure {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(insecure::NoVerify::new()))
            .with_no_client_auth()
    } else {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        builder.with_root_certificates(roots).with_no_client_auth()
    };
    config.alpn_protocols = vec![b"h2".to_vec()];

    let connector = TlsConnector::from(Arc::new(config));
    let domain = tokio_rustls::rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|e| format!("`{host}` is not a valid TLS name: {e}"))?;
    let stream = connector
        .connect(domain, tcp)
        .await
        .map_err(|e| format!("the TLS handshake failed: {e}"))?;
    if stream.get_ref().1.alpn_protocol() != Some(b"h2") {
        return Err("the server did not agree to HTTP/2 (no h2 over ALPN)".to_string());
    }
    Ok(stream)
}

/// A certificate verifier that accepts anything, for `--insecure` against a
/// target under test. Off by default and named for what it does.
mod insecure {
    use tokio_rustls::rustls::client::danger::{
        HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
    };
    use tokio_rustls::rustls::crypto::{
        CryptoProvider, ring, verify_tls12_signature, verify_tls13_signature,
    };
    use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use tokio_rustls::rustls::{DigitallySignedStruct, Error, SignatureScheme};

    #[derive(Debug)]
    pub struct NoVerify {
        provider: CryptoProvider,
    }

    impl NoVerify {
        pub fn new() -> Self {
            Self {
                provider: ring::default_provider(),
            }
        }
    }

    impl ServerCertVerifier for NoVerify {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, Error> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, Error> {
            verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, Error> {
            verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.provider.signature_verification_algorithms.supported_schemes()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One length-prefixed frame, the shape `h5i_grpc::frame` builds.
    fn frame(message: &[u8]) -> Vec<u8> {
        let mut out = vec![0];
        out.extend_from_slice(&(message.len() as u32).to_be_bytes());
        out.extend_from_slice(message);
        out
    }

    /// A cleartext HTTP/2 server that answers one unary call with `reply` framed
    /// and a `grpc-status: 0` trailer, the way a real gRPC server does.
    fn one_shot_server(reply: Vec<u8>) -> std::net::SocketAddr {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        listener.set_nonblocking(true).expect("nonblocking");
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).expect("from_std");
                let (tcp, _) = listener.accept().await.expect("accept");
                let mut conn = h2::server::handshake(tcp).await.expect("h2 handshake");
                // The accept loop drives the connection: it keeps polling after
                // the one response so the queued frames flush, and ends when the
                // client closes.
                while let Some(request) = conn.accept().await {
                    let (request, mut respond) = request.expect("request");
                    let mut body = request.into_body();
                    while let Some(chunk) = body.data().await {
                        let chunk = chunk.expect("chunk");
                        let _ = body.flow_control().release_capacity(chunk.len());
                    }
                    let response = http::Response::builder()
                        .status(200)
                        .header("content-type", "application/grpc")
                        .body(())
                        .expect("response");
                    let mut send = respond.send_response(response, false).expect("send response");
                    send.send_data(Bytes::from(reply.clone()), false).expect("send data");
                    let mut trailers = http::HeaderMap::new();
                    trailers.insert("grpc-status", http::HeaderValue::from_static("0"));
                    send.send_trailers(trailers).expect("send trailers");
                }
            });
        });
        addr
    }

    #[test]
    fn a_unary_call_reads_its_frame_and_the_grpc_status_trailer() {
        let addr = one_shot_server(frame(b"pong"));
        let reply = call_blocking(GrpcRequest {
            url: format!("http://{addr}"),
            path: "/echo.Echo/Say".to_string(),
            authority: None,
            body: frame(b"ping"),
            metadata: vec![],
            addrs: vec![],
            insecure: false,
            timeout: Some(Duration::from_secs(5)),
        })
        .expect("the call completes");

        assert_eq!(reply.http_status, 200);
        assert_eq!(reply.grpc_status, Some(0));
        // The body is the framed reply: flag, length, then "pong".
        assert_eq!(reply.body, frame(b"pong"));
    }

    #[test]
    fn a_bad_scheme_is_refused_before_dialing() {
        let reply = call_blocking(GrpcRequest {
            url: "ftp://host:50051".to_string(),
            path: "/x/Y".to_string(),
            authority: None,
            body: vec![],
            metadata: vec![],
            addrs: vec![],
            insecure: false,
            timeout: Some(Duration::from_secs(1)),
        });
        assert!(reply.is_err());
    }
}
