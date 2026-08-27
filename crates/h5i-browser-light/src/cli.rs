//! The engine's command line.
//!
//! **Reached through `h5i __engine`, not through a second binary.** The engine
//! used to ship as `h5i-browser-light` alongside `h5i`, because it was a second
//! product somebody might want on its own. It is now the renderer behind
//! `h5i browser`, and two files bought three problems: an install that left the
//! headline command broken by default, a version skew between two halves of one
//! protocol with no handshake between them, and a box that could *read* the
//! engine without being allowed to `exec` it.
//!
//! The process boundary that mattered is untouched. `h5i browser` still runs
//! the engine as a separate process and speaks a protocol to it; it execs
//! itself to get there instead of a second file. What was separate was the
//! file, not the process.
//!
//! One honesty rule travels with running it directly: outside a box there is no
//! egress proxy and no receipt store, so what runs here is a light browser with
//! a request log — the containment claims belong to the box, and this entry
//! point does not imply them.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Args, Parser, Subcommand};
use crate::engine::{Page, PageFactory, PageOptions};
use crate::net::Broker;
use crate::policy::Policy;
use crate::receipt::{JsonlSink, MemorySink, RequestRecord, Sink};
use crate::{fonts, Capabilities};
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

/// Where a session's Unix control socket is. Set by h5i for a session it placed
/// in a box, where a port cannot be reached across the per-run network
/// namespace; unset everywhere else, where the port is simpler.
const CONTROL_SOCKET_VAR: &str = "H5I_BROWSER_CONTROL_SOCKET";

/// Where h5i wants the agent's verbs recorded, so the console's agent-actions
/// pane has a source on an engine that has no mediated socket in front of it.
const ACTIONS_VAR: &str = "H5I_BROWSER_ACTIONS";

