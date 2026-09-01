//! Starting, describing and ending a share.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use h5i_error::H5iError;

use crate::bridge::Bridge;
use crate::dialer::Dialer;
use crate::session::{self, ShareSession, Transport};

/// How often the share checks whether it has been stopped from elsewhere.
const STOP_POLL: Duration = Duration::from_secs(1);

/// How often the share checks that the box still has a session.
///
/// Slower than the revoke poll: a box going away is not a security event, and
/// the check walks a directory.
const BOX_POLL: Duration = Duration::from_secs(3);

/// How long the connections get to close themselves, with a reason, before the
/// transport closes them without one.
const SHUTDOWN_GRACE: Duration = Duration::from_millis(200);

/// How long to wait, after the transport is closed, for the connections it was
/// carrying to finish recording what they moved.
const QUIESCE: Duration = Duration::from_secs(5);

/// What `h5i box share` was asked for.
pub struct Request {
    pub env_dir: PathBuf,
    pub env_id: String,
    pub policy_digest: String,
    pub box_name: String,
    /// The pid that identifies the box, which means different things on the
    /// two platforms because they bound a box differently: on Linux, a pid
    /// living *inside* the box's namespaces; on macOS, the box's *session*
    /// process, whose descendants are the box.
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
    // dialling a network. It is *not* the check that matters: `session::claim`
    // re-does it under the lock, because between here and there is a window two
    // starts could both walk through.
    if let Some(existing) = session::read(&req.env_dir)
        && session::is_live(&existing)
    {
        return Err(session::already_shared(&existing, &req.box_name));
    }

    // Before the runtime. See the module note; this is the whole reason this
    // function is not simply `async`.
    let dialer = Dialer::spawn(req.box_pid, req.port)?;
    // What the dialer is pinned to, so the share can notice if the box later
    // becomes a different box. Asked of the *dialer*, which is holding the
    // route, rather than re-read off the box process: `spawn` returns after its
    // helper has entered the namespace, and if the box process exited in that
    // window the read returned `None` while the helper held the namespace
    // fine, and `box_went_away` read `None` as "nothing to compare" and
    // skipped restart detection for the whole share. See
    // `Dialer::pinned_route`.
    let Some(pinned_route) = dialer.pinned_route() else {
        // Refused rather than served with the check off. A share that cannot
        // establish what it is pinned to cannot notice the box being replaced
        // under it, and its public URL would go on claiming to work.
        return Err(H5iError::Metadata(format!(
            "h5i could not establish which network the route into `{}` holds, so a share of \
             it could not tell the box restarting from the box still being there — and would \
             keep advertising a URL that reaches nothing. Start a fresh session and try again.",
            req.box_name
        )));
    };

    // A share of a port with nothing behind it is almost always a mistake, and
    // the peer is the one who would find out. Warn rather than refuse: an agent
    // that is about to start its dev server is a perfectly good reason to share
    // a port that is not up yet.
    let warning = match dialer.connect() {
        Ok(_) => None,
        // Refused, not warned.
        Err(e) if e.fatal() => {
            let loopback = e.no_loopback();
            // The inner error's own text, not its `Display`: wrapping one
            // `H5iError` in another prints "Metadata error: Metadata error:".
            let H5iError::Metadata(said) = e.into_inner() else {
                return Err(H5iError::Metadata("this box cannot be shared".into()));
            };
            if !loopback {
                // A port held by something that is not the box (macOS). The
                // dialer's message already names what holds it and what to do;
                // the namespace advice below would be wrong here. There is no
                // profile that changes who owns a host port.
                return Err(H5iError::Metadata(said));
            }
            return Err(H5iError::Metadata(format!(
                "{said}\n   This is decided by the *profile*, not the tier: a profile that \
                 denies egress gets a namespace of its own with nothing brought up in it, at \
                 every tier. Create the box with an agent profile — `--profile agent`, \
                 `agent-claude` or `agent-codex` — which get an egress allowlist and a working \
                 loopback with it."
            )));
        }
        // The reason is carried through rather than assumed.
        Err(e) if e.nothing_listening() => Some(format!(
            "nothing is listening on port {} inside the box yet — peers will get an error \
             until the dev server starts",
            req.port
        )),
        Err(e) => Some(format!(
            "h5i could not reach inside the box to check port {}: {e}. This is not your dev \
             server; peers will get an error until the share is restarted.",
            req.port
        )),
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| H5iError::Metadata(format!("could not start the share runtime: {e}")))?;

