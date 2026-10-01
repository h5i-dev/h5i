//! The Chromium lane: an h5i-owned intercepting proxy writing the ordinary
//! browser receipt and message-store formats.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use clap::Parser;
use h5i_browser::capture::{Capture, Received};
use h5i_browser::dom_inject;
use h5i_browser::net::{LocalBroker, ProxyResponse};
use h5i_browser::receipt::JsonlSink;
use http_body_util::BodyExt as _;
use hudsucker::certificate_authority::RcgenAuthority;
use hudsucker::hyper::{Request, Response, StatusCode};
use hudsucker::rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, Issuer, KeyPair,
};
use hudsucker::rustls::crypto::aws_lc_rs;
use hudsucker::{Body, HttpContext, HttpHandler, Proxy, RequestOrResponse};

#[derive(Parser, Debug)]
#[command(name = "h5i __capture-proxy")]
pub struct Args {
    #[arg(long)]
    pub target: String,
    #[arg(long)]
    pub control_file: PathBuf,
    #[arg(long)]
    pub stream_file: PathBuf,
    #[arg(long)]
    pub proxy_file: PathBuf,
    #[arg(long)]
    pub ca_cert: PathBuf,
    #[arg(long)]
    pub ca_key: PathBuf,
    #[arg(long)]
    pub receipts: PathBuf,
    #[arg(long)]
    pub actions: PathBuf,
    #[arg(long)]
    pub messages: PathBuf,
    #[arg(long = "allow")]
    pub allow: Vec<String>,
    #[arg(long = "deny")]
    pub deny: Vec<String>,
    #[arg(long = "deny-path")]
    pub deny_path: Vec<String>,
    #[arg(long)]
    pub no_loopback: bool,
    /// Turn on DOM instrumentation: inject the instrument into HTML responses
    /// and append the reports it beacons back to this file, one JSON per line.
    #[arg(long = "dom-report", value_name = "PATH")]
    pub dom_report: Option<PathBuf>,
}

#[derive(Clone)]
struct Handler {
    broker: Arc<LocalBroker>,
    request: Option<h5i_browser::RequestRecord>,
    started: Option<Instant>,
    /// Set together when DOM instrumentation is on: the script to inject, and
    /// the file the injected script's reports are appended to.
    instrument: Option<Arc<str>>,
    dom_report: Option<Arc<Mutex<std::fs::File>>>,
}

/// The host and path of a request, for matching the instrument's report beacon.
/// The authority rides in the URI or, in origin-form, the Host header.
fn host_and_path(parts: &hudsucker::hyper::http::request::Parts) -> (Option<String>, String) {
    let host = parts.uri.host().map(str::to_string).or_else(|| {
        parts
            .headers
            .get("host")
            .and_then(|v| v.to_str().ok())
            .map(|h| h.split(':').next().unwrap_or(h).to_string())
    });
    (host, parts.uri.path().to_string())
}

fn headers<T>(
    message: &hudsucker::hyper::http::request::Parts,
    _body: &T,
) -> Vec<(String, String)> {
    message
        .headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|v| (name.as_str().to_string(), v.to_string()))
        })
        .collect()
}

fn response_headers(message: &hudsucker::hyper::http::response::Parts) -> Vec<(String, String)> {
    message
        .headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|v| (name.as_str().to_string(), v.to_string()))
        })
        .collect()
}

