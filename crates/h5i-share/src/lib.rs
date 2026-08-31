//! `h5i box share`: letting one other person try the web app in a box.
//!
//! Everything else h5i does is about what leaves a box. This is the one path
//! that lets something in.
//!
//! ```text
//!   their browser
//!        │  http://127.0.0.1:… (p2p)   or   https://….trycloudflare.com
//!        ▼
//!   [ the gate ]      token, expiry, revocation, checked per connection
//!        │
//!        ▼
//!   [ the bridge ]    counts, receipt, and the only route into the box
//!        │  Linux: enters the box's netns by pid, connects, passes the fd back
//!        │  macOS: finds the box's own listening socket, and refuses any other
//!        ▼
//!   dev server on 127.0.0.1:3000, inside the box
//! ```
//!
//! Both platforms answer *is this port the box's?*. Linux makes it true by
//! construction; macOS establishes it by observation, per connection ([`owner`]).
//!
//! [`ticket`] is the capability a peer holds, possession being authorization.
//! [`session`] is the grant table on disk, outside every path a box can write.
//! [`dialer`] is the single fork into the box's namespaces, pinned to one port.
//! [`owner`] answers the macOS question. [`gate`] and [`http_front`] read the
//! credential off a request and keep it from travelling upstream. [`bridge`]
//! does authorization, accounting and the ingress receipt, under the [`p2p`] and
//! [`tunnel`] transports, with [`pump`] moving and counting bytes. [`run`]
//! starts, describes and ends a share; [`join`] is the other machine.
//!
//! Four properties: the box's port is never published, and on macOS that
//! promises this route reaches the box's own server rather than that nothing
//! else on the machine can. Authorization is per connection from disk, so a
//! revoke lands on the next one and a watchdog drops the rest. The credential
//! never reaches the box, which is agent-written code we are showing someone.
//! And being shared is recorded, so the export says which artifact this is.

pub mod bridge;
// Random heads for the two parsers. Tests only: it exists to be run, not
// shipped, and nothing outside a test may depend on it.
pub mod dialer;
#[cfg(test)]
mod fuzz;
pub mod gate;
pub mod http_front;
// Whose port is this? The macOS half of "the only route in goes to the box".
// On Linux a namespace makes it true by construction, and here it is
// established by observation. The rule it applies is pure and compiled
// everywhere; the questions it asks Darwin are not.
pub mod owner;
pub mod pump;
pub mod run;
pub mod session;
pub mod ticket;
pub mod wire;

// The peer-to-peer transport and the joining side. Optional: a build without
// them keeps the tunnel transport, which needs no QUIC stack of its own.
#[cfg(feature = "p2p")]
pub mod join;
#[cfg(feature = "p2p")]
pub mod p2p;

pub mod tunnel;
