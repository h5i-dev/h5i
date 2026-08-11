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
            return Err(session::already_shared(&existing, &req.box_name));
        }
    }

    // Before the runtime. See the module note; this is the whole reason this
    // function is not simply `async`.
    let dialer = Dialer::spawn(req.box_pid, req.port)?;
    // Which namespace the helper went into, so the share can notice if the box
    // later gets a different one. Read here rather than inside the loop: after
    // the fork this is a fact about what we are pinned to, not about what the
    // box happens to have now.
    let pinned_netns = std::fs::read_link(format!("/proc/{}/ns/net", req.box_pid))
        .ok()
        .map(|p| p.to_string_lossy().into_owned());

    // A share of a port with nothing behind it is almost always a mistake, and
    // the peer is the one who would find out. Warn rather than refuse: an agent
    // that is about to start its dev server is a perfectly good reason to share
    // a port that is not up yet.
    let warning = match dialer.connect() {
        Ok(_) => None,
        // Refused, not warned. This box's namespace has no loopback, so
        // nothing inside it can reach itself and no ticket minted here will
        // ever move a byte — the share would start, print an invite, and leave
        // both people reading messages about a dev server that is running
        // fine. `process` with a profile that denies egress is exactly this
        // shape, and it is a configuration the docs used to recommend.
        Err(e) if e.no_loopback() => {
            // The inner error's own text, not its `Display`: wrapping one
            // `H5iError` in another prints "Metadata error: Metadata error:".
            let H5iError::Metadata(said) = e.into_inner() else {
                return Err(H5iError::Metadata("this box cannot be shared".into()));
            };
            return Err(H5iError::Metadata(format!(
                "{said}\n   This is decided by the *profile*, not the tier: a profile that \
                 denies egress gets a namespace of its own with nothing brought up in it, at \
                 every tier. Create the box with an agent profile — `--profile agent`, \
                 `agent-claude` or `agent-codex` — which get an egress allowlist and a working \
                 loopback with it."
            )));
        }
        // The reason is carried through rather than assumed. "Nothing is
        // listening yet" is the overwhelmingly common cause and the only one
        // worth advice, but it is not the only way this fails — the helper can
        // have failed to enter the namespace, or the channel can have been
        // retired — and telling somebody to start their dev server when the
        // dev server is not the problem is an afternoon spent in the wrong
        // place.
        // Using the classifier, not assuming. `DialError` knows which of the
        // two this is, and this is the only place a person sees the answer
        // *while* the share is running — the receipt, which they read
        // afterwards, was the half that got fixed. Telling somebody to start a
        // dev server that is already running is the whole defect.
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
        .block_on(async move { serve_async(req, dialer, warning, pinned_netns, announce).await });
    // Not a plain drop. `open_upstream` runs under `spawn_blocking`, blocking
    // tasks are never cancelled, and the dialer's reply is bounded only by
    // `CONNECT_TIMEOUT` — so a dev server that accepts nothing meant this
    // command sat there for ten more seconds after printing everything it had
    // to say, which is an operator's whole experience of a hang.
    runtime.shutdown_timeout(Duration::from_millis(200));
    out
}

