//! `h5i-browser-light` — the engine's command line.
//!
//! Standalone on purpose (ROADMAP 7.1 step 3): it runs on a bare host with no
//! h5i anywhere, which is how someone tries it, and h5i drives the same binary
//! as a process rather than linking it. One honesty rule travels with that:
//! outside a box there is no egress proxy and no receipt store, so what runs
//! here is a light browser with a request log — the containment claims belong
//! to the box, and this binary does not imply them.

use std::path::{Path, PathBuf};
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

/// Where to advertise the session's control port. Optional: without it the
/// control file sits beside the stream file.
const CONTROL_FILE_VAR: &str = "H5I_BROWSER_CONTROL_FILE";

/// Where h5i wants the agent's verbs recorded, so the console's agent-actions
/// pane has a source on an engine that has no mediated socket in front of it.
const ACTIONS_VAR: &str = "H5I_BROWSER_ACTIONS";

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

        /// Advertise the session's control port here, for the verbs that drive
        /// it (`snapshot`, `navigate`, `click`). Defaults to the stream file
        /// with a `.control` extension, so a box that sets one gets both.
        #[arg(long, value_name = "PATH")]
        control_file: Option<PathBuf>,

        /// Record the verbs an agent asks for here, as JSON lines. Defaults to
        /// $H5I_BROWSER_ACTIONS. With one set, a verb that cannot be recorded
        /// is refused rather than performed unseen.
        #[arg(long, value_name = "PATH")]
        actions: Option<PathBuf>,

        /// Serve one viewer, then exit.
        #[arg(long)]
        once: bool,
    },

    /// Drive the resident session a `serve` is holding open.
    ///
    /// This is the agent-facing half of the engine. `open` renders its own page
    /// and exits, so two `open`s share nothing — no history, no cookies, and
    /// nothing a viewer can watch. These verbs act on the page `serve` is
    /// holding, which is the page `h5i box view` is showing.
    #[command(subcommand)]
    Session(SessionVerb),

    /// Report what this engine can and cannot do, as JSON.
    ///
    /// h5i reads this to decide what to route here rather than inferring it
    /// from a version number.
    Capabilities {
        /// Report what this engine can do with `--script` on.
        #[arg(long)]
        script: bool,
    },

    /// Report the environment: fonts, proxy, and what the policy would allow.
    Doctor {
        #[command(flatten)]
        net: NetArgs,
    },
}

#[derive(Subcommand)]
enum SessionVerb {
    /// What the session is on right now.
    Status {
        #[command(flatten)]
        at: SessionArgs,
    },
    /// The page as a model should read it: the fenced outline, with `@ref`
    /// handles for the things that can be acted on.
    Snapshot {
        #[command(flatten)]
        at: SessionArgs,
    },
    /// Go to a URL, resolved against the current page like a click would be.
    Navigate {
        url: String,
        #[command(flatten)]
        at: SessionArgs,
    },
    /// Scroll the page. Negative scrolls up.
    Scroll {
        /// Pixels to scroll by.
        #[arg(allow_negative_numbers = true)]
        by: f64,
        #[command(flatten)]
        at: SessionArgs,
    },
    /// Put text into a field, replacing what was there.
    Type {
        /// `e3` or `@e3`, from a `snapshot`.
        reference: String,
        /// The text to put in it.
        text: String,
        #[command(flatten)]
        at: SessionArgs,
    },
    /// Submit the form containing a `@ref`.
    Submit {
        /// Any `@ref` inside the form — the submit button, or a field.
        reference: String,
        #[command(flatten)]
        at: SessionArgs,
    },
    /// Follow a `@ref` from the last snapshot.
    Click {
        /// `e3` or `@e3`, from a `snapshot`.
        reference: String,
        #[command(flatten)]
        at: SessionArgs,
    },
}

