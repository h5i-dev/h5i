//! `h5i browser`: sessions, and the verbs that drive them.
//!
//! A session is h5i's capture proxy (`h5i __capture-proxy`), which checks and
//! records every request, plus a Chromium run by agent-browser and pointed at
//! it. Page verbs go to the Chromium; workbench verbs (`requests`, `resend`,
//! `socket`, `grpc`) go to the proxy's control channel.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use clap::Subcommand;
use console::style;
use serde_json::{Value, json};

use h5i_core::browser_session as bs;
use h5i_core::ui::SUCCESS;

/// The most sends one `--set-each` or `--walk` may ask for, as for `--repeat`.
const MAX_WALK_STEPS: usize = 1000;

/// How long `proxy` waits for the capture proxy to advertise itself.
const PROXY_READY_TIMEOUT: Duration = Duration::from_secs(15);

/// Read a file named by a flag, or standard input when it is `-`.
fn read_arg_file(flag: &str, path: &str) -> anyhow::Result<String> {
    if path == "-" {
        use std::io::Read as _;
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|e| anyhow::anyhow!("{flag} -: stdin could not be read: {e}"))?;
        return Ok(text);
    }
    std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("{flag}: {path} could not be read: {e}"))
}

/// What a new session is allowed, fixed when its proxy starts.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct ProxyGrants {
    /// The engagement this session belongs to. When
    /// `~/.config/h5i/projects/<name>.toml` exists it is also the scope: its
    /// grants join `--allow` and its deny rules are checked before the wire.
    #[arg(long, value_name = "NAME")]
    pub project: Option<String>,
    /// Grant another origin. Repeatable. Without any, nothing remote is
    /// reachable except the URL's own origin.
    #[arg(long = "allow", value_name = "ORIGIN")]
    pub allow: Vec<String>,
    /// Refuse loopback too. It is reachable by default: it is the dev server.
    #[arg(long)]
    pub no_loopback: bool,
    /// End the session automatically after this many seconds. `status` still
    /// answers afterwards, and says it expired.
    #[arg(long, value_name = "SECONDS")]
    pub expires_in: Option<u64>,
    /// Instrument pages for prototype pollution and DOM-XSS: inject the DOM
    /// instrument into HTML and collect what reaches a sink, for
    /// `h5i websec dom scan`.
    #[arg(long = "dom-instrument")]
    pub dom_instrument: bool,
}

impl ProxyGrants {
    /// The flags that only mean something when a session is made, by name, so
    /// `open` can refuse them on a session that already exists rather than
    /// accept a grant and do nothing with it.
    fn given(&self) -> Vec<&'static str> {
        let mut given = Vec::new();
        if self.project.is_some() {
            given.push("--project");
        }
        if !self.allow.is_empty() {
            given.push("--allow");
        }
        if self.no_loopback {
            given.push("--no-loopback");
        }
        if self.expires_in.is_some() {
            given.push("--expires-in");
        }
        if self.dom_instrument {
            given.push("--dom-instrument");
        }
        given
    }
}

#[derive(Subcommand)]
pub enum BrowserCommands {
    /// Open a URL, making a session if there is not one already.
    ///
    /// A new session starts its capture proxy and opens the URL in a Chromium
    /// pointed at it, and becomes the default; an existing one goes there.
    Open {
        /// The URL to open.
        url: String,
        /// Name this session, so several can run at once. A name is reusable
        /// once its session ends; the id in `--json` is the durable reference.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        /// Make a new session even if one is already open, rather than
        /// navigating it.
        #[arg(long)]
        new: bool,
        #[command(flatten)]
        grants: ProxyGrants,
        /// Print the session record as JSON instead of a summary.
        #[arg(long)]
        json: bool,
    },

