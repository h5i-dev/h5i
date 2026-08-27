//! `h5i browser` — the front door.
//!
//! One noun an agent has to learn: a **session**. `h5i browser open` makes one
//! and every other verb acts on it. Nothing else is agent-facing. Not the
//! process that renders the page, not the port it listens on, not whether it is
//! running inside a box — and not, in the ordinary case, the session itself.
//!
//! # The ordinary case types no id
//!
//! ```text
//! h5i browser open https://example.com
//! h5i browser snapshot
//! h5i browser click @e3
//! h5i browser close
//! ```
//!
//! `open` makes a session and points the **default** at it; every later verb
//! follows that pointer. The opaque id (`br_7k2xqa`) still exists, and it is
//! what `--json` and the receipts carry, because a durable reference must
//! survive a rename. It is simply not what a person or an agent types.
//!
//! Demanding one on every verb is the shape of a remote-browser HTTP API, where
//! the id exists because the client and the browser share nothing else. Here
//! they share a filesystem, so the id can stay where it belongs.
//!
//! Running several at once is what `--session <name>` is for
//! (`h5i browser open <url> --session auth`), and a name is comfortable to type
//! precisely because it is not an identity: it can be reused once the session
//! it named has ended. The id cannot, which is why the id is what gets written
//! down.
//!
//! # Containment is a placement, not a product
//!
//! Started with no flags, a session runs here, in this user's ordinary process
//! space, exactly like any other headless browser. What it still does that no
//! other headless browser does is **record**: the engine checks every request
//! against the session's policy and writes the decision before the bytes move,
//! and it refuses the fetch when it cannot write the record
//! (the engine's `net::Broker`). That is the default, and it is
//! auditability rather than containment — the honest claim, because the engine
//! is describing itself.
//!
//! `--in <box>` places the same session inside a box. Every verb keeps its name
//! and its answer; what changes is who saw the network. The box's egress
//! enforcement is h5i's, at a boundary outside the thing being described, so
//! the session's lane goes from engine-claimed to host-observed
//! ([`h5i_core::browser_session::Lane`]). That is what a box buys, stated as
//! something a reader can check rather than as an adjective.
//!
//! # Why verbs are carried rather than dialled
//!
//! The engine's control listener is loopback TCP. A supervised box always has
//! its own network namespace, so that port is not the host's to connect to, and
//! the fix is not to punch a hole: it is to hand the verb to `h5i box run`,
//! which is the same path a person typing the command takes. Two things fall
//! out of that, both wanted. Every verb into a box gets a receipt like any
//! other run. And the control lock is checked **here**, on the host, outside
//! the box — which is the one configuration in which it is a boundary rather
//! than a request (see `h5i_core::browser_proxy`'s limits, which describe the
//! opposite arrangement).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use clap::Subcommand;
use console::style;
use serde_json::Value;

use h5i_core::browser_session as bs;
use h5i_core::ui::SUCCESS;