#[derive(Args, Clone)]
struct SessionArgs {
    /// The file a `serve` wrote its control port into. Defaults to
    /// $H5I_BROWSER_CONTROL_FILE, then to the control file beside
    /// $H5I_BROWSER_STREAM_FILE — so inside a box these verbs need no flags.
    #[arg(long, value_name = "PATH")]
    control_file: Option<PathBuf>,

    /// The control port directly, when there is no file to read it from.
    #[arg(long, conflicts_with = "control_file")]
    port: Option<u16>,

    /// Print the session's raw JSON answer instead of human output.
    #[arg(long)]
    json: bool,
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

    /// Run the page's own JavaScript. **Limited preview** — see the README for
    /// what is and is not implemented.
    ///
    /// Off by default on purpose. With script off, page-borne prompt injection
    /// has no delivery channel at all because no engine is running; turning it
    /// on spends that, and it is a decision rather than a default (ROADMAP
    /// §12.5).
    #[arg(long)]
    script: bool,
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
        Command::Capabilities { script } => {
            // Reported for the configuration asked about, because what h5i
            // routes on is whether *this* invocation runs script.
            println!(
                "{}",
                serde_json::to_string_pretty(&Capabilities::with_script(script))?
            );
            Ok(())
        }
        Command::Doctor { net } => doctor(&net),
        Command::Session(verb) => session(verb),
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
            control_file,
            actions,
            once,
        } => serve(
            &target,
            &net,
            &view,
            addr,
            quality,
            stream_file,
            control_file,
            actions,
            once,
        ),
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
            script: view.script,
        },
    );

    let page = match parse_target(target)? {
        Target::Remote(url) => factory.open(&url)?,
        Target::Local(path) => {
            let html = std::fs::read_to_string(&path).map_err(|e| H5iError::with_path(e, &path))?;
            factory.from_html(&html, &local_base(&path)?)
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
    control_file: Option<PathBuf>,
    action_log: Option<PathBuf>,
    once: bool,
) -> Result<(), H5iError> {
    let (_display, factory, page) = load(target, net, view)?;
    let stream_file = stream_file.or_else(|| std::env::var(STREAM_FILE_VAR).ok().map(PathBuf::from));
    let control_file = control_file
        .or_else(|| std::env::var(CONTROL_FILE_VAR).ok().map(PathBuf::from))
        .or_else(|| stream_file.as_deref().map(control_file_beside));
    let action_log = action_log.or_else(|| std::env::var(ACTIONS_VAR).ok().map(PathBuf::from));
    h5i_browser_light::stream::serve(
        factory,
        page,
        h5i_browser_light::stream::ServeOptions {
            addr,
            quality,
            stream_file,
            control_file,
            action_log,
            once,
        },
    )
}

/// Drive the resident session.
fn session(verb: SessionVerb) -> Result<(), H5iError> {
    let (at, request) = match &verb {
        SessionVerb::Status { at } => (at, serde_json::json!({"verb": "status"})),
        SessionVerb::Snapshot { at } => (at, serde_json::json!({"verb": "snapshot"})),
        SessionVerb::Navigate { url, at } => {
            (at, serde_json::json!({"verb": "navigate", "url": url}))
        }
        SessionVerb::Scroll { by, at } => (at, serde_json::json!({"verb": "scroll", "by": by})),
        SessionVerb::Type { reference, text, at } => (
            at,
            serde_json::json!({"verb": "type", "ref": reference, "text": text}),
        ),
        SessionVerb::Submit { reference, at } => {
            (at, serde_json::json!({"verb": "submit", "ref": reference}))
        }
        SessionVerb::Click { reference, at } => {
            (at, serde_json::json!({"verb": "click", "ref": reference}))
        }
    };

    let port = session_port(at)?;
    let reply = h5i_browser_light::stream::ask(port, &request)?;

    if at.json {
        println!("{reply}");
        // A refusal is still an answer, and `--json` promised the answer. The
        // exit code carries the verdict so a script does not have to parse it.
        return exit_status(&reply);
    }

    if reply.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        let text = reply
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("the session refused, without saying why");
        return Err(H5iError::Metadata(text.to_string()));
    }

    // The snapshot is the whole point of the verb; everything else is a line.
    if let Some(text) = reply.get("text").and_then(serde_json::Value::as_str) {
        println!("{text}");
    } else if let Some(moved) = reply.get("moved").and_then(serde_json::Value::as_bool) {
        let offset = reply.get("offset").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
        let height = reply
            .get("content_height")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0);
        // "did not move" is the answer an agent needs to stop scrolling, so it
        // is said rather than left to be inferred from an unchanged number.
        println!(
            "{} at {offset:.0} of {height:.0}",
            if moved { "scrolled" } else { "already at the end —" }
        );
    } else if let Some(url) = reply.get("url").and_then(serde_json::Value::as_str) {
        println!("url: {url}");
    } else if let Some(reference) = reply.get("ref").and_then(serde_json::Value::as_str) {
        // A verb that printed nothing read as a verb that did nothing. The
        // typed text is deliberately not echoed: it may be a password, and the
        // engine's whole posture is that a credential does not travel back out
        // through a surface an agent or a log can read.
        println!("typed into {reference}");
    }
    Ok(())
}