    /// Start a capture session: an h5i HTTP/S proxy for a Chromium to go through.
    ///
    /// Prints the `agent-browser` flags for it, which pass
    /// `--ignore-https-errors`. `h5i browser open <url>` does this and opens the
    /// page in one step.
    Proxy {
        /// The first target, used to seed the origin allowlist.
        url: String,
        /// Name this session.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        #[command(flatten)]
        grants: ProxyGrants,
        /// Start without a browser: for a session driven only by `resend` and
        /// the plugins, or a browser you point at the proxy yourself.
        #[arg(long)]
        no_browser: bool,
        /// Print the session record as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Open a URL in the Chromium a session drives. The same as `navigate`.
    ProxyOpen {
        /// The URL to open.
        url: String,
        /// The session to drive.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        /// Print the result as JSON.
        #[arg(long)]
        json: bool,
    },

    /// List the browser sessions on this machine.
    List {
        /// Include sessions that have ended. They are kept: the record of how a
        /// session ended is the part a reviewer needs.
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },

    /// What a session is, where it runs, and who saw its network.
    Status {
        /// Which session, when more than one is open. A name from
        /// `--session` at open time, or an opaque id. Defaults to
        /// $H5I_BROWSER_SESSION, then to the session `open` last made.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// End a session: its proxy and its Chromium. Its record stays.
    Close {
        /// Which session, when more than one is open. A name from
        /// `--session` at open time, or an opaque id. Defaults to
        /// $H5I_BROWSER_SESSION, then to the session `open` last made.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        /// End every live session on this machine.
        #[arg(long, conflicts_with = "session")]
        all: bool,
        /// Delete the captured messages (bodies and credentials in full) as
        /// well. The record and the request log stay.
        #[arg(long = "capture-drop")]
        capture_drop: bool,
        #[arg(long)]
        json: bool,
    },

    /// Erase sessions: their records, logs and stored messages.
    ///
    /// `close` keeps a session's account; this removes it. One failure does not
    /// abort the rest.
    Rm {
        /// One or more session names or ids.
        #[arg(num_args = 1..)]
        names: Vec<String>,
        /// Remove every session that has ended, rather than naming them.
        #[arg(long, conflicts_with = "names")]
        ended: bool,
        /// Remove every record on this machine. A live session still needs
        /// `--force`.
        #[arg(long, conflicts_with_all = ["names", "ended"])]
        all: bool,
        /// Only sessions that ended at least this long ago, in days. On its
        /// own it means `--ended`, since a live session has no ending to be
        /// older than.
        #[arg(long = "older-than", value_name = "DAYS", conflicts_with = "names")]
        older_than: Option<u64>,
        /// Say what would be removed, and remove nothing.
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// Remove even a session that is still live. Its proxy is asked to
        /// stop first.
        #[arg(long)]
        force: bool,
        #[arg(long)]
        json: bool,
    },

    /// Reclaim the stored messages of ended sessions, keeping their records,
    /// request logs and recon ledgers. The bulk form of `close --capture-drop`.
    Gc {
        /// Only sessions that ended at least this long ago, in days. `0`
        /// reclaims from every ended session.
        #[arg(long = "older-than", value_name = "DAYS", default_value_t = 7)]
        older_than: u64,
        /// Say what would be reclaimed, and reclaim nothing.
        #[arg(long = "dry-run")]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },

    /// The page as a model should read it: the accessibility outline, with
    /// `@ref` handles the other page verbs take.
    Snapshot {
        /// Which session, when more than one is open.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        /// Only the interactive elements: what can be clicked, typed into or
        /// chosen.
        #[arg(long, short = 'i')]
        interactive: bool,
        /// Report only what changed since the last snapshot.
        #[arg(long)]
        delta: bool,
        /// Go here first, then read.
        #[arg(long, value_name = "URL")]
        url: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Go to a URL in this session's Chromium.
    Navigate {
        /// Which session, when more than one is open.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        url: String,
        #[arg(long)]
        json: bool,
    },

    /// Load the current page again.
    Reload {
        /// Which session, when more than one is open.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Write a PNG of the page this session is on.
    Screenshot {
        /// Which session, when more than one is open.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        /// Where to write it. Defaults to a file h5i names in the session's own
        /// artifacts directory.
        #[arg(long, value_name = "PATH")]
        out: Option<PathBuf>,
        /// The whole page, not only the viewport.
        #[arg(long)]
        full: bool,
        /// Go here first, then paint.
        #[arg(long, value_name = "URL")]
        url: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Click an element: `@e3` from a snapshot, or a CSS selector.
    Click {
        /// `@e3` from a `snapshot`, or a CSS selector.
        #[arg(value_name = "TARGET")]
        target: String,
        /// Which session, when more than one is open.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Put text into a field, replacing what was there.
    ///
    /// `$H5I_SECRET_<NAME>` is resolved from h5i's environment and masked in
    /// everything printed or kept. `h5i browser env` names the ones there are.
    Type {
        /// `@e3` from a `snapshot`, or a CSS selector.
        #[arg(value_name = "TARGET")]
        target: String,
        /// The text.
        text: String,
        /// Which session, when more than one is open.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Press a key that *does* something: Enter, Escape, Tab. To enter text use
    /// `type`.
    Press {
        /// The key, as `Enter`, `Tab` or `Control+a`.
        key: String,
        /// Which session, when more than one is open.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Choose an option in a `<select>`, by its value.
    Select {
        /// `@e3` from a `snapshot`, or a CSS selector.
        #[arg(value_name = "TARGET")]
        target: String,
        /// The option's value.
        option: String,
        /// Which session, when more than one is open.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Set a checkbox or radio to a state, rather than toggling it with a click.
    SetChecked {
        /// `@e3` from a `snapshot`, or a CSS selector.
        #[arg(value_name = "TARGET")]
        target: String,
        /// `true` or `false`.
        #[arg(action = clap::ArgAction::Set, value_parser = clap::value_parser!(bool))]
        checked: bool,
        /// Which session, when more than one is open.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Scroll the page by this many pixels. Negative scrolls up.
    Scroll {
        #[arg(allow_negative_numbers = true)]
        by: i64,
        /// Which session, when more than one is open.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Wait until an element, or some text, is on the page.
    WaitFor {
        /// Which session, when more than one is open.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        /// A CSS selector or `@ref` to wait for.
        #[arg(long, value_name = "CSS", required_unless_present = "text")]
        selector: Option<String>,
        /// Text to wait for.
        #[arg(long, value_name = "TEXT", conflicts_with = "selector")]
        text: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Wait until a page expression is true.
    WaitForScript {
        /// Which session, when more than one is open.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        /// The JavaScript expression.
        expr: String,
        #[arg(long)]
        json: bool,
    },

    /// Run JavaScript in the page this session's Chromium is on, and print the
    /// result. How a DOM-side effect (an XSS that fires) is confirmed; the
    /// result is kept in the session's artifacts.
    Eval {
        /// The JavaScript to evaluate in the current page.
        #[arg(value_name = "JS")]
        js: String,
        /// Which session, when more than one is open.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Print the console log of this session's Chromium. Kept in the session's
    /// artifacts.
    Console {
        /// Clear the log after reading it.
        #[arg(long)]
        clear: bool,
        /// Which session, when more than one is open.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Open a WebSocket, send frames, and report what came back.
    ///
    /// One exchange through the session's policy and receipts: it opens, sends,
    /// listens for `--wait-ms`, and closes.
    Socket {
        /// The `ws://` or `wss://` endpoint to open.
        #[arg(value_name = "URL")]
        url: String,
        /// A text frame to send. Repeatable, and sent in the order given.
        #[arg(long = "send", value_name = "TEXT")]
        send: Vec<String>,
        /// How long to listen for replies, in milliseconds.
        #[arg(long = "wait-ms", value_name = "MS", default_value = "5000")]
        wait_ms: u64,
        /// Which session, when more than one is open.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Make one gRPC call over HTTP/2, through the same policy, budget and
    /// receipts as `resend`. Framed bytes in and out; `h5i websec grpc` speaks
    /// JSON.
    Grpc {
        /// The http(s) endpoint, e.g. `http://host:50051`.
        #[arg(value_name = "URL")]
        url: String,
        /// The `:path`, `/package.Service/Method`.
        #[arg(long, value_name = "PATH")]
        path: String,
        /// The base64 of the already-framed request body.
        #[arg(long = "request-frame", value_name = "BASE64")]
        request_frame: Option<String>,
        /// `name:value` gRPC metadata. Repeatable.
        #[arg(long = "metadata", value_name = "NAME:VALUE")]
        metadata: Vec<String>,
        /// Skip TLS certificate verification, for a target under test.
        #[arg(long)]
        insecure: bool,
        /// Give up after this many milliseconds.
        #[arg(long = "timeout-ms", value_name = "MS")]
        timeout_ms: Option<u64>,
        /// Override the `:authority` header.
        #[arg(long, value_name = "HOST")]
        authority: Option<String>,
        /// Which session, when more than one is open.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Everything recorded about this session, in one ordered timeline: verbs,
    /// requests, who was driving, and how it ended.
    Audit {
        /// Which session, when more than one is open. A name from
        /// `--session` at open time, or an opaque id. Defaults to
        /// $H5I_BROWSER_SESSION, then to the session `open` last made.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// The request log: what this session sent, and what was refused. Written
    /// before the bytes moved.
    Requests {
        /// Which session, when more than one is open. A name from
        /// `--session` at open time, or an opaque id. Defaults to
        /// $H5I_BROWSER_SESSION, then to the session `open` last made.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        /// Only what happened after this sequence number.
        #[arg(long, value_name = "SEQ")]
        since: Option<u64>,
        /// Only this method.
        #[arg(long, value_name = "METHOD")]
        method: Option<String>,
        /// Only rows whose URL contains this.
        #[arg(long, value_name = "TEXT")]
        url_contains: Option<String>,
        /// Only responses with this status.
        #[arg(long, value_name = "CODE")]
        status: Option<u16>,
        /// Only `subresource` (what the browser sent through the proxy),
        /// `redirect`, or `replay` (what h5i sent).
        #[arg(long, value_name = "KIND")]
        initiator: Option<String>,
        /// Only what policy refused.
        #[arg(long)]
        denied_only: bool,
        /// At most this many rows, newest last.
        #[arg(long, value_name = "N")]
        limit: Option<u64>,
        #[arg(long)]
        json: bool,
        /// Emit the human-readable view (the default).
        #[arg(long, conflicts_with = "json")]
        human: bool,
    },

    /// Run a multi-step flow: send, extract, send again with what was found.
    ///
    /// For a token minted by the request before. Steps stop at the first
    /// failure. The file is JSON:
    ///
    /// ```json
    /// {"steps": [
    ///   {"resend": 3, "extract": {"csrf": "regex:value=\"([^\"]+)\""}},
    ///   {"resend": 5, "set": ["header.X-CSRF-Token=${csrf}", "json.role=admin"]}
    /// ]}
    /// ```
    Sequence {
        /// The sequence file.
        #[arg(value_name = "FILE")]
        file: PathBuf,
        /// Bind a name before the first step, as `name=value`. Repeatable.
        #[arg(long = "var", value_name = "NAME=VALUE")]
        vars: Vec<String>,
        /// Run every step even after one fails, to read a whole file's failures
        /// at once.
        #[arg(long)]
        keep_going: bool,
        /// Which session, when more than one is open.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        #[arg(long)]
        json: bool,
    },





    /// Send one of this session's own requests again, with changes.
    ///
    /// Through the same policy and receipts; a replay gets its own sequence
    /// number and stored message, so it is replayable too.
    Resend {
        /// The sequence number to send again, as `requests` lists them.
        #[arg(value_name = "SEQ")]
        from: u64,
        /// Change one part of it: `query.id=456`, `header.X-Real-IP=127.0.0.1`,
        /// `cookie.session=forged`, `json.role=admin`, `form.user=admin`,
        /// `path=/admin`, `method=POST`, `body.raw=<bytes>`. Repeatable, in
        /// order; the value is everything after the first `=`.
        ///
        /// A `json.` value is typed the way it reads (`json.id=99` is a number);
        /// quote it to insist on a string.
        #[arg(long = "set", value_name = "TARGET=VALUE")]
        set: Vec<String>,
        /// Set one part of it to a file's bytes: `multipart.userfile=./payload.jpg`.
        /// Applied after every `--set`.
        #[arg(long = "set-file", value_name = "TARGET=PATH")]
        set_file: Vec<String>,
        /// Remove one part of it, by the same names.
        #[arg(long = "unset", value_name = "TARGET")]
        unset: Vec<String>,
        /// Add a target that is not there rather than refusing it as a typo.
        #[arg(long)]
        create: bool,
        /// Send it this many times and report every sample, with a median and a
        /// median absolute deviation, for a timing test.
        #[arg(long, default_value_t = 1, value_name = "N")]
        repeat: u32,
        /// Release the repeated sends together, for a check-then-act race. A
        /// burst from threads that meet at a barrier, not a single-packet attack.
        #[arg(long, requires = "repeat")]
        race: bool,
        /// Stop at the first redirect and report it, rather than following it.
        #[arg(long)]
        no_follow: bool,
        /// Start this session's request allowance again before sending, for a
        /// long walk that would otherwise stop partway.
        #[arg(long)]
        reset_budget: bool,
        /// Send it from another session instead: the authorization test. The
        /// message comes from `--session`, with its credentials dropped; the
        /// cookies, policy and receipts are the named session's.
        #[arg(long = "as", value_name = "SESSION")]
        as_session: Option<String>,
        /// Carry the source session's `Cookie` and `Authorization` headers into
        /// the other session. Only meaningful with `--as`.
        #[arg(long, requires = "as_session")]
        keep_credentials: bool,
        /// Send this request-target unchanged, without URL normalization.
        #[arg(long = "raw-target", value_name = "TARGET")]
        raw_target: Option<String>,
        /// Preserve header-name casing while retaining edits and cookies.
        #[arg(long = "raw-headers")]
        raw_headers: bool,
        /// Send once per line, as `query.id=./ids.txt`, after shared edits.
        #[arg(long = "set-each", value_name = "TARGET=PATH")]
        set_each: Option<String>,
        /// Send a complete request unchanged from this file; `-` reads stdin.
        ///
        /// The stored URL supplies the authority for policy checks and dialing.
        #[arg(long = "raw-request", value_name = "PATH")]
        raw_request: Option<String>,
        /// Walk a list of sends from this file (`-` reads stdin):
        /// `{"steps":[{"label":"a","set":["query.user=alice"]},…]}`.
        #[arg(long = "walk", value_name = "PATH", conflicts_with = "set_each")]
        walk: Option<String>,
        /// Send at most this many requests per second.
        #[arg(long, value_name = "PER_SECOND")]
        rate: Option<f64>,
        /// Which session, when more than one is open.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Names that moved to the workbench plugin, kept hidden so a script that
    /// used one is told where it went (design-websec.md W21).
    #[command(name = "message", hide = true)]
    MovedMessage {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<std::ffi::OsString>,
    },
    #[command(name = "diff", hide = true)]
    MovedDiff {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<std::ffi::OsString>,
    },
    #[command(name = "match", hide = true)]
    MovedMatch {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<std::ffi::OsString>,
    },
    #[command(name = "sitemap", hide = true)]
    MovedSitemap {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<std::ffi::OsString>,
    },

    /// Speak one request per line, and answer one per line, without a process
    /// per verb. Each line carries an `id` and a `verb`; the reply has the `id`.
    Rpc {
        /// Read requests from stdin and write replies to stdout.
        #[arg(long)]
        stdio: bool,
        /// Which session, when more than one is open.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
    },

    /// Which credentials this session can use, by name. Never their values.
    Env {
        /// Which session, when more than one is open. A name from
        /// `--session` at open time, or an opaque id. Defaults to
        /// $H5I_BROWSER_SESSION, then to the session `open` last made.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Take control as a human. The agent's automation pauses at its next verb.
    Take {
        /// Which session, when more than one is open.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
    },

    /// Hand control back to the agent. It must re-snapshot before acting,
    /// because the page moved while you were driving.
    Release {
        /// Which session, when more than one is open.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
    },


    /// Print the loopback viewer URL for a box, token included. `h5i box view`
    /// is what actually serves it; this is for pasting into a browser when a
    /// forward is already running.
    Url {
        /// A box name.
        name: String,
        #[arg(long, default_value_t = 7331)]
        port: u16,
    },
}

pub fn run(action: BrowserCommands) -> anyhow::Result<()> {
    let root = bs::root()?;
    // Cheap, and it means every entry point sweeps: there is no daemon to hold
    // a timer, so expiry happens the next time anyone looks.
    expire_and_stop(&root);

    match action {
        BrowserCommands::Proxy {
            url,
            session,
            grants,
            no_browser,
            json,
        } => {
            let session = start_proxy(&root, &url, session, &grants, !no_browser)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&session)?);
            } else {
                print_started(&session, &url, !no_browser);
            }
            Ok(())
        }
        BrowserCommands::ProxyOpen { url, session, json } => {
            proxy_open(&root, &url, session.as_deref(), json)
        }
        BrowserCommands::Open {
            url,
            session,
            new,
            grants,
            json,
        } => open(&root, url, session, new, grants, json),
        BrowserCommands::List { all, json } => list(&root, all, json),
        BrowserCommands::Status { session, json } => status(&root, session.as_deref(), json),
        BrowserCommands::Close {
            session,
            all,
            capture_drop,
            json,
        } => close(&root, session.as_deref(), all, capture_drop, json),

        BrowserCommands::Rm {
            names,
            ended,
            all,
            older_than,
            dry_run,
            force,
            json,
        } => rm(&root, &names, ended, all, older_than, dry_run, force, json),
        BrowserCommands::Gc {
            older_than,
            dry_run,
            json,
        } => gc(&root, older_than, dry_run, json),

        BrowserCommands::Snapshot {
            session,
            interactive,
            delta,
            url,
            json,
        } => {
            if let Some(url) = url {
                go_to(&root, session.as_deref(), &url)?;
            }
            let mut argv = vec!["snapshot".to_string()];
            if interactive {
                argv.push("-i".into());
            }
            if delta {
                argv.push("--delta".into());
            }
            page(&root, session.as_deref(), argv, "snapshot", false, json)?;
            // A delivered reading is what clears the stale-ref flag a human
            // takeover set: the agent has now seen the page as it is.
            if let Ok(s) = bs::resolve(&root, session.as_deref()) {
                let _ = h5i_core::control::snapshotted(&bs::dir(&root, &s.id));
            }
            Ok(())
        }
        BrowserCommands::Navigate { session, url, json } => {
            proxy_open(&root, &url, session.as_deref(), json)
        }
        BrowserCommands::Reload { session, json } => {
            page(&root, session.as_deref(), vec!["reload".into()], "reload", true, json)
        }
        BrowserCommands::Screenshot {
            session,
            out,
            full,
            url,
            json,
        } => screenshot(&root, session.as_deref(), out, full, url, json),
        BrowserCommands::Click {
            target,
            session,
            json,
        } => page(&root, session.as_deref(), vec!["click".into(), target], "click", true, json),
        BrowserCommands::Type {
            target,
            text,
            session,
            json,
        } => {
            // Resolved here; what is kept and printed has the placeholder.
            let resolved = substitute_secrets(&text)?;
            page_with(
                &root,
                session.as_deref(),
                vec!["fill".into(), target.clone(), resolved],
                vec!["fill".into(), target, text],
                "type",
                true,
                json,
            )
        }
        BrowserCommands::Press { key, session, json } => {
            page(&root, session.as_deref(), vec!["press".into(), key], "press", true, json)
        }
        BrowserCommands::Select {
            target,
            option,
            session,
            json,
        } => page(
            &root,
            session.as_deref(),
            vec!["select".into(), target, option],
            "select",
            true,
            json,
        ),
        BrowserCommands::SetChecked {
            target,
            checked,
            session,
            json,
        } => {
            let verb = if checked { "check" } else { "uncheck" };
            page(&root, session.as_deref(), vec![verb.into(), target], "set-checked", true, json)
        }
        BrowserCommands::Scroll { by, session, json } => {
            let direction = if by < 0 { "up" } else { "down" };
            page(
                &root,
                session.as_deref(),
                vec!["scroll".into(), direction.into(), by.unsigned_abs().to_string()],
                "scroll",
                true,
                json,
            )
        }
        BrowserCommands::WaitFor {
            session,
            selector,
            text,
            json,
        } => {
            let argv = match (selector, text) {
                (Some(selector), _) => vec!["wait".to_string(), selector],
                (None, Some(text)) => vec!["wait".to_string(), "--text".into(), text],
                (None, None) => anyhow::bail!("`wait-for` needs --selector or --text"),
            };
            page(&root, session.as_deref(), argv, "wait-for", false, json)
        }
        BrowserCommands::WaitForScript {
            session,
            expr,
            json,
        } => page(
            &root,
            session.as_deref(),
            vec!["wait".into(), "--fn".into(), expr],
            "wait-for-script",
            false,
            json,
        ),
        BrowserCommands::Eval { js, session, json } => {
            page(&root, session.as_deref(), vec!["eval".into(), js], "eval", true, json)
        }
        BrowserCommands::Console {
            clear,
            session,
            json,
        } => {
            let mut argv = vec!["console".to_string()];
            if clear {
                argv.push("--clear".into());
            }
            page(&root, session.as_deref(), argv, "console", false, json)
        }
        BrowserCommands::Socket {
            url,
            send,
            wait_ms,
            session,
            json,
        } => verb(
            &root,
            session.as_deref(),
            json!({"verb": "socket", "url": url, "send": send, "wait_ms": wait_ms}),
            true,
            json,
        ),
        BrowserCommands::Grpc {
            url,
            path,
            request_frame,
            metadata,
            insecure,
            timeout_ms,
            authority,
            session,
            json,
        } => {
            // `name:value` metadata, split once so a value may hold a colon.
            let metadata: Vec<[String; 2]> = metadata
                .iter()
                .filter_map(|pair| {
                    pair.split_once(':')
                        .map(|(name, value)| [name.trim().to_string(), value.trim().to_string()])
                })
                .collect();
            // Mutating: it puts bytes on the wire under the session's identity.
            verb(
                &root,
                session.as_deref(),
                json!({
                    "verb": "grpc",
                    "url": url,
                    "path": path,
                    "request_frame_b64": request_frame,
                    "metadata": metadata,
                    "insecure": insecure,
                    "timeout_ms": timeout_ms,
                    "authority": authority,
                }),
                true,
                json,
            )
        }
        BrowserCommands::Requests {
            session,
            since,
            method,
            url_contains,
            status,
            initiator,
            denied_only,
            limit,
            json,
            human: _,
        } => verb(
            &root,
            session.as_deref(),
            json!({
                "verb": "requests",
                "since": since,
                "method": method,
                "url_contains": url_contains,
                "status": status,
                "initiator": initiator,
                "denied_only": denied_only,
                "limit": limit,
            }),
            false,
            json,
        ),
        BrowserCommands::Sequence {
            file,
            vars,
            keep_going,
            session,
            json,
        } => {
            let bindings: Vec<(String, String)> = vars
                .into_iter()
                .map(|spec| match spec.split_once('=') {
                    Some((name, value)) => (name.to_string(), value.to_string()),
                    None => (spec, String::new()),
                })
                .collect();
            super::websec::sequence(
                &root,
                session.as_deref(),
                &file,
                &bindings,
                keep_going,
                json,
            )
        }
        BrowserCommands::Resend {
            from,
            set,
            set_file,
            unset,
            create,
            repeat,
            race,
            no_follow,
            reset_budget,
            as_session,
            keep_credentials,
            raw_target,
            raw_headers,
            set_each,
            raw_request,
            walk,
            rate,
            session,
            json,
        } => {
            use base64::Engine as _;
            let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
            let mut request = json!({
                "verb": "resend",
                "set": set,
                "unset": unset,
                "create": create,
                "repeat": repeat.max(1),
                "together": race,
                "no_follow": no_follow,
                "reset_budget": reset_budget,
                "raw_target": raw_target,
                "raw_headers": raw_headers,
            });
            // With `--as`, the stored request is read here and handed to the
            // other session whole; what was dropped goes in the reply too.
            let mut dropped: Vec<String> = Vec::new();
            match &as_session {
                None => request["from"] = json!(from),
                Some(_) => {
                    let (carried, left_behind) = super::websec::carry(
                        &root,
                        session.as_deref(),
                        from,
                        keep_credentials,
                    )?;
                    dropped = left_behind;
                    if !dropped.is_empty() && !json {
                        println!(
                            "  note     : {} left behind, so this is the other session's \
                             own request",
                            dropped.join(", ")
                        );
                    }
                    request["request"] = carried;
                }
            }
            // Read here and carried as base64: this hop is a JSON control
            // message, and the point of the flag is bytes JSON cannot hold.
            let mut from_files: Vec<(String, String)> = Vec::new();
            for spec in set_file {
                let (target, path) = spec.split_once('=').ok_or_else(|| {
                    anyhow::anyhow!(
                        "`--set-file {spec}` needs `target=path` — the field to set, then the \
                         file to read it from, joined by `=` (for example \
                         `--set-file body.raw=./payload.bin`). This has no `=`"
                    )
                })?;
                from_files.push((target.to_string(), b64(&read_a_payload(target, path)?)));
            }
            request["set_file"] = json!(from_files);
            if let Some(spec) = set_each {
                let (target, path) = spec.split_once('=').ok_or_else(|| {
                    anyhow::anyhow!(
                        "--set-each takes `target=path`, as `query.id=./ids.txt`: the target to \
                         walk and the file of values to walk it over"
                    )
                })?;
                let text = std::fs::read_to_string(path).map_err(|e| {
                    anyhow::anyhow!("--set-each: {path} could not be read: {e}")
                })?;
                // Preserve empty values; drop only the final newline.
                let mut values: Vec<&str> = text.split('\n').collect();
                if values.last() == Some(&"") {
                    values.pop();
                }
                if values.is_empty() {
                    anyhow::bail!("--set-each: {path} has no values in it");
                }
                // Match the `--repeat` ceiling.
                if values.len() > MAX_WALK_STEPS {
                    anyhow::bail!(
                        "--set-each: {path} has {} values, and {MAX_WALK_STEPS} is the most one \
                         walk sends. Split the file",
                        values.len()
                    );
                }
                let values: Vec<&str> = values.iter().map(|v| v.trim_end_matches('\r')).collect();
                request["each"] = json!({"target": target, "values": values});
            }
            if let Some(path) = walk {
                let text = read_arg_file("--walk", &path)?;
                let parsed: serde_json::Value = serde_json::from_str(&text)
                    .map_err(|e| anyhow::anyhow!("--walk: {path} is not JSON: {e}"))?;
                let steps = parsed
                    .get("steps")
                    .and_then(|steps| steps.as_array())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "--walk: {path} has no `steps` list. One send per step, as \
                             `{{\"steps\":[{{\"label\":\"alice\",\"set\":\
                             [\"query.user=alice\"]}}]}}`"
                        )
                    })?;
                if steps.is_empty() {
                    anyhow::bail!("--walk: {path} has no steps in it");
                }
                // The same ceiling `--set-each` has, for the same reason.
                if steps.len() > MAX_WALK_STEPS {
                    anyhow::bail!(
                        "--walk: {path} has {} steps, and {MAX_WALK_STEPS} is the most one walk \
                         sends. Split the file",
                        steps.len()
                    );
                }
                // The same number a single request line may be.
                let compact = parsed.to_string();
                if compact.len() > RPC_MAX_LINE {
                    anyhow::bail!(
                        "--walk: {path} is {} bytes of steps, and {RPC_MAX_LINE} is the most one \
                         walk carries. Split the file",
                        compact.len()
                    );
                }
                request["each"] = parsed;
            }
            if let Some(rate) = rate {
                if rate <= 0.0 || !rate.is_finite() {
                    anyhow::bail!("--rate is sends per second and has to be a positive number");
                }
                request["rate"] = json!(rate);
            }
            if let Some(path) = raw_request {
                let bytes = if path == "-" {
                    use std::io::Read as _;
                    let mut buf = Vec::new();
                    std::io::stdin().read_to_end(&mut buf).map_err(|e| {
                        anyhow::anyhow!("--raw-request -: stdin could not be read: {e}")
                    })?;
                    buf
                } else {
                    std::fs::read(&path).map_err(|e| {
                        anyhow::anyhow!("--raw-request: {path} could not be read: {e}")
                    })?
                };
                request["raw_request"] = json!(b64(&bytes));
            }
            // Mutating, and delivered to `--as` when there is one.
            let target = as_session.as_deref().or(session.as_deref());
            // With one send there is nothing to summarise and the reply is the
            // answer. With several, the medians go in beside the samples.
            verb_then(&root, target, request, true, json, |answer, _refused| {
                // On a refusal too: that is when the count matters most.
                if let Some(samples) = answer.get("samples").and_then(Value::as_array)
                    && let Some(summary) = super::websec::timing_summary(samples)
                {
                    answer["timing"] = summary;
                }
                if !dropped.is_empty() {
                    answer["credentials_dropped"] = serde_json::json!(dropped);
                }
                Ok(())
            })
        }
        BrowserCommands::Audit { session, json } => audit(&root, session.as_deref(), json),
        BrowserCommands::MovedMessage { .. } => moved("message", "show"),
        BrowserCommands::MovedDiff { .. } => moved("diff", "diff"),
        BrowserCommands::MovedMatch { .. } => moved("match", "match"),
        BrowserCommands::MovedSitemap { .. } => moved("sitemap", "sitemap"),
        BrowserCommands::Rpc { stdio, session } => rpc(&root, session.as_deref(), stdio),
        BrowserCommands::Env { session, json } => {
            verb(&root, session.as_deref(), json!({"verb": "env"}), false, json)
        }
        BrowserCommands::Take { session } => take(&root, session.as_deref()),
        BrowserCommands::Release { session } => release(&root, session.as_deref()),
        BrowserCommands::Url { name, port } => viewer_url(&name, port),
    }
}

/// End every session whose time has passed: the processes, then the record.
fn expire_and_stop(root: &Path) {
    for session in bs::expire_due(root).unwrap_or_default() {
        let _ = stop_engine(&session);
    }
}

/// Most a `--set-file` payload may be. Generous, because a large upload is a
/// real test; bounded, because the file is read whole and base64'd through a
/// control message.
const MAX_PAYLOAD_BYTES: u64 = 64 * 1024 * 1024;

/// The bytes of a `--set-file`, refused when they are not a regular file's.
fn read_a_payload(target: &str, path: &str) -> anyhow::Result<Vec<u8>> {
    let meta = std::fs::metadata(path)
        .map_err(|e| anyhow::anyhow!("`--set-file {target}`: {path} could not be read: {e}"))?;
    if !meta.is_file() {
        anyhow::bail!(
            "`--set-file {target}`: {path} is not a regular file, so it has no bytes to send — a \
             device or a pipe is read until it stops, and these do not stop"
        );
    }
    if meta.len() > MAX_PAYLOAD_BYTES {
        anyhow::bail!(
            "`--set-file {target}`: {path} is {} bytes, past the {MAX_PAYLOAD_BYTES} byte limit \
             on one payload. It is read whole, encoded and carried through a control message, so \
             this would cost several times that in memory",
            meta.len()
        );
    }
    std::fs::read(path)
        .map_err(|e| anyhow::anyhow!("`--set-file {target}`: {path} could not be read: {e}"))
}

/// Open a URL: navigate the session that is already there, or make one.
fn open(
    root: &Path,
    url: String,
    name: Option<String>,
    new: bool,
    grants: ProxyGrants,
    json_out: bool,
) -> anyhow::Result<()> {
    if !new {
        // A name that is live, or the default when none was given.
        let existing = match &name {
            Some(name) => bs::find_by_name(root, name),
            None => bs::resolve(root, None).ok(),
        };
        if let Some(existing) = existing.filter(|s| s.state.is_live() && s.proxy.is_some()) {
            // A session's policy is fixed when its proxy starts, so accepting a
            // grant here and doing nothing with it would be a grant the caller
            // believes it made.
            let given = grants.given();
            if !given.is_empty() {
                anyhow::bail!(
                    "{} only take effect when a session is made, and session {} is already \
                     open. Pass `--new` (with a new `--session` name) to make another, or close \
                     this one first.",
                    given.join(", "),
                    label(&existing)
                );
            }
            return proxy_open(root, &url, Some(&existing.id), json_out);
        }
    }
    let previous_default = bs::read_default(root);
    let session = start_proxy(root, &url, name, &grants, true)?;
    if let Err(error) = drive_to(root, &session, &url) {
        // Nothing half-made is left running. A session that sent nothing has
        // no evidence to keep, so its record goes too; one that did is kept.
        let mut session = session;
        let _ = stop_engine(&session);
        let sent = session
            .logs
            .requests
            .as_ref()
            .and_then(|path| std::fs::metadata(path).ok())
            .is_some_and(|meta| meta.len() > 0);
        if sent {
            bs::end(root, &mut session, bs::State::Died, "the browser could not open the page");
        } else {
            let _ = bs::remove(root, &session.id);
        }
        // The session that was the default before this one still is.
        if let Some(previous) = previous_default.filter(|id| bs::read(root, id).is_ok()) {
            let _ = bs::set_default(root, &previous);
        }
        return Err(error);
    }
    if json_out {
        println!("{}", serde_json::to_string_pretty(&session)?);
    } else {
        println!("{} browser session {} on {url}", SUCCESS, label(&session));
        print_summary(&session);
        println!("  next     : h5i browser snapshot   ·   h5i browser requests");
    }
    Ok(())
}

/// Navigate first, for a read verb that was given `--url`.
fn go_to(root: &Path, selector: Option<&str>, url: &str) -> anyhow::Result<()> {
    let session = live_session(root, selector);
    drive_to(root, &session, url)
}

/// The live session a verb acts on, or the exit that says it is gone.
fn live_session(root: &Path, selector: Option<&str>) -> bs::Session {
    match bs::resolve(root, selector) {
        Ok(session) => session,
        Err(gone) => {
            eprintln!("{gone}");
            std::process::exit(bs::EXIT_SESSION_GONE);
        }
    }
}

/// `$H5I_SECRET_<NAME>` placeholders, resolved from this environment. An unset
/// one is refused rather than typed as written.
fn substitute_secrets(text: &str) -> anyhow::Result<String> {
    let resolved = h5i_browser::secrets::Secrets::from_env().substitute(text);
    if let Some(name) = resolved.missing.first() {
        anyhow::bail!(
            "`${name}` names a credential that is not set in this environment. Set `{name}` \
             where h5i runs, or type the text without it."
        );
    }
    Ok(resolved.text)
}

/// A PNG of the page, written where h5i chooses unless `--out` says otherwise.
fn screenshot(
    root: &Path,
    selector: Option<&str>,
    out: Option<PathBuf>,
    full: bool,
    url: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    if let Some(url) = url {
        go_to(root, selector, &url)?;
    }
    let session = live_session(root, selector);
    let path = match out {
        Some(path) => path,
        None => {
            let dir = bs::dir(root, &session.id).join("artifacts");
            std::fs::create_dir_all(&dir)?;
            dir.join(format!("screenshot-{}.png", stamp_millis()))
        }
    };
    let mut argv = vec!["screenshot".to_string(), path.display().to_string()];
    if full {
        argv.push("--full".into());
    }
    let target = path.display().to_string();
    begin_page_action(&session, "screenshot", Some(&target))?;
    let outcome = run_agent_browser(&session, &argv);
    record_page_action(&session, "screenshot", Some(&target), outcome.as_ref().err());
    let output = outcome?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "ok": true,
                "session": session.id,
                "path": path.display().to_string(),
                "output": output,
            }))?
        );
    } else {
        println!("{} {}", SUCCESS, path.display());
    }
    Ok(())
}

fn stamp_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn label(session: &bs::Session) -> String {
    match &session.name {
        Some(name) => format!("{name} ({})", session.id),
        None => session.id.clone(),
    }
}

fn executable_on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

/// The agent-browser this machine has, checked by running it.
/// Where agent-browser is: on `PATH`, or where its installers put it.
fn find_agent_browser() -> Option<PathBuf> {
    executable_on_path("agent-browser")
        .or_else(|| h5i_core::sandbox::agent_browser_binary().map(PathBuf::from))
}

fn proxy_agent_browser() -> anyhow::Result<PathBuf> {
    let path = find_agent_browser()
        .ok_or_else(|| anyhow::anyhow!(
            "`h5i browser` drives Chromium through agent-browser, which is not installed.\n\n  \
             Install it (it needs Node.js), then retry:\n    \
             npm install -g agent-browser\n    \
             agent-browser install            # downloads Chromium\n\n  \
             On Linux, `agent-browser install --with-deps` also installs the system libraries \
             Chromium needs.\n\n  \
             Nothing was started. `h5i browser proxy <url> --no-browser` starts a session \
             without a browser, for `resend` and the plugins."
        ))?;
    let version = Command::new(&path).arg("--version").output().map_err(|e| {
        anyhow::anyhow!(
            "found agent-browser at {}, but it could not be executed ({e}).\n\n  \
             Repair or reinstall it, then retry. Nothing was started.",
            path.display()
        )
    })?;
    if !version.status.success() {
        let detail = String::from_utf8_lossy(&version.stderr);
        // agent-browser is a Node script; a missing `node` is the usual reason.
        if detail.contains("node") && detail.contains("No such file") {
            anyhow::bail!(
                "agent-browser is installed at {}, but Node.js is not on PATH, so it cannot \
                 run.\n\n  Install Node.js (or add it to PATH), then retry. Nothing was started.",
                path.display()
            );
        }
        anyhow::bail!(
            "found agent-browser at {}, but `agent-browser --version` exited with {}: {}\n\n  \
             Repair or reinstall it, then retry. Nothing was started.",
            path.display(),
            version.status,
            bs::scrub_text(detail.trim())
        );
    }
    let reported = String::from_utf8_lossy(&version.stdout);
    if let Some(found) = agent_browser_version(&reported)
        && found < MIN_AGENT_BROWSER
    {
        let (major, minor, patch) = found;
        anyhow::bail!(
            "agent-browser {major}.{minor}.{patch} is installed at {}, and h5i needs {}.{}.0 or \
             newer (it hands every command over as a stdin batch).\n\n  \
             Upgrade it, then retry. Nothing was started:\n    \
             npm install -g agent-browser@latest",
            path.display(),
            MIN_AGENT_BROWSER.0,
            MIN_AGENT_BROWSER.1,
        );
    }
    Ok(path)
}

/// The oldest agent-browser whose `batch` reads a JSON array of commands from
/// stdin, which is how every command is handed over.
const MIN_AGENT_BROWSER: (u32, u32, u32) = (0, 25, 0);

/// `agent-browser 0.39.1` as numbers. `None` for a shape this does not know,
/// which is not refused: a version it cannot read is not one it knows is old.
fn agent_browser_version(reported: &str) -> Option<(u32, u32, u32)> {
    let word = reported.split_whitespace().find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()))?;
    let mut parts = word.split(['.', '-']).map(|p| p.parse::<u32>());
    Some((parts.next()?.ok()?, parts.next()?.ok()?, parts.next().and_then(Result::ok).unwrap_or(0)))
}

fn shell_word(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Quote a word for a copy-paste shell line only when it needs it, so plain
/// flags like `--proxy` stay readable and only args with shell metacharacters
/// (spaces, `<`, `>`, …) are quoted.
fn shell_word_if_needed(value: &str) -> String {
    let safe = !value.is_empty()
        && value.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'/' | b':' | b'=' | b'-' | b'+' | b'@' | b'%' | b',')
        });
    if safe {
        value.to_string()
    } else {
        shell_word(value)
    }
}

/// Whether this machine can drive a proxied Chromium: Linux and macOS, with
/// Chromium told to accept the proxy's certificates.
fn host_proxy_chromium_tls() -> anyhow::Result<bs::ChromiumTls> {
    if cfg!(target_os = "linux") || cfg!(target_os = "macos") {
        Ok(bs::ChromiumTls::IgnoreHttpsErrors)
    } else {
        anyhow::bail!(
            "an h5i browser session drives a proxied Chromium on Linux and macOS. This platform \
             is neither, so nothing was started."
        )
    }
}

/// The agent-browser arguments that point it at this proxy. The loopback
/// bypass rule keeps Chromium from skipping the proxy for localhost.
fn agent_browser_proxy_args(proxy_url: &str) -> Vec<String> {
    agent_browser_proxy_args_with(proxy_url, std::env::var("AGENT_BROWSER_ARGS").ok().as_deref())
}

/// The same, merging `extra` (AGENT_BROWSER_ARGS) into the one `--args`
/// string, which would otherwise shadow it.
fn agent_browser_proxy_args_with(proxy_url: &str, extra: Option<&str>) -> Vec<String> {
    let mut launch = String::from("--proxy-bypass-list=<-loopback>");
    if let Some(extra) = extra {
        let extra = extra.trim();
        if !extra.is_empty() {
            launch.push(',');
            launch.push_str(extra);
        }
    }
    vec![
        "--proxy".into(),
        proxy_url.into(),
        "--ignore-https-errors".into(),
        "--args".into(),
        launch,
    ]
}

/// The environment that keeps every agent-browser launch from a shell on this
/// proxy, including a relaunch after its daemon idles out.
fn agent_browser_proxy_env(proxy_url: &str) -> String {
    format!(
        "export AGENT_BROWSER_PROXY={} AGENT_BROWSER_IGNORE_HTTPS_ERRORS=1 AGENT_BROWSER_ARGS={}",
        shell_word_if_needed(proxy_url),
        shell_word_if_needed("--proxy-bypass-list=<-loopback>")
    )
}

/// The shell line a person copies to point agent-browser at this proxy.
fn agent_browser_open_line(agent_browser: &str, proxy_url: &str, url: &str) -> String {
    let flags = agent_browser_proxy_args(proxy_url)
        .iter()
        .map(|a| shell_word_if_needed(a))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "{} {flags} open {}",
        shell_word_if_needed(agent_browser),
        shell_word_if_needed(url)
    )
}

/// Start a session: its capture proxy, control channel and record. With
/// `with_browser`, a missing agent-browser is refused before anything starts.
fn start_proxy(
    root: &Path,
    url: &str,
    name: Option<String>,
    grants: &ProxyGrants,
    with_browser: bool,
) -> anyhow::Result<bs::Session> {
    if !is_web_url(url) {
        anyhow::bail!("a browser session needs an http:// or https:// target, not {url:?}");
    }
    // Every external-tool check before allocating a session id, creating a
    // CA, or spawning the proxy. A missing browser must leave no half-session.
    let trust = if with_browser {
        let trust = host_proxy_chromium_tls()?;
        proxy_agent_browser()?;
        trust
    } else {
        bs::ChromiumTls::IgnoreHttpsErrors
    };
    if let Some(name) = &name
        && let Some(existing) = bs::find_by_name(root, name)
    {
        anyhow::bail!(
            "browser session `{name}` is already open as {}. Close it first or choose another name.",
            existing.id
        );
    }
    let scope = match &grants.project {
        Some(name) => crate::cli::scope::resolve(name)?,
        None => None,
    };
    let id = bs::new_id(root)?;
    let dir = bs::dir(root, &id);
    let control = dir.join(bs::CONTROL_FILE);
    let proxy_file = dir.join("proxy");
    let ca_cert = dir.join("proxy-ca.pem");
    let ca_key = dir.join("proxy-ca-key.pem");
    let log_path = dir.join("proxy.log");

    let mut allow = grants.allow.clone();
    if !allow.iter().any(|origin| origin == url) {
        allow.insert(0, url.to_string());
    }
    let now = chrono::Utc::now();
    let expires_at = grants.expires_in.map(|secs| {
        (now + chrono::Duration::seconds(secs as i64))
            .to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
    });

    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("__capture-proxy")
        .arg("--target")
        .arg(url)
        .arg("--control-file")
        .arg(&control)
        .arg("--proxy-file")
        .arg(&proxy_file)
        .arg("--ca-cert")
        .arg(&ca_cert)
        .arg("--ca-key")
        .arg(&ca_key)
        .arg("--receipts")
        .arg(dir.join(bs::RECEIPTS_FILE))
        .arg("--actions")
        .arg(dir.join(bs::ACTIONS_FILE))
        .arg("--messages")
        .arg(dir.join(bs::MESSAGES_DIR));
    for origin in &allow {
        command.arg("--allow").arg(origin);
    }
    if let Some(scope) = &scope {
        command.args(scope.engine_args());
    }
    if grants.no_loopback {
        command.arg("--no-loopback");
    }
    if grants.dom_instrument {
        command.arg("--dom-report").arg(dir.join("dom-report.jsonl"));
    }
    if let Some(at) = &expires_at {
        command.arg("--expires-at").arg(at);
    }
    let log = std::fs::File::create(&log_path)?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    detach(&mut command);
    let mut child = command
        .spawn()
        .map_err(|e| anyhow::anyhow!("could not start the capture proxy ({e})"))?;
    let pid = child.id();

    let deadline = Instant::now() + PROXY_READY_TIMEOUT;
    while !(control.is_file() && proxy_file.is_file() && ca_cert.is_file()) {
        if let Some(status) = child.try_wait()? {
            let why = std::fs::read_to_string(&log_path).unwrap_or_default();
            anyhow::bail!(
                "the capture proxy exited with {status}: {}",
                bs::scrub_text(why.trim())
            );
        }
        if Instant::now() >= deadline {
            kill(pid);
            anyhow::bail!(
                "the capture proxy did not become ready within {} seconds",
                PROXY_READY_TIMEOUT.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let proxy_url = std::fs::read_to_string(&proxy_file)?.trim().to_string();
    // Fixed now, for every later call: see `session_browser_args`.
    std::fs::write(
        dir.join(BROWSER_ARGS_FILE),
        serde_json::to_vec(&agent_browser_proxy_args(&proxy_url))?,
    )?;
    let session = bs::Session {
        id: id.clone(),
        name,
        project: grants.project.clone(),
        engine: bs::Engine::Chromium,
        placement: bs::Placement::Host,
        lane: bs::Lane::ProxyObserved,
        url: url.to_string(),
        started_at: now.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        expires_at,
        storage: bs::Storage::Ephemeral,
        policy_digest: host_policy_digest(&allow, grants.no_loopback, scope.as_ref()),
        scope_digest: scope.as_ref().map(|s| s.digest.clone()).unwrap_or_default(),
        identity: if with_browser {
            "agent-browser/chromium".into()
        } else {
            "none".into()
        },
        identity_digest: String::new(),
        restored_from: None,
        state: bs::State::Live,
        ended_at: None,
        end_reason: None,
        confinement: h5i_core::browser_sandbox::Confinement::None {
            why: "agent-browser and Chromium run as ordinary host processes".into(),
        },
        enclosing_box: None,
        control: bs::Control {
            channel: bs::Channel::Port,
            file: Some(control),
            witness: Some(proxy_file),
            pid: Some(pid),
        },
        logs: bs::Logs {
            actions: Some(dir.join(bs::ACTIONS_FILE)),
            requests: Some(dir.join(bs::RECEIPTS_FILE)),
        },
        permissive_cors: false,
        proxy: Some(bs::Proxy {
            url: proxy_url,
            ca_cert,
            chromium_tls: trust,
        }),
    };
    bs::write(root, &session)?;
    bs::set_default(root, &id)?;
    Ok(session)
}

/// What `proxy` prints about the session it made.
fn print_started(session: &bs::Session, url: &str, with_browser: bool) {
    let Some(proxy) = &session.proxy else { return };
    println!("{} capture session {}", SUCCESS, label(session));
    print_summary(session);
    println!("  proxy    : {}", proxy.url);
    println!(
        "  CA       : {} (install it yourself to avoid --ignore-https-errors)",
        proxy.ca_cert.display()
    );
    println!();
    if with_browser {
        let agent_browser = find_agent_browser()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "agent-browser".into());
        println!("  drive    : h5i browser navigate <url> (passes the proxy on every launch)");
        println!(
            "  or       : {}",
            agent_browser_open_line(&agent_browser, &proxy.url, url)
        );
        println!("  env      : {}", agent_browser_proxy_env(&proxy.url));
        println!(
            "             export these first, so a relaunched agent-browser (its daemon \
             restarts when idle) is proxied too; otherwise capture stops silently"
        );
        println!("  login    : agent-browser dashboard start");
    } else {
        println!("  browser  : none; point one at the proxy above, or drive it with `resend`");
    }
    println!("  inspect  : h5i browser requests   ·   h5i websec requests");
}

/// The agent-browser session name a proxy session's browser runs under.
fn proxy_browser_session(id: &str) -> String {
    format!("h5i-{id}")
}

/// Run one agent-browser command against a session's Chromium and hand back its
/// stdout. The proxy flags go on every call, so a relaunch is proxied too.
///
/// The command goes as a `batch` on stdin, never in argv: agent-browser reads
/// flags anywhere in its argv, so a selector or a script that looks like
/// `--proxy` would be taken as one, and a credential there is readable by any
/// local process. In a batch every element is an argument.
fn run_agent_browser(session: &bs::Session, command_argv: &[String]) -> anyhow::Result<String> {
    if session.proxy.is_none() {
        anyhow::bail!(
            "session {} was opened by an h5i that had its own browser engine, and that engine \
             is gone. Its record, requests and messages are still readable; open a new session \
             with `h5i browser open <url>` to drive a page.",
            session.id
        );
    }
    let agent_browser = proxy_agent_browser()?;
    let mut command = Command::new(&agent_browser);
    command
        .arg("--session")
        .arg(proxy_browser_session(&session.id))
        .args(session_browser_args(session))
        .args(["batch", "--bail"])
        // Its own colours would reach the scrub as control characters.
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| anyhow::anyhow!("could not run agent-browser: {e}"))?;
    if let Some(mut pipe) = child.stdin.take() {
        use std::io::Write as _;
        // Dropped at the end of this block, which is the end of the batch.
        if let Err(e) = pipe.write_all(json!([command_argv]).to_string().as_bytes()) {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("could not hand agent-browser its command: {e}");
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|e| anyhow::anyhow!("could not run agent-browser: {e}"))?;
    // What a page composed, on its way to a terminal and a model: escapes and
    // control characters go, the same as from every other reply.
    let stdout = bs::scrub_text(&String::from_utf8_lossy(&output.stdout));
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "agent-browser {} failed for session {}: {}{}{}",
            command_argv.first().map(String::as_str).unwrap_or(""),
            session.id,
            bs::scrub_text(stderr.trim()),
            if stdout.trim().is_empty() {
                String::new()
            } else {
                format!("\n{}", stdout.trim())
            },
            sandbox_hint(&stderr),
        );
    }
    Ok(stdout)
}

/// A page verb: one agent-browser call on the session's Chromium, recorded.
fn page(
    root: &Path,
    selector: Option<&str>,
    ab_argv: Vec<String>,
    label: &str,
    mutating: bool,
    json: bool,
) -> anyhow::Result<()> {
    let shown = ab_argv.clone();
    page_with(root, selector, ab_argv, shown, label, mutating, json)
}

/// The same, given how to run it and the argv to record (`shown`, which has
/// placeholders where the run has credentials). Checks the control lock,
/// records the verb before it acts, masks every session credential out of
/// what comes back, and keeps the output in the session's artifacts.
fn page_with(
    root: &Path,
    selector: Option<&str>,
    run: Vec<String>,
    shown: Vec<String>,
    label: &str,
    mutating: bool,
    json: bool,
) -> anyhow::Result<()> {
    let session = live_session(root, selector);
    let dir = bs::dir(root, &session.id);
    if let Some(explanation) = h5i_core::control::check(&dir, mutating).explain() {
        anyhow::bail!("{explanation}");
    }
    // Everything after the verb, so the audit reads `scroll down 200` rather
    // than `scroll down`.
    let target = (shown.len() > 1).then(|| shown[1..].join(" "));
    begin_page_action(&session, label, target.as_deref())?;
    let secrets = h5i_browser::secrets::Secrets::from_env();
    let outcome = run_agent_browser(&session, &run)
        .map(|stdout| secrets.redact(&stdout))
    .map_err(|e| anyhow::anyhow!("{}", secrets.redact(&e.to_string())));
    record_page_action(&session, label, target.as_deref(), outcome.as_ref().err());
    let stdout = outcome?;
    let recorded = record_browser_observation(root, &session, label, &shown, &stdout);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "ok": true,
                "session": session.id,
                "verb": label,
                "output": stdout,
                "artifact": recorded.as_ref().map(|p| p.display().to_string()),
            }))?
        );
    } else {
        print!("{stdout}");
        if !stdout.ends_with('\n') {
            println!();
        }
        if let Some(p) = &recorded {
            eprintln!("  (recorded to {})", p.display());
        }
    }
    Ok(())
}

