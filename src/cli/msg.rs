//! `h5i msg` — CLI handlers (migrated from main.rs).
#![allow(clippy::all)]
use crate::*;

#[derive(Subcommand)]
pub enum MsgCommands {
    /// Send a message to another agent (or `all` to broadcast).
    ///
    /// The body is variadic, so options must appear BEFORE the recipient:
    ///   h5i msg send --from alice --tag review bob look at the auth refactor
    Send {
        /// Recipient agent name, or `all` to broadcast to everyone else.
        to: String,
        /// Message body. Multiple words are joined with spaces.
        #[arg(trailing_var_arg = true, required = true)]
        body: Vec<String>,
        /// Sender identity. Defaults to $H5I_AGENT or the stored identity;
        /// when given, it is remembered as this repo's default identity.
        /// Must be placed before the recipient (the body consumes trailing args).
        #[arg(long)]
        from: Option<String>,
        /// Optional classification, e.g. `review` or `risk` (coloured in the UI).
        #[arg(long)]
        tag: Option<String>,
        /// Git branch this message relates to (default: current branch; pass "" to leave untagged).
        #[arg(long)]
        branch: Option<String>,
    },

    /// Reply to a numbered message from your last inbox / dashboard view.
    ///   h5i msg reply 1 on it, reviewing now
    Reply {
        /// The message number shown in the most recent view.
        number: usize,
        /// Reply body. Multiple words are joined with spaces.
        #[arg(trailing_var_arg = true, required = true)]
        body: Vec<String>,
        /// Reply as this identity (defaults to your stored identity).
        #[arg(long)]
        from: Option<String>,
    },

    /// Set this repo's default agent identity (e.g. `h5i msg as codex`).
    As {
        /// The agent name to act as.
        name: String,
    },

    /// One-time wiring for Claude Code messaging: set this agent's identity
    /// (`env.H5I_AGENT`) and add the turn-delivery Stop hook to settings.json.
    /// Identity is per-agent (no `--as` needed afterward). For Codex, just
    /// launch it with `H5I_AGENT=<name>` — it doesn't read .claude/settings.json.
    Setup {
        /// Identity this Claude Code uses (written to env.H5I_AGENT).
        #[arg(default_value = "claude")]
        name: String,
        /// `project` (default) → ./.claude/settings.json; `user` → ~/.claude/settings.json (all projects).
        #[arg(long, value_enum, default_value_t = SetupScope::Project)]
        scope: SetupScope,
        /// Notify-only hook (`systemMessage`) instead of the default autonomous
        /// `--block` hook that makes the agent handle incoming messages.
        #[arg(long = "no-block")]
        no_block: bool,
    },

    /// i5h ASK: a general request that expects a response.
    ///   h5i msg ask codex can you inspect the failing auth test
    Ask {
        to: String,
        #[arg(trailing_var_arg = true, required = true)]
        body: Vec<String>,
        #[arg(long)]
        from: Option<String>,
        /// Git branch this ask relates to (default: current branch; pass "" to leave untagged).
        #[arg(long)]
        branch: Option<String>,
    },

    /// i5h REVIEW_REQUEST: ask for code/design/security review.
    ///   h5i msg review --branch auth --focus src/auth.rs --risk "expiry edges" codex review token refresh
    Review {
        to: String,
        #[arg(trailing_var_arg = true, required = true)]
        body: Vec<String>,
        #[arg(long)]
        from: Option<String>,
        /// Git branch to review (default: current branch; pass "" to leave untagged).
        #[arg(long)]
        branch: Option<String>,
        /// File/symbol/test to inspect first (repeatable).
        #[arg(long)]
        focus: Vec<String>,
        /// Concise risk statement.
        #[arg(long)]
        risk: Option<String>,
        /// Related PR number (stored under links.pr).
        #[arg(long)]
        pr: Option<u64>,
    },

