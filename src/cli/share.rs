//! `h5i box share` and `h5i join`. The CLI over `h5i-share`.
//!
//! This module owns every byte the user sees. The library decides policy and
//! moves bytes; the wording of a ticket, a warning and a refusal lives here,
//! next to the rest of the CLI's voice.

use std::time::Duration;

use clap::Subcommand;
use h5i_share::session::Transport;

use h5i_core::ui::{SUCCESS, WARN};

/// Verbs under `h5i box share`.
#[derive(Subcommand)]
pub enum ShareCommands {
    /// Show a share: its endpoint, its grants, and how long each has left.
    Status {
        name: String,
        /// Emit the share record as JSON instead of the human view
        #[arg(long)]
        json: bool,
    },
    /// List this clone's share records, live ones and any left by a crash.
    #[command(visible_alias = "ls")]
    List {
        #[arg(long)]
        json: bool,
    },
    /// Mint another ticket for a running share, so a second person can join.
    ///
    /// `--tunnel` shares only. A peer-to-peer ticket needs the running
    /// endpoint's addressing, which lives in the serving process, so adding
    /// somebody to one means stopping it and starting again.
    Grant {
        name: String,
        /// A name for this peer, to make `status` and the receipt readable
        #[arg(long)]
        label: Option<String>,
        /// How long this ticket lasts: `30m`, `2h`, `90s`, or bare seconds. 24h max.
        #[arg(long, default_value = "60m")]
        expire: String,
    },
    /// Revoke one peer's ticket. The share keeps serving everyone else, and any
    /// connection that peer already has is dropped within a second.
    Revoke { name: String, grant: String },
    /// Stop a share running in another terminal.
    ///
    /// Revokes every grant rather than killing the process, so the share writes
    /// its receipt and clears its record on the way out.
    Stop {
        name: String,
        /// Delete the share record instead of asking its process to stop.
        ///
        /// For a record whose pid has been taken over by an unrelated process:
        /// h5i then believes the share is serving forever, and no other verb
        /// gets out of that. No receipt is written, because the process that
        /// would have written one is gone.
        #[arg(long)]
        force: bool,
    },
}

/// `h5i box share <name>` starts one; with a verb it is the management surface.
/// Same shape as `h5i box` itself, for the same reason: the common case is the
/// bare form, and clap resolves the verb first.
#[derive(clap::Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct ShareArgs {
    #[command(subcommand)]
    pub action: Option<ShareCommands>,

    /// The box whose dev server you want someone else to try.
    #[arg(value_name = "NAME")]
    pub name: Option<String>,

    /// The port inside the box. Whatever the dev server in there binds.
    ///
    /// Not `0`: that is not a port a server can be listening on, and the same
    /// spelling means "pick a free one" on `h5i join`, which is the opposite.
    #[arg(long, default_value_t = 3000)]
    pub port: u16,

    /// How long the ticket lasts: `30m`, `2h`, `90s`, or bare seconds. 24h max.
    #[arg(long, default_value = "60m")]
    pub expire: String,

    /// A name for the peer you are sending this to, for `status` and the
    /// receipt.
    #[arg(long)]
    pub label: Option<String>,

    /// Publish through a Cloudflare quick tunnel instead of peer to peer, so
    /// the other side needs no h5i. Just a browser.
    ///
    /// The trade is real and is recorded in the receipt: Cloudflare terminates
    /// TLS, so this path is not end-to-end encrypted.
    #[arg(long, conflicts_with = "direct_only")]
    pub tunnel: bool,

    /// Refuse to move any application byte over a relay. If a direct
    /// peer-to-peer path cannot be established, the share fails instead.
    #[arg(long)]
    pub direct_only: bool,
}