/// The action-log row a page verb writes before it acts. No record, no action,
/// as on the control channel.
fn begin_page_action(session: &bs::Session, verb: &str, target: Option<&str>) -> anyhow::Result<()> {
    let row = json!({"phase": "request", "verb": verb, "target": target, "via": "agent-browser"});
    append_action_row(session, row).map_err(|e| {
        anyhow::anyhow!("refusing to act: the action could not be recorded: {e}")
    })
}

/// The row after it acted. Best effort: the browser has already acted.
fn record_page_action(
    session: &bs::Session,
    verb: &str,
    target: Option<&str>,
    error: Option<&anyhow::Error>,
) {
    let row = json!({
        "phase": "result",
        "verb": verb,
        "target": target,
        "ok": error.is_none(),
        "error": error.map(|e| bs::scrub_text(&e.to_string())),
        "via": "agent-browser",
    });
    let _ = append_action_row(session, row);
}

/// One row in the session's action log, in the proxy's own shape. No `seq`:
/// those are the proxy's. One write, because the proxy appends to the same
/// file.
fn append_action_row(session: &bs::Session, mut row: Value) -> std::io::Result<()> {
    use std::io::Write as _;
    let Some(path) = &session.logs.actions else {
        return Ok(());
    };
    row["at"] = json!(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true));
    let mut file = std::fs::OpenOptions::new().append(true).open(path)?;
    file.write_all(format!("{row}\n").as_bytes())
}

