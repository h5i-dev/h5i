//! `h5i box share` — letting one other person try the web app in a box.
//!
//! Everything else h5i does is about what leaves a box. This is the one path
//! that lets something in: a second person, on their own machine, opening the
//! dev server an agent has been building, while it is still running inside the
//! boundary.
//!
//! # The shape
//!
//! ```text
//!   their browser
//!        │  http://127.0.0.1:… (p2p)   or   https://….trycloudflare.com
//!        ▼
//!   [ the gate ]      token, expiry, revocation — checked per connection
//!        │
//!        ▼
//!   [ the bridge ]    counts, receipt, and the only route into the box
//!        │  enters the box's netns by pid, connects, passes the fd back
//!        ▼
//!   dev server on 127.0.0.1:3000, inside the box
//! ```
//!
//! # What each module owns
//!
//! * [`ticket`] — the capability a peer holds. Possession is authorization.
//! * [`session`] — the grant table on disk, outside every path a box can write.
//!   Revocation lives here because the process that revokes is not the process
//!   that serves.
//! * [`dialer`] — the single fork into the box's namespaces, pinned to one port
//!   for its whole life, so nothing on the wire can redirect it.
//! * [`gate`] and [`http_front`] — reading a credential off a request and
//!   making sure it never travels upstream.
//! * [`bridge`] — authorization, accounting, and the ingress receipt.
//! * [`p2p`] and [`tunnel`] — the two transports, over the same bridge.
//! * [`join`] — the other machine.
//!
//! # The properties worth holding onto
//!
//! * **The box's port is never published.** Not on the host, not by either
//!   transport. The only route in is the dialer, and it goes one place.
//! * **Authorization is per connection, from disk.** A revoke written by
//!   another process takes effect on the next connection, and a watchdog drops
//!   the ones already open.
//! * **The credential never reaches the box.** The shared app is agent-written
//!   code we are deliberately showing someone; handing it the token that
//!   admitted its visitor would be handing it the share.
//! * **Being shared is recorded.** A box that was opened to someone and an
//!   identical box that was not are different artifacts, and the export says
//!   which one it came from.

pub mod bridge;
pub mod dialer;
pub mod gate;
pub mod http_front;
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
