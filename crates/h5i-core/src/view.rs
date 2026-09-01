//! The viewer forward: the whole trusted surface between a human and a box.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};

use crate::error::H5iError;

/// The per-box viewer token, relative to an env directory.
///
/// A sibling of the receipt log, deliberately *not* under `<env>/spool` or
/// `<env>/tmp`: the two paths a box can write. A token the box could read is a
/// token the agent could hand to anything it can reach.
const TOKEN_FILE: &str = "view-token";

/// 128 bits of OS entropy, hex ([`crate::token`]). Long enough that guessing is
/// not a strategy against a loopback listener that exists for the life of one
/// session, and drawn from the system CSPRNG, because this one is written to
/// disk and outlives the process that minted it.
const TOKEN_BYTES: usize = 16;

fn token_path(env_dir: &Path) -> PathBuf {
    env_dir.join(TOKEN_FILE)
}

/// Mint this box's viewer token if it has none, and return it.
///
/// Called at box creation so the token exists before anything inside the box
/// runs: a token created on first `view` would be a token minted after an agent
/// had already had a chance to watch for it.
pub fn ensure_token(env_dir: &Path) -> Result<String, H5iError> {
    if let Some(existing) = read_token(env_dir) {
        return Ok(existing);
    }
    let token = crate::token::hex(TOKEN_BYTES)?;
    let p = token_path(env_dir);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| H5iError::with_path(e, parent))?;
    }
    std::fs::write(&p, &token).map_err(|e| H5iError::with_path(e, &p))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(token)
}

/// This box's viewer token, if one has been minted.
pub fn read_token(env_dir: &Path) -> Option<String> {
    let t = std::fs::read_to_string(token_path(env_dir)).ok()?;
    let t = t.trim().to_string();
    (!t.is_empty()).then_some(t)
}

/// Constant-time-ish comparison. The token is compared on every connection to a
/// loopback listener, so a timing oracle is a stretch, but the cost of not
/// leaking one is three lines.
fn token_matches(expected: &str, given: &str) -> bool {
    if expected.len() != given.len() {
        return false;
    }
    expected
        .bytes()
        .zip(given.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

/// The port agent-browser's stream server is listening on *inside* the box.
///
/// The daemon writes it to `<session>.stream` next to its socket, and that
/// directory is the box's private `/tmp`, backed by `<env>/tmp`, so the port is
/// readable from the host without asking the box anything.
pub fn stream_port(env_dir: &Path) -> Option<u16> {
    let dir = env_dir.join("tmp").join("agent-browser");
    let mut ports: Vec<u16> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".stream"))
        // The turbofish is load-bearing on Windows, not decoration. Inferring
        // the parse target through `collect()` needs `Vec<u16>: FromIterator`
        // to be unique, and it is not there: `encode_unicode` (via `console`)
        // adds a second impl, so the whole chain becomes ambiguous and only
        // that target fails to compile.
        .filter_map(|e| {
            std::fs::read_to_string(e.path())
                .ok()?
                .trim()
                .parse::<u16>()
                .ok()
        })
        .collect();
    ports.sort_unstable();
    ports.into_iter().next()
}

/// The port a host session's engine is listening on, from the file it wrote.
///
/// The counterpart of [`stream_port`]: a box advertises inside its own `/tmp`
/// and is found by scanning, while a host session writes one known file.
pub fn session_stream_port(stream_file: &Path) -> Option<u16> {
    let port = std::fs::read_to_string(stream_file)
        .ok()?
        .trim()
        .parse::<u16>()
        .ok()?;
    // Port 0 means the engine wrote the file before it had bound.
    (port != 0).then_some(port)
}

/// Where a browser's frame stream is, and how to reach it.
///
/// Two arms, because they are two security stories rather than two transports:
/// [`Route::Boxed`] enters a box's namespaces and nothing is bound on the host,
/// with egress enforced outside the engine; [`Route::Host`] is plain loopback,
/// where the engine's own confinement is all there is.
///
/// A type rather than a pair of code paths, so the second arm is not bolted on
/// once per viewer with two ideas of what "not streaming" should say.
#[derive(Debug, Clone)]
pub enum Route {
    Boxed {
        pid: u32,
        pid_ns: std::ffi::OsString,
        port: u16,
    },
    Host {
        port: u16,
    },
}

impl Route {
    pub fn port(&self) -> u16 {
        match self {
            Route::Boxed { port, .. } | Route::Host { port } => *port,
        }
    }

    /// Whether something outside the engine is holding it. Read by the status
    /// line, which must not imply containment a host session does not have.
    pub fn is_boxed(&self) -> bool {
        matches!(self, Route::Boxed { .. })
    }

    pub fn connect(&self) -> Result<TcpStream, H5iError> {
        match self {
            Route::Boxed { pid, pid_ns, port } => connect_in_netns(*pid, *port, pid_ns),
            Route::Host { port } => TcpStream::connect(("127.0.0.1", *port)).map_err(|e| {
                H5iError::Metadata(format!(
                    "the session's browser is not answering on 127.0.0.1:{port}: {e}. \
                     It may have exited since it wrote its stream port."
                ))
            }),
        }
    }
}

/// A pid living *inside* the box's namespaces.
pub fn box_pid(env_dir: &Path) -> Option<u32> {
    box_pid_ns(env_dir).map(|(pid, _)| pid)
}

/// [`box_pid`], keeping the namespace it was identified by.
///
/// The pid alone is not an identity. Pids are reissued, and this one is a
/// descendant that carries none of the `started_ticks` binding
/// [`session_pid_verified`] gives the session pid. Every caller that goes on to
/// *enter* the namespace wants the link this walk actually matched on, so
/// [`connect_in_netns`] can check that it arrived where the walk was looking
/// rather than wherever the pid points by the time it is used.
pub fn box_pid_ns(env_dir: &Path) -> Option<(u32, std::ffi::OsString)> {
    // Verified, because this pid decides which network namespace a share
    // publishes a port out of. See [`session_pid_verified`].
    let session = session_pid_verified(env_dir, true)?;
    let own_netns = std::fs::read_link("/proc/self/ns/net").ok()?;
    let children = child_map();

    // Breadth-first: the box's outermost process is the one whose namespaces
    // enclose everything else in it, so the shallowest match is the right one.
    let mut queue = std::collections::VecDeque::from([session]);
    let mut seen = std::collections::HashSet::from([session]);
    while let Some(pid) = queue.pop_front() {
        if let Ok(ns) = std::fs::read_link(format!("/proc/{pid}/ns/net"))
            && ns != own_netns
        {
            return Some((pid, ns.into_os_string()));
        }
        for &child in children.get(&pid).map(Vec::as_slice).unwrap_or(&[]) {
            if seen.insert(child) {
                queue.push_back(child);
            }
        }
    }
    None
}

/// pid → its children, from `/proc/<pid>/stat`.
fn child_map() -> std::collections::HashMap<u32, Vec<u32>> {
    let mut map: std::collections::HashMap<u32, Vec<u32>> = std::collections::HashMap::new();
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return map;
    };
    for e in rd.flatten() {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        // The comm field is parenthesised and may contain spaces, so ppid is
        // read relative to the last ')' rather than by splitting the whole line.
        let Some(after_comm) = stat.rfind(')').map(|i| &stat[i + 1..]) else {
            continue;
        };
        let Some(ppid) = after_comm
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        map.entry(ppid).or_default().push(pid);
    }
    map
}

/// The host-side `h5i` pid holding this box's session, from the live registry.
///
/// Public because it is what "the box" *means* on a platform with no
/// namespaces. [`box_pid`] answers "which pid is inside the box's network
/// namespace", which is a question only Linux has; on macOS a box is the
/// process tree under this pid, and `h5i box share` identifies it that way
/// (`h5i_share::owner`).
pub fn session_pid(env_dir: &Path) -> Option<u32> {
    session_pid_verified(env_dir, false)
}

/// [`session_pid`], optionally insisting the record proves whose pid it is.
pub fn session_pid_verified(env_dir: &Path, verified: bool) -> Option<u32> {
    let mut best: Option<(String, u32)> = None;
    for e in std::fs::read_dir(env_dir.join("live")).ok()?.flatten() {
        let Ok(text) = std::fs::read_to_string(e.path()) else {
            continue;
        };
        let Ok(rec) = serde_json::from_str::<crate::env::LiveSession>(&text) else {
            continue;
        };
        if !crate::env::live_is_writer(&rec.kind) {
            continue;
        }
        if !pid_alive(rec.pid) {
            continue;
        }
        if verified && rec.started_ticks.is_none() {
            continue;
        }
        if !crate::env::live_identity_holds(&rec) {
            continue;
        }
        if best.as_ref().is_none_or(|(s, _)| *s <= rec.started_at) {
            best = Some((rec.started_at.clone(), rec.pid));
        }
    }
    best.map(|(_, pid)| pid)
}

fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

// ─── crossing into the box's namespaces ─────────────────────────────────────

/// Connect to loopback inside `pid`'s user and network namespaces.
///
/// A forked child enters the namespaces and returns the socket over `SCM_RIGHTS`.
/// `want_ns` is the namespace observed with the original process identity; it is
/// checked after `setns` to reject PID reuse between discovery and connection.
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub fn connect_in_netns(pid: u32, port: u16, want_ns: &std::ffi::OsStr) -> Result<TcpStream, H5iError> {
    use h5i_sandbox::seccomp_notify::recv_fd;
    use std::os::fd::FromRawFd;

    // Read before forking: the parent may allocate, the child may not.
    let own_ns = std::fs::read_link("/proc/self/ns/net")
        .ok()
        .map(|p| p.into_os_string());
    if own_ns.as_deref() == Some(want_ns) {
        return Err(H5iError::Metadata(format!(
            "the namespace recorded for pid {pid} is this process's own, so it is not a box — \
             refusing to connect to 127.0.0.1:{port} on the host (fail-closed)."
        )));
    }
    let want_ns = want_ns.as_encoded_bytes();

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
    let close = |fd: i32| unsafe { libc::close(fd) };

    let child = unsafe { libc::fork() };
    if child < 0 {
        close(parent_end);
        close(child_end);
        return Err(H5iError::Io(std::io::Error::last_os_error()));
    }
    if child == 0 {
        // Child. Nothing here may allocate through a lock the parent held at
        // fork time, so it stays to raw syscalls and exits via `_exit`.
        close(parent_end);
        // `want_ns` is memory the parent allocated before the fork, so reading
        // it here allocates nothing and takes no lock.
        let code = enter_and_connect(pid, port, child_end, want_ns);
        unsafe { libc::_exit(code) };
    }

    close(child_end);
    let fd = unsafe { recv_fd(parent_end) };
    close(parent_end);
    // Reap, so a viewer that opens many connections does not leave zombies.
    let mut status = 0;
    unsafe { libc::waitpid(child, &mut status, 0) };

    if let Ok(fd) = fd {
        return Ok(unsafe { TcpStream::from_raw_fd(fd) });
    }
    // No fd came back. `recv_fd` can only say "short read", which is exactly
    // the kind of message that cost a session on the browser daemon, so the
    // child reports *which* step failed through its exit status, and that is
    // what the operator sees.
    let code = if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        -1
    };
    Err(H5iError::Metadata(match code {
        EXIT_WRONG_NS => format!(
            "entering the box's network namespace (pid {pid}) landed somewhere else — \
             refusing to connect to 127.0.0.1:{port} from a namespace that is not the box's. \
             The session that held it has ended and its pid has been reissued (fail-closed)."
        ),
        EXIT_NO_NS => format!(
            "the box (pid {pid}) is gone — its namespaces no longer exist. \
             A box only has them while a session is running; start one with \
             `h5i box shell <name>`."
        ),
        // Deliberately no errno here: the failing `setns` happened in the
        // forked child, so the parent's `last_os_error` describes an unrelated
        // syscall. A wrong errno is worse than none.
        EXIT_SETNS => format!(
            "could not enter the box's network namespace (pid {pid}). \
             Joining it needs the user namespace that created it, so run `h5i box view` \
             as the user that started the box's session."
        ),
        EXIT_CONNECT => format!(
            "entered the box, but nothing is listening on 127.0.0.1:{port} inside it. \
             The browser stopped streaming — inside the box, run `agent-browser stream enable`."
        ),
        _ => format!("could not reach the box's browser stream (pid {pid}, port {port})"),
    }))
}

/// Exit codes the forked child uses to say which step failed. A forked child
/// has no safe way to format an error, but it can always pick a number.
///
/// Gated with the code that uses them: everything here is `setns`-specific, so
/// on a non-Linux target they are dead code, and `-D warnings` in CI is right
/// to say so.
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
const EXIT_NO_NS: i32 = 2;
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
const EXIT_SETNS: i32 = 3;
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
const EXIT_CONNECT: i32 = 4;
/// `setns` returned success and the thread is not in the namespace it aimed at,
/// including the case where it never left the one it started in.
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
const EXIT_WRONG_NS: i32 = 5;

/// The child half of [`connect_in_netns`]. Returns the exit code the parent
/// turns back into a message, since a forked child cannot safely format one.
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn enter_and_connect(pid: u32, port: u16, sock: i32, want_ns: &[u8]) -> i32 {
    use h5i_sandbox::seccomp_notify::send_fd;
    use std::os::fd::AsRawFd;

    // Order matters: the netns is owned by the box's user namespace, so we have
    // to be in that userns before the kernel will let us join the netns.
    for (ns, kind) in [("user", libc::CLONE_NEWUSER), ("net", libc::CLONE_NEWNET)] {
        let path = format!("/proc/{pid}/ns/{ns}\0");
        let fd = unsafe { libc::open(path.as_ptr() as *const libc::c_char, libc::O_RDONLY) };
        if fd < 0 {
            // The whole `/proc/<pid>/ns` directory going missing means the
            // process died, which is the common case and deserves its own
            // message rather than "setns failed".
            return EXIT_NO_NS;
        }
        let rc = unsafe { libc::setns(fd, kind) };
        unsafe { libc::close(fd) };
        // A box at the `process` tier has no user namespace of its own to join,
        // so only the netns attempt is fatal.
        if rc != 0 && kind == libc::CLONE_NEWNET {
            return EXIT_SETNS;
        }
    }

    // Where the thread actually ended up, not where `setns` was pointed, and
    // not where `/proc/<pid>` says it should be. That is the reading in
    // question. `setns` into the namespace you are already in returns 0, so the
    // return code above is not an answer; this readlink is. Checked before the
    // connect rather than after, because the connect is the thing being aimed.
    let mut got = [0u8; 64];
    let n = unsafe {
        libc::readlink(
            c"/proc/self/ns/net".as_ptr(),
            got.as_mut_ptr() as *mut libc::c_char,
            got.len(),
        )
    };
    if n <= 0 || got[..n as usize] != *want_ns {
        return EXIT_WRONG_NS;
    }

    let Ok(stream) = TcpStream::connect(("127.0.0.1", port)) else {
        return EXIT_CONNECT;
    };
    if unsafe { send_fd(sock, stream.as_raw_fd()) }.is_err() {
        return 1;
    }
    0
}

/// macOS: there is no namespace to enter.
#[cfg(target_os = "macos")]
pub fn connect_in_netns(
    pid: u32,
    port: u16,
    want_ns: &std::ffi::OsStr,
) -> Result<TcpStream, H5iError> {
    // No namespace to enter and none to check: the box's listener is on the
    // host's shared loopback, which is the point the doc above makes.
    let _ = (pid, want_ns);
    TcpStream::connect(("127.0.0.1", port)).map_err(|e| {
        H5iError::Metadata(format!(
            "viewer could not reach the box's stream port {port} on loopback: {e}"
        ))
    })
}

#[cfg(not(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    target_os = "macos"
)))]
pub fn connect_in_netns(
    _pid: u32,
    _port: u16,
    _want_ns: &std::ffi::OsStr,
) -> Result<TcpStream, H5iError> {
    Err(H5iError::Metadata(
        "the viewer forward needs Linux namespaces or macOS loopback; it is not available on \
         this platform"
            .into(),
    ))
}

// ─── the handshake gate ─────────────────────────────────────────────────────

/// What a client asked for, as far as the forward cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub path: String,
    pub token: Option<String>,
    pub origin: Option<String>,
    pub upgrade: bool,
}

/// Why a connection was refused. Each maps to an HTTP status, and each is
/// distinct on purpose: "you forgot the token" and "your page may not do this"
/// are different problems for whoever is reading the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// No token, or the wrong one.
    BadToken,
    /// An `Origin` that is not this forward's own page.
    CrossOrigin,
}

impl Refusal {
    pub fn status(self) -> (u16, &'static str) {
        match self {
            Refusal::BadToken => (401, "Unauthorized"),
            Refusal::CrossOrigin => (403, "Forbidden"),
        }
    }
    pub fn explain(self) -> &'static str {
        match self {
            Refusal::BadToken => {
                "this box's viewer token is missing or wrong — `h5i browser url <box>` prints \
                 the URL that carries it"
            }
            Refusal::CrossOrigin => {
                "cross-origin connections to a box are refused: a page you have open in \
                 another tab must not be able to drive a running box"
            }
        }
    }
}

