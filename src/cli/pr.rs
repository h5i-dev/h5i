//! `h5i pr` — CLI handlers (migrated from main.rs).
#![allow(clippy::all)]
use crate::*;

#[derive(Subcommand)]
pub enum PrCommands {
    /// Post (or upsert) a sticky comment on the current branch's open PR.
    /// Uses `gh` CLI under the hood.
    Post {
        /// PR number (default: auto-detect from current branch)
        #[arg(long, value_name = "N")]
        number: Option<u64>,

        /// Limit number of commits included
        #[arg(short, long, default_value_t = 25)]
        limit: usize,

        /// Hero block layout: `receipt` (default — scannable summary block),
        /// `review` (reviewer-first triage brief), `detective` (narrative:
        /// goal → considered → key insight → shipped), or `replay`
        /// (DAG-as-hero with milestone markers).
        #[arg(long, value_enum, default_value_t = PrStyleArg::Receipt)]
        style: PrStyleArg,

        /// Print the markdown body and exit without calling `gh`
        #[arg(long)]
        dry_run: bool,

        /// Omit the 💬 Agent coordination section (branch-scoped i5h messages)
        #[arg(long)]
        no_msg: bool,

        /// Include a redacted excerpt for *every* message kind, not just
        /// review-typed ones (default: FYI/free-text are metadata-only).
        #[arg(long)]
        msg_bodies: bool,

        /// Cap on coordination threads rendered before eliding
        #[arg(long, value_name = "N", default_value_t = 12)]
        msg_limit: usize,
    },

    /// Print the PR comment markdown to stdout (for piping into `gh pr edit --body-file -`)
    Body {
        /// Limit number of commits included
        #[arg(short, long, default_value_t = 25)]
        limit: usize,

        /// Hero block layout — see `h5i share pr post --help` for options.
        #[arg(long, value_enum, default_value_t = PrStyleArg::Receipt)]
        style: PrStyleArg,

        /// Omit the 💬 Agent coordination section (branch-scoped i5h messages)
        #[arg(long)]
        no_msg: bool,

        /// Include a redacted excerpt for *every* message kind, not just
        /// review-typed ones (default: FYI/free-text are metadata-only).
        #[arg(long)]
        msg_bodies: bool,

        /// Cap on coordination threads rendered before eliding
        #[arg(long, value_name = "N", default_value_t = 12)]
        msg_limit: usize,
    },
}

pub fn run(action: PrCommands) -> anyhow::Result<()> {
    match action {
            PrCommands::Post {
                number,
                limit,
                style,
                dry_run,
                no_msg,
                msg_bodies,
                msg_limit,
            } => {
                let workdir = std::env::current_dir()?;
                let msg_opts = h5i_core::pr::MsgOptions {
                    include: !no_msg,
                    full_bodies: msg_bodies,
                    max_threads: msg_limit,
                };
                let body = h5i_core::pr::render_body_with_options(
                    &workdir,
                    limit,
                    style.into(),
                    &msg_opts,
                )?;
                if dry_run {
                    println!("{}", body);
                    return Ok(());
                }
                h5i_core::pr::post_comment(&workdir, number, &body)?;
            }
            PrCommands::Body {
                limit,
                style,
                no_msg,
                msg_bodies,
                msg_limit,
            } => {
                let workdir = std::env::current_dir()?;
                let msg_opts = h5i_core::pr::MsgOptions {
                    include: !no_msg,
                    full_bodies: msg_bodies,
                    max_threads: msg_limit,
                };
                let body = h5i_core::pr::render_body_with_options(
                    &workdir,
                    limit,
                    style.into(),
                    &msg_opts,
                )?;
                println!("{}", body);
            }
        }
    Ok(())
}