    let out = runtime
        .block_on(async move { serve_async(req, dialer, warning, pinned_route, announce).await });
    // Not a plain drop. `open_upstream` runs under `spawn_blocking`, blocking
    // tasks are never cancelled, and the dialer's reply is bounded only by
    // `CONNECT_TIMEOUT`, so a dev server that accepts nothing meant this
    // command sat there for ten more seconds after printing everything it had
    // to say, which is an operator's whole experience of a hang.
    runtime.shutdown_timeout(Duration::from_millis(200));
    out
}

async fn serve_async(
    req: Request,
    dialer: Dialer,
    warning: Option<String>,
    pinned_netns: String,
    announce: impl FnOnce(&Started),
) -> Result<(), H5iError> {
    // The box is claimed before the network is touched.
    let mut sess = ShareSession::new(&req.env_id, req.port, req.transport, "", chrono::Utc::now());
    sess.starting = true;
    match session::claim(&req.env_dir, &sess, &req.box_name) {
        Ok(Some(stale)) => eprintln!(
            "share: cleared a leftover share record from pid {stale} (that process is gone)"
        ),
        Ok(None) => {}
        Err(e) => return Err(e),
    }
    // The identity of the record just claimed, held here so every teardown
    // mutation below can name the share it is ending. Without it, a bridge
    // whose record was force-deleted and replaced marked the *new* share as
    // winding up on its way out.
    let claimed_at = sess.started_at.clone();

    let mut started = match Setup::start(&req).await {
        Ok(s) => s,
        Err(e) => {
            // The claim goes with the failure. Leaving it would refuse the
            // next `box share` of this box until somebody found `--force`.
            session::clear(&req.env_dir, &claimed_at);
            return Err(e);
        }
    };

    // Everything that may have changed while setup was waiting, checked before
    // a single byte is announced.
    if let Err(e) = still_ours(&req, &claimed_at, &pinned_netns) {
        started.shutdown().await;
        session::clear(&req.env_dir, &claimed_at);
        return Err(e);
    }

    // The endpoint and the first grant, written in one locked step that also
    // takes the record out of `starting`. The grant is minted *inside* that
    // step for the two reasons `run::grant` gives: its lifetime should start
    // when it is recorded, not up to five seconds earlier while the lock was
    // being acquired, and its id has to be one the table is not already using.
    let minted = std::cell::RefCell::new(None);
    let endpoint = started.endpoint().to_string();
    let updated = session::update(&req.env_dir, |s| {
        if s.pid != std::process::id() || s.started_at != claimed_at {
            return Err(H5iError::Metadata(
                "this box was claimed by another share while this one was starting. Nothing was \
                 announced."
                    .into(),
            ));
        }
        // `share stop` reached the starting record and marked it. That is an
        // operator saying no, and it arrived first.
        if s.winding_up {
            return Err(H5iError::Metadata(
                "this share was stopped while it was starting, so it will not open. Run \
                 `h5i box share` again if that was not what you meant."
                    .into(),
            ));
        }
        let expires_at = (chrono::Utc::now()
            + chrono::Duration::from_std(req.expire).unwrap_or_default())
        .timestamp();
        let (grant, secret) = session::mint_grant_unlike(&s.grants, req.label.clone(), expires_at)?;
        *minted.borrow_mut() = Some((grant.id.clone(), secret, expires_at));
        s.grants.push(grant);
        s.endpoint = endpoint.clone();
        s.starting = false;
        Ok(())
    });
    if let Err(e) = updated {
        started.shutdown().await;
        session::clear(&req.env_dir, &claimed_at);
        return Err(e);
    }
    let (grant_id, secret, expires_at) = minted
        .into_inner()
        .expect("update ran its closure to completion");
    sess.endpoint = started.endpoint().to_string();
    sess.starting = false;

    let bridge = Arc::new(Bridge::new(
        req.env_dir.clone(),
        req.env_id.clone(),
        req.policy_digest.clone(),
        req.box_name.clone(),
        req.transport,
        started.endpoint().to_string(),
        dialer,
        // Pinned to the record just claimed, so every later read of
        // `share.json` is a read of *ours*. `share stop --force` deletes a
        // live record without stopping its process and a fresh `box share` may
        // legitimately claim the path a moment later; without this the old
        // bridge kept serving the old port under the new share's grants.
        crate::bridge::ClaimedRecord {
            pid: sess.pid,
            started_at: sess.started_at.clone(),
        },
    ));

    announce(&Started {
        invite: started.invite(&req, &secret, &grant_id, expires_at),
        how: started.how(),
        grant_id,
        expires_at,
        transport: req.transport,
        warning,
    });

    // The `bool` is "a signal has already been delivered", and it decides who
    // owns the *next* one. On this branch `interrupted()` has armed the
    // hard-exit watcher, so a second Ctrl-C exits without a receipt, as
    // promised, and the teardown below must not also race for it.
    let (outcome, already_signalled) = tokio::select! {
        r = started.serve(bridge.clone(), req.direct_only) => (r, false),
        _ = interrupted(&req.env_dir, &claimed_at) => (Ok(()), true),
        reason = stopped_elsewhere(bridge.clone()) => {
            eprintln!("share: {reason}");
            (Ok(()), false)
        }
        reason = box_went_away(req.env_dir.clone(), pinned_netns.clone()) => {
            eprintln!("share: {reason}");
            (Ok(()), false)
        }
    };

    // Say so on disk before doing any of it.
    if let Err(e) = session::begin_winding_up(&req.env_dir, &bridge.claimed().started_at) {
        eprintln!(
            "share: could not mark this share as shutting down ({e}). If `h5i box share grant` \
             ran in the last few seconds, the ticket it printed may already be dead."
        );
    }

    // The teardown, and a way out of the *waiting* part of it.
    let waited = if already_signalled {
        // The hard-exit watcher is armed and owns the next signal. Racing it
        // here would make a second Ctrl-C do one of two different things
        // depending on which task woke first.
        teardown(&bridge, &mut started).await;
        true
    } else {
        tokio::select! {
            _ = teardown(&bridge, &mut started) => true,
            _ = interrupted(&req.env_dir, &claimed_at) => {
                eprintln!(
                    "share: not waiting for connections to finish — the receipt is still written"
                );
                false
            }
        }
    };
    if !waited {
        // The transport still has to go, or `cloudflared` outlives this
        // process. Not awaited on the interrupted path beyond its own bounds.
        started.shutdown().await;
        // Nothing to record here. `settled` starts false and only a completed
        // `quiesce` sets it true, so abandoning the teardown leaves it false by
        // construction and the receipt already says it is partial. The explicit
        // call this used to make was worse than redundant: both arms of the
        // `select!` above can become ready in the same poll and the pick is
        // random, so a run whose quiesce *had* completed could still land here
        // and unmark it. A receipt calling itself partial when it had waited.
    }
    // Every path out writes the receipt and takes the session file with it, so
    // `share ls` describes what is running rather than what once ran.
    // Which receipt depends on how it ended. A share whose tunnel died did not
    // succeed, and a reader of the export should not have to guess.
    match &outcome {
        Ok(()) => bridge.write_receipt(),
        // The reason goes in the body. Without it a reader who opened the
        // receipt to find out why the box was flagged saw a body identical to
        // a successful share's, and quick tunnels drop routinely.
        Err(e) => bridge.write_receipt_failed(&e.to_string()),
    }
    session::clear(&req.env_dir, &bridge.claimed().started_at);
    outcome
}

