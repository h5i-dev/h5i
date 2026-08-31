//! The bridge: everything a share does that is not about how the bytes travel.
//!
//! Both transports sit on this. It holds the three jobs that touch the
//! boundary, and holding them in one place is the point. A second transport
//! must not be a second chance to get authorization or evidence wrong:
//!
//! * Reach the dev server, through [`crate::dialer`], pinned to one port of
//!   one box for the bridge's whole life.
//! * Hold the capability, by resolving a presented secret against the grant
//!   table on disk ([`crate::session`]) on *every* connection, so a revoke
//!   written by another process takes effect on the next one.
//! * Write the ingress receipt. Every other lane in a box's receipt
//!   observes what left. This is the first that records what came in: who
//!   connected, when, over what path, how much, and who was turned away.
//!
//! The receipt lane is host observed in the strongest sense available. H5i
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
    /// Through a relay. Still end-to-end encrypted, the relay moves sealed
    /// packets and cannot read them, but a third party was on the wire and the
    /// receipt says so.
    Relayed,
    /// A Cloudflare quick tunnel. TLS terminates at Cloudflare, so unlike the
    /// two above this one is *not* end to end.
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
    /// because one connection is all a request gets, so this is also the count
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
    /// A browser request that came from a page that is not this share, carrying
    /// the share cookie the browser attached on its own. The gate refuses it
    /// because the page had no business making it.
    ForeignOrigin,
    /// An attempt to register a service worker, which would keep control of
    /// the joiner's loopback origin after the share ended.
    ServiceWorker,
}

impl TurnedAwayReason {
    fn as_str(self) -> &'static str {
        match self {
            TurnedAwayReason::NoDirectPath => "no direct path was available",
            TurnedAwayReason::NeverGreeted => "connected but never presented a ticket",
            TurnedAwayReason::Unparseable => "sent something this share would not parse",
            TurnedAwayReason::ForeignOrigin => {
                "came from another page, with this share's cookie attached"
            }
            TurnedAwayReason::ServiceWorker => {
                "tried to register a service worker, which would outlive the share"
            }
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
    /// See [`Summary::turned_away_overflow`].
    turned_away_overflow: u64,
    /// See [`Summary::failed_because`].
    failed_because: Option<String>,
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
    /// inflatable one, a flooder can drive this into the millions, so it gets
    /// its own line rather than being added to a number a reader would take as
    /// the box's ceiling having been hit.
    front_refused: u64,
    /// Peers past what the record list holds. Counted, like the denial list's
    /// overflow, because a receipt that stops at 256 and says nothing is a
    /// receipt that reports a share nobody could read as a share nobody used.
    peers_overflow: u64,
    /// Connections and bytes belonging to peers past the record cap. See
    /// [`Bridge::peer_connection`].
    overflow_connections: u64,
    overflow_bytes_to_peer: u64,
    overflow_bytes_from_peer: u64,
    /// Peers who presented a good ticket and found nothing listening inside the
    /// box. Without this a share where the dev server was down reads as one
    /// nobody ever tried to use.
    unreachable: u64,
    /// Responses the box left unfinished: short of a `Content-Length` it
    /// declared, or an unframed stream it stopped feeding without closing. A
    /// truncated download reads to the visitor as the app being broken, so the
    /// receipt says which it was, and a visitor who cancelled a download is
    /// deliberately not counted here.
    truncated: u64,
}

/// How many connections into the box a share will carry at once.
///
/// A share is a door on the open internet in tunnel mode, and an iroh endpoint
/// anyone may dial in P2P mode. Without a ceiling, a peer holding a valid link,
/// or a page on the shared app opening sockets in a loop, turns into unbounded
/// tasks on the host and unbounded sockets into the box, which is a denial of
/// service against the box the share was meant to show off.
///
/// Sixty-four is chosen to be uninteresting: a browser opens about six
/// connections per origin, so this is roughly ten simultaneous viewers, and a
/// share is for one person. Reaching it is a signal, and it is recorded.
const MAX_CONNECTIONS: usize = 64;

/// Ceiling on accepted-but-not-yet-authorized handlers, across both
/// transports. Above either transport's own front-door limit (256 each), so it
/// never becomes the binding constraint. It exists to be *waited on*, not to
/// refuse anybody. See [`Bridge::enter_front`].
const MAX_FRONT: usize = 4096;

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
/// Which `share.json` a serving process is the server of.
///
/// A bridge reads that file on every connection (to authorize, to check a
/// grant, to notice the share ending) and it was reading whatever file was at
/// that path, not the one it wrote. `share stop --force` exists precisely to
/// delete a live record without stopping its process, relying on that process
/// to notice within a second, and a new `h5i box share` may legitimately claim
/// the now-empty path inside that second.
///
/// What the old bridge did next: read the *replacement* record, find a live
/// grant in it, and keep serving. A second transport into the old box, on the
/// old port, for the whole life of the new share, admitting the new share's
/// tickets. And when it eventually did exit, its unconditional
/// `begin_winding_up` marked the *new* share as winding up. `session::clear`
/// checks the pid, so it stopped the final delete and nothing else.
///
/// Identity is the pid plus the recorded start: a pid alone is reusable, and
/// two shares of one box a second apart are exactly the case that has to be
/// told apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedRecord {
    pub pid: u32,
    pub started_at: String,
}

impl ClaimedRecord {
    /// Is this the record we claimed?
    pub fn matches(&self, s: &session::ShareSession) -> bool {
        s.pid == self.pid && s.started_at == self.started_at
    }

    /// The identity of a record this process is about to serve.
    pub fn of(s: &session::ShareSession) -> ClaimedRecord {
        ClaimedRecord {
            pid: s.pid,
            started_at: s.started_at.clone(),
        }
    }

    /// The identity of whatever record is on disk right now.
    ///
    /// For a bridge built over a record it did not write, which is every test
    /// helper, and nothing in production, where `serve_async` claims first and
    /// pins what it claimed. A directory with no record yields an identity
    /// that matches nothing, which is the correct answer for a bridge with no
    /// share behind it.
    pub fn on_disk(env_dir: &std::path::Path) -> ClaimedRecord {
        session::read(env_dir)
            .map(|s| ClaimedRecord::of(&s))
            .unwrap_or(ClaimedRecord {
                pid: 0,
                started_at: String::new(),
            })
    }
}

pub struct Bridge {
    env_dir: std::path::PathBuf,
    env_id: String,
    policy_digest: String,
    box_name: String,
    transport: Transport,
    endpoint: String,
    dialer: Dialer,
    /// The record this process claimed. Every read of `share.json` is checked
    /// against it; see [`ClaimedRecord`].
    claimed: ClaimedRecord,
    started: DateTime<Utc>,
    /// The same instant on a clock nothing can move. Every expiry decision in
    /// this process is floored against it; see [`Bridge::now`].
    started_mono: std::time::Instant,
    /// What the wall clock has been seen doing. See [`SeenClock`].
    clock: Mutex<SeenClock>,
    tally: Mutex<Tally>,
    /// One permit per live connection into the box, held for its lifetime.
    capacity: Arc<tokio::sync::Semaphore>,
    /// One permit per *accepted* connection, from before it has a credential
    /// until its handler returns. See [`Bridge::enter_front`].
    front: Arc<tokio::sync::Semaphore>,
    /// Flipped when the share is winding up, so connections end promptly
    /// instead of being waited on.
    shutdown: tokio::sync::watch::Sender<bool>,
}

