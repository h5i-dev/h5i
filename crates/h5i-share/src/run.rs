//! Starting, describing and ending a share.
//!
//! The order of operations here is load bearing, and one step in particular:
//! **the dialer is forked before the async runtime exists.** Forking a process
//! that already has a thread pool inherits one thread plus whatever locks the
//! others were holding — the allocator's among them — and the child deadlocks
//! somewhere with no stack to look at. So the one fork a share does happens
//! while this process is still single-threaded, and everything async comes
//! after it.
//!
//! Ending a share is the other thing this module is careful about. It is a
//! foreground command that people stop with Ctrl-C, so a receipt written only
//! on the clean path would be missing from every session anybody actually ran.
//! Three things end a share and all three write the receipt: the interrupt,
//! `h5i box share stop` from another terminal, and the last grant expiring.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use h5i_error::H5iError;

use crate::bridge::Bridge;
use crate::dialer::Dialer;
use crate::session::{self, ShareSession, Transport};

/// How often the share checks whether it has been stopped from elsewhere.
const STOP_POLL: Duration = Duration::from_secs(1);

/// How long to wait, after the transport is closed, for the connections it was
/// carrying to finish recording what they moved.
const QUIESCE: Duration = Duration::from_secs(5);

/// What `h5i box share` was asked for.
pub struct Request {
    pub env_dir: PathBuf,
    pub env_id: String,
    pub policy_digest: String,
    pub box_name: String,
    /// A pid living inside the box's namespaces.
    pub box_pid: u32,
    /// The port to share, inside the box.
    pub port: u16,
    pub expire: Duration,
    pub label: Option<String>,
    pub transport: Transport,
    /// Refuse to move application bytes over a relay. P2P only.
    pub direct_only: bool,
}

/// What a caller should print once the share is up. Returned through a callback
/// rather than printed here, so the CLI owns every byte the user sees.
pub struct Started {
    /// The ticket (P2P) or the invite URL (tunnel).
    pub invite: String,
    /// How a peer joins, in words.
    pub how: String,
    pub grant_id: String,
    pub expires_at: i64,
    pub transport: Transport,
    /// Set when the share started but nothing is listening on the port yet.
    pub warning: Option<String>,
}

/// Start sharing, and serve until stopped.
pub fn serve(req: Request, announce: impl FnOnce(&Started)) -> Result<(), H5iError> {
    // An early, friendly refusal so `share` fails before forking a helper and
    // dialling a network. It is **not** the check that matters — `session::claim`
    // re-does it under the lock, because between here and there is a window two
    // starts could both walk through.
    if let Some(existing) = session::read(&req.env_dir) {
        if session::is_live(&existing) {
            return Err(H5iError::Metadata(format!(
                "this box is already being shared by pid {} over {}. Stop it first \
                 (`h5i box share stop <name>`), or add a peer to the share you have \
                 (`h5i box share grant <name>`).",
                existing.pid,
                existing.transport.as_str()
            )));
        }
    }

    // Before the runtime. See the module note; this is the whole reason this
    // function is not simply `async`.
    let dialer = Dialer::spawn(req.box_pid, req.port)?;

    // A share of a port with nothing behind it is almost always a mistake, and
    // the peer is the one who would find out. Warn rather than refuse: an agent
    // that is about to start its dev server is a perfectly good reason to share
    // a port that is not up yet.
    let warning = match dialer.connect() {
        Ok(_) => None,
        Err(_) => Some(format!(
            "nothing is listening on port {} inside the box yet — peers will get an error until \
             the dev server starts (`h5i box ports {}` shows what is up)",
            req.port, req.box_name
        )),
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| H5iError::Metadata(format!("could not start the share runtime: {e}")))?;

    runtime.block_on(async move { serve_async(req, dialer, warning, announce).await })
}

