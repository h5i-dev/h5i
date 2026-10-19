//! h5i's network layer: the policy, receipts, cookie jar, budget, secrets and
//! message store a session's capture proxy writes through, and the control
//! channel `h5i browser` and the plugins speak to. There is no page engine.

pub mod broker;
pub mod budget;
pub mod capture;
pub mod control;
pub mod cookies;
pub mod cors;
pub mod dom_inject;
pub mod edits;
pub mod grpc;
pub mod multipart;
pub mod net;
pub mod policy;
pub mod rawsock;
pub mod receipt;
pub mod secrets;
pub mod sse;
pub mod verbs;
pub mod ws;
pub mod wsclient;

pub use broker::Broker;
pub use policy::{Policy, Verdict};
pub use receipt::{JsonlSink, MemorySink, RequestRecord, Sink};