/// How long `start` waits for the engine to advertise its control file.
///
/// Generous, because the first thing a session does is fetch and render the URL
/// it was given, and a cold font scan is not instant. A start that gives up
/// says what it was waiting for and leaves the engine's own log behind.
const START_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Subcommand)]
pub enum BrowserCommands {
    /// Open a URL, making a session if there is not one already.
    ///
    /// With no `--session`, this uses the default session and points the
    /// default at whatever it makes, so the verbs that follow need no id.
    /// Without `--in`, the session runs here with no containment beyond the
    /// engine itself; with `--in <box>`, the same session runs inside that box.
    Open {
        /// A URL, or a path to a local HTML file.
        url: String,

        /// Name this session, so several can run at once.
        ///
        /// A name is not an identity: it can be reused once the session it
        /// named has ended. The opaque id in `--json` and in receipts is what
        /// cannot be, and is what to keep when you need a durable reference.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,

        /// Make a new session even if one is already open.
        ///
        /// Without this, `open` navigates the session it finds, because that is
        /// what opening a URL means when a browser is already up.
        #[arg(long)]
        new: bool,

        /// Run the session inside this box instead of on this machine.
        ///
        /// The box must already exist (`h5i box`). Its egress allowlist is
        /// enforced at the box's boundary, so the session's request lane
        /// becomes host-observed.
        #[arg(long = "in", value_name = "BOX")]
        in_box: Option<String>,

        /// Grant an origin. Repeatable. Without any, nothing remote is
        /// reachable except the URL's own origin.
        #[arg(long = "allow", value_name = "ORIGIN")]
        allow: Vec<String>,

        /// Refuse loopback too. It is reachable by default: it is the dev
        /// server.
        #[arg(long)]
        no_loopback: bool,

        /// Run the page's own JavaScript. Off by default: with script off,
        /// page-borne prompt injection has no delivery channel at all.
        #[arg(long)]
        script: bool,

        /// Viewport width.
        #[arg(long, default_value_t = 1280)]
        width: u32,

        /// Viewport height.
        #[arg(long, default_value_t = 720)]
        height: u32,

        /// End the session automatically after this many seconds.
        ///
        /// Recorded as an ending on the session's record when it passes, not as
        /// a disappearance: `h5i browser status` still answers afterwards, and
        /// says it expired.
        #[arg(long, value_name = "SECONDS")]
        expires_in: Option<u64>,

        /// Seed this session's storage from one that has ended.
        ///
        /// A restore is a **new session with a new id**, and the inheritance is
        /// written into its record. Nothing resurrects an id: an agent holding
        /// a stale one always gets a refusal, never a different session wearing
        /// the same name. Takes an id, not a name, because a name can be reused
        /// and storage has to come from one definite session.
        #[arg(long, value_name = "SESSION_ID")]
        restore: Option<String>,

        /// Print the session record as JSON instead of a summary line.
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

    /// End a session. Its record stays.
    Close {
        /// Which session, when more than one is open. A name from
        /// `--session` at open time, or an opaque id. Defaults to
        /// $H5I_BROWSER_SESSION, then to the session `open` last made.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        /// End every live session on this machine.
        #[arg(long, conflicts_with = "session")]
        all: bool,
        #[arg(long)]
        json: bool,
    },

    /// The page as a model should read it: the outline, with `@ref` handles.
    Snapshot {
        /// Which session, when more than one is open. A name from
        /// `--session` at open time, or an opaque id. Defaults to
        /// $H5I_BROWSER_SESSION, then to the session `open` last made.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        /// Report only what changed since the last snapshot.
        #[arg(long)]
        delta: bool,
        #[arg(long)]
        json: bool,
    },

    /// Go to a URL, resolved against the current page like a click would be.
    Navigate {
        /// Which session, when more than one is open. A name from
        /// `--session` at open time, or an opaque id. Defaults to
        /// $H5I_BROWSER_SESSION, then to the session `open` last made.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        url: String,
        #[arg(long)]
        json: bool,
    },

    /// Follow a `@ref` from the last snapshot.
    Click {
        /// Which session, when more than one is open. A name from
        /// `--session` at open time, or an opaque id. Defaults to
        /// $H5I_BROWSER_SESSION, then to the session `open` last made.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        /// `e3` or `@e3`, from a `snapshot`.
        reference: String,
        #[arg(long)]
        json: bool,
    },

    /// Put text into a field, replacing what was there.
    Type {
        /// Which session, when more than one is open. A name from
        /// `--session` at open time, or an opaque id. Defaults to
        /// $H5I_BROWSER_SESSION, then to the session `open` last made.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        reference: String,
        text: String,
        #[arg(long)]
        json: bool,
    },

    /// Submit the form containing a `@ref`.
    Submit {
        /// Which session, when more than one is open. A name from
        /// `--session` at open time, or an opaque id. Defaults to
        /// $H5I_BROWSER_SESSION, then to the session `open` last made.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        reference: String,
        #[arg(long)]
        json: bool,
    },

    /// Scroll the page. Negative scrolls up.
    Scroll {
        /// Which session, when more than one is open. A name from
        /// `--session` at open time, or an opaque id. Defaults to
        /// $H5I_BROWSER_SESSION, then to the session `open` last made.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        #[arg(allow_negative_numbers = true)]
        by: f64,
        #[arg(long)]
        json: bool,
    },

    /// Wait until something is on the page, or until nothing can put it there.
    WaitFor {
        /// Which session, when more than one is open. A name from
        /// `--session` at open time, or an opaque id. Defaults to
        /// $H5I_BROWSER_SESSION, then to the session `open` last made.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        #[arg(long, value_name = "CSS")]
        selector: Option<String>,
        #[arg(long, value_name = "TEXT", conflicts_with = "selector")]
        text: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Wait until a page expression is true. Needs `open --script`.
    WaitForScript {
        /// Which session, when more than one is open. A name from
        /// `--session` at open time, or an opaque id. Defaults to
        /// $H5I_BROWSER_SESSION, then to the session `open` last made.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        expr: String,
        #[arg(long)]
        json: bool,
    },

    /// Pull structured data out of the page by selector.
    Extract {
        /// Which session, when more than one is open. A name from
        /// `--session` at open time, or an opaque id. Defaults to
        /// $H5I_BROWSER_SESSION, then to the session `open` last made.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        /// The schema, as JSON.
        schema: String,
        #[arg(long)]
        json: bool,
    },

    /// The page as markdown: what a reader would read, without the handles.
    Markdown {
        /// Which session, when more than one is open. A name from
        /// `--session` at open time, or an opaque id. Defaults to
        /// $H5I_BROWSER_SESSION, then to the session `open` last made.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        #[arg(long, value_name = "BYTES")]
        max_bytes: Option<usize>,
        #[arg(long)]
        json: bool,
    },

    /// Everything recorded about this session, in one ordered timeline.
    ///
    /// What the agent asked for, what the engine decided about each fetch, who
    /// was driving, and how the session ended. Each row says which lane it came
    /// from: the action and request logs are the engine's own account, the
    /// handovers and the lifecycle are h5i's, written from outside.
    ///
    /// `requests` is the network layer of this on its own, and stays the verb
    /// to reach for in a loop. This is the one to read afterwards.
    Audit {
        /// Which session, when more than one is open. A name from
        /// `--session` at open time, or an opaque id. Defaults to
        /// $H5I_BROWSER_SESSION, then to the session `open` last made.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// The request log: what this session asked for, and what was refused.
    ///
    /// The engine is the HTTP client, so this is the decision record written
    /// before the bytes moved, not an observation made from beside the network.
    /// If a request is not in this list, it did not happen.
    Requests {
        /// Which session, when more than one is open. A name from
        /// `--session` at open time, or an opaque id. Defaults to
        /// $H5I_BROWSER_SESSION, then to the session `open` last made.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        /// Only what happened after this sequence number.
        #[arg(long, value_name = "SEQ")]
        since: Option<u64>,
        #[arg(long)]
        json: bool,
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

    /// Hand the page to the human at the live view for as long as a login takes.
    Login {
        /// Which session, when more than one is open. A name from
        /// `--session` at open time, or an opaque id. Defaults to
        /// $H5I_BROWSER_SESSION, then to the session `open` last made.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,
        /// End login mode and make the page readable again.
        #[arg(long)]
        off: bool,
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
    let _ = bs::expire_due(&root);

    match action {
        BrowserCommands::Open {
            url,
            session,
            new,
            in_box,
            allow,
            no_loopback,
            script,
            width,
            height,
            expires_in,
            restore,
            json,
        } => open(
            &root,
            session,
            new,
            StartOptions {
                url,
                in_box,
                allow,
                no_loopback,
                script,
                width,
                height,
                expires_in,
                restore,
            },
            json,
        ),
        BrowserCommands::List { all, json } => list(&root, all, json),
        BrowserCommands::Status { session, json } => status(&root, session.as_deref(), json),
        BrowserCommands::Close { session, all, json } => close(&root, session.as_deref(), all, json),

        BrowserCommands::Snapshot {
            session,
            delta,
            json,
        } => {
            let mut argv = vec!["snapshot".to_string()];
            if delta {
                argv.push("--delta".into());
            }
            verb(&root, session.as_deref(), argv, false, json)
        }
        BrowserCommands::Navigate { session, url, json } => {
            verb(&root, session.as_deref(), vec!["navigate".into(), url], true, json)
        }
        BrowserCommands::Click {
            session,
            reference,
            json,
        } => verb(&root, session.as_deref(), vec!["click".into(), reference], true, json),
        BrowserCommands::Type {
            session,
            reference,
            text,
            json,
        } => verb(
            &root,
            session.as_deref(),
            vec!["type".into(), reference, text],
            true,
            json,
        ),
        BrowserCommands::Submit {
            session,
            reference,
            json,
        } => verb(&root, session.as_deref(), vec!["submit".into(), reference], true, json),
        BrowserCommands::Scroll { session, by, json } => verb(
            &root,
            session.as_deref(),
            vec!["scroll".into(), by.to_string()],
            true,
            json,
        ),
        BrowserCommands::WaitFor {
            session,
            selector,
            text,
            json,
        } => {
            let mut argv = vec!["wait-for".to_string()];
            if let Some(selector) = selector {
                argv.push("--selector".into());
                argv.push(selector);
            }
            if let Some(text) = text {
                argv.push("--text".into());
                argv.push(text);
            }
            verb(&root, session.as_deref(), argv, false, json)
        }
        BrowserCommands::WaitForScript {
            session,
            expr,
            json,
        } => verb(
            &root,
            session.as_deref(),
            vec!["wait-for-script".into(), expr],
            false,
            json,
        ),
        BrowserCommands::Extract {
            session,
            schema,
            json,
        } => verb(&root, session.as_deref(), vec!["extract".into(), schema], false, json),
        BrowserCommands::Markdown {
            session,
            max_bytes,
            json,
        } => {
            let mut argv = vec!["markdown".to_string()];
            if let Some(max) = max_bytes {
                argv.push("--max-bytes".into());
                argv.push(max.to_string());
            }
            verb(&root, session.as_deref(), argv, false, json)
        }
        BrowserCommands::Requests {
            session,
            since,
            json,
        } => {
            let mut argv = vec!["requests".to_string()];
            if let Some(since) = since {
                argv.push("--since".into());
                argv.push(since.to_string());
            }
            verb(&root, session.as_deref(), argv, false, json)
        }
        BrowserCommands::Audit { session, json } => audit(&root, session.as_deref(), json),
        BrowserCommands::Env { session, json } => {
            verb(&root, session.as_deref(), vec!["env".into()], false, json)
        }
        BrowserCommands::Login { session, off, json } => {
            let mut argv = vec!["login".to_string()];
            argv.push(if off { "--off".into() } else { "--on".into() });
            // Not mutating in the lock's sense: `login` is how a human takes the
            // keyboard, so refusing it while a human holds control would refuse
            // the very thing they are here to do.
            verb(&root, session.as_deref(), argv, false, json)
        }

        BrowserCommands::Take { session } => take(&root, session.as_deref()),
        BrowserCommands::Release { session } => release(&root, session.as_deref()),
        BrowserCommands::Url { name, port } => viewer_url(&name, port),
    }
}

struct StartOptions {
    url: String,
    in_box: Option<String>,
    allow: Vec<String>,
    no_loopback: bool,
    script: bool,
    width: u32,
    height: u32,
    expires_in: Option<u64>,
    restore: Option<String>,
}

/// Open a URL: navigate the session that is already there, or make one.
///
/// The two halves are deliberately not one. Opening a URL in a browser that is
/// already up means *go there*, and making a second session behind the agent's
/// back would leave the first one holding a page nothing points at any more.
/// So a live session is navigated, and `--new` is how you say you meant a
/// second one.
///
/// The flags that only make sense at creation are refused rather than ignored
/// when a session is reused. A session's policy is fixed when its engine
/// starts, so accepting `--allow` here and doing nothing with it would be a
/// grant the caller believes it made.
fn open(
    root: &Path,
    selector: Option<String>,
    force_new: bool,
    opts: StartOptions,
    json: bool,
) -> anyhow::Result<()> {
    if !force_new
        && let Ok(existing) = bs::resolve(root, selector.as_deref())
    {
        let creation_only = creation_flags(&opts);
        if !creation_only.is_empty() {
            anyhow::bail!(
                "browser session {} is already open, and its policy was fixed when its \
                 engine started, so {} cannot apply now.\n\n  \
                 Open a second session with `--new`, or end this one with \
                 `h5i browser close` first.",
                label(&existing),
                creation_only.join(" and ")
            );
        }
        let dir = bs::dir(root, &existing.id);
        let mut answer = deliver(&existing, &dir, vec!["navigate".into(), opts.url.clone()])?;
        bs::scrub(&mut answer);
        if answer.get("ok").and_then(Value::as_bool) == Some(false) {
            // The session is still on whatever it was on. Saying otherwise
            // would leave an agent acting on a page it never reached.
            if json {
                println!("{}", serde_json::to_string_pretty(&answer)?);
            }
            anyhow::bail!("{} did not go to {}: {}", label(&existing), opts.url, refusal(&answer));
        }
        // The record follows the page. `url` is what this session was last told
        // to open, so `h5i browser list` keeps naming something true.
        let mut moved = existing.clone();
        moved.url = opts.url.clone();
        let _ = bs::write(root, &moved);
        if json {
            let mut record = serde_json::to_value(&moved)?;
            record["navigated"] = answer;
            println!("{}", serde_json::to_string_pretty(&record)?);
        } else {
            println!("{} {} is now on {}", SUCCESS, label(&moved), opts.url);
        }
        return Ok(());
    }
    start(root, selector, opts, json)
}

/// Which creation-only flags the caller set, named the way they typed them.
fn creation_flags(opts: &StartOptions) -> Vec<&'static str> {
    let mut set = Vec::new();
    if !opts.allow.is_empty() {
        set.push("`--allow`");
    }
    if opts.in_box.is_some() {
        set.push("`--in`");
    }
    if opts.script {
        set.push("`--script`");
    }
    if opts.no_loopback {
        set.push("`--no-loopback`");
    }
    if opts.expires_in.is_some() {
        set.push("`--expires-in`");
    }
    if opts.restore.is_some() {
        set.push("`--restore`");
    }
    set
}

/// How to refer to a session in a sentence: the name if it has one, and the id
/// otherwise, because the id is the only thing every session has.
fn label(session: &bs::Session) -> String {
    match &session.name {
        Some(name) => format!("`{name}` ({})", session.id),
        None => session.id.clone(),
    }
}

fn start(
    root: &Path,
    name: Option<String>,
    opts: StartOptions,
    json: bool,
) -> anyhow::Result<()> {
    // The box h5i is standing in, if it is standing in one. Read once, here,
    // rather than at each use: the three markers are the host's, and a process
    // does not move between boxes while it runs.
    let enclosing_box = h5i_core::env::in_env_box()
        .then(|| std::env::var(h5i_core::env::H5I_ENV_ID_VAR).ok())
        .flatten();

    if let (Some(target), Some(here)) = (&opts.in_box, &enclosing_box) {
        // `--in` means "put this session in a box I am outside of", and that is
        // the whole reason it can promise an enforced takeover and a lane the
        // engine did not claim for itself. From in here neither is true, and a
        // box inside a box is not a thing this product has.
        anyhow::bail!(
            "`--in {target}` cannot run from inside a box. This process is already in \
             `{here}`.\n\n  \
             Open the session without `--in`: it runs beside you, in this box, and its \
             record says so. To place a session in a box from outside one, run \
             `h5i browser open --in {target}` on the host."
        );
    }

    let placement = match &opts.in_box {
        None => bs::Placement::Host,
        Some(name) => bs::Placement::Box { name: name.clone() },
    };

    // The inheritance is resolved before anything is spawned, so a bad
    // `--restore` fails before there is a session to clean up.
    let restored_from = match &opts.restore {
        None => None,
        Some(id) => {
            let previous = bs::read(root, id)?;
            Some(previous.id)
        }
    };

    let id = bs::new_id(root)?;
    let dir = bs::dir(root, &id);

    let mut spawned = match &placement {
        bs::Placement::Host => spawn_on_host(&dir, &opts, enclosing_box.is_some())?,
        bs::Placement::Box { name } => spawn_in_box(name, &dir, &opts)?,
    };
    let lane = bs::Session::lane_for(&placement, spawned.boundary_enforced);

    if let Some(from) = &restored_from {
        seed_storage(root, from, &dir)?;
    }

    let session = bs::Session {
        id: id.clone(),
        name: name.clone(),
        engine: bs::Engine::H5iLight,
        lane,
        placement,
        url: opts.url.clone(),
        // Microseconds, to match the rest of the record: these stamps interleave
        // with the engine's in an audit, and a whole agent loop fits inside one
        // second.
        started_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        expires_at: opts.expires_in.map(|secs| {
            (chrono::Utc::now() + chrono::Duration::seconds(secs as i64))
                .to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
        }),
        storage: bs::Storage::Ephemeral,
        policy_digest: spawned.policy_digest.clone(),
        restored_from,
        state: bs::State::Live,
        ended_at: None,
        end_reason: None,
        enclosing_box,
        control: bs::Control {
            channel: spawned.channel,
            file: Some(spawned.control_in_engine_view.clone()),
            witness: spawned.control_on_host.clone(),
            pid: spawned.pid,
        },
        logs: spawned.logs.clone(),
    };
    bs::write(root, &session)?;
    // The default follows the newest session whether or not it was named, so a
    // `--session auth` that is the only thing running is still what a bare
    // `h5i browser snapshot` acts on. A name adds a way to address it; it does
    // not take away the ordinary one.
    let _ = bs::set_default(root, &session.id);

    // Wait for the engine to say it is up, and record the death if it is not.
    if let Err(e) = await_control(&mut spawned, &dir) {
        // Stop whatever did start. A timeout that leaves an engine running is a
        // session nothing owns: its record says died, its process is serving,
        // and the next start in the same box collides with it.
        (spawned.stop)();
        let mut dead = session.clone();
        bs::end(root, &mut dead, bs::State::Died, &e);
        anyhow::bail!("{e}\n\n  The session is recorded as `{}`, died.", dead.id);
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&session)?);
    } else {
        match &session.name {
            Some(name) => println!(
                "{} browser session {} ({})",
                SUCCESS,
                style(name).cyan(),
                style(&session.id).dim()
            ),
            None => println!("{} browser session {}", SUCCESS, style(&session.id).dim()),
        }
        print_summary(&session);
        // The next command, spelled the way it is actually used. Printing the
        // id here and expecting it back would teach the id as the interface.
        let sel = match &session.name {
            Some(name) => format!(" --session {name}"),
            None => String::new(),
        };
        println!("\n  next     : {}", style(format!("h5i browser snapshot{sel}")).dim());
    }
    Ok(())
}

/// What a spawn produced, in both views of the filesystem it has to be seen in.
struct Spawned {
    /// Which channel the engine is listening on.
    channel: bs::Channel,
    /// Ask whether the engine is still on its way up. `Some(reason)` means it
    /// is not, and the reason is what the user is told.
    ///
    /// A closure rather than a pid, because the honest answer differs by
    /// placement and a pid cannot carry that. On the host it owns the `Child`
    /// and asks `try_wait` — **a child nobody waits on is a zombie, and a
    /// zombie answers `kill(pid, 0)`**, so polling the pid would wait the full
    /// timeout on an engine that exited immediately, which is the commonest
    /// failure there is. In a box it asks the service registry, which knows
    /// that a microvm's pid is a guest pid and not this machine's to signal.
    alive: Box<dyn FnMut() -> Option<String>>,
    /// The process this machine can signal, when there is one. `None` for a
    /// boxed session: at the microvm tier the service's pid belongs to the
    /// guest, and a host `kill` on that number would be aimed at whatever
    /// unrelated process happens to hold it.
    pid: Option<u32>,
    /// The control file's path **as the engine sees it**. Inside a box that is a
    /// box path; on the host it is a host path. Never mixed: binding one and
    /// reading the other is how enforcement goes silently missing.
    control_in_engine_view: PathBuf,
    /// The same file as this machine sees it, when it can.
    control_on_host: Option<PathBuf>,
    policy_digest: String,
    /// Where this machine can read the session's logs, when it can.
    logs: bs::Logs,
    /// Whether something outside the engine actually decides what may leave.
    /// See [`bs::Session::lane_for`] — this is the input to the one claim the
    /// product makes that a reader can check.
    boundary_enforced: bool,
    /// How to end it. `close` calls this before recording the ending.
    stop: Box<dyn FnOnce()>,
}

fn spawn_on_host(dir: &Path, opts: &StartOptions, in_a_box: bool) -> anyhow::Result<Spawned> {
    // A port on a bare host, a socket inside a box.
    //
    // The port is the simpler channel and the session directory can be
    // anywhere, which matters: a socket address is capped near 100 bytes and a
    // registry under a long temp path would exceed it. Inside a box a port is
    // not merely awkward, it does not work — the netns may have no usable
    // loopback at all, and `net.mode = deny` leaves nothing to dial. The
    // registry inside a box lives under the box's own tmp, which is short.
    let channel = if in_a_box {
        bs::Channel::Socket
    } else {
        bs::Channel::Port
    };
    let control = match channel {
        bs::Channel::Port => dir.join(bs::CONTROL_FILE),
        bs::Channel::Socket => dir.join(bs::CONTROL_FILE).with_extension("sock"),
    };
    let mut argv: Vec<String> = vec![
        ENGINE_SUBCOMMAND.into(),
        "serve".into(),
        opts.url.clone(),
        "--stream-file".into(),
        dir.join(bs::STREAM_FILE).display().to_string(),
        channel.flag().into(),
        control.display().to_string(),
        "--receipts".into(),
        dir.join(bs::RECEIPTS_FILE).display().to_string(),
        "--actions".into(),
        dir.join(bs::ACTIONS_FILE).display().to_string(),
        "--width".into(),
        opts.width.to_string(),
        "--height".into(),
        opts.height.to_string(),
    ];
    argv.extend(net_args(opts));
    if opts.script {
        argv.push("--script".into());
    }

    let log = std::fs::File::create(dir.join("engine.log"))?;
    let mut command = Command::new(engine_binary()?);
    command
        .args(&argv)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    detach(&mut command);

    let mut child = command
        .spawn()
        .map_err(|e| anyhow::anyhow!("could not start the browser engine ({e})"))?;
    let pid = child.id();

    Ok(Spawned {
        channel,
        alive: Box::new(move || match child.try_wait() {
            Ok(Some(status)) => Some(format!("the browser engine exited ({status})")),
            Ok(None) => None,
            Err(e) => Some(format!("lost track of the browser engine: {e}")),
        }),
        pid: Some(pid),
        control_in_engine_view: control.clone(),
        control_on_host: Some(control),
        policy_digest: host_policy_digest(opts),
        logs: bs::Logs {
            actions: Some(dir.join(bs::ACTIONS_FILE)),
            requests: Some(dir.join(bs::RECEIPTS_FILE)),
        },
        // Nothing outside the engine is deciding anything here. That is what
        // "no containment beyond the engine" means, and the lane says so.
        boundary_enforced: false,
        stop: Box::new(move || kill(pid)),
    })
}

/// Everything about a box that has to be true before an engine is started in it,
/// checked before anything is started.
///
/// Written as a preflight rather than discovered by the 30-second start timeout
/// because all three of these failures look identical from outside — the engine
/// never advertises — and each of them has a different fix. A timeout that
/// says "did not come up" for a box that could never have run it is the kind of
/// error that costs an afternoon.
fn preflight_box(
    name: &str,
    h5i_root: &Path,
    manifest: &h5i_core::env::EnvManifest,
) -> anyhow::Result<()> {
    let policy = h5i_core::env::load_policy(h5i_root, manifest)?;

    // 1. The tier has to be able to hold a long-lived process at all. A
    //    resident browser is a service, and services are a workspace/process/
    //    microvm capability today: the supervised and container tiers cannot
    //    spawn one (h5i-sandbox's `spawn_background`, "Idea 3.5").
    let claim = policy.claim;
    let holds_a_service = matches!(
        claim,
        h5i_core::sandbox::IsolationClaim::Workspace
            | h5i_core::sandbox::IsolationClaim::Process
            | h5i_core::sandbox::IsolationClaim::Microvm
    );
    if !holds_a_service {
        anyhow::bail!(
            "box `{name}` is at isolation `{}`, which cannot hold a resident process yet, so \
             it cannot hold a browser session.\n\n  \
             The tiers that can are workspace, process and microvm. Note the standing \
             trade-off: the `browser` profile's egress allowlist needs supervised or \
             container, and those are exactly the tiers that cannot hold a service — so on \
             Linux today the only tier that does both is microvm.\n\n  \
             Run the session on this machine instead (drop `--in`), which records every \
             request the same way and claims no containment for it.",
            claim.as_str()
        );
    }

    // 2. The box's h5i has to be one that carries an engine.
    //
    //    A weaker check than the one this replaced, and a more useful one. The
    //    old check asked whether a *second binary* could be executed at all,
    //    because Landlock makes `~/.cargo/bin` readable and not executable and
    //    `command -v` could not tell. That cannot happen now. What can is a box
    //    running an older or `--no-default-features` h5i, which has no
    //    `__engine` — and the failure looks identical from outside either way:
    //    the session never advertises.
    let probe = Command::new(std::env::current_exe()?)
        .arg("box")
        .arg("run")
        .arg(name)
        .arg("--")
        .arg("sh")
        .arg("-c")
        .arg(format!(
            "{} {ENGINE_SUBCOMMAND} capabilities >/dev/null 2>&1",
            h5i_in_box()
        ))
        .output()?;
    if !probe.status.success() {
        anyhow::bail!(
            "the `{}` inside box `{name}` has no browser engine in it.\n\n  \
             The engine is part of the h5i binary, so the box needs an h5i new enough to \
             carry one, built with the `browser` feature. Check what it has:\n    \
             h5i box run {name} -- {} --version\n\n  \
             Set $H5I_IN_BOX to point at a different one inside the box.",
            h5i_in_box(),
            h5i_in_box()
        );
    }
    Ok(())
}

fn spawn_in_box(name: &str, dir: &Path, opts: &StartOptions) -> anyhow::Result<Spawned> {
    let repo = super::discover_repo("h5i browser --in")?;
    let h5i_root = h5i_core::storage::h5i_root_for_repo(&repo)?;
    let manifest = h5i_core::env::find(&h5i_root, name)?;
    preflight_box(name, &h5i_root, &manifest)?;

    // What this box actually enforces at its boundary, read from the policy it
    // was created with rather than assumed from the fact that it is a box. Box
    // creation is fail-closed on the combination — a profile that declares an
    // egress allowlist cannot be created at a tier that cannot enforce one — so
    // a declared allowlist here is an enforced one.
    let boundary_enforced = match h5i_core::env::load_policy(&h5i_root, &manifest) {
        Ok(policy) => {
            !policy.profile.net_egress.is_empty()
                || policy.profile.net_mode == h5i_core::sandbox::NetMode::Deny
        }
        Err(_) => false,
    };

    // Both views of the same file, named here rather than inherited from the
    // box's environment. The `browser` profile does set `H5I_BROWSER_STREAM_FILE`
    // and the engine would derive a control file beside it, but relying on that
    // would tie `--in` to one profile — and a session in a box is not a
    // property of the profile, it is a property of the placement.
    let files = h5i_core::env::box_tmp_file(&h5i_root, &manifest, BROWSER_SERVICE);
    let (control_in_box, control_on_host) = match &files {
        Some((in_box, on_host)) => (
            in_box.with_extension("sock"),
            Some(on_host.with_extension("sock")),
        ),
        // Image-backed: the box's /tmp is inside the image. The engine still
        // needs a path, and it is the box's own; this machine simply cannot
        // watch it.
        None => (
            PathBuf::from("/tmp")
                .join(BROWSER_SERVICE)
                .with_extension("sock"),
            None,
        ),
    };
    let in_box_base = control_in_box.with_extension("");

    // `sockaddr_un.sun_path` is 108 bytes on Linux and 104 on macOS, and a bind
    // past it fails with a message about the address family rather than about
    // the length. The path is h5i's own, so the failure is h5i's to explain, and
    // to explain now rather than at the first verb.
    const SUN_LEN: usize = 100;
    if control_in_box.as_os_str().len() > SUN_LEN {
        anyhow::bail!(
            "the control socket for a session in `{name}` would be {} bytes, and a Unix socket \
             path cannot exceed about {SUN_LEN}:\n    {}\n\n  \
             The path comes from the box's own /tmp. Create the box under a shorter directory, \
             or run the session on this machine (drop `--in`).",
            control_in_box.as_os_str().len(),
            control_in_box.display()
        );
    }

    if control_on_host.is_none() {
        // Not fatal: an image-backed tier has a `/tmp` the host cannot read, and
        // a session there is still a session. What is lost is only the ability
        // to answer "is it alive" without sending a verb, and `probe` says so
        // by declining to guess rather than by reporting a death.
        eprintln!(
            "  {}     `{name}` keeps its /tmp inside its image, so `h5i browser list` cannot \
             see whether this session is still up without sending it a verb",
            style("note").yellow()
        );
    }

    let mut argv: Vec<String> = vec![
        h5i_in_box(),
        ENGINE_SUBCOMMAND.into(),
        "serve".into(),
        opts.url.clone(),
        // A socket, not a port. Every `h5i box run` gets its own network
        // namespace, so a verb carried in later has a loopback of its own and
        // the port this session binds is not on it — the connection fails with
        // ENETUNREACH, which reads exactly like a session that is not running.
        // The box's filesystem is one filesystem across every run in it, so a
        // path is the address that survives.
        "--control-socket".into(),
        control_in_box.display().to_string(),
        "--stream-file".into(),
        in_box_base.with_extension("stream").display().to_string(),
        "--receipts".into(),
        in_box_base.with_extension("requests.jsonl").display().to_string(),
        "--actions".into(),
        in_box_base.with_extension("actions.jsonl").display().to_string(),
        "--width".into(),
        opts.width.to_string(),
        "--height".into(),
        opts.height.to_string(),
    ];
    argv.extend(net_args(opts));
    if opts.script {
        argv.push("--script".into());
    }

    // **A service, not a run.** `h5i box run` takes the box's exclusive writer
    // lock and holds it for the life of the command, so a resident engine
    // started that way locks every later verb out of its own box — the failure
    // this path was rewritten to fix. A service takes the service lock instead,
    // which is what lets a brief `box run` carry a verb in while the engine
    // keeps serving.
    let def = h5i_core::env::ServiceDef {
        command: shell_join(&argv),
        port: None,
        restart: None,
        logs: true,
    };
    let record = h5i_core::env::service_start_with_def(
        &repo,
        &h5i_root,
        &manifest,
        BROWSER_SERVICE,
        &def,
    )
    .map_err(|e| {
        let detail = e.to_string();
        // Two failures reach here and they want different next steps, so the
        // hint is chosen rather than a paragraph covering both.
        let hint = if detail.contains("services are not supported at isolation") {
            format!(
                "A resident browser is a long-lived process in the box, and `{name}` is on a \
                 tier that cannot hold one. Make the box at a tier that can:\n    \
                 h5i box --profile browser --engine h5i --isolation process --name {name}"
            )
        } else {
            format!(
                "The box needs `h5i` on its own PATH. Check with:\n    \
                 h5i box run {name} -- command -v h5i"
            )
        };
        anyhow::anyhow!("could not start the browser engine in `{name}`: {detail}\n\n  {hint}")
    })?;

    let log = PathBuf::from(record.log.clone());
    if let Ok(mut sink) = std::fs::File::create(dir.join("engine.log")) {
        use std::io::Write;
        let _ = writeln!(sink, "the engine's own log is in the box, at {}", log.display());
    }

    let alive_root = h5i_root.clone();
    let alive_manifest = manifest.clone();
    let stop_root = h5i_root.clone();
    let stop_manifest = manifest.clone();
    let stop_repo_path = repo.path().to_path_buf();

    Ok(Spawned {
        alive: Box::new(move || {
            let running = h5i_core::env::service_status(&alive_root, &alive_manifest)
                .into_iter()
                .find(|s| s.record.name == BROWSER_SERVICE)
                .map(|s| s.alive)
                .unwrap_or(false);
            (!running).then(|| "the browser engine exited inside the box".to_string())
        }),
        // The service's pid is the box's, not this machine's to signal.
        pid: None,
        // The box's own view. `serve` with no `--control-file` derives it from
        // `$H5I_BROWSER_STREAM_FILE`, which the box's environment sets.
        control_in_engine_view: control_in_box,
        control_on_host,
        channel: bs::Channel::Socket,
        policy_digest: manifest.policy_digest.clone(),
        // The box's own logs, as this machine sees them. `None` on a tier whose
        // /tmp lives in an image, and an audit then says `unavailable` rather
        // than rendering an empty list that looks like a quiet session.
        logs: bs::Logs {
            actions: files
                .as_ref()
                .map(|(_, on_host)| on_host.with_extension("actions.jsonl")),
            requests: files
                .as_ref()
                .map(|(_, on_host)| on_host.with_extension("requests.jsonl")),
        },
        boundary_enforced,
        stop: Box::new(move || {
            if let Ok(repo) = git2::Repository::open(&stop_repo_path) {
                let _ = h5i_core::env::service_stop(
                    &repo,
                    &stop_root,
                    &stop_manifest,
                    BROWSER_SERVICE,
                );
            }
        }),
    })
}

/// The service name a boxed browser session runs under.
///
/// One per box, because one box holds one resident engine: the box's
/// environment names a single stream file, and two engines writing it would be
/// two sessions the viewers could not tell apart.
const BROWSER_SERVICE: &str = "h5i-browser";

/// Quote an argv into the single shell string a service definition carries.
///
/// A service's command goes through `sh -c`, so a URL with a `&` in it, or a
/// path with a space, is a command that does something other than what was
/// asked. Single quotes with the usual escape, applied to every word rather
/// than to the ones that look dangerous.
fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|word| format!("'{}'", word.replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The hidden subcommand `h5i` becomes when it is the engine.
///
/// The engine used to be a second binary, and finding it was a problem with
/// two halves that both bit: on the host it might be an older install earlier
/// on `$PATH`, and inside a box it might sit in `~/.cargo/bin`, which Landlock
/// makes readable and **not executable** — so `command -v` found it and `exec`
/// refused it. Neither can happen now: the engine is this binary, and a box
/// that can run `h5i` at all can run it.
const ENGINE_SUBCOMMAND: &str = "__engine";

/// How to invoke h5i inside a box.
///
/// Bare `h5i`, not a path: a box's `PATH` is the thing that knows where the
/// system install is, and it is already the binary `h5i box run` executes.
///
/// `$H5I_IN_BOX` overrides it, for the two cases where the name is not enough:
/// a box whose h5i is somewhere its `PATH` does not cover, and a working copy
/// newer than the system install, which is the ordinary state of anyone
/// developing h5i itself.
fn h5i_in_box() -> String {
    std::env::var("H5I_IN_BOX").unwrap_or_else(|_| "h5i".to_string())
}

fn net_args(opts: &StartOptions) -> Vec<String> {
    let mut argv = Vec::new();
    for origin in &opts.allow {
        argv.push("--allow".to_string());
        argv.push(origin.clone());
    }
    if opts.no_loopback {
        argv.push("--no-loopback".into());
    }
    argv
}

/// The digest of what a host session was allowed to do.
///
/// A host session has no box and so no box policy; its policy *is* the
/// allowlist and the two switches it was started with. Digesting them means
/// two sessions with the same digest were allowed the same things, which is the
/// only promise the field makes anywhere.
fn host_policy_digest(opts: &StartOptions) -> String {
    use sha2::{Digest, Sha256};
    let mut allow = opts.allow.clone();
    allow.sort();
    let material = format!(
        "host\nallow={}\nloopback={}\nscript={}\n",
        allow.join(","),
        !opts.no_loopback,
        opts.script
    );
    format!("sha256:{:x}", Sha256::digest(material.as_bytes()))
}

/// Wait until the engine advertises its control file, or until it is clear it
/// never will.
fn await_control(spawned: &mut Spawned, dir: &Path) -> Result<(), String> {
    let Some(witness) = spawned.control_on_host.clone() else {
        // Nothing on this side to watch. The first verb finds out.
        return Ok(());
    };
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        if witness.exists() {
            return Ok(());
        }
        if let Some(reason) = (spawned.alive)() {
            return Err(format!(
                "{reason} before it served a page. Its own output:\n{}",
                tail_of(&dir.join("engine.log"))
            ));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the browser engine did not come up within {}s (see {})",
                START_TIMEOUT.as_secs(),
                dir.join("engine.log").display()
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The last few lines of the engine's own output, scrubbed.
///
/// Quoted into an error because the useful half of a failed start is almost
/// always one line the engine already printed — a URL the box cannot see, an
/// engine not on its `PATH` — and telling someone to go and read a file is one
/// step more than they need. Scrubbed like any other answer: this text came
/// from a process that was rendering a page.
fn tail_of(log: &Path) -> String {
    let body = std::fs::read_to_string(log).unwrap_or_default();
    let tail: Vec<&str> = body
        .lines()
        .filter(|line| !line.trim().is_empty())
        .rev()
        .take(6)
        .collect();
    tail.into_iter()
        .rev()
        .map(|line| format!("    {}", bs::scrub_text(line)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Carry a previous session's cookie jar into a new session's directory.
///
/// Deliberately narrow: cookies only, and by copy. Nothing about the old
/// session's process, port or box comes across, because none of it is still
/// true — this is an inheritance of state, not a resumption of a run.
fn seed_storage(root: &Path, from: &str, into: &Path) -> anyhow::Result<()> {
    let source = bs::dir(root, from).join("cookies.json");
    if source.exists() {
        std::fs::copy(&source, into.join("cookies.json"))?;
    }
    Ok(())
}

/// Send one verb to a session and print what came back.
///
/// The three things that happen here and nowhere else, in order:
///
/// 1. **The session must be live.** An ended one is refused with
///    [`bs::EXIT_SESSION_GONE`], never restarted. An agent that retries into a
///    silently restarted browser has lost the page it was reasoning about and
///    the record of how it lost it.
/// 2. **The control lock is checked**, before the verb leaves this process.
/// 3. **The answer is scrubbed.** Everything a session returns was composed by
///    a page.
fn verb(
    root: &Path,
    selector: Option<&str>,
    argv: Vec<String>,
    mutating: bool,
    json: bool,
) -> anyhow::Result<()> {
    let session = match bs::resolve(root, selector) {
        Ok(session) => session,
        Err(gone) => {
            eprintln!("{}", gone);
            std::process::exit(bs::EXIT_SESSION_GONE);
        }
    };
    let dir = bs::dir(root, &session.id);

    if let Some(explanation) = h5i_core::control::check(&dir, mutating).explain() {
        anyhow::bail!("{explanation}");
    }

    let is_snapshot = argv.first().map(String::as_str) == Some("snapshot");
    let mut answer = deliver(&session, &dir, argv)?;

    // A completed snapshot is what clears the stale-ref flag a human takeover
    // set. It has to happen here, after the answer came back, because the flag
    // means "the agent has not seen the page since it moved" and only a
    // delivered reading changes that. Clearing it on request rather than on
    // answer would clear it for a snapshot that failed.
    if is_snapshot && answer.get("ok").and_then(Value::as_bool) != Some(false) {
        let _ = h5i_core::control::snapshotted(&dir);
    }
    bs::scrub(&mut answer);

    // A refusal is an answer, and `--json` promised the answer — so it is
    // printed either way. What must not happen is printing it and exiting 0: a
    // script that checks the status code would read "denied by policy" as
    // success, which is the failure this whole design is arranged against.
    let refused = answer.get("ok").and_then(Value::as_bool) == Some(false);

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

/// What a session said when it refused, or a stand-in when it said nothing.
fn refusal(answer: &Value) -> String {
    answer
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("the session refused, without saying why")
        .to_string()
}

/// Carry a verb to wherever the session actually is.
fn deliver(session: &bs::Session, dir: &Path, argv: Vec<String>) -> anyhow::Result<Value> {
    let output = match &session.placement {
        bs::Placement::Host => {
            // Whatever the start recorded, not a second derivation of it: two
            // places that have to agree about an address are two places that
            // can stop agreeing.
            let control = session.control.file.clone().unwrap_or(dir.join(bs::CONTROL_FILE));
            let mut command = Command::new(engine_binary()?);
            command
                .arg(ENGINE_SUBCOMMAND)
                .arg("session")
                .args(&argv)
                .arg(session.control.channel.flag())
                .arg(&control)
                .arg("--json");
            command.output()?
        }
        bs::Placement::Box { name } => {
            // The control file **as the box sees it**, straight from the record
            // the start wrote. Deriving it here instead would be a second place
            // that has to agree with the first.
            let control = session
                .control
                .file
                .clone()
                .ok_or_else(|| anyhow::anyhow!("this session's record names no control socket"))?;
            let mut command = Command::new(std::env::current_exe()?);
            command
                .arg("box")
                .arg("run")
                .arg("--json")
                .arg(name)
                .arg("--")
                .arg(h5i_in_box())
                .arg(ENGINE_SUBCOMMAND)
                .arg("session")
                .args(&argv)
                .arg(session.control.channel.flag())
                .arg(&control)
                .arg("--json");
            command.output()?
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if !output.status.success() && stdout.trim().is_empty() {
        let stderr = bs::scrub_text(&String::from_utf8_lossy(&output.stderr));
        anyhow::bail!("the session refused the verb: {}", stderr.trim());
    }

    let parsed: Value = serde_json::from_str(stdout.trim()).map_err(|e| {
        anyhow::anyhow!(
            "could not read the session's answer ({e}): {}",
            bs::scrub_text(stdout.trim())
        )
    })?;

    // A boxed verb comes back wrapped in `h5i box run --json`'s envelope, whose
    // `output` field is the engine's own answer as the receipt recorded it.
    // Unwrapping here rather than at the call site keeps every verb's answer the
    // same shape whatever the placement, which is the promise `--in` makes.
    if session.placement.box_name().is_some() {
        let inner = parsed
            .get("output")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        return serde_json::from_str(&inner).map_err(|e| {
            anyhow::anyhow!(
                "the box ran the verb but its answer was unreadable ({e}): {}",
                bs::scrub_text(&inner)
            )
        });
    }
    Ok(parsed)
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
    let mut session = match bs::resolve(root, selector) {
        Ok(session) => session,
        Err(bs::SessionGone::Ended { id, .. }) => bs::read(root, &id)?,
        Err(gone) => anyhow::bail!("{gone}"),
    };
    let id = &session.id.clone();
    // Reading status is the moment to notice a death and write it down.
    if session.state.is_live() && !session.probe() {
        bs::end(
            root,
            &mut session,
            bs::State::Died,
            "the engine stopped answering",
        );
    }
    if json {
        let control = h5i_core::control::read(&bs::dir(root, id));
        let mut value = serde_json::to_value(&session)?;
        value["control_lock"] = serde_json::to_value(&control)?;
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
    if let Some(reason) = &session.end_reason {
        println!("  ended    : {} — {}", session.state.as_str(), reason);
    }
    Ok(())
}

fn print_summary(session: &bs::Session) {
    println!("  url      : {}", session.url);
    match (&session.placement, &session.enclosing_box) {
        (bs::Placement::Box { name }, _) => {
            println!("  placed   : in box {}", style(name).cyan())
        }
        (bs::Placement::Host, Some(id)) => println!(
            "  placed   : {}, which is box {} (its policy is not readable from in here)",
            style("this machine").dim(),
            style(id).cyan()
        ),
        (bs::Placement::Host, None) => println!(
            "  placed   : {} (no containment beyond the engine)",
            style("this machine").dim()
        ),
    }
    // The honest half of the product, printed every time rather than claimed
    // once in a README: what this session's network record actually is.
    let lane = match session.lane {
        bs::Lane::EngineClaimed => style("engine-claimed").yellow(),
        bs::Lane::HostObserved => style("host-observed").green(),
    };
    println!(
        "  requests : {} ({})",
        lane,
        match session.lane {
            bs::Lane::EngineClaimed =>
                "fail-closed, and the engine's own account of what it fetched",
            bs::Lane::HostObserved => "also seen at the box's boundary, outside the engine",
        }
    );
    println!("  policy   : {}", session.policy_digest);
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
    json: bool,
) -> anyhow::Result<()> {
    let targets: Vec<bs::Session> = if all {
        bs::list(root)?
            .into_iter()
            .filter(|s| s.state.is_live())
            .collect()
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
        }
        return Ok(());
    }

    let mut closed = Vec::new();
    for mut session in targets {
        if session.state.is_live() {
            stop_engine(&session)?;
            bs::end(root, &mut session, bs::State::Closed, "closed by the user");
        }
        if !json {
            println!(
                "{} browser session {} {}. Its record stays at {}.",
                SUCCESS,
                label(&session),
                session.state.describe(),
                bs::dir(root, &session.id).display()
            );
        }
        closed.push(session);
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&closed)?);
    }
    Ok(())
}

/// The whole record of one session, merged and ordered.
///
/// Reads the record rather than requiring a live session: the session a
/// reviewer most wants to audit is usually the one that has already ended.
fn audit(root: &Path, selector: Option<&str>, json: bool) -> anyhow::Result<()> {
    let session = match bs::resolve(root, selector) {
        Ok(session) => session,
        Err(bs::SessionGone::Ended { id, .. }) => bs::read(root, &id)?,
        Err(gone) => anyhow::bail!("{gone}"),
    };
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
    println!(
        "  sources  : actions {} · requests {} · control {}",
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
    // Said once rather than on every row: the engine's stamps are its own
    // claim about its own clock, which nothing outside the box can check.
    println!(
        "  {}     engine rows are ordered by the engine's own clock, which h5i cannot verify",
        style("note").dim()
    );
    println!();

    for event in &audit.events {
        // The lane on every row, because the two are not the same kind of
        // claim: `host` is what h5i saw from outside, `engine` is the engine's
        // own account of itself.
        let lane = match event.lane {
            h5i_core::browser_events::Lane::HostObserved => style("host  ").green(),
            _ => style("engine").yellow(),
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
    }
}

/// End the process behind a session, wherever it is.
///
/// The host path signals the engine directly. The boxed path goes through
/// `service_stop`, which is what ingests the engine's in-box log as a capture
/// and writes the stop into the box's event log — so closing a boxed session
/// leaves evidence in the box's own record, not only in the session's.
fn stop_engine(session: &bs::Session) -> anyhow::Result<()> {
    match &session.placement {
        bs::Placement::Host => {
            if let Some(pid) = session.control.pid {
                kill(pid);
            }
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

/// Render an engine answer for a person.
///
/// Known shapes get a plain rendering; anything else falls back to the JSON,
/// which is a shape an agent can still use. Nothing here interprets the values
/// — they came from a page, and [`bs::scrub`] has already run.
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

/// The engine is this binary. There is nothing to find.
fn engine_binary() -> anyhow::Result<PathBuf> {
    std::env::current_exe().map_err(|e| {
        anyhow::anyhow!("could not find this executable, so the browser engine cannot start: {e}")
    })
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

#[cfg(unix)]
fn kill(pid: u32) {
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
}

#[cfg(not(unix))]
fn kill(_pid: u32) {}
