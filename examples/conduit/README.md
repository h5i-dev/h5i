# Conduit executor benchmark

Source: the Conduit Rust kernel, PostgreSQL store, HTTP API, load test and
random-command test copied from h5i-app-bench's read-only reference at
`performance/realworld/rust-h5i` (Apache-2.0). The original benchmark repository
was not modified. Framework dependencies here use this checkout.

The kernel source is unchanged. The shell adds a PG/RAM switch and calculates
list/feed counts from the RAM snapshot when the PG projection has not already
provided them. Only generated Schema.lean is included, to keep the existing
schema consistency test. This is a measurement fixture, not a copy of the
Conduit Lean proof project; its theorems are not newly claimed or gated here.

Build with `cargo build --release --manifest-path examples/conduit/Cargo.toml
-p conduit-server --example load`. Against a disposable PostgreSQL database,
set DATABASE_URL and run `BENCH_ENGINE=pg <target>/release/examples/load 5
get-article,list,feed,favorite 1,16,64`, then repeat with BENCH_ENGINE=mem.
The harness truncates and seeds its own `conduit` schema. See
[measurements](../../docs/app/MEMORY.md) for the deployment model and results.
