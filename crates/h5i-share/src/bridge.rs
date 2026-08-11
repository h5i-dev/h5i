//! The bridge: everything a share does that is not about how the bytes travel.
//!
//! Both transports sit on this. It holds the three jobs that touch the
//! boundary, and holding them in one place is the point — a second transport
//! must not be a second chance to get authorization or evidence wrong:
//!
//! * **Reach the dev server**, through [`crate::dialer`], pinned to one port of
//!   one box for the bridge's whole life.
//! * **Hold the capability**, by resolving a presented secret against the grant
//!   table on disk ([`crate::session`]) on *every* connection, so a revoke
//!   written by another process takes effect on the next one.
//! * **Write the ingress receipt.** Every other lane in a box's receipt
//!   observes what left. This is the first that records what came in: who
//!   connected, when, over what path, how much, and who was turned away.
//!
//! The receipt lane is host observed in the strongest sense available — h5i
//! owns both ends of the bridge, the box supplies none of it, and the box
//! cannot suppress it. A box that was shared and an identical box that was not
//! are different artifacts, and an export should not be silent about which one
//! it came from.

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use h5i_error::H5iError;

use crate::dialer::Dialer;
use crate::session::{self, Denied, ShareSession, Transport};

/// How a peer's bytes actually travelled, as observed rather than as hoped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Path {
    /// A direct peer-to-peer path. Nothing but the two machines.
    Direct,
    /// Through a relay. Still end-to-end encrypted — the relay moves sealed
    /// packets and cannot read them — but a third party was on the wire and the
    /// receipt says so.
    Relayed,
    /// A Cloudflare quick tunnel. TLS terminates at Cloudflare, so unlike the
    /// two above this one is **not** end to end.
    Tunnel,
}

impl Path {
    pub fn as_str(self) -> &'static str {
        match self {
            Path::Direct => "direct",
            Path::Relayed => "relayed",
            Path::Tunnel => "tunnel",
        }
    }
}

/// One peer's time on the share.
#[derive(Debug, Clone)]
pub struct PeerRecord {
    /// Who, as far as the transport can say: an endpoint id for P2P, or a note
    /// that the tunnel cannot tell one browser from another.
    pub peer: String,
    pub grant: String,
    pub label: Option<String>,
    /// How this peer's bytes travelled, once something actually observed it.
    /// `None` until then, and rendered as such: a transport that has not
    /// selected a path yet is not evidence of either answer, and writing a
    /// guess here put "via direct" in the receipt for connections a relay
    /// carried.
    pub path: Option<Path>,
    pub opened: DateTime<Utc>,
    pub closed: Option<DateTime<Utc>>,
    /// The last time this peer did anything, for a transport with no close to
    /// observe. See [`Bridge::peer_seen`].
    pub last_seen: Option<DateTime<Utc>>,
    /// TCP connections into the box carried for this peer. One request each,
    /// because one connection is all a request gets — so this is also the count
    /// of requests that reached the box, and *not* the count of requests made:
    /// a peer that followed the invite link and read nothing has none.
    pub connections: u64,
    pub bytes_to_peer: u64,
    pub bytes_from_peer: u64,
}

/// A connection turned away for something other than its credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnedAway {
    pub at: DateTime<Utc>,
    pub why: TurnedAwayReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnedAwayReason {
    /// `--direct-only`, and the connection was on a relay.
    NoDirectPath,
    /// Connected, then never presented a ticket.
    NeverGreeted,
    /// A request the gate would not parse: the smuggling shapes it exists to
    /// refuse.
    Unparseable,
}

impl TurnedAwayReason {
    fn as_str(self) -> &'static str {
        match self {
            TurnedAwayReason::NoDirectPath => "no direct path was available",
            TurnedAwayReason::NeverGreeted => "connected but never presented a ticket",
            TurnedAwayReason::Unparseable => "sent something this share would not parse",
        }
    }
}

/// Someone knocked and was not let in. Worth recording: a share that was probed
/// is a fact about the session, and it is invisible everywhere else.
#[derive(Debug, Clone)]
pub struct DeniedAttempt {
    pub at: DateTime<Utc>,
    pub reason: Denied,
}

#[derive(Debug, Default)]
struct Tally {
    /// Did every connection finish before the receipt was written? Starts
    /// false: a receipt written without a quiesce at all has not settled
    /// either, and defaulting the other way would make silence look like a
    /// clean ending.
    settled: bool,
    /// See [`Summary::route_broken`].
    route_broken: u64,
    /// See [`Summary::turned_away`].
    turned_away: Vec<TurnedAway>,
    peers: Vec<PeerRecord>,
    denied: Vec<DeniedAttempt>,
    /// Attempts past what the list will hold. Counted so the receipt can say
    /// "1024 recorded, plus this many more" rather than reporting a share
    /// knocked on fifty thousand times as having been knocked on 1024 times.
    denied_overflow: u64,
    /// Connections turned away because the share was already carrying its
    /// limit into the box. Counted rather than listed: the interesting number
    /// is whether it happened at all.
    over_capacity: u64,
    /// Connections turned away at the front door, before a credential was ever
    /// asked for. A different fact from the one above and an anonymously
    /// inflatable one — a flooder can drive this into the millions — so it gets
    /// its own line rather than being added to a number a reader would take as
    /// the box's ceiling having been hit.
    front_refused: u64,
    /// Peers past what the record list holds. Counted, like the denial list's
    /// overflow, because a receipt that stops at 256 and says nothing is a
    /// receipt that reports a share nobody could read as a share nobody used.
    peers_overflow: u64,
    /// Peers who presented a good ticket and found nothing listening inside the
    /// box. Without this a share where the dev server was down reads as one
    /// nobody ever tried to use.
    unreachable: u64,
    /// Responses the box left unfinished: short of a `Content-Length` it
    /// declared, or an unframed stream it stopped feeding without closing. A
    /// truncated download reads to the visitor as the app being broken, so the
    /// receipt says which it was — and a visitor who cancelled a download is
    /// deliberately not counted here.
    truncated: u64,
}

/// How many connections into the box a share will carry at once.
///
/// A share is a door on the open internet in tunnel mode, and an iroh endpoint
/// anyone may dial in P2P mode. Without a ceiling, a peer holding a valid link —
/// or a page on the shared app opening sockets in a loop — turns into unbounded
/// tasks on the host and unbounded sockets into the box, which is a denial of
/// service against the box the share was meant to show off.
///
/// Sixty-four is chosen to be uninteresting: a browser opens about six
/// connections per origin, so this is roughly ten simultaneous viewers, and a
/// share is for one person. Reaching it is a signal, and it is recorded.
const MAX_CONNECTIONS: usize = 64;