/// Keep a DOM/console observation beside the session's HTTP capture, so it is
/// evidence the session holds rather than output the agent saw once. Best
/// effort: a failure to write is not a failure to observe.
fn record_browser_observation(
    root: &Path,
    session: &bs::Session,
    label: &str,
    ab_argv: &[String],
    output: &str,
) -> Option<PathBuf> {
    let dir = bs::dir(root, &session.id).join("artifacts");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{label}-{}.txt", stamp_millis()));
    let header = format!("# h5i browser {} ({})\n\n", label, ab_argv.join(" "));
    std::fs::write(&path, format!("{header}{output}")).ok()?;
    Some(path)
}

/// Best-effort shutdown of the agent-browser daemon a session started. Nothing
/// else reaps it, so closing the h5i session must, or a scan leaves a headless
/// Chromium resident.
fn close_proxy_browser(session: &bs::Session) {
    if session.proxy.is_none() {
        return;
    }
    if let Some(agent_browser) = find_agent_browser() {
        let _ = Command::new(agent_browser)
            .arg("--session")
            .arg(proxy_browser_session(&session.id))
            .args(session_browser_args(session))
            .arg("close")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Where a session keeps the agent-browser launch flags it started with.
const BROWSER_ARGS_FILE: &str = "agent-browser.args";

/// The agent-browser flags this session's browser was launched with, the same
/// on every later call. agent-browser treats a call whose launch options
/// differ from its daemon's as a different browser and starts a fresh one,
/// so recomputing them from a changed `AGENT_BROWSER_ARGS` would drop the
/// page mid-session, and a `close` would start a browser to close.
fn session_browser_args(session: &bs::Session) -> Vec<String> {
    let Some(proxy) = &session.proxy else {
        return Vec::new();
    };
    session
        .control
        .file
        .as_ref()
        .and_then(|control| control.parent())
        .and_then(|dir| std::fs::read(dir.join(BROWSER_ARGS_FILE)).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(|| agent_browser_proxy_args(&proxy.url))
}

/// Close every h5i Chromium whose session in this registry has ended: one left
/// by a proxy that died, or a session another tool expired. A session this
/// registry has no record of may be another registry's, and is left alone.
fn close_orphaned_browsers(root: &Path) {
    let Some(agent_browser) = find_agent_browser() else {
        return;
    };
    let Ok(output) = Command::new(agent_browser)
        .args(["session", "list", "--json"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return;
    };
    let listed: Value = serde_json::from_slice(&output.stdout).unwrap_or_default();
    let names = listed["data"]["sessions"].as_array().cloned().unwrap_or_default();
    for name in names.iter().filter_map(Value::as_str) {
        let Some(id) = name.strip_prefix("h5i-") else {
            continue;
        };
        if let Ok(session) = bs::read(root, id)
            && !session.state.is_live()
        {
            close_proxy_browser(&session);
        }
    }
}

/// The way out when Chromium cannot start its sandbox. h5i never adds
/// `--no-sandbox` itself.
fn sandbox_hint(stderr: &str) -> String {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("no usable sandbox") || lower.contains("sandboxhelper") || lower.contains("setuid sandbox") {
        "\n\nChromium could not start its sandbox (usual without root). Retry with the sandbox \
         off: `AGENT_BROWSER_ARGS=--no-sandbox h5i browser navigate <url>`, or run agent-browser \
         yourself with `--args --no-sandbox`. This disables Chromium's sandbox, so use it only for \
         a target you trust."
            .to_string()
    } else {
        String::new()
    }
}

/// Drive a session's Chromium to a URL.
fn drive_to(root: &Path, session: &bs::Session, url: &str) -> anyhow::Result<()> {
    let dir = bs::dir(root, &session.id);
    if let Some(explanation) = h5i_core::control::check(&dir, true).explain() {
        anyhow::bail!("{explanation}");
    }
    begin_page_action(session, "navigate", Some(url))?;
    let outcome = run_agent_browser(session, &["open".to_string(), url.to_string()]);
    record_page_action(session, "navigate", Some(url), outcome.as_ref().err());
    outcome.map(|_| ())
}

/// `navigate` and `proxy-open`: drive the session's Chromium to a URL.
fn proxy_open(
    root: &Path,
    url: &str,
    selector: Option<&str>,
    json_out: bool,
) -> anyhow::Result<()> {
    let session = live_session(root, selector);
    drive_to(root, &session, url)?;
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": true, "session": session.id, "url": url,
            }))?
        );
    } else {
        println!("{} opened {url} through session {}", SUCCESS, label(&session));
    }
    Ok(())
}

