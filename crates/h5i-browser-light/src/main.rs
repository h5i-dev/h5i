//! `h5i-browser-light` — the engine's command line.
//!
//! Standalone on purpose (ROADMAP 7.1 step 3): it runs on a bare host with no
//! h5i anywhere, which is how someone tries it, and h5i drives the same binary
//! as a process rather than linking it. One honesty rule travels with that:
//! outside a box there is no egress proxy and no receipt store, so what runs
//! here is a light browser with a request log — the containment claims belong
//! to the box, and this binary does not imply them.

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Args, Parser, Subcommand};
use h5i_browser_light::engine::{Page, PageFactory, PageOptions};
use h5i_browser_light::net::Broker;
use h5i_browser_light::policy::Policy;
use h5i_browser_light::receipt::{JsonlSink, MemorySink, RequestRecord, Sink};
use h5i_browser_light::{fonts, Capabilities};
use h5i_error::H5iError;
use url::Url;

/// The environment variable h5i uses to hand a box its egress proxy.
const EGRESS_PROXY_VAR: &str = "H5I_EGRESS_PROXY";

/// Origins h5i granted this box, as a comma-separated list. Read as a default
/// for `--allow` so a box inherits its own `net.egress` without the agent
/// having to restate it (and without it being able to widen it by omission).
const ALLOW_VAR: &str = "H5I_BROWSER_ALLOW";

/// Where h5i wants the request log. Read as a default for `--receipts`, which
/// is what puts the fail-closed guarantee under h5i's control rather than the
/// caller's: no writable log, no fetch.
const RECEIPTS_VAR: &str = "H5I_BROWSER_RECEIPTS";

/// Where h5i wants `serve` to advertise its port, so `h5i box view` finds it
/// without being told this engine exists.
const STREAM_FILE_VAR: &str = "H5I_BROWSER_STREAM_FILE";

#[derive(Parser)]
#[command(
    name = "h5i-browser-light",
    version,
    about = "A lightweight visual browser for coding agents: every request is policy-checked and receipted before it reaches the wire."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Load a page, then report what it says and what it tried to reach.
    Open {
        /// A URL, or a path to a local HTML file.
        target: String,

        #[command(flatten)]
        net: NetArgs,

        #[command(flatten)]
        view: ViewArgs,

        /// Write a PNG of the viewport here.
        #[arg(long, value_name = "PATH")]
        screenshot: Option<PathBuf>,

        /// Print the page's prose instead of its outline.
        #[arg(long)]
        text: bool,

        /// Emit one JSON object instead of human output.
        #[arg(long)]
        json: bool,
    },

    /// Serve a live view of a page over WebSocket.
    ///
    /// Speaks the format h5i's viewers already use, so `h5i box view` and
    /// `h5i box view --term` attach to this engine unchanged.
    Serve {
        /// A URL, or a path to a local HTML file.
        target: String,

        #[command(flatten)]
        net: NetArgs,

        #[command(flatten)]
        view: ViewArgs,

        /// Address to listen on. Port 0 picks a free one.
        #[arg(long, default_value = "127.0.0.1:0")]
        addr: String,

        /// JPEG quality for frames.
        #[arg(long, default_value_t = 80)]
        quality: u8,

        /// Advertise the bound port here. h5i's viewers look for
        /// `<env>/tmp/agent-browser/*.stream`.
        #[arg(long, value_name = "PATH")]
        stream_file: Option<PathBuf>,

        /// Serve one viewer, then exit.
        #[arg(long)]
        once: bool,
    },

    /// Report what this engine can and cannot do, as JSON.
    ///
    /// h5i reads this to decide what to route here rather than inferring it
    /// from a version number.
    Capabilities,

    /// Report the environment: fonts, proxy, and what the policy would allow.
    Doctor {
        #[command(flatten)]
        net: NetArgs,
    },
}