async fn serve_async(
    req: Request,
    dialer: Dialer,
    warning: Option<String>,
    pinned_netns: Option<String>,
    announce: impl FnOnce(&Started),
) -> Result<(), H5iError> {
    // Transport setup first: it decides the endpoint the session records, and
    // it is the step most likely to fail (no network, no cloudflared). Failing
    // before anything is written keeps a dead share.json off disk.
    let mut started = Setup::start(&req).await?;

    // Minted *after* that, not before. `--tunnel` waits up to 45 seconds for
    // `cloudflared` to publish a URL, and the clock was started before the
    // wait: `--tunnel --expire 30s` printed a success tick, a complete
    // ticket, and `expires in 0s` — a ticket already dead when it was handed
    // over, and printed once. Whoever asked for thirty seconds meant thirty
    // seconds of somebody being able to use it.
    let expires_at = (chrono::Utc::now()
        + chrono::Duration::from_std(req.expire).unwrap_or_default())
    .timestamp();
    let (grant, secret) = session::mint_grant(req.label.clone(), expires_at)?;
    let grant_id = grant.id.clone();

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
    match session::claim(&req.env_dir, &sess, &req.box_name) {
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

    // The `bool` is "a signal has already been delivered", and it decides who
    // owns the *next* one. On this branch `interrupted()` has armed the
    // hard-exit watcher, so a second Ctrl-C exits without a receipt, as
    // promised — and the teardown below must not also race for it.
    let (outcome, already_signalled) = tokio::select! {
        r = started.serve(bridge.clone(), req.direct_only) => (r, false),
        _ = interrupted(&req.env_dir) => (Ok(()), true),
        reason = stopped_elsewhere(bridge.clone()) => {
            eprintln!("share: {reason}");
            (Ok(()), false)
        }
        reason = box_went_away(req.env_dir.clone(), pinned_netns.clone()) => {
            eprintln!("share: {reason}");
            (Ok(()), false)
        }
    };

    // Say so on disk before doing any of it. `is_live` is `kill(pid, 0)`, and
    // this process is about to spend several seconds writing a receipt while
    // still very much alive — so `share grant` in that window minted a ticket,
    // printed the one copy of its secret, and then watched this function delete
    // the table it went into.
    session::begin_winding_up(&req.env_dir);

    // The teardown, and a way out of the *waiting* part of it.
    //
    // This is a select and not an unconditional `arm_second_signal` for a
    // reason that cost a round to learn. Arming the hard-exit watcher here
    // meant that on the three exits where no signal had been delivered yet —
    // `share stop` from another terminal, the box going away, the transport
    // ending — the operator's **first** Ctrl-C hit a watcher built for their
    // second: it printed "interrupted again", threw the receipt away and
    // exited. Pressing Ctrl-C once to get a prompt back destroyed the one
    // artifact this whole feature exists to produce, and told them they had
    // done it twice.
    //
    // What an interrupt during the teardown should mean is "stop waiting", not
    // "stop recording". So it skips the grace and the quiesce — losing the
    // closing bytes of whatever was still mid-copy, which is the trade the
    // operator just asked for — and still writes the receipt. Only a *second*
    // one, armed by `interrupted()` on the path where a first has actually been
    // delivered, exits without it.
    let waited = if already_signalled {
        // The hard-exit watcher is armed and owns the next signal. Racing it
        // here would make a second Ctrl-C do one of two different things
        // depending on which task woke first.
        teardown(&bridge, &mut started).await;
        true
    } else {
        tokio::select! {
            _ = teardown(&bridge, &mut started) => true,
            _ = interrupted(&req.env_dir) => {
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
        // and unmark it — a receipt calling itself partial when it had waited.
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
    session::clear(&req.env_dir);
    outcome
}

/// The orderly half: tell the connections, then the transport, then wait.
async fn teardown(bridge: &Arc<Bridge>, started: &mut Setup) {
    // Announced, so a test can wait for the window rather than guess at it.
    // The end-to-end check for "a first Ctrl-C during the teardown" slept 0.4s
    // after `share stop` and signalled — but the serving process only learns
    // about a stop by polling at `STOP_POLL`, so at 0.4s it was still in the
    // main select and the signal landed on the ordinary Ctrl-C path. The check
    // passed, for a byte-for-byte repeat of the test above it.
    eprintln!("share: shutting down");
    // Tell the connections first, tear the transport down second. `iroh`'s
    // `Endpoint::close` closes every connection with code `0` and an empty
    // reason, so a connection that wanted to close with an explanation has to
    // have done it before that runs — otherwise the joiner is told "closed by
    // peer: 0" for a ticket that simply expired.
    bridge.begin_shutdown();
    tokio::time::sleep(SHUTDOWN_GRACE).await;

    // Then the transport, then wait for the connections it was
    // carrying to actually finish. Closing the endpoint tells them to stop; it
    // does not join them, and they are detached tasks. Writing the receipt
    // straight afterwards is a race that a fast network usually wins and a slow
    // one usually loses, and what it loses is the bytes and closing times of
    // every peer still mid-copy — the half of a share a reviewer most wants.
    started.shutdown().await;
    bridge.quiesce(QUIESCE).await;
}

/// Resolves when the operator asks this process to stop.
///
/// `SIGTERM` as well as Ctrl-C, because closing the terminal, a `kill`, or a
/// process supervisor tidying up are all ordinary ways a foreground command
/// ends — and handling only the interrupt means the ingress receipt is lost in
/// exactly those cases, which are the ones nobody planned for.
async fn interrupted(env_dir: &std::path::Path) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                arm_second_signal(env_dir);
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
        arm_second_signal(env_dir);
    }
    #[cfg(not(unix))]
    {
        let _ = env_dir;
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
fn arm_second_signal(env_dir: &std::path::Path) {
    // Armed at most once. The normal shutdown path calls this too, so on a
    // Ctrl-C both calls happen and a second watcher would race the first to
    // `exit(130)` — harmless, but it also means two handlers for one signal.
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
        // this process's own orderly shutdown — the one being abandoned — and
        // five seconds of retrying is not what "stop now" means.
        session::clear_now(&env_dir);
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

/// Resolves when the box stops having a session of its own.
///
/// This is not tidiness. The dialer's helper lives *inside* the box's network
/// namespace, so it keeps that namespace alive after every other process in it
/// has gone — and loopback inside it still exists, so connections are refused
/// rather than failing in a way anybody would notice. Left alone, a share whose
/// box died answers `502` forever, and a box restarted afterwards gets a *new*
/// namespace that this share will never reach. So the share ends, and says why.
async fn box_went_away(env_dir: PathBuf, pinned: Option<String>) -> String {
    loop {
        tokio::time::sleep(BOX_POLL).await;
        // Only *writers* count. A read-only observer is a session, and it kept
        // this loop quiet after the box's real session had gone — so somebody
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
        // "Is there *any* session" was the whole test, and it is not the
        // question. Every session of a box gets a brand-new network namespace,
        // and the dialer is pinned to the one that existed at startup — so a
        // person who exits their shell and starts another, or who has a
        // read-only observer attached while they restart, leaves this loop
        // quiet and the share pinned to a namespace nothing is in. `share ls`
        // reported it healthy; the visitor was told to ask the sharer to start
        // a dev server that was already running; and the fix — restart the
        // share — was undiscoverable from either terminal.
        // And a box with no *writer* has no namespace to compare against, which
        // is not "nothing to check" but "the thing we were pinned to is gone" —
        // the very case this comparison was added for. The `if let` skipped it
        // silently.
        if let Some(want) = &pinned {
            let now = current_netns(&env_dir);
            if now.as_deref() != Some(want.as_str()) {
                return "the box restarted, so it has a new network namespace and this share \
                        is pinned to the old one. Nothing it serves can reach the box any \
                        more — start a fresh share."
                    .to_string();
            }
        }
    }
}

/// The identity of the network namespace the box's session is in right now.
///
/// `/proc/<pid>/ns/net` reads as `net:[4026536311]`, and that number changes
/// with every session, which is exactly what makes it usable as "is this still
/// the box I forked into".
fn current_netns(env_dir: &std::path::Path) -> Option<String> {
    let pid = h5i_core::view::box_pid(env_dir)?;
    std::fs::read_link(format!("/proc/{pid}/ns/net"))
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
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
/// One ticket admits one peer, so this is how a second person is added — and it
/// is why revocation can be per person rather than all or nothing.
pub fn grant(
    env_dir: &std::path::Path,
    label: Option<String>,
    expire: Duration,
) -> Result<Minted, H5iError> {
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
            // session file records only its id. Re-deriving it here would be a
            // second source of truth for something that must match exactly, so
            // this verb is honest about what it cannot do alone.
            // Naming the procedure that works, rather than one that does not.
            // "Start a second share" was the advice, and starting a second
            // share is refused because this one is running — so the two
            // messages pointed at each other and somebody wanting a second
            // viewer could alternate between them forever without learning
            // that the answer is to stop and restart.
            return Err(H5iError::Metadata(
                "a peer-to-peer share carries one ticket: a second one would need the running \
                 endpoint's addressing, and only the serving process has that. To add \
                 somebody, stop this share and start a new one — everybody gets a fresh \
                 ticket — or share with `--tunnel`, where `grant` mints extra links."
                    .into(),
            ));
        }
        s.grants.push(g);
        Ok(s.clone())
    })?;
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
/// for the lock — up to five seconds of retries. Measured with a three-second
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
    // One lock hold for the whole decision — see `session::stop`.
    Ok(if session::stop(env_dir)? {
        Stopped::Serving
    } else {
        Stopped::Stale
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
        let err = session::claim(dir.path(), &s, "demo").expect_err("already shared");
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
        // The regression this exists for: arming the hard-exit watcher after
        // the select meant that on the three exits where no signal had been
        // delivered yet — `share stop` elsewhere, the box going away, the
        // transport ending — the operator's *first* Ctrl-C hit a watcher built
        // for their second. It printed "interrupted again", threw the receipt
        // away and exited. One press, to get a prompt back, destroyed the one
        // artifact the feature exists to produce.
        //
        // What replaced it is a `select!` whose other branch abandons the
        // teardown mid-flight. So the claim to pin is that abandoning it
        // mid-flight still leaves a bridge that can write its receipt.
        let dir = tempfile::tempdir().expect("tempdir");
        let bridge = std::sync::Arc::new(crate::bridge::Bridge::new(
            dir.path().to_path_buf(),
            "env/a/demo".into(),
            "digest".into(),
            "demo".into(),
            Transport::Tunnel,
            "https://x".into(),
            crate::dialer::Dialer::spawn_local(1).expect("dialer"),
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
        session::begin_winding_up(dir.path());
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
        // about to be deleted — it added a *live* grant, which is the very
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