/// Parse the request line and the headers the gate needs. Returns `None` for
/// anything that is not a well-formed HTTP request, which is refused without
/// further thought.
pub fn parse_request(head: &str) -> Option<Request> {
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    if parts.next()? != "GET" {
        return None;
    }
    let target = parts.next()?;
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), Some(q)),
        None => (target.to_string(), None),
    };
    let token = query.and_then(|q| {
        q.split('&')
            .filter_map(|kv| kv.split_once('='))
            .find(|(k, _)| *k == "token")
            .map(|(_, v)| v.to_string())
    });

    let mut origin = None;
    let mut upgrade = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "origin" => origin = Some(value.to_string()),
            "upgrade" => upgrade = value.eq_ignore_ascii_case("websocket"),
            _ => {}
        }
    }
    Some(Request {
        path,
        token,
        origin,
        upgrade,
    })
}

/// The gate: may this connection through?
///
/// `self_origin` is the forward's own `http://127.0.0.1:<port>`. A request with
/// no `Origin` at all is allowed. That is a command-line client (`websocat`, a
/// test), not a web page, and browsers always send one. What is refused is a
/// *different* origin, which is precisely a page the human has open elsewhere.
pub fn gate(req: &Request, expected_token: &str, self_origin: &str) -> Result<(), Refusal> {
    match &req.token {
        Some(t) if token_matches(expected_token, t) => {}
        _ => return Err(Refusal::BadToken),
    }
    if let Some(origin) = &req.origin
        && origin != self_origin
    {
        return Err(Refusal::CrossOrigin);
    }
    Ok(())
}

// ─── WebSocket framing, only as far as the lock needs ───────────────────────

/// What to do with one client→box frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameVerdict {
    /// The forward answers this itself. Never written upstream.
    Forward,
    /// Connection management: forwarded regardless of who holds the lock, or
    /// the socket would stall and time out while a human watches.
    Control,
    /// A request about frame *delivery*. How fast, and how many in flight.
    /// Forwarded regardless of the lock, because it drives nothing on the page
    /// and gating it breaks watching, which never needed the lock.
    Pacing,
    /// Actual input. A click, a keystroke. Forwarded only for the lock holder.
    Input,
}

/// The two message names that ask about delivery rather than about the page.
///
/// An allowlist of exact strings, and it has to stay one. The reason
/// [`classify_opcode`] refuses to look at payloads is that guessing "probably
/// harmless" is how gates get bypassed; the answer to a message that genuinely
/// must pass is not to guess more loosely but to *name* it.
const PACING_MESSAGES: [&str; 2] = ["config", "ack"];

/// Messages the forward acts on itself and never passes upstream.
///
/// Exactly one: the control lock. It is a *host* fact kept in a file beside the
/// box, and the thing on the other end of this socket is inside the box — the
/// forward is the only party that can write it. Without this the web viewer's
/// only way to take the controls was to leave the page and reload.
const FORWARD_MESSAGES: [&str; 1] = ["control"];

/// Largest text frame worth parsing to see whether it is a pacing message.
/// `{"type":"config","maxFps":30,"pacing":"ack"}` is 44 bytes; anything in this
/// neighbourhood is generous, and it stops a hostile client from making the
/// forward parse a megabyte of JSON per frame for free.
const MAX_PACING_FRAME: usize = 4096;

/// Classify a frame by its opcode.
///
/// This is the whole of the WebSocket protocol the forward needs to understand.
/// It does not decode payloads, reassemble fragments or care what an input frame
/// says: the lock decides by *direction and kind*, so a new message type
/// upstream is governed correctly the day it is added rather than the day we
/// notice it.
pub fn classify_opcode(opcode: u8) -> FrameVerdict {
    match opcode {
        // close, ping, pong. The whole control-opcode range
        0x8..=0xA => FrameVerdict::Control,
        // continuation, text, binary, and anything reserved, which is treated
        // as input because guessing "harmless" about an unknown frame is how
        // gates get bypassed.
        _ => FrameVerdict::Input,
    }
}

/// Classify a frame, looking at the payload only far enough to spot the two
/// messages that must not be gated.
///
/// Gating those two is not a conservative default, it is a broken viewer, and it
/// was: the page asks for a frame-rate cap the moment it connects, that ask is a
/// text frame, every text frame was `Input`, and a box is agent-held by default,
/// so the cap was dropped on essentially every session and the box streamed
/// uncapped. Silent, because a dropped `config` looks exactly like a `config`
/// that was honoured.
pub fn classify(opcode: u8, fin: bool, payload: &[u8]) -> FrameVerdict {
    if classify_opcode(opcode) == FrameVerdict::Control {
        return FrameVerdict::Control;
    }
    // Text only, and only a whole message. A fragment cannot be judged from the
    // fragment in hand, so it is input.
    if opcode == 0x1 && fin && payload.len() <= MAX_PACING_FRAME {
        match message_type(payload) {
            Some(name) if PACING_MESSAGES.contains(&name.as_str()) => {
                return FrameVerdict::Pacing;
            }
            Some(name) if FORWARD_MESSAGES.contains(&name.as_str()) => {
                return FrameVerdict::Forward;
            }
            _ => {}
        }
    }
    FrameVerdict::Input
}

/// The `type` of a JSON message, if it is one.
fn message_type(payload: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(payload).ok()?;
    v.get("type")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

/// Take or hand back the control lock, on a viewer's say-so.
///
/// Whoever is on this socket reached a loopback port with the box's viewer
/// token — the same standing `h5i browser take` has — so this grants nothing
/// new. A take always succeeds: the agent is a program that can wait.
fn apply_control(payload: &[u8], env_dir: &Path) {
    let Some(v) = serde_json::from_slice::<serde_json::Value>(payload).ok() else {
        return;
    };
    match v.get("hold").and_then(serde_json::Value::as_bool) {
        Some(true) => {
            let _ = crate::control::take(env_dir);
            crate::browser_session::journal_control(
                env_dir,
                crate::control::Holder::Human.as_str(),
                Some("taken at the live view"),
            );
        }
        Some(false) => {
            let _ = crate::control::release(env_dir);
            crate::browser_session::journal_control(
                env_dir,
                crate::control::Holder::Agent.as_str(),
                Some("handed back at the live view"),
            );
        }
        None => {}
    }
}

/// One frame from the client, as read.
struct ClientFrame {
    opcode: u8,
    /// The final fragment of its message? A partial message cannot be judged
    /// from the part in hand.
    fin: bool,
    /// The exact bytes as they arrived, so a forwarded frame is byte-identical
    /// to the original. Mask and all.
    raw: Vec<u8>,
    /// The same payload with the client's mask removed, which is the only form
    /// [`classify`] can read.
    payload: Vec<u8>,
}

/// Read one WebSocket frame from `r`.
///
/// `None` at a clean end of stream; an error for a truncated or absurd frame.
fn read_frame(r: &mut impl Read) -> std::io::Result<Option<ClientFrame>> {
    let mut head = [0u8; 2];
    match r.read_exact(&mut head) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let mut raw = head.to_vec();
    let opcode = head[0] & 0x0f;
    let fin = head[0] & 0x80 != 0;
    let masked = head[1] & 0x80 != 0;
    let len7 = head[1] & 0x7f;

    let payload_len: u64 = match len7 {
        126 => {
            let mut ext = [0u8; 2];
            r.read_exact(&mut ext)?;
            raw.extend_from_slice(&ext);
            u16::from_be_bytes(ext) as u64
        }
        127 => {
            let mut ext = [0u8; 8];
            r.read_exact(&mut ext)?;
            raw.extend_from_slice(&ext);
            u64::from_be_bytes(ext)
        }
        n => n as u64,
    };
    // A frame larger than any viewport input could be is a protocol error, not
    // something to allocate for.
    if payload_len > MAX_FRAME {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "oversized websocket frame",
        ));
    }
    let mut key = [0u8; 4];
    if masked {
        r.read_exact(&mut key)?;
        raw.extend_from_slice(&key);
    }
    let mut payload = vec![0u8; payload_len as usize];
    r.read_exact(&mut payload)?;
    // Appended before unmasking: what is forwarded is what arrived.
    raw.extend_from_slice(&payload);
    if masked {
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= key[i % 4];
        }
    }
    Ok(Some(ClientFrame {
        opcode,
        fin,
        raw,
        payload,
    }))
}

