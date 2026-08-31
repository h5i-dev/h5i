//! The one way into the box, and the only place a share touches a namespace.
//!
//! A shared port is never published. It stays inside the box's private network
//! namespace and the bridge reaches it the way the viewer forward does: h5i is
//! the parent process, so it can enter the box's user and network namespaces by
//! pid, connect from inside, and pass the connected socket back out over
//! `SCM_RIGHTS` ([`h5i_core::view::connect_in_netns`] is the same idea).
//!
//! Two things are different here, and both are deliberate.
//!
//! The fork happens once, at startup, not per connection. A share runs an
//! async runtime, and `fork()` in a process with a thread pool is a trap: the
//! child inherits one thread and any lock another thread held at fork time,
//! including the allocator's. So the dialer forks while the process is still
//! single-threaded and keeps the child alive as a small helper that answers
//! "connect me" over a socketpair for as long as the share lasts.
//!
//! The destination is pinned at spawn. The child holds `port` in its own
//! memory and reads nothing but a one-byte request. There is no field on the
//! wire, from a peer or from the box, that can redirect where this connects,
//! so the bridge cannot be turned into a general-purpose proxy into the box's
//! namespace, which is the failure mode that would matter most if a peer or a
//! shared page went looking.

use std::net::TcpStream;
#[cfg(target_os = "linux")]
use std::sync::Mutex;

use h5i_error::H5iError;

/// The child's answer to a connect request, and its startup report. Numbers
/// rather than messages because a forked child has no safe way to format one.
///
/// All of it Linux-only, like the helper it describes. Left ungated, these
/// were nine `-D warnings` errors in a build for any other platform, which is
/// how far the platform story had actually been taken.
#[cfg(target_os = "linux")]
const STATUS_OK: u8 = 0;
#[cfg(target_os = "linux")]
const STATUS_CONNECT_FAILED: u8 = 1;
/// The connect did not fail. It never finished. A dev server whose accept
/// queue is full *is* listening, and reporting that as "nothing is listening on
/// port 3000" sends somebody to start a server that is already running. Sent
/// for anything that is not a refusal: the timeout, and h5i's own resource
/// failures inside the helper (`EMFILE`, `ENETDOWN`), which are not facts about
/// the user's dev server at all.
#[cfg(target_os = "linux")]
const STATUS_CONNECT_STUCK: u8 = 2;
/// There is no route to `127.0.0.1` at all. The box's network namespace has no
/// loopback interface up. Nothing inside such a box can reach itself, so a
/// share of it can never work, and it is worth its own status because the
/// answer is not "start your dev server" but "this box cannot be shared".
#[cfg(target_os = "linux")]
const STATUS_NO_LOOPBACK: u8 = 3;
#[cfg(target_os = "linux")]
const STATUS_NO_NS: u8 = 2;
#[cfg(target_os = "linux")]
const STATUS_SETNS: u8 = 3;
/// The parent's request. One value, so there is nothing to parse.
#[cfg(target_os = "linux")]
const REQUEST: u8 = 0x01;

/// How long to wait for the box's dev server to accept.
///
/// Loopback either way (inside the box's namespace on Linux, on the host's on
/// macOS) so a working server answers in microseconds and a dead port refuses
/// immediately. This bounds the one case in between: a server that is up but
/// not accepting.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// A live route into one port of one box.
#[derive(Debug)]
pub struct Dialer {
    #[cfg(target_os = "linux")]
    inner: Inner,
    #[cfg(target_os = "macos")]
    mac: Mac,
    port: u16,
}

/// The macOS route: no helper and no namespace, because there is neither to
/// enter. What is pinned instead is the box's *process tree*, and every dial
/// re-answers "is the box still the one holding this port".
#[cfg(target_os = "macos")]
#[derive(Debug)]
struct Mac {
    /// The process whose descendants are the box. The session `h5i` started.
    /// Its children are the shell, the agent and the dev server.
    root: u32,
    /// When that process started, so the pid can be told from a later tenant of
    /// the same number. See [`crate::owner::root_unchanged`].
    root_started: u64,
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
enum Inner {
    /// A helper process living in the box's namespaces.
    Helper {
        /// Serialized: one request and one reply at a time on a shared socket.
        ///
        /// The `bool` is "this channel is still in step". A request whose reply
        /// never arrived leaves that reply, and the descriptor attached to it,
        /// queued on the socket, and the *next* caller's `recvmsg` would pick it
        /// up and be handed somebody else's connection. The mutex prevents two
        /// callers overlapping; it cannot prevent them getting out of step. So
        /// a failed exchange retires the channel instead.
        sock: Mutex<(std::os::fd::OwnedFd, bool)>,
        child: libc::pid_t,
    },
}

impl Dialer {
    /// The port inside the box that this dialer is pinned to.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The route this dialer actually holds, as an identity to compare later.
    ///
    /// Read off the *helper*, not off the box process it was pointed at, and
    /// that is the whole point. `spawn` forks a helper which enters the box's
    /// namespaces and then reports; only afterwards did the caller read
    /// `/proc/<box_pid>/ns/net` for something to compare against. If the box
    /// process exited in that gap, the helper was holding the namespace
    /// perfectly well and the read returned `None`, and `box_went_away`
    /// treated `None` as "nothing to check" and skipped the comparison for the
    /// rest of the share. Start the box again before the next writer poll and
    /// the share stayed up for its whole ticket, reporting healthy, while
    /// every dial went into the abandoned namespace and every visitor got a
    /// connection failure.
    ///
    /// The helper is alive for as long as the dialer is, so this cannot fail
    /// for that reason. `None` means there is genuinely nothing to pin (no
    /// helper (macOS, or `spawn_local`)) and the caller says so rather than
    /// disabling the check.
    pub fn pinned_route(&self) -> Option<String> {
        #[cfg(target_os = "linux")]
        {
            let Inner::Helper { child, .. } = &self.inner;
            std::fs::read_link(format!("/proc/{child}/ns/net"))
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        }
        #[cfg(target_os = "macos")]
        {
            // No namespace to hold. The identity is the process tree, and its
            // root is what every dial re-checks anyway.
            Some(format!("session {}", self.mac.root))
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            None
        }
    }

