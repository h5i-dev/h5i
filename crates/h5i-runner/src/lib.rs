//! h5i remote runner — a box on a machine that is not this one.
//!
//! The design is ROADMAP.md R1 to R13. The short version:
//!
//! - **Placement is an axis, not a tier** (R1). A box declares how it is
//!   confined; this adds *where*. A runner requires Linux and this protocol, and
//!   everything past that (isolation tiers, a container runtime, memory,
//!   storage, persistence, its own internet route) is an advertised
//!   [`proto::Capabilities`]. A capability the runner lacks is a refusal, never
//!   a silent weakening.
//! - **The worker is h5i** (R3). Not a thin shim driving podman: the same
//!   binary running the same `h5i-sandbox`, so the policy-to-argv logic and the
//!   egress proxy stay where the container runtime is.
//! - **Nothing listens** (R4). The worker is an SSH forced command speaking
//!   frames on stdio, one process per RPC. No daemon, no port, no token, no
//!   TLS. Mutual authentication is the pair key outbound and the pinned host key
//!   inbound.
//! - **Identity is cryptographic** (R6). A runner is the SHA-256 of its host key
//!   ([`identity`]), not its name. Names are labels, and labels can be
//!   re-pointed at other hardware.
//!
//! The layering runs bottom-up, each layer testable without the one above:
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
//! Create, exec and export are declared on the wire ([`proto::FrameKind`]) and
//! refused with [`proto::ErrorCode::Unimplemented`] until their milestones land,
//! so a client meeting an older or newer runner gets a sentence rather than a
//! closed pipe.

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
