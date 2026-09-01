//! `h5i-wasm-harness`: a minimal, sans-io coding-agent loop that runs both natively and as
//! WebAssembly.

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