/// How many distinct peers the receipt will describe individually.
///
/// A share is for one person, so reaching this means something odd is
/// happening; the ceiling exists so that "something odd" cannot also mean
/// unbounded host memory and an unreadable receipt.
const MAX_PEER_RECORDS: usize = 256;

/// A handle to one peer's entry in the tally.
#[derive(Debug, Clone, Copy)]
pub struct PeerId(usize);

/// The live share.
pub struct Bridge {
    env_dir: std::path::PathBuf,
    env_id: String,
    policy_digest: String,
    box_name: String,
    transport: Transport,
    endpoint: String,
    dialer: Dialer,
    started: DateTime<Utc>,
    tally: Mutex<Tally>,
    /// One permit per live connection into the box, held for its lifetime.
    capacity: Arc<tokio::sync::Semaphore>,
    /// Flipped when the share is winding up, so connections end promptly
    /// instead of being waited on.
    shutdown: tokio::sync::watch::Sender<bool>,
}

impl Bridge {
    pub fn new(
        env_dir: std::path::PathBuf,
        env_id: String,
        policy_digest: String,
        box_name: String,
        transport: Transport,
        endpoint: String,
        dialer: Dialer,
    ) -> Bridge {
        Bridge {
            env_dir,
            env_id,
            policy_digest,
            box_name,
            transport,
            endpoint,
            dialer,
            started: Utc::now(),
            tally: Mutex::new(Tally::default()),
            capacity: Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS)),
            shutdown: tokio::sync::watch::Sender::new(false),
        }
    }

    /// A connection turned away at the front door, before it was ever asked
    /// for a credential.
    ///
    /// A different ceiling from [`Self::admit`]'s and an earlier one: that
    /// bounds sockets into the box, this bounds connections this process will
    /// hold at all. A share taken down by a flood of anonymous connections used
    /// to write a receipt that said nobody came.
    pub fn record_front_refusal(&self) {
        self.tally().front_refused += 1;
    }

    /// The tally, recovered rather than lost if a previous holder panicked.
    ///
    /// One accessor for all of them. Half of these used `if let Ok(…)` and
    /// silently stopped counting after a poison while the other half carried on
    /// — which is the asymmetry the one that recovered was added to object to.
    fn tally(&self) -> std::sync::MutexGuard<'_, Tally> {
        match self.tally.lock() {
            Ok(t) => t,
            Err(p) => p.into_inner(),
        }
    }

    /// Take a slot for one connection into the box, or `None` if the share is
    /// already carrying its limit.
    ///
    /// Refuses rather than queues on purpose. A queue would turn a flood into
    /// latency for the person who is legitimately using the share, and hide the
    /// fact that anything unusual happened; a refusal is immediate, visible in
    /// the receipt, and leaves the connections already in flight alone.
    /// How many of the share's connection slots are free. For tests that need
    /// to show a path did *not* take one — "a later stream still works" leaves
    /// a handful of leaked permits invisible.
    /// The tally as it stands, for a test that needs to assert about a counter
    /// rather than about the sentence it eventually produces.
    pub fn snapshot(&self) -> Summary {
        self.summarise(Utc::now())
    }

    /// One place the tally becomes a `Summary`, so a snapshot and a receipt
    /// cannot drift into describing the same share differently.
    fn summarise(&self, ended: DateTime<Utc>) -> Summary {
        let t = self.tally();
        Summary {
            route_broken: t.route_broken,
            settled: t.settled,
            turned_away: t.turned_away.clone(),
            transport: self.transport,
            endpoint: self.endpoint.clone(),
            port: self.dialer.port(),
            started: self.started,
            ended,
            peers: t.peers.clone(),
            peers_overflow: t.peers_overflow,
            denied: t.denied.clone(),
            denied_overflow: t.denied_overflow,
            over_capacity: t.over_capacity,
            front_refused: t.front_refused,
            unreachable: t.unreachable,
            truncated: t.truncated,
        }
    }

    pub fn free_slots(&self) -> usize {
        self.capacity.available_permits()
    }

    /// The ceiling, so a test can say "all of them" without hard-coding it.
    pub const MAX_CONNECTIONS: usize = MAX_CONNECTIONS;

    pub fn admit(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        match self.capacity.clone().try_acquire_owned() {
            Ok(permit) => Some(permit),
            Err(_) => {
                self.tally().over_capacity += 1;
                None
            }
        }
    }

    pub fn port(&self) -> u16 {
        self.dialer.port()
    }

    pub fn box_name(&self) -> &str {
        &self.box_name
    }

    pub fn env_dir(&self) -> &std::path::Path {
        &self.env_dir
    }

    /// Resolve a presented secret against the grant table **as it is on disk
    /// right now**.
    ///
    /// Re-read per connection rather than cached, and that is the whole
    /// mechanism behind revocation: `h5i box share revoke` runs in a different
    /// process, so a cached table would be a revoke that appeared to work and
    /// did nothing. The cost is one small file read per connection, which is
    /// nothing next to opening a TCP connection into a namespace.
    pub fn authorize(&self, secret: &str) -> Result<AuthorizedGrant, Denied> {
        let now = Utc::now().timestamp();
        let Some(s) = session::read(&self.env_dir) else {
            // The grant table is gone or unreadable, so nothing authorizes
            // anything. Recorded like any other refusal: it is the one denial
            // class that used to leave the receipt silent.
            self.record_denied(Denied::Unknown);
            return Err(Denied::Unknown);
        };
        match s.authorize(secret, now) {
            Ok(g) => Ok(AuthorizedGrant {
                id: g.id.clone(),
                label: g.label.clone(),
                expires_at: g.expires_at,
            }),
            Err(d) => {
                self.record_denied(d);
                Err(d)
            }
        }
    }

    /// Is this particular grant still able to admit anyone?
    ///
    /// The connection watchdogs ask about *their own* grant rather than about
    /// the share as a whole. Asking about the share only closes connections
    /// when every grant is spent, which means revoking one peer while another
    /// is still admitted would leave the revoked peer's open connections —
    /// their hot-reload socket, their event stream — running. Revocation is
    /// advertised as per person; this is what makes it so.
    pub fn grant_is_live(&self, grant_id: &str) -> bool {
        let now = Utc::now().timestamp();
        match session::read(&self.env_dir) {
            Some(s) => s
                .grants
                .iter()
                .any(|g| g.id == grant_id && !g.revoked && g.expires_at > now),
            // The file is gone, so nothing authorizes anything. Fail closed.
            None => false,
        }
    }

    /// True once no grant can admit anyone: everything revoked, or everything
    /// expired. The transports poll this so a share that has been cut off drops
    /// the connections it is already carrying, instead of serving them until
    /// the peer gets bored.
    pub fn is_spent(&self) -> bool {
        let now = Utc::now().timestamp();
        match session::read(&self.env_dir) {
            Some(s) => s.is_spent(now),
            // The file is gone, so nothing authorizes anything. Fail closed.
            None => true,
        }
    }

    /// Open a fresh connection into the box.
    /// A socket into the box, and the accounting that goes with failing to get
    /// one.
    ///
    /// The two failures are recorded apart because the receipt says different
    /// things about them, and one of them blames the wrong person: `unreached
    /// N connection(s) ... found nothing listening on port 3000` is a sentence
    /// about the user's dev server, and it was printed for a broken dialer
    /// too. That case is sticky — a retired channel fails every later request
    /// the same way — so one lost reply produced a receipt asserting hundreds
    /// of times that a dev server was down while it was running the whole time.
    pub fn open_upstream(&self) -> Result<std::net::TcpStream, H5iError> {
        match self.dialer.connect() {
            Ok(s) => Ok(s),
            Err(e) => {
                if e.nothing_listening() {
                    self.tally().unreachable += 1;
                } else {
                    self.tally().route_broken += 1;
                }
                Err(e.into_inner())
            }
        }
    }

    fn record_denied(&self, reason: Denied) {
        // Bounded: a share left open on the internet can be knocked on all day,
        // and an unbounded list would be a memory leak with a receipt attached.
        // The count past the cap shows up as its own figure in the summary.
        let mut t = self.tally();
        if t.denied.len() < 1024 {
            t.denied.push(DeniedAttempt {
                at: Utc::now(),
                reason,
            });
        } else {
            t.denied_overflow += 1;
        }
    }

    /// Note that a peer has arrived. Returns the handle its traffic is counted
    /// against.
    ///
    /// Bounded, like the denial list and for the same reason: one entry carries
    /// a `String` and becomes a line in the receipt, and a peer cycling
    /// connections would grow both without limit.
    ///
    /// Past the cap the connection is still served — it is authorized, after
    /// all — and its handle names no record, so its traffic is counted nowhere.
    /// That is deliberate and it is not free: the *number* of such peers is
    /// counted here and reported, because folding them into the last record
    /// corrupted that record, and dropping them silently made a busy share
    /// look like a quiet one.
    pub fn peer_joined(&self, peer: String, grant: &AuthorizedGrant, path: Option<Path>) -> PeerId {
        // Fail soft like every other accessor here. This one used to `expect`,
        // and it is called while the tunnel front holds its own peer map — so a
        // poisoned tally would have poisoned that too, and taken every later
        // connection with it.
        let mut t = self.tally();
        if t.peers.len() >= MAX_PEER_RECORDS {
            // A handle that names nothing. Returning the *last* record folded
            // peer 257's bytes, connections and path observations into peer
            // 256 — including setting a path 256 had never had observed.
            t.peers_overflow += 1;
            return PeerId(usize::MAX);
        }
        t.peers.push(PeerRecord {
            peer,
            grant: grant.id.clone(),
            label: grant.label.clone(),
            path,
            opened: Utc::now(),
            closed: None,
            last_seen: None,
            connections: 0,
            bytes_to_peer: 0,
            bytes_from_peer: 0,
        });
        PeerId(t.peers.len() - 1)
    }

    /// A peer's path can change under it: iroh starts on a relay and moves to a
    /// direct path when hole punching lands. The receipt should say what
    /// actually carried the bytes, so the transport corrects this when it sees
    /// the path change.
    /// Record an actual observation of how a peer's bytes are travelling.
    ///
    /// Only ever called with something a transport really saw. The first one
    /// wins outright; after that only the weaker claim sticks, because a
    /// connection that spent any time on a relay is a connection that used one,
    /// and rounding that off would be flattering.
    pub fn peer_path(&self, id: PeerId, path: Path) {
        if let Some(p) = self.tally().peers.get_mut(id.0) {
            match p.path {
                None => p.path = Some(path),
                Some(Path::Direct) if path == Path::Relayed => p.path = Some(Path::Relayed),
                Some(_) => {}
            }
        }
    }

    /// The last moment this peer was seen doing anything.
    ///
    /// The tunnel has no connection lifecycle to hang `peer_left` on — a
    /// visitor is a grant, and their connections come and go — so without this
    /// `closed` stayed `None` and every tunnel peer rendered as held to the end
    /// of the share. Somebody who opened one page in minute two of a six-hour
    /// share was written down as having been connected for six hours.
    /// Somebody was turned away for a reason that has nothing to do with their
    /// ticket.
    ///
    /// These left no trace anywhere. A user behind a symmetric NAT running
    /// `--direct-only` — the flag this feature advertises as its strongest
    /// guarantee — watched `refused ... no direct path` scroll past their
    /// terminal and then got a receipt saying `peers none — the share was open
    /// but nobody connected`. Likewise every smuggling attempt the gate exists
    /// to refuse: answered with a `400` and forgotten, in the one lane whose
    /// job is to say who was turned away.
    pub fn record_turned_away(&self, why: TurnedAwayReason) {
        let mut t = self.tally();
        // Bounded like the denial list, and for the same reason.
        if t.turned_away.len() < 1024 {
            t.turned_away.push(TurnedAway {
                at: Utc::now(),
                why,
            });
        }
    }

    pub fn peer_seen(&self, id: PeerId) {
        if let Some(p) = self.tally().peers.get_mut(id.0) {
            p.last_seen = Some(Utc::now());
        }
    }

    pub fn peer_connection(&self, id: PeerId) {
        if let Some(p) = self.tally().peers.get_mut(id.0) {
            p.connections += 1;
        }
    }

    pub fn peer_bytes(&self, id: PeerId, to_peer: u64, from_peer: u64) {
        if let Some(p) = self.tally().peers.get_mut(id.0) {
            p.bytes_to_peer += to_peer;
            p.bytes_from_peer += from_peer;
        }
    }

    pub fn peer_left(&self, id: PeerId) {
        if let Some(p) = self.tally().peers.get_mut(id.0) {
            p.closed = Some(Utc::now());
        }
    }

    /// Someone knocked with no credential at all.
    ///
    /// `authorize` only ever sees a token that was presented, so without this
    /// the commonest probe of a public tunnel URL — a scanner fetching `/` —
    /// left the receipt completely silent about having been probed.
    pub fn record_refused(&self) {
        self.record_denied(Denied::NoCredential);
    }

    /// A response the box left unfinished.
    pub fn record_truncated(&self) {
        self.tally().truncated += 1;
    }

    /// Wait for every connection into the box to finish, or give up.
    ///
    /// Called between shutting the transport down and writing the receipt. The
    /// connection tasks are spawned and detached — closing the endpoint tells
    /// them to stop but does not wait for them — so without this the receipt is
    /// written while peers are still mid-copy, and their bytes and closing
    /// times are simply missing from it.
    ///
    /// Uses the capacity permits as the count of live connections, which they
    /// already are: each is released after its connection has recorded what it
    /// moved.
    /// Tell every connection the share is winding up.
    ///
    /// Separate from [`Self::quiesce`], and called before the transport is torn
    /// down, because the order is the whole point: `Endpoint::close` closes
    /// every connection with code `0` and an empty reason, so anything that
    /// wanted to close one with an *explanation* has to have done it already.
    /// Setting the flag inside `quiesce` — after the shutdown — meant the task
    /// that closes with a reason could never win, and the commit that added it
    /// was inert.
    pub fn begin_shutdown(&self) {
        let _ = self.shutdown.send_replace(true);
    }

    pub async fn quiesce(&self, within: std::time::Duration) {
        // Tell them to stop *before* waiting for them to. A connection carrying
        // a response with no declared length waits up to five minutes for the
        // box to go quiet; without this signal, a plain Ctrl-C would time out
        // waiting for it and write a receipt missing everything it carried.
        // `send_replace`, not `send`: tokio's `send` returns an error *without
        // storing the value* when no receiver is currently subscribed, and a
        // connection between accepting and its `select!` holds none. That made
        // the flag stay false for the rest of the process — intermittently, and
        // exactly on the path this exists for.
        let _ = self.shutdown.send_replace(true);
        let all = u32::try_from(MAX_CONNECTIONS).unwrap_or(u32::MAX);
        // The answer is kept. Two paths write a receipt with connections still
        // mid-copy — this timing out, and an interrupt that skips the wait
        // entirely — and in both the byte counts are short and the peers render
        // as still connected. The stderr line saying so is gone by the time
        // anybody reads the artifact, so the artifact has to say it itself.
        let settled = tokio::time::timeout(within, self.capacity.acquire_many(all))
            .await
            .is_ok();
        self.tally().settled = settled;
    }

    /// Record that the teardown did not wait for the connections at all.
    pub fn skipped_the_wait(&self) {
        self.tally().settled = false;
    }

    /// Resolves when the share is winding up. Connections select on it so a
    /// shutdown does not have to wait out their idle timeouts.
    pub async fn shutting_down(&self) {
        let mut rx = self.shutdown.subscribe();
        while !*rx.borrow_and_update() {
            if rx.changed().await.is_err() {
                return;
            }
        }
    }

    /// How many peers have connected. Used by tests and by the sharer's
    /// terminal line.
    pub fn peer_count(&self) -> usize {
        self.tally().peers.len()
    }

    /// Write the session into the box's receipt log.
    ///
    /// Called once, when the share ends. A share is a foreground command that
    /// usually ends on Ctrl-C, so the transports install a signal handler and
    /// call this on the way out: a receipt that only got written on a clean
    /// shutdown would be missing from exactly the sessions people actually run.
    pub fn write_receipt(&self) {
        self.write_receipt_with(0);
    }

    /// The same, for a share that ended badly.
    ///
    /// The code was a constant `0` on every path, including the one where
    /// `cloudflared` died and the error is "the public URL for this share is
    /// gone". The receipt said the session succeeded, `signals()` did not count
    /// it among the failures, and the export table showed a zero. It is a
    /// constant because leaving it unset renders as "signal", which reads as a
    /// kill — the answer to that is a code for the failures, not a code for
    /// everything.
    pub fn write_receipt_failed(&self) {
        self.write_receipt_with(1);
    }

    fn write_receipt_with(&self, exit_code: i32) {
        let ended = Utc::now();
        let seconds = (ended - self.started).num_seconds().max(0);
        // Snapshotted, and the lock let go before anything is written. Held
        // across `receipt::append` — a file write plus a redaction scan over
        // the whole body — it blocks any connection still trying to record what
        // it moved, which is the last thing that should be losing a race with
        // the receipt.
        let summary = self.summarise(ended);
        let peers_seen = summary.peers.len() as u64 + summary.peers_overflow;
        let body = render_receipt(&summary);
        let input = h5i_core::receipt::RecordInput {
            env_id: self.env_id.clone(),
            policy_digest: Some(self.policy_digest.clone()),
            // Its own lane. Not a command the box ran, not something the box
            // claimed: something h5i did to the box's front door.
            source: "share".into(),
            cmd: Some(format!(
                "h5i box share {}{} (port {}, {} peer(s), {seconds}s)",
                self.box_name,
                match self.transport {
                    Transport::P2p => String::new(),
                    Transport::Tunnel => " --tunnel".into(),
                },
                self.dialer.port(),
                // Plus the ones past the record cap. The body learned to say
                // so a round ago; this line, which is what a receipt listing
                // shows, did not.
                peers_seen
            )),
            wall_ms: u64::try_from(seconds * 1000).ok(),
            // A share ends when it is asked to. Left unset, the receipt viewer
            // renders "signal" for it, which reads in an export as though the
            // session had been killed.
            exit_code: Some(exit_code),
            ..Default::default()
        };
        if let Err(e) = h5i_core::receipt::append(&self.env_dir, input, body.as_bytes()) {
            eprintln!("share: could not record the session: {e}");
        }
    }
}

