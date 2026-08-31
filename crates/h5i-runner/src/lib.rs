//! h5i remote runner. A box on a machine that is not this one.
//!
//! design-runner.md R1 to R13 carry the design. Four decisions govern the code:
//!
//! - Placement is an axis, not a tier (R1). A runner requires Linux and this
//!   protocol; isolation tiers, a container runtime, memory, storage,
//!   persistence and its own internet route are advertised
//!   [`proto::Capabilities`]. A capability the runner lacks is a refusal, never
//!   a silent weakening.
//! - The worker is h5i (R3), not a shim driving podman, so the
//!   policy-to-argv logic and the egress proxy stay where the runtime is.
//! - *Nothing listens* (R4). An SSH forced command speaking frames on stdio,
//!   one process per RPC. No daemon, port, token or TLS; authentication is the
//!   pair key outbound and the pinned host key inbound.
//! - Identity is cryptographic (R6). A runner is the SHA-256 of its host key
//!   ([`identity`]), not its name, because names can be re-pointed at other
//!   hardware.
//!
//! ```text
//! wire       framing, no transport and no meaning
//! proto      meaning: frame kinds, messages, versioning, validation
//! identity   runner_id and fingerprints from a pinned host key
//! host       what a worker may truthfully say about its machine
//! boxstore   what a box is on the runner's disk: creating/ then live/
//! source     building a git bundle here, materialising it there
//! transport  channels: SSH, or a child process for CI
//! serve      the worker loop      client   the control-plane side
//! config     where a paired runner is remembered, host-scoped
//! ```
//!
//! Built here is R13.1: pairing, probing, and the codec with its failure modes.
//! Create, exec and export are declared on the wire and refused with
//! [`proto::ErrorCode::Unimplemented`], so a client meeting an older or newer
//! runner gets a sentence rather than a closed pipe.

pub mod boxstore;
pub mod client;
pub mod config;
pub mod host;
pub mod identity;
pub mod proto;
pub mod serve;
pub mod source;
pub mod transport;
pub mod wire;

/// The confinement crate this protocol carries policy for.
///
/// Re-exported so a caller can build a `CreateRequest` without declaring a
/// second dependency on it: the policy types are part of this protocol's
/// surface whether or not they are defined here, and one import path for them
/// is one fewer way for two versions to end up in a build.
pub use h5i_sandbox;

pub use client::{Client, ClientError, Probed};
pub use config::RunnerRecord;
pub use identity::HostKey;
pub use proto::{Capabilities, PROTOCOL_VERSION};
pub use serve::Worker;
pub use transport::{ChildProcessTransport, SshTransport, Transport};