/// Whether a target is fetched over the network, judged by its scheme alone.
fn is_web_url(target: &str) -> bool {
    let target = target.trim_start();
    ["http://", "https://"]
        .iter()
        .any(|scheme| target.len() > scheme.len() && target[..scheme.len()].eq_ignore_ascii_case(scheme))
}

/// The digest of what a session was allowed to do.
fn host_policy_digest(
    allow: &[String],
    no_loopback: bool,
    scope: Option<&crate::cli::scope::Resolved>,
) -> String {
    use sha2::{Digest, Sha256};
    let mut allow = allow.to_vec();
    allow.sort();
    // In here as well as in its own digest: this one answers "allowed the same
    // things", which a deny rule changes. `scope_digest` answers "under which
    // document".
    let scope = scope.map(|s| s.digest.as_str()).unwrap_or("none");
    let material = format!(
        "proxy\nallow={}\nloopback={}\nscope={}\n",
        allow.join(","),
        !no_loopback,
        scope
    );
    format!("sha256:{:x}", Sha256::digest(material.as_bytes()))
}

/// Send a control request and print the answer.
fn verb(
    root: &Path,
    selector: Option<&str>,
    request: Value,
    mutating: bool,
    json: bool,
) -> anyhow::Result<()> {
    verb_then(root, selector, request, mutating, json, |_, _| Ok(()))
}

/// What to say when a script asks for a verb that is now the plugin's.
fn moved(was: &str, now: &str) -> anyhow::Result<()> {
    anyhow::bail!(
        "`h5i browser {was}` is now `h5i websec {now}`, and it arrives with the workbench \
         plugin:\n    h5i plugin install websec --from ./h5i-websec\n\n  \
         Reading a captured store is what installing the plugin adds. Sending is still \
         here: `h5i browser requests`, `resend`, `sequence` and `rpc`."
    )
}

/// `h5i browser rpc --stdio`: one request per line, one answer per line, each
/// through `ask_session` like a typed verb.
fn rpc(root: &Path, selector: Option<&str>, stdio: bool) -> anyhow::Result<()> {
    if !stdio {
        anyhow::bail!("`rpc` speaks over stdin and stdout: pass `--stdio`");
    }
    use std::io::{BufRead, Read};

    let stdin = std::io::stdin();
    let mut reader = std::io::BufReader::new(stdin.lock());
    let mut line = String::new();
    loop {
        line.clear();
        // Read the cap, not past it: checking the length afterwards would mean
        // holding whatever was sent before deciding it was too much.
        let read = (&mut reader)
            .take(RPC_MAX_LINE as u64 + 1)
            .read_line(&mut line)?;
        if read == 0 {
            return Ok(());
        }
        if !line.ends_with('\n') && read > RPC_MAX_LINE {
            drain_line(&mut reader)?;
            println!(
                "{}",
                json!({"error": {"code": "too-long", "message": format!(
                    "a request line may be at most {RPC_MAX_LINE} bytes"
                )}})
            );
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(request) => rpc_one(root, selector, &request),
            Err(why) => json!({"error": {"code": "unreadable", "message": why.to_string()}}),
        };
        println!("{reply}");
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
    }
}

/// The longest request line the loop will read.
const RPC_MAX_LINE: usize = 1024 * 1024;

/// Throw away the rest of a line that was too long, in bounded pieces, so the
/// next read starts on a request rather than in the middle of one.
fn drain_line(reader: &mut impl std::io::BufRead) -> std::io::Result<()> {
    use std::io::Read;
    loop {
        let mut chunk = Vec::new();
        let read = std::io::BufRead::read_until(
            &mut reader.by_ref().take(64 * 1024),
            b'\n',
            &mut chunk,
        )?;
        if read == 0 || chunk.ends_with(b"\n") {
            return Ok(());
        }
    }
}

/// One RPC request, answered.
fn rpc_one(root: &Path, selector: Option<&str>, request: &Value) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let verb = request.get("verb").and_then(Value::as_str).unwrap_or("");
    let answer = match verb {
        // Cheap, and the way a caller finds out whether this build speaks the
        // protocol at all before it commits a run to it.
        "ping" => Ok(json!({"ok": true, "verb": "ping"})),
        "resend" => rpc_resend(root, selector, request),
        other => Err(anyhow::anyhow!(
            "`{other}` is not a verb this speaks. It speaks: ping, resend"
        )),
    };
    match answer {
        Ok(mut value) => {
            if let Some(map) = value.as_object_mut() {
                map.insert("id".to_string(), id);
            }
            value
        }
        Err(why) => json!({"id": id, "error": {"code": "verb", "message": why.to_string()}}),
    }
}