/// The orderly half: tell the connections, then the transport, then wait.
async fn teardown(bridge: &Arc<Bridge>, started: &mut Setup) {
    // Announced, so a test can wait for the window rather than guess at it.
    // The end-to-end check for "a first Ctrl-C during the teardown" slept 0.4s
    // after `share stop` and signalled, but the serving process only learns
    // about a stop by polling at `STOP_POLL`, so at 0.4s it was still in the
    // main select and the signal landed on the ordinary Ctrl-C path. The check
    // passed, for a byte-for-byte repeat of the test above it.
    eprintln!("share: shutting down");
    // Tell the connections first, tear the transport down second. `iroh`'s
    // `Endpoint::close` closes every connection with code `0` and an empty
    // reason, so a connection that wanted to close with an explanation has to
    // have done it before that runs. Otherwise the joiner is told "closed by
    // peer: 0" for a ticket that simply expired.
    bridge.begin_shutdown();
    tokio::time::sleep(SHUTDOWN_GRACE).await;

    // Then the transport, then wait for the connections it was
    // carrying to actually finish. Closing the endpoint tells them to stop; it
    // does not join them, and they are detached tasks. Writing the receipt
    // straight afterwards is a race that a fast network usually wins and a slow
    // one usually loses, and what it loses is the bytes and closing times of
    // every peer still mid-copy. The half of a share a reviewer most wants.
    started.shutdown().await;
    bridge.quiesce(QUIESCE).await;
}