    /// i5h RISK: flag a hazard the recipient should inspect.
    ///   h5i msg risk --focus src/auth.rs --priority high all auth cache crosses requests
    Risk {
        to: String,
        #[arg(trailing_var_arg = true, required = true)]
        body: Vec<String>,
        #[arg(long)]
        from: Option<String>,
        /// Git branch this risk relates to (default: current branch; pass "" to leave untagged).
        #[arg(long)]
        branch: Option<String>,
        #[arg(long)]
        focus: Vec<String>,
        /// low | normal | high | urgent.
        #[arg(long)]
        priority: Option<String>,
    },

    /// i5h HANDOFF: transfer task ownership/context to another agent.
    ///   h5i msg handoff --branch auth --context auth reviewer please take expiry work
    Handoff {
        to: String,
        #[arg(trailing_var_arg = true, required = true)]
        body: Vec<String>,
        #[arg(long)]
        from: Option<String>,
        /// Git branch being handed off (default: current branch; pass "" to leave untagged).
        #[arg(long)]
        branch: Option<String>,
        /// h5i context branch relevant to the handoff.
        #[arg(long)]
        context: Option<String>,
        #[arg(long)]
        focus: Vec<String>,
    },

    /// i5h ACK: acknowledge a numbered message (optionally with a note).
    ///   h5i msg ack 1
    Ack {
        number: usize,
        #[arg(trailing_var_arg = true)]
        body: Vec<String>,
        #[arg(long)]
        from: Option<String>,
    },

    /// i5h DONE: report a numbered request complete.
    ///   h5i msg done 1 fixed in 1a2b3c4
    Done {
        number: usize,
        #[arg(trailing_var_arg = true)]
        body: Vec<String>,
        #[arg(long)]
        from: Option<String>,
    },

    /// i5h DECLINE: decline a numbered request.
    ///   h5i msg decline 1 cannot take this now
    Decline {
        number: usize,
        #[arg(trailing_var_arg = true)]
        body: Vec<String>,
        #[arg(long)]
        from: Option<String>,
    },

    /// Show messages addressed to you that arrived since you last checked,
    /// then mark them read (advance your local cursor).
    Inbox {
        /// Whose inbox to read. Defaults to $H5I_AGENT or the stored identity.
        #[arg(long = "as")]
        as_agent: Option<String>,
        /// Show unread without advancing the cursor (don't mark as read).
        #[arg(long)]
        peek: bool,
    },

    /// Show the full message history (oldest-first within the window).
    History {
        /// Maximum number of messages to show.
        #[arg(short, long, default_value_t = 30)]
        limit: usize,
        /// Restrict to a conversation with this agent (sender or recipient).
        #[arg(long)]
        with: Option<String>,
        /// Restrict to the conversation tied to this git branch (whole threads
        /// that have at least one message tagged with the branch).
        #[arg(long)]
        branch: Option<String>,
    },

    /// Replay the conversation like a live feed — print each message in turn
    /// with a pause between them, so the thread unfolds as if it were happening
    /// now. Same selection as `history`; oldest-first.
    Replay {
        /// Maximum number of messages to replay.
        #[arg(short, long, default_value_t = 30)]
        limit: usize,
        /// Restrict to a conversation with this agent (sender or recipient).
        #[arg(long)]
        with: Option<String>,
        /// Restrict to the conversation tied to this git branch (whole threads
        /// that have at least one message tagged with the branch).
        #[arg(long)]
        branch: Option<String>,
        /// Seconds to pause between messages (fractional allowed, e.g. 0.5).
        #[arg(short, long, default_value_t = 1.0)]
        interval: f64,
    },

    /// List the known agents on this repo's message roster.
    Team,

    /// Show or set this repo's stored default agent identity.
    Whoami {
        /// If given, set the stored identity to this name.
        name: Option<String>,
    },