fn rpc_resend(root: &Path, selector: Option<&str>, request: &Value) -> anyhow::Result<Value> {
    let from = request.get("from").and_then(Value::as_u64);
    let given = request.get("request").filter(|value| !value.is_null());
    if from.is_some() == given.is_some() {
        anyhow::bail!("resend needs exactly one of `from` or `request`");
    }
    // A whole request is the same shape cross-session replay uses: it is
    // still sent by this session's broker, under its jar, policy, budget and
    // receipts. It is not a second network lane.
    let strings = |field: &str| -> Vec<String> {
        request
            .get(field)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    let flag = |field: &str| request.get(field).and_then(Value::as_bool) == Some(true);
    let outgoing = json!({
        "verb": "resend",
        "from": from,
        "request": given,
        "set": strings("set"),
        "unset": strings("unset"),
        "raw_target": request.get("raw_target").and_then(Value::as_str),
        "create": flag("create"),
        "no_follow": flag("no_follow"),
        "reset_budget": flag("reset_budget"),
    });
    ask_session(root, selector, outgoing, true)
}

pub(crate) fn resend_step(
    root: &Path,
    selector: Option<&str>,
    step: &super::websec::Sending<'_>,
) -> anyhow::Result<Value> {
    let mut request = json!({
        "verb": "resend",
        "set": step.set,
        "unset": step.unset,
        "create": step.create,
    });
    match step.as_session {
        None => request["from"] = json!(step.from),
        Some(_) => {
            let (carried, _) =
                super::websec::carry(root, selector, step.from, step.keep_credentials)?;
            request["request"] = carried;
        }
    }
    // The session the request becomes part of, which is the other one when
    // there is one.
    let target = step.as_session.or(selector);
    ask_session(root, target, request, true)
}

/// Send a control request and hand back the answer, without printing it: same
/// resolution, control-lock check and scrub as a typed verb.
pub(crate) fn ask_session(
    root: &Path,
    selector: Option<&str>,
    request: Value,
    mutating: bool,
) -> anyhow::Result<Value> {
    let session = live_session(root, selector);
    let dir = bs::dir(root, &session.id);
    // Only a human at the controls pauses these. The re-snapshot rule after a
    // handover is about `@refs` on a page that moved; a stored request has
    // none, and a session with no browser could never snapshot to clear it.
    let verdict = h5i_core::control::check(&dir, mutating);
    if verdict == h5i_core::control::Verdict::HeldByHuman
        && let Some(explanation) = verdict.explain()
    {
        anyhow::bail!("{explanation}");
    }
    let mut answer = deliver(&session, &dir, &request)?;
    bs::scrub(&mut answer);
    Ok(answer)
}

fn verb_then(
    root: &Path,
    selector: Option<&str>,
    request: Value,
    mutating: bool,
    json: bool,
    after: impl FnOnce(&mut Value, bool) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let clock = Instant::now();
    let mut answer = ask_session(root, selector, request, mutating)?;
    // The client half of `H5I_BROWSER_TIMING`, whose server half puts a
    // `timing_ms` object in the reply. Together they say which side of the
    // socket a verb spent its time on.
    if std::env::var_os("H5I_BROWSER_TIMING").is_some() {
        eprintln!(
            "client timing: {:.2} ms (includes the session's own work)",
            clock.elapsed().as_secs_f64() * 1000.0,
        );
    }

    // A refusal is printed like any answer, and still exits non-zero.
    let refused = answer.get("ok").and_then(Value::as_bool) == Some(false);
    // The hook runs either way and is told which it is. Skipping it on a
    // refusal is wrong for one that only annotates: a burst stopped halfway
    // came back with samples and no summary.
    after(&mut answer, refused)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&answer)?);
    } else if refused {
        anyhow::bail!("{}", refusal(&answer));
    } else {
        print_answer(&answer);
    }
    if refused {
        std::process::exit(1);
    }
    Ok(())
}

fn refusal(answer: &Value) -> String {
    ["error", "message"]
        .iter()
        .find_map(|key| answer.get(key).and_then(Value::as_str))
        .or_else(|| {
            answer
                .get("response")
                .and_then(|response| response.get("error"))
                .and_then(Value::as_str)
        })
        .unwrap_or("the session refused, without saying why")
        .to_string()
}

/// Carry a control request to the session's proxy, and read its answer.
fn deliver(session: &bs::Session, dir: &Path, request: &Value) -> anyhow::Result<Value> {
    if session.proxy.is_none() {
        anyhow::bail!(
            "session {} was opened by an h5i that had its own browser engine, and that engine \
             is gone. Its record, requests and messages are still readable with `h5i websec`; \
             open a new session with `h5i browser open <url>` to send.",
            session.id
        );
    }
    // Whatever the start recorded, not a second derivation of it: two places
    // that have to agree about an address are two places that can stop
    // agreeing.
    let control = session
        .control
        .file
        .clone()
        .unwrap_or_else(|| dir.join(bs::CONTROL_FILE));
    let port = h5i_browser::control::read_port_file(&control)
        .map_err(|e| anyhow::anyhow!("this session's control channel is gone: {e}"))?;
    h5i_browser::control::ask(port, request).map_err(|e| anyhow::anyhow!("{e}"))
}

/// The session's findings that no project holds a copy of.
fn unpromoted_findings(root: &Path, session: &bs::Session) -> Vec<String> {
    use h5i_core::project;
    let sdir = bs::dir(root, &session.id);
    let findings = h5i_core::session_view::findings(&sdir);
    if findings.is_empty() {
        return Vec::new();
    }
    // With no project named, none of them can have been promoted.
    let Some(name) = &session.project else {
        return findings.into_iter().map(|f| f.id).collect();
    };
    let Ok(project_root) = project::root() else {
        return findings.into_iter().map(|f| f.id).collect();
    };
    let Ok(p) = project::Project::open(&project_root, name) else {
        return findings.into_iter().map(|f| f.id).collect();
    };
    let promoted = project::finding::read(&p).unwrap_or_default();
    findings
        .into_iter()
        .filter(|f| project::finding::promoted_from(&promoted, &session.id, &f.id).is_none())
        .map(|f| f.id)
        .collect()
}

/// The selector to hand back in a hint: the friendly name if it has one, else
/// the id.
fn label_selector(session: &bs::Session) -> String {
    session.name.clone().unwrap_or_else(|| session.id.clone())
}

/// `h5i browser rm`.
#[allow(clippy::too_many_arguments)]
fn rm(
    root: &Path,
    names: &[String],
    ended: bool,
    all: bool,
    older_than: Option<u64>,
    dry_run: bool,
    force: bool,
    json_out: bool,
) -> anyhow::Result<()> {
    let targets = rm_targets(root, names, ended, all, older_than)?;
    if targets.is_empty() {
        if json_out {
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &json!({"dry_run": dry_run, "removed": [], "refused": [], "freed": 0})
                )?
            );
        } else {
            println!("  no session matched.");
        }
        return Ok(());
    }

    let mut removed: Vec<Value> = Vec::new();
    let mut refused: Vec<Value> = Vec::new();
    let mut freed = 0u64;

    for (name, session) in targets {
        let Some(session) = session else {
            refused.push(json!({"name": name, "why": "no session by that name or id"}));
            continue;
        };
        if session.state.is_live() && !force {
            refused.push(json!({
                "name": name,
                "id": session.id,
                "why": "it is still live: close it first, or pass --force",
            }));
            continue;
        }
        // Findings die with the session unless they were promoted into a
        // project. Refuse rather than lose them, and name what would go.
        if !force {
            let unsaved = unpromoted_findings(root, &session);
            if !unsaved.is_empty() {
                let hint = match &session.project {
                    Some(p) => format!("h5i project finding promote --all -p {p} --session {}", label_selector(&session)),
                    None => "open the session under a project (--project) and promote them, or pass --force".to_string(),
                };
                refused.push(json!({
                    "name": name,
                    "id": session.id,
                    "why": format!(
                        "{} finding(s) here are not saved to a project and would be lost: {}. \
                         Save them first ({hint}), or pass --force",
                        unsaved.len(),
                        unsaved.join(", ")
                    ),
                }));
                continue;
            }
        }
        if dry_run {
            let held = bs::held(root, &session.id);
            freed += held.total();
            removed.push(json!({
                "name": session.name,
                "id": session.id,
                "freed": held.total(),
                "store": held.store,
            }));
            continue;
        }
        // Stop what is left before the directory goes, ended sessions too: a
        // dead proxy can leave its Chromium up, and once the record is gone
        // nothing would know to close it.
        if session.state.is_live() || session.proxy.is_some() {
            let _ = stop_engine(&session);
        }
        match bs::remove(root, &session.id) {
            Ok(held) => {
                freed += held.total();
                removed.push(json!({
                    "name": session.name,
                    "id": session.id,
                    "freed": held.total(),
                    "store": held.store,
                }));
            }
            Err(why) => refused.push(json!({
                "name": name,
                "id": session.id,
                "why": why.to_string(),
            })),
        }
    }

    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "dry_run": dry_run,
                "removed": removed,
                "refused": refused,
                "freed": freed,
            }))?
        );
    } else {
        let verb = if dry_run { "would rm" } else { "removed" };
        // A named removal answers for each name. A filtered one can be
        // hundreds of records nobody chose one by one, so it answers with the
        // tally rather than scrolling them past.
        if names.is_empty() {
            println!("  {verb:<9}: {} session(s)", removed.len());
        } else {
            for row in &removed {
                println!(
                    "  {verb:<9}: {} ({})",
                    row["name"].as_str().unwrap_or_else(|| row["id"].as_str().unwrap_or("?")),
                    human_bytes(row["freed"].as_u64().unwrap_or(0))
                );
            }
        }
        for row in &refused {
            println!(
                "  kept     : {} — {}",
                row["name"].as_str().unwrap_or("?"),
                row["why"].as_str().unwrap_or("")
            );
        }
        if !removed.is_empty() {
            println!(
                "  {:<9}: {}",
                if dry_run { "would free" } else { "freed" },
                human_bytes(freed)
            );
        }
    }
    // One failure among several is reported, not fatal; all of them is.
    if removed.is_empty() && !refused.is_empty() {
        std::process::exit(1);
    }
    Ok(())
}

/// What `rm` was asked to erase, with the name to report when it is not there.
fn rm_targets(
    root: &Path,
    names: &[String],
    ended: bool,
    all: bool,
    older_than: Option<u64>,
) -> anyhow::Result<Vec<(String, Option<bs::Session>)>> {
    if !names.is_empty() {
        return Ok(names
            .iter()
            .map(|name| (name.clone(), find_any(root, name)))
            .collect());
    }
    if !(ended || all || older_than.is_some()) {
        anyhow::bail!(
            "name a session, or say which ones: --ended, --all, or --older-than DAYS"
        );
    }
    let cutoff = older_than.map(|days| chrono::Utc::now() - chrono::Duration::days(days as i64));
    Ok(bs::list(root)?
        .into_iter()
        .filter(|s| all || !s.state.is_live())
        .filter(|s| match cutoff {
            None => true,
            // Live sessions are `--force`'s to decide; an ended one with no
            // recorded time is left alone.
            Some(_) if s.state.is_live() => true,
            Some(cutoff) => s
                .ended_at
                .as_deref()
                .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
                .is_some_and(|at| at.with_timezone(&chrono::Utc) <= cutoff),
        })
        .map(|s| {
            let label = s.name.clone().unwrap_or_else(|| s.id.clone());
            (label, Some(s))
        })
        .collect())
}

/// A session by name or id, live or ended.
fn find_any(root: &Path, name: &str) -> Option<bs::Session> {
    bs::find_by_name(root, name)
        .or_else(|| bs::find_ended_by_name(root, name))
        .or_else(|| bs::read(root, name).ok())
}

/// `h5i browser gc`.
fn gc(root: &Path, older_than_days: u64, dry_run: bool, json_out: bool) -> anyhow::Result<()> {
    let cutoff = chrono::Utc::now() - chrono::Duration::days(older_than_days as i64);
    let sessions = bs::list(root).unwrap_or_default();
    let live = sessions.iter().filter(|s| s.state.is_live()).count();

    let mut reclaimed: Vec<Value> = Vec::new();
    let mut freed = 0u64;
    let mut too_new = 0usize;

    for session in sessions.iter().filter(|s| !s.state.is_live()) {
        let held = bs::held(root, &session.id);
        if held.store == 0 {
            continue;
        }
        // The clock the record carries. A record with no ending, or one this
        // cannot read, is left alone rather than guessed about.
        let ended = session
            .ended_at
            .as_deref()
            .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok());
        match ended {
            Some(at) if at.with_timezone(&chrono::Utc) > cutoff => {
                too_new += 1;
                continue;
            }
            None => {
                too_new += 1;
                continue;
            }
            _ => {}
        }
        if dry_run {
            freed += held.store;
            reclaimed.push(json!({"id": session.id, "name": session.name, "store": held.store}));
            continue;
        }
        match bs::drop_store(root, &session.id) {
            Ok(bytes) => {
                freed += bytes;
                reclaimed.push(json!({"id": session.id, "name": session.name, "store": bytes}));
            }
            Err(why) => eprintln!("  note     : {} kept its store — {why}", session.id),
        }
    }

    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "dry_run": dry_run,
                "freed": freed,
                "sessions": reclaimed.len(),
                "reclaimed": reclaimed,
                "live_untouched": live,
                "too_new": too_new,
            }))?
        );
        return Ok(());
    }

    println!(
        "  {:<9}: {} from {} ended session(s)",
        if dry_run { "would free" } else { "freed" },
        human_bytes(freed),
        reclaimed.len()
    );
    println!("  kept     : their records, request logs and ledgers");
    if too_new > 0 {
        println!(
            "  too new  : {too_new} ended session(s) within {older_than_days} day(s); \
             `--older-than 0` includes them"
        );
    }
    if live > 0 {
        println!("  untouched: {live} live session(s). `h5i browser close` ends one");
    }
    Ok(())
}