    /// Open a fresh connection to the box's port.
    ///
    /// Every peer connection gets its own: the bridge never multiplexes two
    /// peers onto one upstream socket, so one peer's keep-alive cannot carry
    /// another's request.
    /// Why a dial failed, kept apart because the receipt says different things
    /// about them and one of them blames the wrong person.
    ///
    /// `unreached N connection(s) were authorized but found nothing listening
    /// on port 3000` is a sentence about the *user's dev server*. It was
    /// printed for every dial failure, including the ones where the route into
    /// the box had broken, and the broken-channel case is sticky, so one lost
    /// reply produced a receipt confidently asserting hundreds of times that
    /// somebody's dev server was down when it was running the whole time.
    pub fn connect(&self) -> Result<TcpStream, DialError> {
        #[cfg(target_os = "linux")]
        {
            let Inner::Helper { sock, child } = &self.inner;
            self.request(sock, *child)
        }
        #[cfg(target_os = "macos")]
        {
            self.dial_attributed()
        }
        // Unreachable in practice (`spawn` refuses on this platform, so no
        // `Dialer` exists to call this on) and an answer rather than a
        // `todo!()`, because "the route into the box is broken" is exactly
        // what a platform with no namespace to enter amounts to.
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        Err(DialError::Broken(unsupported()))
    }
}

/// Why a dial failed.
#[derive(Debug)]
pub enum DialError {
    /// The box's namespace has no loopback at all. Distinct from the two below
    /// because it is not a transient condition and not the dev server's fault:
    /// the box cannot be shared, and the answer is to use a different tier.
    NoLoopback(H5iError),
    /// The route into the box worked and the port had nothing on it. A fact
    /// about the user's dev server, and the only one the receipt should report
    /// as such.
    NothingListening(H5iError),
    /// The route itself failed: the helper is gone, retired, or answering
    /// nonsense. A fact about h5i, and it used to be reported as the first.
    Broken(H5iError),
    /// Something is listening on the port and it is not this box, or the
    /// box and something else hold it in a way that decides per connection who
    /// answers. macOS only, where the box's port and the host's port are the
    /// same port and only attribution tells them apart ([`crate::owner`]).
    ///
    /// Kept apart from all three above because it is the one failure where
    /// serving the connection would be the *unsafe* outcome rather than a
    /// useless one: what is on the other end is a process the operator never
    /// offered to share. A share refuses to start on it, and a share already
    /// running refuses the connection.
    NotTheBox(H5iError),
}

impl DialError {
    pub fn into_inner(self) -> H5iError {
        match self {
            DialError::NoLoopback(e)
            | DialError::NothingListening(e)
            | DialError::Broken(e)
            | DialError::NotTheBox(e) => e,
        }
    }

    /// The box cannot be shared at all, so the caller should refuse rather than
    /// start something that will never move a byte.
    pub fn no_loopback(&self) -> bool {
        matches!(self, DialError::NoLoopback(_))
    }

    pub fn nothing_listening(&self) -> bool {
        matches!(self, DialError::NothingListening(_))
    }

    /// The port is not the box's. The caller must refuse rather than serve it.
    pub fn not_the_box(&self) -> bool {
        matches!(self, DialError::NotTheBox(_))
    }

    /// This share must not start, or must not continue: the box cannot be
    /// reached and no ticket minted for it can ever be honoured safely.
    ///
    /// The two are grouped because callers act on them identically, refuse,
    /// and grouping them here is what stops a third such condition being added
    /// later and quietly handled as a warning at one of the two call sites.
    pub fn fatal(&self) -> bool {
        self.no_loopback() || self.not_the_box()
    }
}

impl std::fmt::Display for DialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DialError::NoLoopback(e)
            | DialError::NothingListening(e)
            | DialError::Broken(e)
            | DialError::NotTheBox(e) => {
                write!(f, "{e}")
            }
        }
    }
}