impl HttpHandler for Handler {
    async fn handle_request(
        &mut self,
        _ctx: &HttpContext,
        req: Request<Body>,
    ) -> RequestOrResponse {
        // Hudsucker calls the handler once for the proxy handshake and again
        // for the decrypted request inside it. The CONNECT authority is not an
        // HTTP URL and carries no application message to store; policy is
        // applied to the inner https:// URL before that request is forwarded.
        if req.method() == hudsucker::hyper::Method::CONNECT {
            return req.into();
        }
        let (parts, body) = req.into_parts();
        // The instrument's report beacon: swallow it here, before policy and
        // before it is recorded, and answer 204. It never reaches upstream (the
        // host does not resolve) and must never land in the capture store.
        if let Some(report) = &self.dom_report {
            let (host, path) = host_and_path(&parts);
            if host.as_deref() == Some(dom_inject::BEACON_HOST) && path == dom_inject::BEACON_PATH {
                if let Ok(collected) = body.collect().await {
                    let mut line = collected.to_bytes().to_vec();
                    line.push(b'\n');
                    if let Ok(mut file) = report.lock() {
                        let _ = file.write_all(&line);
                        let _ = file.flush();
                    }
                }
                return Response::builder()
                    .status(StatusCode::NO_CONTENT)
                    .body(Body::empty())
                    .expect("static proxy response")
                    .into();
            }
        }
        let method = parts.method.as_str().to_string();
        let url = parts.uri.to_string();
        let sent_headers = headers(&parts, &());
        let body = match body.collect().await {
            Ok(body) => body.to_bytes(),
            Err(error) => {
                return Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .body(Body::from(format!(
                        "h5i could not read the request body: {error}"
                    )))
                    .expect("static proxy response")
                    .into();
            }
        };
        let record = match self
            .broker
            .observe_proxy_request(&method, &url, sent_headers, &body)
        {
            Ok(record) => record,
            Err(error) => {
                return Response::builder()
                    .status(StatusCode::INSUFFICIENT_STORAGE)
                    .body(Body::from(format!(
                        "h5i refused an unrecorded request: {error}"
                    )))
                    .expect("static proxy response")
                    .into();
            }
        };
        if !record.allowed {
            return Response::builder()
                .status(StatusCode::FORBIDDEN)
                .body(Body::from(
                    record
                        .denied_reason
                        .clone()
                        .unwrap_or_else(|| "refused by policy".into()),
                ))
                .expect("static proxy response")
                .into();
        }
        self.request = Some(record);
        self.started = Some(Instant::now());
        Request::from_parts(parts, Body::from(body)).into()
    }

    async fn handle_response(&mut self, _ctx: &HttpContext, res: Response<Body>) -> Response<Body> {
        let (parts, body) = res.into_parts();
        let status = parts.status.as_u16();
        let kept_headers = response_headers(&parts);
        let body = match body.collect().await {
            Ok(body) => body.to_bytes(),
            Err(error) => {
                if let Some(request) = &self.request {
                    let _ = self.broker.observe_proxy_response(
                        request,
                        ProxyResponse {
                            status: None,
                            headers: kept_headers,
                            body: Received::NotRead,
                            bytes: 0,
                            duration: self.started.map(|at| at.elapsed()).unwrap_or_default(),
                            error: Some(error.to_string()),
                        },
                    );
                }
                return Response::builder()
                    .status(StatusCode::BAD_GATEWAY)
                    .body(Body::from(format!(
                        "h5i could not read the response body: {error}"
                    )))
                    .expect("static proxy response");
            }
        };
        if let Some(request) = &self.request {
            let _ = self.broker.observe_proxy_response(
                request,
                ProxyResponse {
                    status: Some(status),
                    headers: kept_headers,
                    body: Received::Bytes(&body),
                    bytes: body.len() as u64,
                    duration: self.started.map(|at| at.elapsed()).unwrap_or_default(),
                    error: None,
                },
            );
        }
        // Inject the instrument into HTML responses, after the capture above has
        // already recorded the original wire bytes. We serve identity: decode,
        // inject, and drop the framing headers so hyper re-frames the new body.
        if let Some(instrument) = &self.instrument {
            let is_html = parts
                .headers
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|ct| ct.to_ascii_lowercase().contains("text/html"));
            // Only a full HTML document with a body. 204/205 must carry none,
            // and a 206 range would be spliced mid-stream with a now-wrong
            // Content-Range, so an empty body or either status is left alone.
            let injectable = is_html
                && !body.is_empty()
                && status != 206
                && (200..300).contains(&status);
            if injectable {
                let encoding = parts
                    .headers
                    .get("content-encoding")
                    .and_then(|v| v.to_str().ok());
                if let Some(decoded) = dom_inject::decode_body(&body, encoding) {
                    let js: &str = instrument;
                    let injected = dom_inject::inject_script(&decoded, js);
                    let mut parts = parts;
                    parts.headers.remove("content-encoding");
                    parts.headers.remove("content-length");
                    parts.headers.remove("transfer-encoding");
                    return Response::from_parts(parts, Body::from(injected));
                }
            }
        }
        Response::from_parts(parts, Body::from(body))
    }

    async fn handle_error(
        &mut self,
        _ctx: &HttpContext,
        error: hudsucker::hyper_util::client::legacy::Error,
    ) -> Response<Body> {
        if let Some(request) = &self.request {
            let _ = self.broker.observe_proxy_response(
                request,
                ProxyResponse {
                    status: None,
                    headers: Vec::new(),
                    body: Received::NotRead,
                    bytes: 0,
                    duration: self.started.map(|at| at.elapsed()).unwrap_or_default(),
                    error: Some(error.to_string()),
                },
            );
        }
        Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .body(Body::from("h5i could not reach the upstream server"))
            .expect("static proxy response")
    }
}

