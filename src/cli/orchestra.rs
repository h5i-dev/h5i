//! `h5i orchestra` — the SDK-facing bridge for out-of-process scores.
//!
//! `serve` speaks line-delimited JSON-RPC 2.0 on stdin/stdout (see
//! `crates/h5i-orchestra/src/rpc.rs` for the protocol). It is spawned as a
//! child process by a host-language SDK (the Python `h5i.orchestra` package),
//! not run by hand: no socket, no daemon, one run per session, exits on EOF.

use clap::Subcommand;

#[derive(Subcommand)]
pub enum OrchestraCommands {
    /// Serve the orchestra JSON-RPC bridge on stdin/stdout (SDK-facing).
    /// stdout is protocol-only; logs go to stderr (H5I_LOG).
    Serve,
}

pub fn run(action: OrchestraCommands) -> anyhow::Result<()> {
    match action {
        OrchestraCommands::Serve => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(h5i_orchestra::rpc::serve_stdio(env!("CARGO_PKG_VERSION")))?;
            Ok(())
        }
    }
}
