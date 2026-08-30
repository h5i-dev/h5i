//! Transport two: a Cloudflare quick tunnel, for a visitor who has no h5i.
//!
//! The peer-to-peer transport needs h5i on both ends, because a browser cannot
//! speak QUIC to an endpoint id, which rules out the person you most often want
//! clicking a prototype. This transport trades the property P2P has for the one
//! it does not: anybody with the link can open it in any browser.
//!
//! What it costs, stated in the receipt rather than only in the docs. **TLS
//! terminates at Cloudflare**, so this path is not end to end and Cloudflare can
//! read the traffic; usually an acceptable trade for an agent-built prototype
//! and never ours to assume, so [`crate::bridge::render_receipt`] writes it into
//! the export. **`cloudflared` is somebody else's binary**, neither shipped nor
//! pinned, and its absence is a failure that names the alternative. **Quick
//! tunnels are explicitly not a production service**: Cloudflare caps
//! concurrency and does not support server-sent events on them.
//!
//! What does not change is the bridge underneath. The URL carries a token
//! checked against the same grant table on every connection, live connections
//! drop when a grant is revoked, and the credential is stripped before anything
//! reaches the box. The capability degrades from "hold the secret" to "hold the
//! link", not to nothing.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use h5i_error::H5iError;
use tokio::io::BufReader;

use crate::bridge::{Bridge, Path};
use crate::http_front::{self, Next};

/// How long to wait for `cloudflared` to publish a URL before giving up.
const URL_TIMEOUT: Duration = Duration::from_secs(45);
/// How often a connection asks whether its grant still admits anyone.
const REVOKE_POLL: Duration = Duration::from_secs(1);
/// How long to pause after an `accept` error before trying again.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// How many connections this front will hold at once, authorized or not.
const MAX_PENDING: usize = 256;

/// A running `cloudflared`, killed when this is dropped.
#[derive(Debug)]
pub struct Tunnel {
    child: tokio::process::Child,
    pub url: String,
}

impl Tunnel {
    /// A tunnel whose `cloudflared` has already exited, for tests that need a
    /// `Setup` and not a network. Spawning `true` rather than faking the field
    /// keeps `stop()` on its real path — killing a child that is already gone
    /// is exactly what a tunnel teardown does when `cloudflared` died first.
    #[cfg(test)]
    pub fn already_gone_for_tests() -> Tunnel {
        let child = tokio::process::Command::new("true")
            .kill_on_drop(true)
            .spawn()
            .expect("spawn true");
        Tunnel {
            child,
            url: "https://test.trycloudflare.com".into(),
        }
    }

    /// The origin a visitor opens, with no token in it.
    pub fn origin(&self) -> &str {
        &self.url
    }

    /// Stop the tunnel. Called on the way out so a quick tunnel does not
    /// outlive the share that created it.
    pub async fn stop(&mut self) {
        let _ = self.child.kill().await;
    }

    /// Resolves when `cloudflared` exits.
    ///
    /// Nothing watched it before, so a tunnel that died mid-share left the
    /// terminal showing a live share with a printed URL that answered nothing,
    /// no message, and no exit. Quick tunnels are documented as unreliable;
    /// this is the failure people will actually hit.
    pub async fn died(&mut self) -> String {
        match self.child.wait().await {
            Ok(status) => format!("`cloudflared` exited ({status})"),
            Err(e) => format!("`cloudflared` could not be waited on: {e}"),
        }
    }
}

/// Pull a quick-tunnel URL out of a line of `cloudflared` logging.
///
/// Strict about what it will accept: the host must be a `trycloudflare.com`
/// subdomain made of the characters a hostname is allowed to have. This is a
/// URL we are about to print and tell someone to open, and `cloudflared`'s
/// output is a log format rather than an interface — a looser match would let a
/// change in its banner, or anything that got into its logs, choose what we
/// hand a person.
pub fn extract_url(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let rest = &line[start..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '|' || c == '"' || c == '<')
        .unwrap_or(rest.len());
    let url = rest[..end].trim_end_matches('/');
    let host = url.strip_prefix("https://")?;
    if !host.ends_with(".trycloudflare.com") {
        return None;
    }
    let label = host.strip_suffix(".trycloudflare.com")?;
    if label.is_empty()
        || !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
    {
        return None;
    }
    Some(url.to_string())
}

/// How much of one `cloudflared` line to hold before deciding it is not a line.
///
/// A bound on what is *kept*, never on what is read. Bounding the read with
/// `take()` made `Take` report EOF at its limit, which ended the drain task,
/// closed the pipe's read end, and gave `cloudflared` an EPIPE that Go turns
/// into a fatal signal — so a healthy share died as soon as its subprocess had
/// logged a megabyte. That is verbatim the failure the drain's own comment
/// warns about, reintroduced by the fix for unbounded line buffering.
const MAX_CLOUDFLARED_LINE: usize = 64 * 1024;

/// Kill `target` when `watch` dies — Darwin's answer to `PR_SET_PDEATHSIG`.
///
/// The hazard is the one the Linux arm describes above and it is not smaller
/// here: `kill_on_drop` is a destructor, `SIGKILL` skips destructors, and a
/// `cloudflared` that outlives its share keeps a public `trycloudflare.com`
/// hostname pointing at a loopback port that has just been freed. Measured on
/// Linux at ten to twenty seconds. macOS had no second rope at all.
///
/// Darwin has no `PR_SET_PDEATHSIG`, and the difference is not just spelling:
/// pdeathsig is something a process asks *for itself*, so Linux can set it in
/// the child between fork and exec. Nothing can ask that on behalf of a program
/// h5i does not compile. So the job goes to a third process — a watchdog that
/// waits on `kqueue` for either process to exit and kills `target` if `watch`
/// went first.
///
/// Being a separate process is the whole point: a `SIGKILL` of the share
/// cannot skip it, because it is not running any of the share's code and does
/// not die with it. It is reparented and keeps waiting.
///
/// **Both** pids are registered, and that is what makes it safe rather than
/// merely prompt. A watchdog that waited only on `watch` would, after
/// `cloudflared` had exited normally and its pid had been recycled, wake up and
/// `SIGKILL` whatever innocent process now holds that number. Registering
/// `target` too means its exit is the event that retires the watchdog.
///
/// The child is allocation-free below the fork, for the reason
/// [`crate::dialer`] spells out at length: this runs inside a tokio runtime, so
/// the child inherits one thread and whatever locks the others held, the
/// allocator's among them.
#[cfg(target_os = "macos")]
pub(crate) fn arm_parent_death_kill(watch: libc::pid_t, target: libc::pid_t) {
    // SAFETY: fork, then raw syscalls and `_exit` in the child.
    let pid = unsafe { libc::fork() };
    if pid != 0 {
        // Parent, including the fork having failed. A share whose watchdog
        // could not start is a share with the protection Linux had before
        // `PR_SET_PDEATHSIG` — `kill_on_drop` still covers every ordinary
        // ending — so this is not worth refusing to serve over.
        return;
    }
    unsafe {
        let kq = libc::kqueue();
        if kq < 0 {
            libc::_exit(1);
        }
        let watch_one = |pid: libc::pid_t| -> i32 {
            let mut ev: libc::kevent = std::mem::zeroed();
            ev.ident = pid as libc::uintptr_t;
            ev.filter = libc::EVFILT_PROC;
            ev.flags = libc::EV_ADD | libc::EV_ENABLE | libc::EV_ONESHOT;
            ev.fflags = libc::NOTE_EXIT;
            libc::kevent(kq, &ev, 1, std::ptr::null_mut(), 0, std::ptr::null())
        };
        // Registered one at a time so a failure says *which* is already gone.
        // A share that died between the spawn and here is exactly the window
        // this exists for, so that case kills rather than gives up.
        if watch_one(watch) < 0 {
            libc::kill(target, libc::SIGKILL);
            libc::_exit(0);
        }
        if watch_one(target) < 0 {
            libc::_exit(0);
        }
        let mut out: libc::kevent = std::mem::zeroed();
        let n = libc::kevent(kq, std::ptr::null(), 0, &mut out, 1, std::ptr::null());
        // Whichever exited first decides. `target` going first is the ordinary
        // end of a share and there is nothing to kill; anything unexpected is
        // treated as `watch` having gone, which is the fail-safe direction.
        if n == 1 && out.ident == target as libc::uintptr_t {
            libc::_exit(0);
        }
        libc::kill(target, libc::SIGKILL);
        libc::_exit(0);
    }
}

/// Start `cloudflared` pointed at a loopback port, and wait for its URL.
///
/// The argv is built here, never through a shell: the only value that varies is
/// a port number this process chose.
pub async fn start(local_port: u16) -> Result<Tunnel, H5iError> {
    let mut cmd = tokio::process::Command::new("cloudflared");
    cmd.arg("tunnel")
        .arg("--no-autoupdate")
        .arg("--url")
        .arg(format!("http://127.0.0.1:{local_port}"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    // And a second, stronger rope. `kill_on_drop` runs a destructor, which a
    // `SIGKILL` of this process skips entirely — so a killed share left
    // `cloudflared` alive for another ten to twenty seconds with its public
    // `trycloudflare.com` hostname still registered and still pointing at
    // `http://127.0.0.1:<port>`. That port is in the ephemeral range and has
    // just been freed, so for that window anything on this machine that binds
    // it is on the public internet under a hostname h5i minted.
    //
    // `PR_SET_PDEATHSIG` is the kernel doing it instead. Precisely: when the
    // *thread* that forked this child exits, the child gets the signal —
    // pdeathsig is thread-scoped, not process-scoped. That is safe here only
    // because `run::serve` drives everything through `Runtime::block_on`, so
    // this fork happens on the main thread and that thread's life is the
    // process's. Moving `serve_async` behind a `tokio::spawn` or a
    // `spawn_blocking` would start killing `cloudflared` mid-share when the
    // worker thread retired, and nothing would point here.
    #[cfg(target_os = "linux")]
    unsafe {
        // Read before the fork. The first version of this compared
        // `getppid() == 1`, which is folklore and wrong in both directions: it
        // misses a parent that died into a `PR_SET_CHILD_SUBREAPER` reaper
        // (a `systemd --user` session is one), and — much worse — it fires
        // when h5i legitimately *is* pid 1, which is every `docker run image
        // h5i box share --tunnel`. There the child `_exit(0)`d before exec,
        // std read a zero-length error pipe as a successful exec, and the
        // share died with "cloudflared exited without publishing a URL" while
        // running the same command by hand worked every time.
        let parent = libc::getpid();
        // `tokio::process::Command` has its own `pre_exec`; the `CommandExt`
        // import this used to carry was shadowed by it and unused.
        cmd.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // A parent that died between the fork and the prctl leaves the
            // child reparented, and the signal will never come — so ask
            // whether we are still the child of who forked us.
            if libc::getppid() != parent {
                libc::_exit(0);
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            H5iError::Metadata(
                "`cloudflared` is not installed, and `--tunnel` is a wrapper around it. \
                     Install it (https://developers.cloudflare.com/cloudflare-one/connections/\
                     connect-networks/downloads/), or share peer-to-peer with `h5i box share` \
                     and have the other side run `h5i join`."
                    .into(),
            )
        } else {
            H5iError::Metadata(format!("could not start `cloudflared`: {e}"))
        }
    })?;

    // The same rope, tied the only way Darwin lets you tie it: no
    // `PR_SET_PDEATHSIG`, so a third process holds it. Armed after the spawn
    // because it needs `cloudflared`'s pid, which is the one thing the Linux
    // arm does not need — that one runs *inside* the child, before its exec.
    #[cfg(target_os = "macos")]
    if let Some(pid) = child.id() {
        arm_parent_death_kill(unsafe { libc::getpid() }, pid as libc::pid_t);
    }

    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| H5iError::Metadata("cloudflared produced no output to read".into()))?;
    // Capped. `lines()` accumulates until it sees a newline, and a
    // `cloudflared` that writes without one — or a different binary of that
    // name — grows one `String` without limit: a fake writing 150 MiB with no
    // newline took this process from 25 MB to 178 MB of RSS in two seconds,
    // and it would have kept going for the whole URL timeout. h5i neither
    // ships nor pins that binary, so what it does is not h5i's to assume.
    let mut reader = BufReader::new(stderr);

    // Kept, so the reason it failed can be repeated. `cloudflared` prints why
    // it is unhappy and this used to read that line, discard it, and then tell
    // the operator to "run it once by hand to see what it says" — having
    // already been told.
    let mut last_said: Option<String> = None;
    let found = tokio::time::timeout(URL_TIMEOUT, async {
        let mut buf = vec![0u8; 8 * 1024];
        // At most one line's worth is held at a time, and something longer
        // than a line is treated as one — a subprocess that never emits a
        // newline cannot make this grow.
        let mut pending = String::new();
        loop {
            let n = match tokio::io::AsyncReadExt::read(&mut reader, &mut buf).await {
                Ok(0) | Err(_) => return None,
                Ok(n) => n,
            };
            pending.push_str(&String::from_utf8_lossy(&buf[..n]));
            while let Some(at) = pending.find('\n') {
                let line: String = pending.drain(..=at).collect();
                if let Some(url) = extract_url(&line) {
                    return Some(url);
                }
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    last_said = Some(trimmed.chars().take(200).collect());
                }
            }
            if pending.len() > MAX_CLOUDFLARED_LINE {
                // Not a line. Keep the tail, in case a URL is part-way through
                // it, and drop what came before unread.
                pending = pending.split_off(pending.len() - 512);
            }
        }
    })
    .await;

    match found {
        Ok(Some(url)) => {
            // Keep reading. Dropping the pipe here closes its read end, and the
            // next time `cloudflared` fills the kernel buffer and writes it
            // takes an EPIPE — which Go turns into a fatal signal on fd 2. A
            // tunnel that dies mid-share for that reason would report nothing
            // at all, so the lines are consumed and discarded for as long as it
            // runs.
            // Consumed and discarded for as long as `cloudflared` runs, with no
            // ceiling of its own — a ceiling here is exactly what closes the
            // pipe and kills it.
            tokio::spawn(async move {
                let mut sink = vec![0u8; 8 * 1024];
                while let Ok(n) = tokio::io::AsyncReadExt::read(&mut reader, &mut sink).await {
                    if n == 0 {
                        break;
                    }
                }
            });
            Ok(Tunnel { child, url })
        }
        Ok(None) => {
            // Whether it is actually gone is checked rather than assumed. This
            // arm is reached whenever the pipe ends, and a pipe can end on a
            // read error with the child alive and well — at which point
            // "`cloudflared` exited" is a sentence about a running process,
            // and the operator goes looking for a crash that did not happen.
            let how = if matches!(child.try_wait(), Ok(None)) {
                NoUrl::WentQuiet
            } else {
                NoUrl::Exited
            };
            let _ = child.kill().await;
            Err(no_url(how, last_said.as_deref()))
        }
        Err(_) => {
            let _ = child.kill().await;
            Err(no_url(NoUrl::TimedOut, last_said.as_deref()))
        }
    }
}