    /// Turn-delivery hook: print unread messages (for use as a Stop hook),
    /// then mark them read. Silent and exit 0 when there is nothing new.
    /// Default emits a `systemMessage` JSON; `--plain` emits raw text.
    Hook {
        /// Whose inbox to check. Defaults to $H5I_AGENT or the stored identity.
        #[arg(long = "as")]
        as_agent: Option<String>,
        /// Autonomous turn mode: emit `{"decision":"block","reason":…}` so the
        /// agent keeps working to handle the message instead of stopping.
        /// Honors `stop_hook_active` to avoid infinite loops.
        #[arg(long)]
        block: bool,
    },

    /// Block until a new message arrives, print it, then exit — the wake
    /// primitive for autonomous delivery. Run via run_in_background (Claude
    /// Code) or in a poll loop (Codex) so an idle agent gets woken on reply.
    /// Peeks (does not consume); the woken agent runs `inbox` to consume.
    Wait {
        /// Whose inbox to wait on. Defaults to $H5I_AGENT or the stored identity.
        #[arg(long = "as")]
        as_agent: Option<String>,
        /// Wait on the whole channel, not one inbox. Implied with no identity.
        #[arg(long)]
        all: bool,
        /// Give up after this many seconds (exit 0, no output). 0 = wait forever.
        #[arg(short, long, default_value_t = 120)]
        timeout: u64,
        /// Seconds between polls.
        #[arg(short, long, default_value_t = 3)]
        interval: u64,
    },

    /// Live watcher — stream the conversation as it happens. Ctrl+C to stop.
    ///
    /// By default this is the stable line-streaming watcher (the format the
    /// Stop hook / Monitor tool consume). Pass `--tui` on an interactive
    /// terminal to open the full-screen cinematic "Agent Radio" dashboard
    /// (roster with per-agent activity, a live transmission feed, and a Git
    /// provenance ticker); `--tui` is ignored when stdout is not a TTY or with
    /// `--plain` / `--once`. With an identity (`--as` / $H5I_AGENT) it scopes to
    /// your conversation (sent + received + broadcast); with `--all` or no
    /// identity it shows the whole channel. Always passive: it never advances
    /// any agent's read cursor.
    Watch {
        /// Whose inbox to watch. Defaults to $H5I_AGENT or the stored identity.
        #[arg(long = "as")]
        as_agent: Option<String>,
        /// Watch the whole channel (all messages), not just one inbox. Implied
        /// when no identity is set — so plain monitoring needs no identity.
        #[arg(long)]
        all: bool,
        /// Seconds between polls.
        #[arg(short, long, default_value_t = 5)]
        interval: u64,
        /// Check once and exit (don't loop) — useful for testing.
        #[arg(long)]
        once: bool,
        /// Open the full-screen Agent Radio TUI instead of the line-streaming
        /// watcher (requires a TTY; ignored with `--plain` or `--once`).
        #[arg(long)]
        tui: bool,
    },
}