impl From<DialError> for H5iError {
    fn from(d: DialError) -> H5iError {
        d.into_inner()
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn unsupported() -> H5iError {
    H5iError::Metadata(
        "sharing a box's port needs a network namespace to enter (Linux) or a way to attribute \
         a listening socket to the box's processes (macOS). `h5i box share` is not available on \
         this platform."
            .into(),
    )
}

// ─── everywhere else ────────────────────────────────────────────────────────

/// The two constructors, refusing.
///
/// `Inner::Unsupported` and a `connect` arm for it have been here since the
/// module was written, and nothing ever built the object on those platforms:
/// `run::serve` calls `Dialer::spawn`, `run.rs` is compiled everywhere, and
/// `spawn` existed only under `cfg(target_os = "linux")`. So h5i did not
/// compile at all for `aarch64-apple-darwin` or `x86_64-pc-windows-msvc`.
/// Found by CI's cross-check job the first time this branch was ever pushed
/// through it.
///
/// Refusing here rather than returning a dialer that cannot dial: the failure
/// belongs at `h5i box share`, with a sentence saying why, not at the first
/// visitor's first request. `h5i join` is unaffected and works on any platform.
/// It terminates QUIC and serves on its own loopback, and needs no namespace
/// to enter.
///
/// macOS left this arm when [`crate::owner`] gave it a route of its own. The
/// reasoning above still stands for it and is worth keeping in view: what was
/// wrong with the *old* macOS arm was not that it lacked a namespace, but that
/// it connected to `127.0.0.1:<port>` and called whatever answered "the box".
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl Dialer {
    pub fn spawn(_box_pid: u32, _port: u16) -> Result<Dialer, H5iError> {
        Err(unsupported())
    }

    pub fn spawn_local(_port: u16) -> Result<Dialer, H5iError> {
        Err(unsupported())
    }
}

// ─── macOS ──────────────────────────────────────────────────────────────────

/// The macOS route into a box.
///
/// There is no namespace here and no helper process: a Seatbelt box's dev
/// server listens on the *host's* loopback, so the connect is an ordinary
/// one and needs nothing entered. What is not ordinary is deciding *what to
/// connect to*, and that is the whole of this arm.
///
/// An earlier version of this file connected to `127.0.0.1:<port>` and treated
/// whatever answered as the box. It was deleted for the right reason: the box's
/// port and the host's port are the same port, so that code published whatever
/// happened to be listening. The one outcome the Linux side refuses by
/// construction. [`crate::owner`] is what makes the question answerable, and
/// every dial asks it again.
/// How many times [`Dialer::resolve`] re-runs a whole attribution pass that
/// ended on a listener pid which had already exited.
///
/// Small on purpose. The condition it covers is a reaped pid, and a reaped pid
/// does not come back: the process is gone from the table and gone from the
/// socket scan, so the next pass sees the surviving listener instead. More than
/// one attempt is only needed because a box mid-build produces these
/// continuously, and a pass can lose the race twice. A dial that loses it four
/// times in a row is refused rather than retried forever, which keeps a wedged
/// process table a refusal with a sentence rather than a hang.
#[cfg(target_os = "macos")]
const ATTRIBUTION_ATTEMPTS: usize = 4;

/// What one attribution pass concluded, and whether asking again could change
/// it.
///
/// The distinction exists because `Err` alone cannot carry it: "this port is a
/// stranger's" and "the pid h5i was about to name exited underneath it" are
/// both refusals to the caller, and only the second is worth re-running. Kept
/// private, so nothing outside this module can act on the difference. The
/// retry is an implementation detail of `resolve`, not a second class of
/// failure for callers to handle.
#[cfg(target_os = "macos")]
enum Pass {
    /// The pass reached an answer about the port. Asking again would reach the
    /// same one.
    Settled(Result<std::net::SocketAddr, DialError>),
    /// The pass named a listener that no longer exists, so it never got to an
    /// answer about the port. The error is what to report if every attempt ends
    /// this way.
    Vanished(DialError),
}

#[cfg(target_os = "macos")]
impl Dialer {
    /// Pin a dialer to one port of one box.
    ///
    /// `box_pid` is the box's *session* process. The `h5i` that is running
    /// the shell or the command, whose descendants are the box. This differs
    /// from the Linux meaning of the same argument (a pid *inside* the box's
    /// namespaces) because the two platforms identify a box differently: there
    /// by the namespace a process is in, here by the tree a process is under.
    ///
    /// Resolved once here so `h5i box share` can refuse with a sentence rather
    /// than print a ticket that cannot work. The same reason the Linux arm
    /// waits for its helper to report before returning.
    ///
    /// A port with *nothing* on it is not a refusal, and that asymmetry is
    /// deliberate: an agent about to start its dev server is a perfectly good
    /// reason to share a port that is not up yet, and `run::serve` turns it
    /// into a warning. A port held by a *stranger* is a refusal, because
    /// there is nothing to wait for. It is already the wrong process.
    pub fn spawn(box_pid: u32, port: u16) -> Result<Dialer, H5iError> {
        // Pinned with its start time, not as a bare number: everything below
        // treats the root pid as the box itself, so a pid that changes hands
        // hands the box over with it. See [`crate::owner::root_unchanged`].
        let Some(root_started) = h5i_core::env::proc_start_ticks(box_pid) else {
            return Err(H5iError::Metadata(format!(
                "the box's session process (pid {box_pid}) is gone — h5i could not read when it \
                 started, so it cannot tell that pid from whatever the kernel gives the number \
                 to next. Start a session and share again."
            )));
        };
        let d = Dialer {
            mac: Mac {
                root: box_pid,
                root_started,
            },
            port,
        };
        match d.resolve() {
            Ok(_) => Ok(d),
            Err(e) if e.nothing_listening() => Ok(d),
            Err(e) => Err(e.into_inner()),
        }
    }

    /// For a box that is this very process tree. Tests only, and the macOS
    /// counterpart of the Linux "no namespace to enter" constructor: there, a
    /// helper that skips `setns`; here, a box whose processes are ours.
    ///
    /// It exercises the real attribution path (a socket this process binds is
    /// found, attributed and dialled exactly as a box's would be) which is
    /// what makes it worth having rather than a stub.
    pub fn spawn_local(port: u16) -> Result<Dialer, H5iError> {
        Dialer::spawn(std::process::id(), port)
    }

    /// Who holds the port, right now.
    ///
    /// Re-run per dial rather than cached, and that is affordable rather than
    /// merely careful: measured on the machine this was written on, the two
    /// scans behind it cost about 1.4 ms together (0.84 ms to walk every
    /// process's sockets, 0.58 ms to walk the process tree). Caching it would
    /// buy a millisecond and cost the property that matters. That a box whose
    /// dev server has died cannot have its share quietly inherited by the next
    /// process to claim the port.
    fn resolve(&self) -> Result<std::net::SocketAddr, DialError> {
        let mut vanished = None;
        for _ in 0..ATTRIBUTION_ATTEMPTS {
            match self.resolve_once() {
                Pass::Settled(r) => return r,
                Pass::Vanished(e) => vanished = Some(e),
            }
        }
        // Every pass landed on a listener that had exited by the time it was
        // asked about. Reported as the refusal it always was rather than
        // guessed past: a port whose holder cannot be held still long enough to
        // be identified is not a port h5i can promise leads to the box.
        Err(vanished.expect("the loop above runs at least once"))
    }

    /// One attribution pass, whole: the process table, every readable process's
    /// descriptors, the decision, and the ancestry re-ask.
    fn resolve_once(&self) -> Pass {
        use crate::owner::{self, Ownership};

        // Before anything is attributed. `is_descendant` answers true for the
        // root itself and `process_tree` seeds its set with it, so a root pid
        // that changed hands makes its new tenant the box entire, and every
        // check below would then be measuring the wrong tree.
        if !owner::root_unchanged(
            self.mac.root_started,
            h5i_core::env::proc_start_ticks(self.mac.root),
        ) {
            return Pass::Settled(Err(DialError::NotTheBox(H5iError::Metadata(format!(
                "the box's session process (pid {}) is gone, and that pid now belongs to \
                 something else — so h5i cannot tell which processes are this box's and will \
                 not connect to any of them. Start a fresh session and a fresh share.",
                self.mac.root
            )))));
        }

        let pids = owner::process_tree(self.mac.root);
        // Only sockets whose owner still exists. A pid that has exited holds
        // nothing, so it cannot take a connection, but its ancestry can no
        // longer be resolved either, which makes it read as a *stranger* on the
        // box's own address, which is a refusal. A busy box's short-lived
        // children are exactly that shape. See [`owner::is_alive`].
        let listeners: Vec<owner::Listener> = owner::listening_sockets()
            .into_iter()
            .filter(|l| owner::is_alive(l.pid))
            .collect();
        let named = |pid: u32| match owner::process_name(pid) {
            Some(n) => format!("`{n}` (pid {pid})"),
            None => format!("pid {pid}"),
        };
        // The snapshot first, because it is free and answers for almost every
        // pid; the live ancestry walk only for one the snapshot has not heard
        // of. That second half is the fix: a box spawns processes constantly,
        // a child inherits the dev server's listening socket across `fork`, and
        // one caught in the moment before `exec` is a real co-holder of the
        // real address. Judged against the snapshot alone it is a *stranger* on
        // the box's own port, which is `Contested`, so a busy box refused its
        // own visitors, intermittently, in a way that looks like the network.
        let is_box = |pid: u32| pids.contains(&pid) || owner::is_descendant(pid, self.mac.root);
        match owner::decide(&listeners, self.port, is_box) {
            // Re-asked upwards, against the process table as it is now. The
            // tree above is a snapshot taken before the sockets were scanned,
            // and a pid that changed hands in between would otherwise carry the
            // snapshot's word for it. See [`owner::is_descendant`].
            Ownership::Box { addr, pid } if owner::is_descendant(pid, self.mac.root) => {
                Pass::Settled(Ok(addr))
            }
            // The re-ask can fail for two reasons that look identical here and
            // are not the same fact, and reading both as a refusal is what made
            // a busy box refuse its own visitors.
            //
            // A pid that is *still alive* and no longer under the root did
            // change hands: the number was reused while h5i was looking at it,
            // and what holds the port now is a process the operator never
            // offered to share. Refused, and not retried. Asking again would
            // only get the same true answer more slowly.
            //
            // A pid that is *gone* held nothing at all. It is the same shape
            // `is_alive` already filters out of the listener list one step
            // earlier, arriving one step later: a box child that inherited the
            // dev server's listening descriptor across `fork` and exited before
            // the re-ask reached it. The address it was found on is still the
            // box's, still held by the process that bound it, so the pass is
            // re-run rather than turned into a sentence about pids, which is
            // what a visitor of a box mid-build would otherwise be shown.
            Ownership::Box { pid, .. } if !owner::is_alive(pid) => {
                Pass::Vanished(DialError::NotTheBox(H5iError::Metadata(format!(
                    "the process found holding port {} (pid {pid}) exited before h5i could \
                     confirm it was this box's, on every attempt. Nothing was published. Try \
                     the connection again.",
                    self.port
                ))))
            }
            Ownership::Box { pid, .. } => {
                Pass::Settled(Err(DialError::NotTheBox(H5iError::Metadata(format!(
                    "the process holding port {} (pid {pid}) stopped being part of this box \
                     while h5i was looking at it, so h5i will not connect to it. This is the \
                     pid changing hands underneath the check; try again.",
                    self.port
                )))))
            }
            Ownership::Nobody => Pass::Settled(Err(DialError::NothingListening(
                H5iError::Metadata(format!(
                    "nothing is listening on port {} in the box. Start the dev server in the \
                     box, or share the port it is actually on.",
                    self.port
                )),
            ))),
            Ownership::Stranger { pid, addr } => {
                Pass::Settled(Err(DialError::NotTheBox(H5iError::Metadata(format!(
                    "port {} on this machine is held by {}, which is not part of this box — so \
                     h5i will not share it.\n   On macOS a box has no network of its own: it \
                     binds the host's loopback, and `{}` is what a connection to port {} \
                     reaches. Sharing it would publish a process you did not choose.\n   Stop \
                     whatever is using port {}, or share the port the box's server is actually \
                     on.",
                    self.port,
                    named(pid),
                    addr,
                    self.port,
                    self.port
                )))))
            }
            Ownership::Contested { addr, others } => {
                let who = others
                    .iter()
                    .map(|p| named(*p))
                    .collect::<Vec<_>>()
                    .join(", ");
                Pass::Settled(Err(DialError::NotTheBox(H5iError::Metadata(format!(
                    "port {} is held on {} by this box *and* by {} at the same time, so the \
                     kernel decides per connection which one answers.\n   h5i will not share a \
                     port it cannot promise leads to the box. Stop the other listener and start \
                     the share again.",
                    self.port, addr, who
                )))))
            }
        }
    }

    /// Resolve, then connect to exactly what was resolved.
    fn dial_attributed(&self) -> Result<TcpStream, DialError> {
        let addr = self.resolve()?;
        TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).map_err(|e| {
            // Classified the way the Linux helper classifies its own connect,
            // so the receipt and the warnings read the same on both platforms.
            // A refusal here is a race rather than the ordinary case (the
            // listener was there a moment ago, when `resolve` saw it) but it
            // is still a fact about the dev server, not about h5i.
            match e.kind() {
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::AddrNotAvailable => {
                    DialError::NothingListening(H5iError::Metadata(format!(
                        "nothing is listening on {addr} in the box any more. Start the dev \
                         server in the box, or share the port it is actually on."
                    )))
                }
                _ => DialError::Broken(H5iError::Metadata(format!(
                    "could not open a connection to {addr} in the box: {e}. Something is \
                     listening but not accepting, or this machine is out of sockets. Not the \
                     same as nothing being there."
                ))),
            }
        })
    }
}

// ─── Linux ──────────────────────────────────────────────────────────────────

/// Linux only, and unlike the viewer forward this module needs no arch gate:
/// it does its own `SCM_RIGHTS` rather than borrowing the seccomp supervisor's
/// helper, so there is no narrower dependency to match.
#[cfg(target_os = "linux")]
impl Dialer {
    /// Fork the helper into the box's namespaces.
    ///
    /// Call this before starting an async runtime. The whole reason the
    /// helper is long-lived is that forking a multi-threaded process is unsafe,
    /// and that reasoning is defeated if the fork happens after the runtime has
    /// spawned its workers.
    pub fn spawn(box_pid: u32, port: u16) -> Result<Dialer, H5iError> {
        Dialer::spawn_inner(Some(box_pid), port)
    }