fn make_ca(cert_path: &PathBuf, key_path: &PathBuf) -> anyhow::Result<Issuer<'static, KeyPair>> {
    let mut params = CertificateParams::new(Vec::<String>::new())?;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, "h5i session capture proxy");
    params.distinguished_name = name;
    let key = KeyPair::generate()?;
    let cert = params.self_signed(&key)?;
    std::fs::write(cert_path, cert.pem())?;
    std::fs::write(key_path, key.serialize_pem())?;
    #[cfg(unix)]
    for path in [cert_path, key_path] {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(Issuer::new(params, key))
}

pub fn run(args: Args) -> anyhow::Result<()> {
    let capture = Arc::new(Capture::open(&args.messages)?);
    let sink = Arc::new(JsonlSink::create(&args.receipts)?);
    let policy = h5i_browser::Policy::new()
        .allow_all_of(&args.allow)
        .deny_all_of(&args.deny)
        .deny_paths_of(&args.deny_path)
        .set_allow_loopback(!args.no_loopback);
    let broker = LocalBroker::with_limits(
        policy,
        sink,
        None,
        h5i_browser::budget::Limits::default(),
        Some(capture),
    )?;

    // DOM instrumentation is on exactly when a report file is named. Create it
    // now so the file's presence tells `websec dom scan` the session is armed.
    let (instrument, dom_report) = match &args.dom_report {
        Some(path) => {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?;
            (
                Some(Arc::<str>::from(dom_inject::render_instrument())),
                Some(Arc::new(Mutex::new(file))),
            )
        }
        None => (None, None),
    };

    let issuer = make_ca(&args.ca_cert, &args.ca_key)?;
    let ca = RcgenAuthority::new(issuer, 1_000, aws_lc_rs::default_provider());
    let proxy_file = args.proxy_file.clone();
    let proxy_broker = broker.clone();
    std::thread::spawn(move || {
        let run = || -> anyhow::Result<()> {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            runtime.block_on(async move {
                let listener =
                    tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
                let address = listener.local_addr()?;
                std::fs::write(&proxy_file, format!("http://{address}\n"))?;
                let proxy = Proxy::builder()
                    .with_listener(listener)
                    .with_ca(ca)
                    .with_rustls_connector(aws_lc_rs::default_provider())
                    .with_http_handler(Handler {
                        broker: proxy_broker,
                        request: None,
                        started: None,
                        instrument,
                        dom_report,
                    })
                    .build()?;
                proxy.start().await.map_err(anyhow::Error::from)
            })
        };
        if let Err(error) = run() {
            eprintln!("h5i capture proxy stopped: {error}");
        }
    });

    // The ordinary session control protocol rides beside the proxy. It gives
    // websec/recon the same requests and resend verbs without making Chromium
    // pretend to be h5i's page engine. The page is created on this thread
    // because the DOM and JavaScript realm are deliberately !Send.
    let control_broker: Arc<dyn h5i_browser::Broker> = broker;
    let factory = h5i_browser::PageFactory::new(
        control_broker,
        Vec::new(),
        h5i_browser::PageOptions::default(),
    );
    let base = url::Url::parse("http://h5i.invalid/")?;
    let page = factory.from_bytes(b"<title>h5i proxy</title>", Some("text/html"), &base);
    h5i_browser::stream::serve(
        factory,
        page,
        h5i_browser::stream::ServeOptions {
            control_file: Some(args.control_file),
            stream_file: Some(args.stream_file),
            action_log: Some(args.actions),
            ..Default::default()
        },
    )?;
    Ok(())
}