/// The grant a peer presented, resolved.
#[derive(Debug, Clone)]
pub struct AuthorizedGrant {
    pub id: String,
    pub label: Option<String>,
    pub expires_at: i64,
}

/// Everything the receipt body is rendered from. Split out so the rendering can
/// be tested without a bridge, a box or a network.
pub struct Summary {
    pub transport: Transport,
    pub endpoint: String,
    pub port: u16,
    pub started: DateTime<Utc>,
    pub ended: DateTime<Utc>,
    pub peers: Vec<PeerRecord>,
    /// Peers past what the record list holds.
    pub peers_overflow: u64,
    pub denied: Vec<DeniedAttempt>,
    /// Refusals past what the list holds.
    pub denied_overflow: u64,
    /// Connections refused because the share was already at its limit.
    pub over_capacity: u64,
    /// Connections refused at the front door, before any credential.
    pub front_refused: u64,
    /// Authorized peers who found nothing listening inside the box.
    pub unreachable: u64,
    /// Dials that failed because the route into the box did, rather than
    /// because nothing was listening on the shared port.
    pub route_broken: u64,
    /// Whether every connection had finished when this was written. When it is
    /// false the byte counts are short and the peers may render as still
    /// connected, and the receipt says so rather than reading as complete.
    pub settled: bool,
    /// Connections turned away for a reason that is not about a credential:
    /// `--direct-only` with no direct path, a greeting that was not one, a
    /// request the gate would not parse.
    pub turned_away: Vec<TurnedAway>,
    /// Responses the box left unfinished.
    pub truncated: u64,
}