/// Cap on a single inbound frame. Input events are tiny; this only has to be
/// larger than any of them and small enough that a hostile client cannot make
/// the forward allocate for free.
const MAX_FRAME: u64 = 1 << 20;

/// Copy client→box, dropping input frames while the agent holds the lock.
pub fn pump_input(mut from_client: impl Read, mut to_box: impl Write, env_dir: &Path) -> Pump {
    let mut pump = Pump::default();
    // Whether a fragmented message is currently being forwarded.
    //
    // The forward/drop decision is per *message*, not per frame. Deciding per
    // frame meant that if the control lock changed hands mid-message, the first
    // fragment went upstream and its continuations did not, leaving
    // agent-browser's parser mid-message, so the next frame it saw was a protocol
    // violation that could kill the stream out from under the watching human.
    let mut forwarding_fragmented: Option<bool> = None;
    loop {
        match read_frame(&mut from_client) {
            Ok(None) => return pump,
            Err(e) => {
                pump.error = Some(e.to_string());
                return pump;
            }
            Ok(Some(frame)) => {
                let verdict = classify(frame.opcode, frame.fin, &frame.payload);
                // Never written upstream: the lock is not the box's to write.
                if verdict == FrameVerdict::Forward {
                    apply_control(&frame.payload, env_dir);
                    continue;
                }
                let is_input = verdict == FrameVerdict::Input;
                // A continuation (opcode 0) inherits the decision made for the
                // message's first frame; anything else decides afresh. Control
                // frames may be interleaved mid-message and carry their own
                // verdict, so they never touch the fragmentation state.
                let is_continuation = frame.opcode == 0;
                let control_frame = frame.opcode >= 0x8;
                let allowed = match (is_continuation, forwarding_fragmented) {
                    (true, Some(decided)) => decided,
                    _ => {
                        !is_input
                            || crate::control::read(env_dir).holder == crate::control::Holder::Human
                    }
                };
                if !control_frame {
                    forwarding_fragmented = if frame.fin { None } else { Some(allowed) };
                }
                if allowed {
                    if let Err(e) = to_box.write_all(&frame.raw).and_then(|()| to_box.flush()) {
                        pump.error = Some(e.to_string());
                        return pump;
                    }
                    if is_input {
                        pump.input_frames += 1;
                    }
                }
            }
        }
    }
}

/// What the input direction did, whether or not it ended tidily.
///
/// The count and the error are returned *together*, and that is the whole
/// point of the type. Returning `Result<u64>` loses the count on the error path,
/// which is not a cosmetic loss: a human who takes control, clicks through a
/// form and then closes the tab abruptly produces exactly that path, and the
/// export would record them as never having touched the box. Evidence must not
/// be discarded because a connection ended untidily.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Pump {
    /// Input frames that actually reached the page, so, frames sent while the
    /// human held the lock.
    pub input_frames: u64,
    /// Why the direction stopped, when it was not a clean end of stream.
    pub error: Option<String>,
}

// ─── the forward ────────────────────────────────────────────────────────────

/// A bound, ready-to-serve viewer forward.
pub struct Forward {
    listener: TcpListener,
    env_dir: PathBuf,
    env_id: String,
    policy_digest: String,
    token: String,
    /// Where the frames come from, resolved once at bind time.
    route: Route,
}

impl Forward {
    /// Bind on loopback and prepare to serve this box.
    ///
    /// Everything that can be known before a connection arrives is resolved
    /// here, so `h5i box view` fails with an explanation instead of printing a
    /// URL that will not work.
    pub fn bind(
        env_dir: &Path,
        env_id: &str,
        policy_digest: &str,
        port: u16,
    ) -> Result<Forward, H5iError> {
        let (pid, pid_ns) = box_pid_ns(env_dir).ok_or_else(|| {
            H5iError::Metadata(
                "this box is not running, so there is no browser to watch. Start a session \
                 (`h5i box shell <name>`) and try again."
                    .into(),
            )
        })?;
        let stream_port = stream_port(env_dir).ok_or_else(|| {
            H5iError::Metadata(
                "the box's browser is not streaming. Inside the box, run \
                 `agent-browser stream enable`, then try again."
                    .into(),
            )
        })?;
        Forward::on(
            env_dir,
            env_id,
            policy_digest,
            port,
            Route::Boxed {
                pid,
                pid_ns,
                port: stream_port,
            },
        )
    }

    /// Bind a forward onto an already-resolved route: the half of `bind` that
    /// does not care whether there is a box.
    pub fn on(
        state_dir: &Path,
        subject: &str,
        policy_digest: &str,
        port: u16,
        route: Route,
    ) -> Result<Forward, H5iError> {
        let token = ensure_token(state_dir)?;
        // Loopback only. Never an external address, on any code path.
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        Ok(Forward {
            listener,
            env_dir: state_dir.to_path_buf(),
            env_id: subject.to_string(),
            policy_digest: policy_digest.to_string(),
            token,
            route,
        })
    }

    /// The URL a human should open, token included.
    pub fn url(&self) -> Result<String, H5iError> {
        Ok(format!(
            "http://127.0.0.1:{}/?token={}",
            self.listener.local_addr()?.port(),
            self.token
        ))
    }

    fn self_origin(&self) -> String {
        format!(
            "http://127.0.0.1:{}",
            self.listener.local_addr().map(|a| a.port()).unwrap_or(0)
        )
    }

    /// Serve until interrupted. One connection at a time is the right shape:
    /// exactly one human is at the viewport, and a second concurrent viewer of
    /// the same box is a control-lock question we have deliberately not opened.
    pub fn serve(&self) -> Result<(), H5iError> {
        let origin = self.self_origin();
        for conn in self.listener.incoming() {
            let Ok(conn) = conn else { continue };
            if let Err(e) = self.handle(conn, &origin) {
                eprintln!("viewer: {e}");
            }
        }
        Ok(())
    }

    fn handle(&self, mut client: TcpStream, origin: &str) -> Result<(), H5iError> {
        let head = read_head(&mut client)?;
        let Some(req) = parse_request(&head) else {
            let _ = respond(&mut client, 400, "Bad Request", "not an HTTP request");
            return Ok(());
        };
        if let Err(refusal) = gate(&req, &self.token, origin) {
            let (code, reason) = refusal.status();
            let _ = respond(&mut client, code, reason, refusal.explain());
            return Ok(());
        }
        if !req.upgrade {
            // The viewer page itself. Served from here rather than from the box
            // so the box never supplies markup the human's browser will run.
            let holder = crate::control::read(&self.env_dir).holder;
            let _ = respond_html(&mut client, &viewer_page(holder));
            return Ok(());
        }

        // Enter the box, connect, and relay the handshake verbatim so
        // agent-browser negotiates with the client directly (we never have to
        // know its subprotocol or its Sec-WebSocket-Accept).
        let mut upstream = self.route.connect()?;
        upstream.write_all(rewrite_head_for_upstream(&head).as_bytes())?;
        upstream.flush()?;

        let mut client_out = client.try_clone()?;
        let mut upstream_out = upstream.try_clone()?;
        let env_dir = self.env_dir.clone();

        let opened = chrono::Utc::now();
        let holder_at_open = crate::control::read(&env_dir).holder;

        // Out: frames to the human, unconditionally. Watching never collides, so
        // this direction has no policy at all.
        //
        // When the BOX's stream ends first, this copy returns and the client must be
        // shut down too. The page has no way to learn the stream is gone, so it holds
        // the socket open and `pump_input` below blocks on a peer that will never
        // close, and `serve()` takes one connection at a time.
        let client_shutdown = client.try_clone()?;
        let out = std::thread::spawn(move || {
            let n = std::io::copy(&mut upstream, &mut client_out).unwrap_or(0);
            let _ = client_shutdown.shutdown(std::net::Shutdown::Read);
            n
        });
        // In: gated by the control lock.
        let pumped = pump_input(&mut client, &mut upstream_out, &env_dir);
        // And the mirror image: closing our end unblocks the outbound copy when
        // the HUMAN disconnects first.
        let _ = upstream_out.shutdown(std::net::Shutdown::Both);
        let bytes_out = out.join().unwrap_or(0);

        record_session(
            &Session {
                env_dir: self.env_dir.clone(),
                env_id: self.env_id.clone(),
                policy_digest: self.policy_digest.clone(),
                transport: Transport::Web,
                command: "h5i box view".into(),
            },
            opened,
            holder_at_open,
            bytes_out,
            &pumped,
        );
        Ok(())
    }
}