#[derive(Parser)]
#[command(
    name = "h5i __engine",
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

        /// Also take control connections on a Unix socket here.
        ///
        /// For a session inside an h5i box. Every `h5i box run` gets its own
        /// network namespace, so a verb carried in afterwards has a loopback of
        /// its own and cannot reach the port this session bound. A path can be
        /// reached because the box's filesystem is one filesystem across every
        /// run in it. Defaults to $H5I_BROWSER_CONTROL_SOCKET.
        #[arg(long, value_name = "PATH")]
        control_socket: Option<PathBuf>,

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

    /// Write or print the agent skill this binary carries.
    ///
    /// The skill teaches an agent to drive this browser on a bare host: the
    /// verbs, the ref rule, the error codes, and — the part it must not get
    /// wrong — which guarantees hold anywhere and which need an h5i box.
    #[command(subcommand)]
    Skill(SkillCommands),

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
        /// Report only what changed since the last snapshot.
        ///
        /// Three hundred lines re-read after every click, of which four are
        /// new, is the wrong shape for an agent loop. When the page changed too
        /// much for a difference to be the shorter answer — a navigation, or a
        /// page that replaced its own body — the full outline is sent instead
        /// and the reply says which it is.
        #[arg(long)]
        delta: bool,
        #[command(flatten)]
        at: SessionArgs,
    },
    /// Hand the page to the human at the live view for as long as a login takes.
    ///
    /// While this is on, every control verb that reads the page is refused: a
    /// credential typed into a page the agent can snapshot has been handed to
    /// the agent. The session the login establishes stays in the jar
    /// afterwards, and the agent can see that it is logged in without ever
    /// reading the cookie that says so.
    ///
    /// **The live view keeps streaming, and that is the limit of this mode.**
    /// The human doing the typing has to see what they are typing, so frames
    /// are not withheld — and the viewer socket is inside the box, where there
    /// is no privilege boundary, so an agent that goes looking can attach to it
    /// and watch the same pixels. This refuses the documented path, which is
    /// the threat it was written for; it is not containment against an agent
    /// that is trying. Type a password here only where that distinction is one
    /// you are willing to make.
    Login {
        /// End login mode and make the page readable again.
        #[arg(long, conflicts_with = "on")]
        off: bool,
        /// Begin login mode. The default.
        #[arg(long)]
        on: bool,
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
    /// Wait until something is on the page, or until nothing can put it there.
    ///
    /// Three answers, and the third is the point: a page that runs no script,
    /// or a scripted page that has gone quiet, cannot grow the thing you are
    /// waiting for — so that comes back immediately rather than after a budget
    /// spent proving it.
    WaitFor {
        /// A CSS selector that must match at least one element.
        #[arg(long, value_name = "CSS")]
        selector: Option<String>,
        /// Text that must appear in the outline a reader would see.
        #[arg(long, value_name = "TEXT", conflicts_with = "selector")]
        text: Option<String>,
        #[command(flatten)]
        at: SessionArgs,
    },

    /// Wait until a page expression is true.
    ///
    /// Needs a session started with `--script`. A condition that throws counts
    /// as *not yet* rather than as an error, because a page mid-build throws on
    /// the way to values it has not made.
    WaitForScript {
        /// The expression, evaluated in the page's realm.
        expr: String,
        #[command(flatten)]
        at: SessionArgs,
    },

    /// Pull structured data out of the page by selector.
    ///
    /// The schema is an object of field names to selector specs: `"h1"` for the
    /// first match's text, `["a"]` for every match, `{"selector":"a",
    /// "attr":"href"}` for an attribute, and `[{"selector":"li","fields":{…}}]`
    /// for one object per match with sub-selectors scoped to it.
    ///
    /// An empty array is a result. A schema where nothing matched is an error,
    /// because an object full of nulls looks like an answer.
    Extract {
        /// The schema, as JSON.
        schema: String,
        #[command(flatten)]
        at: SessionArgs,
    },

    /// The page as markdown: what a reader would read, without the handles.
    Markdown {
        /// Stop after this many bytes. Truncation is always announced.
        #[arg(long, value_name = "BYTES")]
        max_bytes: Option<usize>,
        #[command(flatten)]
        at: SessionArgs,
    },

    /// Which credentials this session can use, by name.
    ///
    /// Names only. No verb in this engine returns a credential's value: the
    /// model names one, the engine resolves it on the way into the field, and
    /// the reply echoes the placeholder. Only the `H5I_SECRET_` namespace is
    /// reachable, so h5i's own configuration is not.
    Env {
        #[command(flatten)]
        at: SessionArgs,
    },

    /// The request log: what this session asked for, and what was refused.
    ///
    /// The engine *is* the HTTP client here, so this is the decision record the
    /// broker wrote before the bytes moved, not an observation of the network
    /// made from beside it. If a request is not in this list, it did not
    /// happen.
    Requests {
        /// Only what happened after this sequence number.
        ///
        /// Pass back the `cursor` from a previous answer to see just what is
        /// new, the way `snapshot --delta` works and for the same reason.
        #[arg(long, value_name = "SEQ")]
        since: Option<u64>,
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

#[derive(Subcommand)]
enum SkillCommands {
    /// Write the skill to disk. Defaults to this runtime's per-user skill
    /// directory; `$H5I_SKILL_DIR` overrides it, which is how a box redirects
    /// an install to an in-box location.
    Install {
        /// Write here instead of the default target.
        #[arg(long, value_name = "DIR")]
        target: Option<PathBuf>,
    },

    /// Print the skill to stdout.
    Show,

    /// Print where `install` would write.
    Path,
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

    /// The session's Unix control socket, when it has one.
    ///
    /// Preferred over a port whenever it is set, because the arrangement that
    /// needs it — a session in a box — is the one where a port cannot work.
    /// Defaults to $H5I_BROWSER_CONTROL_SOCKET.
    #[arg(long, value_name = "PATH")]
    control_socket: Option<PathBuf>,

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

/// Run the engine's CLI over `args`, which must include the program name.
///
/// Exits the process on failure rather than returning, because the caller is a
/// `main` whose only remaining job would be to do the same thing. The prefix on
/// the error names the engine, not h5i: a page that failed to load is the
/// engine's answer, and attributing it to the caller would send someone looking
/// in the wrong place.
pub fn main<I, T>(args: I) -> !
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    match run(args) {
        Ok(()) => std::process::exit(0),
        Err(error) => {
            eprintln!("h5i browser engine: {error}");
            std::process::exit(1);
        }
    }
}

fn run<I, T>(args: I) -> Result<(), H5iError>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    match Cli::parse_from(args).command {
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
        Command::Skill(action) => skill(action),
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
            control_socket,
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
            control_socket,
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
            // Bytes rather than `read_to_string`, so a local file gets the same
            // encoding treatment a fetched one does. `read_to_string` also
            // *refuses* a file that is not valid UTF-8, which is exactly the
            // file this path most needs to be able to open.
            let bytes = std::fs::read(&path).map_err(|e| H5iError::with_path(e, &path))?;
            factory.from_bytes(&bytes, None, &local_base(&path)?)
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
    control_socket: Option<PathBuf>,
    action_log: Option<PathBuf>,
    once: bool,
) -> Result<(), H5iError> {
    let (requests, factory, page) = load(target, net, view)?;
    let control_socket =
        control_socket.or_else(|| std::env::var(CONTROL_SOCKET_VAR).ok().map(PathBuf::from));
    let stream_file = stream_file.or_else(|| std::env::var(STREAM_FILE_VAR).ok().map(PathBuf::from));
    let chosen = control_file
        .or_else(|| std::env::var(CONTROL_FILE_VAR).ok().map(PathBuf::from))
        .or_else(|| stream_file.as_deref().map(control_file_beside));
    // The other half of `session_port`'s default. Without this the two
    // disagree: the verbs would look somewhere `serve` never wrote.
    let defaulted = chosen.is_none();
    let control_file = chosen.or_else(default_control_file);

    // Created 0700 before anything is advertised into it — but **only** the
    // directory this binary chose. `session_port` already exempts a path
    // someone typed on the same principle, and applying the check here anyway
    // broke a documented invocation: SKILL.md tells an agent to give each
    // concurrent session its own `--control-file`, and
    // `serve --control-file /tmp/a.control` then aborted on `/tmp` being
    // mode 1777 before opening anything.
    if defaulted
        && let Some(path) = &control_file
        && let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        make_private_dir(parent)?;
    }
    let action_log = action_log.or_else(|| std::env::var(ACTIONS_VAR).ok().map(PathBuf::from));
    crate::stream::serve(
        factory,
        page,
        crate::stream::ServeOptions {
            addr,
            quality,
            stream_file,
            control_file,
            control_socket,
            action_log,
            once,
            requests,
        },
    )
}

/// Write or print the skill this binary carries.
fn skill(action: SkillCommands) -> Result<(), H5iError> {
    match action {
        SkillCommands::Install { target } => {
            let target = match target {
                Some(path) => path,
                None => crate::skill::default_target()?,
            };
            let written = crate::skill::install(&target)?;
            println!(
                "installed the {} skill ({} page(s), v{}) -> {}",
                crate::skill::NAME,
                written.len(),
                env!("CARGO_PKG_VERSION"),
                target.display()
            );
        }
        SkillCommands::Show => print!("{}", crate::skill::page(None)?),
        SkillCommands::Path => {
            println!("{}", crate::skill::default_target()?.display())
        }
    }
    Ok(())
}

/// Drive the resident session.
fn session(verb: SessionVerb) -> Result<(), H5iError> {
    // Every name comes from `Verb`, so the CLI cannot ask for a verb the session
    // does not have. This used to be eight string literals that happened to
    // match eight others in `stream.rs`, with nothing enforcing the agreement.
    use crate::verbs::Verb;
    let (at, request) = match &verb {
        SessionVerb::Status { at } => (at, serde_json::json!({"verb": Verb::Status.name()})),
        SessionVerb::Snapshot { delta, at } => (
            at,
            serde_json::json!({"verb": Verb::Snapshot.name(), "delta": delta}),
        ),
        SessionVerb::Login { off, on: _, at } => (
            at,
            serde_json::json!({"verb": Verb::Login.name(), "on": !off}),
        ),
        SessionVerb::Navigate { url, at } => (
            at,
            serde_json::json!({"verb": Verb::Navigate.name(), "url": url}),
        ),
        SessionVerb::Scroll { by, at } => (
            at,
            serde_json::json!({"verb": Verb::Scroll.name(), "by": by}),
        ),
        SessionVerb::Type { reference, text, at } => (
            at,
            serde_json::json!({"verb": Verb::Type.name(), "ref": reference, "text": text}),
        ),
        SessionVerb::Submit { reference, at } => (
            at,
            serde_json::json!({"verb": Verb::Submit.name(), "ref": reference}),
        ),
        SessionVerb::Click { reference, at } => (
            at,
            serde_json::json!({"verb": Verb::Click.name(), "ref": reference}),
        ),
        SessionVerb::Requests { since, at } => (
            at,
            serde_json::json!({"verb": Verb::Requests.name(), "since": since}),
        ),
        SessionVerb::WaitFor {
            selector,
            text,
            at,
        } => (
            at,
            serde_json::json!({
                "verb": Verb::WaitFor.name(),
                "selector": selector,
                "text": text,
            }),
        ),
        SessionVerb::WaitForScript { expr, at } => (
            at,
            serde_json::json!({"verb": Verb::WaitForScript.name(), "expr": expr}),
        ),
        SessionVerb::Extract { schema, at } => {
            // Parsed here so a typo is a message from the CLI rather than a
            // refusal from the far end of a socket.
            let parsed: serde_json::Value = serde_json::from_str(schema).map_err(|e| {
                H5iError::Metadata(format!("the schema is not valid JSON: {e}"))
            })?;
            (
                at,
                serde_json::json!({"verb": Verb::Extract.name(), "schema": parsed}),
            )
        }
        SessionVerb::Markdown { max_bytes, at } => (
            at,
            serde_json::json!({"verb": Verb::Markdown.name(), "max_bytes": max_bytes}),
        ),
        SessionVerb::Env { at } => (at, serde_json::json!({"verb": Verb::Env.name()})),
    };

    // The socket wins when there is one. It is only ever set deliberately —
    // by a flag or by h5i inside a box — and in a box it is the only channel
    // that reaches the session at all.
    let reply = match session_socket(at) {
        Some(path) => crate::stream::ask_unix(&path, &request)?,
        None => crate::stream::ask(session_port(at)?, &request)?,
    };

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
        // Why the full outline arrived when a difference was asked for. Said
        // rather than left to be inferred from the length.
        if let Some(reason) = reply.get("reason").and_then(serde_json::Value::as_str) {
            eprintln!("note: {reason}");
        }
        println!("{text}");
    } else if let Some(message) = reply.get("message").and_then(serde_json::Value::as_str) {
        println!("{message}");
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
/// The Unix control socket to use, if one was named.
///
/// Never guessed: a socket is either passed or put in the environment by
/// whatever started the session. Guessing a path would mean a verb silently
/// talking to a different session that happened to leave a socket behind.
fn session_socket(at: &SessionArgs) -> Option<PathBuf> {
    at.control_socket
        .clone()
        .or_else(|| std::env::var(CONTROL_SOCKET_VAR).ok().map(PathBuf::from))
}

fn session_port(at: &SessionArgs) -> Result<u16, H5iError> {
    if let Some(port) = at.port {
        return Ok(port);
    }
    let explicit = at
        .control_file
        .clone()
        .or_else(|| std::env::var(CONTROL_FILE_VAR).ok().map(PathBuf::from))
        .or_else(|| {
            std::env::var(STREAM_FILE_VAR)
                .ok()
                .map(|s| control_file_beside(Path::new(&s)))
        });

    let explicit_none = explicit.is_none();
    let path = match explicit {
        // Named by the caller or by h5i, which is a deliberate act; the
        // directory check below is for the path nobody named.
        Some(path) => path,
        // Nothing said where the session is, which on a bare host is the
        // ordinary case rather than a mistake: h5i sets those variables inside
        // a box and nothing sets them outside one. `serve` writes here by
        // default, so the documented no-flags path works with no h5i anywhere.
        None => default_control_file().ok_or_else(|| {
            H5iError::Metadata(
                "no session to talk to, and no per-user runtime directory to look in. \
                 Pass --control-file or --port, or set $XDG_RUNTIME_DIR or $HOME."
                    .into(),
            )
        })?,
    };

    // Absence first, and privacy second. The other order made the *first* thing
    // a new standalone user ever sees — running a verb before `serve` — a
    // warning about credentials being redirected to somebody else's listener,
    // when the real answer is "nothing is running yet". A missing directory is
    // not a suspicious one.
    if !path.exists() {
        return Err(H5iError::Metadata(format!(
            "no session is listening ({} does not exist). Start one with \
             `h5i-browser-light serve <url>` — it holds a page open for these verbs to \
             drive — or point at a running one with --control-file or --port.",
            path.display()
        )));
    }

    // Only for the default. A path someone typed is a path someone chose.
    if explicit_none
        && let Some(parent) = path.parent()
        && let Err(why) = check_private_dir(parent)
    {
        return Err(H5iError::Metadata(format!(
            "refusing to read a session port from {}: {why}. A port number there is enough \
             to point `session type` — carrying a substituted credential — at somebody \
             else's listener. Pass --control-file or --port to use it anyway.",
            path.display()
        )));
    }
    crate::stream::read_port_file(&path)
}

/// Where a session advertises itself when nothing else says.
///
/// **Per-user, and never a shared directory.** The file holds a port number,
/// and a port number is enough to point `session type` — with a substituted
/// credential in it — at somebody else's listener. On a multi-user host a
/// default under `/tmp` would make that a one-line attack, so there is no
/// fallback to one: `$XDG_RUNTIME_DIR` first (per-user and 0700 by
/// convention), then a directory under `$HOME`, and then nothing rather than
/// somewhere writable by strangers.
fn default_control_file() -> Option<PathBuf> {
    default_session_dir().map(|dir| dir.join("session.control"))
}

/// Whether a directory is ours alone.
///
/// Ownership and mode rather than a list of bad paths. Blacklisting `/tmp`
/// looked sufficient until a test set `HOME=/tmp` — which happens for real
/// daemons — and the default landed under a world-writable parent anyway.
/// A rule about the directory itself does not have that class of hole.
///
/// Non-Unix has no cheap equivalent, so it answers yes: `LOCALAPPDATA` is
/// per-user by construction, and inventing a Windows ACL check here would be a
/// guess wearing the shape of a guarantee.
#[cfg(unix)]
fn check_private_dir(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;

    let meta = std::fs::metadata(dir).map_err(|e| format!("cannot inspect {}: {e}", dir.display()))?;
    if !meta.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }
    // Safe: the only caller is this process reading its own runtime dir.
    let me = unsafe { libc_getuid() };
    if meta.uid() != me {
        return Err(format!(
            "{} is owned by uid {} and this process is uid {me}",
            dir.display(),
            meta.uid()
        ));
    }
    if meta.mode() & 0o022 != 0 {
        return Err(format!(
            "{} is writable by group or other (mode {:o})",
            dir.display(),
            meta.mode() & 0o777
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_private_dir(_dir: &Path) -> Result<(), String> {
    Ok(())
}

/// `getuid`, without taking a dependency on `libc` for one call.
#[cfg(unix)]
unsafe fn libc_getuid() -> u32 {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

/// Create the session directory, private to this user.
///
/// `serve` calls this before advertising. 0700 at creation rather than a
/// chmod afterwards, so there is no window in which it exists and is readable.
#[cfg(unix)]
fn make_private_dir(dir: &Path) -> Result<(), H5iError> {
    use std::os::unix::fs::DirBuilderExt;
    if dir.exists() {
        return check_private_dir(dir).map_err(H5iError::Metadata);
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| H5iError::with_path(e, dir))
}

#[cfg(not(unix))]
fn make_private_dir(dir: &Path) -> Result<(), H5iError> {
    std::fs::create_dir_all(dir).map_err(|e| H5iError::with_path(e, dir))
}

fn default_session_dir() -> Option<PathBuf> {
    if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR")
        && !runtime.trim().is_empty()
    {
        return Some(PathBuf::from(runtime).join("h5i-browser-light"));
    }
    // `LOCALAPPDATA` on Windows, `HOME` elsewhere. Both are per-user.
    let home = std::env::var("LOCALAPPDATA")
        .ok()
        .or_else(|| std::env::var("HOME").ok())
        .filter(|h| !h.trim().is_empty())?;
    Some(
        PathBuf::from(home)
            .join(".cache")
            .join("h5i-browser-light"),
    )
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
            // Machine-readable forms of what the snapshot says in prose, so a
            // caller aggregating across many pages does not have to parse
            // sentences back out of the outline.
            "unsupported": page
                .unsupported()
                .into_iter()
                .map(|(name, count)| serde_json::json!({ "api": name, "calls": count }))
                .collect::<Vec<_>>(),
            "console": page
                .console()
                .into_iter()
                .map(|line| {
                    serde_json::json!({
                        "level": line.level,
                        "text": line.text,
                        // Which of the two is talking. "the site reported an
                        // error" and "the browser could not do something" call
                        // for different responses and were indistinguishable.
                        "source": line.source,
                        "repeats": line.repeats,
                    })
                })
                .collect::<Vec<_>>(),
            "settled": page.settled().map(|s| s.render()),
            "script": page.has_script(),
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
            .filter(|r| r.phase == crate::receipt::Phase::Response)
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

    /// The default session path is per-user, or absent.
    ///
    /// Guarded because the failure is silent and serious: a shared directory
    /// would let any local user publish a port and receive the next
    /// `session type` — including one carrying a substituted credential.
    #[test]
    fn the_default_session_directory_is_never_shared() {
        // Nothing to go on: no path at all rather than a guess.
        temp_env(&[("XDG_RUNTIME_DIR", None), ("HOME", None), ("LOCALAPPDATA", None)], || {
            assert!(default_control_file().is_none());
        });

        // A runtime dir is preferred, because it is per-user and 0700.
        temp_env(&[("XDG_RUNTIME_DIR", Some("/run/user/1000")), ("HOME", Some("/home/a"))], || {
            let path = default_control_file().expect("a path");
            assert!(
                path.starts_with("/run/user/1000"),
                "{}",
                path.display()
            );
        });

        // Falling back to HOME, still per-user.
        temp_env(&[("XDG_RUNTIME_DIR", Some("")), ("HOME", Some("/home/a"))], || {
            let path = default_control_file().expect("a path");
            assert!(path.starts_with("/home/a"), "{}", path.display());
        });

    }

    #[cfg(unix)]
    #[test]
    fn a_session_directory_somebody_else_can_write_is_refused() {
        // The rule that replaced a blacklist. Setting `HOME=/tmp` — which real
        // daemons do — put the default under a world-writable parent, and no
        // list of bad paths would have caught it. This checks the directory
        // instead, which does not have that class of hole.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");

        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
        assert!(check_private_dir(dir.path()).is_ok());

        for mode in [0o777, 0o770, 0o707, 0o722] {
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(mode))
                .expect("chmod");
            let why = check_private_dir(dir.path())
                .expect_err(&format!("mode {mode:o} should be refused"));
            assert!(why.contains("writable"), "{why}");
        }

        // And a path that is not a directory at all.
        let file = dir.path().join("f");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
        std::fs::write(&file, "1").expect("write");
        assert!(check_private_dir(&file).is_err());
    }

    #[test]
    fn a_control_file_the_caller_named_is_not_second_guessed() {
        // SKILL.md tells an agent to give each concurrent session its own
        // `--control-file`. Applying the private-directory rule to a path
        // somebody typed made `serve --control-file /tmp/a.control` abort on
        // `/tmp` being mode 1777, before it opened anything — a documented
        // invocation refused by a guard meant for the path nobody chose.
        //
        // `session_port` already drew that line; this is the same line on the
        // serving side, asserted through the predicate they share.
        assert!(
            check_private_dir(std::path::Path::new("/tmp")).is_err(),
            "the fixture assumes /tmp is world-writable"
        );
        // The rule still applies to a default, and default_control_file never
        // points at a shared directory in the first place.
        temp_env(&[("XDG_RUNTIME_DIR", Some("/run/user/1000"))], || {
            let path = default_control_file().expect("a path");
            assert!(!path.starts_with("/tmp"), "{}", path.display());
        });
    }

    #[cfg(unix)]
    #[test]
    fn serve_creates_its_session_directory_private() {
        use std::os::unix::fs::PermissionsExt;
        let base = tempfile::tempdir().expect("tempdir");
        let dir = base.path().join("nested").join("session");
        make_private_dir(&dir).expect("created");
        let mode = std::fs::metadata(&dir).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "created {:o}", mode & 0o777);
    }

    #[test]
    fn serve_and_the_verbs_agree_on_where_a_session_lives() {
        // The two halves have to name the same file or the verbs look
        // somewhere `serve` never wrote, which reads as "no session running"
        // on a host where one is.
        temp_env(&[("XDG_RUNTIME_DIR", Some("/run/user/4242"))], || {
            let path = default_control_file().expect("a path");
            assert_eq!(
                path,
                std::path::PathBuf::from("/run/user/4242/h5i-browser-light/session.control")
            );
        });
    }

    /// Set some environment variables, run, and put them back.
    ///
    /// Serialised on a mutex: these tests write process-global state, and the
    /// test harness runs them on threads.
    fn temp_env(vars: &[(&str, Option<&str>)], body: impl FnOnce()) {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved: Vec<(String, Option<String>)> = vars
            .iter()
            .map(|(name, _)| (name.to_string(), std::env::var(name).ok()))
            .collect();
        for (name, value) in vars {
            match value {
                Some(v) => unsafe { std::env::set_var(name, v) },
                None => unsafe { std::env::remove_var(name) },
            }
        }
        body();
        for (name, value) in saved {
            match value {
                Some(v) => unsafe { std::env::set_var(&name, v) },
                None => unsafe { std::env::remove_var(&name) },
            }
        }
    }

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
        // The gate ROADMAP.md §B3.3 asks for: script is a decision someone
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