/// Bytes, in the unit a person reads.
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

fn list(root: &Path, all: bool, json: bool) -> anyhow::Result<()> {
    let sessions: Vec<bs::Session> = bs::list(root)?
        .into_iter()
        .filter(|s| all || s.state.is_live())
        .collect();

    if json {
        println!("{}", serde_json::to_string_pretty(&sessions)?);
        return Ok(());
    }
    if sessions.is_empty() {
        println!(
            "  no browser sessions{}. Open one with `h5i browser open <url>`.",
            if all { "" } else { " running" }
        );
        return Ok(());
    }
    let default = bs::read_default(root);
    println!(
        "  {:<14}  {:<10}  {:<8}  {:<9}  {:<15}  URL",
        "SESSION", "ID", "STATE", "PLACED", "LANE"
    );
    for session in sessions {
        let state = match session.state {
            bs::State::Live => style(session.state.as_str()).green(),
            bs::State::Closed => style(session.state.as_str()).dim(),
            _ => style(session.state.as_str()).yellow(),
        };
        // The default is marked rather than hidden: an agent reading this
        // needs to know which row a bare verb will land on.
        let is_default = default.as_deref() == Some(session.id.as_str());
        let shown = match (&session.name, is_default) {
            (Some(name), true) => format!("{name} *"),
            (Some(name), false) => name.clone(),
            (None, true) => "(default) *".to_string(),
            (None, false) => "-".to_string(),
        };
        println!(
            "  {:<14}  {:<10}  {:<8}  {:<9}  {:<15}  {}",
            style(shown).cyan(),
            style(&session.id).dim(),
            state,
            session.placement.as_str(),
            session.lane.as_str(),
            session.url
        );
    }
    Ok(())
}

fn status(root: &Path, selector: Option<&str>, json: bool) -> anyhow::Result<()> {
    // Not `resolve`: a status on a session that has ended is exactly the
    // question worth answering, so this reads the record rather than refusing.
    let mut session = resolve_for_reading(root, selector)?;
    let id = &session.id.clone();
    // Reading status is the moment to notice a death and write it down.
    if session.state.is_live() && !session.probe() {
        bs::end(
            root,
            &mut session,
            bs::State::Died,
            "the session's proxy stopped answering",
        );
    }
    // What the engine knows and the record cannot. `errors` is the only signal
    // that the stored evidence has a hole in it. Best-effort and live-only,
    // because `status` answers about ended sessions too.
    let health = session
        .state
        .is_live()
        .then(|| deliver(&session, &bs::dir(root, id), &json!({"verb": "status"})).ok())
        .flatten()
        .and_then(|reply| reply.get("capture").cloned())
        .filter(|capture| !capture.is_null());

    if json {
        let control = h5i_core::control::read(&bs::dir(root, id));
        let mut value = serde_json::to_value(&session)?;
        value["control_lock"] = serde_json::to_value(&control)?;
        if let Some(health) = health {
            value["capture"] = health;
        }
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    match &session.name {
        Some(name) => println!(
            "  session  : {} ({})",
            style(name).cyan(),
            style(&session.id).dim()
        ),
        None => println!("  session  : {}", style(&session.id).cyan()),
    }
    print_summary(&session);
    let lock = h5i_core::control::read(&bs::dir(root, id));
    println!(
        "  control  : {} (since {})",
        style(lock.holder.as_str()).cyan(),
        lock.since
    );
    if lock.needs_resnapshot {
        println!(
            "  {}    the agent's @refs are stale — re-snapshot before acting \
             (`h5i browser snapshot`)",
            style("stale").yellow()
        );
    }
    if let Some(health) = &health {
        let number = |key: &str| health.get(key).and_then(Value::as_u64).unwrap_or(0);
        let errors = number("errors");
        let line = format!(
            "{} messages, {} bytes",
            number("messages"),
            number("bytes")
        );
        if errors > 0 {
            println!(
                "  captured : {line}, and {} it could not write — the store has a hole in it",
                style(errors).red()
            );
        } else {
            println!("  captured : {line}");
        }
    }
    if let Some(reason) = &session.end_reason {
        println!("  ended    : {} — {}", session.state.as_str(), reason);
    }
    Ok(())
}

/// Past this, a live proxy session that has captured nothing from its targets is
/// worth a warning: the usual cause is a browser that is no longer proxied.
const PROXY_QUIET_SECS: i64 = 300;

/// When the proxy last let a request through, from the end of the receipts.
/// Denied requests do not count: Chromium's own background traffic keeps
/// arriving (and being refused) whether or not the target's does.
fn last_allowed_request(path: &Path) -> Option<chrono::DateTime<chrono::Utc>> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines().rev().find_map(|line| {
        let record: Value = serde_json::from_str(line).ok()?;
        if record.get("allowed").and_then(Value::as_bool) != Some(true) {
            return None;
        }
        let at = record.get("at").and_then(Value::as_str)?;
        chrono::DateTime::parse_from_rfc3339(at)
            .ok()
            .map(|at| at.with_timezone(&chrono::Utc))
    })
}

/// How long ago a proxy session last captured a target request, and a warning
/// with the fix when that is long enough to mean capture has stopped.
fn print_proxy_capture_age(session: &bs::Session, proxy_url: &str) {
    let Some(path) = &session.logs.requests else { return };
    match last_allowed_request(path) {
        None => println!("  last     : no target request captured yet"),
        Some(at) => {
            let secs = (chrono::Utc::now() - at).num_seconds().max(0);
            if secs < PROXY_QUIET_SECS {
                println!("  last     : target request captured {secs}s ago");
            } else {
                println!(
                    "  last     : {} — if the browser is still in use it is no longer going \
                     through this proxy (a relaunched agent-browser without the proxy \
                     settings). Export them and relaunch: {}",
                    style(format!("nothing captured for {}m", secs / 60)).yellow(),
                    agent_browser_proxy_env(proxy_url)
                );
            }
        }
    }
}

fn print_summary(session: &bs::Session) {
    println!("  url      : {}", session.url);
    // One sentence, from the record, so the summary and the audit cannot say
    // different things about the same session.
    let placed = session.where_it_ran();
    match (&session.placement, session.confinement.is_confined()) {
        (bs::Placement::Box { .. }, _) => println!("  placed   : {}", style(&placed).cyan()),
        (_, true) => println!("  placed   : {}", style(&placed).green()),
        (_, false) => println!("  placed   : {}", style(&placed).yellow()),
    }
    // The honest half of the product, printed every time rather than claimed
    // once in a README: what this session's network record actually is.
    let lane = match session.lane {
        bs::Lane::EngineClaimed => style("engine-claimed").yellow(),
        bs::Lane::HostObserved => style("host-observed").green(),
        bs::Lane::ProxyObserved => style("proxy-observed").green(),
    };
    println!(
        "  requests : {} ({})",
        lane,
        match session.lane {
            bs::Lane::EngineClaimed =>
                "fail-closed, and the engine's own account of what it fetched",
            bs::Lane::HostObserved => "also seen at the box's boundary, outside the engine",
            bs::Lane::ProxyObserved => {
                "captured at h5i's proxy; the external browser itself is not confined"
            }
        }
    );
    if session
        .proxy
        .as_ref()
        .is_some_and(|proxy| proxy.chromium_tls == bs::ChromiumTls::IgnoreHttpsErrors)
    {
        println!(
            "  tls      : {} — Chromium accepts every certificate error for this session",
            style("ignore-https-errors").yellow()
        );
    }
    if let (Some(proxy), bs::State::Live) = (&session.proxy, &session.state) {
        print_proxy_capture_age(session, &proxy.url);
    }
    if let Some(project) = &session.project {
        println!("  project  : {project}");
    }
    println!("  policy   : {}", session.policy_digest);
    if !session.scope_digest.is_empty() {
        println!("  scope    : {}", session.scope_digest);
    }
    // Named, not left to the digest: a digest says two sessions differ, and
    // this is the difference that changes what a finding means.
    if session.permissive_cors {
        println!(
            "  cors     : {} — a page here may send this session's credentials \
             cross-origin with `no-cors`, as a browser does",
            style("permissive (--permissive-cors)").yellow()
        );
    }
    if let Some(from) = &session.restored_from {
        println!("  storage  : inherited from {from}");
    }
    if let Some(expires) = &session.expires_at {
        println!("  expires  : {expires}");
    }
}

fn close(
    root: &Path,
    selector: Option<&str>,
    all: bool,
    capture_drop: bool,
    json: bool,
) -> anyhow::Result<()> {
    let mut kept_records = 0usize;
    let targets: Vec<bs::Session> = if all {
        let records = bs::list(root)?;
        kept_records = records.iter().filter(|s| !s.state.is_live()).count();
        records.into_iter().filter(|s| s.state.is_live()).collect()
    } else {
        match bs::resolve(root, selector) {
            Ok(session) => vec![session],
            // Closing something already closed is the state `close` wanted, so
            // it reports rather than fails. Only "no such session" is an error.
            Err(bs::SessionGone::Ended { id, .. }) => vec![bs::read(root, &id)?],
            Err(gone) => anyhow::bail!("{gone}"),
        }
    };

    if targets.is_empty() {
        if json {
            println!("[]");
        } else {
            println!("  no browser session is open.");
            // Say what was kept, so a long list afterwards is not read as a
            // close that failed.
            if kept_records > 0 {
                println!(
                    "  kept     : {kept_records} ended session record(s), which is what \
                     `h5i ui` and `h5i browser list --all` still show."
                );
                println!(
                    "  to erase : `h5i browser rm --ended`, or `h5i browser gc` for just \
                     their stored messages."
                );
            }
        }
        return Ok(());
    }

    if all {
        close_orphaned_browsers(root);
    }
    let mut closed = Vec::new();
    let mut dropped: Vec<String> = Vec::new();
    for mut session in targets {
        if session.state.is_live() {
            stop_engine(&session)?;
            bs::end(root, &mut session, bs::State::Closed, "closed by the user");
        } else if session.proxy.is_some() {
            // Ended without a close: its proxy died or its time ran out, and
            // the browser may still be up. Closing one that is not is a no-op.
            close_proxy_browser(&session);
        }
        // After the engine has stopped, so nothing is still writing there.
        if capture_drop {
            let store = bs::dir(root, &session.id).join(bs::MESSAGES_DIR);
            match std::fs::remove_dir_all(&store) {
                Ok(()) => dropped.push(session.id.clone()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => anyhow::bail!("{} could not be removed: {e}", store.display()),
            }
        }
        if !json {
            println!(
                "{} browser session {} {}. Its record stays at {}.",
                SUCCESS,
                label(&session),
                session.state.describe(),
                bs::dir(root, &session.id).display()
            );
            if dropped.last() == Some(&session.id) {
                println!("  dropped  : its captured messages. The request log stays.");
            }
        }
        closed.push(session);
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&closed)?);
    }
    Ok(())
}

/// The session a *reader* means, live or not. `resolve` finds a name only
/// while it is live; `status`, `audit` and websec read ended ones too.
pub(crate) fn resolve_for_reading(
    root: &Path,
    selector: Option<&str>,
) -> anyhow::Result<bs::Session> {
    match bs::resolve(root, selector) {
        Ok(session) => Ok(session),
        Err(bs::SessionGone::Ended { id, .. }) => Ok(bs::read(root, &id)?),
        Err(gone) => match selector.and_then(|name| bs::find_ended_by_name(root, name)) {
            Some(ended) => Ok(ended),
            None => anyhow::bail!("{gone}"),
        },
    }
}