/// `30m`, `2h`, `90s`, or a bare number of seconds.
fn parse_expire(s: &str) -> anyhow::Result<Duration> {
    let s = s.trim();
    let (value, unit) = match s.chars().last() {
        Some('s') => (&s[..s.len() - 1], 1),
        Some('m') => (&s[..s.len() - 1], 60),
        Some('h') => (&s[..s.len() - 1], 3600),
        Some(c) if c.is_ascii_digit() => (s, 1),
        _ => anyhow::bail!("could not read `{s}` as a duration — try `30m`, `2h` or `90s`"),
    };
    let n: u64 = value
        .parse()
        .map_err(|_| anyhow::anyhow!("could not read `{s}` as a duration — try `30m` or `2h`"))?;
    if n == 0 {
        anyhow::bail!("a share that expires immediately would admit nobody");
    }
    // A day is already a long time to leave a door into a running box open, and
    // the ticket can always be re-minted. Refusing here is cheaper than
    // explaining a share somebody forgot about a week ago.
    //
    // Checked rather than wrapping: `--expire 99999999999999999999h` would
    // otherwise panic in a debug build and, in a release one, wrap into a value
    // small enough to pass the ceiling below.
    let d = n
        .checked_mul(unit)
        .map(Duration::from_secs)
        .ok_or_else(|| anyhow::anyhow!("`{s}` is not a length of time — the longest is 24h"))?;
    if d > Duration::from_secs(24 * 3600) {
        anyhow::bail!("the longest a share may last is 24h — mint a fresh ticket instead");
    }
    Ok(d)
}

fn root() -> anyhow::Result<(git2::Repository, std::path::PathBuf)> {
    let repo = super::discover_repo("h5i box share")?;
    let root = h5i_core::storage::h5i_root_for_repo(&repo)?;
    Ok((repo, root))
}