async fn serve_async(
    req: Request,
    dialer: Dialer,
    warning: Option<String>,
    announce: impl FnOnce(&Started),
) -> Result<(), H5iError> {
    let expires_at = (chrono::Utc::now() + chrono::Duration::from_std(req.expire).unwrap_or_default())
        .timestamp();
    let (grant, secret) = session::mint_grant(req.label.clone(), expires_at)?;
    let grant_id = grant.id.clone();

    // Transport setup first: it decides the endpoint the session records, and
    // it is the step most likely to fail (no network, no cloudflared). Failing
    // before anything is written keeps a dead share.json off disk.
    let mut started = Setup::start(&req).await?;

    let mut sess = ShareSession::new(
        &req.env_id,
        req.port,
        req.transport,
        started.endpoint(),
        chrono::Utc::now(),
    );
    sess.grants.push(grant);
    // Check and write in one locked step. A transport that is already running
    // but a claim that fails means we tear the transport down again, which is
    // the right order: better a wasted endpoint than two bridges sharing one
    // grant table on two different ports, where a ticket for one authorizes the
    // other.
    match session::claim(&req.env_dir, &sess) {
        Ok(Some(stale)) => eprintln!(
            "share: cleared a leftover share record from pid {stale} (that process is gone)"
        ),
        Ok(None) => {}
        Err(e) => {
            started.shutdown().await;
            return Err(e);
        }
    }

    let bridge = Arc::new(Bridge::new(
        req.env_dir.clone(),
        req.env_id.clone(),
        req.policy_digest.clone(),
        req.box_name.clone(),
        req.transport,
        started.endpoint().to_string(),
        dialer,
    ));

    announce(&Started {
        invite: started.invite(&req, &secret, &grant_id, expires_at),
        how: started.how(),
        grant_id,
        expires_at,
        transport: req.transport,
        warning,
    });

    let outcome = tokio::select! {
        r = started.serve(bridge.clone(), req.direct_only) => r,
        _ = interrupted() => Ok(()),
        reason = stopped_elsewhere(bridge.clone()) => {
            eprintln!("share: {reason}");
            Ok(())
        }
    };

    // Shut the transport down *first*, then wait for the connections it was
    // carrying to actually finish. Closing the endpoint tells them to stop; it
    // does not join them, and they are detached tasks. Writing the receipt
    // straight afterwards is a race that a fast network usually wins and a slow
    // one usually loses, and what it loses is the bytes and closing times of
    // every peer still mid-copy — the half of a share a reviewer most wants.
    started.shutdown().await;
    bridge.quiesce(QUIESCE).await;
    // Every path out writes the receipt and takes the session file with it, so
    // `share ls` describes what is running rather than what once ran.
    bridge.write_receipt();
    session::clear(&req.env_dir);
    outcome
}

/// Resolves when the operator asks this process to stop.
///
/// `SIGTERM` as well as Ctrl-C, because closing the terminal, a `kill`, or a
/// process supervisor tidying up are all ordinary ways a foreground command
/// ends — and handling only the interrupt means the ingress receipt is lost in
/// exactly those cases, which are the ones nobody planned for.
async fn interrupted() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                arm_second_signal();
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
        arm_second_signal();
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Make a second interrupt end the process, rather than doing nothing.
///
/// Handling a signal means the default disposition is gone for the rest of this
/// process's life, so after the first Ctrl-C a second one — and a plain
/// `kill` — would be swallowed. The orderly shutdown that follows is bounded
/// (the endpoint's close, the drain, the quiesce) but it is not instant, and an
/// operator pressing Ctrl-C twice is asking for it to stop now. They lose the
/// receipt, which is the trade they just made.
#[cfg(unix)]
fn arm_second_signal() {
    tokio::spawn(async {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).ok();
        let again = async {
            match term.as_mut() {
                Some(t) => {
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {}
                        _ = t.recv() => {}
                    }
                }
                None => {
                    let _ = tokio::signal::ctrl_c().await;
                }
            }
        };
        again.await;
        eprintln!("share: interrupted again — exiting without writing the receipt");
        std::process::exit(130);
    });
}

/// Resolves when the grant table stops admitting anyone — `share stop` from
/// another terminal, a revoke of the last grant, or simple expiry.
async fn stopped_elsewhere(bridge: Arc<Bridge>) -> String {
    loop {
        tokio::time::sleep(STOP_POLL).await;
        if bridge.is_spent() {
            return "this share was stopped or has expired".to_string();
        }
    }
}