/// How the wait for a URL ended.
enum NoUrl {
    /// The child is gone.
    Exited,
    /// The pipe ended and the child is still running.
    WentQuiet,
    /// Neither: h5i gave up waiting.
    TimedOut,
}

/// What the operator is told when no URL arrived.
///
/// Its own function so it can be tested. `start` runs the real `cloudflared`
/// from `PATH`, so nothing in the suite could reach these three sentences, and
/// two of the three were wrong for a round each without a test noticing: one
/// announced that a live child had exited, and the other — the arm a blocked
/// or throttled network actually reaches — threw away the reason `cloudflared`
/// had just printed and offered a guess about outbound access in its place.
fn no_url(how: NoUrl, said: Option<&str>) -> H5iError {
    let opening = match how {
        NoUrl::Exited => "`cloudflared` exited without publishing a URL".to_string(),
        NoUrl::WentQuiet => {
            "`cloudflared` stopped talking to h5i (it is still running) and published no URL"
                .to_string()
        }
        NoUrl::TimedOut => format!(
            "`cloudflared` did not publish a URL within {}s. Quick tunnels need outbound \
             network access to Cloudflare",
            URL_TIMEOUT.as_secs()
        ),
    };
    H5iError::Metadata(match said {
        Some(said) => format!(
            "{opening}. The last thing it said was: {}",
            h5i_core::redact::sanitize_display(said)
        ),
        None => format!(
            "{opening}, and said nothing at all. Run it by hand to see: `cloudflared tunnel \
             --url http://127.0.0.1:3000`."
        ),
    })
}

/// What a visitor gets when the share is at capacity. Deliberately not a `401`:
/// their link is fine, and telling them otherwise sends them back to ask for a
/// new one that would work no better.
const BUSY_BODY: &str = "This share is busy right now. Wait a moment and reload the page.";

/// Built rather than written out, because a hand-counted `Content-Length` is a
/// truncated page waiting for somebody to edit the sentence. Written out once
/// anyway, and wrong by two bytes within the hour — hence one builder for both.
fn plain_response(status: &str, extra: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         {extra}\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )
}

fn busy_response() -> String {
    plain_response("503 Service Unavailable", "Retry-After: 2\r\n", BUSY_BODY)
}

fn unreachable_response() -> String {
    plain_response("502 Bad Gateway", "", UNREACHABLE_BODY)
}

/// What a visitor gets when the share is up and the box is not.
const UNREACHABLE_BODY: &str =
    "This share is up, but nothing is answering inside it. Whoever shared it needs to look.";

/// The visitor-facing link: the origin plus the token that authorizes one grant.
pub fn invite_url(origin: &str, secret: &str) -> String {
    format!("{origin}/?{}={secret}", crate::gate::QUERY_PARAM)
}

/// Serve the tunnel's loopback side until the process stops.
///
/// The listener is bound on `127.0.0.1` and nothing else, on every path: what
/// reaches the internet is `cloudflared`'s outbound connection, not a port on
/// this machine.
pub async fn serve(bridge: Arc<Bridge>, listener: tokio::net::TcpListener) -> Result<(), H5iError> {
    // One entry per grant, because a tunnel genuinely cannot tell two browsers
    // apart — the peers it sees are Cloudflare's. Counting per grant is the
    // finest honest granularity, and the receipt says so rather than implying
    // a precision the transport does not have.
    let peers: Arc<Mutex<HashMap<String, crate::bridge::PeerId>>> = Default::default();

    // A ceiling on connections this front will hold, which is a different
    // number from `Bridge::admit`'s: that one is taken *after* authorization
    // and bounds sockets into the box. Everything before it — the head read,
    // its buffers, the task — is available to anyone who can reach the tunnel
    // URL, which is public by construction.
    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_PENDING));
    loop {
        let sock = match listener.accept().await {
            Ok((sock, _)) => sock,
            // Not `continue`: tokio only clears a listener's readiness on
            // `WouldBlock`, so a persistent error — `EMFILE` is the one that
            // actually happens — returns immediately every time and turns this
            // into a busy loop that never recovers. Pausing gives descriptors a
            // chance to come back and keeps the share responsive if they do.
            Err(e) => {
                eprintln!("share: could not accept a connection: {e}");
                tokio::time::sleep(ACCEPT_BACKOFF).await;
                continue;
            }
        };
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            // Refused without a task and without a reply. Whoever is flooding
            // this is not owed an explanation, and the visitor with a valid
            // link is owed the slots. Recorded, though: a share taken down at
            // its own front door used to write a receipt saying nobody came.
            bridge.record_front_refusal();
            continue;
        };
        let bridge = bridge.clone();
        let peers = peers.clone();
        // Taken before the head is read and held until the handler returns, so
        // teardown can wait for connections that have been accepted and not
        // yet authorized. `None` means the share is already winding up: the
        // socket is dropped rather than served, which is what makes quiescence
        // a barrier. See `Bridge::enter_front`.
        let Some(front) = bridge.enter_front() else {
            continue;
        };
        tokio::spawn(async move {
            let _slot = slot;
            let _front = front;
            if let Err(e) = handle(bridge, peers, sock).await {
                eprintln!("share: {e}");
            }
        });
    }
}

/// The record this grant's traffic is counted against, creating it if this is
/// the first thing that grant has done.
///
/// One entry per grant, because a tunnel genuinely cannot tell two browsers
/// apart — the peers it sees are Cloudflare's.
///
/// The map is taken with a poison recovery, not an `expect`, and the reason is
/// the one `Bridge::tally` gives: `peer_joined` runs while this lock is held, so
/// an `expect` would turn a single panic under it into a front where *every
/// later connection* dies at the same line — a share that answers resets and
/// says nothing about why. The map is a `grant id → PeerId` lookup with no
/// invariant across entries; a recovered one is at worst missing a row, which
/// costs a receipt line and no access.
fn register(
    bridge: &Arc<Bridge>,
    peers: &Arc<Mutex<HashMap<String, crate::bridge::PeerId>>>,
    grant: &crate::bridge::AuthorizedGrant,
) -> crate::bridge::PeerId {
    let mut map = peers.lock().unwrap_or_else(|p| p.into_inner());
    *map.entry(grant.id.clone()).or_insert_with(|| {
        bridge.peer_joined(
            "a browser (the tunnel cannot tell two apart)".into(),
            grant,
            // Observed, and there is nothing else it could be: this transport
            // has exactly one path.
            Some(Path::Tunnel),
        )
    })
}

/// Resolves when this connection's own grant stops admitting anyone.
///
/// Per grant rather than per share: revoking one peer has to cut that peer off
/// even while somebody else's ticket is still good, and a check on "is the
/// whole share spent" would not.
async fn revoked(bridge: Arc<Bridge>, grant_id: String) {
    loop {
        tokio::time::sleep(REVOKE_POLL).await;
        if !bridge.grant_is_live(&grant_id) {
            return;
        }
    }
}

