//! `h5i-wasm-harness`: a minimal, sans-io coding-agent loop that runs both
//! natively and as WebAssembly.
//!
//! The library is `#![no_std] + alloc` with zero dependencies, so the exact
//! same core compiles three ways from one source: natively for `cargo test`
//! and the `i5h` host binary, and to `wasm32-unknown-unknown` as a `cdylib`
//! that any `WebAssembly.instantiate` (browser / Node) or WASI runtime can
//! load. All I/O — the model HTTP call and every tool run — is delegated to
//! the embedding host through `Effect` / `Event` values; the module itself
//! performs none, which is what lets one binary serve both environments.
//!
//! Layout:
//! - [`agent`] — the state machine (loop shape from mini-swe-agent, structural
//!   termination from hax). Pure; never does I/O.
//! - [`json`] — a tiny no_std JSON parser/serializer (insertion-ordered
//!   objects, f64 numbers), so the crate needs no `serde` and stays clean for
//!   the offline `build-std`-free wasm build.
//! - [`proto`] — the JSON wire schema for the effect/event boundary plus the
//!   `init` / `step` / `dump` contract shared by every host.
//! - `wasm` (wasm32 only) — the pinned six-symbol ABI over the above.
//!
//! The `i5h` binary (`src/bin/i5h.rs`) is the native host: a real filesystem,
//! a scripted mock model, and an optional plain-HTTP local model.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod agent;
pub mod json;
pub mod proto;

#[cfg(target_arch = "wasm32")]
mod wasm;

pub use agent::{Agent, Config, Effect, Event, Msg, ToolCall, TOOL_UNIVERSE};
pub use json::Value;