/// The transport, once it is running. Kept as an enum rather than a trait
/// because there are exactly two and they differ in what they hand back, not in
/// how they are driven.
enum Setup {
    #[cfg(feature = "p2p")]
    P2p {
        endpoint_id: String,
        addr: serde_json::Value,
        endpoint: iroh::Endpoint,
    },
    Tunnel {
        tunnel: crate::tunnel::Tunnel,
        listener: Option<tokio::net::TcpListener>,
    },
}

impl Setup {
    async fn start(req: &Request) -> Result<Setup, H5iError> {
        match req.transport {
            Transport::P2p => {
                #[cfg(feature = "p2p")]
                {
                    let endpoint = crate::p2p::bind_sharer().await?;
                    let (endpoint_id, addr) = crate::p2p::addressing(&endpoint).await?;
                    Ok(Setup::P2p {
                        endpoint_id,
                        addr,
                        endpoint,
                    })
                }
                #[cfg(not(feature = "p2p"))]
                {
                    Err(H5iError::Metadata(
                        "this h5i was built without the peer-to-peer transport. Use \
                         `--tunnel`, or install a build with default features."
                            .into(),
                    ))
                }
            }
            Transport::Tunnel => {
                // Loopback only, and `cloudflared` is pointed at it. What
                // reaches the internet is its outbound connection, never a port
                // on this machine.
                let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                    .await
                    .map_err(|e| H5iError::Metadata(format!("could not bind a local port: {e}")))?;
                let local = listener.local_addr()?;
                let tunnel = crate::tunnel::start(local.port()).await?;
                Ok(Setup::Tunnel {
                    tunnel,
                    listener: Some(listener),
                })
            }
        }
    }

    fn endpoint(&self) -> &str {
        match self {
            #[cfg(feature = "p2p")]
            Setup::P2p { endpoint_id, .. } => endpoint_id,
            Setup::Tunnel { tunnel, .. } => tunnel.origin(),
        }
    }

    /// The thing the sharer sends to the other person.
    ///
    /// Takes the grant's own id and expiry rather than recomputing them: the
    /// ticket a peer holds and the row `share status` prints have to describe
    /// the same grant, and two derivations of "now plus the expiry" are two
    /// chances for them not to.
    fn invite(&self, req: &Request, secret: &str, grant_id: &str, expires_at: i64) -> String {
        // A ticket is a peer-to-peer thing. Without that transport compiled in
        // there is nothing to put in one, but the signature stays the same so a
        // feature flag does not change the shape of this module.
        #[cfg(not(feature = "p2p"))]
        let _ = (req, grant_id, expires_at);
        match self {
            #[cfg(feature = "p2p")]
            Setup::P2p { addr, .. } => crate::ticket::Ticket {
                v: 1,
                box_id: req.env_id.clone(),
                port: req.port,
                grant: grant_id.to_string(),
                expires_at,
                secret: secret.to_string(),
                addr: addr.clone(),
            }
            .encode()
            .unwrap_or_else(|e| format!("<could not encode the ticket: {e}>")),
            Setup::Tunnel { tunnel, .. } => crate::tunnel::invite_url(tunnel.origin(), secret),
        }
    }

    fn how(&self) -> String {
        match self {
            #[cfg(feature = "p2p")]
            Setup::P2p { .. } => {
                "they run `h5i join <ticket>` — peer to peer, end-to-end encrypted".into()
            }
            Setup::Tunnel { .. } => {
                "they open the link in any browser — no h5i needed, and Cloudflare can read this \
                 traffic"
                    .into()
            }
        }
    }

    async fn serve(&mut self, bridge: Arc<Bridge>, direct_only: bool) -> Result<(), H5iError> {
        // Likewise: relaying is a peer-to-peer question, and a tunnel-only
        // build has no relay to refuse.
        #[cfg(not(feature = "p2p"))]
        let _ = direct_only;
        match self {
            #[cfg(feature = "p2p")]
            Setup::P2p { endpoint, .. } => {
                crate::p2p::serve(bridge, endpoint.clone(), direct_only).await
            }
            Setup::Tunnel { listener, .. } => {
                let Some(listener) = listener.take() else {
                    return Ok(());
                };
                crate::tunnel::serve(bridge, listener).await
            }
        }
    }