pub fn run(args: ShareArgs) -> anyhow::Result<()> {
    let (_repo, h5i_root) = root()?;

    match args.action {
        Some(ShareCommands::List { json }) => {
            let rows: Vec<_> = h5i_core::env::list(&h5i_root)
                .into_iter()
                .filter_map(|m| {
                    let dir = h5i_core::env::env_dir(&h5i_root, &m.agent, &m.slug);
                    h5i_share::session::read(&dir).map(|s| (m, s))
                })
                .collect();
            if json {
                // With the box's name and whether anything is actually serving.
                // The record alone carries neither: a consumer could not tell a
                // running share from one left by a crash, the human view says
                // "GONE" for that and the JSON said nothing, and could not map
                // a row back to a name to act on it.
                let out: Vec<_> = rows.iter().map(|(m, s)| envelope(&m.slug, s)).collect();
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else if rows.is_empty() {
                println!("No box on this clone is being shared.");
            } else {
                let now = chrono::Utc::now().timestamp();
                for (_, s) in &rows {
                    print!("{}", h5i_share::bridge::render_status(s, now));
                }
            }
            Ok(())
        }

        Some(ShareCommands::Status { name, json }) => {
            let dir = box_dir(&h5i_root, &name)?;
            let Some(s) = h5i_share::session::read(&dir) else {
                anyhow::bail!(
                    "`{name}` is not being shared. Start one with `h5i box share {name}`."
                );
            };
            if json {
                // The same envelope `ls --json` uses. This printed the bare
                // record, so the one consumer that asks about a single box got
                // strictly less than the one that lists them all: no name to
                // act on, and no way to tell a serving share from a record a
                // crashed process left behind, which is the exact gap `ls`
                // had fixed for it two rounds earlier.
                println!("{}", serde_json::to_string_pretty(&envelope(&name, &s))?);
            } else {
                print!(
                    "{}",
                    h5i_share::bridge::render_status(&s, chrono::Utc::now().timestamp())
                );
            }
            Ok(())
        }

        Some(ShareCommands::Grant {
            name,
            label,
            expire,
        }) => {
            let dir = box_dir(&h5i_root, &name)?;
            not_shared(&dir, &name)?;
            let minted = h5i_share::run::grant(&dir, label, parse_expire(&expire)?)?;
            let id = minted.id;
            println!("{} minted grant {id} for {name}", SUCCESS);
            println!();
            println!("   send them  {}", minted.invite);
            println!();
            // The announce block carries an expiry and this did not, so a
            // second link went out with nobody having seen how long it lives.
            //
            // Derived from the grant that was actually written, not from
            // `--expire` re-parsed against a fresh clock. The write waits for
            // the share lock, so the two disagreed by however long that took.
            println!(
                "   expires   in {} (grant {id})",
                h5i_share::session::humanise(minted.expires_at - chrono::Utc::now().timestamp())
            );
            println!("   revoke    h5i box share revoke {name} {id}");
            Ok(())
        }

        Some(ShareCommands::Revoke { name, grant }) => {
            let dir = box_dir(&h5i_root, &name)?;
            not_shared(&dir, &name)?;
            h5i_share::run::revoke(&dir, &grant)?;
            println!("{} revoked grant {grant} on {name}", SUCCESS);
            println!("   Any connection that peer had is dropped within a second.");
            // Revoking the last live grant ends the share, because a share that
            // admits nobody is over. That is right and it is not obvious, and
            // finding out by way of "this box is not being shared" when you go
            // to mint a replacement is a bad way to learn it.
            let now = chrono::Utc::now().timestamp();
            if h5i_share::session::read(&dir)
                .map(|s| s.live_grants(now) == 0)
                .unwrap_or(true)
            {
                println!(
                    "   {} that was the last grant, so the share itself is over. Start a new one \
                     with `h5i box share {name}`.",
                    WARN
                );
            }
            Ok(())
        }

        Some(ShareCommands::Stop { name, force }) => {
            let dir = box_dir(&h5i_root, &name)?;
            if force {
                match h5i_share::session::forget(&dir)? {
                    h5i_share::session::Forgotten::Deleted => {
                        println!("{} deleted the share record for {name}", SUCCESS);
                        println!(
                            "   Nothing was asked to stop. A process that really was serving it \
                             notices within a second and then exits, writing its receipt on the \
                             way — so use this only when you believe nothing is."
                        );
                    }
                    h5i_share::session::Forgotten::NothingThere => {
                        println!(
                            "{} `{name}` had no share record — nothing to delete.",
                            SUCCESS
                        );
                    }
                    // The delete worked and the record was back before this
                    // line ran. Saying "deleted" there would have been a
                    // sentence about a file, read as a sentence about access.
                    h5i_share::session::Forgotten::Reappeared => {
                        println!(
                            "{} deleted the share record for {name}, and something wrote it \
                             straight back.",
                            WARN
                        );
                        println!(
                            "   That means a process really is serving this box: it rewrites the \
                             record as it runs. Access is NOT cut off. Stop it in its own \
                             terminal, or `h5i box share stop {name}` without `--force`."
                        );
                    }
                }
                return Ok(());
            }
            if h5i_share::session::read(&dir).is_none() {
                // Asking to stop something that is not running is not an error
                // worth failing on, and `update`'s "run `h5i box share` first"
                // is advice for a different verb entirely.
                println!(
                    "{} `{name}` is not being shared — nothing to stop.",
                    SUCCESS
                );
                return Ok(());
            }
            match h5i_share::run::stop(&dir)? {
                h5i_share::run::Stopped::Serving => {
                    println!("{} stopping the share on {name}", SUCCESS);
                    // "Within a second" was the time it takes to *notice*
                    // (`STOP_POLL`). Then comes the grace, the transport's
                    // close and up to five seconds of waiting for connections
                    // still mid-copy, so somebody who watched for a second and
                    // saw it still listed concluded the stop had not worked.
                    println!(
                        "   It notices within a second, then writes its receipt and exits — \
                         up to about six seconds if connections are still finishing."
                    );
                    println!(
                        "   If it is still listed a minute from now, nothing was really there: \
                         `h5i box share stop {name} --force` deletes the record."
                    );
                }
                h5i_share::run::Stopped::Starting => {
                    println!("{} stopping the share on {name} before it opened", SUCCESS);
                    println!(
                        "   It had claimed the box and was still setting up its transport, so \
                         nothing was ever announced and there is no receipt to write."
                    );
                }
                h5i_share::run::Stopped::Stale => {
                    println!("{} cleared a leftover share record on {name}", SUCCESS);
                    println!(
                        "   The process that owned it was already gone, so nothing was serving."
                    );
                }
            }
            Ok(())
        }

        None => start(&h5i_root, args),
    }
}

/// Refuse early, with the box's actual name in it. The library's own message
/// has to say `<name>`, because it does not know one.
fn not_shared(dir: &std::path::Path, name: &str) -> anyhow::Result<()> {
    if h5i_share::session::read(dir).is_none() {
        anyhow::bail!("`{name}` is not being shared. Start one with `h5i box share {name}`.");
    }
    Ok(())
}

fn box_dir(h5i_root: &std::path::Path, name: &str) -> anyhow::Result<std::path::PathBuf> {
    let m = h5i_core::env::find(h5i_root, name)?;
    Ok(h5i_core::env::env_dir(h5i_root, &m.agent, &m.slug))
}

/// The box must have a network namespace of its own, and a live process to
/// borrow it from. Without one, "the box's port 3000" and "this machine's port
/// 3000" are the same port, and a share would publish whatever happened to be
/// listening on the host.
///
/// The condition is deliberately "does this box have a netns of its own", not a
/// list of tiers. A `process`-tier box gets one when its profile denies egress
/// and shares the host's when it does not, so naming tiers here would be advice
/// that is wrong half the time.
///
/// One function per platform rather than one function with three `cfg` blocks
/// inside it. The unsupported branch used to be written as `let box_pid: u32 =
/// anyhow::bail!(…)`, and `bail!` expands to a `return`: on any other target
/// that binding diverged, which made every binding above it unused and every
/// statement below it unreachable, four hard errors under `-D warnings`.
#[cfg(target_os = "linux")]
fn box_process(
    dir: &std::path::Path,
    name: &str,
    m: &h5i_core::env::EnvManifest,
    port: u16,
) -> anyhow::Result<u32> {
    // A runner box has no process on this machine at all, so the generic
    // message below would tell the operator to start a session with a command
    // that refuses for the same reason. A circle rather than an answer.
    if h5i_core::env::is_remote(m) {
        anyhow::bail!(
            "`{name}` runs on `{}`, so there is no process here whose network h5i could \
             share. Sharing a port out of a runner box needs the WAN transport, which is a \
             later milestone; a box created on this machine can be shared today.",
            m.runner.as_deref().unwrap_or("a runner")
        );
    }
    let Some(pid) = h5i_core::view::box_pid(dir) else {
        // A *writer*, which is the same filter `box_pid` applies. Asking whether
        // the registry is non-empty counts `box shell --readonly` observers too, so
        // with only an observer alive this branch told the operator their box
        // shares the host's network and that they needed a different tier or
        // profile. Neither was true: `box_pid` rejected it because there is no
        // writer session, and a process- or supervised-tier observer can have a
        // namespace of its own.
        let running = h5i_core::env::live_sessions(dir)
            .iter()
            .any(|s| h5i_core::env::live_is_writer(&s.kind));
        anyhow::bail!(
            "h5i cannot find a process of `{name}` in a network namespace of its own, so it \
             cannot tell the box's port {} from any other port on this machine — and sharing \
             the wrong one would publish something you did not choose.\n   {}",
            port,
            if running {
                format!(
                    "The box is running, but at the `{}` tier with this profile it shares the \
                     host's network. A box only gets a network of its own at `supervised` or \
                     `container`, or at `process` with a profile that denies egress.",
                    m.isolation_claim
                )
            } else {
                format!("Start a session first: `h5i box shell {name}`.")
            }
        );
    };
    Ok(pid)
}

#[cfg(target_os = "macos")]
fn box_process(
    dir: &std::path::Path,
    name: &str,
    m: &h5i_core::env::EnvManifest,
    port: u16,
) -> anyhow::Result<u32> {
    // macOS identifies a box by its *process tree*, not by a namespace, and
    // the safety that a namespace gives Linux for free is established here by
    // attribution instead: `h5i_share::owner` asks Darwin which process holds
    // the listening socket and refuses unless it is one of this box's. The
    // dialer does that on every connection, so what this function needs is only
    // the root of the tree. The session process itself.
    let pid = {
        // A box inside a VM has no host process holding its port, so attribution
        // would find nothing and report "nothing is listening", which is both
        // untrue and unactionable.
        //
        // Keyed on the resolved claim rather than on probing for a VM: these are
        // exactly the tiers whose boxes do not run as host processes.
        if matches!(
            m.isolation_claim.as_str(),
            "container" | "hardened-container" | "microvm"
        ) {
            anyhow::bail!(
                "`{name}` runs at the `{}` tier, which on macOS means it runs inside a virtual \
                 machine — a Podman machine or a microVM guest — and its port {} lives in that \
                 machine's network, not on this one.\n   h5i shares a macOS box by identifying \
                 the process that holds the port, and there is no such process here to \
                 identify. Sharing it would need a route through the VM that h5i does not \
                 have.\n   A box at the `workspace`, `process` or `supervised` tier runs as \
                 processes on this Mac and can be shared.",
                m.isolation_claim,
                port
            );
        }
        // Verified for the same reason `box_pid` verifies on Linux: this pid
        // is the root of the tree the dialer attributes a listening socket to,
        // and a pid a crashed session left behind can belong to anything.
        match h5i_core::view::session_pid_verified(dir, true) {
            Some(pid) => pid,
            None => anyhow::bail!(
                "`{name}` has no session running, so h5i has no box to attribute port {} to. \
                 A box is only a set of processes while a session is running.\n   Start one \
                 first: `h5i box shell {name}`.",
                port
            ),
        }
    };
    Ok(pid)
}

/// Neither a namespace to enter nor a process tree to attribute a socket to.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn box_process(
    _dir: &std::path::Path,
    _name: &str,
    _m: &h5i_core::env::EnvManifest,
    _port: u16,
) -> anyhow::Result<u32> {
    // `Err(..)` rather than `bail!`, because this is the one branch that cannot
    // be compiled on the machine it was written on: an explicit return value
    // needs no reasoning about what the macro expands to in tail position.
    Err(anyhow::anyhow!(
        "`h5i box share` needs either a network namespace to enter (Linux) or a way to \
         attribute a listening socket to the box's processes (macOS). It is not available on \
         this platform."
    ))
}

