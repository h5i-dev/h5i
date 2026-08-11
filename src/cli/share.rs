//! `h5i box share` and `h5i join` — the CLI over `h5i-share`.
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

    /// The port inside the box — whatever the dev server in there binds.
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
    /// the other side needs no h5i — just a browser.
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
    let repo = git2::Repository::discover(".")?;
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
                // running share from one left by a crash — the human view says
                // "GONE" for that and the JSON said nothing — and could not map
                // a row back to a name to act on it.
                let out: Vec<_> = rows
                    .iter()
                    .map(|(m, s)| {
                        serde_json::json!({
                            "name": m.slug,
                            "live": h5i_share::session::is_live(s),
                            "share": s,
                        })
                    })
                    .collect();
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
                println!("{}", serde_json::to_string_pretty(&s)?);
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
            let (id, invite) = h5i_share::run::grant(&dir, label, parse_expire(&expire)?)?;
            println!("{} minted grant {id} for {name}", SUCCESS);
            println!();
            println!("   send them  {invite}");
            println!();
            // The announce block carries an expiry and this did not, so a
            // second link went out with nobody having seen how long it lives.
            println!(
                "   expires   in {} (grant {id})",
                h5i_share::session::humanise(
                    (chrono::Utc::now()
                        + chrono::Duration::from_std(parse_expire(&expire)?).unwrap_or_default())
                    .timestamp()
                        - chrono::Utc::now().timestamp()
                )
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
                if h5i_share::session::forget(&dir)? {
                    println!("{} deleted the share record for {name}", SUCCESS);
                    println!(
                        "   Nothing was asked to stop. A process that really was serving it \
                         notices within a second and then exits, writing its receipt on the \
                         way — so use this only when you believe nothing is."
                    );
                } else {
                    println!(
                        "{} `{name}` had no share record — nothing to delete.",
                        SUCCESS
                    );
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
                    // still mid-copy — so somebody who watched for a second and
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
        // zero, which nothing ever will be — and `h5i join --port 0` means
        // "pick a free one", so the same spelling meant opposite things on the
        // two sides.
        anyhow::bail!(
            "`--port 0` is not a port a dev server can be listening on. Name the port your \
             server binds inside the box (often 3000, 5173 or 8080)."
        );
    }
    let m = h5i_core::env::find(h5i_root, &name)?;
    let dir = h5i_core::env::env_dir(h5i_root, &m.agent, &m.slug);

    // The box must have a network namespace of its own, and a live process to
    // borrow it from. Without one, "the box's port 3000" and "this machine's
    // port 3000" are the same port, and a share would publish whatever happened
    // to be listening on the host — which is the one outcome nobody would
    // forgive. So this refuses rather than guessing.
    //
    // The condition is deliberately "does this box have a netns of its own",
    // not a list of tiers. A `process`-tier box gets one when its profile
    // denies egress and shares the host's when it does not, so naming tiers
    // here would be advice that is wrong half the time.
    let Some(box_pid) = h5i_core::view::box_pid(&dir) else {
        // Said plainly rather than through the tier advice below, which on a
        // platform with no network namespaces at all is advice nobody can
        // follow: there is no tier that satisfies it, so the message sent
        // people to try each one in turn.
        #[cfg(not(target_os = "linux"))]
        anyhow::bail!(
            "`h5i box share` is Linux-only. A share is safe because the box has a network \
             namespace of its own and the only route in enters it; on this platform a box \
             binds the host's loopback, so h5i cannot tell the box's port {} from anything \
             else listening on it.",
            args.port
        );
        #[cfg(target_os = "linux")]
        let running = !h5i_core::env::live_sessions(&dir).is_empty();
        #[cfg(target_os = "linux")]
        anyhow::bail!(
            "h5i cannot find a process of `{name}` in a network namespace of its own, so it \
             cannot tell the box's port {} from any other port on this machine — and sharing \
             the wrong one would publish something you did not choose.\n   {}",
            args.port,
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

/// `h5i join <ticket>`.
pub fn join(ticket: &str, port: u16) -> anyhow::Result<()> {
    let ticket = h5i_share::ticket::Ticket::decode(ticket)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let ending = runtime.block_on(h5i_share::join::run(ticket, port, |joined| {
        // Sanitised, because this string came out of a ticket somebody pasted.
        // `ticket::decode` validates the version, the base64, the JSON shape
        // and the secret's width, and nothing about `box_id` — so a `\r` or an
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
        // Said plainly, because the person joining is the one taking this risk
        // and they are not the one who chose to.
        println!(
            "   {} the page you are about to open is somebody else's agent's code, served on \
             your own loopback — which browsers trust more than a website does.",
            WARN
        );
        println!(
            "             It shares an origin with anything else you run on this port, so what \
             it leaves behind — cookies, cached responses, stored data, any permission you \
             grant it — outlives this share and belongs to whatever you run there next. \
             Cookies reach your other local services too, since they ignore the port."
        );
        println!(
            "             Use a private window if you would rather none of that stuck, and \
             close it when you are done looking."
        );
        println!("   stop      Ctrl-C");
    }))?;
    // A share ending is the most ordinary thing that happens to one, so it
    // leaves by the front door: a line on stdout and exit 0. It used to be
    // `Error: Metadata error: the share ended: closed by peer: h5i: this share
    // has ended (code 5)` — an internal enum name, the same fact three times
    // and a wire constant — for a revoke, an expiry, a Ctrl-C and a stopped
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