async fn handle(
    bridge: Arc<Bridge>,
    peers: Arc<Mutex<HashMap<String, crate::bridge::PeerId>>>,
    mut sock: tokio::net::TcpStream,
) -> Result<(), H5iError> {
    let (head, rest) = match http_front::read_head(&mut sock).await {
        Ok(pair) => pair,
        // A head this share would not read is the same fact as a head the gate
        // refuses, one step earlier — and it used to be the one thing a receipt
        // could not mention. A 32 KB header block, or a TLS hello sent to the
        // plaintext front, left no trace at all.
        Err(http_front::NoHead::Refused) => {
            bridge.record_turned_away(crate::bridge::TurnedAwayReason::Unparseable);
            return Ok(());
        }
        // And a peer that said nothing still leaves none, deliberately: a
        // browser's preconnect is this shape, so recording it would bury every
        // row that means something under noise anybody can generate.
        Err(http_front::NoHead::Silent) => return Ok(()),
    };

    // Resolved once, here, so the decision and the accounting agree about which
    // grant let this connection in.
    let mut grant = None;
    // Whether a credential was presented at all. `authorize` records its own
    // refusals, so counting every `401` here logged a revoked ticket twice —
    // once truthfully and once as "unknown" — in the one number the receipt
    // sells as ingress evidence.
    let mut presented = false;
    // Kept, because two of the five refusals are not about the visitor at all.
    // The gate answers every one of them with the same `401` on purpose — a
    // prober must not learn whether a ticket is unknown, expired or revoked —
    // but "the share has ended" and "this machine cannot read its own grant
    // table" are facts about *this side*, and a browser told to go and ask for
    // a new invite will come back with one that fails identically.
    let mut why: Option<crate::session::Denied> = None;
    let next = http_front::decide(
        &head,
        // The bare name: a quick tunnel's host is its own site (trycloudflare
        // is on the Public Suffix List), so there is no other origin to
        // collide with.
        crate::gate::COOKIE,
        |token| {
            presented = true;
            match bridge.authorize(token) {
                Ok(g) => {
                    grant = Some(g);
                    true
                }
                Err(d) => {
                    why = Some(d);
                    false
                }
            }
        },
        // The visitor's origin is https, because Cloudflare terminates it.
        true,
        // The tunnel front is on the *sharer's* machine, in front of their own
        // box, reached over a public hostname with a cookie jar of its own.
        // Nothing else has written to that jar, so there is nothing to tell
        // apart — see `gate::AppCookies`, which exists for the one case where
        // that is not true.
        None,
    );

    let (head, req) = match next {
        Next::Respond(mut body, refusal) => {
            // Substituted after the fact rather than threaded through `decide`,
            // which takes a `bool` and is shared with the joiner's front. Only
            // the `401` is rewritten: a `400` is about the request's bytes and
            // a `403` about where it came from, and neither becomes truer for
            // the share having stopped.
            //
            // Keyed on the typed reason, not on the rendered status text. The
            // block below already learned that lesson — "by its own typed
            // reason rather than by reading the status back out of the bytes we
            // just rendered" — and these two were left testing
            // `starts_with("HTTP/1.1 401")`, which is the same recovery by a
            // different spelling. It is correct today and it is correct by
            // coincidence: `Refusal::status` and `refusal_response`'s format
            // string are both free to change, and either would silently stop a
            // stopped share from telling its visitors so and stop the receipt
            // counting the commonest event a public URL has.
            let unauthorized = refusal == Some(crate::gate::Refusal::NotAuthorized);
            if unauthorized {
                match why {
                    Some(crate::session::Denied::ShareOver) => {
                        body = plain_response(
                            "410 Gone",
                            "",
                            "This share has ended. Nothing is wrong with your link — whoever \
                             shared it stopped the share.",
                        );
                    }
                    Some(crate::session::Denied::TableUnreadable) => {
                        body = plain_response(
                            "503 Service Unavailable",
                            "",
                            "The sharing machine could not read its own record of who is \
                             invited. Nothing is wrong with your link, and a new one would \
                             fail the same way.",
                        );
                    }
                    _ => {}
                }
            }
            // A `401` here is somebody knocking with nothing, which `authorize`
            // never sees and so never counted. On a public tunnel URL that is
            // the commonest thing that happens to a share.
            if !presented && unauthorized {
                bridge.record_refused();
            }
            // Every refusal that is not about a credential, by its own typed
            // reason rather than by reading the status back out of the bytes
            // we just rendered. The status test reached the `400`s and left
            // both `403`s counted nowhere — so a session consisting entirely
            // of foreign-origin requests carrying the share cookie, or of
            // service-worker registrations, reported no turned-away activity
            // at all, in the one lane whose job is to say who was turned away
            // before a ticket was weighed.
            match refusal {
                Some(crate::gate::Refusal::Malformed) => {
                    bridge.record_turned_away(crate::bridge::TurnedAwayReason::Unparseable)
                }
                Some(crate::gate::Refusal::ForeignOrigin) => {
                    bridge.record_turned_away(crate::bridge::TurnedAwayReason::ForeignOrigin)
                }
                Some(crate::gate::Refusal::ServiceWorker) => {
                    bridge.record_turned_away(crate::bridge::TurnedAwayReason::ServiceWorker)
                }
                // A `401` is about a credential, and `record_refused` above
                // is its lane. `None` is the invite redirect.
                Some(crate::gate::Refusal::NotAuthorized) | None => {}
            }
            // A redirect is not a refusal: it is a visitor following the invite
            // link, authorized, on the first request every visitor makes. A
            // peer who opened the link and then read nothing used to leave the
            // receipt saying nobody came.
            if let Some(g) = &grant {
                // Registered, and its bytes counted — but *not* counted as a
                // connection. A redirect never reaches the box, and the
                // receipt's connection count is documented as connections into
                // it. Saying "somebody arrived" and "somebody reached the dev
                // server" are different facts and the receipt should not merge
                // them.
                let id = register(&bridge, &peers, g);
                bridge.peer_bytes(id, body.len() as u64, 0);
                // Once. `peer_seen` stamps a timestamp; three calls in a row
                // stamped the same one three times.
                bridge.peer_seen(id);
            }
            http_front::respond(&mut sock, &body).await;
            return Ok(());
        }
        Next::Proxy { head, req } => (head, req),
    };
    let Some(grant) = grant else {
        return Ok(());
    };

    // Authorized, but the share may already be carrying all it will. A refusal
    // here is a `503` with a `Retry-After`, because unlike a bad token this one
    // is worth trying again.
    let id = register(&bridge, &peers, &grant);

    let Some(_slot) = bridge.admit() else {
        let body = busy_response();
        bridge.peer_bytes(id, body.len() as u64, 0);
        bridge.peer_seen(id);
        http_front::respond(&mut sock, &body).await;
        return Ok(());
    };

    // On a blocking pool because it is blocking: the dialer waits for its
    // helper to hand back a connected socket. A runtime worker parked on that
    // syscall is a worker not serving the other requests of the same page.
    let upstream = {
        let bridge2 = bridge.clone();
        // Raced against the grant and the share ending, for the reason the P2P
        // front gives at the same point: the `revoked` arm of the `select!`
        // below is installed *after* this await, and the dialer serialises
        // every request behind one mutex with a ten-second connect timeout. A
        // dev server with a full accept queue therefore let authorized
        // requests pile up here, holding every permit, past a `revoke` that
        // had already told the operator open connections were dropped.
        let opened = tokio::select! {
            r = tokio::task::spawn_blocking(move || bridge2.open_upstream()) => {
                r.map_err(|e| H5iError::Metadata(format!("the box dialer panicked: {e}")))?
            }
            _ = revoked(bridge.clone(), grant.id.clone()) => {
                let body = crate::gate::refusal_response(crate::gate::Refusal::NotAuthorized);
                bridge.peer_bytes(id, body.len() as u64, 0);
                bridge.peer_seen(id);
                http_front::respond(&mut sock, &body).await;
                return Ok(());
            }
            _ = bridge.shutting_down() => {
                let body = crate::gate::refusal_response(crate::gate::Refusal::NotAuthorized);
                bridge.peer_bytes(id, body.len() as u64, 0);
                bridge.peer_seen(id);
                http_front::respond(&mut sock, &body).await;
                return Ok(());
            }
        };
        match opened {
            Ok(s) => s,
            Err(e) => {
                // Answered rather than dropped. A closed socket renders in a
                // browser as "the connection was reset", which tells the
                // visitor nothing about a dev server that is simply not up —
                // and the joiner's proxy has answered this case since it was
                // written.
                let body = unreachable_response();
                bridge.peer_bytes(id, body.len() as u64, 0);
                // Stamped, like every other path that answers a visitor. Left
                // out, a peer whose most recent request received this `502`
                // kept the activity time of some earlier one — and for a
                // cookie-authenticated visitor whose every request failed this
                // way, the receipt showed them as still connected at the end
                // of the share.
                bridge.peer_seen(id);
                http_front::respond(&mut sock, &body).await;
                return Err(e);
            }
        }
    };
    upstream.set_nonblocking(true)?;
    let upstream = tokio::net::TcpStream::from_std(upstream)?;

    bridge.peer_connection(id);

    let (up_r, up_w) = upstream.into_split();
    let counts = http_front::Counters::default();
    let forwarded = http_front::Forwarded {
        head: &head,
        rest: &rest,
        req: &req,
    };
    tokio::select! {
        _ = http_front::proxy_one(sock, up_r, up_w, forwarded, &counts, None) => {}
        _ = revoked(bridge.clone(), grant.id.clone()) => {}
        _ = bridge.shutting_down() => {}
    }
    // Outside the select, so a connection cut short by a revoke still reports
    // what it had already moved.
    let (to_box, to_peer) = counts.read();
    bridge.peer_bytes(id, to_peer, to_box);
    // Stamped when the connection *finishes*, not when it starts. The tunnel
    // has no close to observe — a visitor is a grant, and their connections
    // come and go — so this is the only thing that can bound how long they
    // were inside. Stamped at the start it turned "held for the whole six-hour
    // share" into "held for one second" for the archetypal case: a page whose
    // hot-reload socket stays open for ninety minutes. Underreporting how long
    // somebody was in the box is the worse direction of the two.
    bridge.peer_seen(id);
    if counts.was_truncated() {
        bridge.record_truncated();
    }
    Ok(())
}

