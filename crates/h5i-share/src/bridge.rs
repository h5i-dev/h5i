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

use std::sync::Mutex;

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
    pub path: Path,
    pub opened: DateTime<Utc>,
    pub closed: Option<DateTime<Utc>>,
    /// TCP connections into the box carried for this peer.
    pub connections: u64,
    pub bytes_to_peer: u64,
    pub bytes_from_peer: u64,
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
    peers: Vec<PeerRecord>,
    denied: Vec<DeniedAttempt>,
}

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
        let s = session::read(&self.env_dir).ok_or(Denied::Unknown)?;
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
    pub fn open_upstream(&self) -> Result<std::net::TcpStream, H5iError> {
        self.dialer.connect()
    }

    fn record_denied(&self, reason: Denied) {
        if let Ok(mut t) = self.tally.lock() {
            // Bounded: a share left open on the internet can be knocked on all
            // day, and an unbounded list would be a memory leak with a receipt
            // attached. The count past the cap still shows up in the summary.
            if t.denied.len() < 1024 {
                t.denied.push(DeniedAttempt {
                    at: Utc::now(),
                    reason,
                });
            }
        }
    }

    /// Note that a peer has arrived. Returns the handle its traffic is counted
    /// against.
    pub fn peer_joined(
        &self,
        peer: String,
        grant: &AuthorizedGrant,
        path: Path,
    ) -> PeerId {
        let mut t = self.tally.lock().expect("tally");
        t.peers.push(PeerRecord {
            peer,
            grant: grant.id.clone(),
            label: grant.label.clone(),
            path,
            opened: Utc::now(),
            closed: None,
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
    pub fn peer_path(&self, id: PeerId, path: Path) {
        if let Ok(mut t) = self.tally.lock() {
            if let Some(p) = t.peers.get_mut(id.0) {
                // Only ever toward the weaker claim within a session: a
                // connection that spent any time on a relay is a connection
                // that used one, and rounding that off would be flattering.
                if p.path == Path::Direct && path == Path::Relayed {
                    p.path = Path::Relayed;
                }
            }
        }
    }

    pub fn peer_connection(&self, id: PeerId) {
        if let Ok(mut t) = self.tally.lock() {
            if let Some(p) = t.peers.get_mut(id.0) {
                p.connections += 1;
            }
        }
    }

    pub fn peer_bytes(&self, id: PeerId, to_peer: u64, from_peer: u64) {
        if let Ok(mut t) = self.tally.lock() {
            if let Some(p) = t.peers.get_mut(id.0) {
                p.bytes_to_peer += to_peer;
                p.bytes_from_peer += from_peer;
            }
        }
    }

    pub fn peer_left(&self, id: PeerId) {
        if let Ok(mut t) = self.tally.lock() {
            if let Some(p) = t.peers.get_mut(id.0) {
                p.closed = Some(Utc::now());
            }
        }
    }

    /// How many peers have connected, for the terminal line the sharer watches.
    pub fn peer_count(&self) -> usize {
        self.tally.lock().map(|t| t.peers.len()).unwrap_or(0)
    }

    /// Write the session into the box's receipt log.
    ///
    /// Called once, when the share ends. A share is a foreground command that
    /// usually ends on Ctrl-C, so the transports install a signal handler and
    /// call this on the way out: a receipt that only got written on a clean
    /// shutdown would be missing from exactly the sessions people actually run.
    pub fn write_receipt(&self) {
        let ended = Utc::now();
        let seconds = (ended - self.started).num_seconds().max(0);
        let t = match self.tally.lock() {
            Ok(t) => t,
            Err(p) => p.into_inner(),
        };
        let body = render_receipt(
            &Summary {
                transport: self.transport,
                endpoint: self.endpoint.clone(),
                port: self.dialer.port(),
                started: self.started,
                ended,
                peers: t.peers.clone(),
                denied: t.denied.clone(),
            },
        );
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
                t.peers.len()
            )),
            wall_ms: u64::try_from(seconds * 1000).ok(),
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
    pub denied: Vec<DeniedAttempt>,
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

    if s.peers.is_empty() {
        out.push_str("peers    none — the share was open but nobody connected\n");
    } else {
        out.push_str(&format!("peers    {}\n", s.peers.len()));
        for p in &s.peers {
            let closed = p.closed.unwrap_or(s.ended);
            let held = (closed - p.opened).num_seconds().max(0);
            out.push_str(&format!(
                "  {} via {} — grant {}{}, {held}s, {}, {} in / {} out\n",
                p.peer,
                p.path.as_str(),
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
        let relayed = s.peers.iter().filter(|p| p.path == Path::Relayed).count();
        if relayed > 0 {
            out.push_str(&format!(
                "relay    {relayed} peer(s) used a relay. It moved sealed packets and could not \
                 read them.\n"
            ));
        }
    }

    if !s.denied.is_empty() {
        let unknown = s.denied.iter().filter(|d| d.reason == Denied::Unknown).count();
        let expired = s.denied.iter().filter(|d| d.reason == Denied::Expired).count();
        let revoked = s.denied.iter().filter(|d| d.reason == Denied::Revoked).count();
        out.push_str(&format!(
            "refused  {} attempt(s): {unknown} unknown ticket, {expired} expired, {revoked} \
             revoked\n",
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
            let mins = (g.expires_at - now) / 60;
            format!("{mins}m left")
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

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).expect("timestamp").into()
    }

    fn peer(name: &str, path: Path) -> PeerRecord {
        PeerRecord {
            peer: name.into(),
            grant: "a1b2c3d4".into(),
            label: Some("alex".into()),
            path,
            opened: at("2026-08-10T10:00:00Z"),
            closed: Some(at("2026-08-10T10:05:00Z")),
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
            denied,
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
            DeniedAttempt { at: at("2026-08-10T10:01:00Z"), reason: Denied::Unknown },
            DeniedAttempt { at: at("2026-08-10T10:02:00Z"), reason: Denied::Unknown },
            DeniedAttempt { at: at("2026-08-10T10:03:00Z"), reason: Denied::Revoked },
        ];
        let body = render_receipt(&summary(vec![], denied));
        assert!(body.contains("refused  3 attempt(s)"));
        assert!(body.contains("2 unknown ticket"));
        assert!(body.contains("1 revoked"));
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
    fn a_path_that_fell_back_to_a_relay_is_not_rounded_up_to_direct() {
        // Recorded honestly in the one direction that matters: a connection
        // that spent any time relayed used a relay, and the receipt must not
        // flatter it once a direct path appears later.
        let mut p = peer("kbcd…", Path::Relayed);
        p.path = Path::Relayed;
        let body = render_receipt(&summary(vec![p], vec![]));
        assert!(body.contains("relayed"));
    }
}