pub fn run(action: Option<MsgCommands>, plain: bool) -> anyhow::Result<()> {
    {
            let repo = H5iRepository::open(".")?;
            let h5i_root = repo.h5i_root.clone();
            let git = repo.git();

            match action {
                // Bare `h5i msg` → the inbox dashboard.
                None => {
                    // Resolve env-first (like every other verb) so the dashboard
                    // shows who *this* host acts as, not whatever the shared
                    // stored slot was last set to. This is a read, so an
                    // ambiguous identity warns and renders without a name rather
                    // than erroring or impersonating another agent.
                    let me = match msg::resolve_identity(&h5i_root, None) {
                        Ok(name) => Some(name),
                        Err(e) => {
                            eprintln!("h5i: warning: {e}");
                            None
                        }
                    };
                    let branch = current_branch(&repo);
                    render_dashboard(&repo, &branch, me.as_deref(), plain)?;
                }

                Some(MsgCommands::Send {
                    to,
                    body,
                    from,
                    tag,
                    branch,
                }) => {
                    let me = msg::resolve_identity(&h5i_root, from.as_deref())?;
                    let body = non_empty_free_text_body(&body)?;
                    let opts = msg::SendOpts {
                        tag,
                        branch,
                        ..Default::default()
                    };
                    let sent = msg::send_msg(git, &h5i_root, &me, &to, &body, opts)?;
                    // Mirror to a confined recipient's per-env read-only inbox so a
                    // boxed team agent receives it (team id unknown here → match on
                    // the recipient agent). No-op if the recipient isn't boxed.
                    h5i_core::env::fan_out_to_env_inbox(&h5i_root, &to, None, &sent);
                    report_sent(&sent);
                }

                Some(MsgCommands::Ask {
                    to,
                    body,
                    from,
                    branch,
                }) => {
                    let me = msg::resolve_identity(&h5i_root, from.as_deref())?;
                    let body = non_empty_free_text_body(&body)?;
                    let opts = msg::SendOpts {
                        kind: Some("ASK".into()),
                        branch,
                        ..Default::default()
                    };
                    report_sent(&msg::send_msg(git, &h5i_root, &me, &to, &body, opts)?);
                }

                Some(MsgCommands::Review {
                    to,
                    body,
                    from,
                    branch,
                    focus,
                    risk,
                    pr,
                }) => {
                    let me = msg::resolve_identity(&h5i_root, from.as_deref())?;
                    let links = pr.map(|n| serde_json::json!({ "pr": n }));
                    let opts = msg::SendOpts {
                        kind: Some("REVIEW_REQUEST".into()),
                        branch,
                        focus,
                        risk,
                        links,
                        ..Default::default()
                    };
                    report_sent(&msg::send_msg(
                        git,
                        &h5i_root,
                        &me,
                        &to,
                        &body.join(" "),
                        opts,
                    )?);
                }

                Some(MsgCommands::Risk {
                    to,
                    body,
                    from,
                    branch,
                    focus,
                    priority,
                }) => {
                    let me = msg::resolve_identity(&h5i_root, from.as_deref())?;
                    let opts = msg::SendOpts {
                        kind: Some("RISK".into()),
                        branch,
                        focus,
                        priority,
                        ..Default::default()
                    };
                    report_sent(&msg::send_msg(
                        git,
                        &h5i_root,
                        &me,
                        &to,
                        &body.join(" "),
                        opts,
                    )?);
                }

                Some(MsgCommands::Handoff {
                    to,
                    body,
                    from,
                    branch,
                    context,
                    focus,
                }) => {
                    let me = msg::resolve_identity(&h5i_root, from.as_deref())?;
                    let opts = msg::SendOpts {
                        kind: Some("HANDOFF".into()),
                        branch,
                        context_branch: context,
                        focus,
                        ..Default::default()
                    };
                    report_sent(&msg::send_msg(
                        git,
                        &h5i_root,
                        &me,
                        &to,
                        &body.join(" "),
                        opts,
                    )?);
                }

                Some(MsgCommands::Reply { number, body, from }) => {
                    let me = msg::resolve_identity(&h5i_root, from.as_deref())?;
                    let original = reply_target(&repo, &me, number)?;
                    send_reply(&repo, &me, &original, None, body.join(" "))?;
                }

                Some(MsgCommands::Ack { number, body, from }) => {
                    let me = msg::resolve_identity(&h5i_root, from.as_deref())?;
                    let original = reply_target(&repo, &me, number)?;
                    send_reply(&repo, &me, &original, Some("ACK"), body.join(" "))?;
                }

                Some(MsgCommands::Done { number, body, from }) => {
                    let me = msg::resolve_identity(&h5i_root, from.as_deref())?;
                    let original = reply_target(&repo, &me, number)?;
                    send_reply(&repo, &me, &original, Some("DONE"), body.join(" "))?;
                }

                Some(MsgCommands::Decline { number, body, from }) => {
                    let me = msg::resolve_identity(&h5i_root, from.as_deref())?;
                    let original = reply_target(&repo, &me, number)?;
                    send_reply(&repo, &me, &original, Some("DECLINE"), body.join(" "))?;
                }

                Some(MsgCommands::As { name }) => {
                    msg::write_identity(&h5i_root, name.trim())?;
                    println!(
                        "{} You are now {} on {}.",
                        SUCCESS,
                        style(name.trim()).green().bold(),
                        style(msg::MSG_REF).magenta()
                    );
                }

                Some(MsgCommands::Setup {
                    name,
                    scope,
                    no_block,
                }) => {
                    let block = !no_block; // autonomous turn mode is the default
                    let name = name.trim();
                    let path = match scope {
                        SetupScope::User => {
                            let home = std::env::var("HOME").map_err(|_| {
                                anyhow::anyhow!("$HOME is not set — use --scope project")
                            })?;
                            PathBuf::from(home).join(".claude").join("settings.json")
                        }
                        SetupScope::Project => {
                            let workdir = git.workdir().ok_or_else(|| {
                                anyhow::anyhow!("bare repository has no working dir")
                            })?;
                            workdir.join(".claude").join("settings.json")
                        }
                    };

                    let existing = std::fs::read_to_string(&path).unwrap_or_default();
                    let merged = msg::merge_settings_json(&existing, name, block)?;
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&path, merged)?;

                    let hook_cmd = if block {
                        "h5i msg hook --block"
                    } else {
                        "h5i msg hook"
                    };
                    println!(
                        "{} Claude Code messaging configured as {} in {}",
                        SUCCESS,
                        style(name).green().bold(),
                        style(path.display()).cyan()
                    );
                    println!(
                        "   {} {}   ·   {} {}",
                        style("env H5I_AGENT=").dim(),
                        style(name).bold(),
                        style("Stop hook:").dim(),
                        style(hook_cmd).bold(),
                    );
                    println!();
                    println!(
                        "   {} open {} once (or restart) so Claude Code reloads the hook.",
                        style("→").dim(),
                        style("/hooks").bold()
                    );
                    println!(
                        "   {} for Codex, launch it with {} (it doesn't read .claude/settings.json).",
                        style("→").dim(),
                        style("H5I_AGENT=codex").bold(),
                    );
                }

                Some(MsgCommands::Inbox { as_agent, peek }) => {
                    let me = msg::resolve_identity(&h5i_root, as_agent.as_deref())?;
                    let unread = msg::inbox(git, &h5i_root, &me, !peek)?;
                    // Persist the numbered view so `reply <n>` works afterwards.
                    let ids: Vec<String> = unread.iter().map(|m| m.id.clone()).collect();
                    msg::write_last_view(&h5i_root, &me, &ids)?;
                    if unread.is_empty() {
                        if !plain {
                            println!(
                                "{} No new messages for {}.",
                                SUCCESS,
                                style(&me).green().bold()
                            );
                        }
                    } else {
                        if !plain {
                            println!(
                                "{} {} new message{} for {}{}\n",
                                STEP,
                                style(unread.len()).cyan().bold(),
                                if unread.len() == 1 { "" } else { "s" },
                                style(&me).green().bold(),
                                if peek {
                                    style(" (peek)").dim().to_string()
                                } else {
                                    String::new()
                                },
                            );
                        }
                        print_messages_numbered(&unread, &me, plain);
                    }
                }

                Some(MsgCommands::History {
                    limit,
                    with,
                    branch,
                }) => {
                    let msgs = msg::history(git, with.as_deref(), branch.as_deref(), limit)?;
                    if msgs.is_empty() {
                        if !plain {
                            println!("{} No messages yet.", WARN);
                        }
                    } else {
                        if !plain {
                            let header = match (&with, &branch) {
                                (Some(w), Some(b)) => format!("Conversation with {w} on {b}"),
                                (Some(w), None) => format!("Conversation with {w}"),
                                (None, Some(b)) => format!("Conversation on {b}"),
                                (None, None) => "Message history".to_string(),
                            };
                            println!("{}\n", style(header).bold().underlined());
                        }
                        // Neutral viewer: show both sides verbatim.
                        print_messages_numbered(&msgs, "", plain);
                    }
                }

                Some(MsgCommands::Replay {
                    limit,
                    with,
                    branch,
                    interval,
                }) => {
                    use std::io::Write as _;
                    let msgs = msg::history(git, with.as_deref(), branch.as_deref(), limit)?;
                    if msgs.is_empty() {
                        if !plain {
                            println!("{} No messages yet.", WARN);
                        }
                    } else {
                        if !plain {
                            radio_border('┌', '┐', "H5I AGENT RADIO · REPLAY");
                            let scope = match (&with, &branch) {
                                (Some(w), Some(b)) => format!("conversation with {w} on {b}"),
                                (Some(w), None) => format!("conversation with {w}"),
                                (None, Some(b)) => format!("conversation on {b}"),
                                (None, None) => "message history".to_string(),
                            };
                            radio_row(&format!(
                                "replaying {} {} {} message{} {} {:.3}s between",
                                scope,
                                style("·").dim(),
                                msgs.len(),
                                if msgs.len() == 1 { "" } else { "s" },
                                style("·").dim(),
                                interval,
                            ));
                            radio_bottom();
                            println!();
                        }
                        // Fractional seconds; clamp negatives to 0 (no pause).
                        let delay = std::time::Duration::from_secs_f64(interval.max(0.0));
                        let last = msgs.len() - 1;
                        for (i, m) in msgs.iter().enumerate() {
                            print_one_message(i + 1, m, "", plain);
                            let _ = std::io::stdout().flush();
                            if i != last && !delay.is_zero() {
                                std::thread::sleep(delay);
                            }
                        }
                    }
                }

                Some(MsgCommands::Team) => {
                    let roster = msg::team(git);
                    if roster.is_empty() {
                        println!(
                            "{} No agents yet — send a message to populate the roster.",
                            WARN
                        );
                    } else {
                        println!("{}\n", style("Agents on this channel").bold().underlined());
                        let me = msg::read_identity(&h5i_root);
                        for (name, last_seen) in roster {
                            let you = if Some(&name) == me.as_ref() {
                                style(" (you)").green().to_string()
                            } else {
                                String::new()
                            };
                            // Roster name + timestamp are pulled — sanitise.
                            println!(
                                "  {} {}{}   {}",
                                style("●").cyan(),
                                style(msg::sanitize_display(&name)).bold(),
                                you,
                                style(format!("last seen {}", msg::sanitize_display(&last_seen)))
                                    .dim()
                            );
                        }
                    }
                }

                Some(MsgCommands::Whoami { name }) => match name {
                    Some(n) => {
                        msg::write_identity(&h5i_root, n.trim())?;
                        println!(
                            "{} Identity for this repo set to {}.",
                            SUCCESS,
                            style(n.trim()).green().bold()
                        );
                    }
                    None => match msg::read_identity(&h5i_root) {
                        Some(id) => println!("{}", style(id).green().bold()),
                        None => println!(
                            "{} No identity set. Run {} or send with {}.",
                            WARN,
                            style("h5i msg as <name>").bold(),
                            style("--from <name>").bold()
                        ),
                    },
                },

                Some(MsgCommands::Hook { as_agent, block }) => {
                    // Inside a confined env box ($H5I_ENV_ID set) the msg store
                    // (.git/.h5i/msg) is deliberately sealed — it stays a
                    // host-mediated coordination channel, so the box can't write
                    // read-state (cursors/views). The Stop hook is inherited from
                    // the project settings and would otherwise hit EACCES advancing
                    // the cursor; turn-delivery isn't the confined agent's job, so
                    // no-op cleanly rather than erroring at the user.
                    if std::env::var(h5i_core::env::H5I_ENV_ID_VAR).is_ok() {
                        return Ok(());
                    }
                    // Turn-delivery: meant to run from a Stop hook. Resolve the
                    // identity quietly; if none is configured there is nothing
                    // to deliver, so exit cleanly rather than erroring out.
                    let Ok(me) = msg::resolve_identity(&h5i_root, as_agent.as_deref()) else {
                        return Ok(());
                    };
                    // In --block mode, bail if this stop was itself caused by a
                    // hook continuation — otherwise we'd loop forever.
                    if block && stdin_stop_hook_active() {
                        return Ok(());
                    }
                    // Peek (advance=false): we commit read-state only *after* the
                    // messages have actually been emitted, so a dropped or failed
                    // render never silently consumes mail (deliver-then-ack).
                    let unread = msg::inbox(git, &h5i_root, &me, false)?;
                    if !unread.is_empty() {
                        let ids: Vec<String> = unread.iter().map(|m| m.id.clone()).collect();
                        msg::write_last_view(&h5i_root, &me, &ids)?;

                        // Frame as quoted, untrusted collaborator input (i5h
                        // §Hook Delivery) — never authoritative instructions.
                        let text = frame_unread(&me, &unread);

                        if block {
                            // Autonomous turn mode: block the stop and feed the
                            // messages back so the agent keeps working to handle
                            // them (agmsg turn semantics).
                            let out = serde_json::json!({ "decision": "block", "reason": text });
                            println!("{}", serde_json::to_string(&out)?);
                        } else if plain {
                            // Codex / other hosts / manual use: raw text.
                            println!("{text}");
                        } else {
                            // Default Claude Code Stop hook: a bare stdout line is
                            // not reliably surfaced, so emit a `systemMessage` JSON
                            // object (shown to the user between turns). Does not
                            // block the stop.
                            let out = serde_json::json!({ "systemMessage": text });
                            println!("{}", serde_json::to_string(&out)?);
                        }

                        // Acknowledge after a successful emit.
                        msg::mark_seen(&h5i_root, &me, &ids)?;
                    }
                }

                Some(MsgCommands::Wait {
                    as_agent,
                    all,
                    timeout,
                    interval,
                }) => {
                    use std::collections::HashSet;
                    use std::io::Write as _;
                    let me: Option<String> = if all {
                        None
                    } else {
                        msg::resolve_identity(&h5i_root, as_agent.as_deref()).ok()
                    };
                    // Channel mode: baseline current ids; wake on the first new
                    // arrival. Inbox mode: peek unread (existing or new) — returns
                    // immediately if mail is already waiting.
                    let mut baseline: HashSet<String> = if me.is_none() {
                        msg::history(git, None, None, usize::MAX)?
                            .into_iter()
                            .map(|m| m.id)
                            .collect()
                    } else {
                        HashSet::new()
                    };
                    let interval = interval.max(1);
                    let mut waited = 0u64;
                    loop {
                        let repo = H5iRepository::open(".")?;
                        let hits: Vec<msg::Message> = match &me {
                            // Peek (no consume): the woken agent runs `inbox` to
                            // consume + number for replies.
                            Some(name) => msg::inbox(repo.git(), &repo.h5i_root, name, false)?,
                            None => {
                                let fresh: Vec<msg::Message> =
                                    msg::history(repo.git(), None, None, usize::MAX)?
                                        .into_iter()
                                        .filter(|m| !baseline.contains(&m.id))
                                        .collect();
                                for m in &fresh {
                                    baseline.insert(m.id.clone());
                                }
                                fresh
                            }
                        };
                        if !hits.is_empty() {
                            if plain {
                                for m in &hits {
                                    println!("{}", stream_line(m));
                                }
                            } else {
                                print_messages_numbered(&hits, me.as_deref().unwrap_or(""), false);
                            }
                            let _ = std::io::stdout().flush();
                            break; // exit on first delivery — the wake signal
                        }
                        // timeout == 0 → wait forever.
                        if timeout != 0 && waited >= timeout {
                            break; // give up quietly (exit 0, no output)
                        }
                        std::thread::sleep(std::time::Duration::from_secs(interval));
                        waited += interval;
                    }
                }

                Some(MsgCommands::Watch {
                    as_agent,
                    all,
                    interval,
                    once,
                    tui,
                }) => {
                    use std::collections::HashSet;
                    use std::io::{IsTerminal, Write as _};
                    // Identity-scoped *conversation* stream (both directions —
                    // sent, received, and broadcasts), unless `--all` or no
                    // identity is resolvable → watch the whole channel.
                    let me: Option<String> = if all {
                        None
                    } else {
                        msg::resolve_identity(&h5i_root, as_agent.as_deref()).ok()
                    };

                    // Cinematic full-screen Agent Radio: opt-in via `--tui`, and
                    // only on a real, live TTY. The default (and every scripted /
                    // piped / Monitor path — `--plain`, `--once`, or a non-TTY
                    // stdout) keeps the stable line-streaming watcher below.
                    if tui && !plain && !once && std::io::stdout().is_terminal() {
                        h5i_core::radio::run_watch(std::path::Path::new("."), me, all, interval)?;
                        return Ok(());
                    }

                    if !once && !plain {
                        radio_border('┌', '┐', "H5I AGENT RADIO · LIVE");
                        let who = match &me {
                            Some(name) => style(name.clone()).green().bold().to_string(),
                            None => style("all messages").yellow().to_string(),
                        };
                        radio_row(&format!(
                            "{} {} listening on {} {} every {}s · Ctrl+C to stop",
                            who,
                            style("·").dim(),
                            style(msg::MSG_REF).magenta(),
                            style("·").dim(),
                            interval,
                        ));
                        // Nudge interactive users toward the richer dashboard.
                        if !tui && std::io::stdout().is_terminal() {
                            radio_row(&format!(
                                "{} try {} for a better, full-screen format!",
                                style("hint:").cyan().bold(),
                                style("--tui").yellow().bold(),
                            ));
                        }
                        radio_bottom();
                    }

                    // `watch` is a PASSIVE dashboard: it must never advance the
                    // shared per-agent read-state (doing so silently consumed mail
                    // before the Stop hook / `inbox` could surface it). Dedup is
                    // tracked in this in-memory `seen` set only. For the firehose
                    // (no identity) we seed it with the current log so we stream
                    // only messages that arrive AFTER launch.
                    let mut seen: HashSet<String> = if me.is_none() {
                        msg::history(git, None, None, usize::MAX)?
                            .into_iter()
                            .map(|m| m.id)
                            .collect()
                    } else {
                        HashSet::new()
                    };

                    loop {
                        // Reopen the repo each tick so messages committed by other
                        // processes become visible.
                        let repo = H5iRepository::open(".")?;
                        // A passive *conversation* view: stream every message the
                        // agent sent or received (and broadcasts), both directions
                        // — like `history`, not just the inbox. Read from the full
                        // log and filter; never touch the per-agent read cursor.
                        let candidates: Vec<msg::Message> = match &me {
                            Some(name) => msg::history(repo.git(), None, None, usize::MAX)?
                                .into_iter()
                                .filter(|m| {
                                    &m.from == name || &m.to == name || m.to == msg::BROADCAST
                                })
                                .collect(),
                            None => msg::history(repo.git(), None, None, usize::MAX)?,
                        };
                        let batch: Vec<msg::Message> = candidates
                            .into_iter()
                            .filter(|m| !seen.contains(&m.id))
                            .collect();
                        for m in &batch {
                            seen.insert(m.id.clone());
                        }
                        if !batch.is_empty() {
                            if let Some(name) = &me {
                                // Persist the batch so `h5i msg reply <n>` works.
                                let ids: Vec<String> = batch.iter().map(|m| m.id.clone()).collect();
                                let _ = msg::write_last_view(&repo.h5i_root, name, &ids);
                            }
                            if plain {
                                for m in &batch {
                                    println!("{}", stream_line(m));
                                }
                                // Flush so the Monitor tool sees lines promptly.
                                let _ = std::io::stdout().flush();
                            } else {
                                print_messages_numbered(&batch, me.as_deref().unwrap_or(""), false);
                            }
                        }
                        if once {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_secs(interval.max(1)));
                    }
                }
            }
        }
    Ok(())
}