// Transport tests, and every one of them dials into a box: the dialer forks a
// helper into a network namespace, which is Linux. Sharing itself refuses on
// other platforms, so there is nothing here for them to check.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_url_is_read_out_of_cloudflareds_banner() {
        let line = "2026-08-10T10:00:00Z INF |  https://odd-cat-1234.trycloudflare.com  |";
        assert_eq!(
            extract_url(line).as_deref(),
            Some("https://odd-cat-1234.trycloudflare.com")
        );
    }

    #[test]
    fn only_a_quick_tunnel_host_is_accepted_as_one() {
        // `cloudflared`'s log format is not an interface, and its output is not
        // all ours. A looser match would let a banner change — or anything that
        // got into its logs — choose the URL we hand a person to open.
        for line in [
            "INF Visit https://developers.cloudflare.com/argo-tunnel for docs",
            "INF see https://example.test/",
            "INF https://evil.test/?x=https://a.trycloudflare.com",
            "INF https://.trycloudflare.com",
            "INF https://a b.trycloudflare.com",
            "nothing here at all",
        ] {
            assert_eq!(extract_url(line), None, "accepted a URL from: {line}");
        }
    }

    #[test]
    fn a_trailing_slash_or_quote_does_not_end_up_in_the_url() {
        assert_eq!(
            extract_url(r#"INF url="https://a-b-c.trycloudflare.com/""#).as_deref(),
            Some("https://a-b-c.trycloudflare.com")
        );
    }

    #[test]
    fn the_invite_link_carries_the_token_and_nothing_else() {
        let url = invite_url("https://odd-cat.trycloudflare.com", "abc123");
        assert_eq!(url, "https://odd-cat.trycloudflare.com/?h5i=abc123");
    }

    #[tokio::test]
    async fn what_the_box_receives_is_what_the_client_sent_except_for_the_rewrites() {
        // A differential check rather than a list of cases somebody thought of.
        // The proxy is entitled to change exactly three things on the way in —
        // strip the share cookie, replace the connection-lifetime headers, and
        // refuse what it will not parse — and everything else about a request
        // should reach the box as sent. Nothing verified that: the tests here
        // assert about particular headers, so a rewrite that quietly dropped,
        // reordered or re-cased an unrelated one would pass all of them.
        let (port, seen) = stubborn_keepalive_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        // Shapes a real client produces, including the awkward ones.
        let cases: Vec<(&str, String)> = vec![
            ("a bare GET", "GET / HTTP/1.1\r\n".into()),
            ("a deep path", "GET /a/b/c/d.js?x=1&y=2 HTTP/1.1\r\n".into()),
            (
                "an escaped path",
                "GET /caf%C3%A9/%20space?q=a%2Bb HTTP/1.1\r\n".into(),
            ),
            ("a HEAD", "HEAD /index.html HTTP/1.1\r\n".into()),
            ("an OPTIONS", "OPTIONS /api HTTP/1.1\r\n".into()),
            (
                "a long target",
                format!("GET /{} HTTP/1.1\r\n", "z".repeat(1000)),
            ),
        ];
        // Headers the client sends that must arrive untouched, including odd
        // casing, a repeated name, and a value with commas and spaces.
        let headers = [
            "x-custom: one",
            "X-CUSTOM-TWO: Two",
            "accept: text/html, application/xhtml+xml;q=0.9",
            "Accept-Language: en-GB,en;q=0.5",
            "x-repeated: a",
            "x-repeated: b",
            "User-Agent: h5i-test/1.0 (differential)",
            "Referer: http://127.0.0.1/from",
        ];
        let extra = format!("{}\r\n", headers.join("\r\n"));

        for (what, line) in cases {
            seen.lock().unwrap().clear();
            let sent = format!(
                "{line}Host: t\r\nCookie: sid=9; h5i_share={secret}; other=1\r\n{extra}\r\n"
            );
            let got = request(addr, &sent).await;
            assert!(got.starts_with("HTTP/1.1 200 "), "{what}: {got}");

            let arrived = seen.lock().unwrap().join("");
            let sent_lines: Vec<&str> = sent.split("\r\n").filter(|l| !l.is_empty()).collect();
            let arrived_lines: Vec<&str> =
                arrived.split("\r\n").filter(|l| !l.is_empty()).collect();

            // The request line reaches the box exactly as sent. A target the
            // proxy rewrote would send the app to a different page than the
            // visitor asked for.
            assert_eq!(
                arrived_lines.first(),
                sent_lines.first(),
                "{what}: the request line was rewritten"
            );

            // Compared as a *sequence*, not as a set. Two `contains` loops
            // caught a dropped or re-cased header and nothing else: mutating
            // the rewrite to emit every header twice, or in reverse order,
            // left both of them green. Duplication is the worse miss — a
            // rewrite that doubled a `Content-Length` is the smuggling shape
            // this crate spends most of its budget refusing.
            let owned = |l: &&str| {
                let name = l.split(':').next().unwrap_or("").to_ascii_lowercase();
                name == "cookie" || name == "connection"
            };
            let expected: Vec<&str> = sent_lines[1..]
                .iter()
                .copied()
                .filter(|l| !owned(l))
                .collect();
            let actual: Vec<&str> = arrived_lines[1..]
                .iter()
                .copied()
                .filter(|l| !owned(l))
                .collect();
            assert_eq!(
                actual, expected,
                "{what}: the headers the box saw are not the headers the client sent, \
                 in the order it sent them"
            );

            // The cookie arrives with ours taken out and the app's left alone.
            let cookie = arrived_lines
                .iter()
                .find(|l| l.to_ascii_lowercase().starts_with("cookie:"))
                .copied()
                .unwrap_or("");
            assert!(!cookie.contains("h5i_share"), "{what}: {cookie}");
            assert!(
                cookie.contains("sid=9"),
                "{what}: the app's own cookie was dropped: {cookie}"
            );
            assert!(cookie.contains("other=1"), "{what}: {cookie}");

            // And exactly one of each header the proxy owns, rather than one
            // more than it started with.
            let count = |name: &str, lines: &[&str]| {
                lines
                    .iter()
                    .filter(|l| l.split(':').next().unwrap_or("").eq_ignore_ascii_case(name))
                    .count()
            };
            assert_eq!(count("connection", &arrived_lines), 1, "{what}");
            assert_eq!(count("cookie", &arrived_lines), 1, "{what}");
        }

        serving.abort();
    }

    /// Answers with a fixed set of response headers, so a test can compare what
    /// the visitor received against what the box actually sent.
    fn header_rich_server() -> u16 {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for conn in l.incoming() {
                let Ok(mut c) = conn else { continue };
                std::thread::spawn(move || {
                    let mut buf = [0u8; 8192];
                    let _ = c.read(&mut buf);
                    let head = [
                        "HTTP/1.1 200 OK",
                        "Content-Type: text/html; charset=utf-8",
                        "Content-Length: 2",
                        "Cache-Control: no-store, max-age=0",
                        "ETag: \"abc123\"",
                        "X-Frame-Options: DENY",
                        "Set-Cookie: sid=9; Path=/; HttpOnly",
                        "Set-Cookie: theme=dark; Path=/",
                        "Vary: Accept-Encoding",
                        "Connection: keep-alive",
                    ]
                    .join("\r\n");
                    let _ = c.write_all(format!("{head}\r\n\r\nhi").as_bytes());
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn what_the_visitor_receives_is_what_the_box_sent_except_for_the_rewrites() {
        // The other half of the differential. The proxy may replace the
        // connection-lifetime headers and drop a share cookie the box tried to
        // set; everything else the app chose — its content type, its caching,
        // its own cookies, its security headers — has to arrive, or the page
        // the visitor sees is not the page the app served.
        let port = header_rich_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        let got = request(
            addr,
            &format!("GET / HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\r\n"),
        )
        .await;
        assert!(got.starts_with("HTTP/1.1 200 OK"), "{got}");

        for must in [
            "Content-Type: text/html; charset=utf-8",
            "Content-Length: 2",
            "Cache-Control: no-store, max-age=0",
            "ETag: \"abc123\"",
            "X-Frame-Options: DENY",
            "Set-Cookie: sid=9; Path=/; HttpOnly",
            "Set-Cookie: theme=dark; Path=/",
            "Vary: Accept-Encoding",
        ] {
            assert!(
                got.contains(must),
                "the box sent `{must}` and the visitor did not get it: {got}"
            );
        }
        assert!(got.ends_with("hi"), "the body did not arrive: {got}");

        // And the one header the proxy owns says what it must.
        assert!(got.contains("Connection: close"), "{got}");
        assert!(!got.to_ascii_lowercase().contains("keep-alive"), "{got}");

        // The reverse direction, which the first version of this test did not
        // have: a proxy that replaced the head with a canned one carrying the
        // eight strings above would have passed it. Every header the visitor
        // received has to be one the box sent, or one this proxy owns.
        let sent_names = [
            "content-type",
            "content-length",
            "cache-control",
            "etag",
            "x-frame-options",
            "set-cookie",
            "vary",
            "connection",
        ];
        for line in got.split("\r\n").skip(1) {
            if line.is_empty() {
                break;
            }
            let name = line.split(':').next().unwrap_or("").to_ascii_lowercase();
            assert!(
                sent_names.contains(&name.as_str()),
                "the visitor received `{line}`, which the box never sent"
            );
        }

        serving.abort();
    }

    #[tokio::test]
    async fn a_revoke_reaches_every_connection_in_flight_and_they_all_report() {
        // Revocation is the promise the whole grant model rests on, and it has
        // only ever been tested with one connection open. Under load there are
        // two separate things to get wrong: a connection the watchdog misses,
        // which keeps serving somebody the sharer meant to cut off; and a
        // connection dropped without recording what it moved, which is exactly
        // the traffic a reviewer most wants to see in the receipt.
        let port = stalling_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        // Forty connections, each parked mid-response by a server that
        // promises a hundred bytes and sends ten.
        const N: usize = 40;
        let mut held = Vec::new();
        for _ in 0..N {
            use tokio::io::AsyncWriteExt;
            let mut c = tokio::net::TcpStream::connect(addr).await.expect("connect");
            c.write_all(
                format!("GET / HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .expect("write");
            held.push(c);
        }
        for _ in 0..400 {
            if bridge.free_slots() == Bridge::MAX_CONNECTIONS - N {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(
            bridge.free_slots(),
            Bridge::MAX_CONNECTIONS - N,
            "the connections never all took a slot"
        );

        // Revoked from another process, which is how a revoke actually happens.
        let id = crate::session::read(dir.path()).expect("session").grants[0]
            .id
            .clone();
        crate::run::revoke(dir.path(), &id).expect("revoke");

        // Every one of them goes, and within the watchdog's poll rather than
        // eventually: the CLI tells the sharer "within a second".
        for _ in 0..400 {
            if bridge.free_slots() == Bridge::MAX_CONNECTIONS {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(
            bridge.free_slots(),
            Bridge::MAX_CONNECTIONS,
            "a revoke left connections serving"
        );

        // And each of them said what it carried on the way out.
        bridge.write_receipt();
        let receipt = receipt_of(dir.path());
        assert!(receipt.contains(&format!("{N} connections")), "{receipt}");
        // Deliberately not asserting per-connection bytes: the receipt renders
        // one line per *peer* with aggregate counts, and all forty of these
        // are one peer — so one connection reporting and thirty-nine
        // reporting nothing produces an identical line. The first version of
        // this test claimed otherwise. What is checkable is that the aggregate
        // is too large to have come from one of them.
        //
        // Read off the tally rather than parsed back out of the rendering: the
        // second version of this scraped the number from the receipt text and
        // broke the moment those counts grew a unit, which is a test coupled
        // to a format it has no opinion about.
        let out: u64 = bridge
            .snapshot()
            .peers
            .iter()
            .map(|p| p.bytes_to_peer)
            .sum();
        assert!(
            out >= 10 * N as u64,
            "the aggregate is too small for {N} connections that each got ten bytes: {receipt}"
        );

        drop(held);
        serving.abort();
    }

    /// Declares ten bytes and sends far more, then keeps talking.
    fn overlong_server() -> u16 {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for conn in l.incoming() {
                let Ok(mut c) = conn else { continue };
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    let _ = c.read(&mut buf);
                    let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n");
                    let _ = c.write_all(b"0123456789");
                    // Everything past the declared length. A visitor's client
                    // reads ten bytes and then treats whatever follows as the
                    // start of the next response on the connection.
                    let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nOWNED");
                    let _ = c.write_all(&vec![b'x'; 4096]);
                    std::thread::sleep(Duration::from_secs(30));
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn a_box_that_sends_more_than_it_declared_cannot_smuggle_a_second_response() {
        // The response-smuggling shape from the box's side. If the front
        // forwarded everything the box wrote rather than exactly the length it
        // declared, the bytes after the body would be read by the visitor's
        // client as a *second* response — one the app appended itself, on a
        // connection the visitor believes belongs to the page they asked for.
        // Nothing tested this: every framing test so far has been about the
        // head, and this is about what happens after it.
        //
        // Verified by construction, and the construction is worth recording:
        // the length is clamped in *two* places — once for the bytes that
        // arrived in the same read as the head, once for the relay loop — and
        // removing either alone leaves the other covering it for this input.
        // Only removing both makes this test fail. So it pins the property
        // rather than one of the two implementations of it.
        let port = overlong_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        let got = request(
            addr,
            &format!("GET / HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\r\n"),
        )
        .await;

        assert!(got.starts_with("HTTP/1.1 200 OK"), "{got}");
        assert!(
            got.ends_with("0123456789"),
            "the declared body did not arrive whole: {got}"
        );
        assert!(
            !got.contains("OWNED"),
            "the box smuggled a second response past the declared length: {got}"
        );
        // And the padding after it is gone too — the visitor gets the body and
        // then the connection ends, which is what `Content-Length: 10` means.
        assert!(!got.contains("xxxx"), "{got}");

        serving.abort();
    }

    /// Linux-only, and this one alone among the tests this module recovered
    /// when macOS got a dialer.
    ///
    /// It is the heaviest test here by a distance: it holds `MAX_CONNECTIONS`
    /// stalled relays open at once and then waits on the scheduler to hand a
    /// slot back. In isolation on macOS it passes every time (15 for 15). Run
    /// as part of the whole suite, where a couple of hundred other tests are
    /// competing for the same machine, it failed about one run in twelve — and
    /// still one in fifteen after the poll budget was tripled to fifteen
    /// seconds, which is long past the point where a longer wait is a fix
    /// rather than a way of not looking.
    ///
    /// So it stays on the platform where it is stable rather than becoming the
    /// flaky test everybody learns to re-run. What it covers — `over_capacity`,
    /// the `503`, and the slot coming back — is platform-independent logic that
    /// Linux CI checks on every push. The gap is honest and it is narrow: no
    /// *behaviour* is unverified on macOS, only this timing-sensitive way of
    /// verifying it.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_share_at_its_ceiling_says_so_and_keeps_the_ones_it_has() {
        // Nothing tested this. `over_capacity` is the one counter that says a
        // share was hammered, and every test that exercised the sentence built
        // the `Summary` by hand — so the increment, the `503`, and the release
        // of a slot afterwards were all unverified.
        let port = stalling_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        // Hold every slot. `stalling_server` never finishes a response, so each
        // of these stays in the relay loop.
        let all = crate::bridge::Bridge::MAX_CONNECTIONS;
        let mut held = Vec::new();
        for _ in 0..all {
            use tokio::io::AsyncWriteExt;
            let mut c = tokio::net::TcpStream::connect(addr).await.expect("connect");
            c.write_all(
                format!("GET / HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .expect("write");
            held.push(c);
        }
        // Wait for them all to have taken a slot rather than sleeping a guess.
        for _ in 0..600 {
            if bridge.free_slots() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(
            bridge.free_slots(),
            0,
            "the connections never took their slots"
        );

        // One more, with a perfectly good ticket.
        let over = request(
            addr,
            &format!("GET / HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\r\n"),
        )
        .await;
        assert!(over.starts_with("HTTP/1.1 503 "), "{over}");

        // And it is recorded as load, not as a credential problem — the two
        // mean opposite things about what happened to this share.
        bridge.write_receipt();
        let receipt = receipt_of(dir.path());
        assert!(
            receipt.contains("capacity 1 connection(s) refused"),
            "{receipt}"
        );
        assert!(
            !receipt.contains("refused  1 attempt"),
            "a busy share read as a probed one: {receipt}"
        );

        // Letting one go frees exactly one slot.
        held.pop();
        for _ in 0..600 {
            if bridge.free_slots() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(
            bridge.free_slots(),
            1,
            "a finished connection did not give its slot back"
        );

        serving.abort();
    }

    /// The watchdog, driven with two real processes.
    ///
    /// `cloudflared` is not needed and deliberately not used: the thing under
    /// test is "when that pid dies, kill this one", and `/bin/sleep` states it
    /// without a network, a Cloudflare account, or a binary this repo does not
    /// ship. The Linux arm cannot be tested this way at all — `PR_SET_PDEATHSIG`
    /// is set by the child on itself, so there is nothing to point at a pid of
    /// our choosing.
    #[cfg(target_os = "macos")]
    mod watchdog {
        use super::*;

        fn sleeper() -> std::process::Child {
            std::process::Command::new("/bin/sleep")
                .arg("120")
                .spawn()
                .expect("spawn a stand-in process")
        }

        /// Whether the process has exited — asked through `try_wait`, not
        /// through `kill(pid, 0)`.
        ///
        /// That distinction cost an hour. These stand-ins are children of the
        /// **test** process, so a killed one becomes a zombie until it is
        /// reaped, and a zombie answers `kill(pid, 0)` with success. The first
        /// version of these tests asked that way and reported that the watchdog
        /// had failed to kill anything, while it had in fact killed it every
        /// time. Only the parent's own `wait` can tell the difference.
        fn exited(p: &mut std::process::Child) -> bool {
            matches!(p.try_wait(), Ok(Some(_)))
        }

        /// Waits up to ~5s. A watchdog that fires eventually is still a
        /// watchdog; one that never fires is the bug.
        fn died_within(p: &mut std::process::Child) -> bool {
            for _ in 0..100 {
                if exited(p) {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            false
        }

        #[test]
        fn a_killed_parent_takes_the_tunnel_with_it() {
            // The whole point: `SIGKILL` skips every destructor h5i has, so
            // this is the only thing standing between a killed share and a
            // public hostname pointing at a freed port.
            let mut parent = sleeper();
            let mut target = sleeper();
            let (ppid, tpid) = (parent.id(), target.id());

            crate::tunnel::arm_parent_death_kill(ppid as libc::pid_t, tpid as libc::pid_t);
            std::thread::sleep(Duration::from_millis(200));
            assert!(
                !exited(&mut target),
                "the watchdog killed it before anything died"
            );

            unsafe { libc::kill(ppid as libc::pid_t, libc::SIGKILL) };
            let _ = parent.wait();
            assert!(
                died_within(&mut target),
                "the share was SIGKILLed and `cloudflared` outlived it — the hazard this exists \
                 to close"
            );
            let _ = target.kill();
            let _ = target.wait();
        }

        #[test]
        fn a_tunnel_that_ends_first_retires_the_watchdog_rather_than_arming_it() {
            // The pid-reuse hazard, and the reason both pids are registered.
            // `cloudflared` exiting normally is the ordinary end of a share;
            // if the watchdog stayed armed on `watch` alone it would later
            // wake and SIGKILL whatever had inherited the recycled pid.
            let mut parent = sleeper();
            let mut target = sleeper();
            let (ppid, tpid) = (parent.id(), target.id());

            crate::tunnel::arm_parent_death_kill(ppid as libc::pid_t, tpid as libc::pid_t);
            std::thread::sleep(Duration::from_millis(200));

            // The tunnel ends first.
            let _ = target.kill();
            let _ = target.wait();
            std::thread::sleep(Duration::from_millis(300));

            // Now the parent goes. Nothing should be signalled — and the proof
            // available to a test is that the watchdog has already exited, so
            // there is nothing left to signal anybody.
            unsafe { libc::kill(ppid as libc::pid_t, libc::SIGKILL) };
            let _ = parent.wait();
            std::thread::sleep(Duration::from_millis(300));

            // A third process standing in for whoever gets the recycled pid.
            // It must survive; the watchdog is gone and holds no claim on it.
            let mut bystander = sleeper();
            std::thread::sleep(Duration::from_millis(500));
            assert!(
                !exited(&mut bystander),
                "the watchdog outlived the tunnel it was watching and would kill a stranger"
            );
            let _ = bystander.kill();
            let _ = bystander.wait();
        }

        #[test]
        fn a_parent_already_gone_kills_the_tunnel_at_once() {
            // The window between spawning `cloudflared` and arming the
            // watchdog. A share killed in it must not leave the tunnel up.
            let mut parent = sleeper();
            let ppid = parent.id();
            unsafe { libc::kill(ppid as libc::pid_t, libc::SIGKILL) };
            let _ = parent.wait();

            let mut target = sleeper();
            let tpid = target.id();
            crate::tunnel::arm_parent_death_kill(ppid as libc::pid_t, tpid as libc::pid_t);
            assert!(
                died_within(&mut target),
                "a watchdog armed after its parent had died left the tunnel running"
            );
            let _ = target.kill();
            let _ = target.wait();
        }
    }

    #[test]
    fn every_no_url_message_repeats_what_cloudflared_said() {
        // The reason `cloudflared` prints is the whole diagnosis, and only one
        // of the three ways this can fail was repeating it. The timeout arm —
        // the one a blocked or throttled network reaches, which is the common
        // case — said "quick tunnels need outbound network access" and threw
        // the actual line away.
        let said = Some("failed to create tunnel: 403 from api.trycloudflare.com");
        for how in [NoUrl::Exited, NoUrl::WentQuiet, NoUrl::TimedOut] {
            let msg = format!("{}", no_url(how, said));
            assert!(msg.contains("403 from api.trycloudflare.com"), "{msg}");
        }

        // And when it said nothing, all three say so and point at the command
        // that would show it, rather than inventing a reason.
        for how in [NoUrl::Exited, NoUrl::WentQuiet, NoUrl::TimedOut] {
            let msg = format!("{}", no_url(how, None));
            assert!(msg.contains("said nothing at all"), "{msg}");
            assert!(msg.contains("cloudflared tunnel --url"), "{msg}");
        }

        // A child that is still running is not described as having exited. The
        // pipe can end on a read error with the process alive, and that arm
        // sent the operator looking for a crash that never happened.
        let quiet = format!("{}", no_url(NoUrl::WentQuiet, None));
        assert!(quiet.contains("still running"), "{quiet}");
        assert!(!quiet.contains("exited"), "{quiet}");

        // Whatever it said is sanitised on the way through: it is a third
        // party's bytes going to a terminal.
        let nasty = format!("{}", no_url(NoUrl::Exited, Some("boom \u{1b}[2J[31mred")));
        assert!(!nasty.contains('\u{1b}'), "{nasty}");
    }

    #[tokio::test]
    async fn a_missing_cloudflared_says_what_to_install_and_what_else_to_try() {
        // Only meaningful when cloudflared really is absent; where it is
        // installed this asserts nothing, which is the right trade for not
        // making the suite depend on a third-party binary.
        if which_cloudflared() {
            return;
        }
        let err = start(3000).await.expect_err("no cloudflared");
        let msg = format!("{err}");
        assert!(msg.contains("cloudflared"), "{msg}");
        assert!(msg.contains("h5i join"), "{msg}");
    }

    // ─── the loopback side, end to end ──────────────────────────────────────
    //
    // `cloudflared` is a plain reverse proxy into this listener, so everything
    // between it and the dev server can be tested by connecting to the listener
    // directly. That covers the gate, the grant table, the dialer and the byte
    // pump — every part of the tunnel path except Cloudflare itself.

    use crate::session::{self, ShareSession};

    /// A stand-in for the dev server in a box. Answers one canned response per
    /// connection. Never joined — see the note in `p2p`'s equivalent.
    fn fake_dev_server() -> u16 {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for conn in l.incoming() {
                let Ok(mut c) = conn else { continue };
                // Read until the peer pauses, not once. A request's head and its
                // body are separate writes and TCP is free to deliver them
                // separately, so a single `read` sees the head alone often
                // enough to make a body assertion flaky under load.
                let _ = c.set_read_timeout(Some(Duration::from_millis(250)));
                let mut got = Vec::new();
                let mut buf = [0u8; 4096];
                while let Ok(n) = c.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    got.extend_from_slice(&buf[..n]);
                }
                let head = String::from_utf8_lossy(&got).to_string();
                // Echo the request back in the body, so a test can assert on
                // what the box actually received.
                let body = format!("SAW<{head}>");
                let _ = c.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            }
        });
        port
    }

    async fn tunnel_bridge(
        dir: &std::path::Path,
        port: u16,
    ) -> (Arc<Bridge>, String, tokio::net::TcpListener) {
        let mut sess = ShareSession::new(
            "env/test/demo",
            port,
            crate::session::Transport::Tunnel,
            "https://test.trycloudflare.com",
            chrono::Utc::now(),
        );
        let (grant, secret) = session::mint_grant(None, 4_000_000_000).unwrap();
        sess.grants.push(grant);
        session::write(dir, &sess).expect("write session");
        let dialer = crate::dialer::Dialer::spawn_local(port).expect("dialer");
        let bridge = Arc::new(Bridge::new(
            dir.to_path_buf(),
            "env/test/demo".into(),
            "digest".into(),
            "demo".into(),
            crate::session::Transport::Tunnel,
            "https://test.trycloudflare.com".into(),
            dialer,
            crate::bridge::ClaimedRecord::on_disk(dir),
        ));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind");
        (bridge, secret, listener)
    }

    /// One request, one connection, exactly as `cloudflared` would send it.
    ///
    /// The `Result` is handed back rather than swallowed, and `request_strict`
    /// is what most tests should use. A `let _ =` here hid a real defect for a
    /// whole round: closing a socket with unread request bytes queued makes the
    /// kernel send an RST, which tells the peer's stack to throw away its
    /// receive buffer — so the response *was* written, `out` kept the bytes
    /// that arrived before the error, and the assertion passed in exactly the
    /// scenario where a browser shows "connection reset".
    async fn request_raw(
        addr: std::net::SocketAddr,
        head: &str,
    ) -> (String, std::io::Result<usize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut c = tokio::net::TcpStream::connect(addr).await.expect("connect");
        c.write_all(head.as_bytes()).await.expect("write");
        let mut out = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(5), c.read_to_end(&mut out))
            .await
            .unwrap_or(Ok(0));
        (String::from_utf8_lossy(&out).to_string(), read)
    }

    async fn request(addr: std::net::SocketAddr, head: &str) -> String {
        request_raw(addr, head).await.0
    }

    /// Like `request`, and insists the connection ended cleanly — no reset.
    async fn request_strict(addr: std::net::SocketAddr, head: &str) -> String {
        let (body, read) = request_raw(addr, head).await;
        read.expect("the connection was reset rather than closed");
        body
    }

    #[tokio::test]
    async fn the_link_admits_a_browser_and_nothing_else_does() {
        let port = fake_dev_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        // No credential at all: refused without touching the box.
        let anon = request(addr, "GET / HTTP/1.1\r\nHost: t\r\n\r\n").await;
        assert!(anon.starts_with("HTTP/1.1 401 "), "{anon}");
        assert!(
            !anon.contains("SAW<"),
            "an anonymous request reached the box"
        );

        // Following the invite link: bounced, with the token moved to a cookie.
        let first = request(addr, "GET /dash HTTP/1.1\r\nHost: t\r\nCookie: x=1\r\n\r\n").await;
        assert!(first.starts_with("HTTP/1.1 401 "), "{first}");
        let invited = request(
            addr,
            &format!("GET /dash?h5i={secret} HTTP/1.1\r\nHost: t\r\n\r\n"),
        )
        .await;
        assert!(invited.contains("302"), "{invited}");
        assert!(invited.contains("Location: /dash"), "{invited}");
        assert!(invited.contains("Set-Cookie: h5i_share="), "{invited}");
        assert!(
            !invited.contains("SAW<"),
            "the invite request reached the box"
        );

        // With the cookie, it reaches the dev server — and the dev server never
        // sees the credential that admitted the visitor.
        let served = request(
            addr,
            &format!("GET /dash HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}; sid=9\r\n\r\n"),
        )
        .await;
        assert!(served.contains("SAW<"), "{served}");
        assert!(
            !served.contains(&secret),
            "the credential reached the box: {served}"
        );
        assert!(
            served.contains("Cookie: sid=9"),
            "the app's own cookie was dropped: {served}"
        );
        assert!(served.contains("Connection: close"), "{served}");

        serving.abort();
    }

    #[tokio::test]
    async fn a_browser_is_told_the_share_ended_rather_than_to_ask_for_a_new_link() {
        // The gate answers every ticket refusal with the same `401` so that a
        // prober learns nothing, and that is right for unknown, expired and
        // revoked. It is wrong for the two refusals that are not about the
        // visitor: a share that has been stopped, and a machine that cannot
        // read its own grant table. Both told the visitor their invite was no
        // good and to ask for another, which would fail identically.
        let port = fake_dev_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        // It works first, so the difference below is the record and not the
        // ticket.
        let ok = request(
            addr,
            &format!("GET / HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\r\n"),
        )
        .await;
        assert!(ok.contains("SAW<"), "{ok}");

        // Kept for the last step, which needs *this* bridge's record rather
        // than a look-alike: a record with a different identity is a different
        // share, which the bridge reads as "my share is over".
        let claimed = session::read(dir.path()).expect("the record this bridge claimed");

        // The share is stopped out from under the serving process, which is
        // what `share stop --force` does and what the last moment of every
        // ordinary stop looks like.
        std::fs::remove_file(dir.path().join("share.json")).expect("stop the share");
        let over = request(
            addr,
            &format!("GET / HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\r\n"),
        )
        .await;
        assert!(over.starts_with("HTTP/1.1 410 "), "{over}");
        assert!(over.contains("This share has ended"), "{over}");
        assert!(!over.contains("ask"), "{over}");
        assert!(!over.contains("SAW<"), "a stopped share reached the box");

        // And a table that is there and cannot be read is the other sentence:
        // a new link would fail the same way, so nobody is sent to fetch one.
        std::fs::write(dir.path().join("share.json"), b"{ not a record").expect("junk");
        let broken = request(
            addr,
            &format!("GET / HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\r\n"),
        )
        .await;
        assert!(broken.starts_with("HTTP/1.1 503 "), "{broken}");
        assert!(broken.contains("could not read its own record"), "{broken}");
        assert!(
            !broken.contains("SAW<"),
            "an unreadable table reached the box"
        );

        // The refusals that *are* about the ticket keep the one `401` they
        // have always had: this must not become an oracle for which of
        // unknown, expired and revoked a probe hit.
        // The *same* record with its grants taken out. See
        // `bridge::ClaimedRecord`.
        let mut sess = claimed;
        sess.grants.clear();
        session::write(dir.path(), &sess).expect("empty table");
        let unknown = request(
            addr,
            &format!("GET / HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\r\n"),
        )
        .await;
        assert!(unknown.starts_with("HTTP/1.1 401 "), "{unknown}");

        serving.abort();
    }

    /// A handler that has been accepted and not yet authorized is work the
    /// teardown waits for.
    ///
    /// Quiescence was defined by the sixty-four `Bridge::admit` permits, and a
    /// handler paused in `read_head`, in parsing, or in authorization holds
    /// none of them — so `quiesce` acquired all sixty-four immediately, marked
    /// the receipt settled, and returned while such a handler was still live.
    /// On Ctrl-C or a transport failure the record is merely `winding_up` and
    /// its grants are still there, so the handler could then resume,
    /// authorize, take a now-free permit, dial the box, and change something
    /// inside it *after* the receipt had snapshotted its tally. An orderly stop
    /// was neither a barrier on access nor a complete account of it.
    #[tokio::test]
    async fn a_connection_accepted_before_the_stop_cannot_reach_the_box_after_it() {
        let port = fake_dev_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        // Accepted, with its head not yet sent: the handler is parked in
        // `read_head`, holding no `admit` permit.
        use tokio::io::AsyncWriteExt;
        let mut paused = tokio::net::TcpStream::connect(addr).await.expect("connect");
        tokio::time::sleep(Duration::from_millis(100)).await;

        // The share is stopped, and quiescence finishes.
        bridge.begin_shutdown();
        bridge.quiesce(Duration::from_secs(2)).await;

        // Now the paused visitor sends a perfectly good, authorized request.
        paused
            .write_all(
                format!("GET /late HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .expect("write");
        let mut out = Vec::new();
        use tokio::io::AsyncReadExt;
        let _ = tokio::time::timeout(Duration::from_secs(3), paused.read_to_end(&mut out)).await;
        let got = String::from_utf8_lossy(&out).to_string();
        assert!(
            !got.contains("SAW<"),
            "a request accepted before the stop reached the box after it: {got}"
        );

        serving.abort();
    }

    /// Every refusal that is not about a credential lands in the receipt.
    ///
    /// The reason was being recovered by testing the rendered response for
    /// `HTTP/1.1 400`, which reached the malformed-request refusals and left
    /// both `403`s counted nowhere. Those two are the gate refusing a
    /// foreign-origin browser request that arrived with the share cookie
    /// attached, and refusing a service worker registration that would keep
    /// control of the joiner's loopback origin after the share ended — at
    /// least as relevant to an ingress receipt as an unparsable head. A
    /// session made entirely of them reported no turned-away activity at all,
    /// under a heading that describes itself as the account of connections
    /// rejected before a ticket was weighed.
    #[tokio::test]
    async fn the_refusals_that_are_not_about_a_ticket_reach_the_receipt() {
        let port = fake_dev_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        // A page on another site, navigating the visitor's browser at this
        // share with the cookie the browser attaches on its own.
        let foreign = request(
            addr,
            &format!(
                "GET /reset HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\
                 Sec-Fetch-Site: cross-site\r\nSec-Fetch-Mode: navigate\r\n\
                 Sec-Fetch-Dest: document\r\n\r\n"
            ),
        )
        .await;
        assert!(foreign.starts_with("HTTP/1.1 403 "), "{foreign}");

        // And a service worker registration.
        let worker = request(
            addr,
            &format!(
                "GET /sw.js HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\
                 Service-Worker: script\r\n\r\n"
            ),
        )
        .await;
        assert!(worker.starts_with("HTTP/1.1 403 "), "{worker}");

        // A malformed head, which was the one that already counted.
        let bad = request(addr, "GET / HTTP/1.1\r\nHost: t\rX: y\r\n\r\n").await;
        assert!(bad.starts_with("HTTP/1.1 400 "), "{bad}");

        let body = crate::bridge::render_receipt(&bridge.snapshot());
        assert!(
            body.contains("came from another page"),
            "a foreign-origin refusal left no trace: {body}"
        );
        assert!(
            body.contains("service worker"),
            "a service-worker refusal left no trace: {body}"
        );
        assert!(body.contains("would not parse"), "{body}");

        // And the two answers that are still keyed off the refusal rather than
        // off the rendered status: a stopped share tells its visitors so, and
        // somebody knocking with nothing is counted. Both used to be recovered
        // by `starts_with("HTTP/1.1 401")` on bytes this file had just
        // rendered, which is the recovery the block above was rewritten to
        // stop doing.
        let anonymous = request(addr, "GET / HTTP/1.1\r\nHost: t\r\n\r\n").await;
        assert!(anonymous.starts_with("HTTP/1.1 401 "), "{anonymous}");
        let body = crate::bridge::render_receipt(&bridge.snapshot());
        assert!(
            body.contains("presented no invite"),
            "a visitor knocking with nothing was not counted: {body}"
        );

        serving.abort();
    }

    /// A head the *reader* refuses reaches the receipt too.
    ///
    /// `turned_away` counted only heads that were read and then refused by the
    /// gate, and the head reader refuses several shapes before the gate ever
    /// sees them: a header block past `MAX_HEAD`, bytes that are not UTF-8, a
    /// head the peer stops feeding. Every one of them was dropped with no
    /// response and no line anywhere — so the largest smuggling-shaped probe a
    /// public share can be sent was the one its receipt could not mention.
    ///
    /// And the other half, which is why this is two answers and not one: a peer
    /// that opens a socket and says nothing still leaves no trace. That is a
    /// browser's speculative preconnect, and recording it would bury every row
    /// that means something under noise anybody can generate for free.
    #[tokio::test]
    async fn a_head_the_reader_would_not_take_is_still_somebody_turned_away() {
        use tokio::io::AsyncWriteExt;
        let port = fake_dev_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, _secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        // Thirty-two kilobytes of header and no end to it.
        let mut flood = tokio::net::TcpStream::connect(addr).await.expect("connect");
        let oversize = format!(
            "GET / HTTP/1.1\r\nHost: t\r\nX: {}\r\n",
            "a".repeat(crate::gate::MAX_HEAD + 64)
        );
        let _ = flood.write_all(oversize.as_bytes()).await;
        let _ = flood.shutdown().await;

        // And a TLS hello aimed at the plaintext front.
        let mut tls = tokio::net::TcpStream::connect(addr).await.expect("connect");
        let _ = tls
            .write_all(b"\x16\x03\x01\x00\x2a\x01\x00\x00\x26\r\n\r\n")
            .await;
        let _ = tls.shutdown().await;

        // Both are recorded, and the count is two.
        for _ in 0..50 {
            if bridge.snapshot().turned_away.len() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let body = crate::bridge::render_receipt(&bridge.snapshot());
        assert!(
            body.contains("2 sent something this share would not parse"),
            "a head the reader refused left no trace: {body}"
        );

        // A peer that says nothing adds nothing.
        let quiet = tokio::net::TcpStream::connect(addr).await.expect("connect");
        drop(quiet);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let body = crate::bridge::render_receipt(&bridge.snapshot());
        assert!(
            body.contains("2 sent something this share would not parse"),
            "a preconnect was recorded as somebody being turned away: {body}"
        );

        serving.abort();
    }

    /// A dev server that does **not** honour `Connection: close`: it answers
    /// keep-alive and stays open. Records every request head it sees.
    fn stubborn_keepalive_server() -> (u16, Arc<Mutex<Vec<String>>>) {
        use std::io::{Read, Write};
        let seen: Arc<Mutex<Vec<String>>> = Default::default();
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = l.local_addr().unwrap().port();
        let out = seen.clone();
        std::thread::spawn(move || {
            for conn in l.incoming() {
                let Ok(mut c) = conn else { continue };
                let seen = out.clone();
                std::thread::spawn(move || loop {
                    let mut buf = [0u8; 4096];
                    let n = match c.read(&mut buf) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    seen.lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&buf[..n]).to_string());
                    let _ = c.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nhi",
                    );
                });
            }
        });
        (port, seen)
    }

    /// The same server, answering with a chunked body instead of a length.
    fn stubborn_chunked_server() -> u16 {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for conn in l.incoming() {
                let Ok(mut c) = conn else { continue };
                std::thread::spawn(move || loop {
                    let mut buf = [0u8; 4096];
                    match c.read(&mut buf) {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }
                    // A complete chunked response: two chunks, the
                    // terminating zero chunk, and the blank line that closes
                    // the (empty) trailer section. Then it keeps the socket
                    // open and says nothing more.
                    let _ = c.write_all(
                        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\
                          Connection: keep-alive\r\n\r\n\
                          2\r\nhi\r\n3\r\n th\r\n0\r\n\r\n",
                    );
                });
            }
        });
        port
    }

    /// A chunked response ends at its terminating chunk, not at the close.
    ///
    /// `Transfer-Encoding: chunked` was classified as "until the box closes",
    /// and this file already treats a server that ignores `Connection: close`
    /// as a supported case. Against one, the browser had a complete response
    /// in hand while the relay waited out `RESPONSE_IDLE` — five minutes with
    /// the `Bridge::admit` permit still held. Sixty-four requests and the share
    /// answers `busy` for that whole interval; a steady trickle keeps it there.
    /// The existing stubborn-keepalive coverage used a `Content-Length`, whose
    /// arithmetic released the permit, so none of this was exercised.
    #[tokio::test]
    async fn a_chunked_response_releases_its_slot_at_the_terminating_chunk() {
        let port = stubborn_chunked_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        let req = format!("GET /a HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\r\n");
        // The assertion is the *timing*, not just the bytes. Without the
        // terminator scan the relay waits for a close this server will never
        // send, so the visitor's connection stays open until `RESPONSE_IDLE`
        // five minutes later — the test client's own five-second read timeout
        // is what would end it, which is why this measures rather than trusts.
        let started = std::time::Instant::now();
        let got = request_strict(addr, &req).await;
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the relay held the connection open after the terminating chunk: {:?}",
            started.elapsed()
        );
        assert!(got.contains("0\r\n\r\n"), "the body was not relayed: {got}");

        // And the permit came back: the share is not carrying anything.
        assert_eq!(
            bridge.snapshot().over_capacity,
            0,
            "a completed chunked response left the share at capacity"
        );
        // Not recorded as a truncation, either: the box said everything it
        // meant to say.
        assert_eq!(bridge.snapshot().truncated, 0);

        serving.abort();
    }

    #[tokio::test]
    async fn a_second_request_cannot_ride_in_on_an_authorized_connection() {
        // The control this feature rests on, tested against the case that
        // breaks the polite version of it: a dev server that ignores
        // `Connection: close`. The box runs agent-written code, so asking it to
        // hang up is a request, not a guarantee — the proxy has to stop reading
        // the client itself.
        let (port, seen) = stubborn_keepalive_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        // Both requests in one write, as a connection-pool reuse or a
        // pipelining client would deliver them. The second carries no
        // credential at all.
        let pipelined = format!(
            "GET /first HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\r\n\
             GET /smuggled HTTP/1.1\r\nHost: t\r\n\r\n"
        );
        let got = request(addr, &pipelined).await;
        assert!(
            got.contains("hi"),
            "the first request should be served: {got}"
        );

        tokio::time::sleep(Duration::from_millis(200)).await;
        let seen = seen.lock().unwrap().join("");
        assert!(
            seen.contains("/first"),
            "the first request never arrived: {seen}"
        );
        assert!(
            !seen.contains("/smuggled"),
            "an ungated second request reached the box: {seen}"
        );

        serving.abort();
    }

    #[tokio::test]
    async fn a_connection_both_ends_stop_using_does_not_hold_a_slot_forever() {
        // The box may ignore `Connection: close`, so the proxy cannot wait for
        // it to hang up. If nobody watched the client either, a quiet
        // connection would hold one of the share's slots until the grant
        // expired — up to a day.
        let (port, _seen) = stubborn_keepalive_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut c = tokio::net::TcpStream::connect(addr).await.expect("connect");
            c.write_all(
                format!("GET / HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .expect("write");
            let mut buf = [0u8; 128];
            let _ = tokio::time::timeout(Duration::from_secs(2), c.read(&mut buf)).await;
            // Walk away. The box's side is still open and still says
            // keep-alive, so only the client going quiet can end this.
            drop(c);
        }

        // The slot has to come back. All 64 of them, in fact — if the
        // connection were still holding one, only 63 would be free.
        //
        // Polled rather than slept, with a budget far past what this ever
        // needs: the release happens when a task the test does not own gets
        // scheduled, and a fixed wait is a test that fails on a busy machine
        // for a reason that has nothing to do with the code. It costs nothing
        // when it passes, which is every time the release actually happens.
        let mut free = 0;
        for _ in 0..600 {
            let mut held = Vec::new();
            while let Some(p) = bridge.admit() {
                held.push(p);
            }
            free = held.len();
            if free == 64 {
                break;
            }
            drop(held);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(free, 64, "a finished connection is still holding a slot");

        serving.abort();
    }

    #[tokio::test]
    async fn a_keep_alive_answer_from_the_box_is_not_relayed_as_one() {
        // The liveness bug this closes: the proxy stops reading the client
        // after one request, so a response telling the client "keep this
        // connection and send me another" produced a connection the client
        // believed reusable and that answered nothing — an intermittent hang,
        // and a 502 for every POST a client will not retry.
        let (port, _seen) = stubborn_keepalive_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        let got = request(
            addr,
            &format!("GET / HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\r\n"),
        )
        .await;
        assert!(got.contains("200 OK"), "{got}");
        assert!(got.contains("Connection: close"), "{got}");
        assert!(!got.to_lowercase().contains("keep-alive"), "{got}");
        // Framed by its `Content-Length`, so the connection ends when the
        // response does rather than waiting on a box that never hangs up.
        assert!(got.ends_with("hi"), "{got}");

        serving.abort();
    }

    #[tokio::test]
    async fn a_chunked_body_reaches_the_box_and_stops_at_its_end() {
        // Every POST through a real quick tunnel arrives chunked: `cloudflared`
        // bridges HTTP/2 to the box's HTTP/1.1 and has no length to carry over.
        // This used to answer `501`, which meant no form on a shared app worked.
        let port = fake_dev_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        let got = request(
            addr,
            &format!(
                "POST /form HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\
                 Transfer-Encoding: chunked\r\n\r\n\
                 9\r\nname=alex\r\n0\r\n\r\n\
                 GET /SMUGGLED HTTP/1.1\r\nHost: t\r\n\r\n"
            ),
        )
        .await;
        assert!(got.contains("name=alex"), "the body did not arrive: {got}");
        assert!(
            !got.contains("SMUGGLED"),
            "a request pipelined after the terminating chunk was forwarded: {got}"
        );

        serving.abort();
    }

    /// A dev server that answers with a framing this proxy refuses to trust.
    fn ambiguous_server() -> u16 {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for conn in l.incoming() {
                let Ok(mut c) = conn else { continue };
                let mut buf = [0u8; 4096];
                let _ = c.read(&mut buf);
                let _ = c.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nContent-Length: 9\r\n\r\nthe body",
                );
            }
        });
        port
    }

    #[tokio::test]
    async fn a_page_with_an_ambiguous_length_still_arrives() {
        // Refusing to trust the framing must not mean refusing to deliver the
        // page: the lengths come off the head and the connection closing is
        // what frames it instead.
        let port = ambiguous_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        let got = request(
            addr,
            &format!("GET / HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\r\n"),
        )
        .await;
        assert!(got.contains("200 OK"), "{got}");
        assert!(!got.to_lowercase().contains("content-length"), "{got}");
        assert!(
            got.ends_with("the body"),
            "the page was not delivered: {got}"
        );

        serving.abort();
    }

    /// A dev server that answers before reading the body and hangs up — a size
    /// limit, or auth that rejects before parsing. The commonest reason a box
    /// closes its read side mid-request.
    fn early_rejecting_server() -> u16 {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for conn in l.incoming() {
                let Ok(mut c) = conn else { continue };
                std::thread::spawn(move || {
                    // The head only. The body is never read.
                    let mut buf = [0u8; 1024];
                    let _ = c.read(&mut buf);
                    let body = b"too big";
                    let _ = c.write_all(
                        format!(
                            "HTTP/1.1 413 Content Too Large\r\nContent-Length: {}\r\n\r\n",
                            body.len()
                        )
                        .as_bytes(),
                    );
                    let _ = c.write_all(body);
                    let _ = c.shutdown(std::net::Shutdown::Both);
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn an_early_rejection_reaches_the_visitor_instead_of_being_replaced() {
        // The box hangs up mid-body because it has already decided. Its answer
        // is sitting in the socket; answering "your request is malformed" and
        // dropping it told the visitor the wrong thing about their own request
        // and hid the app's real reply.
        let port = early_rejecting_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        let big = "x".repeat(400_000);
        let got = request_strict(
            addr,
            &format!(
                "POST /upload HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\
                 Content-Length: {}\r\n\r\n{big}",
                big.len()
            ),
        )
        .await;
        assert!(
            got.contains("413"),
            "the box's own answer did not reach the visitor: {got}"
        );
        assert!(got.contains("too big"), "{got}");

        serving.abort();
    }

    /// The same rejection, from a server that does **not** hang up.
    ///
    /// The existing coverage uses a server that closes, which is the one shape
    /// the old code could notice: a failed write into the box was the signal.
    /// Nothing in HTTP requires that. This one answers and keeps reading
    /// nothing, which is what a framework with a body-size limit does.
    fn polite_early_rejecting_server() -> u16 {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for conn in l.incoming() {
                let Ok(mut c) = conn else { continue };
                std::thread::spawn(move || {
                    let mut buf = [0u8; 1024];
                    let _ = c.read(&mut buf);
                    let body = b"too big";
                    let _ = c.write_all(
                        format!(
                            "HTTP/1.1 413 Content Too Large\r\nContent-Length: {}\r\n\r\n",
                            body.len()
                        )
                        .as_bytes(),
                    );
                    let _ = c.write_all(body);
                    // And then it sits there, socket open, reading nothing.
                    // Held so the connection is not closed by the drop.
                    std::thread::sleep(Duration::from_secs(120));
                    drop(c);
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn an_answer_that_arrives_mid_upload_is_relayed_rather_than_waited_out() {
        // The client declares a large body and sends only a prefix, which is
        // what a paused upload looks like. The box has already answered `413`.
        //
        // The body was forwarded to completion before anything read the box, so
        // the only thing that ended this was `BODY_IDLE` thirty seconds later —
        // and it ended it by *replacing* the box's answer with h5i's own `408`.
        // The visitor was told their upload timed out for a request the app had
        // already refused, and the app's reason never reached them.
        let port = polite_early_rejecting_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut c = tokio::net::TcpStream::connect(addr).await.expect("connect");
        c.write_all(
            format!(
                "POST /upload HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\
                 Content-Length: 400000\r\n\r\n{}",
                "x".repeat(1024)
            )
            .as_bytes(),
        )
        .await
        .expect("write the head and a prefix of the body");

        // Promptly: well inside `BODY_IDLE`, which is what used to end it.
        let mut out = Vec::new();
        let started = std::time::Instant::now();
        let read = tokio::time::timeout(Duration::from_secs(10), c.read_to_end(&mut out)).await;
        assert!(read.is_ok(), "the answer never arrived");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the box's answer waited out the body timeout: {:?}",
            started.elapsed()
        );
        let got = String::from_utf8_lossy(&out).to_string();
        assert!(
            got.contains("413"),
            "the box's own answer was replaced: {got}"
        );
        assert!(got.contains("too big"), "{got}");
        assert!(!got.contains("408"), "{got}");

        serving.abort();
    }

    #[tokio::test]
    async fn a_declared_body_reaches_the_box_whole() {
        // The other half of the one-request rule: stopping at the declared
        // length must not truncate a real form post.
        let port = fake_dev_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        let got = request(
            addr,
            &format!(
                "POST /form HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\
                 Content-Length: 9\r\n\r\nname=alex"
            ),
        )
        .await;
        assert!(got.contains("name=alex"), "the body was truncated: {got}");

        serving.abort();
    }

    /// A dev server that answers an upgrade request however the test says, then
    /// echoes whatever follows. Stands in for a hot-reload socket.
    fn upgrading_server(status: &'static str) -> u16 {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for conn in l.incoming() {
                let Ok(mut c) = conn else { continue };
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    let _ = c.read(&mut buf);
                    let _ = c.write_all(status.as_bytes());
                    // Then behave like a frame-oriented protocol: echo.
                    loop {
                        match c.read(&mut buf) {
                            Ok(0) | Err(_) => return,
                            Ok(n) => {
                                if c.write_all(&buf[..n]).is_err() {
                                    return;
                                }
                            }
                        }
                    }
                });
            }
        });
        port
    }

    /// Send a request and keep the socket, so a test can talk after the head.
    async fn open_request(
        addr: std::net::SocketAddr,
        head: &str,
    ) -> (tokio::net::TcpStream, String) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut c = tokio::net::TcpStream::connect(addr).await.expect("connect");
        c.write_all(head.as_bytes()).await.expect("write");
        let mut buf = [0u8; 256];
        let n = tokio::time::timeout(Duration::from_secs(5), c.read(&mut buf))
            .await
            .map(|r| r.unwrap_or(0))
            .unwrap_or(0);
        (c, String::from_utf8_lossy(&buf[..n]).to_string())
    }

    #[tokio::test]
    async fn hot_reload_gets_its_two_way_pipe_once_the_box_says_101() {
        // A share of a dev server that never hot-reloads is not a share of a
        // dev server, so the upgrade path has to actually work.
        let port = upgrading_server(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\n\r\n",
        );
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        let (mut c, resp) = open_request(
            addr,
            &format!(
                "GET /hmr HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\
                 Upgrade: websocket\r\nConnection: Upgrade\r\n\r\n"
            ),
        )
        .await;
        assert!(resp.contains("101"), "{resp}");

        // Frames both ways, after the head. This is the thing a one-request
        // rule would have broken if the exception were not carved out.
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        c.write_all(b"frame-one").await.expect("write");
        let mut back = [0u8; 9];
        tokio::time::timeout(Duration::from_secs(5), c.read_exact(&mut back))
            .await
            .expect("no echo came back")
            .expect("read");
        assert_eq!(&back, b"frame-one");

        serving.abort();
    }

    #[tokio::test]
    async fn asking_to_upgrade_and_being_refused_does_not_buy_a_two_way_pipe() {
        // The opt-out this closes: a client attaches `Upgrade:` to an ordinary
        // request, the box answers 200 because it has no idea what `h2c` is,
        // and the connection would otherwise have become a raw pipe on which a
        // second, ungated request could ride in.
        let port = upgrading_server(
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nhi",
        );
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        let (mut c, resp) = open_request(
            addr,
            &format!(
                "GET / HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\
                 Upgrade: h2c\r\nConnection: keep-alive, Upgrade\r\n\r\n"
            ),
        )
        .await;
        assert!(resp.contains("200"), "{resp}");

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        c.write_all(b"smuggled").await.expect("write");
        let mut back = [0u8; 8];
        let echoed = tokio::time::timeout(Duration::from_secs(2), c.read_exact(&mut back)).await;
        assert!(
            echoed.is_err() || echoed.unwrap().is_err(),
            "bytes sent after a refused upgrade reached the box"
        );

        serving.abort();
    }

    /// The share's receipt body, as an export would carry it.
    fn receipt_of(dir: &std::path::Path) -> String {
        let log = std::fs::read_to_string(dir.join("receipt.jsonl")).expect("receipt log");
        let line = log
            .lines()
            .find(|l| l.contains("\"source\":\"share\""))
            .expect("a share record");
        let oid = line
            .split("\"raw_oid\":\"sha256:")
            .nth(1)
            .and_then(|r| r.get(..16))
            .expect("a payload id");
        std::fs::read_to_string(dir.join("receipts").join(format!("{oid}.raw")))
            .expect("the payload")
    }

    /// A dev server that promises a hundred bytes and sends ten.
    fn short_server() -> u16 {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for conn in l.incoming() {
                let Ok(mut c) = conn else { continue };
                let mut buf = [0u8; 4096];
                let _ = c.read(&mut buf);
                let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nshort-body");
            }
        });
        port
    }

    #[tokio::test]
    async fn a_refusal_reaches_a_visitor_who_was_still_uploading() {
        // The refusal paths are where a peer is *most* likely to still be
        // sending — it declared a body and we refused part-way through it — so
        // they are where a reset would most reliably destroy the answer. Only
        // two of the seven close paths had the drain when this was written.
        let port = fake_dev_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        // Genuinely still uploading. The first version of this test sent its
        // whole (short) body in one write, so `read_head` had already consumed
        // every byte of it into userspace and the peer had stopped: the
        // precondition it was written for — bytes queued in the kernel when the
        // socket closes — never existed, and it passed with the drain deleted.
        let anon = refused_mid_upload(
            addr,
            "POST / HTTP/1.1\r\nHost: t\r\nContent-Length: 900000\r\n\r\n",
        )
        .await;
        assert!(anon.starts_with("HTTP/1.1 401 "), "{anon}");

        // And a chunked body whose framing does not parse, which is answered
        // mid-request with the peer still mid-send.
        let bad = refused_mid_upload(
            addr,
            &format!(
                "POST / HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\
                 Transfer-Encoding: chunked\r\n\r\nnot-a-chunk-size\r\n"
            ),
        )
        .await;
        assert!(bad.starts_with("HTTP/1.1 400 "), "{bad}");

        serving.abort();
    }

    /// Send a head, then keep pushing body bytes until the far side answers.
    ///
    /// The point is that the peer is *still sending* when the refusal is
    /// written and when the socket closes — which is the state a drain exists
    /// for, and which a single `write_all` of a short body does not produce.
    async fn refused_mid_upload(addr: std::net::SocketAddr, head: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let c = tokio::net::TcpStream::connect(addr).await.expect("connect");
        let (mut r, mut w) = c.into_split();
        w.write_all(head.as_bytes()).await.expect("head");
        let pushing = tokio::spawn(async move {
            let chunk = vec![b'u'; 64 * 1024];
            // Bounded, so a proxy that answers nothing cannot wedge the test.
            for _ in 0..64 {
                if w.write_all(&chunk).await.is_err() {
                    break;
                }
            }
        });
        let mut got = Vec::new();
        let out = tokio::time::timeout(Duration::from_secs(10), r.read_to_end(&mut got))
            .await
            .expect("the proxy never answered");
        pushing.abort();
        out.expect("the connection was reset rather than closed");
        String::from_utf8_lossy(&got).to_string()
    }

    /// Answers every request with eight megabytes, so the response is still in
    /// this side's send queue when the connection closes.
    fn big_server(body: usize) -> u16 {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for conn in l.incoming() {
                let Ok(mut c) = conn else { continue };
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    let _ = c.read(&mut buf);
                    let _ = c.write_all(
                        format!("HTTP/1.1 200 OK\r\nContent-Length: {body}\r\n\r\n").as_bytes(),
                    );
                    let _ = c.write_all(&vec![b'b'; body]);
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn a_big_response_reaches_a_client_that_pipelined_another_request() {
        // Two things at once, and both were broken. The share serves exactly
        // one request per connection, so a keep-alive client's follow-up sits
        // unread — and the relay loop, which reads and discards whatever the
        // peer says, has to keep delivering the response it is in the middle
        // of rather than treating the peer's traffic as a reason to stop.
        //
        // Written first as a test for the linger drain, asserting that the
        // close did not reset the connection. It passed with that drain
        // deleted, so the claim was wrong and the name went with it — see
        // `finish_with`. What it does discriminate is the megabytes.
        const BODY: usize = 8 * 1024 * 1024;
        let port = big_server(BODY);
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let c = tokio::net::TcpStream::connect(addr).await.expect("connect");
        let (mut r, mut w) = c.into_split();
        w.write_all(
            format!("GET / HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\r\n").as_bytes(),
        )
        .await
        .expect("request");
        // A second request this share will never read: it serves exactly one
        // per connection. Any keep-alive client that sends a follow-up leaves
        // the connection in this state, so it is not an exotic shape.
        let pipelined = tokio::spawn(async move {
            let _ = w.write_all(&vec![b'x'; 1024 * 1024]).await;
        });
        // Do not read for a moment, so the response fills the buffers on both
        // sides and there is something left to lose.
        tokio::time::sleep(Duration::from_millis(300)).await;

        let mut got = Vec::new();
        let out = tokio::time::timeout(Duration::from_secs(30), r.read_to_end(&mut got))
            .await
            .expect("the response never ended");
        pipelined.abort();
        out.expect("the connection was reset mid-response");
        assert!(
            got.len() > BODY,
            "got {} bytes of an {BODY}-byte body plus head",
            got.len()
        );

        serving.abort();
    }

    /// Accepts, reads, and hangs up without saying anything.
    fn silent_server() -> u16 {
        use std::io::Read;
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for conn in l.incoming() {
                let Ok(mut c) = conn else { continue };
                let mut buf = [0u8; 4096];
                let _ = c.read(&mut buf);
            }
        });
        port
    }

    #[tokio::test]
    async fn an_upgrade_the_box_never_answers_gets_a_readable_refusal() {
        // This returned with nothing written at all, so the visitor's WebSocket
        // failed with a bare close and no status — which a browser reports as
        // "closed before receiving a handshake response", a sentence that says
        // nothing about where the fault is. Every other silent box gets a 502.
        let port = silent_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        let got = request(
            addr,
            &format!(
                "GET /ws HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\
                 Upgrade: websocket\r\nConnection: Upgrade\r\n\r\n"
            ),
        )
        .await;
        assert!(got.starts_with("HTTP/1.1 502 "), "{got}");

        serving.abort();
    }

    #[tokio::test]
    async fn a_client_that_half_closes_still_gets_its_whole_response() {
        // Sending a request and then shutting down the write side is legal
        // HTTP/1.1 and is what anything built out of one write and one read
        // does — `printf ... | nc`, a CI scraper, `curl -T-`. That EOF was read
        // as "the visitor left", so the relay stopped on the spot and those
        // clients got the first read of a download and a clean close, with
        // nothing recorded anywhere.
        const BODY: usize = 2 * 1024 * 1024;
        let port = big_server(BODY);
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut c = tokio::net::TcpStream::connect(addr).await.expect("connect");
        c.write_all(
            format!("GET / HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\r\n").as_bytes(),
        )
        .await
        .expect("request");
        c.shutdown().await.expect("half close");

        let mut got = Vec::new();
        tokio::time::timeout(Duration::from_secs(30), c.read_to_end(&mut got))
            .await
            .expect("the response never ended")
            .expect("read");
        assert!(
            got.len() > BODY,
            "a half-closed client got {} bytes of an {BODY}-byte body",
            got.len()
        );

        serving.abort();
        bridge.quiesce(Duration::from_secs(2)).await;
        bridge.write_receipt();
        let receipt = receipt_of(dir.path());
        assert!(
            !receipt.contains("truncated"),
            "a complete response was recorded as truncated: {receipt}"
        );
    }

    #[tokio::test]
    async fn a_response_the_box_left_unfinished_is_recorded() {
        let port = short_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        let got = request(
            addr,
            &format!("GET / HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\r\n"),
        )
        .await;
        assert!(got.ends_with("short-body"), "{got}");

        serving.abort();
        bridge.quiesce(Duration::from_secs(2)).await;
        bridge.write_receipt();
        let receipt = receipt_of(dir.path());
        assert!(
            receipt.contains("truncated 1 response(s) the box left unfinished"),
            "{receipt}"
        );
    }

    /// Promises a hundred bytes, sends ten, and then holds the connection open
    /// without ever finishing — so only the *visitor* can end it.
    fn stalling_server() -> u16 {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for conn in l.incoming() {
                let Ok(mut c) = conn else { continue };
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    let _ = c.read(&mut buf);
                    let _ =
                        c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nshort-body");
                    std::thread::sleep(Duration::from_secs(120));
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn a_visitor_who_walks_away_is_not_the_box_truncating() {
        // The commonest way this loop ends is somebody closing a tab. Calling
        // that "the box left a response unfinished" libels the box in the one
        // artifact that is supposed to be evidence.
        let port = stalling_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut c = tokio::net::TcpStream::connect(addr).await.expect("connect");
            c.write_all(
                format!("GET / HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .expect("write");
            let mut buf = [0u8; 64];
            let _ = tokio::time::timeout(Duration::from_secs(5), c.read(&mut buf)).await;
            // And walk away mid-download.
        }

        serving.abort();
        // A real barrier rather than a sleep, and positive assertions as well
        // as the negative one: without them this passes just as well when the
        // request never reached the box at all, which is the wrong reason to be
        // green about a claim concerning what the box did.
        bridge.quiesce(Duration::from_secs(5)).await;
        bridge.write_receipt();
        let receipt = receipt_of(dir.path());
        assert!(
            receipt.contains("1 connection"),
            "no connection was recorded, so nothing was proved about one: {receipt}"
        );
        assert!(
            !receipt.contains(" 0 out"),
            "no bytes reached the visitor, so the relay loop was never entered: {receipt}"
        );
        assert!(
            !receipt.contains("truncated"),
            "the visitor was blamed on the box: {receipt}"
        );
    }

    #[tokio::test]
    async fn a_stopped_share_stops_admitting_the_link() {
        let port = fake_dev_server();
        let dir = tempfile::tempdir().expect("tempdir");
        let (bridge, secret, listener) = tunnel_bridge(dir.path(), port).await;
        let addr = listener.local_addr().unwrap();
        let serving = tokio::spawn({
            let bridge = bridge.clone();
            async move { serve(bridge, listener).await }
        });

        let ok = request(
            addr,
            &format!("GET / HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\r\n"),
        )
        .await;
        assert!(ok.contains("SAW<"), "{ok}");

        // `h5i box share stop`, from another process.
        crate::run::stop(dir.path()).expect("stop");

        let after = request(
            addr,
            &format!("GET / HTTP/1.1\r\nHost: t\r\nCookie: h5i_share={secret}\r\n\r\n"),
        )
        .await;
        assert!(after.starts_with("HTTP/1.1 401 "), "{after}");
        assert!(
            !after.contains("SAW<"),
            "a stopped share still reached the box"
        );

        serving.abort();
    }

    #[test]
    fn every_built_in_page_declares_the_length_it_actually_has() {
        // Hand-counted lengths are how a body gets truncated the first time
        // somebody rewords the sentence. Both of these were written out by
        // hand once and one of them was wrong.
        for r in [busy_response(), unreachable_response()] {
            let (head, body) = r.split_once("\r\n\r\n").expect("a head and a body");
            assert!(
                head.contains(&format!("Content-Length: {}", body.len())),
                "{r}"
            );
            assert!(head.contains("Connection: close"), "{r}");
        }
        assert!(busy_response().starts_with("HTTP/1.1 503 "));
        assert!(busy_response().contains("Retry-After:"));
        assert!(unreachable_response().starts_with("HTTP/1.1 502 "));
    }

    fn which_cloudflared() -> bool {
        std::process::Command::new("cloudflared")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
    }
}