fn plural(n: u64, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

/// Render the ingress receipt.
///
/// Written for someone reading an export weeks later who has to decide whether
/// this patch was produced in a room they understand. So it answers, in order:
/// was this box shared, over what, with whom, for how long, how much moved, and
/// did anyone try who should not have.
pub fn render_receipt(s: &Summary) -> String {
    let seconds = (s.ended - s.started).num_seconds().max(0);
    let mut out = String::new();
    out.push_str(&format!(
        "share session, {seconds}s ({} transport)\n",
        s.transport.as_str()
    ));
    out.push_str(&format!("opened   {}\n", s.started.to_rfc3339()));
    out.push_str(&format!("closed   {}\n", s.ended.to_rfc3339()));
    out.push_str(&format!(
        "shared   port {} inside the box, never published on the host\n",
        s.port
    ));
    out.push_str(&format!("endpoint {}\n", s.endpoint));
    if s.transport == Transport::Tunnel {
        // Said in the receipt, not only in the docs. Whoever reads this later
        // is exactly the person who needs to know a third party could read the
        // traffic, and they will not be re-reading MANUAL.md to find out.
        out.push_str(
            "note     a Cloudflare quick tunnel terminated TLS, so this traffic was not \
             end-to-end encrypted\n",
        );
    }

    if !s.settled {
        // Said before the numbers, not after, because it changes how to read
        // every one of them.
        out.push_str(
            "partial  this was written before every connection had finished, so the byte \
             counts below are short and a peer may read as still connected\n",
        );
    }

    if s.peers.is_empty() {
        out.push_str("peers    none — the share was open but nobody connected\n");
    } else {
        out.push_str(&format!("peers    {}\n", s.peers.len()));
        for p in &s.peers {
            // `closed` where a transport has a connection lifecycle, the last
            // time we saw them where it does not, and only then the end of the
            // share — which is the honest reading of "still there when it
            // ended" rather than a default that quietly applies to everyone.
            let closed = p.closed.or(p.last_seen).unwrap_or(s.ended);
            let held = (closed - p.opened).num_seconds().max(0);
            out.push_str(&format!(
                "  {} via {} — grant {}{}, {held}s, {}, {} in / {} out\n",
                p.peer,
                p.path
                    .map(|x| x.as_str())
                    .unwrap_or("a path nothing observed"),
                p.grant,
                p.label
                    .as_ref()
                    .map(|l| format!(" ({l})"))
                    .unwrap_or_default(),
                plural(p.connections, "connection", "connections"),
                p.bytes_from_peer,
                p.bytes_to_peer,
            ));
        }
        if s.peers_overflow > 0 {
            out.push_str(&format!(
                "  and {} more peer(s), past the {MAX_PEER_RECORDS} this receipt lists \
                 individually — their traffic is not counted above\n",
                s.peers_overflow
            ));
        }
        let relayed = s
            .peers
            .iter()
            .filter(|p| p.path == Some(Path::Relayed))
            .count();
        if relayed > 0 {
            out.push_str(&format!(
                "relay    {relayed} peer(s) used a relay. It moved sealed packets and could not \
                 read them.\n"
            ));
        }
    }

    if s.over_capacity > 0 {
        // Worth its own line rather than folding into "refused": these were
        // turned away for load, not for credentials, and the two mean opposite
        // things about what happened here.
        out.push_str(&format!(
            "capacity {} connection(s) refused because {MAX_CONNECTIONS} were already \
             reaching the box\n",
            s.over_capacity
        ));
    }

    if s.front_refused > 0 {
        // Its own line, and deliberately not added to the one above. This one
        // costs an anonymous flooder a TCP connect, so it can be driven into
        // the millions — and a reader who saw that number under "capacity"
        // would take it as the box's ceiling having been hit that many times.
        out.push_str(&format!(
            "flooded  {} connection(s) refused at the front door, before any credential was \
             asked for\n",
            s.front_refused
        ));
    }

    if s.truncated > 0 {
        out.push_str(&format!(
            "truncated {} response(s) the box left unfinished, so what the visitor got was \
             incomplete\n",
            s.truncated
        ));
    }

    if !s.turned_away.is_empty() {
        // Its own line, not folded into `refused`, because none of these is
        // about a credential. A `--direct-only` refusal in particular is the
        // feature working exactly as advertised, and it used to leave the
        // receipt saying nobody connected while the operator watched the
        // refusals scroll past their terminal.
        let mut kinds: Vec<(TurnedAwayReason, usize)> = Vec::new();
        for t in &s.turned_away {
            match kinds.iter_mut().find(|(k, _)| *k == t.why) {
                Some((_, n)) => *n += 1,
                None => kinds.push((t.why, 1)),
            }
        }
        let listed: Vec<String> = kinds
            .iter()
            .map(|(k, n)| format!("{n} {}", k.as_str()))
            .collect();
        out.push_str(&format!(
            "turned   {} connection(s) away before any ticket was weighed: {}\n",
            s.turned_away.len(),
            listed.join(", ")
        ));
    }

    if s.route_broken > 0 {
        // Deliberately not folded into the line below. That one is a sentence
        // about the user's dev server, and this is a sentence about h5i: the
        // route into the box failed, which is sticky once it does, so a reader
        // told "nothing was listening" would go and check a server that was
        // running fine the whole time.
        out.push_str(&format!(
            "route    {} connection(s) were authorized and then h5i could not reach the box \
             at all\n",
            s.route_broken
        ));
    }

    if s.unreachable > 0 {
        // A peer with a good ticket that found nothing listening is a fact
        // about the box, not about the peer, and it is the difference between
        // "nobody came" and "somebody came and got an error".
        out.push_str(&format!(
            "unreached {} connection(s) were authorized but found nothing listening on port {}\n",
            s.unreachable, s.port
        ));
    }

    if !s.denied.is_empty() {
        let unknown = s
            .denied
            .iter()
            .filter(|d| d.reason == Denied::Unknown)
            .count();
        let expired = s
            .denied
            .iter()
            .filter(|d| d.reason == Denied::Expired)
            .count();
        let revoked = s
            .denied
            .iter()
            .filter(|d| d.reason == Denied::Revoked)
            .count();
        let none = s
            .denied
            .iter()
            .filter(|d| d.reason == Denied::NoCredential)
            .count();
        // The leading number counts every refusal, not just the ones kept
        // individually. It read `refused 1024 attempt(s)` for fifty thousand,
        // with the truth in a trailing clause — and the sub-counts are over the
        // recorded sample, so their sum does not match and the line now says so.
        out.push_str(&format!(
            "refused  {} attempt(s){}: of the {} recorded, {none} presented no invite, \
             {unknown} an unknown ticket, {expired} expired, {revoked} revoked\n",
            s.denied.len() as u64 + s.denied_overflow,
            if s.denied_overflow > 0 {
                format!(" ({} of them not recorded individually)", s.denied_overflow)
            } else {
                String::new()
            },
            s.denied.len()
        ));
    }
    out
}

/// The grant table as `h5i box share status` shows it.
pub fn render_status(s: &ShareSession, now: i64) -> String {
    let mut out = String::new();
    let live = session::is_live(s);
    out.push_str(&format!(
        "{} — sharing port {} over {}\n",
        s.box_id,
        s.port,
        s.transport.as_str()
    ));
    out.push_str(&format!("  endpoint  {}\n", s.endpoint));
    out.push_str(&format!("  started   {}\n", s.started_at));
    out.push_str(&format!(
        "  process   pid {}{}\n",
        s.pid,
        if live {
            ""
        } else {
            " — GONE. This share is not serving anything; run `h5i box share stop`."
        }
    ));
    if s.grants.is_empty() {
        out.push_str("  grants    none\n");
        return out;
    }
    out.push_str("  grants\n");
    for g in &s.grants {
        let state = if g.revoked {
            "revoked".to_string()
        } else if g.expires_at <= now {
            "expired".to_string()
        } else {
            format!("{} left", crate::session::humanise(g.expires_at - now))
        };
        out.push_str(&format!(
            "    {}  {:<10}{}\n",
            g.id,
            state,
            g.label
                .as_ref()
                .map(|l| format!("  {l}"))
                .unwrap_or_default()
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bridge over a temp directory, for the accounting tests. Uses the
    /// no-namespace dialer: nothing here opens a connection.
    fn test_bridge(dir: &std::path::Path) -> Bridge {
        Bridge::new(
            dir.to_path_buf(),
            "env/test/demo".into(),
            "digest".into(),
            "demo".into(),
            Transport::P2p,
            "local".into(),
            crate::dialer::Dialer::spawn_local(1).expect("dialer"),
        )
    }

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).expect("timestamp").into()
    }

    fn peer(name: &str, path: Path) -> PeerRecord {
        PeerRecord {
            peer: name.into(),
            grant: "a1b2c3d4".into(),
            label: Some("alex".into()),
            path: Some(path),
            opened: at("2026-08-10T10:00:00Z"),
            closed: Some(at("2026-08-10T10:05:00Z")),
            last_seen: None,
            connections: 12,
            bytes_to_peer: 5_000,
            bytes_from_peer: 900,
        }
    }

    fn summary(peers: Vec<PeerRecord>, denied: Vec<DeniedAttempt>) -> Summary {
        Summary {
            transport: Transport::P2p,
            endpoint: "abcdef".into(),
            port: 3000,
            started: at("2026-08-10T10:00:00Z"),
            ended: at("2026-08-10T10:10:00Z"),
            peers,
            peers_overflow: 0,
            denied,
            denied_overflow: 0,
            over_capacity: 0,
            front_refused: 0,
            unreachable: 0,
            route_broken: 0,
            settled: true,
            turned_away: Vec::new(),
            truncated: 0,
        }
    }

    #[test]
    fn the_receipt_names_the_peer_the_path_and_the_traffic() {
        let body = render_receipt(&summary(vec![peer("kbcd…", Path::Direct)], vec![]));
        assert!(body.contains("share session, 600s (p2p transport)"));
        assert!(body.contains("port 3000 inside the box, never published"));
        assert!(body.contains("kbcd… via direct"));
        assert!(body.contains("grant a1b2c3d4 (alex)"));
        assert!(body.contains("12 connections"));
        assert!(body.contains("900 in / 5000 out"));
    }

    #[test]
    fn a_knock_with_no_invite_is_not_an_unknown_ticket() {
        // On a public tunnel URL a scanner fetching `/` is the commonest event
        // of the whole session, and folding it into "unknown ticket" made the
        // dominant row of the receipt a sentence that was usually false.
        let mut s = summary(vec![], vec![]);
        s.denied = vec![
            DeniedAttempt {
                at: at("2026-08-10T10:01:00Z"),
                reason: Denied::NoCredential,
            },
            DeniedAttempt {
                at: at("2026-08-10T10:02:00Z"),
                reason: Denied::Unknown,
            },
        ];
        let body = render_receipt(&s);
        assert!(body.contains("1 presented no invite"), "{body}");
        assert!(body.contains("1 an unknown ticket"), "{body}");
    }

    #[test]
    fn a_broken_route_is_not_the_user_s_dev_server_being_down() {
        // `unreached ... found nothing listening on port 3000` is a sentence
        // about somebody's dev server. It was printed for a broken dialer too,
        // and that case is sticky — so one lost reply produced a receipt
        // asserting hundreds of times that a server was down while it ran.
        let mut s = summary(vec![], vec![]);
        s.route_broken = 3;
        s.unreachable = 0;
        let body = render_receipt(&s);
        assert!(body.contains("could not reach the box at all"), "{body}");
        assert!(!body.contains("nothing listening"), "{body}");
    }

    #[test]
    fn a_connection_turned_away_before_any_ticket_leaves_a_trace() {
        // A user behind a symmetric NAT running `--direct-only` — the flag this
        // feature advertises as its strongest guarantee — watched the refusals
        // scroll past their terminal and then got a receipt saying nobody
        // connected.
        let mut s = summary(vec![], vec![]);
        s.turned_away = vec![
            TurnedAway {
                at: at("2026-08-10T10:01:00Z"),
                why: TurnedAwayReason::NoDirectPath,
            },
            TurnedAway {
                at: at("2026-08-10T10:02:00Z"),
                why: TurnedAwayReason::NoDirectPath,
            },
            TurnedAway {
                at: at("2026-08-10T10:03:00Z"),
                why: TurnedAwayReason::Unparseable,
            },
        ];
        let body = render_receipt(&s);
        assert!(body.contains("turned   3 connection(s) away"), "{body}");
        assert!(body.contains("2 no direct path was available"), "{body}");
        assert!(
            body.contains("1 sent something this share would not parse"),
            "{body}"
        );
    }

    #[test]
    fn a_receipt_written_before_the_connections_finished_says_so() {
        // Two paths reach here — the quiesce timing out, and an interrupt that
        // skips the wait — and in both the byte counts are short. The stderr
        // line saying so is gone by the time anyone reads the artifact.
        let mut s = summary(vec![], vec![]);
        s.settled = false;
        let body = render_receipt(&s);
        assert!(
            body.contains("written before every connection had finished"),
            "{body}"
        );

        s.settled = true;
        assert!(!render_receipt(&s).contains("written before every"));
    }

    #[test]
    fn a_tunnel_peer_is_not_held_to_the_end_of_the_share() {
        // The tunnel has no close to observe, so `closed` stayed `None` and
        // every peer rendered as connected until the share ended: somebody who
        // opened one page in minute two of a six-hour share was written down as
        // having been there for six hours.
        let mut p = peer("alex", Path::Tunnel);
        p.opened = at("2026-08-10T10:01:00Z");
        p.closed = None;
        p.last_seen = Some(at("2026-08-10T10:02:00Z"));
        let body = render_receipt(&summary(vec![p], vec![]));
        assert!(body.contains("60s"), "{body}");
        assert!(
            !body.contains("540s"),
            "the peer was held to the end anyway: {body}"
        );
    }

    #[test]
    fn status_says_seconds_when_seconds_is_what_is_left() {
        // `announce` was fixed to stop rendering a 45-second ticket as "0m",
        // and `status` — the view somebody actually consults before re-minting
        // — was left on integer minutes, one column away from "expired".
        let now = chrono::Utc::now().timestamp();
        let mut s = crate::session::ShareSession::new(
            "env/a/demo",
            3000,
            Transport::Tunnel,
            "https://x",
            chrono::Utc::now(),
        );
        let (mut g, _) = crate::session::mint_grant(None, now + 45).expect("mint");
        g.expires_at = now + 45;
        s.grants = vec![g];
        let out = render_status(&s, now);
        assert!(out.contains("45s left"), "{out}");
        assert!(!out.contains("0m"), "{out}");
    }

    #[tokio::test]
    async fn winding_up_is_told_to_the_connections_before_anything_waits_on_them() {
        // The order is the whole point, and it was wrong for a round: the flag
        // used to be set inside `quiesce`, after the transport had already
        // closed every connection with an empty reason, so the task that closes
        // one with an explanation could never win.
        let dir = tempfile::tempdir().expect("tempdir");
        let b = std::sync::Arc::new(test_bridge(dir.path()));
        let watcher = {
            let b = b.clone();
            tokio::spawn(async move { b.shutting_down().await })
        };
        // Not yet. An actual sleep, so the spawned watcher is really given a
        // chance to run: `timeout(_, async {})` returns without yielding at
        // all, so on a current-thread runtime the watcher had never been polled
        // and `is_finished()` was false whatever `shutting_down` did.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!watcher.is_finished());

        b.begin_shutdown();
        tokio::time::timeout(std::time::Duration::from_secs(2), watcher)
            .await
            .expect("shutting_down did not fire")
            .expect("the watcher task");

        // And a receiver that subscribes *after* the flag is set returns at
        // once, which is what a connection accepted during shutdown needs.
        tokio::time::timeout(std::time::Duration::from_secs(2), b.shutting_down())
            .await
            .expect("a late subscriber waited forever");
    }

    #[test]
    fn a_receipt_with_everything_in_it_still_reads_as_sentences() {
        // The receipt is this feature's evidence artifact and the counters were
        // added one round at a time. This is the only place they are all seen
        // together, which is where a line that reads as noise would show up.
        let mut s = summary(
            vec![
                peer("kbcd…", Path::Direct),
                peer("a browser", Path::Relayed),
            ],
            vec![
                DeniedAttempt {
                    at: at("2026-08-10T10:01:00Z"),
                    reason: Denied::Unknown,
                },
                DeniedAttempt {
                    at: at("2026-08-10T10:02:00Z"),
                    reason: Denied::Revoked,
                },
            ],
        );
        s.transport = Transport::Tunnel;
        s.peers_overflow = 3;
        s.denied_overflow = 12;
        s.over_capacity = 5;
        s.front_refused = 900_000;
        s.unreachable = 2;
        s.truncated = 1;
        let body = render_receipt(&s);

        // Past the label column, which is aligned on purpose, a run of spaces
        // means a string continuation was broken — which has happened twice.
        for line in body.lines() {
            let text = line.get(10..).unwrap_or("");
            assert!(!text.contains("  "), "a run of spaces in: {line:?}");
            assert!(line.len() < 200, "a line nobody will read: {line:?}");
        }
        for expected in [
            "share session, 600s (tunnel transport)",
            "not end-to-end encrypted",
            "peers    2",
            "and 3 more peer(s)",
            "relay    1 peer(s) used a relay",
            "capacity 5 connection(s) refused",
            "flooded  900000 connection(s) refused at the front door",
            "unreached 2 connection(s) were authorized",
            "truncated 1 response(s) the box left unfinished",
            "refused  14 attempt(s)",
            "(12 of them not recorded individually)",
        ] {
            assert!(body.contains(expected), "missing {expected:?} in:\n{body}");
        }
    }

    #[test]
    fn a_peer_who_arrived_and_read_nothing_is_a_peer_with_no_connections() {
        // Following the invite link is a fact worth recording, and it is a
        // different fact from reaching the dev server. The receipt says the
        // second thing, so a redirect must not be counted as one.
        let mut p = peer("a browser", Path::Tunnel);
        p.connections = 0;
        p.bytes_to_peer = 190;
        p.bytes_from_peer = 0;
        let body = render_receipt(&summary(vec![p], vec![]));
        assert!(body.contains("0 connections"), "{body}");
        assert!(!body.contains("nobody connected"), "{body}");
    }

    #[test]
    fn a_share_nobody_used_says_so_rather_than_saying_nothing() {
        let body = render_receipt(&summary(vec![], vec![]));
        assert!(body.contains("nobody connected"));
    }

    #[test]
    fn using_a_relay_is_recorded_as_using_a_relay() {
        let body = render_receipt(&summary(vec![peer("kbcd…", Path::Relayed)], vec![]));
        assert!(body.contains("via relayed"));
        assert!(body.contains("could not read them"));
    }

    #[test]
    fn the_tunnels_weaker_claim_is_in_the_receipt_not_only_the_docs() {
        let mut s = summary(vec![], vec![]);
        s.transport = Transport::Tunnel;
        let body = render_receipt(&s);
        assert!(body.contains("not end-to-end encrypted"));
    }

    #[test]
    fn being_knocked_on_is_evidence_and_is_kept() {
        let denied = vec![
            DeniedAttempt {
                at: at("2026-08-10T10:01:00Z"),
                reason: Denied::Unknown,
            },
            DeniedAttempt {
                at: at("2026-08-10T10:02:00Z"),
                reason: Denied::Unknown,
            },
            DeniedAttempt {
                at: at("2026-08-10T10:03:00Z"),
                reason: Denied::Revoked,
            },
        ];
        let body = render_receipt(&summary(vec![], denied));
        assert!(body.contains("refused  3 attempt(s)"));
        assert!(body.contains("2 an unknown ticket"), "{body}");
        assert!(body.contains("1 revoked"));
    }

    #[test]
    fn more_peers_than_the_receipt_lists_is_said_rather_than_dropped() {
        // The same failure the denial list already had a test for: a cap that
        // stops counting makes a busy share read as a quiet one.
        let mut s = summary(vec![peer("kbcd…", Path::Direct)], vec![]);
        s.peers_overflow = 41;
        let body = render_receipt(&s);
        assert!(body.contains("and 41 more peer(s)"), "{body}");
        assert!(body.contains("not counted above"), "{body}");
    }

    #[test]
    fn a_share_knocked_on_more_than_the_list_holds_says_so() {
        // The cap used to claim in a comment that the overflow "still shows up
        // in the summary". It did not: 50,000 attempts reported as 1024.
        let mut s = summary(
            vec![],
            (0..3)
                .map(|_| DeniedAttempt {
                    at: at("2026-08-10T10:01:00Z"),
                    reason: Denied::Unknown,
                })
                .collect(),
        );
        s.denied_overflow = 48_976;
        let body = render_receipt(&s);
        // The leading number is every refusal, not just the recorded sample:
        // it read "refused 1024 attempt(s)" for fifty thousand of them, with
        // the truth demoted to a trailing clause.
        assert!(body.contains("refused  48979 attempt(s)"), "{body}");
        assert!(body.contains("48976 of them not recorded"), "{body}");
    }

    #[test]
    fn a_good_ticket_that_found_nothing_listening_is_not_silence() {
        // Otherwise a share where the dev server was down renders as one
        // nobody ever tried to use.
        let mut s = summary(vec![], vec![]);
        s.unreachable = 4;
        let body = render_receipt(&s);
        assert!(body.contains("4 connection(s) were authorized"), "{body}");
        assert!(body.contains("nothing listening on port 3000"), "{body}");
    }

    #[test]
    fn the_capacity_line_reads_as_a_sentence() {
        // It was written with a broken string continuation once, and a receipt
        // with a run of spaces through it is a receipt somebody stops trusting.
        let mut s = summary(vec![], vec![]);
        s.over_capacity = 1;
        let body = render_receipt(&s);
        assert!(!body.contains("  of "), "{body}");
        assert!(body.contains("64 were already reaching the box"), "{body}");
    }

    #[test]
    fn hitting_the_ceiling_is_recorded_as_load_not_as_a_bad_ticket() {
        let mut s = summary(vec![], vec![]);
        s.over_capacity = 7;
        let body = render_receipt(&s);
        assert!(body.contains("capacity 7 connection(s) refused"), "{body}");
        assert!(
            !body.contains("refused  "),
            "load must not read as a credential failure"
        );
    }

    #[test]
    fn a_peer_still_connected_when_the_share_ends_is_counted_to_the_end() {
        let mut p = peer("kbcd…", Path::Direct);
        p.closed = None;
        let body = render_receipt(&summary(vec![p], vec![]));
        assert!(body.contains("600s"), "{body}");
    }

    #[test]
    fn status_flags_a_share_whose_process_is_gone() {
        // The failure this catches: a share.json left by a crash reads exactly
        // like a live share, and someone hands out a ticket nothing will answer.
        let mut s = ShareSession::new("env/a/demo", 3000, Transport::P2p, "abc", Utc::now());
        s.pid = 0;
        let out = render_status(&s, 0);
        assert!(out.contains("GONE"));
        assert!(out.contains("share stop"));
    }

    #[test]
    fn status_tells_the_three_states_of_a_grant_apart() {
        let mut s = ShareSession::new("env/a/demo", 3000, Transport::P2p, "abc", Utc::now());
        let (mut live, _) = session::mint_grant(Some("alex".into()), 3_600).unwrap();
        live.id = "aaaaaaaa".into();
        let (mut gone, _) = session::mint_grant(None, 10).unwrap();
        gone.id = "bbbbbbbb".into();
        let (mut cut, _) = session::mint_grant(None, 3_600).unwrap();
        cut.id = "cccccccc".into();
        cut.revoked = true;
        s.grants = vec![live, gone, cut];
        let out = render_status(&s, 600);
        assert!(out.contains("aaaaaaaa  50m left"), "{out}");
        assert!(out.contains("bbbbbbbb  expired"), "{out}");
        assert!(out.contains("cccccccc  revoked"), "{out}");
        assert!(out.contains("alex"));
    }

    #[test]
    fn a_peer_past_the_record_cap_changes_nothing_rather_than_the_last_one() {
        // The overflow handle used to be the *last* record's, so peer 257's
        // bytes, connections and path landed on peer 256 — including giving it
        // a path nothing had observed.
        let dir = tempfile::tempdir().expect("tempdir");
        let b = test_bridge(dir.path());
        let g = AuthorizedGrant {
            id: "a1b2c3d4".into(),
            label: None,
            expires_at: 4_000_000_000,
        };
        let mut overflow = None;
        for i in 0..MAX_PEER_RECORDS + 8 {
            overflow = Some(b.peer_joined(format!("peer{i}"), &g, None));
        }
        assert_eq!(b.peer_count(), MAX_PEER_RECORDS);
        let last_before = b.tally.lock().unwrap().peers[MAX_PEER_RECORDS - 1].clone();

        let id = overflow.expect("a handle");
        b.peer_connection(id);
        b.peer_bytes(id, 1000, 2000);
        b.peer_path(id, Path::Relayed);
        b.peer_left(id);

        let after = b.tally.lock().unwrap().peers[MAX_PEER_RECORDS - 1].clone();
        assert_eq!(after.connections, last_before.connections);
        assert_eq!(after.bytes_to_peer, last_before.bytes_to_peer);
        assert_eq!(after.path, last_before.path);
        assert!(after.closed.is_none());
    }

    #[test]
    fn a_path_nobody_has_seen_yet_is_replaced_by_the_first_one_that_is() {
        // The guess made at join time used to be permanent in the optimistic
        // direction: `peer_path` only ever downgraded, so a peer whose path had
        // simply not been selected yet was recorded as direct for good.
        let dir = tempfile::tempdir().expect("tempdir");
        let b = test_bridge(dir.path());
        let g = AuthorizedGrant {
            id: "a1b2c3d4".into(),
            label: None,
            expires_at: 4_000_000_000,
        };
        let id = b.peer_joined("kbcd…".into(), &g, None);
        assert_eq!(b.tally.lock().unwrap().peers[0].path, None);
        b.peer_path(id, Path::Relayed);
        assert_eq!(b.tally.lock().unwrap().peers[0].path, Some(Path::Relayed));

        // And once observed, only the weaker claim sticks.
        b.peer_path(id, Path::Direct);
        assert_eq!(b.tally.lock().unwrap().peers[0].path, Some(Path::Relayed));
    }

    #[test]
    fn a_path_that_fell_back_to_a_relay_is_not_rounded_up_to_direct() {
        // Recorded honestly in the one direction that matters: a connection
        // that spent any time relayed used a relay, and the receipt must not
        // flatter it once a direct path appears later.
        let mut p = peer("kbcd…", Path::Relayed);
        p.path = Some(Path::Relayed);
        let body = render_receipt(&summary(vec![p], vec![]));
        assert!(body.contains("relayed"));
    }
}
