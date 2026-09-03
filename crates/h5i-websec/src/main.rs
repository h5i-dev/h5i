//! `h5i websec`: the HTTP workbench, as an h5i plugin.
//!
//! Burp Suite is an HTTP workbench for humans. This is one for agents: read what
//! a browser session sent, change a part of it, send it again, and compare the
//! answers. The design is `docs/design/design-websec.md`.
//!
//! This binary holds no privilege of its own. Every verb here runs an `h5i
//! browser` verb in a subprocess, which means a plugin cannot reach a session by
//! any path the CLI does not already offer, and its requests are the engine's
//! fetches: checked by the engine's policy, spent from the engine's budget,
//! written into the engine's receipts. That is the whole reason a plugin is a
//! separate process rather than something loaded into h5i (see `src/cli/
//! plugin.rs`).
//!
//! What it adds over typing the underlying verbs is naming. `req_42` rather than
//! a bare number, one noun for the workbench rather than verbs scattered through
//! `h5i browser`, and defaults chosen for a loop rather than for a person
//! reading a page.

use std::ffi::OsString;
use std::process::Command;

use clap::{Parser, Subcommand};

/// The h5i that launched this plugin.
///
/// Passed in rather than found on `$PATH`: a plugin that guessed could compose
/// verbs from a *different* build than the one the user ran, which would make
/// "the same verbs a person types" quietly false.
fn h5i() -> OsString {
    std::env::var_os("H5I_BIN").unwrap_or_else(|| OsString::from("h5i"))
}

#[derive(Parser)]
#[command(
    name = "h5i websec",
    version,
    about = "An HTTP workbench for agents: read, edit, resend and compare what a session sent."
)]
struct Cli {
    #[command(subcommand)]
    command: Verb,

    /// Which session, when more than one is open.
    #[arg(long, short = 's', global = true, value_name = "NAME")]
    session: Option<String>,

    /// Emit JSON. The default for every verb here, because the caller is
    /// usually a script; pass `--human` for the reading version.
    #[arg(long, global = true)]
    human: bool,
}

#[derive(Subcommand)]
enum Verb {
    /// What this session sent, newest last.
    Requests {
        /// Only this method.
        #[arg(long, value_name = "METHOD")]
        method: Option<String>,
        /// Only rows whose URL contains this.
        #[arg(long, value_name = "TEXT")]
        url_contains: Option<String>,
        /// Only responses with this status.
        #[arg(long, value_name = "CODE")]
        status: Option<u16>,
        /// Only `navigation`, `subresource`, `frame` or `redirect`.
        #[arg(long, value_name = "KIND")]
        initiator: Option<String>,
        /// Only what policy refused.
        #[arg(long)]
        denied_only: bool,
        /// At most this many rows.
        #[arg(long, value_name = "N")]
        limit: Option<u64>,
    },

    /// One stored message, as it went out or as it came back.
    Show {
        /// `req_42`, `res_42`, or just `42`.
        #[arg(value_name = "ID")]
        id: String,
        /// Print it as an HTTP message.
        #[arg(long)]
        raw: bool,
    },

    /// Send one of this session's requests again, with changes.
    Replay {
        /// `req_42`, or just `42`.
        #[arg(value_name = "ID")]
        id: String,
        /// `query.id=456`, `header.X-Real-IP=1.2.3.4`, `json.role=admin`,
        /// `multipart.file.filename=shell.php`. Repeatable, applied in order.
        #[arg(long = "set", value_name = "TARGET=VALUE")]
        set: Vec<String>,
        /// Remove a target.
        #[arg(long = "unset", value_name = "TARGET")]
        unset: Vec<String>,
        /// Add targets that are not there.
        #[arg(long)]
        create: bool,
        /// Send it from another session, with that session's credentials.
        #[arg(long = "as", value_name = "SESSION")]
        as_session: Option<String>,
        /// Send it this many times and report the clock.
        #[arg(long, value_name = "N")]
        repeat: Option<u32>,
        /// Release the repeats together, for a race.
        #[arg(long)]
        race: bool,
        /// Stop at the first redirect and report it.
        #[arg(long)]
        no_follow: bool,
    },

    /// How two of this session's responses differ.
    Diff {
        #[arg(value_name = "ID")]
        left: String,
        #[arg(value_name = "ID")]
        right: String,
    },

    /// Ask a stored response a question. Exits 0 when it holds, 1 when it does
    /// not, 2 when it could not be asked.
    Match {
        #[arg(value_name = "ID")]
        id: String,
        #[arg(long, value_name = "PATTERN")]
        regex: Option<String>,
        #[arg(long, value_name = "TEXT")]
        contains: Option<String>,
        #[arg(long = "json-path", value_name = "PATH[=VALUE]")]
        json_path: Option<String>,
        #[arg(long, value_name = "NAME[=VALUE]")]
        header: Option<String>,
        #[arg(long, value_name = "CODE")]
        status: Option<u16>,
        #[arg(long, value_name = "BYTES")]
        longer_than: Option<u64>,
        #[arg(long, value_name = "BYTES")]
        shorter_than: Option<u64>,
    },

    /// What this session reached, as origins and endpoints.
    Sitemap,

    /// Run a multi-step flow with bindings between the steps.
    Sequence {
        #[arg(value_name = "FILE")]
        file: String,
        #[arg(long = "var", value_name = "NAME=VALUE")]
        vars: Vec<String>,
        #[arg(long)]
        keep_going: bool,
    },
}