fn start(h5i_root: &std::path::Path, args: ShareArgs) -> anyhow::Result<()> {
    let Some(name) = args.name.clone() else {
        anyhow::bail!("which box? `h5i box share <name>` — `h5i box ls` lists them");
    };
    // Before the lookup: a mistyped duration is a mistyped duration whether or
    // not the box exists, and hearing about the box first sends people looking
    // in the wrong place.
    let expire = parse_expire(&args.expire)?;
    if args.port == 0 {
        // Refused at the front, beside `--expire 0`. It used to mint a
        // complete ticket and then warn that nothing was listening on port
        // zero, which nothing ever will be, and `h5i join --port 0` means
        // "pick a free one", so the same spelling meant opposite things on the
        // two sides.
        anyhow::bail!(
            "`--port 0` is not a port a dev server can be listening on. Name the port your \
             server binds inside the box (often 3000, 5173 or 8080)."
        );
    }
    let m = h5i_core::env::find(h5i_root, &name)?;
    let dir = h5i_core::env::env_dir(h5i_root, &m.agent, &m.slug);

    let box_pid = box_process(&dir, &name, &m, args.port)?;

    let transport = if args.tunnel {
        Transport::Tunnel
    } else {
        Transport::P2p
    };

    h5i_share::run::serve(
        h5i_share::run::Request {
            env_dir: dir,
            env_id: m.id.clone(),
            policy_digest: m.policy_digest.clone(),
            box_name: name.clone(),
            box_pid,
            port: args.port,
            expire,
            label: args.label.clone(),
            transport,
            direct_only: args.direct_only,
        },
        |started| announce(&name, &args, started),
    )?;
    Ok(())
}