    /// For a box with no network namespace of its own. The `workspace` tier,
    /// where nothing is unshared. The port is already on this machine's
    /// loopback.
    ///
    /// Still forks a helper rather than connecting inline, and that is worth a
    /// sentence: one code path means one thing to get right, and the caller
    /// gets the same object with the same pinned destination either way. It
    /// also means the fd handoff is exercised by every share rather than only
    /// by the ones that enter a namespace.
    ///
    /// The difference worth stating rather than hiding: with a namespace, this
    /// dialer is the only route to the shared port. Without one, the port is on
    /// shared loopback and any local process can reach it directly. The grant
    /// table governs *this* path; it cannot govern the port itself.
    pub fn spawn_local(port: u16) -> Result<Dialer, H5iError> {
        Dialer::spawn_inner(None, port)
    }

    fn spawn_inner(box_pid: Option<u32>, port: u16) -> Result<Dialer, H5iError> {
        use std::os::fd::{FromRawFd, OwnedFd};

        let mut sv = [0i32; 2];
        let rc = unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
                0,
                sv.as_mut_ptr(),
            )
        };
        if rc != 0 {
            return Err(H5iError::Io(std::io::Error::last_os_error()));
        }
        let (parent_end, child_end) = (sv[0], sv[1]);

        let child = unsafe { libc::fork() };
        if child < 0 {
            unsafe {
                libc::close(parent_end);
                libc::close(child_end);
            }
            return Err(H5iError::Io(std::io::Error::last_os_error()));
        }
        if child == 0 {
            unsafe { libc::close(parent_end) };
            let code = helper_main(box_pid, port, child_end);
            unsafe { libc::_exit(code) };
        }
        unsafe { libc::close(child_end) };
        let sock = unsafe { OwnedFd::from_raw_fd(parent_end) };