/// Which viewer a session was watched through.
///
/// Both write the same lane, because both are h5i watching on a human's behalf
/// and an export should not have to care which one was used. The label is
/// recorded rather than dropped for the one question it does answer: a terminal
/// session is one where the frames never reached a host browser at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// The loopback forward, watched in the host's browser.
    Web,
    /// The in-process terminal viewer. No listener, no token, no host browser.
    Terminal,
}

impl Transport {
    fn as_str(self) -> &'static str {
        match self {
            Transport::Web => "web",
            Transport::Terminal => "terminal",
        }
    }
}

/// What a viewer session was watching, and how it was started.
pub struct Session {
    /// Where the receipt is filed and where `control.json` lives. A box's env
    /// directory, or a browser session's own directory.
    pub env_dir: PathBuf,
    /// What was being watched: a box id, or a browser session id.
    pub env_id: String,
    pub policy_digest: String,
    pub transport: Transport,
    /// The command that started this viewer, as invoked. Carried rather than
    /// reconstructed: more than one command opens a viewer now, and a receipt
    /// naming a flag the reader did not type cannot be retraced.
    pub command: String,
}

/// Record a viewer session in the box's receipt log, so it reaches the export like any other
/// observation.
pub fn record_session(
    session: &Session,
    opened: chrono::DateTime<chrono::Utc>,
    holder_at_open: crate::control::Holder,
    bytes_out: u64,
    pumped: &Pump,
) {
    {
        let (env_dir, env_id, policy_digest) =
            (&session.env_dir, &session.env_id, &session.policy_digest);
        let input_frames = pumped.input_frames;
        let closed = chrono::Utc::now();
        let holder_at_close = crate::control::read(env_dir).holder;
        // Ground truth, not inference: input frames were forwarded, which the
        // forward only does for the lock holder. Comparing the holder at open
        // and close would miss the ordinary shape (take control, do the thing,
        // hand it back) which is exactly the session a reviewer most wants
        // flagged.
        let took_control = input_frames > 0;
        let seconds = (closed - opened).num_seconds().max(0);

        let mut body = String::new();
        body.push_str(&format!(
            "viewer session, {seconds}s ({} viewer)\n",
            session.transport.as_str()
        ));
        body.push_str(&format!("opened   {}\n", opened.to_rfc3339()));
        body.push_str(&format!("closed   {}\n", closed.to_rfc3339()));
        body.push_str(&format!(
            "control  {} at open, {} at close{}\n",
            holder_at_open.as_str(),
            holder_at_close.as_str(),
            if took_control {
                " — a human drove this box"
            } else {
                ""
            }
        ));
        body.push_str(&format!(
            "frames   {bytes_out} bytes streamed to the viewer\n"
        ));
        body.push_str(&format!(
            "input    {input_frames} frame(s) forwarded to the page\n"
        ));
        if let Some(err) = &pumped.error {
            body.push_str(&format!("note     the connection ended abruptly: {err}\n"));
        }

        let input = crate::receipt::RecordInput {
            env_id: env_id.clone(),
            policy_digest: Some(policy_digest.clone()),
            // Its own lane: neither a command the box ran nor anything the box
            // claimed, but something h5i itself did on the human's behalf.
            source: "viewer".into(),
            cmd: Some(format!(
                "{} ({}, {seconds}s)",
                session.command,
                if took_control {
                    "human took control"
                } else {
                    holder_at_close.as_str()
                }
            )),
            wall_ms: u64::try_from(seconds * 1000).ok(),
            ..Default::default()
        };
        if let Err(e) = crate::receipt::append(env_dir, input, body.as_bytes()) {
            eprintln!("viewer: could not record the session: {e}");
        }
    }
}

/// Read up to the end of the HTTP headers: how long one `read()` may block, and
/// how long the whole head may take.
///
/// Two numbers because they answer two questions, and having only the first was
/// the bug: `SO_RCVTIMEO` bounds a single syscall, so a peer sending one byte
/// every nine seconds never triggered it and never finished either. `serve()`
/// handles one connection at a time and this runs *before* the token is checked,
/// so that peer held the human's viewer shut for as long as it cared to.
const HEAD_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const HEAD_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

fn read_head(s: &mut TcpStream) -> std::io::Result<String> {
    read_head_within(s, HEAD_READ_TIMEOUT, HEAD_DEADLINE)
}

/// [`read_head`] with both bounds given, so a test can drive the deadline
/// without waiting on the production one.
fn read_head_within(
    s: &mut TcpStream,
    per_read: std::time::Duration,
    whole: std::time::Duration,
) -> std::io::Result<String> {
    // Bounded in time as well as size. A peer that connects and sends nothing,
    // a browser's speculative preconnect is the ordinary case, used to stall
    // every later viewer until it went away on its own.
    let prev = s.read_timeout()?;
    s.set_read_timeout(Some(per_read))?;
    let out = read_head_inner(s, whole);
    let _ = s.set_read_timeout(prev);
    out
}