/// Print a line, and do not die if nobody is reading.
///
/// `println!` panics on `EPIPE` because Rust ignores `SIGPIPE`, so
/// `h5i box share demo | head -1` unwound out of the serving function between
/// claiming the share and serving it: no receipt, `cloudflared` killed only by
/// its drop guard, and `share.json` left on disk claiming a share that never
/// started.
macro_rules! say {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout(), $($arg)*);
    }};
}

fn announce(name: &str, args: &ShareArgs, s: &h5i_share::run::Started) {
    let left = h5i_share::session::humanise(s.expires_at - chrono::Utc::now().timestamp());
    say!("{} sharing port {} of {name}", SUCCESS, args.port);
    say!("");
    say!("   {}", s.invite);
    say!("");
    say!("   they      {}", s.how);
    say!("   expires   in {left} (grant {})", s.grant_id);
    match s.transport {
        Transport::P2p => {
            if args.direct_only {
                say!(
                    "   relay     refused — a peer that cannot get a direct path is turned \
                     away rather than relayed, and one that loses it is cut off"
                );
            } else {
                say!(
                    "   relay     used only if a direct path cannot be made; it moves sealed \
                     packets and cannot read them"
                );
            }
        }
        Transport::Tunnel => {
            say!(
                "   {} Cloudflare terminates TLS on this path, so it is not end-to-end \
                 encrypted. That is recorded in the box's receipt.",
                WARN
            );
        }
    }
    say!("   revoke    h5i box share revoke {name} {}", s.grant_id);
    say!("   stop      Ctrl-C, or `h5i box share stop {name}`");
    if let Some(w) = &s.warning {
        say!("");
        say!("   {WARN} {w}");
    }
}