#[derive(Args, Clone)]
struct NetArgs {
    /// Grant an origin. Repeatable. Without any, nothing remote is reachable.
    #[arg(long = "allow", value_name = "ORIGIN")]
    allow: Vec<String>,

    /// Refuse loopback too (it is reachable by default: it is the dev server).
    #[arg(long)]
    no_loopback: bool,

    /// Append the request log here as JSON lines.
    #[arg(long, value_name = "PATH")]
    receipts: Option<PathBuf>,

    /// The egress proxy to route through. Defaults to $H5I_EGRESS_PROXY.
    #[arg(long, value_name = "URL")]
    proxy: Option<String>,

    /// Refuse a response larger than this many bytes.
    #[arg(long, default_value_t = 8 * 1024 * 1024, value_name = "BYTES")]
    max_response_bytes: u64,

    /// How many redirect hops to follow. Every hop is policy-checked.
    #[arg(long, default_value_t = 5)]
    max_redirects: usize,
}

#[derive(Args, Clone)]
struct ViewArgs {
    #[arg(long, default_value_t = 1280)]
    width: u32,

    #[arg(long, default_value_t = 720)]
    height: u32,

    #[arg(long, default_value_t = 1.0)]
    scale: f32,

    /// A font file to register. Repeatable, and never subject to the scan cap.
    #[arg(long = "font-file", value_name = "PATH")]
    font_files: Vec<PathBuf>,

    /// A directory to scan for fonts. Repeatable. Replaces the defaults.
    #[arg(long = "font-dir", value_name = "PATH")]
    font_dirs: Vec<PathBuf>,

    /// Most lines of outline to emit.
    #[arg(long, default_value_t = 500)]
    max_snapshot_lines: usize,
}

/// Writes to both sinks, and fails if either refuses.
///
/// The display sink is what the CLI prints at the end; the file sink is the
/// durable record. Requiring both to succeed keeps the fail-closed rule from
/// quietly weakening when someone passes `--receipts`.
struct TeeSink {
    display: Arc<MemorySink>,
    file: Arc<dyn Sink>,
}