fn read_head_inner(s: &mut TcpStream, whole: std::time::Duration) -> std::io::Result<String> {
    let deadline = std::time::Instant::now() + whole;
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    // Bounded: a header block this large is a client we do not want to serve.
    while buf.len() < 16 * 1024 {
        if std::time::Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "request head exceeded its deadline",
            ));
        }
        if s.read(&mut byte)? == 0 {
            break;
        }
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Strip the token from the request line before it goes upstream.
///
/// agent-browser has no idea what our token is and does not need to: it is the
/// forward's credential, not the stream server's. Leaving it on would put it in
/// the box's logs, which is the one place it must never appear.
fn rewrite_head_for_upstream(head: &str) -> String {
    // Only the header block, and *exactly* one terminating blank line. Getting
    // this wrong is silent and expensive: a stray trailing CRLF is not a
    // protocol error the server reports, it is two bytes the server reads as
    // the beginning of the client's first WebSocket frame, after which the
    // handshake completes, nothing is ever sent back, and the viewer simply
    // hangs.
    let header_block = head.split("\r\n\r\n").next().unwrap_or(head);
    let mut lines = header_block.split("\r\n").filter(|l| !l.is_empty());
    let mut out = String::new();
    if let Some(request_line) = lines.next() {
        let mut parts = request_line.splitn(3, ' ');
        let method = parts.next().unwrap_or("GET");
        let target = parts.next().unwrap_or("/");
        let version = parts.next().unwrap_or("HTTP/1.1");
        let target = target.split('?').next().unwrap_or("/");
        let target = if target.is_empty() { "/" } else { target };
        out.push_str(&format!("{method} {target} {version}\r\n"));
    }
    for line in lines {
        // `Origin` does not travel. `gate` has already compared it to this
        // forward's own page, which is the only comparison that means anything
        // here; the box's stream server refuses any handshake that carries one
        // at all, because for it a header no h5i viewer sends is a page.
        if line
            .split_once(':')
            .is_some_and(|(name, _)| name.trim().eq_ignore_ascii_case("origin"))
        {
            continue;
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    out.push_str("\r\n");
    out
}

fn respond(s: &mut TcpStream, code: u16, reason: &str, body: &str) -> std::io::Result<()> {
    let response = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(response.as_bytes())
}

fn respond_html(s: &mut TcpStream, body: &str) -> std::io::Result<()> {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(response.as_bytes())
}

/// The viewer page: JPEG frames in, input events out.
fn viewer_page(holder: crate::control::Holder) -> String {
    // The substituted value is one of two fixed strings from h5i's own enum, so
    // this cannot become an injection point regardless of what the box does.
    include_str!("viewer.html").replace("__H5I_HOLDER__", holder.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// `SO_RCVTIMEO` bounds one `read()`, not the head. `serve()` takes one
    /// connection at a time and this runs *before* the token is checked, so a
    /// peer sending one byte just inside the per-read timeout held the human's
    /// viewer shut for as long as it cared to, no token, no log line.
    #[test]
    fn a_peer_that_dribbles_a_head_cannot_hold_the_viewer_shut() {
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let dribbler = std::thread::spawn(move || {
            let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
            // A byte at a time, each comfortably inside the per-read timeout,
            // and never the blank line that ends a head.
            for _ in 0..200 {
                if c.write_all(b"X").is_err() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        });

        let (mut server, _) = listener.accept().unwrap();
        let started = std::time::Instant::now();
        let out = read_head_within(
            &mut server,
            std::time::Duration::from_millis(500),
            std::time::Duration::from_millis(300),
        );
        let took = started.elapsed();

        assert!(
            out.as_ref().is_err_and(|e| e.kind() == std::io::ErrorKind::TimedOut),
            "the whole head must have a deadline of its own: {out:?}"
        );
        assert!(
            took < std::time::Duration::from_secs(3),
            "it gave up after {took:?}, which is not a bound"
        );
        drop(server);
        let _ = dribbler.join();
    }

    #[test]
    fn the_token_is_minted_once_and_kept_out_of_the_boxs_reach() {
        let td = TempDir::new().unwrap();
        let a = ensure_token(td.path()).unwrap();
        let b = ensure_token(td.path()).unwrap();
        assert_eq!(a, b, "a second call must not rotate a live token");
        assert_eq!(a.len(), TOKEN_BYTES * 2);
        assert_eq!(read_token(td.path()).as_deref(), Some(a.as_str()));

        // The two paths a box can write are its spool and its /tmp scratch. A
        // token in either is a token the agent can read and pass on.
        let p = token_path(td.path());
        assert!(!p.starts_with(td.path().join("spool")));
        assert!(!p.starts_with(td.path().join("tmp")));
    }

    #[test]
    fn a_request_without_the_right_token_does_not_get_through() {
        let req =
            |q: &str| parse_request(&format!("GET /{q} HTTP/1.1\r\nHost: x\r\n\r\n")).unwrap();
        let origin = "http://127.0.0.1:9000";

        assert_eq!(gate(&req("?token=good"), "good", origin), Ok(()));
        assert_eq!(gate(&req(""), "good", origin), Err(Refusal::BadToken));
        assert_eq!(
            gate(&req("?token=bad"), "good", origin),
            Err(Refusal::BadToken)
        );
        // A prefix must not pass: the comparison is length-checked first.
        assert_eq!(
            gate(&req("?token=goo"), "good", origin),
            Err(Refusal::BadToken)
        );
    }

    #[test]
    fn a_page_from_another_origin_cannot_reach_a_running_box() {
        let with_origin = |o: &str| {
            parse_request(&format!(
                "GET /?token=good HTTP/1.1\r\nOrigin: {o}\r\nUpgrade: websocket\r\n\r\n"
            ))
            .unwrap()
        };
        let origin = "http://127.0.0.1:9000";

        // Our own page: fine.
        assert_eq!(gate(&with_origin(origin), "good", origin), Ok(()));
        // A page the human happens to have open. The whole reason this check
        // exists. Note it is refused even though it carries a valid token,
        // because a token in a URL is exactly what leaks into a browser's
        // history and referrers.
        assert_eq!(
            gate(&with_origin("https://evil.example"), "good", origin),
            Err(Refusal::CrossOrigin)
        );
        // Another loopback port is still another origin.
        assert_eq!(
            gate(&with_origin("http://127.0.0.1:9999"), "good", origin),
            Err(Refusal::CrossOrigin)
        );
        // No Origin at all is a command-line client, not a web page.
        let cli = parse_request("GET /?token=good HTTP/1.1\r\nUpgrade: websocket\r\n\r\n").unwrap();
        assert_eq!(gate(&cli, "good", origin), Ok(()));
    }

    #[test]
    fn the_request_parser_reads_what_the_gate_needs() {
        let r = parse_request(
            "GET /?token=abc&fps=10 HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: http://x\r\n\
             Upgrade: WebSocket\r\n\r\n",
        )
        .unwrap();
        assert_eq!(r.path, "/");
        assert_eq!(r.token.as_deref(), Some("abc"));
        assert_eq!(r.origin.as_deref(), Some("http://x"));
        assert!(r.upgrade, "the Upgrade header is case-insensitive");

        // Not HTTP, or not a GET: refused without interpretation.
        assert!(parse_request("POST / HTTP/1.1\r\n\r\n").is_none());
        assert!(parse_request("garbage").is_none());
    }

    #[test]
    fn the_token_never_goes_upstream() {
        // agent-browser does not know our token and must never log it. It is
        // the forward's credential, not the stream server's.
        let head = "GET /?token=secret123 HTTP/1.1\r\nHost: h\r\nUpgrade: websocket\r\n\r\n";
        let rewritten = rewrite_head_for_upstream(head);
        assert!(!rewritten.contains("secret123"), "{rewritten}");
        assert!(rewritten.starts_with("GET / HTTP/1.1\r\n"), "{rewritten}");
        // Everything else survives, or the handshake fails.
        assert!(rewritten.contains("Upgrade: websocket"), "{rewritten}");

        // Exactly one terminating blank line, and this is the assertion that
        // matters most. An extra CRLF is not rejected by the server. It is
        // read as the first two bytes of a client WebSocket frame, so the
        // handshake succeeds and then the viewer hangs with no error anywhere.
        assert!(rewritten.ends_with("\r\n\r\n"), "{rewritten:?}");
        assert!(!rewritten.ends_with("\r\n\r\n\r\n"), "{rewritten:?}");
        // Same guarantee when the caller hands us bytes past the header block.
        let with_body = format!("{head}trailing-garbage");
        let rewritten = rewrite_head_for_upstream(&with_body);
        assert!(!rewritten.contains("trailing-garbage"), "{rewritten:?}");
        assert!(rewritten.ends_with("\r\n\r\n"), "{rewritten:?}");
    }

    /// The web viewer's way of reaching for the controls, handled by the forward
    /// and never written upstream.
    #[test]
    fn a_viewer_can_take_the_lock_without_leaving_the_page() {
        let dir = TempDir::new().unwrap();
        assert_eq!(crate::control::read(dir.path()).holder, crate::control::Holder::Agent);

        let take = client_frame(0x1, br#"{"type":"control","hold":true}"#);
        let mut upstream = Vec::new();
        let pumped = pump_input(&take[..], &mut upstream, dir.path());

        assert_eq!(crate::control::read(dir.path()).holder, crate::control::Holder::Human);
        assert!(
            upstream.is_empty(),
            "a control message reached the box: {upstream:?}"
        );
        // Not counted as input either: nothing was forwarded to the page.
        assert_eq!(pumped.input_frames, 0);

        let release = client_frame(0x1, br#"{"type":"control","hold":false}"#);
        let mut upstream = Vec::new();
        pump_input(&release[..], &mut upstream, dir.path());
        assert_eq!(crate::control::read(dir.path()).holder, crate::control::Holder::Agent);
        assert!(upstream.is_empty());
    }

    /// Three verdicts, and the third must not widen the second: anything that is
    /// neither pacing nor the forward's own message is input.
    #[test]
    fn only_the_named_messages_escape_the_lock() {
        let text = |body: &[u8]| classify(0x1, true, body);
        assert_eq!(text(br#"{"type":"config"}"#), FrameVerdict::Pacing);
        assert_eq!(text(br#"{"type":"ack"}"#), FrameVerdict::Pacing);
        assert_eq!(text(br#"{"type":"control","hold":true}"#), FrameVerdict::Forward);
        for body in [
            &br#"{"type":"hints"}"#[..],
            &br#"{"type":"act","ref":"e1"}"#[..],
            &br#"{"type":"insert","ref":"e1","text":"x"}"#[..],
            &br#"{"type":"history","go":-1}"#[..],
            &br#"{"type":"reload"}"#[..],
            &br#"{"type":"input_mouse"}"#[..],
            &br#"{"type":"controlled"}"#[..],
            &br#"not json"#[..],
        ] {
            assert_eq!(
                text(body),
                FrameVerdict::Input,
                "{}",
                String::from_utf8_lossy(body)
            );
        }
        // And a fragment is never judged from the fragment in hand.
        assert_eq!(
            classify(0x1, false, br#"{"type":"control","hold":true}"#),
            FrameVerdict::Input
        );
    }

    #[test]
    fn watching_never_needs_the_lock_but_typing_does() {
        assert_eq!(classify_opcode(0x8), FrameVerdict::Control); // close
        assert_eq!(classify_opcode(0x9), FrameVerdict::Control); // ping
        assert_eq!(classify_opcode(0xA), FrameVerdict::Control); // pong
        assert_eq!(classify_opcode(0x1), FrameVerdict::Input); // text
        assert_eq!(classify_opcode(0x2), FrameVerdict::Input); // binary
        assert_eq!(classify_opcode(0x0), FrameVerdict::Input); // continuation
                                                               // An opcode we do not recognize is input, not "probably harmless".
        assert_eq!(classify_opcode(0x3), FrameVerdict::Input);
        assert_eq!(classify_opcode(0xB), FrameVerdict::Input);
    }

    /// One masked client frame, as a browser would send it.
    fn client_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mut f = vec![0x80 | opcode, 0x80 | payload.len() as u8];
        let key = [0xAA, 0xBB, 0xCC, 0xDD];
        f.extend_from_slice(&key);
        f.extend(payload.iter().enumerate().map(|(i, b)| b ^ key[i % 4]));
        f
    }

    #[test]
    fn asking_about_frame_delivery_is_not_input_and_is_never_gated() {
        // The bug this exists to pin. The page sends `config` the moment it
        // connects, a box is agent-held by default, and every text frame used
        // to be input, so the frame-rate cap was dropped on essentially every
        // session and nothing anywhere said so.
        let td = TempDir::new().unwrap();
        let config = br#"{"type":"config","maxFps":30,"pacing":"ack"}"#;
        let ack = br#"{"type":"ack","seq":41}"#;

        let mut input = Vec::new();
        input.extend_from_slice(&client_frame(0x1, config));
        input.extend_from_slice(&client_frame(0x1, ack));
        input.extend_from_slice(&client_frame(0x1, br#"{"type":"input_mouse","x":1}"#));

        let mut forwarded = Vec::new();
        let pumped = pump_input(&input[..], &mut forwarded, td.path());

        let mut expected = client_frame(0x1, config);
        expected.extend_from_slice(&client_frame(0x1, ack));
        assert_eq!(forwarded, expected, "pacing passes, the click does not");
        // And neither is counted as a human driving the box. Only the click
        // would have been, and it was dropped.
        assert_eq!(pumped.input_frames, 0);
    }

    #[test]
    fn the_pacing_allowlist_is_two_exact_names_and_nothing_near_them() {
        let whole = |p: &[u8]| classify(0x1, true, p);
        assert_eq!(
            whole(br#"{"type":"config","maxFps":10}"#),
            FrameVerdict::Pacing
        );
        assert_eq!(whole(br#"{"type":"ack","seq":1}"#), FrameVerdict::Pacing);

        // Everything else is input, including things that merely mention the
        // allowed names. An allowlist that matches substrings is not one.
        assert_eq!(
            whole(br#"{"type":"input_keyboard","key":"config"}"#),
            FrameVerdict::Input
        );
        assert_eq!(whole(br#"{"type":"configure"}"#), FrameVerdict::Input);
        assert_eq!(whole(br#"{"type":"input_mouse"}"#), FrameVerdict::Input);
        assert_eq!(whole(b"not json"), FrameVerdict::Input);
        assert_eq!(whole(b""), FrameVerdict::Input);
        assert_eq!(whole(br#"{"no":"type"}"#), FrameVerdict::Input);
        // A null or non-string type must not panic its way past the gate.
        assert_eq!(whole(br#"{"type":null}"#), FrameVerdict::Input);
        assert_eq!(whole(br#"{"type":7}"#), FrameVerdict::Input);

        // Binary is never pacing, whatever it contains.
        assert_eq!(
            classify(0x2, true, br#"{"type":"ack"}"#),
            FrameVerdict::Input
        );
        // Nor is a fragment: the rest of the message has not been seen, and a
        // judgement on a prefix is a judgement on nothing.
        assert_eq!(
            classify(0x1, false, br#"{"type":"ack"}"#),
            FrameVerdict::Input
        );
        assert_eq!(
            classify(0x0, true, br#"{"type":"ack"}"#),
            FrameVerdict::Input
        );

        // A payload too large to be a pacing message is not parsed at all.
        let mut fat = br#"{"type":"ack","pad":""#.to_vec();
        fat.resize(MAX_PACING_FRAME + 64, b'x');
        fat.extend_from_slice(br#""}"#);
        assert_eq!(whole(&fat), FrameVerdict::Input);

        // And control frames are still control, whatever they carry.
        assert_eq!(classify(0x9, true, b"ping"), FrameVerdict::Control);
    }

    #[test]
    fn input_is_dropped_while_the_agent_holds_control() {
        let td = TempDir::new().unwrap();
        // A fresh box is agent-held.
        let mut input = Vec::new();
        input.extend_from_slice(&client_frame(0x1, b"click"));
        input.extend_from_slice(&client_frame(0x9, b"ping"));

        let mut forwarded = Vec::new();
        let pumped = pump_input(&input[..], &mut forwarded, td.path());

        // The ping is forwarded. Dropping it would stall the socket and kill a
        // viewer that is only watching.
        assert_eq!(forwarded, client_frame(0x9, b"ping"));
        // And nothing a human typed reached the page, which is what the
        // session record reports.
        assert_eq!(pumped.input_frames, 0);
    }

    /// A fragmented message must be forwarded or dropped whole. Deciding per
    /// frame left agent-browser's parser mid-message when the control lock
    /// changed hands, and the next frame it saw was a protocol violation.
    #[test]
    fn a_fragmented_message_is_forwarded_or_dropped_whole() {
        // Non-final text frame, then a final continuation (opcode 0).
        let frag = |opcode: u8, payload: &[u8], fin: bool| -> Vec<u8> {
            let mut f = vec![
                if fin { 0x80 | opcode } else { opcode },
                0x80 | payload.len() as u8,
            ];
            let key = [0xAA, 0xBB, 0xCC, 0xDD];
            f.extend_from_slice(&key);
            f.extend(payload.iter().enumerate().map(|(i, b)| b ^ key[i % 4]));
            f
        };

        // Agent-held: the whole message is dropped, continuation included.
        let td = TempDir::new().unwrap();
        let mut input = Vec::new();
        input.extend_from_slice(&frag(0x1, b"cli", false));
        input.extend_from_slice(&frag(0x0, b"ck", true));
        let mut forwarded = Vec::new();
        pump_input(&input[..], &mut forwarded, td.path());
        assert!(forwarded.is_empty(), "no fragment may leak: {forwarded:?}");

        // Human-held: the whole message goes.
        let td2 = TempDir::new().unwrap();
        crate::control::take(td2.path()).unwrap();
        let mut forwarded2 = Vec::new();
        pump_input(&input[..], &mut forwarded2, td2.path());
        assert_eq!(forwarded2, input, "both fragments must be forwarded");
    }

    /// A control frame may be interleaved inside a fragmented message; it
    /// carries its own verdict and must not disturb the message's.
    #[test]
    fn an_interleaved_control_frame_does_not_break_fragment_tracking() {
        let td = TempDir::new().unwrap();
        crate::control::take(td.path()).unwrap();
        let frag = |opcode: u8, payload: &[u8], fin: bool| -> Vec<u8> {
            let mut f = vec![
                if fin { 0x80 | opcode } else { opcode },
                0x80 | payload.len() as u8,
            ];
            let key = [0xAA, 0xBB, 0xCC, 0xDD];
            f.extend_from_slice(&key);
            f.extend(payload.iter().enumerate().map(|(i, b)| b ^ key[i % 4]));
            f
        };
        let mut input = Vec::new();
        input.extend_from_slice(&frag(0x1, b"cli", false));
        input.extend_from_slice(&client_frame(0x9, b"ping"));
        input.extend_from_slice(&frag(0x0, b"ck", true));
        let mut forwarded = Vec::new();
        pump_input(&input[..], &mut forwarded, td.path());
        assert_eq!(
            forwarded, input,
            "everything goes when the human holds control"
        );
    }

    #[test]
    fn input_flows_once_the_human_takes_control() {
        let td = TempDir::new().unwrap();
        crate::control::take(td.path()).unwrap();

        let input = client_frame(0x1, b"click");
        let mut forwarded = Vec::new();
        let pumped = pump_input(&input[..], &mut forwarded, td.path());
        assert_eq!(
            forwarded, input,
            "the holder's input passes through byte-identical"
        );
        // Counted, because this is how the export learns a human drove.
        assert_eq!(pumped.input_frames, 1);

        // And handing control back stops it again, in the same session. The
        // count from the earlier stretch is what survives into the record, so
        // a take-then-release is still reported as a human having driven.
        crate::control::release(td.path()).unwrap();
        let mut after = Vec::new();
        assert_eq!(
            pump_input(&input[..], &mut after, td.path()).input_frames,
            0
        );
        assert!(after.is_empty());
    }

    #[test]
    fn a_long_frame_is_read_whole_and_an_absurd_one_is_refused() {
        // 16-bit extended length: the header grows and the parser must still
        // hand back the exact bytes, or a forwarded frame is corrupt.
        let payload = vec![b'x'; 300];
        let mut f = vec![0x80 | 0x1, 0x80 | 126];
        f.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        let key = [1, 2, 3, 4];
        f.extend_from_slice(&key);
        f.extend(payload.iter().enumerate().map(|(i, b)| b ^ key[i % 4]));

        let td = TempDir::new().unwrap();
        crate::control::take(td.path()).unwrap();
        let mut forwarded = Vec::new();
        assert_eq!(
            pump_input(&f[..], &mut forwarded, td.path()).input_frames,
            1
        );
        assert_eq!(forwarded, f);

        // A 64-bit length beyond the cap must not become an allocation.
        let mut huge = vec![0x80 | 0x1, 0x80 | 127];
        huge.extend_from_slice(&u64::MAX.to_be_bytes());
        let mut out = Vec::new();
        assert!(pump_input(&huge[..], &mut out, td.path()).error.is_some());
    }

    #[test]
    fn input_already_forwarded_survives_an_untidy_disconnect() {
        let td = TempDir::new().unwrap();
        crate::control::take(td.path()).unwrap();

        // A human drives, then the connection dies mid-frame. A closed tab, a
        // dropped link. The count must survive: recording that session as
        // "nobody touched the box" would be a false statement in the export,
        // and it is the exact shape a `Result<u64>` return would produce.
        let mut stream = client_frame(0x1, b"click");
        stream.extend_from_slice(&client_frame(0x1, b"type"));
        stream.extend_from_slice(&[0x81, 0x85, 0x01]); // truncated header

        let mut forwarded = Vec::new();
        let pumped = pump_input(&stream[..], &mut forwarded, td.path());
        assert_eq!(
            pumped.input_frames, 2,
            "the two complete frames still count"
        );
        assert!(
            pumped.error.is_some(),
            "and the untidy end is still reported"
        );
    }

    #[test]
    fn the_stream_port_is_read_from_the_boxs_own_scratch() {
        let td = TempDir::new().unwrap();
        assert_eq!(stream_port(td.path()), None);
        let dir = td.path().join("tmp").join("agent-browser");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("default.stream"), "37505\n").unwrap();
        assert_eq!(stream_port(td.path()), Some(37505));
    }

    #[test]
    fn the_web_viewer_speaks_the_message_names_the_box_dispatches() {
        // This shipped broken. The page sent `mousedown` / `keydown`, the DOM
        // event names, and agent-browser's stream server dispatches only on
        // `input_mouse`, `input_keyboard` and `input_touch`, falling through to
        // a no-op for anything else. Nothing reports it: the socket stays open,
        // frames keep arriving, the forward counts the input frames as
        // forwarded, and the page never moves. A human taking control saw a
        // working viewer that ignored them.
        let page = viewer_page(crate::control::Holder::Agent);
        assert!(
            page.contains("input_mouse"),
            "the mouse path must use input_mouse"
        );
        assert!(
            page.contains("input_keyboard"),
            "the key path must use input_keyboard"
        );
        for dom_name in [
            "\"mousedown\"",
            "\"mouseup\"",
            "\"keydown\"",
            "\"keyup\"",
            "\"wheel\"",
        ] {
            assert!(
                !page.contains(&format!("type: {dom_name}")),
                "a DOM event name is being sent as a message type: {dom_name}"
            );
        }
        // The fields CDP will not do without: a *named* button, and a click
        // count, because a press with zero is not a click.
        assert!(
            page.contains("clickCount"),
            "{}",
            "a press needs a click count"
        );
        assert!(page.contains("mousePressed") && page.contains("mouseReleased"));
        assert!(page.contains("windowsVirtualKeyCode"));
    }

    #[test]
    fn the_web_viewer_decodes_off_the_main_thread_and_paints_the_newest_frame() {
        // Both halves of what makes the page smooth, pinned because the page is
        // a string in this file and nothing else compiles it. Decoding through
        // a `data:` URL and an `Image` element put a 70 KB string parse and a
        // decode on the main thread for every frame; painting on arrival queued
        // frames a slow tab could never catch up with.
        let page = viewer_page(crate::control::Holder::Agent);
        assert!(
            page.contains("createImageBitmap"),
            "frames must decode off the main thread"
        );
        assert!(
            page.contains("requestAnimationFrame"),
            "frames must paint on a frame boundary"
        );
        assert!(
            !page.contains("data:image/jpeg;base64,"),
            "a data URL per frame is the shape this replaced"
        );
        // Decoded pixels are held by the bitmap; a session that never closes
        // them grows without bound.
        assert!(page.contains(".close()"), "decoded frames must be released");

        // Dropping has to happen before the decode as well as after it. The box
        // does not wait for this page, so a decode started per arriving frame
        // queues jobs and their blobs for as long as decoding is slower than the
        // frame rate, and that version still *looks* like it drops frames,
        // because it does, one stage too late to matter.
        assert!(
            page.contains("queuedBytes") && page.contains("decoding"),
            "only the newest undecoded frame may be held, and one decode at a time"
        );
    }

    #[test]
    fn the_pages_control_display_is_stamped_with_the_state_it_can_actually_know() {
        // There is no channel for h5i to update the page, so the value is
        // stamped at serve time and the page says so. A display that silently
        // never changed would read as "taking the lock did not work".
        let agent = viewer_page(crate::control::Holder::Agent);
        assert!(
            !agent.contains("__H5I_HOLDER__"),
            "the placeholder survived"
        );
        assert!(agent.contains(">agent</span>"), "holder not stamped");

        let human = viewer_page(crate::control::Holder::Human);
        assert!(human.contains(">human</span>"));
        assert!(
            human.contains("control at page load"),
            "and it says what it knows"
        );
    }

    /// A host session advertises one port in one file.
    #[test]
    fn a_host_sessions_stream_port_is_read_from_the_file_the_engine_wrote() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("stream");
        std::fs::write(&path, "45123\n").expect("write");
        assert_eq!(session_stream_port(&path), Some(45123));
    }

    /// Port 0 means the file was written before the engine bound.
    #[test]
    fn a_stream_file_written_before_the_engine_bound_is_not_an_address() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("stream");
        std::fs::write(&path, "0").expect("write");
        assert_eq!(session_stream_port(&path), None);

        std::fs::write(&path, "not a port").expect("write");
        assert_eq!(session_stream_port(&path), None);
        assert_eq!(session_stream_port(&dir.path().join("absent")), None);
    }

    /// Why the two arms are a type: a host session must not be shown a boxed
    /// session's chrome.
    #[test]
    fn only_a_boxed_route_claims_a_boundary_outside_the_engine() {
        assert!(!Route::Host { port: 1 }.is_boxed());
        assert!(
            Route::Boxed {
                pid: 1,
                pid_ns: std::ffi::OsString::from("net:[4026531840]"),
                port: 1,
            }
            .is_boxed()
        );
    }

    /// The stale-pid case, made deterministic: hand the connect the namespace
    /// this process is already in, which is what a reissued pid looks like from
    /// here. The answer has to be a refusal, because what follows otherwise is
    /// `TcpStream::connect(("127.0.0.1", port))` in *this* namespace, with the
    /// port read out of `<env>/tmp`, one of the two paths the box can write.
    ///
    /// Unprivileged, the kernel refuses the `setns` that would get there anyway;
    /// run as root it does not, and either way this says no before the fork.
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn the_viewer_refuses_to_connect_in_its_own_network_namespace() {
        let own = std::fs::read_link("/proc/self/ns/net").expect("a netns link");
        // Port 9 is discard; nothing should listen, and nothing should get far
        // enough to find out.
        let err = connect_in_netns(std::process::id(), 9, own.as_os_str())
            .expect_err("a connect inside our own namespace must be refused");
        let text = format!("{err}");
        assert!(text.contains("not a box"), "{text}");
        assert!(text.contains("fail-closed"), "{text}");
    }

    /// `box_pid` is the same walk as `box_pid_ns` with the namespace dropped.
    /// Asserted so a later edit cannot give the two different answers, which
    /// would mean the identity checked at connect time is not the identity the
    /// pid was chosen by.
    #[test]
    fn the_pid_and_the_namespace_come_out_of_one_walk() {
        let td = TempDir::new().unwrap();
        assert_eq!(box_pid(td.path()), box_pid_ns(td.path()).map(|(p, _)| p));
    }

    #[test]
    fn a_box_that_is_not_running_has_no_pid_to_enter() {
        let td = TempDir::new().unwrap();
        assert_eq!(box_pid(td.path()), None);
        // A registry entry for a dead process is not a running box.
        let live = td.path().join("live");
        std::fs::create_dir_all(&live).unwrap();
        std::fs::write(
            live.join("999999.json"),
            br#"{"pid":999999,"kind":"shell","started_at":"2026-08-05T00:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(box_pid(td.path()), None);
    }
}