// ─── the other machine ──────────────────────────────────────────────────────

/// `h5i join <ticket>`, and one JSON shape for every command that hands back a
/// share record.
///
/// `live` is "a process holds this record" and `admitting` is "somebody could
/// still get in", which are different questions with different answers: a share
/// winding up, or one whose grants are all revoked or expired, is live and
/// admits nobody. Everything below `h5i-share` already distinguishes them, `box
/// rm` and `box apply` ask the second, and a consumer of this JSON could only
/// ask the first.
fn envelope(name: &str, s: &h5i_share::session::ShareSession) -> serde_json::Value {
    let now = chrono::Utc::now().timestamp();
    let live = h5i_share::session::is_live(s);
    serde_json::json!({
        "name": name,
        "live": live,
        "admitting": live && !s.winding_up && !s.is_spent(now),
        "winding_up": s.winding_up,
        "live_grants": s.live_grants(now),
        "share": s,
    })
}

/// The ticket, from the argument or from stdin.
///
/// `-` is not a convenience. A ticket is the entire authorization, this process
/// runs for the length of the session, and `/proc/<pid>/cmdline` is
/// world-readable on an ordinary Linux box, so `h5i join h5i1_…` hands every
/// other user on the machine a working invite for as long as you are joined.
/// Reading it from a pipe keeps it out of shell history too.
#[cfg(feature = "share")]
fn ticket_text(arg: &str) -> anyhow::Result<String> {
    if arg != "-" {
        return Ok(arg.to_string());
    }
    let mut buf = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
    let ticket = buf.trim().to_string();
    if ticket.is_empty() {
        anyhow::bail!(
            "nothing arrived on stdin. `h5i join -` reads the ticket from a pipe: \
             `pbpaste | h5i join -`, or paste it and press Ctrl-D."
        );
    }
    Ok(ticket)
}

/// Gated on `share`, not on `share-tunnel`: joining is dialling a ticket, and
/// a ticket is a peer-to-peer thing. A tunnel-only build has `box share
/// --tunnel` and no `join`, which is the correct shape. The person on the
/// other end of a tunnel opens a link in a browser and needs no h5i at all.
#[cfg(feature = "share")]
/// Whether this process runs inside WSL, where Windows forwards only
/// `127.0.0.1` into the VM: any other loopback address binds fine here and is
/// then unreachable from a Windows browser, with nothing failing on this
/// side to say so.
fn under_wsl() -> bool {
    std::env::var_os("WSL_DISTRO_NAME").is_some()
        || std::env::var_os("WSL_INTEROP").is_some()
        || std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .map(|s| s.to_ascii_lowercase().contains("microsoft"))
            .unwrap_or(false)
}