        // Wait for the helper to report that it is in the namespaces. Doing it
        // here rather than on the first connection is what lets `h5i box share`
        // fail with an explanation instead of printing a ticket that cannot
        // work.
        let (status, fd) = recv_status(parent_end);
        if let Some(fd) = fd {
            unsafe { libc::close(fd) };
        }
        if status != Some(STATUS_OK) {
            let mut wait_status = 0;
            unsafe { libc::waitpid(child, &mut wait_status, 0) };
            let pid = box_pid.unwrap_or(0);
            return Err(match status {
                Some(STATUS_NO_NS) => H5iError::Metadata(format!(
                    "the box (pid {pid}) is gone — its namespaces no longer exist. A box only \
                     has them while a session is running; start one with `h5i box shell <name>`."
                )),
                Some(STATUS_SETNS) => H5iError::Metadata(format!(
                    "could not enter the box's network namespace (pid {pid}). Joining it needs \
                     the user namespace that created it, so run `h5i box share` as the user that \
                     started the box's session."
                )),
                _ => H5iError::Metadata(format!(
                    "could not reach into the box (pid {pid}) to share port {port}"
                )),
            });
        }
        Ok(Dialer {
            inner: Inner::Helper {
                sock: Mutex::new((sock, true)),
                child,
            },
            port,
        })
    }

    fn request(
        &self,
        sock: &Mutex<(std::os::fd::OwnedFd, bool)>,
        child: libc::pid_t,
    ) -> Result<TcpStream, DialError> {
        use std::os::fd::{AsRawFd, FromRawFd};

        // One request in flight at a time. The reply carries an fd in its
        // ancillary data, and two overlapping `recvmsg` calls on one socket
        // would hand a caller the other's socket.
        let mut guard = sock
            .lock()
            // A poisoned lock means a previous caller panicked mid-exchange, so
            // the channel is in an unknown state for the same reason a short
            // read leaves it in one. Take the guard and retire it, rather than
            // refusing forever with a message nobody can act on.
            .unwrap_or_else(|p| p.into_inner());
        let (ref owned, ref mut in_step) = *guard;
        if !*in_step {
            return Err(DialError::Broken(H5iError::Metadata(format!(
                "the box dialer (pid {child}) lost track of a reply and was retired, so this \
                 share can no longer reach the box. Restart the share."
            ))));
        }
        let fd = owned.as_raw_fd();
        let req = [REQUEST];
        if unsafe {
            libc::send(
                fd,
                req.as_ptr() as *const libc::c_void,
                1,
                libc::MSG_NOSIGNAL,
            )
        } != 1
        {
            *in_step = false;
            return Err(DialError::Broken(H5iError::Metadata(format!(
                "the box dialer (pid {child}) is gone; the share cannot reach the box any more"
            ))));
        }
        let (status, got) = recv_status(fd);
        // A descriptor arriving with anything but a clean OK is one nobody is
        // going to use. Closing it here is the difference between a failed
        // connection and a leaked one.
        let close_stray = |got: Option<i32>| {
            if let Some(raw) = got {
                unsafe { libc::close(raw) };
            }
        };
        match (status, got) {
            (Some(STATUS_OK), Some(raw)) => Ok(unsafe { TcpStream::from_raw_fd(raw) }),
            (Some(STATUS_CONNECT_FAILED), got) => {
                close_stray(got);
                Err(DialError::NothingListening(H5iError::Metadata(format!(
                    "nothing is listening on 127.0.0.1:{} inside the box. Start the dev server \
                     in the box, or share the port it is actually on.",
                    self.port
                ))))
            }
            (Some(STATUS_NO_LOOPBACK), got) => {
                close_stray(got);
                Err(DialError::NoLoopback(H5iError::Metadata(format!(
                    "this box's network namespace has no loopback interface, so nothing inside \
                     it can reach 127.0.0.1:{} — not even the box itself. A share of it cannot \
                     work.",
                    self.port
                ))))
            }
            (Some(STATUS_CONNECT_STUCK), got) => {
                close_stray(got);
                Err(DialError::Broken(H5iError::Metadata(format!(
                    "could not open a connection to 127.0.0.1:{} inside the box — something is \
                     listening but not accepting, or this machine is out of sockets. Not the \
                     same as nothing being there.",
                    self.port
                ))))
            }
            (Some(_), got) => {
                close_stray(got);
                Err(DialError::Broken(H5iError::Metadata(format!(
                    "the box dialer (pid {child}) answered something unexpected; the share \
                     cannot reach the box any more"
                ))))
            }
            // No reply at all. The helper may yet send one, and it would land in
            // the *next* caller's `recvmsg`, so the channel is out of step and
            // must not serve anyone again.
            (None, got) => {
                close_stray(got);
                *in_step = false;
                Err(DialError::Broken(H5iError::Metadata(format!(
                    "the box dialer (pid {child}) stopped answering; the share cannot reach the \
                     box any more"
                ))))
            }
        }
    }
}