    async fn shutdown(&mut self) {
        match self {
            #[cfg(feature = "p2p")]
            Setup::P2p { endpoint, .. } => endpoint.close().await,
            Setup::Tunnel { tunnel, .. } => tunnel.stop().await,
        }
    }
}

// ─── the other verbs ────────────────────────────────────────────────────────

/// Mint another ticket for a share that is already running.
///
/// One ticket admits one peer, so this is how a second person is added — and it
/// is why revocation can be per person rather than all or nothing.
pub fn grant(
    env_dir: &std::path::Path,
    label: Option<String>,
    expire: Duration,
) -> Result<(String, String), H5iError> {
    let expires_at =
        (chrono::Utc::now() + chrono::Duration::from_std(expire).unwrap_or_default()).timestamp();
    let (g, secret) = session::mint_grant(label, expires_at)?;
    let id = g.id.clone();
    let sess = session::update(env_dir, |s| {
        if !session::is_live(s) {
            return Err(H5iError::Metadata(
                "the process serving this share is gone, so a new ticket would not reach \
                 anything. Start a fresh share."
                    .into(),
            ));
        }
        // Every reason to refuse is checked *before* the grant goes in the
        // table. A grant written and then refused is a grant whose secret was
        // printed nowhere: unusable by anyone, and still counted as live, so
        // the share it belongs to could never expire on its own.
        if s.transport == Transport::P2p {
            // The addressing a ticket needs is the running endpoint's, and the
            // session file records only its id. Re-deriving it here would be a
            // second source of truth for something that must match exactly, so
            // this verb is honest about what it cannot do alone.
            return Err(H5iError::Metadata(
                "adding a peer to a peer-to-peer share is not built yet: a new ticket needs the \
                 running endpoint's addressing, and only the serving process has it. Start a \
                 second share, or use `--tunnel`."
                    .into(),
            ));
        }
        s.grants.push(g);
        Ok(s.clone())
    })?;
    Ok((id, crate::tunnel::invite_url(&sess.endpoint, &secret)))
}

/// Revoke one grant. The share keeps serving everyone else.
pub fn revoke(env_dir: &std::path::Path, grant_id: &str) -> Result<(), H5iError> {
    session::update(env_dir, |s| {
        let found = s.grants.iter_mut().find(|g| g.id == grant_id);
        match found {
            Some(g) => {
                g.revoked = true;
                Ok(())
            }
            None => Err(H5iError::Metadata(format!(
                "this share has no grant `{grant_id}` — `h5i box share status` lists them"
            ))),
        }
    })
}