pub fn join(
    ticket: &str,
    port: u16,
    bind: Option<std::net::Ipv4Addr>,
    shared_jar: bool,
) -> anyhow::Result<()> {
    let ticket = h5i_share::ticket::Ticket::decode(&ticket_text(ticket)?)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let ending = runtime.block_on(h5i_share::join::run(ticket, port, bind, shared_jar, |joined| {
        // Sanitised, because this string came out of a ticket somebody pasted.
        // `ticket::decode` validates the version, the base64, the JSON shape
        // and the secret's width, and nothing about `box_id`, so a `\r` or an
        // `\x1b[1A` in it can erase or forge the lines below, one of which is
        // the warning telling this person they are about to run somebody else's
        // agent's code on their loopback. They are the one who did not choose
        // to take that risk.
        println!(
            "{} joined {}",
            SUCCESS,
            h5i_core::redact::sanitize_display(&joined.box_id)
        );
        println!();
        println!("   {}", joined.url);
        println!();
        // `None` is not "relayed": a connection has no selected path for a
        // moment after it is established, and saying "relayed" there would be
        // telling someone a third party is on the wire when none is.
        match joined.path {
            Some(h5i_share::bridge::Path::Direct) => println!(
                "   path      direct — straight to the other machine, end-to-end encrypted"
            ),
            Some(h5i_share::bridge::Path::Relayed) => println!(
                "   path      relayed — through a relay, still end-to-end encrypted (it cannot \
                 read the traffic)"
            ),
            Some(h5i_share::bridge::Path::Tunnel) => println!("   path      through a tunnel"),
            None => println!("   path      settling — end-to-end encrypted either way"),
        }
        // Said here, at the moment it bites, because nothing on this side
        // fails: the bind succeeds, the P2P path comes up, and the URL is
        // simply dead in a Windows browser. `path: direct` above makes it look
        // healthy, which is exactly why the person staring at a blank tab
        // needs the one fact this machine has and their browser does not.
        if !joined.shared_jar && under_wsl() {
            println!(
                "   {} this is WSL: Windows forwards only 127.0.0.1 into it, so a Windows \
                 browser cannot reach this address. If the page will not load there, stop \
                 this join (Ctrl-C) and rerun with `{} 127.0.0.1`.",
                WARN,
                h5i_share::join::BIND_FLAG
            );
        }
        // Said plainly, because the person joining is the one taking this risk
        // and they are not the one who chose to.
        println!(
            "   {} the page you are about to open is somebody else's agent's code, served on \
             your own loopback — which browsers trust more than a website does.",
            WARN
        );
        println!(
            // "this address and port" is right for an *origin* and wrong for
            // the thing people care about: cookies are scoped by host and
            // ignore the port, so they are shared more widely than storage and
            // permissions are. Said as one sentence it read as a single scope,
            // which is the scope the next paragraph then has to contradict.
            "             What it leaves behind outlives this share: storage and permissions \
             belong to whatever you next run on this address *and port*, and cookies are \
             wider still — they ignore the port, so they are shared with everything on this \
             address."
        );
        // The cookie jar is per *host*, and this join has a host of its own,
        // unless the bind for one failed, which is the case worth a sentence.
        if joined.shared_jar {
            println!(
                "             {} You asked for `127.0.0.1`, which shares one cookie jar with \
                 every local service you have. Cookies ignore the port, so this page's \
                 credential is sent to any local service you visit while you are joined, and \
                 it is enough to reach the box. Prefer a private window with nothing else \
                 open in it.",
                WARN
            );
            // The half that *is* handled, and the half that is not. Saying only the
            // first read as "your local credentials are safe", which is a claim about
            // the proxy being made about the situation: this front forwards only what
            // the box set, and the page it serves is on `127.0.0.1` where cookies
            // ignore the port, so script on it can read any non-HttpOnly cookie in that
            // jar and send it to the box itself. The filter stops the proxy handing them
            // over; nothing stops the page.
            println!(
                "             This proxy forwards only cookies the box itself set, so nothing \
                 of yours travels there automatically — but the page runs on 127.0.0.1 and \
                 cookies ignore the port, so script on it can read any non-HttpOnly cookie in \
                 that jar and send it wherever it likes."
            );
            println!(
                "             The same rule is why the app may behave differently here: \
                 anything it stored from JavaScript is not sent back to it either."
            );
        }
        println!(
            "             Use a private window if you would rather none of that stuck, and \
             close it when you are done looking."
        );
        println!("   stop      Ctrl-C");
        // Restored. This block was lost when the closure was restructured, and
        // nothing printed `Joined.warning` for a round, so a joiner whose
        // share was full, or whose box had nothing listening, or whose route
        // into the box had broken, got a clean "joined" and a URL, then a bare
        // 502 on the first page load. The explanation was in hand and thrown
        // away, which is the thing the check exists to avoid.
        if let Some(w) = &joined.warning {
            println!();
            println!("   {WARN} {w}");
        }
    }))?;
    // A share ending is the most ordinary thing that happens to one, so it
    // leaves by the front door: a line on stdout and exit 0. It used to be
    // `Error: Metadata error: the share ended: closed by peer: h5i: this share
    // has ended (code 5)` (an internal enum name, the same fact three times
    // and a wire constant) for a revoke, an expiry, a Ctrl-C and a stopped
    // box alike.
    println!();
    println!("{} {ending}", SUCCESS);
    println!("   Ask them for a new invite if you need another look.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_json_answers_both_questions_a_consumer_has() {
        let now = chrono::Utc::now().timestamp();
        let mut s = h5i_share::session::ShareSession::new(
            "env/human/demo",
            3000,
            h5i_share::session::Transport::Tunnel,
            "https://x.example",
            chrono::Utc::now(),
        );
        let (g, _secret) = h5i_share::session::mint_grant(None, now + 600).expect("mint");
        s.grants.push(g);
        // `is_live` is `kill(pid, 0)`, so a record naming this process is the
        // only portable way to write "a live share" in a test.
        s.pid = std::process::id();

        let v = envelope("demo", &s);
        assert_eq!(v["name"], "demo");
        assert_eq!(v["live"], true);
        assert_eq!(v["admitting"], true);
        assert_eq!(v["live_grants"], 1);

        // Winding up: alive, and admitting nobody. A consumer reading only
        // `live` would have shown this as a share somebody can still reach,
        // for the whole of a teardown.
        s.winding_up = true;
        let v = envelope("demo", &s);
        assert_eq!(v["live"], true);
        assert_eq!(v["admitting"], false);
        assert_eq!(v["winding_up"], true);

        // Every grant revoked is the same answer by a different route.
        s.winding_up = false;
        for g in &mut s.grants {
            g.revoked = true;
        }
        let v = envelope("demo", &s);
        assert_eq!(v["live"], true);
        assert_eq!(v["admitting"], false);
        assert_eq!(v["live_grants"], 0);

        // And the record itself is still there for anything that wants it.
        assert_eq!(v["share"]["port"], 3000);
    }

    #[test]
    #[cfg(feature = "share")]
    fn a_ticket_can_arrive_without_going_through_the_process_table() {
        // `/proc/<pid>/cmdline` is world-readable on an ordinary Linux box
        // (mode `-r--r--r--`, no `hidepid`) and `h5i join` runs for the whole
        // session. A ticket passed as an argument is therefore a working
        // invite, legible to every other user on the machine, for as long as
        // somebody is joined. Anything but `-` is still taken literally.
        assert_eq!(ticket_text("h5i1_abc").expect("literal"), "h5i1_abc");
        // Including a ticket that merely starts with one: only the whole
        // argument means stdin.
        assert_eq!(ticket_text("-h5i1_abc").expect("literal"), "-h5i1_abc");
    }

    #[test]
    fn a_short_share_does_not_announce_itself_as_already_over() {
        // Integer minutes rendered `--expire 45` as "expires in 0m".
        assert_eq!(h5i_share::session::humanise(45), "45s");
        assert_eq!(h5i_share::session::humanise(59), "59s");
        assert_eq!(h5i_share::session::humanise(60), "1m");
        assert_eq!(h5i_share::session::humanise(3599), "59m");
        assert_eq!(h5i_share::session::humanise(3600), "1h0m");
        assert_eq!(h5i_share::session::humanise(-5), "0s");
    }

    #[test]
    fn a_duration_is_read_the_way_people_write_one() {
        assert_eq!(parse_expire("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_expire("30m").unwrap(), Duration::from_secs(1800));
        assert_eq!(parse_expire("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_expire("45").unwrap(), Duration::from_secs(45));
    }

    #[test]
    fn a_share_cannot_be_opened_forever_or_not_at_all() {
        // Both ends of the range are refusals rather than silent clamps: a
        // share is a door into a running box, and "0" and "a week" are both
        // more likely to be a mistake than an intention.
        assert!(parse_expire("0m").is_err());
        assert!(parse_expire("48h").is_err());
        // Arithmetic that would have wrapped, or panicked in a debug build.
        assert!(parse_expire("99999999999999999999h").is_err());
        assert!(parse_expire(&format!("{}h", u64::MAX)).is_err());
        assert!(parse_expire("").is_err());
        assert!(parse_expire("soon").is_err());
        assert!(parse_expire("-5m").is_err());
    }
}