impl Sink for TeeSink {
    fn append(&self, record: &RequestRecord) -> Result<(), H5iError> {
        self.display.append(record)?;
        self.file.append(record)
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("h5i-browser-light: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), H5iError> {
    match Cli::parse().command {
        Command::Capabilities => {
            println!("{}", serde_json::to_string_pretty(&Capabilities::current())?);
            Ok(())
        }
        Command::Doctor { net } => doctor(&net),
        Command::Open {
            target,
            net,
            view,
            screenshot,
            text,
            json,
        } => open(&target, &net, &view, screenshot, text, json),
        Command::Serve {
            target,
            net,
            view,
            addr,
            quality,
            stream_file,
            once,
        } => serve(&target, &net, &view, addr, quality, stream_file, once),
    }
}

/// Build the factory and load the first page, shared by `open` and `serve`.
fn load(
    target: &str,
    net: &NetArgs,
    view: &ViewArgs,
) -> Result<(Arc<MemorySink>, PageFactory, Page), H5iError> {
    let policy = build_policy(net);
    let (display, sink) = build_sinks(net)?;
    let broker = Arc::new(Broker::new(policy, sink, proxy_of(net).as_deref())?);
    let font_setup = load_fonts(view);
    if font_setup.is_empty() {
        eprintln!("h5i-browser-light: no fonts registered; text will not be drawn.");
        eprintln!("      pass --font-file <path.ttf> or --font-dir <dir>.");
    }

    let factory = PageFactory::new(
        broker,
        font_setup.sources.clone(),
        PageOptions {
            width: view.width,
            height: view.height,
            scale: view.scale,
            max_snapshot_lines: view.max_snapshot_lines,
        },
    );

    let page = match parse_target(target)? {
        Target::Remote(url) => factory.open(&url)?,
        Target::Local(path) => {
            let html = std::fs::read_to_string(&path).map_err(|e| H5iError::with_path(e, &path))?;
            let base = Url::from_file_path(path.canonicalize().unwrap_or(path.clone()))
                .map_err(|_| H5iError::InvalidPath(path.display().to_string()))?;
            factory.from_html(&html, &base)
        }
    };

    Ok((display, factory, page))
}

#[allow(clippy::too_many_arguments)]
fn serve(
    target: &str,
    net: &NetArgs,
    view: &ViewArgs,
    addr: String,
    quality: u8,
    stream_file: Option<PathBuf>,
    once: bool,
) -> Result<(), H5iError> {
    let (_display, factory, page) = load(target, net, view)?;
    let stream_file = stream_file.or_else(|| std::env::var(STREAM_FILE_VAR).ok().map(PathBuf::from));
    h5i_browser_light::stream::serve(
        factory,
        page,
        h5i_browser_light::stream::ServeOptions {
            addr,
            quality,
            stream_file,
            once,
        },
    )
}

fn build_policy(net: &NetArgs) -> Policy {
    // Flags first, then whatever h5i granted the box. Both are additive: an
    // agent cannot widen the box's policy by passing `--allow`, because the
    // sandbox's own egress enforcement is still the boundary underneath.
    let from_env: Vec<String> = std::env::var(ALLOW_VAR)
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    Policy::new()
        .allow_all_of(&net.allow)
        .allow_all_of(&from_env)
        .set_allow_loopback(!net.no_loopback)
        .set_max_redirects(net.max_redirects)
        .set_max_response_bytes(net.max_response_bytes)
}

fn proxy_of(net: &NetArgs) -> Option<String> {
    net.proxy
        .clone()
        .or_else(|| std::env::var(EGRESS_PROXY_VAR).ok())
        .filter(|value| !value.trim().is_empty())
}

fn build_sinks(net: &NetArgs) -> Result<(Arc<MemorySink>, Arc<dyn Sink>), H5iError> {
    let display = Arc::new(MemorySink::new());
    let receipts = net
        .receipts
        .clone()
        .or_else(|| std::env::var(RECEIPTS_VAR).ok().map(PathBuf::from));
    match &receipts {
        None => Ok((display.clone(), display)),
        Some(path) => {
            let file = Arc::new(JsonlSink::create(path)?);
            let tee = Arc::new(TeeSink {
                display: display.clone(),
                file,
            });
            Ok((display, tee))
        }
    }
}

fn load_fonts(view: &ViewArgs) -> fonts::FontSetup {
    let dirs = if view.font_dirs.is_empty() {
        fonts::default_font_dirs()
    } else {
        view.font_dirs.clone()
    };
    fonts::load(&view.font_files, &dirs, None)
}

fn doctor(net: &NetArgs) -> Result<(), H5iError> {
    let policy = build_policy(net);
    let proxy = proxy_of(net);
    let font_setup = fonts::load(&[], &fonts::default_font_dirs(), None);

    println!("engine     : h5i-browser-light {}", env!("CARGO_PKG_VERSION"));
    println!("fonts      : {}", font_setup.summary());
    if font_setup.is_empty() {
        println!(
            "             (pass --font-file to name one; without fonts, pages render but text does not)"
        );
    }
    match &proxy {
        Some(url) => println!("egress     : via {url}"),
        None => println!(
            "egress     : direct (no {EGRESS_PROXY_VAR}; outside a box there is no proxy to route through)"
        ),
    }
    println!(
        "loopback   : {}",
        if net.no_loopback {
            "refused"
        } else {
            "allowed (the dev server)"
        }
    );

    let origins: Vec<_> = policy.origins().collect();
    if origins.is_empty() {
        println!("allowlist  : empty — nothing remote is reachable");
    } else {
        println!("allowlist  : {}", origins.join(", "));
    }
    println!("script     : not linked in this tier");

    // Prove the client can actually be built with these settings rather than
    // reporting a configuration that fails at the first fetch.
    Broker::new(policy, Arc::new(MemorySink::new()), proxy.as_deref())?;
    println!("client     : ok");
    Ok(())
}

fn open(
    target: &str,
    net: &NetArgs,
    view: &ViewArgs,
    screenshot: Option<PathBuf>,
    as_text: bool,
    as_json: bool,
) -> Result<(), H5iError> {
    let (display, _factory, mut page) = load(target, net, view)?;
    let snapshot = page.snapshot();

    let screenshot_bytes = match &screenshot {
        Some(path) => {
            let png = page.screenshot_png()?;
            std::fs::write(path, &png).map_err(|e| H5iError::with_path(e, path))?;
            Some((path.clone(), png.len()))
        }
        None => None,
    };

    let records = display.records();

    if as_json {
        let payload = serde_json::json!({
            "url": snapshot.url,
            "title": snapshot.title,
            "snapshot": snapshot,
            "text": page.text(),
            "requests": records,
            "screenshot": screenshot_bytes.as_ref().map(|(path, len)| serde_json::json!({
                "path": path.display().to_string(),
                "bytes": len,
            })),
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    if as_text {
        println!("{}", page.text());
    } else {
        print!("{}", snapshot.render());
    }

    // The request log is the point of this engine, so it is printed by default
    // rather than hidden behind a flag.
    if !records.is_empty() {
        eprintln!("\nrequests:");
        for record in records
            .iter()
            .filter(|r| r.phase == h5i_browser_light::receipt::Phase::Response)
        {
            eprintln!("  {}", record.render());
        }
    }

    if let Some((path, len)) = screenshot_bytes {
        eprintln!("\nscreenshot: {} ({len} bytes)", path.display());
    }
    Ok(())
}

#[derive(Debug)]
enum Target {
    Remote(Url),
    Local(PathBuf),
}

/// Decide whether the caller named a URL or a file.
///
/// A bare path is common enough (`open ./page.html`) that treating it as a
/// failed URL parse would be unhelpful, and a `file:` URL means the same thing.
fn parse_target(target: &str) -> Result<Target, H5iError> {
    if let Ok(url) = Url::parse(target) {
        match url.scheme() {
            "http" | "https" => return Ok(Target::Remote(url)),
            "file" => {
                let path = url
                    .to_file_path()
                    .map_err(|_| H5iError::InvalidPath(target.to_string()))?;
                return Ok(Target::Local(path));
            }
            other => {
                return Err(H5iError::InvalidPath(format!(
                    "`{other}:` is not something this engine opens (try http, https, or a path)"
                )));
            }
        }
    }
    Ok(Target::Local(PathBuf::from(target)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_targets_are_remote_and_paths_are_local() {
        assert!(matches!(
            parse_target("https://example.com/").unwrap(),
            Target::Remote(_)
        ));
        assert!(matches!(
            parse_target("./page.html").unwrap(),
            Target::Local(_)
        ));
        assert!(matches!(
            parse_target("/tmp/page.html").unwrap(),
            Target::Local(_)
        ));
    }

    #[test]
    fn an_unopenable_scheme_says_so_rather_than_being_read_as_a_filename() {
        // `data:...` as a navigation target would otherwise become a file
        // named "data:..." and fail with a confusing not-found.
        let error = parse_target("ftp://example.com/x").unwrap_err();
        assert!(error.to_string().contains("ftp"));
    }

    #[test]
    fn cli_parses_the_shapes_the_docs_promise() {
        // Cheap guard against a flag rename breaking the documented usage.
        Cli::try_parse_from([
            "h5i-browser-light",
            "open",
            "https://example.com",
            "--allow",
            "example.com",
            "--screenshot",
            "/tmp/x.png",
            "--receipts",
            "/tmp/r.jsonl",
        ])
        .expect("documented open invocation parses");

        Cli::try_parse_from(["h5i-browser-light", "capabilities"]).expect("capabilities parses");
        Cli::try_parse_from(["h5i-browser-light", "doctor"]).expect("doctor parses");
    }
}