/// Stop a share running in another terminal.
///
/// Implemented as "revoke everything" rather than as a signal, and that is the
/// safer shape: the serving process notices within a second, drops its live
/// connections, writes its receipt and clears the session file on its own way
/// out. Killing it would skip the receipt, which is the part that matters.
pub fn stop(env_dir: &std::path::Path) -> Result<(), H5iError> {
    session::update(env_dir, |s| {
        for g in &mut s.grants {
            g.revoked = true;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_share_of_the_same_box_is_refused() {
        // Two shares would be two grant tables in one file, and the loser's
        // tickets would stop working with no explanation to whoever held one.
        let dir = tempfile::tempdir().expect("tempdir");
        let s = ShareSession::new(
            "env/a/demo",
            3000,
            Transport::P2p,
            "abc",
            chrono::Utc::now(),
        );
        session::write(dir.path(), &s).expect("write");
        let err = session::claim(dir.path(), &s).expect_err("already shared");
        assert!(format!("{err}").contains("already being shared"));
    }

    #[test]
    fn a_share_record_left_by_a_crash_is_taken_over_rather_than_obeyed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut dead = ShareSession::new(
            "env/a/demo",
            3000,
            Transport::P2p,
            "abc",
            chrono::Utc::now(),
        );
        dead.pid = 0;
        session::write(dir.path(), &dead).expect("write");

        let fresh = ShareSession::new(
            "env/a/demo",
            4000,
            Transport::Tunnel,
            "https://x.trycloudflare.com",
            chrono::Utc::now(),
        );
        let cleared = session::claim(dir.path(), &fresh).expect("a dead share must not block one");
        assert_eq!(cleared, Some(0), "the operator should be told what was cleared");
        assert_eq!(session::read(dir.path()).expect("read").port, 4000);
    }

    #[test]
    fn stopping_revokes_every_grant_so_the_serving_process_can_finish_properly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut s = ShareSession::new(
            "env/a/demo",
            3000,
            Transport::Tunnel,
            "https://x.trycloudflare.com",
            chrono::Utc::now(),
        );
        let (a, secret_a) = session::mint_grant(None, 4_000_000_000).unwrap();
        let (b, secret_b) = session::mint_grant(None, 4_000_000_000).unwrap();
        s.grants = vec![a, b];
        session::write(dir.path(), &s).expect("write");

        stop(dir.path()).expect("stop");
        let after = session::read(dir.path()).expect("read");
        assert!(after.is_spent(0), "a stopped share must admit nobody");
        assert!(after.authorize(&secret_a, 0).is_err());
        assert!(after.authorize(&secret_b, 0).is_err());
    }

    #[test]
    fn revoking_one_peer_leaves_the_others_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut s = ShareSession::new(
            "env/a/demo",
            3000,
            Transport::Tunnel,
            "https://x.trycloudflare.com",
            chrono::Utc::now(),
        );
        let (a, secret_a) = session::mint_grant(Some("alex".into()), 4_000_000_000).unwrap();
        let (b, secret_b) = session::mint_grant(Some("sam".into()), 4_000_000_000).unwrap();
        let id_a = a.id.clone();
        s.grants = vec![a, b];
        session::write(dir.path(), &s).expect("write");

        revoke(dir.path(), &id_a).expect("revoke");
        let after = session::read(dir.path()).expect("read");
        assert!(after.authorize(&secret_a, 0).is_err());
        assert!(after.authorize(&secret_b, 0).is_ok());
        assert!(!after.is_spent(0));
    }

    #[test]
    fn revoking_a_grant_that_is_not_there_says_so() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = ShareSession::new(
            "env/a/demo",
            3000,
            Transport::Tunnel,
            "https://x.trycloudflare.com",
            chrono::Utc::now(),
        );
        session::write(dir.path(), &s).expect("write");
        let err = revoke(dir.path(), "nosuch01").expect_err("unknown grant");
        assert!(format!("{err}").contains("nosuch01"));
    }

    #[test]
    fn a_refused_grant_leaves_no_trace_in_the_table() {
        // The bug this pins: minting first and refusing afterwards wrote a
        // grant whose secret was printed nowhere. Nobody could use it, and it
        // still counted as live — so the share could never expire on its own.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut s = ShareSession::new(
            "env/a/demo",
            3000,
            Transport::P2p,
            "abc",
            chrono::Utc::now(),
        );
        let (expiring, _) = session::mint_grant(None, 1_000).unwrap();
        s.grants = vec![expiring];
        session::write(dir.path(), &s).expect("write");

        let err = grant(dir.path(), None, Duration::from_secs(600)).expect_err("p2p refuses");
        assert!(format!("{err}").contains("peer-to-peer"));

        let after = session::read(dir.path()).expect("read");
        assert_eq!(after.grants.len(), 1, "a refused grant was written anyway");
        assert!(after.is_spent(2_000), "a phantom grant kept the share alive");
    }

    #[test]
    fn a_new_ticket_for_a_dead_share_is_refused() {
        // Otherwise `grant` cheerfully hands out a ticket for a share whose
        // process died, and the peer gets a connection error they cannot read.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut s = ShareSession::new(
            "env/a/demo",
            3000,
            Transport::Tunnel,
            "https://x.trycloudflare.com",
            chrono::Utc::now(),
        );
        s.pid = 0;
        session::write(dir.path(), &s).expect("write");
        let err = grant(dir.path(), None, Duration::from_secs(600)).expect_err("dead share");
        assert!(format!("{err}").contains("gone"));
    }
}