fn exit_status(reply: &serde_json::Value) -> Result<(), H5iError> {
    if reply.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        Ok(())
    } else {
        std::process::exit(1)
    }
}

/// Where the session is listening.
///
/// The fallback chain ends at the stream file because that is the one thing
/// h5i already sets in a box: an agent that has to be told a port is an agent
/// that has to be told this engine exists.
fn session_port(at: &SessionArgs) -> Result<u16, H5iError> {
    if let Some(port) = at.port {
        return Ok(port);
    }
    let path = at
        .control_file
        .clone()
        .or_else(|| std::env::var(CONTROL_FILE_VAR).ok().map(PathBuf::from))
        .or_else(|| {
            std::env::var(STREAM_FILE_VAR)
                .ok()
                .map(|s| control_file_beside(Path::new(&s)))
        })
        .ok_or_else(|| {
            H5iError::Metadata(
                "no session to talk to: pass --control-file or --port, or set \
                 $H5I_BROWSER_STREAM_FILE (h5i sets it inside a box)."
                    .into(),
            )
        })?;
    h5i_browser_light::stream::read_port_file(&path)
}

/// The control file that belongs to a given stream file.
///
/// Derived rather than configured so that a box which sets only
/// `H5I_BROWSER_STREAM_FILE` — which is what h5i injects today — still gets a
/// drivable session. A second variable h5i would also have to learn to set is a
/// second thing that can be forgotten, and the failure would be a session that
/// looks live and cannot be driven.
fn control_file_beside(stream_file: &Path) -> PathBuf {
    stream_file.with_extension("control")
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
/// The `file://` base a local page resolves its relative links against.
///
/// `canonicalize` is preferred because it resolves symlinks, so a page reached
/// through one gets the base its neighbours actually live at. But it walks the
/// path **by name**, and that walk can fail for a file that was just read
/// successfully: a box's supervised tier redirects `/tmp`, so a working
/// directory underneath it survives as the shell's fd and stops resolving as a
/// path. `open ./page.html` then hit an earlier version of this code that fell
/// back to the *relative* path, which `from_file_path` refuses, and reported
/// the target as an invalid path — naming the wrong thing entirely, since the
/// file had already been read by then.
///
/// So the fallback is [`std::path::absolute`], which is pure path arithmetic
/// and needs no directory to exist. The base is only ever resolved against, so
/// a path that no longer resolves is still a usable one.
fn local_base(path: &Path) -> Result<Url, H5iError> {
    let absolute = match path.canonicalize() {
        Ok(resolved) => resolved,
        // The canonicalize error is the informative one — it says *why* the
        // walk failed — so it is what surfaces if even this cannot produce an
        // absolute path.
        Err(error) => std::path::absolute(path).map_err(|_| H5iError::with_path(error, path))?,
    };
    Url::from_file_path(&absolute).map_err(|_| {
        H5iError::InvalidPath(format!(
            "{} cannot be expressed as a file:// base",
            absolute.display()
        ))
    })
}

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
    fn a_relative_target_gets_an_absolute_file_base() {
        let base = local_base(Path::new("./page.html")).expect("a relative path has a base");
        assert_eq!(base.scheme(), "file");
        assert!(
            base.path().ends_with("/page.html"),
            "the base keeps the file it was built from: {base}"
        );
        assert!(
            !base.path().starts_with("/./"),
            "the cwd was joined rather than pasted: {base}"
        );
    }

    #[test]
    fn a_path_that_cannot_be_walked_still_yields_a_base() {
        // The regression this exists for: inside a box the supervised tier
        // redirects `/tmp`, so a cwd underneath it reads fine through the
        // shell's fd and fails to resolve by name. `canonicalize` fails on a
        // path that does not resolve, and the old fallback handed
        // `from_file_path` a relative path, which it refuses — turning a
        // readable page into "invalid path". Nothing here may depend on the
        // path existing.
        let missing = Path::new("./no-such-dir-b7f1/page.html");
        assert!(missing.canonicalize().is_err(), "the premise of this test");

        let base = local_base(missing).expect("an unwalkable path still has a base");
        assert_eq!(base.scheme(), "file");
        assert!(base.path().ends_with("/no-such-dir-b7f1/page.html"), "{base}");
    }

    #[test]
    fn the_control_file_sits_beside_the_stream_file() {
        // The whole reason the path is derived: h5i injects
        // H5I_BROWSER_STREAM_FILE and nothing else, so a session must be
        // drivable without h5i learning a second variable.
        assert_eq!(
            control_file_beside(Path::new("/tmp/agent-browser/h5i-light.stream")),
            PathBuf::from("/tmp/agent-browser/h5i-light.control")
        );
    }

    #[test]
    fn session_verbs_parse_the_way_an_agent_would_type_them() {
        for argv in [
            vec!["h5i-browser-light", "session", "status"],
            vec!["h5i-browser-light", "session", "snapshot", "--json"],
            vec!["h5i-browser-light", "session", "navigate", "/docs"],
            vec!["h5i-browser-light", "session", "click", "@e3"],
            vec!["h5i-browser-light", "session", "click", "e3", "--port", "9000"],
        ] {
            Cli::try_parse_from(&argv).unwrap_or_else(|e| panic!("{argv:?} should parse: {e}"));
        }

        // Two ways to name the same session is a way to name two different
        // ones by accident.
        assert!(
            Cli::try_parse_from([
                "h5i-browser-light",
                "session",
                "status",
                "--port",
                "9000",
                "--control-file",
                "/tmp/x.control",
            ])
            .is_err(),
            "--port and --control-file must not be given together"
        );
    }

    #[test]
    fn script_is_opt_in_at_the_command_line() {
        // The gate ROADMAP_BROWSER §3.3 asks for: script is a decision someone
        // makes, never a default they inherit.
        for argv in [
            vec!["h5i-browser-light", "open", "https://x.example/", "--script"],
            vec!["h5i-browser-light", "serve", "https://x.example/", "--script"],
            vec!["h5i-browser-light", "capabilities", "--script"],
        ] {
            Cli::try_parse_from(&argv).unwrap_or_else(|e| panic!("{argv:?}: {e}"));
        }
    }

    #[test]
    fn capabilities_report_the_configuration_asked_about() {
        // What h5i routes on is whether *this* invocation runs script, not what
        // the binary could do if asked differently.
        assert!(!Capabilities::current().javascript, "off unless asked");
        assert!(!Capabilities::with_script(false).javascript);
        assert!(Capabilities::with_script(true).javascript);

        // The rest is a property of the engine either way, and stays honest.
        let with = Capabilities::with_script(true);
        assert!(with.fail_closed_receipts);
        assert!(with.snapshot && with.screenshot && with.live_view);
        assert!(!with.video && !with.webgl, "still absent, and still said so");
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
