//! `h5i box share` — letting one other person try the web app in a box.
//!
//! Everything else h5i does is about what leaves a box. This is the one path
//! that lets something in: a second person, on their own machine, opening the
//! dev server an agent is building while it runs inside the boundary.
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
//! Both platforms answer *is this port the box's?*; only the argument differs.
//! Linux makes it true by construction, since the port is in a namespace and
//! the dialer is the only way in. macOS has no namespace, so it is established
//! by observation, per connection ([`owner`]).
//!
//! # What each module owns
//!
//! * [`ticket`] — the capability a peer holds. Possession is authorization.
//! * [`session`] — the grant table on disk, outside every path a box can write.
//!   Revocation lives here because the process that revokes is not the one that
//!   serves.
//! * [`dialer`] — the single fork into the box's namespaces, pinned to one port
//!   for its whole life, so nothing on the wire can redirect it.
//! * [`owner`] — whose port is this, on the platform with no namespace.
//! * [`gate`] and [`http_front`] — reading a credential off a request and
//!   keeping it from travelling upstream.
//! * [`bridge`] — authorization, accounting, and the ingress receipt.
//! * [`p2p`] and [`tunnel`] — the two transports, over the same bridge.
//! * [`pump`] — moving bytes, and counting them as they go.
//! * [`run`] — starting, describing and ending a share, in the order required.
//! * [`join`] — the other machine.
//!
//! # The properties worth holding onto
//!
//! * **The box's port is never published**, on the host or by either transport.
//!   The dialer is the only route in and it goes one place. On macOS the port is
//!   already on the host's loopback, where h5i did not put it and cannot remove
//!   it, so a share promises that *this* route reaches the box's own server,
//!   not that nothing else on the machine can.
//! * **Authorization is per connection, from disk.** A revoke written by
//!   another process takes effect on the next connection, and a watchdog drops
//!   the ones already open.
//! * **The credential never reaches the box.** The shared app is agent-written
//!   code we are deliberately showing someone; handing it the token that
//!   admitted its visitor would be handing it the share.
//! * **Being shared is recorded.** A box opened to someone and an identical box
//!   that was not are different artifacts, and the export says which it is.

pub mod bridge;
// Random heads for the two parsers. Tests only: it exists to be run, not
// shipped, and nothing outside a test may depend on it.
pub mod dialer;
#[cfg(test)]
mod fuzz;
pub mod gate;
pub mod http_front;
// Whose port is this? The macOS half of "the only route in goes to the box" —
// on Linux a namespace makes it true by construction, and here it is
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