/// `/proc/<pid>/ns/<kind>`, NUL-terminated, built on the stack.
///
/// Nothing in the child may allocate, see [`helper_main`], and `format!`
/// does. Sixty-four bytes is well past the longest this can be (a pid is at
/// most ten digits).
#[cfg(target_os = "linux")]
fn ns_path(buf: &mut [u8; 64], pid: u32, kind: &[u8]) -> *const libc::c_char {
    let mut n = 0;
    let mut put = |bytes: &[u8]| {
        for &b in bytes {
            if n < buf.len() - 1 {
                buf[n] = b;
                n += 1;
            }
        }
    };
    put(b"/proc/");
    let mut digits = [0u8; 10];
    let mut d = digits.len();
    let mut v = pid;
    loop {
        d -= 1;
        digits[d] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    put(&digits[d..]);
    put(b"/ns/");
    put(kind);
    buf[n] = 0;
    buf.as_ptr() as *const libc::c_char
}

/// The helper. Enters the namespaces once, then answers connect requests until
/// the parent closes its end of the socketpair.
///
/// Everything below the fork is allocation-free, and that is a requirement
/// rather than a style. `Dialer::spawn` is documented as being called before a
/// runtime starts, but "documented" is not "enforced". A caller that gets it
/// wrong forks a multi-threaded process, and the child then inherits whatever
/// locks the other threads held, the allocator's among them. A child that never
/// allocates cannot deadlock on that lock no matter who called it wrong. So the
/// path is built on the stack and the connect takes a `SocketAddr` rather than
/// a `(&str, u16)`, whose `ToSocketAddrs` allocates.
#[cfg(target_os = "linux")]
fn helper_main(box_pid: Option<u32>, port: u16, sock: i32) -> i32 {
    // Order matters: the netns is owned by the box's user namespace, so we have
    // to be in that userns before the kernel will let us join the netns.
    if let Some(box_pid) = box_pid {
        for (ns, kind) in [
            (&b"user"[..], libc::CLONE_NEWUSER),
            (&b"net"[..], libc::CLONE_NEWNET),
        ] {
            let mut buf = [0u8; 64];
            let path = ns_path(&mut buf, box_pid, ns);
            let fd = unsafe { libc::open(path, libc::O_RDONLY) };
            if fd < 0 {
                send_status(sock, STATUS_NO_NS, None);
                return 2;
            }
            let rc = unsafe { libc::setns(fd, kind) };
            unsafe { libc::close(fd) };
            // A box at the `process` tier has no user namespace of its own to
            // join, so only the netns attempt is fatal.
            if rc != 0 && kind == libc::CLONE_NEWNET {
                send_status(sock, STATUS_SETNS, None);
                return 3;
            }
        }
    }
    send_status(sock, STATUS_OK, None);

    let dest = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    loop {
        let mut req = [0u8; 1];
        let n = unsafe { libc::recv(sock, req.as_mut_ptr() as *mut libc::c_void, 1, 0) };
        // Zero is the parent closing its end, which is how a share shuts the
        // helper down. Anything else is a broken socket, and the answer is the
        // same: stop.
        if n != 1 {
            return 0;
        }
        // Deadlined, and this is not a nicety. The parent holds a mutex across
        // this exchange and waits for the reply, so an unbounded connect
        // serialises every other connection of the share behind it, and the
        // shutdown path waits for the helper, so it would hang there too. A dev
        // server whose accept queue is full makes the kernel drop the SYN, and
        // the default retry schedule is around two minutes. A box runs
        // agent-written code; a wedged single-threaded dev server is ordinary.
        match TcpStream::connect_timeout(&dest, CONNECT_TIMEOUT) {
            Ok(stream) => {
                use std::os::fd::AsRawFd;
                send_status(sock, STATUS_OK, Some(stream.as_raw_fd()));
            }
            // Classified here, because here is where the `io::Error` exists.
            // Doing it a layer up meant `route_broken` only ever counted
            // channel-protocol failures and a wedged dev server was still
            // filed as an absent one.
            Err(e) => {
                let status = match e.kind() {
                    std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::AddrNotAvailable => STATUS_CONNECT_FAILED,
                    // `ENETUNREACH`. A namespace with no `lo` up gives this for
                    // every address including its own loopback.
                    std::io::ErrorKind::NetworkUnreachable => STATUS_NO_LOOPBACK,
                    _ => STATUS_CONNECT_STUCK,
                };
                send_status(sock, status, None);
            }
        }
    }
}

/// One status byte, optionally with one fd attached.
///
/// Written out here rather than reused from the seccomp supervisor because that
/// helper carries a *dummy* payload byte: it can say "here is an fd" but not
/// "here is why there isn't one", and a share that could only ever report
/// "short read" is exactly the class of unhelpful failure this codebase keeps
/// paying for.
#[cfg(target_os = "linux")]
fn send_status(sock: i32, status: u8, fd: Option<i32>) {
    unsafe {
        let mut payload = [status];
        let mut iov = libc::iovec {
            iov_base: payload.as_mut_ptr() as *mut libc::c_void,
            iov_len: 1,
        };
        let mut cmsg_buf = [0u8; 64];
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        if let Some(fd) = fd {
            msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
            msg.msg_controllen = libc::CMSG_SPACE(std::mem::size_of::<i32>() as u32) as _;
            let cmsg = libc::CMSG_FIRSTHDR(&msg);
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<i32>() as u32) as _;
            std::ptr::copy_nonoverlapping(&fd, libc::CMSG_DATA(cmsg) as *mut i32, 1);
        }
        libc::sendmsg(sock, &msg, libc::MSG_NOSIGNAL);
    }
}

/// The other half. Returns the status byte and the fd, when one came with it.
///
/// A truncated control message is refused rather than read: a partial ancillary
/// buffer must never be mistaken for a valid descriptor.
#[cfg(target_os = "linux")]
fn recv_status(raw: i32) -> (Option<u8>, Option<i32>) {
    unsafe {
        let mut payload = [0u8; 1];
        let mut iov = libc::iovec {
            iov_base: payload.as_mut_ptr() as *mut libc::c_void,
            iov_len: 1,
        };
        let mut cmsg_buf = [0u8; 64];
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = cmsg_buf.len() as _;

        // Retried on `EINTR`, and only on `EINTR`. Anything else that is not
        // exactly one byte retires the channel for the life of the share.
        // Every later connection answers "the box dialer lost track of a reply"
        // and the share serves that error until somebody restarts it. A signal
        // arriving mid-`recvmsg` is not a reason to be in that state. tokio
        // installs its handlers with `SA_RESTART`, so this should not fire; the
        // cost of being wrong about that is the whole share.
        let mut n = libc::recvmsg(raw, &mut msg, 0);
        while n < 0 && *libc::__errno_location() == libc::EINTR {
            n = libc::recvmsg(raw, &mut msg, 0);
        }
        if n != 1 {
            return (None, None);
        }
        if msg.msg_flags & (libc::MSG_CTRUNC | libc::MSG_TRUNC) != 0 {
            return (Some(payload[0]), None);
        }
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        if cmsg.is_null()
            || (*cmsg).cmsg_type != libc::SCM_RIGHTS
            || (*cmsg).cmsg_level != libc::SOL_SOCKET
            || (*cmsg).cmsg_len < libc::CMSG_LEN(std::mem::size_of::<i32>() as u32) as _
        {
            return (Some(payload[0]), None);
        }
        let mut fd: i32 = -1;
        std::ptr::copy_nonoverlapping(libc::CMSG_DATA(cmsg) as *const i32, &mut fd, 1);
        if fd < 0 {
            return (Some(payload[0]), None);
        }
        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        (Some(payload[0]), Some(fd))
    }
}

#[cfg(target_os = "linux")]
impl Drop for Dialer {
    fn drop(&mut self) {
        let Inner::Helper { sock, child } = &self.inner;
        // Closing our end is the shutdown signal: the helper's `recv` returns
        // zero and it exits. Then reap it, so a long-lived `h5i` that shares
        // several boxes in turn does not accumulate zombies.
        //
        // The shutdown must happen even if the mutex is poisoned. The `waitpid`
        // below is unconditional, and the descriptor that would tell the helper
        // to stop lives *inside* the mutex, so skipping the shutdown on a
        // poisoned lock means waiting forever for a process that was never told
        // to exit.
        {
            use std::os::fd::AsRawFd;
            let guard = sock.lock().unwrap_or_else(|p| p.into_inner());
            unsafe { libc::shutdown(guard.0.as_raw_fd(), libc::SHUT_RDWR) };
        }
        // Polled rather than blocking. The helper only reads the socket
        // *between* connects, so if it is inside one it cannot notice the
        // shutdown until that connect finishes, and this runs on the way out
        // of `h5i box share`, where a hang is the operator's whole experience
        // of the command. `CONNECT_TIMEOUT` bounds how long that can be, but a
        // shutdown that waits ten seconds is still a shutdown nobody wants, so
        // this gives up and leaves the child to `init`.
        let mut status = 0;
        for _ in 0..100 {
            let rc = unsafe { libc::waitpid(*child, &mut status, libc::WNOHANG) };
            if rc != 0 {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}

/// The macOS route, driven end to end through the real `Dialer`.
///
/// [`crate::owner`] tests the decision rule against a table of listeners; these
/// test that the dialer built on it actually reaches the right socket and
/// refuses the wrong one. The difference between a correct rule and a correct
/// feature.
///
/// The trick that makes the refusal testable in one process: root the dialer at
/// a *child* process rather than at ourselves. The box is then a tree that
/// this test is not in, so a socket the test binds is, by construction, a
/// stranger's, which is exactly the shape that must be refused.
#[cfg(all(test, target_os = "macos"))]
mod mac_tests {
    use super::*;
    use std::io::{Read, Write};

    /// A child that does nothing but exist, so its pid can root a tree.
    fn a_child() -> std::process::Child {
        std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn a child to root the box's tree at")
    }

    #[test]
    fn the_dialer_reaches_a_listener_in_the_boxs_tree() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = server.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut s, _) = server.accept().expect("accept");
            let mut buf = [0u8; 5];
            s.read_exact(&mut buf).expect("read");
            s.write_all(b"pong").expect("write");
        });