/// `req_42`, `res_42` and `42` all name sequence 42.
///
/// The prefixes exist because a finding reads better with them and because a
/// request and its response share a number; neither is a different thing to
/// look up. Anything else is refused rather than parsed as far as it goes: `42x`
/// silently becoming 42 is how a loop tests the wrong request.
fn sequence_of(id: &str) -> anyhow::Result<String> {
    let bare = id
        .strip_prefix("req_")
        .or_else(|| id.strip_prefix("res_"))
        .unwrap_or(id);
    bare.parse::<u64>()
        .map(|n| n.to_string())
        .map_err(|_| anyhow::anyhow!("`{id}` is not a message id: try `req_42`, `res_42` or `42`"))
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("{e}");
        std::process::exit(2);
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    let mut argv: Vec<String> = vec!["browser".to_string()];
    fn push(argv: &mut Vec<String>, args: &[&str]) {
        argv.extend(args.iter().map(|arg| (*arg).to_string()));
    }
    fn flag(argv: &mut Vec<String>, name: &str, value: Option<String>) {
        if let Some(value) = value {
            argv.push(format!("--{name}"));
            argv.push(value);
        }
    }

    match cli.command {
        Verb::Requests {
            method,
            url_contains,
            status,
            initiator,
            denied_only,
            limit,
        } => {
            push(&mut argv, &["requests"]);
            flag(&mut argv, "method", method);
            flag(&mut argv, "url-contains", url_contains);
            flag(&mut argv, "status", status.map(|s| s.to_string()));
            flag(&mut argv, "initiator", initiator);
            flag(&mut argv, "limit", limit.map(|s| s.to_string()));
            if denied_only {
                argv.push("--denied-only".into());
            }
        }
        Verb::Show { id, raw } => {
            let seq = sequence_of(&id)?;
            push(&mut argv, &["message"]);
            argv.push(seq);
            // `res_42` asks about the response half and `req_42` the request
            // half. Naming a half and being shown both would make the prefix
            // decorative.
            if id.starts_with("res_") {
                push(&mut argv, &["--part", "response"]);
            } else if id.starts_with("req_") {
                push(&mut argv, &["--part", "request"]);
            }
            if raw {
                argv.push("--raw".into());
            }
        }
        Verb::Replay {
            id,
            set,
            unset,
            create,
            as_session,
            repeat,
            race,
            no_follow,
        } => {
            let seq = sequence_of(&id)?;
            push(&mut argv, &["resend"]);
            argv.push(seq);
            for spec in set {
                argv.push("--set".into());
                argv.push(spec);
            }
            for spec in unset {
                argv.push("--unset".into());
                argv.push(spec);
            }
            if create {
                argv.push("--create".into());
            }
            flag(&mut argv, "as", as_session);
            flag(&mut argv, "repeat", repeat.map(|n| n.to_string()));
            if race {
                argv.push("--race".into());
            }
            if no_follow {
                argv.push("--no-follow".into());
            }
        }
        Verb::Diff { left, right } => {
            push(&mut argv, &["diff"]);
            argv.push(sequence_of(&left)?);
            argv.push(sequence_of(&right)?);
        }
        Verb::Match {
            id,
            regex,
            contains,
            json_path,
            header,
            status,
            longer_than,
            shorter_than,
        } => {
            push(&mut argv, &["match"]);
            argv.push(sequence_of(&id)?);
            flag(&mut argv, "regex", regex);
            flag(&mut argv, "contains", contains);
            flag(&mut argv, "json-path", json_path);
            flag(&mut argv, "header", header);
            flag(&mut argv, "status", status.map(|s| s.to_string()));
            flag(&mut argv, "longer-than", longer_than.map(|s| s.to_string()));
            flag(&mut argv, "shorter-than", shorter_than.map(|s| s.to_string()));
        }
        Verb::Sitemap => push(&mut argv, &["sitemap"]),
        Verb::Sequence {
            file,
            vars,
            keep_going,
        } => {
            push(&mut argv, &["sequence"]);
            argv.push(file);
            for var in vars {
                argv.push("--var".into());
                argv.push(var);
            }
            if keep_going {
                argv.push("--keep-going".into());
            }
        }
    }

    if let Some(session) = cli.session {
        argv.push("--session".into());
        argv.push(session);
    }
    // JSON unless asked otherwise. The caller here is usually a loop, and a
    // workbench whose default output has to be re-parsed out of prose is a
    // workbench nobody scripts.
    if !cli.human {
        argv.push("--json".into());
    }

    let status = Command::new(h5i())
        .args(&argv)
        .status()
        .map_err(|e| anyhow::anyhow!("could not run h5i: {e}"))?;
    // The underlying verb's code, unchanged. `match` exits 1 for "did not
    // match" and 2 for "could not look", and flattening those here would break
    // every script built on them.
    std::process::exit(status.code().unwrap_or(2));
}

#[cfg(test)]
mod tests {
    use super::sequence_of;

    #[test]
    fn a_message_id_is_read_with_or_without_its_prefix() {
        assert_eq!(sequence_of("req_42").unwrap(), "42");
        assert_eq!(sequence_of("res_42").unwrap(), "42");
        assert_eq!(sequence_of("42").unwrap(), "42");
    }

    /// Parsing as far as it goes is how a loop tests the wrong request.
    #[test]
    fn something_that_is_not_an_id_is_refused_rather_than_salvaged() {
        for bad in ["42x", "req_", "", "req_-1", "one"] {
            assert!(sequence_of(bad).is_err(), "`{bad}` should be refused");
        }
    }
}
