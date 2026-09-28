//! The Chromium lane: an h5i-owned intercepting proxy writing the ordinary
//! browser receipt and message-store formats.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use clap::Parser;
use h5i_browser::capture::{Capture, Received};
use h5i_browser::net::LocalBroker;
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
}

#[derive(Clone)]
struct Handler {
    broker: Arc<LocalBroker>,
    request: Option<h5i_browser::RequestRecord>,
    started: Option<Instant>,
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
                        None,
                        kept_headers,
                        Received::NotRead,
                        0,
                        self.started.map(|at| at.elapsed()).unwrap_or_default(),
                        Some(error.to_string()),
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
                Some(status),
                kept_headers,
                Received::Bytes(&body),
                body.len() as u64,
                self.started.map(|at| at.elapsed()).unwrap_or_default(),
                None,
            );
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
                None,
                Vec::new(),
                Received::NotRead,
                0,
                self.started.map(|at| at.elapsed()).unwrap_or_default(),
                Some(error.to_string()),
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
