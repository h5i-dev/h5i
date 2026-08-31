//! The `h5i` binary: a three-line router over the library that owns the CLI.
//!
//! Everything (the clap tree, the argument bootstrap, the dispatch) lives in
//! `src/lib.rs`, because `examples/gen_man.rs` renders the man page from that
//! same command tree and a binary crate has nothing another target can import.

fn main() -> anyhow::Result<()> {
    h5i::run()
}