/// Resolves when the operator asks this process to stop.
///
/// `SIGTERM` as well as Ctrl-C, because closing the terminal, a `kill`, or a
/// process supervisor tidying up are all ordinary ways a foreground command
/// ends, and handling only the interrupt means the ingress receipt is lost in
/// exactly those cases, which are the ones nobody planned for.
async fn interrupted(env_dir: &std::path::Path, started_at: &str) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                arm_second_signal(env_dir, started_at);
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
        arm_second_signal(env_dir, started_at);
    }
    #[cfg(not(unix))]
    {
        // Both, because the hard-exit watcher they identify a share to is
        // `#[cfg(unix)]`: there is no second-signal disposition to arm here.
        let _ = (env_dir, started_at);
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Make a second interrupt end the process, rather than doing nothing.
///
/// Handling a signal means the default disposition is gone for the rest of this
/// process's life, so after the first Ctrl-C a second one, and a plain
/// `kill`, would be swallowed. The orderly shutdown that follows is bounded
/// (the endpoint's close, the drain, the quiesce) but it is not instant, and an
/// operator pressing Ctrl-C twice is asking for it to stop now. They lose the
/// receipt, which is the trade they just made.
#[cfg(unix)]
fn arm_second_signal(env_dir: &std::path::Path, started_at: &str) {
    let started_at = started_at.to_string();
    // Armed at most once. The normal shutdown path calls this too, so on a
    // Ctrl-C both calls happen and a second watcher would race the first to
    // `exit(130)`: harmless, but it also means two handlers for one signal.
    static ARMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if ARMED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let env_dir = env_dir.to_path_buf();
    tokio::spawn(async move {
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
        // The receipt is the trade the operator just made. The *record* is not:
        // leaving `share.json` behind means the next `share ls` shows a share
        // that is gone and the next `share` refuses to start, which is a mess
        // made by this exit rather than chosen by them.
        //
        // Unlocked, because the lock this would wait on is most likely held by
        // this process's own orderly shutdown, the one being abandoned, and
        // five seconds of retrying is not what "stop now" means.
        session::clear_now(&env_dir, &started_at);
        std::process::exit(130);
    });
}

/// Resolves when the grant table stops admitting anyone: `share stop` from
/// another terminal, a revoke of the last grant, or simple expiry.
async fn stopped_elsewhere(bridge: Arc<Bridge>) -> String {
    loop {
        tokio::time::sleep(STOP_POLL).await;
        if bridge.is_spent() {
            return "this share was stopped or has expired".to_string();
        }
    }
}

/// Resolves when the box stops having a session of its own.
fn still_ours(req: &Request, claimed_at: &str, pinned: &str) -> Result<(), H5iError> {
    let writers = h5i_core::env::live_sessions(&req.env_dir)
        .iter()
        .any(|s| h5i_core::env::live_is_writer(&s.kind));
    if !writers {
        return Err(H5iError::Metadata(format!(
            "`{}` stopped running while this share was starting, so there is nothing left to \
             share. Start a session and share again.",
            req.box_name
        )));
    }
    if current_route(&req.env_dir).as_deref() != Some(pinned) {
        return Err(H5iError::Metadata(format!(
            "`{}` restarted while this share was starting, so the route this share holds goes \
             into a box that is no longer there. Start a fresh share.",
            req.box_name
        )));
    }
    // And the claim is still ours to announce. `update` below re-checks this
    // under the lock; this is the early, cheap half of the same question.
    match session::read(&req.env_dir) {
        Some(s) if s.started_at == claimed_at && s.pid == std::process::id() => Ok(()),
        _ => Err(H5iError::Metadata(
            "this share's claim on the box was taken over or removed while it was starting. \
             Nothing was announced."
                .into(),
        )),
    }
}

async fn box_went_away(env_dir: PathBuf, pinned: String) -> String {
    loop {
        tokio::time::sleep(BOX_POLL).await;
        // Only *writers* count. A read-only observer is a session, and it kept
        // this loop quiet after the box's real session had gone, so somebody
        // could `h5i box abort` a box, be told it was aborted, and have a
        // public tunnel URL keep pointing at it for the rest of the ticket's
        // life while `share ls` reported it healthy. An observer holds a
        // worktree open; it is not a box that is running.
        let writers = h5i_core::env::live_sessions(&env_dir)
            .iter()
            .any(|s| h5i_core::env::live_is_writer(&s.kind));
        if !writers {
            return "the box is no longer running, so there is nothing left to share \
                    (start a session and share again)"
                .to_string();
        }
        // "Is there *any* session" was the whole test, and it is not the question.
        {
            let now = current_route(&env_dir);
            if now.as_deref() != Some(pinned.as_str()) {
                // Worded for what both platforms have in common, because both
                // have this failure: on Linux the restarted box has a new
                // namespace this share cannot enter, and on macOS it is a new
                // process tree, so the dialer would attribute the port to a
                // box that no longer exists, and refuse every connection.
                return "the box restarted, so this share is pinned to a box that is no longer \
                        there. Nothing it serves can reach the box any more — start a fresh \
                        share."
                    .to_string();
            }
        }
    }
}

/// What the dialer is pinned to, named so it can be compared later.
///
/// The two platforms pin different things, because they identify a box
/// differently, and both change when a box restarts:
///
/// * *Linux*: the network namespace. `/proc/<pid>/ns/net` reads as
///   `net:[4026536311]`, and that number is new for every session.
/// * *macOS*: the session process itself. There is no namespace to name, and
///   the dialer's whole notion of "the box" is the tree under that pid.
fn pinned_route(box_pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_link(format!("/proc/{box_pid}/ns/net"))
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    }
    #[cfg(not(target_os = "linux"))]
    {
        Some(format!("session {box_pid}"))
    }
}