impl Bridge {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        env_dir: std::path::PathBuf,
        env_id: String,
        policy_digest: String,
        box_name: String,
        transport: Transport,
        endpoint: String,
        dialer: Dialer,
        claimed: ClaimedRecord,
    ) -> Bridge {
        Bridge {
            env_dir,
            env_id,
            policy_digest,
            box_name,
            transport,
            endpoint,
            dialer,
            claimed,
            started: Utc::now(),
            started_mono: std::time::Instant::now(),
            clock: Mutex::new(SeenClock::default()),
            tally: Mutex::new(Tally::default()),
            capacity: Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS)),
            front: Arc::new(tokio::sync::Semaphore::new(MAX_FRONT)),
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
    /// silently stopped counting after a poison while the other half carried on,
    /// which is the asymmetry the one that recovered was added to object to.
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
    /// to show a path did *not* take one. "A later stream still works" leaves
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
            seconds: self.started_mono.elapsed().as_secs() as i64,
            clock_moved: self.clock_stepped_by(),
            route_broken: t.route_broken,
            settled: t.settled,
            turned_away: t.turned_away.clone(),
            turned_away_overflow: t.turned_away_overflow,
            failed_because: t.failed_because.clone(),
            transport: self.transport,
            endpoint: self.endpoint.clone(),
            port: self.dialer.port(),
            started: self.started,
            ended,
            peers: t.peers.clone(),
            peers_overflow: t.peers_overflow,
            overflow_connections: t.overflow_connections,
            overflow_bytes_to_peer: t.overflow_bytes_to_peer,
            overflow_bytes_from_peer: t.overflow_bytes_from_peer,
            denied: t.denied.clone(),
            denied_overflow: t.denied_overflow,
            over_capacity: t.over_capacity,
            front_refused: t.front_refused,
            unreachable: t.unreachable,
            truncated: t.truncated,
        }
    }

    /// Now, but never earlier than the share's own start plus the time that
    /// has actually passed.
    ///
    /// Every expiry decision here was a bare `Utc::now()`, and the wall clock
    /// is not a clock a share can rely on: an NTP step after boot, a VM resumed
    /// from a snapshot, a dual-boot laptop whose RTC holds local time. Stepping
    /// a running share's clock back an hour was measured to extend *every* live
    /// grant by an hour of real time. Past what its holder was told, past what
    /// the sharer was told, with nothing anywhere recording that the clock had
    /// moved. A ticket is a promise about elapsed time and this is the only
    /// clock that measures it.
    ///
    /// Deliberately one-directional. A backward step is refused; a forward step
    /// is honoured, because expiring a ticket early is the safe direction and
    /// because a machine whose clock was genuinely wrong at start-up should be
    /// allowed to find that out.
    ///
    /// A *step*, not drift. The first version of this floored the wall clock
    /// against `started + elapsed` outright, which assumes the two clocks run
    /// at the same rate. Measured on the WSL2 host this was written on,
    /// `CLOCK_REALTIME` advances 56.9s per 60s of `CLOCK_MONOTONIC`: a five
    /// per cent difference, continuous, with nothing wrong. Under that floor a
    /// one-hour ticket died at fifty-seven minutes and a day-long one would
    /// lose over an hour, which is a promise broken in the other direction.
    /// [`SeenClock`] tells the two apart.
    /// Recovered rather than unwrapped, for the reason [`Self::tally`] gives and
    /// with more at stake: this is on the path of *every* authorization, so an
    /// `expect` here turns one panic anywhere under this lock into a share that
    /// answers nothing for the rest of its life. Every connection task dying in
    /// the same place, with no receipt line to say why. A `SeenClock` is four
    /// integers and a flag, and the worst a recovered one can be is one
    /// observation out of date.
    fn now(&self) -> DateTime<Utc> {
        let mut seen = self.clock.lock().unwrap_or_else(|p| p.into_inner());
        seen.observe(Utc::now(), self.started_mono.elapsed())
    }

    /// How far the wall clock has moved *out from under* this share, in
    /// seconds: negative for a backward step, positive for a forward one.
    ///
    /// Reported rather than corrected. The receipt's job is to say what
    /// happened, and "this session's timestamps are 3600s apart from what the
    /// elapsed time says" is a fact a reader needs in order to weigh the rest
    /// of it.
    fn clock_stepped_by(&self) -> i64 {
        self.clock.lock().unwrap_or_else(|p| p.into_inner()).stepped
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

    /// Resolve a presented secret against the grant table as it is on disk
    /// right now.
    ///
    /// Re-read per connection rather than cached, and that is the whole
    /// mechanism behind revocation: `h5i box share revoke` runs in a different
    /// process, so a cached table would be a revoke that appeared to work and
    /// did nothing. The cost is one small file read per connection, which is
    /// nothing next to opening a TCP connection into a namespace.
    pub fn authorize(&self, secret: &str) -> Result<AuthorizedGrant, Denied> {
        // Closed as soon as shutdown begins, not when the record catches up.
        // `winding_up` reaches disk and the grants stay live until teardown
        // finishes, so a handler that was already accepted when the share
        // began stopping could authorize, dial the box and change something
        // after the receipt had been written. The in-process flag flips first
        // and this is the one place a connection turns into access.
        if *self.shutdown.borrow() {
            self.record_denied(Denied::ShareOver);
            return Err(Denied::ShareOver);
        }
        let now = self.now().timestamp();
        let s = match self.our_record() {
            session::ReadState::Present(s) => s,
            // Gone, not broken. `share stop --force` removes the file and the
            // serving process only notices on its next poll, so every request
            // in that second used to be written into the receipt as a machine
            // problem. For something the operator did on purpose.
            session::ReadState::Gone => {
                self.record_denied(Denied::ShareOver);
                return Err(Denied::ShareOver);
            }
            session::ReadState::Unreadable => {
                // The grant table is gone or unreadable, so nothing authorizes
                // anything, but *why* it could not be read is the sharer's
                // problem, not the visitor's. Reported as `Unknown` this told the
                // visitor to ask for a new invite (which would behave identically),
                // told the sharer their peer had presented a ticket nobody knows,
                // and wrote "3 unknown ticket" into the receipt, for a full disk.
                self.record_denied(Denied::TableUnreadable);
                return Err(Denied::TableUnreadable);
            }
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
    /// is still admitted would leave the revoked peer's open connections
    /// (their hot-reload socket, their event stream) running. Revocation is
    /// advertised as per person; this is what makes it so.
    pub fn grant_is_live(&self, grant_id: &str) -> bool {
        let now = self.now().timestamp();
        match self.our_record() {
            session::ReadState::Present(s) => s
                .grants
                .iter()
                .any(|g| g.id == grant_id && !g.revoked && g.expires_at > now),
            // The file is gone, unreadable, or belongs to a different share.
            // Nothing authorizes anything. Fail closed.
            _ => false,
        }
    }

    /// Has the share itself been told to stop?
    ///
    /// Distinct from any one grant being dead, and the distinction is a
    /// sentence somebody reads. `session::stop` revokes every grant *and* sets
    /// this in one write, so the per-connection watchdog's revoke check always
    /// won the race and closed with "this ticket was revoked or has expired".
    /// Telling the visitor their invite had run out, when what happened is
    /// that the person sharing pressed stop. Measured 3 times out of 3.
    pub fn share_is_ending(&self) -> bool {
        match self.our_record() {
            session::ReadState::Present(s) => s.winding_up,
            // No record at all is the *most* ended a share gets: that is what
            // `share stop --force` leaves. Written as `unwrap_or(false)` this
            // sent exactly that case down the other branch and told the
            // visitor their ticket had been revoked. The sentence this
            // method exists to stop.
            session::ReadState::Gone => true,
            // A table this cannot read says nothing about whether the share
            // was stopped, so it does not get to claim it was.
            session::ReadState::Unreadable => false,
        }
    }

    /// True once no grant can admit anyone: everything revoked, or everything
    /// expired. The transports poll this so a share that has been cut off drops
    /// the connections it is already carrying, instead of serving them until
    /// the peer gets bored.
    pub fn is_spent(&self) -> bool {
        let now = self.now().timestamp();
        match self.our_record() {
            session::ReadState::Present(s) => s.is_spent(now),
            // Gone, unreadable, or somebody else's. All three mean this
            // process has nothing left to serve. Fail closed.
            _ => true,
        }
    }

    /// The record on disk, if it is still the one this process claimed.
    ///
    /// A replacement reads as [`session::ReadState::Gone`], which is the
    /// honest answer to every question a bridge asks: *this* share is over.
    /// Reading it as present is what let a force-stopped bridge keep serving
    /// the old port under the new share's tickets. See [`ClaimedRecord`].
    fn our_record(&self) -> session::ReadState {
        match session::read_state(&self.env_dir) {
            session::ReadState::Present(s) if self.claimed.matches(&s) => {
                session::ReadState::Present(s)
            }
            session::ReadState::Present(_) => session::ReadState::Gone,
            other => other,
        }
    }

    /// The identity this bridge claimed, for the teardown to check before it
    /// mutates anything.
    pub fn claimed(&self) -> &ClaimedRecord {
        &self.claimed
    }

    /// Adopt whatever record is on disk as this bridge's own.
    ///
    /// Tests only, and it exists because a test helper builds the bridge and
    /// then writes the record, where `serve_async` does it the other way
    /// round. Production has no use for it: a bridge that adopts a record it
    /// did not claim is the defect [`ClaimedRecord`] exists to close.
    #[cfg(test)]
    pub(crate) fn repin_for_test(&mut self) {
        self.claimed = ClaimedRecord::on_disk(&self.env_dir);
    }

    /// Open a fresh connection into the box.
    /// A socket into the box, and the accounting that goes with failing to get
    /// one.
    ///
    /// The two failures are recorded apart because the receipt says different
    /// things about them, and one of them blames the wrong person: `unreached
    /// N connection(s) ... found nothing listening on port 3000` is a sentence
    /// about the user's dev server, and it was printed for a broken dialer
    /// too. That case is sticky, a retired channel fails every later request
    /// the same way, so one lost reply produced a receipt asserting hundreds
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
    /// Past the cap the connection is still served (it is authorized, after
    /// all) and its handle names no record, so its traffic is counted nowhere.
    /// That is deliberate and it is not free: the *number* of such peers is
    /// counted here and reported, because folding them into the last record
    /// corrupted that record, and dropping them silently made a busy share
    /// look like a quiet one.
    pub fn peer_joined(&self, peer: String, grant: &AuthorizedGrant, path: Option<Path>) -> PeerId {
        // Fail soft like every other accessor here. This one used to `expect`,
        // and it is called while the tunnel front holds its own peer map, so a
        // poisoned tally would have poisoned that too, and taken every later
        // connection with it.
        let mut t = self.tally();
        if t.peers.len() >= MAX_PEER_RECORDS {
            // A handle that names nothing. Returning the *last* record folded
            // peer 257's bytes, connections and path observations into peer
            // 256, including setting a path 256 had never had observed.
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
    /// The tunnel has no connection lifecycle to hang `peer_left` on (a
    /// visitor is a grant, and their connections come and go) so without this
    /// `closed` stayed `None` and every tunnel peer rendered as held to the end
    /// of the share. Somebody who opened one page in minute two of a six-hour
    /// share was written down as having been connected for six hours.
    /// Somebody was turned away for a reason that has nothing to do with their
    /// ticket.
    ///
    /// These left no trace anywhere. A user behind a symmetric NAT running
    /// `--direct-only`, the flag this feature advertises as its strongest
    /// guarantee, watched `refused ... no direct path` scroll past their
    /// terminal and then got a receipt saying `peers none. The share was open
    /// but nobody connected`. Likewise every smuggling attempt the gate exists
    /// to refuse: answered with a `400` and forgotten, in the one lane whose
    /// job is to say who was turned away.
    pub fn record_turned_away(&self, why: TurnedAwayReason) {
        let mut t = self.tally();
        // Bounded like the denial list, and, unlike the first version of this
        // function, counting what it drops. `Unparseable` is driven by
        // anonymous traffic on a public URL, so a scanner spraying malformed
        // requests pinned the line at exactly 1024 and said nothing about it:
        // the very defect the commit that added this counter fixed for the
        // denial list, reintroduced in the same commit.
        if t.turned_away.len() < 1024 {
            t.turned_away.push(TurnedAway {
                at: Utc::now(),
                why,
            });
        } else {
            t.turned_away_overflow += 1;
        }
    }

    pub fn peer_seen(&self, id: PeerId) {
        if let Some(p) = self.tally().peers.get_mut(id.0) {
            p.last_seen = Some(Utc::now());
        }
    }

    /// Counted against a peer's record, or, for a peer past the record cap,
    /// against the aggregate.
    ///
    /// Every one of these was a silent no-op for an overflow handle, and byte
    /// accounting is a stated purpose of this receipt, so that made the
    /// evidence deliberately evadable: fill the record list with 256
    /// probe-only connections, which cost nothing and record nothing, then do
    /// the real transfer on connection 257. The receipt said overflow peers
    /// existed and reported zero of their connections and zero of their bytes.
    ///
    /// The individual-record cap stays, a record carries a `String` and
    /// becomes a line, but the totals no longer have a hole in them.
    pub fn peer_connection(&self, id: PeerId) {
        let mut t = self.tally();
        match t.peers.get_mut(id.0) {
            Some(p) => p.connections += 1,
            None => t.overflow_connections += 1,
        }
    }

    pub fn peer_bytes(&self, id: PeerId, to_peer: u64, from_peer: u64) {
        let mut t = self.tally();
        match t.peers.get_mut(id.0) {
            Some(p) => {
                p.bytes_to_peer += to_peer;
                p.bytes_from_peer += from_peer;
            }
            None => {
                t.overflow_bytes_to_peer += to_peer;
                t.overflow_bytes_from_peer += from_peer;
            }
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
    /// the commonest probe of a public tunnel URL, a scanner fetching `/`,
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
    /// connection tasks are spawned and detached, closing the endpoint tells
    /// them to stop but does not wait for them, so without this the receipt is
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
    /// Setting the flag inside `quiesce`, after the shutdown, meant the task
    /// that closes with a reason could never win, and the commit that added it
    /// was inert.
    pub fn begin_shutdown(&self) {
        let _ = self.shutdown.send_replace(true);
    }

    /// Mark a connection as accepted and being handled.
    ///
    /// The permit is held from before the head is read until the handler
    /// returns, which is the span [`Self::quiesce`] could not see. Quiescence
    /// waited only on [`Self::admit`] permits, and a handler paused in
    /// `read_head`, in parsing, or in authorization holds none of those, so
    /// `quiesce` acquired all sixty-four immediately, marked the receipt
    /// settled and returned while such a handler was still live. On Ctrl-C or
    /// a transport failure the record is merely `winding_up` and its grants
    /// are still there, so that handler could then resume, authorize, take a
    /// now-free permit, dial the box and send a state-changing request *after*
    /// the receipt had snapshotted its tally. Killing `cloudflared` does not
    /// help: a complete request may already be buffered on an accepted
    /// loopback socket.
    ///
    /// `None` once the share is winding up: a connection accepted after that
    /// point is not owed a handler, and refusing it here is what makes
    /// quiescence a barrier rather than a hope.
    pub fn enter_front(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        if *self.shutdown.borrow() {
            return None;
        }
        self.front.clone().try_acquire_owned().ok()
    }

    pub async fn quiesce(&self, within: std::time::Duration) {
        // Tell them to stop *before* waiting for them to. A connection carrying
        // a response with no declared length waits up to five minutes for the
        // box to go quiet; without this signal, a plain Ctrl-C would time out
        // waiting for it and write a receipt missing everything it carried.
        // `send_replace`, not `send`: tokio's `send` returns an error *without
        // storing the value* when no receiver is currently subscribed, and a
        // connection between accepting and its `select!` holds none. That made
        // the flag stay false for the rest of the process. Intermittently, and
        // exactly on the path this exists for.
        let _ = self.shutdown.send_replace(true);
        let all = u32::try_from(MAX_CONNECTIONS).unwrap_or(u32::MAX);
        // The answer is kept. Two paths write a receipt with connections still
        // mid-copy (this timing out, and an interrupt that skips the wait
        // entirely) and in both the byte counts are short and the peers render
        // as still connected. The stderr line saying so is gone by the time
        // anybody reads the artifact, so the artifact has to say it itself.
        // Both, and the front door first: an accepted handler that has not yet
        // taken an `admit` permit will take one, so waiting on `capacity`
        // alone is waiting for a number that can still go back up.
        let front = u32::try_from(MAX_FRONT).unwrap_or(u32::MAX);
        let settled = tokio::time::timeout(within, async {
            let _pre = self.front.acquire_many(front).await;
            let _live = self.capacity.acquire_many(all).await;
        })
        .await
        .is_ok();
        self.tally().settled = settled;
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
    /// kill. The answer to that is a code for the failures, not a code for
    /// everything.
    pub fn write_receipt_failed(&self, why: &str) {
        self.tally().failed_because = Some(why.to_string());
        self.write_receipt_with(1);
    }

    fn write_receipt_with(&self, exit_code: i32) {
        let ended = Utc::now();
        // From the monotonic clock, like the body below. `wall_ms` in the
        // receipt log said `0` for a two-minute session after a backward
        // clock step, so an export summed the whole thing as nothing.
        let seconds = self.started_mono.elapsed().as_secs() as i64;
        // Snapshotted, and the lock let go before anything is written. Held
        // across `receipt::append`, a file write plus a redaction scan over
        // the whole body, it blocks any connection still trying to record what
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
            // The same facts as the command line above, as fields. A reader
            // deciding whether a third party could read this traffic was
            // reduced to searching the rendered string for `tunnel`, which
            // the box's own name can supply, so the one security-relevant
            // claim in this record now travels as data.
            share: Some(h5i_core::receipt::ShareEvidence {
                transport: self.transport.as_str().to_string(),
                port: self.dialer.port(),
                peers: peers_seen,
                seconds,
                turned_away: summary.turned_away.len() as u64 + summary.turned_away_overflow,
            }),
            wall_ms: u64::try_from(seconds * 1000).ok(),
            // A share ends when it is asked to. Left unset, the receipt viewer
            // renders "signal" for it, which reads in an export as though the
            // session had been killed.
            exit_code: Some(exit_code),
            ..Default::default()
        };
        // Not if the box is gone. `receipt::append` creates the directory it
        // writes into, so a share that outlived `h5i box rm` recreated the env
        // directory it had just erased. Leaving a `receipt.jsonl` and a blob
        // under a path with no manifest, which `box ls`, `share ls`, `gc` and
        // the console all answer "no environment named that" for, and which
        // nothing but `rm -rf` can clear.
        //
        // The receipt is lost in that case, and that is the right trade: the
        // box it was evidence about has been deleted, and a record of a thing
        // that no longer exists, readable by no tool, is worse than none.
        if !self.env_dir.exists() {
            eprintln!(
                "share: the box was removed while this share was running, so there is nowhere \
                 to write the receipt"
            );
            return;
        }
        match h5i_core::receipt::append(&self.env_dir, input, body.as_bytes()) {
            // Named on the way out. The one command whose whole pitch is "the
            // session lands in the box's receipt" was the one that never
            // mentioned the receipt again, and no discovery command lists it,
            // so the id existed only inside `receipt.jsonl` and you had to
            // know to `cat` it. `h5i box run` has printed its receipt id since
            // it was written.
            Ok(rec) => eprintln!(
                "◈  receipt {} · {} peer(s), {}s, {} to visitors · h5i box inspect {} --capture {}",
                rec.id,
                peers_seen,
                seconds,
                bytes(summary.peers.iter().map(|p| p.bytes_to_peer).sum()),
                self.box_name,
                rec.id
            ),
            Err(e) => eprintln!("share: could not record the session: {e}"),
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
    /// How long the session really lasted, measured on a clock nothing can
    /// move. Not `ended - started`: those are wall-clock readings, and a
    /// backward NTP step between them produced a receipt reading `share
    /// session, 0s` for a session that ran two minutes and moved 2.7 KiB,
    /// with `closed` 58 minutes before `opened`. The `.max(0)` that used to
    /// sit here is what turned an absurdity a reader would question into a
    /// plausible-looking zero.
    pub seconds: i64,
    /// The wall-clock *steps* seen during the session, signed and summed:
    /// negative for a clock that jumped backwards. Zero on any ordinary run,
    /// including on a host whose two clocks run at visibly different rates.
    /// See `SeenClock` for why those are not the same thing.
    pub clock_moved: i64,
    pub peers: Vec<PeerRecord>,
    /// Peers past what the record list holds.
    pub peers_overflow: u64,
    /// What those peers did, in aggregate: their connections and their bytes.
    /// Individually unrecorded, the record list is capped, but never
    /// uncounted, because a byte total with a hole in it is a byte total
    /// anybody can walk through.
    pub overflow_connections: u64,
    pub overflow_bytes_to_peer: u64,
    pub overflow_bytes_from_peer: u64,
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
    /// Turned-away connections past what the list holds. Counted rather than
    /// dropped, so the leading number is the truth.
    pub turned_away_overflow: u64,
    /// Why the share ended badly, when it did. Without it a failed share wrote
    /// a body identical to a successful one and only the exit code differed,
    /// which flips the box's console verdict to "attention" for good, with
    /// nothing in the artifact saying why.
    pub failed_because: Option<String>,
    /// Responses the box left unfinished.
    pub truncated: u64,
}

/// The wall clock, watched for jumps.
///
/// Two clocks disagreeing is not one thing. A *step* is a discontinuity (an
/// NTP correction, a VM resumed from a snapshot, somebody running `date -s`)
/// and it moves a ticket's expiry relative to the time its holder was
/// promised. *Drift* is the two clocks running at different rates, which is
/// ordinary: the machine this was written on runs `CLOCK_REALTIME` five per
/// cent slower than `CLOCK_MONOTONIC`, all day, with nothing wrong.
///
/// The first version of the floor could not tell them apart, and treating
/// drift as a step shortened every ticket on this host by five per cent.
///
/// So each observation is compared against what the monotonic clock says
/// should have passed since the last one, with a tolerance proportional to
/// that gap. Drift stays inside the tolerance by definition (it is a rate);
/// a step of any size that matters does not.
#[derive(Debug, Default)]
struct SeenClock {
    /// The raw wall reading at the last observation.
    last_wall: i64,
    /// The monotonic reading at the last observation, in seconds since start.
    last_mono: i64,
    /// Total of the backward steps seen, added back so they buy nobody time.
    correction: i64,
    /// Every step seen, signed and summed, for the receipt to report. Distinct
    /// from `correction`, which only accumulates the backward half.
    stepped: i64,
    seen_any: bool,
}

/// The smallest jump worth calling a step, and the share of the gap between
/// observations that is written off as drift.
const STEP_FLOOR: i64 = 2;
const DRIFT_SHARE: i64 = 4;

impl SeenClock {
    fn observe(&mut self, wall: DateTime<Utc>, elapsed: std::time::Duration) -> DateTime<Utc> {
        let wall_s = wall.timestamp();
        let mono_s = elapsed.as_secs() as i64;
        if !self.seen_any {
            self.seen_any = true;
            self.last_wall = wall_s;
            self.last_mono = mono_s;
            return wall;
        }
        let gap = (mono_s - self.last_mono).max(0);
        // A quarter of the gap, never less than two seconds. Drift is a rate,
        // so it stays under any proportional bound wider than the rate itself;
        // this host's five per cent has twenty points of room.
        let tolerance = (gap / DRIFT_SHARE).max(STEP_FLOOR);
        let moved = wall_s - (self.last_wall + gap);
        if moved.abs() > tolerance {
            self.stepped += moved;
            if moved < 0 {
                // A backward jump buys nobody time.
                self.correction -= moved;
            } else {
                // A forward jump *unwinds* the correction, down to zero and no
                // further. Without this, the commonest real pattern, a clock
                // that goes wrong and is then put right, left the correction
                // standing: back an hour, forward an hour, and every ticket
                // afterwards expired an hour early against a clock that was
                // now correct. NTP overshoot and a VM resumed then resynced
                // both look exactly like that.
                //
                // Below zero it would become a way to buy time by jumping
                // forward and back, so it clamps: a genuine forward jump past
                // the correction is honoured, which is the safe direction.
                self.correction = (self.correction - moved).max(0);
            }
        }
        self.last_wall = wall_s;
        self.last_mono = mono_s;
        // `checked_add_signed`, because `DateTime + Duration` panics on
        // overflow and this is on the path of every authorization the share
        // does.
        wall.checked_add_signed(chrono::Duration::seconds(self.correction))
            .unwrap_or(wall)
    }
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
    let seconds = s.seconds.max(0);
    let mut out = String::new();
    out.push_str(&format!(
        "share session, {seconds}s ({} transport)\n",
        s.transport.as_str()
    ));
    out.push_str(&format!("opened   {}\n", s.started.to_rfc3339()));
    out.push_str(&format!("closed   {}\n", s.ended.to_rfc3339()));
    // Said out loud, because everything else on this page is a wall-clock
    // reading and the reader has no other way to know they moved. The two
    // timestamps above can be out of order, and the per-peer `held` figures
    // are wall-clock subtractions, so this is the line that tells a reader
    // which numbers below to weigh and which to distrust. Five seconds of
    // tolerance: a poll interval and a rounding, not a clock step.
    if s.clock_moved != 0 {
        let (dir, by) = if s.clock_moved < 0 {
            ("back", -s.clock_moved)
        } else {
            ("forward", s.clock_moved)
        };
        out.push_str(&format!(
            "clock    this machine's clock jumped {dir} {by}s during the session — the length \
             above is measured, but the two timestamps and each peer's held time are clock \
             readings and are off by that much\n"
        ));
    } else {
        // No jump, and the two clocks still disagree about how long this was.
        // That is drift. Two clocks running at different rates, which is
        // ordinary and is not the sharer's machine misbehaving. Reported
        // anyway, in weaker words, because the timestamps on this page are the
        // wall clock's opinion and the length above is not, and a reader
        // subtracting one from the other deserves to know they will not match.
        // The host this was written on runs its wall clock five per cent slow,
        // so this is not a hypothetical.
        let span = (s.ended - s.started).num_seconds();
        let off = span - seconds;
        // Five seconds and two per cent. Two, because the host this was
        // written on drifts five and a ten per cent bar would never fire where
        // it is most true; five seconds absolute, so a short share does not
        // report a rounding as a rate.
        if off.abs() >= 5 && off.abs() * 50 >= seconds {
            out.push_str(&format!(
                "clock    the timestamps above are {}s apart and the session was measured at \
                 {seconds}s — this machine's wall clock and its elapsed-time clock run at \
                 different rates. Nothing jumped; the length is the measured one\n",
                span
            ));
        }
    }
    out.push_str(&format!(
        "shared   port {} inside the box, never published on the host\n",
        s.port
    ));
    // Cleaned like the label and `failed_because` below it. A tunnel endpoint
    // is charset-checked by `extract_url` and a node id is base32, so this is
    // safe today, and "this field cannot hold one" is the reasoning that left
    // four renderers in `env.rs` unfixed.
    out.push_str(&format!(
        "endpoint {}\n",
        h5i_core::redact::sanitize_display(&s.endpoint)
    ));
    if s.transport == Transport::Tunnel {
        // Said in the receipt, not only in the docs. Whoever reads this later
        // is exactly the person who needs to know a third party could read the
        // traffic, and they will not be re-reading MANUAL.md to find out.
        out.push_str(
            "note     a Cloudflare quick tunnel terminated TLS, so this traffic was not \
             end-to-end encrypted\n",
        );
    }

    if let Some(why) = &s.failed_because {
        // First line after the header, because it changes what the rest of the
        // receipt is an account of.
        out.push_str(&format!(
            "ended    badly: {}\n",
            h5i_core::redact::sanitize_display(why)
        ));
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
            // share, which is the honest reading of "still there when it
            // ended" rather than a default that quietly applies to everyone.
            let closed = p.closed.or(p.last_seen).unwrap_or(s.ended);
            let held = (closed - p.opened).num_seconds().max(0);
            out.push_str(&format!(
                "  {} via {} — grant {}{}, {held}s, {}, {} in / {} out\n",
                h5i_core::redact::sanitize_display(&p.peer),
                p.path
                    .map(|x| x.as_str())
                    .unwrap_or("a path nothing observed"),
                h5i_core::redact::sanitize_display(&p.grant),
                // Sanitised, like `failed_because` twelve lines up and like
                // every other string this repository renders that it did not
                // author. A label is `--label` from a command line, so it is
                // the operator's own, but "the operator typed it" and "the
                // operator authored it" are different claims once it comes from
                // an automation, a ticket title or a paste, and a receipt is
                // read in a terminal by somebody who was not there. An `\r` in
                // it rewrites the line above.
                p.label
                    .as_ref()
                    .map(|l| format!(" ({})", h5i_core::redact::sanitize_display(l)))
                    .unwrap_or_default(),
                plural(p.connections, "connection", "connections"),
                bytes(p.bytes_from_peer),
                bytes(p.bytes_to_peer),
            ));
        }
        if s.peers_overflow > 0 {
            // With their traffic, which used to be the sentence's other half:
            // "not counted above" was true of the rows *and* of the totals, so
            // filling this list with 256 probe-only connections and moving the
            // real data on the 257th produced a receipt that mentioned the
            // overflow and reported nothing it did.
            out.push_str(&format!(
                "  and {} more peer(s), past the {MAX_PEER_RECORDS} this receipt lists \
                 individually — not shown above by peer, but counted: {} connection(s), \
                 {} in / {} out\n",
                s.peers_overflow,
                s.overflow_connections,
                bytes(s.overflow_bytes_from_peer),
                bytes(s.overflow_bytes_to_peer),
            ));
        }
        // A ticket is a bearer capability: forwarding the text admits everyone
        // it reaches, all under the one grant. That is the design, and the
        // receipt is the only place it ever becomes visible. Two endpoint ids
        // against one grant id, which a reader has to notice for themselves in
        // a list that may be long. Said out loud here, because it is also the
        // one thing that makes `share revoke` cut off more people than the
        // sharer thinks they are cutting off.
        let mut per_grant: std::collections::HashMap<&str, usize> = Default::default();
        for p in &s.peers {
            *per_grant.entry(p.grant.as_str()).or_default() += 1;
        }
        let mut forwarded: Vec<(&str, usize)> =
            per_grant.into_iter().filter(|(_, n)| *n > 1).collect();
        forwarded.sort();
        for (grant, n) in forwarded {
            out.push_str(&format!(
                "ticket   grant {grant} was used by {n} separate peers — a ticket admits \
                 everyone it is forwarded to, and revoking it cuts off all of them\n"
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
        // the millions, and a reader who saw that number under "capacity"
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
            "turned   {} connection(s) away before any ticket was weighed{}: of the {} \
             recorded, {}\n",
            s.turned_away.len() as u64 + s.turned_away_overflow,
            if s.turned_away_overflow > 0 {
                format!(
                    " ({} of them not recorded individually)",
                    s.turned_away_overflow
                )
            } else {
                String::new()
            },
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
        let unreadable = s
            .denied
            .iter()
            .filter(|d| d.reason == Denied::TableUnreadable)
            .count();
        let after_stop = s
            .denied
            .iter()
            .filter(|d| d.reason == Denied::ShareOver)
            .count();
        // The leading number counts every refusal, not just the ones kept
        // individually. It read `refused 1024 attempt(s)` for fifty thousand,
        // with the truth in a trailing clause, and the sub-counts are over the
        // recorded sample, so their sum does not match and the line now says so.
        out.push_str(&format!(
            "refused  {} attempt(s){}: of the {} recorded, {none} presented no invite, \
             {unknown} an unknown ticket, {expired} expired, {revoked} revoked{}{}\n",
            s.denied.len() as u64 + s.denied_overflow,
            if s.denied_overflow > 0 {
                format!(" ({} of them not recorded individually)", s.denied_overflow)
            } else {
                String::new()
            },
            s.denied.len(),
            if unreadable > 0 {
                format!(
                    "; and {unreadable} could not be weighed at all, because this share could \
                     not read its own grant table — a problem on the sharing machine"
                )
            } else {
                String::new()
            },
            // Its own clause, because it is not a refusal in the same sense:
            // these arrived after the grant table was removed, which is what
            // stopping a share does. Counted with the unreadable ones it read
            // as a machine fault; counted with the unknown ones it read as
            // somebody probing with a bad ticket.
            if after_stop > 0 {
                format!("; and {after_stop} arrived after the share had been stopped")
            } else {
                String::new()
            }
        ));
    }
    out
}

/// The grant table as `h5i box share status` shows it.
/// A byte count somebody can read at a glance.
///
/// The receipt printed bare numbers with no unit, so `96 in / 7209077 out` left
/// a reader to guess at both the unit and the magnitude, and the magnitude is
/// the thing they are reading the line for. Powers of 1024, because that is
/// what a socket buffer and a page cache are counted in, and one decimal place
/// because two is a precision this number does not have.
fn bytes(n: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    const TIB: u64 = GIB * 1024;
    // Thresholds allow for the rounding that follows them. At exactly `>= KIB`
    // a value a hair under a mebibyte formats as `1024.0 KiB`, which is a
    // number nobody writes; the unit has to change slightly before the
    // arithmetic says so.
    const ROUNDS_UP: f64 = 1023.95;
    let scaled = |unit: u64| n as f64 / unit as f64;
    match n {
        _ if scaled(TIB) >= 1.0 => format!("{:.1} TiB", scaled(TIB)),
        _ if scaled(GIB) >= ROUNDS_UP => format!("{:.1} TiB", scaled(TIB)),
        _ if scaled(GIB) >= 1.0 => format!("{:.1} GiB", scaled(GIB)),
        _ if scaled(MIB) >= ROUNDS_UP => format!("{:.1} GiB", scaled(GIB)),
        _ if scaled(MIB) >= 1.0 => format!("{:.1} MiB", scaled(MIB)),
        _ if scaled(KIB) >= ROUNDS_UP => format!("{:.1} MiB", scaled(MIB)),
        _ if scaled(KIB) >= 1.0 => format!("{:.1} KiB", scaled(KIB)),
        // Under a kibibyte the exact count is both short and interesting: it is
        // the difference between a request that carried a body and one that
        // carried none.
        n => format!("{n} B"),
    }
}

/// The slug a person types, out of a full `env/agent/slug` id.
fn short_name(box_id: &str) -> &str {
    box_id.rsplit('/').next().unwrap_or(box_id)
}

pub fn render_status(s: &ShareSession, now: i64) -> String {
    let mut out = String::new();
    let live = session::is_live(s);
    // Grants get a say in the headline, not just the process. A live process
    // whose only grant has expired printed "sharing port 3000 over tunnel" and
    // "allgone1  expired" three lines apart, from one command, while
    // `is_admitting` correctly answered no everywhere else in the codebase, so
    // `box rebase` went straight through the record this claimed was live. The
    // window is about a second normally, and the whole width of a suspend, a
    // freeze or a starved host otherwise.
    let headline = if !live {
        "— was sharing"
    } else if s.winding_up {
        "— shutting down, was sharing"
    } else if s.is_spent(now) {
        "— nobody can get in (every ticket is expired or revoked); was sharing"
    } else {
        "— sharing"
    };
    // Every variable field, for the reason the grant label three screens down
    // already carries: `share.json` is read back off disk, and a receipt or a
    // status line is read in a terminal by somebody who was not there.
    use h5i_core::redact::sanitize_display as clean;
    out.push_str(&format!(
        "{} {headline} port {} over {}\n",
        clean(&s.box_id),
        s.port,
        s.transport.as_str()
    ));
    out.push_str(&format!("  endpoint  {}\n", clean(&s.endpoint)));
    out.push_str(&format!("  started   {}\n", clean(&s.started_at)));
    // A share that started in this machine's future means the clock moved
    // between then and now. Worth a line, because the serving process floors
    // its expiry decisions against a monotonic clock and this command cannot:
    // it is reading a file with whatever clock this shell has. So after a
    // backward step the door closes earlier than the countdown below says, and
    // without this the only symptom is a ticket that "still had time left"
    // being refused.
    // One rule, shared with `run::grant`, which refuses to mint under the same
    // condition, so the countdown and the minting cannot disagree about
    // whether the clocks have moved apart.
    if let Some(ahead) = session::started_in_the_future(s, now) {
        out.push_str(&format!(
            "            NOTE: this share reports starting {} in the future by this \
             machine's clock. The clock moved; the times below are this clock's opinion, \
             and the share itself goes by elapsed time, so it will close sooner than they \
             say. `h5i box share grant` is refused while that is true.\n",
            session::humanise(ahead)
        ));
    }
    out.push_str(&format!(
        "  process   pid {}{}\n",
        s.pid,
        if live {
            if s.winding_up {
                // The human view was the only one that did not say this: the
                // JSON carried `winding_up` and `grant` refused on it, while
                // `status` one second earlier still said "sharing".
                " — shutting down; it is writing its receipt and will be gone in a moment."
            } else {
                ""
            }
        } else {
            ""
        }
    ));
    if !live {
        // With the box's name in it. Printed bare, this told somebody whose
        // share had just been declared dead to run a command that answers with
        // a clap usage error.
        out.push_str(&format!(
            "            GONE. This share is not serving anything; run \
             `h5i box share stop {}`.\n",
            short_name(&s.box_id)
        ));
    }
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
            clean(&g.id),
            state,
            // Sanitised for the reason the receipt's copy is: this is a string
            // rendered straight into a terminal, and `share ls` renders it for
            // every box on the clone at once, so one label decides how the
            // rows around it read.
            g.label
                .as_ref()
                .map(|l| format!("  {}", h5i_core::redact::sanitize_display(l)))
                .unwrap_or_default()
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bridge over a temp directory, for the accounting tests. Uses the
    /// local dialer: nothing here opens a connection.
    ///
    /// This and the tests that use it were Linux-only for as long as
    /// `Dialer::spawn_local` was. It forked a helper into a network namespace,
    /// and neither the fork nor the namespace exists on macOS. macOS has its
    /// own `spawn_local` now (the box is this process's tree), so the gate is
    /// gone and these nine run on both platforms, which is where they are worth
    /// the most: they are about accounting, receipts and fail-closed
    /// authorization, none of which is platform-specific, and all of which was
    /// being checked on one platform only.
    fn test_bridge(dir: &std::path::Path) -> Bridge {
        Bridge::new(
            dir.to_path_buf(),
            "env/test/demo".into(),
            "digest".into(),
            "demo".into(),
            Transport::P2p,
            "local".into(),
            crate::dialer::Dialer::spawn_local(1).expect("dialer"),
            ClaimedRecord::on_disk(dir),
        )
    }

    /// A force-stopped bridge does not adopt the share that replaces it.
    ///
    /// `share stop --force` deletes a live record without stopping its process
    /// and relies on that process noticing within a second. A new
    /// `h5i box share` may legitimately claim the empty path inside that
    /// second, and the old bridge, reading whatever file was at the path,
    /// found the replacement's live grant and carried on: a second transport
    /// into the old box, on the old port, admitting the new share's tickets,
    /// for the whole life of the new share. Its own eventual exit then marked
    /// the *new* share as winding up.
    #[test]
    fn a_bridge_whose_record_was_replaced_is_spent_rather_than_adopted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut a = crate::session::ShareSession::new(
            "env/a/demo",
            3000,
            Transport::P2p,
            "abc",
            Utc::now(),
        );
        let (g, _secret) = crate::session::mint_grant(None, 4_000_000_000).expect("mint");
        let a_grant = g.id.clone();
        a.grants.push(g);
        session::write(dir.path(), &a).expect("write");

        let mut bridge = test_bridge(dir.path());
        bridge.repin_for_test();
        assert!(bridge.grant_is_live(&a_grant));
        assert!(!bridge.is_spent());
        assert!(!bridge.share_is_ending());

        // `share stop --force`, then a fresh share claims the box. Same pid
        // (one test process), different start, which is what distinguishes
        // two shares of one box a moment apart.
        std::fs::remove_file(crate::session::session_path(dir.path())).expect("force stop");
        let mut b = crate::session::ShareSession::new(
            "env/a/demo",
            9999,
            Transport::P2p,
            "def",
            Utc::now() + chrono::Duration::seconds(1),
        );
        let (g, b_secret) = crate::session::mint_grant(None, 4_000_000_000).expect("mint");
        let b_grant = g.id.clone();
        b.grants.push(g);
        session::write(dir.path(), &b).expect("write");

        // The old bridge is over, and knows it.
        assert!(
            bridge.is_spent(),
            "a replaced record kept the old bridge alive"
        );
        assert!(
            bridge.share_is_ending(),
            "the old bridge did not treat a replacement as its own ending"
        );
        assert!(
            !bridge.grant_is_live(&b_grant),
            "the old bridge watched the new share's grant"
        );
        assert!(
            !bridge.grant_is_live(&a_grant),
            "the old bridge still admitted its own dead grant"
        );
        assert!(
            matches!(
                bridge.authorize(&b_secret),
                Err(crate::session::Denied::ShareOver)
            ),
            "the old bridge admitted a ticket minted for the new share"
        );

        // And its teardown leaves the new share alone.
        crate::session::begin_winding_up(dir.path(), &a.started_at).expect("winding up");
        assert!(
            !crate::session::read(dir.path()).expect("read").winding_up,
            "a force-stopped bridge marked the replacement share as winding up"
        );
        crate::session::clear(dir.path(), &a.started_at);
        assert!(
            crate::session::read(dir.path()).is_some(),
            "a force-stopped bridge deleted the replacement share's record"
        );
    }

    pub(super) fn at(s: &str) -> DateTime<Utc> {
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
            seconds: 600,
            clock_moved: 0,
            peers,
            peers_overflow: 0,
            overflow_connections: 0,
            overflow_bytes_to_peer: 0,
            overflow_bytes_from_peer: 0,
            denied,
            denied_overflow: 0,
            over_capacity: 0,
            front_refused: 0,
            unreachable: 0,
            route_broken: 0,
            settled: true,
            turned_away: Vec::new(),
            turned_away_overflow: 0,
            failed_because: None,
            truncated: 0,
        }
    }

    /// The same property `h5i-core`'s renderers now carry, on this crate's two.
    ///
    /// A receipt is read in a terminal by somebody who was not there, and half
    /// of what it names came from off this machine. Every field is safe *today*
    /// (a peer id is base32, a tunnel endpoint is charset-checked by
    /// `extract_url`, a grant id is minted here) and that is exactly the
    /// reasoning that left four renderers in `env.rs` unfixed until a property
    /// test asked them all at once.
    #[test]
    fn neither_share_renderer_puts_a_control_character_on_the_terminal() {
        const HOSTILE: &str = "x\u{1b}[2J\u{1b}[1;1Hforged\u{202e}\u{7}";
        let clean = |rendered: &str, what: &str| {
            assert!(
                !rendered.chars().any(|c| c.is_control() && c != '\n'),
                "{what} put a control character on the terminal: {rendered:?}"
            );
            assert!(!rendered.contains('\u{202e}'), "{what} kept a bidi override");
        };

        let mut p = peer(HOSTILE, Path::Direct);
        p.grant = HOSTILE.into();
        p.label = Some(HOSTILE.into());
        let mut s = summary(vec![p], vec![]);
        s.endpoint = HOSTILE.into();
        s.failed_because = Some(HOSTILE.into());
        clean(&render_receipt(&s), "render_receipt");

        let mut session = ShareSession::new(
            HOSTILE,
            3000,
            Transport::Tunnel,
            HOSTILE,
            at("2026-08-10T10:00:00Z"),
        );
        session.started_at = HOSTILE.into();
        let (mut g, _secret) = session::mint_grant(Some(HOSTILE.into()), 4_000_000_000)
            .expect("mint a grant");
        g.id = HOSTILE.into();
        session.grants.push(g);
        clean(&render_status(&session, 1_000), "render_status");
    }

    #[test]
    fn the_receipt_names_the_peer_the_path_and_the_traffic() {
        let body = render_receipt(&summary(vec![peer("kbcd…", Path::Direct)], vec![]));
        assert!(body.contains("share session, 600s (p2p transport)"));
        assert!(body.contains("port 3000 inside the box, never published"));
        assert!(body.contains("kbcd… via direct"));
        assert!(body.contains("grant a1b2c3d4 (alex)"));
        assert!(body.contains("12 connections"));
        assert!(body.contains("900 B in / 4.9 KiB out"), "{body}");
    }

    // Builds its bridge inline rather than through `test_bridge`, which is how
    // it slipped past the gating and turned up on the macOS runner instead.
    #[test]
    fn a_receipt_does_not_resurrect_a_box_that_was_removed() {
        // `receipt::append` creates the directory it writes into, so a share
        // that outlived `h5i box rm` recreated the env directory that had just
        // been erased. What was left behind had a receipt log and a payload
        // and no manifest, so every tool answered "no environment named that"
        // and only `rm -rf` could clear it.
        let dir = tempfile::tempdir().expect("tempdir");
        let gone = dir.path().join("removed");
        std::fs::create_dir_all(&gone).expect("mkdir");
        let b = Bridge::new(
            gone.clone(),
            "env/a/demo".into(),
            "digest".into(),
            "demo".into(),
            Transport::P2p,
            "local".into(),
            crate::dialer::Dialer::spawn_local(1).expect("dialer"),
            ClaimedRecord::on_disk(&gone),
        );

        b.write_receipt();
        assert!(
            gone.join("receipt.jsonl").exists(),
            "the ordinary case still writes one"
        );

        std::fs::remove_dir_all(&gone).expect("remove");
        b.write_receipt();
        assert!(
            !gone.exists(),
            "the receipt recreated a box that had been removed"
        );
    }

    #[test]
    fn a_byte_count_says_what_it_counts() {
        // `96 in / 7209077 out` left a reader guessing at both the unit and
        // the magnitude, and the magnitude is what they are reading the line
        // for.
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(96), "96 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1024), "1.0 KiB");
        assert_eq!(bytes(5000), "4.9 KiB");
        assert_eq!(bytes(7_209_077), "6.9 MiB");
        assert_eq!(bytes(4 * 1024 * 1024 * 1024), "4.0 GiB");
        // Boundaries, which the first version of this test skipped. The one
        // that matters: just under a megabyte must not read as "1024.0 KiB",
        // which is a number nobody writes.
        assert_eq!(
            bytes(1_048_575),
            "1.0 MiB",
            "a hair under a mebibyte is not 1024.0 KiB"
        );
        assert_eq!(bytes(1_073_741_823), "1.0 GiB");
        assert_eq!(bytes(1_048_576), "1.0 MiB");
        // And there is a terabyte unit now, so a big share does not read as
        // four figures of gibibytes.
        assert_eq!(bytes(1024u64 * 1024 * 1024 * 1024), "1.0 TiB");
        assert_eq!(bytes(u64::MAX), "16777216.0 TiB");
    }

    #[test]
    fn a_ticket_used_by_two_people_says_so() {
        // Measured against a real box: two `h5i join` processes on one ticket
        // both got 200, both reached the dev server, and both appear in the
        // receipt under the same grant id. That is the design (a ticket is a
        // bearer capability, and the manual says so) but the roadmap still
        // claimed "one ticket admits one peer ... revocation is per person",
        // and the receipt left a reader to notice two endpoint ids against one
        // grant in a list that can run to 256 entries.
        let mut a = peer("aaaa", Path::Direct);
        a.grant = "g1".into();
        let mut b = peer("bbbb", Path::Direct);
        b.grant = "g1".into();
        let mut c = peer("cccc", Path::Direct);
        c.grant = "g2".into();

        let body = render_receipt(&summary(vec![a, b, c], vec![]));
        assert!(
            body.contains("ticket   grant g1 was used by 2 separate peers"),
            "{body}"
        );
        assert!(body.contains("cuts off all of them"), "{body}");
        // The grant that only one person used gets no such line: this is a
        // notice about a thing that happened, not a lecture attached to every
        // receipt.
        assert!(!body.contains("grant g2 was used"), "{body}");

        // And one peer per grant says nothing at all.
        let quiet = render_receipt(&summary(vec![peer("aaaa", Path::Direct)], vec![]));
        assert!(!quiet.contains("separate peers"), "{quiet}");
    }

    #[test]
    fn a_stopped_share_is_the_most_ended_a_share_gets() {
        // Written first as `session::read(..).map(|s| s.winding_up)
        // .unwrap_or(false)`, which sends the *strongest* case (no record at
        // all, which is what `share stop --force` leaves) down the branch
        // that tells the visitor their ticket was revoked. That sentence is
        // the one this method was added to stop saying.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut b = test_bridge(dir.path());

        let mut s = crate::session::ShareSession::new(
            "env/a/demo",
            3000,
            Transport::P2p,
            "abc",
            Utc::now(),
        );
        session::write(dir.path(), &s).expect("write");
        // The helper builds the bridge before the record exists; `serve_async`
        // claims first and pins what it claimed.
        b.repin_for_test();
        assert!(!b.share_is_ending(), "a serving share is not ending");

        s.winding_up = true;
        session::write(dir.path(), &s).expect("write");
        assert!(b.share_is_ending());

        std::fs::remove_file(dir.path().join("share.json")).expect("force stop");
        assert!(
            b.share_is_ending(),
            "no record at all is not 'still serving'"
        );

        // And a table that cannot be read says nothing either way, so it does
        // not get to claim the operator stopped anything.
        std::fs::write(dir.path().join("share.json"), b"{ junk").expect("junk");
        assert!(!b.share_is_ending());
    }

    #[test]
    fn a_clock_stepped_backwards_cannot_lengthen_a_ticket() {
        // Measured, not imagined: stepping a running share's clock back an
        // hour turned `8m left` into `1h8m left`, and the share went on
        // serving a peer through it. Every expiry decision in the process was
        // a bare `Utc::now()`, so a backward step of an hour extended every
        // live grant by an hour of real time. Past what its holder was told,
        // past what the sharer was told.
        let base = at("2026-08-10T10:00:00Z");
        let sec = |n: u64| std::time::Duration::from_secs(n);
        let mut c = SeenClock::default();

        // First reading establishes the pair; it cannot judge anything yet.
        assert_eq!(c.observe(base, sec(0)), base);
        // Two ordinary seconds.
        assert_eq!(
            c.observe(base + chrono::Duration::seconds(2), sec(2)),
            base + chrono::Duration::seconds(2)
        );

        // The clock goes back an hour while one second passes. The share must
        // read the second, not the hour.
        let stepped = c.observe(base - chrono::Duration::seconds(3597), sec(3));
        assert_eq!(stepped, base + chrono::Duration::seconds(3));
        assert_eq!(c.stepped, -3600);
        // And it stays corrected on the next reading, rather than snapping
        // back the moment nothing new happens.
        let after = c.observe(base - chrono::Duration::seconds(3596), sec(4));
        assert_eq!(after, base + chrono::Duration::seconds(4));

        // Forward is honoured. Expiring a ticket early is the safe direction,
        // and a machine whose clock was wrong when the share started should be
        // allowed to find out.
        let mut c = SeenClock::default();
        c.observe(base, sec(0));
        let jumped = c.observe(base + chrono::Duration::seconds(3601), sec(1));
        assert_eq!(jumped, base + chrono::Duration::seconds(3601));
        assert_eq!(c.stepped, 3600);
    }

    #[test]
    fn a_clock_put_right_again_does_not_leave_every_ticket_short() {
        // The commonest real pattern is not a jump: it is a jump and then a
        // correction. NTP overshooting and settling, a VM resumed from a
        // snapshot and then resynced. Both go wrong and then right. The first
        // version of this kept the backward correction forever, so after the
        // clock was fixed every ticket expired by however far it had wandered:
        // back an hour, forward an hour, and a one-hour ticket dies on issue.
        let base = at("2026-08-10T10:00:00Z");
        let sec = |n: u64| std::time::Duration::from_secs(n);
        let mut c = SeenClock::default();
        c.observe(base, sec(0));

        // Back an hour.
        let out = c.observe(base - chrono::Duration::seconds(3599), sec(1));
        assert_eq!(out, base + chrono::Duration::seconds(1));
        assert_eq!(c.correction, 3600);

        // And put right again a second later.
        let out = c.observe(base + chrono::Duration::seconds(2), sec(2));
        assert_eq!(c.correction, 0, "the correction outlived the problem");
        assert_eq!(
            out,
            base + chrono::Duration::seconds(2),
            "a corrected clock was still being corrected"
        );
        // Both jumps are still reported; they cancel, and the receipt says so
        // by saying nothing, which is right. The session's timestamps do
        // bracket its length again.
        assert_eq!(c.stepped, 0);

        // A forward jump on its own is honoured, and cannot dig the correction
        // below zero. Otherwise jumping forward and back would be a way to
        // buy time rather than lose it.
        let mut c = SeenClock::default();
        c.observe(base, sec(0));
        let out = c.observe(base + chrono::Duration::seconds(3601), sec(1));
        assert_eq!(out, base + chrono::Duration::seconds(3601));
        assert_eq!(c.correction, 0);
        let out = c.observe(base - chrono::Duration::seconds(3598), sec(2));
        assert_eq!(
            out,
            base + chrono::Duration::seconds(3602),
            "a clock that went forward and came back bought itself time"
        );
    }

    #[test]
    fn a_clock_that_merely_runs_slow_is_not_a_clock_that_jumped() {
        // The first version of the floor could not tell a step from a rate,
        // and this host makes that expensive: `CLOCK_REALTIME` here advances
        // 56.9s per 60s of `CLOCK_MONOTONIC`, continuously, with nothing
        // wrong. Flooring the wall clock against `started + elapsed` outright
        // therefore expired a one-hour ticket at fifty-seven minutes, and
        // would take over an hour off a day-long one.
        let base = at("2026-08-10T10:00:00Z");
        let mut c = SeenClock::default();
        let mut wall = 0i64;
        // An hour of this host's five per cent, sampled once a minute.
        for minute in 0..60 {
            wall += 57;
            let out = c.observe(
                base + chrono::Duration::seconds(wall),
                std::time::Duration::from_secs((minute + 1) * 60),
            );
            // Untouched: no correction, because nothing jumped.
            assert_eq!(
                out,
                base + chrono::Duration::seconds(wall),
                "drift was corrected as if it were a step, at minute {minute}"
            );
        }
        assert_eq!(c.stepped, 0, "drift was reported as a jump");

        // And a real jump is still caught on a host that drifts.
        let out = c.observe(
            base + chrono::Duration::seconds(wall - 600),
            std::time::Duration::from_secs(3660),
        );
        assert!(
            c.stepped < 0,
            "a ten-minute jump went unnoticed on a drifting host"
        );
        // Corrected up to where the monotonic clock says the wall clock
        // should have been: the last reading plus the minute that passed.
        assert_eq!(out, base + chrono::Duration::seconds(wall + 660 - 600));
    }

    #[test]
    fn a_receipt_measures_the_session_and_says_when_the_clock_moved() {
        // This is the real receipt from a share that ran two minutes and moved
        // 2.7 KiB, with the clock stepped back an hour mid-session:
        //
        //     share session, 0s (p2p transport)
        //     opened   2026-08-11T14:22:14+00:00
        //     closed   2026-08-11T13:23:51+00:00
        //
        // `closed` before `opened`, and `0s`: from `(ended - started)
        // .num_seconds().max(0)`, where the clamp turned an absurdity a reader
        // would question into a plausible zero. The length is now measured on
        // a clock nothing can move, and the reason the timestamps disagree
        // with it is on the page.
        let mut s = summary(vec![], vec![]);
        s.started = at("2026-08-11T14:22:14Z");
        s.ended = at("2026-08-11T13:23:51Z");
        s.seconds = 97;
        s.clock_moved = -3503;

        let body = render_receipt(&s);
        assert!(body.contains("share session, 97s"), "{body}");
        assert!(!body.contains("share session, 0s"), "{body}");
        assert!(body.contains("clock jumped back 3503s"), "{body}");
        assert!(
            body.contains("are clock readings and are off by that much"),
            "{body}"
        );

        // A forward step reads as one, and does not inflate the length: 683s
        // was recorded for a 23-second session.
        let mut s = summary(vec![], vec![]);
        s.seconds = 23;
        s.clock_moved = 660;
        let body = render_receipt(&s);
        assert!(body.contains("share session, 23s"), "{body}");
        assert!(body.contains("clock jumped forward 660s"), "{body}");

        // And an ordinary session says nothing about clocks.
        let mut s = summary(vec![], vec![]);
        s.started = at("2026-08-10T10:00:00Z");
        s.ended = at("2026-08-10T10:10:00Z");
        s.seconds = 600;
        s.clock_moved = 0;
        assert!(!render_receipt(&s).contains("clock"));

        // A host whose two clocks run at different rates gets a weaker
        // sentence, and specifically not "your clock jumped". This one runs
        // its wall clock five per cent slow all day with nothing wrong, and
        // calling that a jump on every receipt would be the same false
        // accusation this round set out to stop making.
        let mut s = summary(vec![], vec![]);
        s.started = at("2026-08-10T10:00:00Z");
        s.ended = at("2026-08-10T10:09:30Z");
        s.seconds = 600;
        s.clock_moved = 0;
        let body = render_receipt(&s);
        assert!(body.contains("run at different rates"), "{body}");
        assert!(!body.contains("clock jumped"), "{body}");
        assert!(body.contains("Nothing jumped"), "{body}");
    }

    #[test]
    fn a_grant_table_this_share_cannot_read_is_not_the_visitors_fault() {
        // Every I/O failure reading `share.json` (a full disk, no descriptors
        // left, a permission problem) used to be reported as `Unknown`. So
        // the visitor was told "the sharer refused this ticket, ask for a new
        // one" (which would behave identically), the sharer's terminal said
        // their peer had presented a ticket nobody knows, and the receipt
        // permanently recorded "3 unknown ticket". For a machine problem on
        // the sharing side.
        let dir = tempfile::tempdir().expect("tempdir");
        let b = test_bridge(dir.path());

        // A file that is there and cannot be understood. It has to be a
        // *present* file: an absent one is what stopping a share leaves, and
        // reading the two as one condition is what the round below fixes.
        std::fs::write(dir.path().join("share.json"), b"{ this is not a record")
            .expect("write junk");
        let err = b.authorize("ab".repeat(32).as_str()).expect_err("no table");
        assert_eq!(err, Denied::TableUnreadable);
        assert!(
            err.explain().contains("problem on the sharing machine"),
            "{}",
            err.explain()
        );
        assert!(!err.explain().contains("not one this share knows"));

        // And it is counted apart in the receipt, with its own sentence, but
        // only when it happened, which is this file's own rule.
        let s = b.snapshot();
        let body = render_receipt(&s);
        assert!(
            body.contains("could not read its own grant table"),
            "{body}"
        );

        let quiet = summary(vec![], vec![]);
        assert!(!render_receipt(&quiet).contains("could not read its own"));
    }

    #[test]
    fn a_share_that_has_been_stopped_is_not_a_broken_machine() {
        // The fix above was itself too wide. `share stop --force` removes
        // `share.json`, and the serving process only notices on its next poll,
        // so for that second every arriving request was recorded as "this share
        // could not read its own grant table. A problem on the sharing
        // machine". Nothing was wrong with the machine: somebody stopped the
        // share on purpose. One round replaced a false accusation against the
        // visitor with a false accusation against the sharer's computer.
        let dir = tempfile::tempdir().expect("tempdir");
        let b = test_bridge(dir.path());

        // No file at all. The stopped share.
        let err = b.authorize("ab".repeat(32).as_str()).expect_err("no table");
        assert_eq!(err, Denied::ShareOver);
        assert_eq!(err.explain(), "this share has ended");
        assert!(!err.explain().contains("sharing machine"));

        let body = render_receipt(&b.snapshot());
        assert!(body.contains("after the share had been stopped"), "{body}");
        assert!(
            !body.contains("could not read its own grant table"),
            "{body}"
        );

        // And the two travel as different bytes, so the visitor's browser is
        // not told to go and ask for a replacement invite when the answer is
        // "that share is over" or "their disk is full".
        assert_ne!(
            crate::wire::REPLY_SHARE_OVER,
            crate::wire::REPLY_SHARER_FAULT
        );
        assert_ne!(crate::wire::REPLY_SHARE_OVER, crate::wire::REPLY_DENIED);
        assert_ne!(crate::wire::REPLY_SHARER_FAULT, crate::wire::REPLY_DENIED);
    }

    #[test]
    fn a_missing_grant_table_authorizes_nobody_and_ends_the_share() {
        // The two fail-closed branches, which nothing touched. If either
        // inverted, deleting `share.json` would leave a share admitting
        // everybody with no way to revoke, and `share stop --force`, which
        // deletes exactly that file, is a documented verb.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut b = test_bridge(dir.path());

        let mut sess = crate::session::ShareSession::new(
            "env/a/demo",
            3000,
            Transport::P2p,
            "local",
            Utc::now(),
        );
        let (g, _secret) = crate::session::mint_grant(None, 4_000_000_000).expect("mint");
        let id = g.id.clone();
        sess.grants.push(g);
        crate::session::write(dir.path(), &sess).expect("write");
        b.repin_for_test();
        assert!(b.grant_is_live(&id), "a live grant read as dead");
        assert!(!b.is_spent(), "a live share read as spent");

        std::fs::remove_file(crate::session::session_path(dir.path())).expect("remove");
        assert!(
            !b.grant_is_live(&id),
            "a deleted grant table still authorized a peer"
        );
        assert!(b.is_spent(), "a deleted grant table left the share serving");

        // And a file that is there but unreadable as a session is the same
        // answer: `session::read` treats a malformed file as absent, so a box
        // that could somehow corrupt it must not thereby open the door.
        std::fs::write(crate::session::session_path(dir.path()), b"{not json").expect("write");
        assert!(
            !b.grant_is_live(&id),
            "a corrupt grant table authorized a peer"
        );
        assert!(b.is_spent(), "a corrupt grant table left the share serving");
    }

    #[test]
    fn more_peers_than_the_list_holds_are_counted_not_dropped() {
        // The overflow path. A share past the peer cap must still say how many
        // it saw, or a heavily used share reads as a lightly used one.
        let dir = tempfile::tempdir().expect("tempdir");
        let b = test_bridge(dir.path());
        let g = AuthorizedGrant {
            id: "a1b2c3d4".into(),
            label: Some("alex".into()),
            expires_at: 4_000_000_000,
        };
        for _ in 0..(MAX_PEER_RECORDS + 5) {
            b.peer_joined("somebody".into(), &g, Some(Path::Direct));
        }
        let s = b.snapshot();
        assert_eq!(s.peers.len(), MAX_PEER_RECORDS);
        assert_eq!(s.peers_overflow, 5);
        let body = render_receipt(&s);
        assert!(body.contains("and 5 more peer(s)"), "{body}");
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
        // and that case is sticky, so one lost reply produced a receipt
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
        // A user behind a symmetric NAT running `--direct-only`, the flag this
        // feature advertises as its strongest guarantee, watched the refusals
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
        // Two paths reach here (the quiesce timing out, and an interrupt that
        // skips the wait) and in both the byte counts are short. The stderr
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
        // and `status`, the view somebody actually consults before re-minting,
        // was left on integer minutes, one column away from "expired".
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
        // means a string continuation was broken, which has happened twice.
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
        s.overflow_connections = 7;
        s.overflow_bytes_from_peer = 900;
        s.overflow_bytes_to_peer = 5000;
        let body = render_receipt(&s);
        assert!(body.contains("and 41 more peer(s)"), "{body}");
        // What they *did*, not only that they existed. The line used to say
        // "their traffic is not counted above", and that was true of the
        // totals as well as of the rows, which made this receipt evadable by
        // design: fill the record list with probe-only connections, then move
        // the real data on the next one.
        assert!(body.contains("7 connection(s)"), "{body}");
        assert!(body.contains("900 B in"), "{body}");
        assert!(body.contains("4.9 KiB out"), "{body}");
        assert!(!body.contains("not counted above"), "{body}");
    }

    /// A peer past the record cap is counted even though it is not listed.
    ///
    /// `peer_joined` returns a handle that names no record once the list is
    /// full, and every mutation through that handle was a silent no-op, so a
    /// holder of one ticket could open and close 256 probe-only connections to
    /// fill the list and then do the real transfer on connection 257, with the
    /// receipt reporting zero connections and zero bytes for all of it. Byte
    /// accounting is a stated purpose of this artifact.
    #[test]
    fn a_peer_past_the_record_cap_still_lands_in_the_totals() {
        let dir = tempfile::tempdir().expect("tempdir");
        let b = test_bridge(dir.path());
        let grant = AuthorizedGrant {
            id: "a1b2c3d4".into(),
            label: None,
            expires_at: 4_000_000_000,
        };

        for i in 0..MAX_PEER_RECORDS {
            let id = b.peer_joined(format!("peer{i}"), &grant, None);
            b.peer_connection(id);
        }
        let overflow = b.peer_joined("the real one".into(), &grant, None);
        b.peer_connection(overflow);
        b.peer_bytes(overflow, 4_000, 1_000);

        let s = b.snapshot();
        assert_eq!(s.peers.len(), MAX_PEER_RECORDS);
        assert_eq!(s.peers_overflow, 1);
        assert_eq!(s.overflow_connections, 1);
        assert_eq!(s.overflow_bytes_to_peer, 4_000);
        assert_eq!(s.overflow_bytes_from_peer, 1_000);
        // And nothing landed on the last listed peer, which is the other way
        // this used to go wrong.
        let last = s.peers.last().expect("a record");
        assert_eq!(last.connections, 1);
        assert_eq!(last.bytes_to_peer, 0);
        assert_eq!(last.bytes_from_peer, 0);
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
        // With a name, because printed bare this is a clap usage error.
        assert!(out.contains("share stop demo"), "{out}");
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
        // bytes, connections and path landed on peer 256, including giving it
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

#[cfg(test)]
mod label_tests {
    use super::*;
    use crate::session::{mint_grant, ShareSession};

    /// A label reaches a terminal through two renderers, and neither may let it
    /// rewrite the lines around it.
    ///
    /// `failed_because` is sanitised in the receipt and the label beside it was
    /// not, which is the same asymmetry `owner::display_name` was fixed for.
    /// The label is the operator's own `--label`, so this is not somebody
    /// else's string, until it comes from an automation, a ticket title or a
    /// paste, and `share ls` renders every box on the clone in one go, so one
    /// label decides how its neighbours read.
    #[test]
    fn a_label_cannot_rewrite_the_lines_around_it() {
        let nasty = "alex\r\u{1b}[2Kadmin\u{202e}";
        let clean = |s: &str, what: &str| {
            assert!(!s.contains('\r'), "{what}: {s:?}");
            assert!(!s.contains('\u{1b}'), "{what}: {s:?}");
            assert!(!s.contains('\u{202e}'), "{what}: {s:?}");
            assert!(s.contains("alex"), "{what} lost the label: {s:?}");
        };

        let mut s = ShareSession::new("env/a/demo", 3000, Transport::P2p, "abc", Utc::now());
        let (g, _secret) = mint_grant(Some(nasty.into()), 4_000_000_000).expect("mint");
        s.grants.push(g);
        clean(&render_status(&s, 1), "share status");

        let summary = Summary {
            transport: Transport::P2p,
            endpoint: "abc".into(),
            port: 3000,
            started: Utc::now(),
            ended: Utc::now(),
            seconds: 1,
            clock_moved: 0,
            peers: vec![PeerRecord {
                peer: "peer".into(),
                grant: "g1".into(),
                label: Some(nasty.into()),
                path: Some(Path::Direct),
                opened: Utc::now(),
                closed: None,
                last_seen: None,
                connections: 1,
                bytes_to_peer: 1,
                bytes_from_peer: 1,
            }],
            peers_overflow: 0,
            overflow_connections: 0,
            overflow_bytes_to_peer: 0,
            overflow_bytes_from_peer: 0,
            denied: Vec::new(),
            denied_overflow: 0,
            over_capacity: 0,
            front_refused: 0,
            unreachable: 0,
            route_broken: 0,
            settled: true,
            turned_away: Vec::new(),
            turned_away_overflow: 0,
            failed_because: None,
            truncated: 0,
        };
        clean(&render_receipt(&summary), "the receipt");
    }
}

#[cfg(test)]
mod receipt_fuzz {
    use super::tests::at;
    use super::*;
    use crate::fuzz::{rounds, Rng};

    fn any_summary(rng: &mut Rng) -> Summary {
        let started = at("2026-08-10T10:00:00Z");
        let peers: Vec<PeerRecord> = (0..rng.below(5))
            .map(|i| {
                let mut p = PeerRecord {
                    peer: format!("peer{i}"),
                    grant: "a1b2c3d4".into(),
                    label: if rng.chance(2) {
                        Some("alex".into())
                    } else {
                        None
                    },
                    path: match rng.below(4) {
                        0 => Some(Path::Direct),
                        1 => Some(Path::Relayed),
                        2 => Some(Path::Tunnel),
                        _ => None,
                    },
                    opened: started,
                    closed: None,
                    last_seen: None,
                    connections: rng.below(2000) as u64,
                    bytes_to_peer: rng.next() % 10_000_000,
                    bytes_from_peer: rng.next() % 10_000_000,
                };
                if rng.chance(2) {
                    p.closed = Some(at("2026-08-10T10:05:00Z"));
                }
                if rng.chance(3) {
                    p.last_seen = Some(at("2026-08-10T10:02:00Z"));
                }
                p
            })
            .collect();
        let denied: Vec<DeniedAttempt> = (0..rng.below(6))
            .map(|_| DeniedAttempt {
                at: started,
                reason: match rng.below(6) {
                    0 => Denied::Unknown,
                    1 => Denied::Expired,
                    2 => Denied::Revoked,
                    3 => Denied::NoCredential,
                    // The two the generator never produced. They are the two
                    // whose rendering is conditional, an `if count > 0` clause
                    // each, so they were exactly the arms the fuzzer was there
                    // to exercise, and it could not reach either.
                    4 => Denied::TableUnreadable,
                    _ => Denied::ShareOver,
                },
            })
            .collect();
        let turned_away: Vec<TurnedAway> = (0..rng.below(4))
            .map(|_| TurnedAway {
                at: started,
                why: match rng.below(3) {
                    0 => TurnedAwayReason::NoDirectPath,
                    1 => TurnedAwayReason::NeverGreeted,
                    _ => TurnedAwayReason::Unparseable,
                },
            })
            .collect();
        Summary {
            transport: if rng.chance(2) {
                Transport::P2p
            } else {
                Transport::Tunnel
            },
            seconds: rng.next() as i64 % 100_000,
            clock_moved: (rng.next() as i64 % 7_200) - 3_600,
            endpoint: "abcdef".into(),
            port: 3000,
            started,
            ended: at("2026-08-10T10:10:00Z"),
            peers,
            peers_overflow: rng.next() % 500,
            overflow_connections: rng.next() % 5_000,
            overflow_bytes_to_peer: rng.next() % 1_000_000,
            overflow_bytes_from_peer: rng.next() % 1_000_000,
            denied,
            denied_overflow: rng.next() % 100_000,
            over_capacity: rng.next() % 100,
            front_refused: rng.next() % 100,
            unreachable: rng.next() % 100,
            route_broken: rng.next() % 100,
            settled: rng.chance(2),
            turned_away,
            turned_away_overflow: rng.next() % 1000,
            truncated: rng.next() % 10,
            failed_because: if rng.chance(4) {
                Some("the tunnel died".into())
            } else {
                None
            },
        }
    }

    /// The receipt is the artifact this whole feature exists to produce, and
    /// every round that has read it found a sentence claiming more than its
    /// counter supported. These are the claims checked against the numbers,
    /// for whatever combination of numbers turns up.
    #[test]
    fn no_line_of_a_receipt_claims_more_than_its_counter() {
        let mut rng = Rng::new(0x2ECE197);
        for i in 0..rounds().min(20_000) {
            let seed = rng.next();
            let mut one = Rng::new(seed);
            let s = any_summary(&mut one);
            let body = render_receipt(&s);
            let ctx = || format!("round {i}, seed {seed:#x}");

            // A line only appears when it has something to say. Each of these
            // was, at some point, printed for a count of zero or omitted for a
            // count that was not.
            assert_eq!(
                body.contains("truncated"),
                s.truncated > 0,
                "truncated said {} for {}: {}",
                body.contains("truncated"),
                s.truncated,
                ctx()
            );
            assert_eq!(body.contains("\npartial "), !s.settled, "{}", ctx());
            assert_eq!(
                body.contains("\ncapacity "),
                s.over_capacity > 0,
                "{}",
                ctx()
            );
            assert_eq!(
                body.contains("\nflooded "),
                s.front_refused > 0,
                "{}",
                ctx()
            );
            assert_eq!(
                body.contains("\nunreached "),
                s.unreachable > 0,
                "{}",
                ctx()
            );
            assert_eq!(body.contains("\nroute "), s.route_broken > 0, "{}", ctx());
            assert_eq!(
                body.contains("\nended    badly"),
                s.failed_because.is_some(),
                "{}",
                ctx()
            );
            assert_eq!(
                body.contains("\nturned "),
                !s.turned_away.is_empty(),
                "{}",
                ctx()
            );
            assert_eq!(
                body.contains("\nrefused "),
                !s.denied.is_empty(),
                "{}",
                ctx()
            );

            // The two leading counts are totals, not samples. Both were
            // sample-sized once, so a share knocked on fifty thousand times
            // read as one knocked on a thousand times.
            if !s.denied.is_empty() {
                let total = s.denied.len() as u64 + s.denied_overflow;
                assert!(
                    body.contains(&format!("refused  {total} attempt(s)")),
                    "the refusal total is not the total: {}",
                    ctx()
                );
            }
            if !s.turned_away.is_empty() {
                let total = s.turned_away.len() as u64 + s.turned_away_overflow;
                assert!(
                    body.contains(&format!("turned   {total} connection(s)")),
                    "the turned-away total is not the total: {}",
                    ctx()
                );
            }

            // Nobody connected is a different statement from somebody did.
            assert_eq!(
                body.contains("nobody connected"),
                s.peers.is_empty(),
                "{}",
                ctx()
            );

            // And a peer is never held for longer than the share existed.
            let whole = (s.ended - s.started).num_seconds();
            for line in body.lines().filter(|l| l.starts_with("  peer")) {
                let held: i64 = line
                    .split(", ")
                    .find_map(|f| f.strip_suffix('s').and_then(|n| n.parse().ok()))
                    .unwrap_or(0);
                assert!(
                    held <= whole,
                    "a peer was held {held}s of a {whole}s share: {line} ({})",
                    ctx()
                );
            }
        }
    }
}
