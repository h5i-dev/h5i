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
    /// List the boxes on this clone that are being shared right now.
    #[command(visible_alias = "ls")]
    List {
        #[arg(long)]
        json: bool,
    },
    /// Mint another ticket for a running share, so a second person can join.
    Grant {
        name: String,
        /// A name for this peer, to make `status` and the receipt readable
        #[arg(long)]
        label: Option<String>,
        /// How long this ticket lasts (`30m`, `2h`, `90s`)
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
    Stop { name: String },
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

    /// The port inside the box. `h5i box ports <name>` lists what is listening.
    #[arg(long, default_value_t = 3000)]
    pub port: u16,

    /// How long the ticket lasts (`30m`, `2h`, `90s`).
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
    let d = Duration::from_secs(n * unit);
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
                let out: Vec<_> = rows.iter().map(|(_, s)| s).collect();
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
            let (id, invite) = h5i_share::run::grant(&dir, label, parse_expire(&expire)?)?;
            println!("{} minted grant {id} for {name}", SUCCESS);
            println!();
            println!("   {invite}");
            println!();
            println!("   revoke   h5i box share revoke {name} {id}");
            Ok(())
        }

        Some(ShareCommands::Revoke { name, grant }) => {
            let dir = box_dir(&h5i_root, &name)?;
            h5i_share::run::revoke(&dir, &grant)?;
            println!("{} revoked grant {grant} on {name}", SUCCESS);
            println!("   Any connection that peer had is dropped within a second.");
            Ok(())
        }

        Some(ShareCommands::Stop { name }) => {
            let dir = box_dir(&h5i_root, &name)?;
            h5i_share::run::stop(&dir)?;
            println!("{} stopping the share on {name}", SUCCESS);
            println!("   The serving process writes its receipt and exits within a second.");
            Ok(())
        }

        None => start(&h5i_root, args),
    }
}

fn box_dir(h5i_root: &std::path::Path, name: &str) -> anyhow::Result<std::path::PathBuf> {
    let m = h5i_core::env::find(h5i_root, name)?;
    Ok(h5i_core::env::env_dir(h5i_root, &m.agent, &m.slug))
}

fn start(h5i_root: &std::path::Path, args: ShareArgs) -> anyhow::Result<()> {
    let Some(name) = args.name.clone() else {
        anyhow::bail!("which box? `h5i box share <name>` — `h5i box ls` lists them");
    };
    let m = h5i_core::env::find(h5i_root, &name)?;
    let dir = h5i_core::env::env_dir(h5i_root, &m.agent, &m.slug);

    // The box must have a network namespace of its own, and a live process to
    // borrow it from. Without one, "the box's port 3000" and "this machine's
    // port 3000" are the same port, and a share would publish whatever happened
    // to be listening on the host — which is the one outcome nobody would
    // forgive. So this refuses rather than guessing.
    let Some(box_pid) = h5i_core::view::box_pid(&dir) else {
        anyhow::bail!(
            "`{name}` has no network namespace h5i can enter, so it cannot tell the box's port \
             {} from any other port on this machine — and sharing the wrong one would publish \
             something you did not choose.\n   \
             The box is at the `{}` tier{}. Share from a box at `supervised` or `container`, \
             with a session running (`h5i box shell {name}`).",
            args.port,
            m.isolation_claim,
            if m.isolation_claim == "workspace" {
                ", which does not confine the network"
            } else {
                " and has no running session"
            }
        );
    };

    let transport = if args.tunnel {
        Transport::Tunnel
    } else {
        Transport::P2p
    };
    let expire = parse_expire(&args.expire)?;

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

fn announce(name: &str, args: &ShareArgs, s: &h5i_share::run::Started) {
    let mins = (s.expires_at - chrono::Utc::now().timestamp()).max(0) / 60;
    println!("{} sharing port {} of {name}", SUCCESS, args.port);
    println!();
    println!("   {}", s.invite);
    println!();
    println!("   they      {}", s.how);
    println!("   expires   in {mins}m (grant {})", s.grant_id);
    match s.transport {
        Transport::P2p => {
            if args.direct_only {
                println!(
                    "   relay     refused — if no direct path can be made, the share fails \
                     rather than relaying"
                );
            } else {
                println!(
                    "   relay     used only if a direct path cannot be made; it moves sealed \
                     packets and cannot read them"
                );
            }
        }
        Transport::Tunnel => {
            println!(
                "   {} Cloudflare terminates TLS on this path, so it is not end-to-end \
                 encrypted. That is recorded in the box's receipt.",
                WARN
            );
        }
    }
    println!("   revoke    h5i box share revoke {name} {}", s.grant_id);
    println!("   stop      Ctrl-C, or `h5i box share stop {name}`");
    if let Some(w) = &s.warning {
        println!();
        println!("   {WARN} {w}");
    }
}

// ─── the other machine ──────────────────────────────────────────────────────

/// `h5i join <ticket>`.
pub fn join(ticket: &str, port: u16) -> anyhow::Result<()> {
    let ticket = h5i_share::ticket::Ticket::decode(ticket)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(h5i_share::join::run(ticket, port, |joined| {
        println!("{} joined {}", SUCCESS, joined.box_id);
        println!();
        println!("   {}", joined.url);
        println!();
        println!(
            "   path      {} — {}",
            joined.path.as_str(),
            match joined.path {
                h5i_share::bridge::Path::Direct =>
                    "straight to the other machine, end-to-end encrypted",
                h5i_share::bridge::Path::Relayed =>
                    "through a relay, still end-to-end encrypted (it cannot read the traffic)",
                h5i_share::bridge::Path::Tunnel => "through a tunnel",
            }
        );
        // Said plainly, because the person joining is the one taking this risk
        // and they are not the one who chose to.
        println!(
            "   {} the page you are about to open is somebody else's agent's code, running on \
             your loopback. Treat it like any link a colleague sends you.",
            WARN
        );
        println!("   stop      Ctrl-C");
    }))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(parse_expire("").is_err());
        assert!(parse_expire("soon").is_err());
        assert!(parse_expire("-5m").is_err());
    }
}