/// The same identity, for the box as it is *now*. `None` when there is nothing
/// to compare against, which the caller treats as "what we pinned is gone".
fn current_route(env_dir: &std::path::Path) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        pinned_route(h5i_core::view::box_pid(env_dir)?)
    }
    #[cfg(not(target_os = "linux"))]
    {
        pinned_route(h5i_core::view::session_pid(env_dir)?)
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
            Setup::Tunnel { tunnel, listener } => {
                let Some(listener) = listener.take() else {
                    return Ok(());
                };
                tokio::select! {
                    r = crate::tunnel::serve(bridge, listener) => r,
                    // The tunnel dying is the end of the share, and it has to
                    // say so: the URL in the terminal stops working and nothing
                    // else would ever mention it.
                    reason = tunnel.died() => Err(H5iError::Metadata(format!(
                        "{reason} — the public URL for this share is gone"
                    ))),
                }
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
/// One ticket admits one peer, so this is how a second person is added, and it
/// is why revocation can be per person rather than all or nothing.
pub fn grant(
    env_dir: &std::path::Path,
    label: Option<String>,
    expire: Duration,
) -> Result<Minted, H5iError> {
    // Everything about the grant is decided *inside* the closure, which runs with `share.lock`
    // held.
    let minted = std::cell::RefCell::new(None);
    let sess = session::update(env_dir, |s| {
        if !session::is_live(s) {
            return Err(H5iError::Metadata(
                "the process serving this share is gone, so a new ticket would not reach \
                 anything. Start a fresh share."
                    .into(),
            ));
        }
        if s.winding_up {
            return Err(H5iError::Metadata(
                "this share is shutting down, so a ticket minted now would be deleted with \
                 the rest of the table a moment later. Start a fresh share."
                    .into(),
            ));
        }
        // Every reason to refuse is checked *before* the grant goes in the
        // table. A grant written and then refused is a grant whose secret was
        // printed nowhere: unusable by anyone, and still counted as live, so
        // the share it belongs to could never expire on its own.
        if s.transport == Transport::P2p {
            // The addressing a ticket needs is the running endpoint's, and the
            // session file records only its id. Re-deriving it here would be a second
            // source of truth for something that must match exactly.
            //
            // Naming the procedure that works, rather than one that does not. "Start
            // a second share" was the advice, and starting a second share is refused
            // because this one is running, so the two messages pointed at each other
            // and somebody wanting a second viewer could alternate between them
            // forever.
            return Err(H5iError::Metadata(
                "a peer-to-peer share carries one ticket: a second one would need the running \
                 endpoint's addressing, and only the serving process has that. To add \
                 somebody, stop this share and start a new one — everybody gets a fresh \
                 ticket — or share with `--tunnel`, where `grant` mints extra links."
                    .into(),
            ));
        }
        // Anchored here, with the lock held: from this instant the peer really
        // does get the duration they were promised.
        let now = chrono::Utc::now();
        // …by *this* process's clock, which is not the one that will judge it.
        if let Some(skew) = session::started_in_the_future(s, now.timestamp()) {
            return Err(H5iError::Metadata(format!(
                "this shell's clock is {} behind the one this share started on, and the \
                 serving process measures expiry against elapsed time — so a ticket minted \
                 now would be refused the moment it was used, while everything here showed \
                 time left. Put this machine's clock right, or stop the share and start a \
                 fresh one.",
                session::humanise(skew)
            )));
        }
        let expires_at = (now + chrono::Duration::from_std(expire).unwrap_or_default()).timestamp();
        let (g, secret) = session::mint_grant_unlike(&s.grants, label.clone(), expires_at)?;
        *minted.borrow_mut() = Some((g.id.clone(), secret, expires_at));
        s.grants.push(g);
        Ok(s.clone())
    })?;
    let (id, secret, expires_at) = minted
        .into_inner()
        .expect("update ran its closure to completion");
    Ok(Minted {
        id,
        invite: crate::tunnel::invite_url(&sess.endpoint, &secret),
        expires_at,
    })
}

/// A grant, as the caller needs to describe it.
///
/// `expires_at` is here because the CLI was computing its own: it re-parsed
/// `--expire`, added it to a fresh `Utc::now()`, and printed that. The grant in
/// the table is anchored at the `now` above, and `session::update` then waits
/// for the lock. Up to five seconds of retries. Measured with a three-second
/// hold, the terminal said `expires in 1m` for a grant that `share status`
/// showed with 58 seconds left. Two clocks for one fact is one clock too many.
#[derive(Debug)]
pub struct Minted {
    pub id: String,
    pub invite: String,
    pub expires_at: i64,
}

/// Revoke one grant. The share keeps serving everyone else.
pub fn revoke(env_dir: &std::path::Path, grant_id: &str) -> Result<(), H5iError> {
    session::update(env_dir, |s| {
        // Checked here for the same reason `grant` checks it: the CLI prints
        // "any connection that peer had is dropped within a second", and
        // against a record left by a crashed process every word of that is
        // false. A share nothing is serving needs `share stop`, not a revoke.
        if !session::is_live(s) {
            return Err(H5iError::Metadata(
                "the process serving this share is gone, so there is nothing to revoke from. \
                 Run `h5i box share stop <name>` to clear the leftover record."
                    .into(),
            ));
        }
        let found = s.grants.iter_mut().find(|g| g.id == grant_id);
        match found {
            Some(g) => {
                g.revoked = true;
                Ok(())
            }
            None => Err(H5iError::Metadata(format!(
                "this share has no grant `{grant_id}` — `h5i box share status {}` lists them",
                s.box_id.rsplit('/').next().unwrap_or(&s.box_id)
            ))),
        }
    })
}

/// What `stop` found when it looked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// A live process was told to wind up; it writes its receipt and exits.
    Serving,
    /// The share had claimed the box and its transport was not up yet, so it
    /// abandons the start instead of tearing one down. No receipt: there was no
    /// session to write one about.
    ///
    /// Without this state the window was invisible. `--tunnel` waits up to
    /// forty-five seconds for a URL and nothing was on disk during it, so `stop`
    /// and `stop --force` both answered "not being shared", printed a success and
    /// returned 0, and the start then announced a public endpoint.
    Starting,
    /// The record was a leftover from a process that is gone, and was removed.
    /// Without this there was no way out of that state at all: `status` told
    /// people to run `stop`, and `stop` revoked grants in a file nobody was
    /// reading and left it exactly where it was.
    Stale,
}

/// Stop a share running in another terminal.
///
/// Implemented as "revoke everything" rather than as a signal, and that is the
/// safer shape: the serving process notices within a second, drops its live
/// connections, writes its receipt and clears the session file on its own way
/// out. Killing it would skip the receipt, which is the part that matters.
pub fn stop(env_dir: &std::path::Path) -> Result<Stopped, H5iError> {
    // Asked before the mutation, because `stop` is what makes the answer
    // false: a share still in `Setup::start` has a record and no transport, and
    // telling the operator it will "write its receipt and exit" would be
    // describing a teardown that never happens.
    let starting = session::read(env_dir).is_some_and(|s| s.starting);
    // One lock hold for the whole decision. See `session::stop`.
    Ok(if session::stop(env_dir)? {
        if starting {
            Stopped::Starting
        } else {
            Stopped::Serving
        }
    } else {
        Stopped::Stale
    })
}

// Same: these drive `run::serve` and its session handling through a real
// dialer.
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
        let err = session::claim(dir.path(), &s, "demo").expect_err("already shared");
        assert!(format!("{err}").contains("already being shared"));
    }

    /// `share stop` reaches a share that has claimed the box and not opened.
    ///
    /// Nothing was written until after `Setup::start`, which for `--tunnel`
    /// waits up to forty-five seconds for a URL. Throughout that window both
    /// `stop` and `stop --force` saw no record, printed "not being shared" and
    /// returned 0, and the start then completed and began admitting visitors.
    /// An operator could explicitly revoke access and have public access begin
    /// afterwards, with no error from the command meant to prevent it.
    #[test]
    fn stopping_a_share_that_has_not_opened_yet_cancels_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut starting = ShareSession::new(
            "env/a/demo",
            3000,
            Transport::Tunnel,
            // No endpoint: the transport is not up. That is what `starting` is.
            "",
            chrono::Utc::now(),
        );
        starting.starting = true;
        session::claim(dir.path(), &starting, "demo").expect("claim the box");

        // The operator says no while setup is still running.
        assert!(matches!(stop(dir.path()).expect("stop"), Stopped::Starting));

        // What the start does when it comes back: the record is marked, so it
        // abandons itself rather than announcing.
        let s = session::read(dir.path()).expect("the record survived");
        assert!(s.winding_up, "stop did not reach a starting share");
        assert!(
            session::update(dir.path(), |s| {
                if s.winding_up {
                    return Err(H5iError::Metadata("stopped while starting".into()));
                }
                s.starting = false;
                Ok(())
            })
            .is_err(),
            "a stopped start went on to open"
        );

        // And a share that opened is not reported as one that had not.
        let dir = tempfile::tempdir().expect("tempdir");
        let open = ShareSession::new(
            "env/a/demo",
            3000,
            Transport::Tunnel,
            "https://x",
            chrono::Utc::now(),
        );
        session::claim(dir.path(), &open, "demo").expect("claim");
        assert!(matches!(stop(dir.path()).expect("stop"), Stopped::Serving));
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
        let cleared =
            session::claim(dir.path(), &fresh, "demo").expect("a dead share must not block one");
        assert_eq!(
            cleared,
            Some(0),
            "the operator should be told what was cleared"
        );
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

        assert_eq!(stop(dir.path()).expect("stop"), Stopped::Serving);
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
        // still counted as live, so the share could never expire on its own.
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
        assert!(
            after.is_spent(2_000),
            "a phantom grant kept the share alive"
        );
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

    #[tokio::test]
    async fn interrupting_the_teardown_skips_the_waiting_and_keeps_the_receipt() {
        // The regression this exists for: arming the hard-exit watcher after the select meant
        // that on the three exits where no signal had been delivered yet the operator's *first*
        // Ctrl-C hit a watcher built for their second.
        let dir = tempfile::tempdir().expect("tempdir");
        let bridge = std::sync::Arc::new(crate::bridge::Bridge::new(
            dir.path().to_path_buf(),
            "env/a/demo".into(),
            "digest".into(),
            "demo".into(),
            Transport::Tunnel,
            "https://x".into(),
            crate::dialer::Dialer::spawn_local(1).expect("dialer"),
            crate::bridge::ClaimedRecord::on_disk(dir.path()),
        ));
        // A connection that never finishes, so the quiesce inside `teardown`
        // really would sit there for its full five seconds.
        let held = bridge.admit().expect("a slot");
        let mut setup = Setup::Tunnel {
            tunnel: crate::tunnel::Tunnel::already_gone_for_tests(),
            listener: None,
        };

        let started = std::time::Instant::now();
        {
            let mut running = std::pin::pin!(teardown(&bridge, &mut setup));
            let cut = tokio::time::timeout(Duration::from_millis(50), &mut running).await;
            assert!(
                cut.is_err(),
                "the teardown finished before it could be cut short"
            );
            // And here the future is dropped, which is what the `select!` does.
        }
        assert!(
            started.elapsed() < QUIESCE,
            "abandoning the teardown still waited it out: {:?}",
            started.elapsed()
        );
        drop(held);

        // The receipt is written after it, on every path. That is the thing an
        // interrupt must not skip.
        bridge.write_receipt();
        let log = std::fs::read_to_string(dir.path().join("receipt.jsonl")).expect("receipt");
        assert!(log.contains(r#""source":"share""#), "{log}");
    }

    #[test]
    fn a_ticket_is_refused_while_the_share_is_winding_up() {
        // The window this closes: `is_live` is `kill(pid, 0)`, and the serving
        // process spends several seconds writing its receipt while very much
        // alive. A grant minted in there printed the one copy of its secret and
        // was then deleted with the rest of the table.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut s = ShareSession::new(
            "env/a/demo",
            3000,
            Transport::Tunnel,
            "https://x.trycloudflare.com",
            chrono::Utc::now(),
        );
        let (g, _) = session::mint_grant(None, 4_000_000_000).unwrap();
        s.grants = vec![g];
        session::write(dir.path(), &s).expect("write");

        grant(dir.path(), None, Duration::from_secs(600)).expect("a live share mints");
        session::begin_winding_up(dir.path(), &s.started_at).expect("mark winding up");
        let err = grant(dir.path(), None, Duration::from_secs(600)).expect_err("winding up");
        assert!(format!("{err}").contains("shutting down"), "{err}");
        assert_eq!(
            session::read(dir.path()).expect("read").grants.len(),
            2,
            "a refused grant was written anyway"
        );

        // And the refusal a second `share` gets says so too, rather than
        // telling somebody to stop a share that is already stopping.
        let err = session::already_shared(&session::read(dir.path()).unwrap(), "demo");
        assert!(format!("{err}").contains("shutting down"), "{err}");
    }

    #[test]
    fn stopping_a_share_shuts_the_door_on_a_racing_grant() {
        // `stop` revokes every grant and lets the serving process notice by
        // polling. A `grant` landing in that gap did not just mint a ticket
        // about to be deleted. It added a *live* grant, which is the very
        // condition the serving process polls for, so the share it was racing
        // could come back from the dead.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut s = ShareSession::new(
            "env/a/demo",
            3000,
            Transport::Tunnel,
            "https://x.trycloudflare.com",
            chrono::Utc::now(),
        );
        let (g, _) = session::mint_grant(None, 4_000_000_000).unwrap();
        s.grants = vec![g];
        session::write(dir.path(), &s).expect("write");

        assert_eq!(stop(dir.path()).expect("stop"), Stopped::Serving);
        let err = grant(dir.path(), None, Duration::from_secs(600)).expect_err("racing grant");
        assert!(format!("{err}").contains("shutting down"), "{err}");
        let after = session::read(dir.path()).expect("read");
        assert!(
            after.is_spent(chrono::Utc::now().timestamp()),
            "a stopped share was brought back to life by a racing grant"
        );
    }

    #[test]
    fn revoking_on_a_share_nothing_is_serving_is_refused() {
        // The CLI prints "any connection that peer had is dropped within a
        // second" on success. Against a record left by a crash, every word of
        // that is false.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut s = ShareSession::new(
            "env/a/demo",
            3000,
            Transport::Tunnel,
            "https://x.trycloudflare.com",
            chrono::Utc::now(),
        );
        let (g, _) = session::mint_grant(None, 4_000_000_000).unwrap();
        let id = g.id.clone();
        s.grants = vec![g];
        s.pid = 0;
        session::write(dir.path(), &s).expect("write");

        let err = revoke(dir.path(), &id).expect_err("dead share");
        assert!(format!("{err}").contains("gone"), "{err}");
        assert!(
            !session::read(dir.path()).unwrap().grants[0].revoked,
            "the table was edited by a verb that refused"
        );
    }
}