        let dialer = Dialer::spawn_local(port).expect("spawn a dialer for our own tree");
        let mut sock = dialer.connect().expect("connect through the dialer");
        sock.write_all(b"hello").expect("write");
        let mut back = [0u8; 4];
        sock.read_exact(&mut back).expect("read");
        assert_eq!(&back, b"pong");
        t.join().expect("server thread");
    }

    /// A dev server bound `[::]` with `IPV6_V6ONLY` on, and the dialer has to
    /// reach it.
    ///
    /// The end of the chain the `owner` tests start: libproc reports this
    /// socket and a dual-stack one identically, so the only safe address to
    /// dial is `[::1]`, and this is what proves the dialer actually gets a
    /// connection rather than `ECONNREFUSED` from `127.0.0.1`. Written with a
    /// raw socket because Rust's `TcpListener` gives no way to ask for
    /// `IPV6_V6ONLY`, and it is exactly that option under test.
    #[test]
    fn a_v6only_server_is_reached_rather_than_reported_missing() {
        use std::os::fd::{FromRawFd, OwnedFd};

        // Attempted a few times, and that is about this *binary*, not about the
        // code under test. Every test here runs in one process and
        // `spawn_local` roots the box at that process, so every socket any
        // other test holds is, correctly, one of the box's. The v4 and v6
        // ephemeral ranges are allocated independently, so another test can be
        // handed the same *number* in the v4 space at any moment, including
        // between this test's check and its dial. `decide` then legitimately
        // prefers that v4 socket, which belongs to a test that may close it a
        // millisecond later, and the dial fails for a reason that has nothing
        // to do with v6-only servers. The kernel picks a different port next
        // attempt.
        for attempt in 0..8 {
            let (fd, port) = unsafe {
                let s = libc::socket(libc::AF_INET6, libc::SOCK_STREAM, 0);
                assert!(s >= 0, "socket");
                let on: libc::c_int = 1;
                assert_eq!(
                    libc::setsockopt(
                        s,
                        libc::IPPROTO_IPV6,
                        libc::IPV6_V6ONLY,
                        &on as *const _ as *const libc::c_void,
                        std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                    ),
                    0,
                    "IPV6_V6ONLY"
                );
                let mut a: libc::sockaddr_in6 = std::mem::zeroed();
                a.sin6_family = libc::AF_INET6 as u8;
                a.sin6_len = std::mem::size_of::<libc::sockaddr_in6>() as u8;
                assert_eq!(
                    libc::bind(
                        s,
                        &a as *const _ as *const libc::sockaddr,
                        std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
                    ),
                    0,
                    "bind [::]"
                );
                assert_eq!(libc::listen(s, 8), 0, "listen");
                let mut got: libc::sockaddr_in6 = std::mem::zeroed();
                let mut len = std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t;
                libc::getsockname(s, &mut got as *mut _ as *mut libc::sockaddr, &mut len);
                (OwnedFd::from_raw_fd(s), u16::from_be(got.sin6_port))
            };

            // Someone else already holds the v4 twin of this number.
            if crate::owner::listening_sockets()
                .into_iter()
                .any(|l| l.addr.port() == port && l.addr.is_ipv4())
            {
                continue;
            }
            // The premise: this socket really does refuse IPv4. If it answers,
            // the number was taken between the check above and here.
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                continue;
            }

            // Retryable for the same reason the two checks above are: the
            // number can be held in the v4 space by something outside this
            // process entirely, a box or a server left behind by an
            // end-to-end run, which makes this a real `Stranger` or
            // `Contested`. Correct behaviour, and nothing to do with what this
            // test is asking.
            let Ok(dialer) = Dialer::spawn_local(port) else {
                drop(fd);
                continue;
            };
            match dialer.connect() {
                Ok(sock) => {
                    // The whole point: reached, and reached over v6, because
                    // `127.0.0.1` is not an address this server answers on.
                    assert!(
                        sock.peer_addr().expect("peer").is_ipv6(),
                        "the dialer reached a v6-only server over something other than v6"
                    );
                    drop(fd);
                    return;
                }
                // A v4 twin appeared after both checks above and vanished
                // again; nothing to conclude, so take a fresh port.
                Err(e) if attempt < 7 => {
                    drop(fd);
                    let _ = e;
                    continue;
                }
                Err(e) => panic!(
                    "the dialer must reach a v6-only server rather than report it missing: {e}"
                ),
            }
        }
        panic!("never got an uncontended port in eight attempts, which is itself suspicious");
    }

    /// Many visitors at once, which is the only way this route is ever used.
    ///
    /// Every dial re-runs the whole attribution (the process table, every
    /// readable process's descriptors, the decision, and the ancestry check)
    /// so a share serving a page with a dozen subresources runs a dozen of them
    /// concurrently. Nothing here is shared mutable state, which is the point
    /// worth holding onto by test rather than by inspection: the answer must be
    /// the same socket every time, and no thread may panic on a process table
    /// that is changing underneath it.
    #[test]
    fn concurrent_dials_all_reach_the_same_box_port() {
        use std::sync::Arc;

        let server = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = server.local_addr().expect("addr").port();
        let accepting = std::thread::spawn(move || {
            // One accept per dial below, then done.
            for _ in 0..24 {
                if server.accept().is_err() {
                    break;
                }
            }
        });

        let dialer = Arc::new(Dialer::spawn_local(port).expect("dialer"));
        // Churn the process table while the dials run, so the scans are reading
        // something that is genuinely moving rather than a still picture.
        let churn = std::thread::spawn(|| {
            for _ in 0..12 {
                if let Ok(mut c) = std::process::Command::new("/usr/bin/true").spawn() {
                    let _ = c.wait();
                }
            }
        });

        let mut threads = Vec::new();
        for _ in 0..24 {
            let d = Arc::clone(&dialer);
            threads.push(std::thread::spawn(move || {
                d.connect().map(|s| s.peer_addr().expect("peer").port())
            }));
        }
        for (i, t) in threads.into_iter().enumerate() {
            match t.join().expect("a dial thread panicked") {
                Ok(got) => assert_eq!(got, port, "dial {i} reached the wrong port"),
                Err(e) => panic!("dial {i} failed while the box was listening: {e}"),
            }
        }
        churn.join().expect("churn thread");
        accepting.join().expect("server thread");
    }

    #[test]
    fn a_dead_port_is_reported_as_a_dead_port() {
        // Port 1, for the reason the Linux test gives: an ephemeral port that
        // was just released gets handed straight back out to another test in
        // this binary. Nothing binds port 1 without privileges we do not have.
        let dialer = Dialer::spawn_local(1).expect("nothing listening is not a refusal");
        let err = dialer.connect().expect_err("nothing is listening");
        assert!(
            err.nothing_listening(),
            "a port with nothing on it is the dev server's business, not a refusal: {err}"
        );
    }

    #[test]
    fn a_port_held_by_a_stranger_is_refused_at_spawn() {
        // The whole reason this platform's route exists, as a test. The socket
        // is bound by *this* process, and the box is a child's tree, so from
        // the dialer's point of view it belongs to somebody else entirely.
        let mut child = a_child();
        let stranger = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = stranger.local_addr().unwrap().port();

        let err = Dialer::spawn(child.id(), port)
            .expect_err("a port held by a stranger must not be shared");
        let msg = format!("{err}");
        assert!(
            msg.contains("not part of this box"),
            "the refusal must say whose port it is: {msg}"
        );
        // And it must name what holds it, or the operator has nowhere to look.
        assert!(
            msg.contains(&format!("{}", std::process::id())) || msg.contains('`'),
            "the refusal must identify the holder: {msg}"
        );

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn a_stranger_that_appears_later_is_refused_at_the_next_dial() {
        // The property that per-dial re-resolution buys, and the reason it is
        // not cached: a dialer that spawned cleanly must not keep serving once
        // the port has changed hands. Here it changes hands the other way,
        // the dialer starts with nothing listening and a stranger arrives,
        // which is the same transition a box's dev server exiting produces.
        let mut child = a_child();
        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = probe.local_addr().unwrap().port();
        drop(probe);

        let dialer = Dialer::spawn(child.id(), port).expect("nothing listening yet");
        let stranger = std::net::TcpListener::bind(("127.0.0.1", port));
        // The kernel can hand this port to somebody else between the drop and
        // the re-bind; if it did, there is nothing to assert about.
        if stranger.is_ok() {
            let err = dialer.connect().expect_err("a stranger now holds the port");
            assert!(
                err.not_the_box(),
                "a port that changed hands must be refused, not served: {err}"
            );
            assert!(err.fatal(), "and it must be one of the refusals");
        }

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn the_destination_cannot_be_redirected_after_spawn() {
        // The same property the Linux arm states: the port is fixed at spawn
        // and nothing a peer sends can move it. Here it is pinned in the
        // `Dialer` itself and re-resolved only against that number.
        let a = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let b = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let (pa, pb) = (
            a.local_addr().unwrap().port(),
            b.local_addr().unwrap().port(),
        );
        let dialer = Dialer::spawn_local(pa).expect("dialer");
        assert_eq!(dialer.port(), pa);
        let sock = dialer.connect().expect("connect");
        assert_eq!(sock.peer_addr().unwrap().port(), pa);
        assert_ne!(pa, pb);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// The no-namespace dialer: a real fork, a real socketpair and a real fd
    /// handoff, with only the `setns` pair skipped.
    ///
    /// Re-entering our *own* network namespace is not a way to fake the other
    /// half: `setns` needs `CAP_SYS_ADMIN` in the namespace's owning user
    /// namespace, which an unprivileged process does not have for the one it is
    /// already in. A share works because h5i created the box's user namespace
    /// and therefore holds that capability in it. So the namespace entry is
    /// covered by a live box, and everything else is covered here.
    fn dialer_to(port: u16) -> Dialer {
        Dialer::spawn_local(port).expect("spawn a dialer on this machine's loopback")
    }

    #[test]
    fn the_helper_hands_back_a_working_socket() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = server.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut s, _) = server.accept().expect("accept");
            let mut buf = [0u8; 5];
            s.read_exact(&mut buf).expect("read");
            s.write_all(b"pong").expect("write");
        });

        let dialer = dialer_to(port);
        let mut sock = dialer.connect().expect("connect through the helper");
        sock.write_all(b"hello").expect("write");
        let mut back = [0u8; 4];
        sock.read_exact(&mut back).expect("read");
        assert_eq!(&back, b"pong");
        t.join().expect("server thread");
    }

    #[test]
    fn a_dead_port_is_reported_as_a_dead_port() {
        // Port 1 rather than "bind an ephemeral port and drop it": the kernel
        // hands ephemeral ports straight back out, so another test in this
        // binary picks it up between the drop and the connect, and the
        // assertion fails for a reason that has nothing to do with the code.
        // Nothing binds port 1 without privileges we do not have.
        let dialer = dialer_to(1);
        let err = dialer.connect().expect_err("nothing is listening");
        assert!(
            format!("{err}").contains("Nothing is listening")
                || format!("{err}").contains("nothing is listening"),
            "unhelpful message: {err}"
        );
    }

    #[test]
    fn the_destination_cannot_be_redirected_after_spawn() {
        // The property, stated as a test: the only thing on the wire between
        // the bridge and the helper is a request byte. There is no port field,
        // so no peer input can move where this connects.
        let a = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let b = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let (pa, pb) = (
            a.local_addr().unwrap().port(),
            b.local_addr().unwrap().port(),
        );
        let dialer = dialer_to(pa);
        assert_eq!(dialer.port(), pa);
        let sock = dialer.connect().expect("connect");
        assert_eq!(sock.peer_addr().unwrap().port(), pa);
        assert_ne!(pa, pb);
    }

    #[test]
    fn many_connections_come_from_one_fork() {
        // The reason the helper is long-lived. Each of these would otherwise be
        // a `fork()` from inside an async runtime.
        let server = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = server.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            for _ in 0..8 {
                let _ = server.accept();
            }
        });
        let dialer = dialer_to(port);
        for _ in 0..8 {
            dialer.connect().expect("connect");
        }
        t.join().expect("server thread");
    }

    #[test]
    fn a_box_that_is_not_running_is_refused_at_spawn() {
        // pid 1 is init: it exists, but it is not ours and its namespaces are
        // not ones an unprivileged process may join. Either way `spawn` must
        // fail with a sentence rather than hand back a dialer that cannot work.
        let err = Dialer::spawn(1, 3000).expect_err("init's namespaces are not joinable");
        let msg = format!("{err}");
        assert!(msg.contains("box"), "unhelpful message: {msg}");
    }
}
