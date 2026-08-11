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
//! **The fork happens once, at startup, not per connection.** A share runs an
//! async runtime, and `fork()` in a process with a thread pool is a trap: the
//! child inherits one thread and any lock another thread held at fork time,
//! including the allocator's. So the dialer forks while the process is still
//! single-threaded and keeps the child alive as a small helper that answers
//! "connect me" over a socketpair for as long as the share lasts.
//!
//! **The destination is pinned at spawn.** The child holds `port` in its own
//! memory and reads nothing but a one-byte request. There is no field on the
//! wire, from a peer or from the box, that can redirect where this connects —
//! so the bridge cannot be turned into a general-purpose proxy into the box's
//! namespace, which is the failure mode that would matter most if a peer or a
//! shared page went looking.

use std::net::TcpStream;
use std::sync::Mutex;

use h5i_error::H5iError;

/// The child's answer to a connect request, and its startup report. Numbers
/// rather than messages because a forked child has no safe way to format one.
const STATUS_OK: u8 = 0;
const STATUS_CONNECT_FAILED: u8 = 1;
/// The connect did not fail — it never finished. A dev server whose accept
/// queue is full *is* listening, and reporting that as "nothing is listening on
/// port 3000" sends somebody to start a server that is already running. Sent
/// for anything that is not a refusal: the timeout, and h5i's own resource
/// failures inside the helper (`EMFILE`, `ENETDOWN`), which are not facts about
/// the user's dev server at all.
const STATUS_CONNECT_STUCK: u8 = 2;
/// There is no route to `127.0.0.1` at all — the box's network namespace has no
/// loopback interface up. Nothing inside such a box can reach itself, so a
/// share of it can never work, and it is worth its own status because the
/// answer is not "start your dev server" but "this box cannot be shared".
const STATUS_NO_LOOPBACK: u8 = 3;
const STATUS_NO_NS: u8 = 2;
const STATUS_SETNS: u8 = 3;
/// The parent's request. One value, so there is nothing to parse.
#[cfg(target_os = "linux")]
const REQUEST: u8 = 0x01;

/// How long the helper will wait for the box's dev server to accept.
///
/// Loopback inside the box, so a working server answers in microseconds and a
/// dead port refuses immediately. This bounds the one case in between: a server
/// that is up but not accepting.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// A live route into one port of one box.
#[derive(Debug)]
pub struct Dialer {
    inner: Inner,
    port: u16,
}

#[derive(Debug)]
enum Inner {
    /// Linux: a helper process living in the box's namespaces.
    #[cfg(target_os = "linux")]
    Helper {
        /// Serialized: one request and one reply at a time on a shared socket.
        ///
        /// The `bool` is "this channel is still in step". A request whose reply
        /// never arrived leaves that reply — and the descriptor attached to it —
        /// queued on the socket, and the *next* caller's `recvmsg` would pick it
        /// up and be handed somebody else's connection. The mutex prevents two
        /// callers overlapping; it cannot prevent them getting out of step. So
        /// a failed exchange retires the channel instead.
        sock: Mutex<(std::os::fd::OwnedFd, bool)>,
        child: libc::pid_t,
    },
    /// Everywhere else: no route, and the constructor says so.
    ///
    /// macOS used to have an arm of its own that connected to
    /// `127.0.0.1:<port>` on the *host*, because a macOS box binds host
    /// loopback. That is not a route into a box, it is the host's own port —
    /// and on Linux this feature refuses exactly that shape, because a
    /// `workspace`-tier box shares the host's network and sharing it would
    /// publish whatever happened to be listening. The macOS arm was the same
    /// situation with the refusal missing, and it was unreachable anyway:
    /// `view::box_pid` returns `None` there, so the CLI declines before a
    /// `Dialer` exists.
    #[cfg(not(target_os = "linux"))]
    Unsupported,
}

impl Dialer {
    /// The port inside the box that this dialer is pinned to.
    pub fn port(&self) -> u16 {
        self.port
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
    /// the box had broken — and the broken-channel case is sticky, so one lost
    /// reply produced a receipt confidently asserting hundreds of times that
    /// somebody's dev server was down when it was running the whole time.
    pub fn connect(&self) -> Result<TcpStream, DialError> {
        match &self.inner {
            #[cfg(target_os = "linux")]
            Inner::Helper { sock, child } => self.request(sock, *child),
            #[cfg(not(target_os = "linux"))]
            Inner::Unsupported => Err(DialError::Broken(unsupported())),
        }
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
}

impl DialError {
    pub fn into_inner(self) -> H5iError {
        match self {
            DialError::NoLoopback(e) | DialError::NothingListening(e) | DialError::Broken(e) => e,
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
}

impl std::fmt::Display for DialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DialError::NoLoopback(e) | DialError::NothingListening(e) | DialError::Broken(e) => {
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

#[cfg(not(target_os = "linux"))]
fn unsupported() -> H5iError {
    H5iError::Metadata(
        "sharing a box's port needs a network namespace to enter, which is a Linux thing. \
         `h5i box share` is not available on this platform."
            .into(),
    )
}

// ─── Linux ──────────────────────────────────────────────────────────────────

/// Linux only, and unlike the viewer forward this module needs no arch gate:
/// it does its own `SCM_RIGHTS` rather than borrowing the seccomp supervisor's
/// helper, so there is no narrower dependency to match.
#[cfg(target_os = "linux")]
impl Dialer {
    /// Fork the helper into the box's namespaces.
    ///
    /// **Call this before starting an async runtime.** The whole reason the
    /// helper is long-lived is that forking a multi-threaded process is unsafe,
    /// and that reasoning is defeated if the fork happens after the runtime has
    /// spawned its workers.
    pub fn spawn(box_pid: u32, port: u16) -> Result<Dialer, H5iError> {
        Dialer::spawn_inner(Some(box_pid), port)
    }

    /// For a box with no network namespace of its own — the `workspace` tier,
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
            // the *next* caller's `recvmsg` — so the channel is out of step and
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
/// Nothing in the child may allocate — see [`helper_main`] — and `format!`
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
/// **Everything below the fork is allocation-free**, and that is a requirement
/// rather than a style. `Dialer::spawn` is documented as being called before a
/// runtime starts, but "documented" is not "enforced" — a caller that gets it
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
        // serialises every other connection of the share behind it — and the
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
        // exactly one byte retires the channel for the life of the share —
        // every later connection answers "the box dialer lost track of a reply"
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
        // to stop lives *inside* the mutex — so skipping the shutdown on a
        // poisoned lock means waiting forever for a process that was never told
        // to exit.
        {
            use std::os::fd::AsRawFd;
            let guard = sock.lock().unwrap_or_else(|p| p.into_inner());
            unsafe { libc::shutdown(guard.0.as_raw_fd(), libc::SHUT_RDWR) };
        }
        // Polled rather than blocking. The helper only reads the socket
        // *between* connects, so if it is inside one it cannot notice the
        // shutdown until that connect finishes — and this runs on the way out
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// The no-namespace dialer: a real fork, a real socketpair and a real fd
    /// handoff, with only the `setns` pair skipped.
    ///
    /// Re-entering our *own* network namespace is not a way to fake the other
    /// half — `setns` needs `CAP_SYS_ADMIN` in the namespace's owning user
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