fn audit(root: &Path, selector: Option<&str>, json: bool) -> anyhow::Result<()> {
    let session = resolve_for_reading(root, selector)?;
    let audit = bs::audit(root, &session);

    if json {
        println!("{}", serde_json::to_string_pretty(&audit)?);
        return Ok(());
    }

    println!("  session  : {}", label(&session));
    print_summary(&session);

    // What could and could not be read, before the rows. A reader has to know
    // whether an empty timeline means a quiet session or a log h5i cannot see.
    let src = &audit.sources;
    // Named only when there *is* a helper log.
    let helpers = match src.helpers {
        bs::Availability::Empty => String::new(),
        other => format!(" · helpers {}", availability(other)),
    };
    // The same rule for the message store: silent on the ordinary session,
    // which was not opened with `--capture` and has nothing to report.
    let messages = match src.messages {
        bs::Availability::Empty => String::new(),
        other => format!(" · messages {}", availability(other)),
    };
    println!(
        "  sources  : actions {} · requests {} · control {}{helpers}{messages}",
        availability(src.actions),
        availability(src.requests),
        availability(src.control)
    );
    if audit.dropped > 0 {
        println!(
            "  {}  {} older rows were dropped by the cap",
            style("capped").yellow(),
            audit.dropped
        );
    }
    // Said once rather than on every row: the proxy's stamps are its own
    // claim about its own clock.
    println!(
        "  {}     session rows are ordered by the clock of the process that wrote them",
        style("note").dim()
    );
    println!();

    for event in &audit.events {
        // The lane on every row, because the two are not the same kind of
        // claim: `host` is what h5i wrote from outside the session, `proxy` is
        // the session's own account, written by its proxy and its verbs.
        let lane = match event.lane {
            h5i_core::browser_events::Lane::HostObserved => style("host  ").green(),
            _ => style("proxy ").yellow(),
        };
        println!("  {lane}  {}", render_event(&event.kind));
    }
    if audit.events.is_empty() {
        println!("  nothing recorded for this session yet.");
    }
    Ok(())
}

fn availability(a: bs::Availability) -> console::StyledObject<&'static str> {
    match a {
        bs::Availability::Read => style(a.as_str()).green(),
        bs::Availability::Empty => style(a.as_str()).dim(),
        // The one that must stand out: nothing can be concluded from the
        // silence of a log h5i could not read.
        bs::Availability::Unavailable => style(a.as_str()).red(),
        // Nor from the end of one that was cut short.
        bs::Availability::Partial => style(a.as_str()).yellow(),
    }
}

/// One audit row, as a line.
fn render_event(kind: &h5i_core::browser_events::EventKind) -> String {
    use h5i_core::browser_events::EventKind as K;
    match kind {
        K::Lifecycle { state, reason } => format!(
            "session {state}{}",
            reason
                .as_deref()
                .map(|r| format!("  ({r})"))
                .unwrap_or_default()
        ),
        K::Control { holder, note } => format!(
            "control -> {holder}{}",
            note.as_deref()
                .map(|n| format!("  ({n})"))
                .unwrap_or_default()
        ),
        K::AgentAction { action, forwarded } => {
            format!("{} {action}", if *forwarded { "verb  " } else { "verb !" })
        }
        K::Request {
            seq,
            method,
            url,
            allowed,
            denied_reason,
            ..
        } => {
            if *allowed {
                format!("#{seq} {method} {url}")
            } else {
                format!(
                    "#{seq} DENIED {method} {url}  ({})",
                    denied_reason.as_deref().unwrap_or("no reason recorded")
                )
            }
        }
        K::Response {
            seq,
            status,
            bytes,
            error,
            ..
        } => match (error, status) {
            (Some(error), _) => format!("#{seq} failed  ({error})"),
            (None, Some(status)) => format!(
                "#{seq} {status}{}",
                bytes.map(|b| format!("  {b} bytes")).unwrap_or_default()
            ),
            // A denied request never reaches the wire, so its outcome row has
            // no status at all. Saying so beats printing an empty line that
            // reads as a response nobody recorded.
            (None, None) => format!("#{seq} no response (refused before the wire)"),
        },
        K::Navigated { url } => format!("page {url}"),
        K::Console { level, text } => format!("console {} {text}", level.as_str()),
        K::PolicyVerdict { subject, reason } => format!("refused {subject}  ({reason})"),
        K::SessionReset { source } => format!("source restarted: {source}"),
        // Only in records an older h5i wrote: its fetches are not in the rows
        // around it.
        K::Helper {
            name,
            argv,
            status,
            note,
        } => {
            let outcome = match status {
                Some(0) | None => String::new(),
                Some(code) => format!("  (exit {code})"),
            };
            format!(
                "helper {name} {}{outcome}{}",
                argv.join(" "),
                note.as_deref()
                    .map(|n| format!("  {n}"))
                    .unwrap_or_default()
            )
        }
    }
}

/// The box service an older h5i ran a boxed session's engine as. Only an
/// ended record can still name one, and `close` on it stops what is left.
const BROWSER_SERVICE: &str = "h5i-browser";

/// End the processes behind a session: its capture proxy and its Chromium,
/// which nothing else reaps. An older boxed record goes through `service_stop`.
fn stop_engine(session: &bs::Session) -> anyhow::Result<()> {
    match &session.placement {
        bs::Placement::Host => {
            if let Some(pid) = session.control.pid
                && process_is_ours(pid)
            {
                kill(pid);
            }
            close_proxy_browser(session);
            Ok(())
        }
        bs::Placement::Box { name } => {
            let repo = super::discover_repo("h5i browser close")?;
            let h5i_root = h5i_core::storage::h5i_root_for_repo(&repo)?;
            let manifest = h5i_core::env::find(&h5i_root, name)?;
            match h5i_core::env::service_stop(&repo, &h5i_root, &manifest, BROWSER_SERVICE) {
                Ok(_) => Ok(()),
                // A service that is already gone is the state `close` wanted.
                Err(e) => {
                    eprintln!("  note     the box had no engine left to stop ({e})");
                    Ok(())
                }
            }
        }
    }
}

fn take(root: &Path, selector: Option<&str>) -> anyhow::Result<()> {
    let session = bs::resolve(root, selector).map_err(|e| anyhow::anyhow!("{e}"))?;
    let dir = bs::dir(root, &session.id);
    let control = h5i_core::control::take(&dir)?;
    bs::journal_control(&dir, control.holder.as_str(), Some("taken by a human"));
    println!(
        "{} control taken by {} — the agent's automation is paused",
        SUCCESS,
        control.holder.as_str()
    );
    // Say which kind of pause this is, because the two are genuinely different
    // and only one of them is a boundary.
    match &session.placement {
        bs::Placement::Box { .. } => println!(
            "  {}  the session is in a box, so this is enforced: every verb is carried in \
             from here and none of them is now",
            style("enforced").green()
        ),
        bs::Placement::Host => println!(
            "  {} the session runs on this machine, so this pauses `h5i browser` and nothing \
             else: an agent that drives the engine binary directly is not stopped by it. \
             Place the session in a box (`--in`) to make the pause a boundary.",
            style("advisory").yellow()
        ),
    }
    Ok(())
}

fn release(root: &Path, selector: Option<&str>) -> anyhow::Result<()> {
    let session = bs::resolve(root, selector).map_err(|e| anyhow::anyhow!("{e}"))?;
    let dir = bs::dir(root, &session.id);
    let control = h5i_core::control::release(&dir)?;
    bs::journal_control(
        &dir,
        control.holder.as_str(),
        Some("handed back; the agent must re-snapshot"),
    );
    println!(
        "{} control returned to {} — it must re-snapshot before acting",
        SUCCESS,
        control.holder.as_str()
    );
    Ok(())
}

fn viewer_url(name: &str, port: u16) -> anyhow::Result<()> {
    let repo = super::discover_repo("h5i browser url")?;
    let h5i_root = h5i_core::storage::h5i_root_for_repo(&repo)?;
    let manifest = h5i_core::env::find(&h5i_root, name)?;
    let dir = h5i_core::env::env_dir(&h5i_root, &manifest.agent, &manifest.slug);
    let token = h5i_core::view::read_token(&dir).ok_or_else(|| {
        anyhow::anyhow!("this box has no viewer token — it predates the viewer. Create a new box.")
    })?;
    // Printed whether or not a forward is running: the URL is a property of the
    // box, and `h5i box view` is what makes it answer.
    println!("http://127.0.0.1:{port}/?token={token}");
    Ok(())
}

/// Render a session's answer for a person: a known text field, or the JSON.
fn print_answer(answer: &Value) {
    let body = answer.get("data").unwrap_or(answer);
    for key in ["outline", "text", "markdown", "message"] {
        if let Some(text) = body.get(key).and_then(Value::as_str) {
            println!("{text}");
            return;
        }
    }
    if let Some(text) = body.as_str() {
        println!("{text}");
        return;
    }
    println!(
        "{}",
        serde_json::to_string_pretty(body).unwrap_or_else(|_| body.to_string())
    );
}

/// Put the child in its own session, so closing the terminal that started it
/// does not take the browser with it.
#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn detach(_command: &mut Command) {}

/// Whether a recorded pid is still an h5i session process, not one the system
/// reused the number for after it ended. Linux can say; elsewhere it is
/// assumed, as before.
fn process_is_ours(pid: u32) -> bool {
    match std::fs::read(format!("/proc/{pid}/cmdline")) {
        Ok(cmdline) => {
            let cmdline = String::from_utf8_lossy(&cmdline);
            cmdline.contains("__capture-proxy") || cmdline.contains("__engine")
        }
        Err(_) => !cfg!(target_os = "linux"),
    }
}

#[cfg(unix)]
fn kill(pid: u32) {
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
}

#[cfg(not(unix))]
fn kill(_pid: u32) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_resend_takes_one_request_source() {
        let root = Path::new("/not-reached");
        for request in [
            json!({"from": 7, "request": {"method": "GET", "url": "https://example.test"}}),
            json!({}),
        ] {
            let error = rpc_resend(root, None, &request).expect_err("ambiguous source");
            assert!(error.to_string().contains("exactly one"), "{error}");
        }
    }


    #[test]
    fn a_sandbox_failure_points_at_no_sandbox_and_proxy_open_honours_it() {
        assert!(sandbox_hint("[FATAL] No usable sandbox! If you are running on Ubuntu")
            .contains("--no-sandbox"));
        assert!(sandbox_hint("some other failure").is_empty());
        // The AGENT_BROWSER_ARGS value the hint names reaches a proxy-open
        // launch (it would be shadowed if --args were passed alone), while the
        // loopback bypass is always kept.
        let args = agent_browser_proxy_args_with("http://127.0.0.1:9", Some("--no-sandbox"));
        let launch = args.last().unwrap();
        assert!(launch.contains("--proxy-bypass-list=<-loopback>"), "{launch}");
        assert!(launch.contains("--no-sandbox"), "{launch}");
        // With nothing set, only the loopback bypass is present.
        let bare = agent_browser_proxy_args_with("http://127.0.0.1:9", None);
        assert_eq!(bare.last().unwrap(), "--proxy-bypass-list=<-loopback>");
    }

    #[test]
    fn an_agent_browser_version_is_read_and_compared() {
        assert_eq!(agent_browser_version("agent-browser 0.39.1\n"), Some((0, 39, 1)));
        assert_eq!(agent_browser_version("0.24.0"), Some((0, 24, 0)));
        assert!(agent_browser_version("agent-browser 0.24.1").unwrap() < MIN_AGENT_BROWSER);
        assert!(agent_browser_version("agent-browser 1.0.0-beta.2").unwrap() >= MIN_AGENT_BROWSER);
        assert_eq!(agent_browser_version("agent-browser dev"), None);
    }

    #[test]
    fn the_printed_environment_keeps_a_relaunched_agent_browser_proxied() {
        let env = agent_browser_proxy_env("http://127.0.0.1:9");
        assert_eq!(
            env,
            "export AGENT_BROWSER_PROXY=http://127.0.0.1:9 AGENT_BROWSER_IGNORE_HTTPS_ERRORS=1 \
             AGENT_BROWSER_ARGS='--proxy-bypass-list=<-loopback>'"
        );
    }

    #[test]
    fn the_last_capture_counts_only_requests_the_proxy_let_through() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("requests.jsonl");
        std::fs::write(
            &path,
            "{\"seq\":1,\"at\":\"2026-10-16T10:00:00Z\",\"allowed\":true}\n\
             {\"seq\":2,\"at\":\"2026-10-16T11:00:00Z\",\"allowed\":false}\n",
        )
        .unwrap();
        let at = last_allowed_request(&path).unwrap();
        assert_eq!(at.to_rfc3339(), "2026-10-16T10:00:00+00:00");
        std::fs::write(&path, "").unwrap();
        assert!(last_allowed_request(&path).is_none());
    }

    #[test]
    fn the_printed_agent_browser_line_accepts_forged_certs_and_keeps_loopback() {
        let line = agent_browser_open_line(
            "/usr/bin/agent-browser",
            "http://127.0.0.1:9",
            "https://app.example/a b",
        );
        assert_eq!(
            line,
            "/usr/bin/agent-browser --proxy http://127.0.0.1:9 --ignore-https-errors --args '--proxy-bypass-list=<-loopback>' open 'https://app.example/a b'"
        );
        // h5i accepts the proxy's certificates with --ignore-https-errors and passes no
        // --ca-cert; changing that is a decision, not a side effect.
        assert!(!line.contains("ca-cert"), "{line}");
    }

    #[test]
    fn an_older_proxy_record_without_chromium_tls_means_the_session_ca() {
        let proxy: bs::Proxy =
            serde_json::from_str(r#"{"url":"http://127.0.0.1:9","ca_cert":"/tmp/ca.pem"}"#)
                .expect("old record");
        assert_eq!(proxy.chromium_tls, bs::ChromiumTls::SessionCa);
    }

    #[test]
    fn this_host_selects_the_chromium_trust_it_can_install() {
        let trust = host_proxy_chromium_tls();
        if cfg!(target_os = "linux") || cfg!(target_os = "macos") {
            assert_eq!(
                trust.expect("linux and macOS drive a proxied Chromium"),
                bs::ChromiumTls::IgnoreHttpsErrors
            );
        } else {
            let error = trust.expect_err("other platforms refuse before a session exists");
            assert!(error.to_string().contains("nothing was started"), "{error}");
        }
    }

}
