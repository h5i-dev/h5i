//! The viewer forward: the whole trusted surface between a human and a box.
//!
//! agent-browser's stream server assumes a friendly localhost — connect to the
//! WebSocket and you can both watch the viewport and type into it. Inside the
//! box that assumption holds, because nothing else is in there. On a developer
//! machine with a browser on it, it does not: any page the human happens to have
//! open could reach a port bound on loopback.
//!
//! So the port is never published. It stays inside the box's private network
//! namespace, and `h5i dev view` runs a forward the **host** owns, with four
//! properties (roadmap 5.9):
//!
//! * **Loopback only, on a port h5i chose.** Nothing binds an external address.
//! * **A per-box token on every connection.** Minted at box creation and never
//!   written anywhere the box can read, so a compromised agent cannot mint
//!   itself a viewer.
//! * **Cross-origin handshakes refused.** A page the human has open in another
//!   tab cannot open a WebSocket to a running box.
//! * **The control lock on the input direction.** Frames flow *out* always —
//!   watching never collides. Frames flow *in* only while the human holds the
//!   lock ([`crate::control`]).
//!
//! Crossing into the namespace is the one genuinely awkward part, and it is
//! deliberately done the way the supervisor already does it: h5i is the parent,
//! so it can enter the box's user and network namespaces by pid, connect from
//! inside, and hand the socket back out over `SCM_RIGHTS`. Nothing is punched
//! through the namespace, and the box gains no reachability it did not have.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};

use crate::error::H5iError;

/// The per-box viewer token, relative to an env directory.
///
/// A sibling of the receipt log, deliberately **not** under `<env>/spool` or
/// `<env>/tmp` — the two paths a box can write. A token the box could read is a
/// token the agent could hand to anything it can reach.
const TOKEN_FILE: &str = "view-token";

/// 128 bits, hex. Long enough that guessing is not a strategy against a
/// loopback listener that exists for the life of one session.
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
    let token: String = (0..TOKEN_BYTES)
        .map(|_| format!("{:02x}", fastrand::u8(..)))
        .collect();
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
/// loopback listener, so a timing oracle is a stretch — but the cost of not
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

/// The port agent-browser's stream server is listening on **inside** the box.
///
/// The daemon writes it to `<session>.stream` next to its socket, and that
/// directory is the box's private `/tmp`, backed by `<env>/tmp` — so the port is
/// readable from the host without asking the box anything.
pub fn stream_port(env_dir: &Path) -> Option<u16> {
    let dir = env_dir.join("tmp").join("agent-browser");
    let mut ports: Vec<u16> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".stream"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok()?.trim().parse().ok())
        .collect();
    ports.sort_unstable();
    ports.into_iter().next()
}

/// A pid living **inside** the box's namespaces.
///
/// The live registry is the starting point, not the answer: it records the
/// host-side `h5i` process that owns the session, and that process is in the
/// *host's* network namespace. Its descendants are the box. So this walks the
/// session's process tree and returns the first descendant whose network
/// namespace differs from ours, which is by definition inside the box.
///
/// Getting this wrong is quiet rather than loud — entering the host's own netns
/// succeeds, and the forward then finds nothing listening — so it is worth
/// doing by observation rather than by assuming which pid means what.
pub fn box_pid(env_dir: &Path) -> Option<u32> {
    let session = session_pid(env_dir)?;
    let own_netns = std::fs::read_link("/proc/self/ns/net").ok()?;
    let children = child_map();

    // Breadth-first: the box's outermost process is the one whose namespaces
    // enclose everything else in it, so the shallowest match is the right one.
    let mut queue = std::collections::VecDeque::from([session]);
    let mut seen = std::collections::HashSet::from([session]);
    while let Some(pid) = queue.pop_front() {
        if let Ok(ns) = std::fs::read_link(format!("/proc/{pid}/ns/net")) {
            if ns != own_netns {
                return Some(pid);
            }
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
fn session_pid(env_dir: &Path) -> Option<u32> {
    let mut best: Option<(String, u32)> = None;
    for e in std::fs::read_dir(env_dir.join("live")).ok()?.flatten() {
        let Ok(text) = std::fs::read_to_string(e.path()) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let (Some(pid), Some(kind)) = (
            v["pid"].as_u64().and_then(|p| u32::try_from(p).ok()),
            v["kind"].as_str(),
        ) else {
            continue;
        };
        if !crate::env::live_is_writer(kind) {
            continue;
        }
        if !pid_alive(pid) {
            continue;
        }
        let started = v["started_at"].as_str().unwrap_or("").to_string();
        if best.as_ref().is_none_or(|(s, _)| *s <= started) {
            best = Some((started, pid));
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

/// Connect to `port` on loopback **inside** the namespaces of `pid`.
///
/// Done in a forked child, and it has to be: joining a user namespace is
/// refused for a multi-threaded process, and `setns` on a network namespace
/// rebinds the calling thread rather than the process. The child enters both,
/// connects, and passes the connected socket back over `SCM_RIGHTS` — the same
/// fd-handoff the supervisor uses for the seccomp listener.
///
/// The user namespace comes first and is not optional: the box's netns was
/// created by an unprivileged `unshare`, so it is owned by that userns and
/// joining it requires being in it.
#[cfg(target_os = "linux")]
pub fn connect_in_netns(pid: u32, port: u16) -> Result<TcpStream, H5iError> {
    use h5i_sandbox::seccomp_notify::recv_fd;
    use std::os::fd::FromRawFd;

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
        let code = enter_and_connect(pid, port, child_end);
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
    // the kind of message that cost a session on the browser daemon — so the
    // child reports *which* step failed through its exit status, and that is
    // what the operator sees.
    let code = if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        -1
    };
    Err(H5iError::Metadata(match code {
        EXIT_NO_NS => format!(
            "the box (pid {pid}) is gone — its namespaces no longer exist. \
             A box only has them while a session is running; start one with \
             `h5i dev shell <name>`."
        ),
        // Deliberately no errno here: the failing `setns` happened in the
        // forked child, so the parent's `last_os_error` describes an unrelated
        // syscall. A wrong errno is worse than none.
        EXIT_SETNS => format!(
            "could not enter the box's network namespace (pid {pid}). \
             Joining it needs the user namespace that created it, so run `h5i dev view` \
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
const EXIT_NO_NS: i32 = 2;
const EXIT_SETNS: i32 = 3;
const EXIT_CONNECT: i32 = 4;

/// The child half of [`connect_in_netns`]. Returns the exit code the parent
/// turns back into a message, since a forked child cannot safely format one.
#[cfg(target_os = "linux")]
fn enter_and_connect(pid: u32, port: u16, sock: i32) -> i32 {
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

    let Ok(stream) = TcpStream::connect(("127.0.0.1", port)) else {
        return EXIT_CONNECT;
    };
    if unsafe { send_fd(sock, stream.as_raw_fd()) }.is_err() {
        return 1;
    }
    0
}

#[cfg(not(target_os = "linux"))]
pub fn connect_in_netns(_pid: u32, _port: u16) -> Result<TcpStream, H5iError> {
    Err(H5iError::Metadata(
        "the viewer forward needs Linux namespaces; it is not available on this platform".into(),
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
/// no `Origin` at all is allowed — that is a command-line client (`websocat`, a
/// test), not a web page, and browsers always send one. What is refused is a
/// *different* origin, which is precisely a page the human has open elsewhere.
pub fn gate(req: &Request, expected_token: &str, self_origin: &str) -> Result<(), Refusal> {
    match &req.token {
        Some(t) if token_matches(expected_token, t) => {}
        _ => return Err(Refusal::BadToken),
    }
    if let Some(origin) = &req.origin {
        if origin != self_origin {
            return Err(Refusal::CrossOrigin);
        }
    }
    Ok(())
}

// ─── WebSocket framing, only as far as the lock needs ───────────────────────

/// What to do with one client→box frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameVerdict {
    /// Connection management: forwarded regardless of who holds the lock, or
    /// the socket would stall and time out while a human watches.
    Control,
    /// Actual input — a click, a keystroke. Forwarded only for the lock holder.
    Input,
}

/// Classify a frame by its opcode.
///
/// This is the whole of the WebSocket protocol the forward needs to understand.
/// It does not decode payloads, reassemble fragments or care what an input frame
/// says: the lock decides by *direction and kind*, so a new message type
/// upstream is governed correctly the day it is added rather than the day we
/// notice it.
pub fn classify_opcode(opcode: u8) -> FrameVerdict {
    match opcode {
        // close, ping, pong — the whole control-opcode range
        0x8..=0xA => FrameVerdict::Control,
        // continuation, text, binary — and anything reserved, which is treated
        // as input because guessing "harmless" about an unknown frame is how
        // gates get bypassed.
        _ => FrameVerdict::Input,
    }
}

/// Read one WebSocket frame from `r`, returning its opcode and the exact bytes
/// as they arrived (so a forwarded frame is byte-identical to the original).
///
/// `None` at a clean end of stream; an error for a truncated or absurd frame.
fn read_frame(r: &mut impl Read) -> std::io::Result<Option<(u8, Vec<u8>)>> {
    let mut head = [0u8; 2];
    match r.read_exact(&mut head) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let mut raw = head.to_vec();
    let opcode = head[0] & 0x0f;
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
    if masked {
        let mut key = [0u8; 4];
        r.read_exact(&mut key)?;
        raw.extend_from_slice(&key);
    }
    let mut payload = vec![0u8; payload_len as usize];
    r.read_exact(&mut payload)?;
    raw.extend_from_slice(&payload);
    Ok(Some((opcode, raw)))
}

/// Cap on a single inbound frame. Input events are tiny; this only has to be
/// larger than any of them and small enough that a hostile client cannot make
/// the forward allocate for free.
const MAX_FRAME: u64 = 1 << 20;

/// Copy client→box, dropping input frames while the agent holds the lock.
///
/// Dropping rather than closing is deliberate. A human who clicks before taking
/// control should see nothing happen and still have a live viewer, not a
/// connection that died on them.
///
/// Returns how many **input** frames actually reached the page. That count is
/// the honest answer to "did a human drive this box?", and it is worth
/// returning rather than inferring: comparing who held the lock at open and at
/// close misses the ordinary case of someone taking control, doing the thing,
/// and handing it straight back.
pub fn pump_input(
    mut from_client: impl Read,
    mut to_box: impl Write,
    env_dir: &Path,
) -> Pump {
    let mut pump = Pump::default();
    loop {
        match read_frame(&mut from_client) {
            Ok(None) => return pump,
            Err(e) => {
                pump.error = Some(e.to_string());
                return pump;
            }
            Ok(Some((opcode, raw))) => {
                let is_input = classify_opcode(opcode) == FrameVerdict::Input;
                let allowed = !is_input
                    || crate::control::read(env_dir).holder == crate::control::Holder::Human;
                if allowed {
                    if let Err(e) = to_box.write_all(&raw).and_then(|()| to_box.flush()) {
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
/// The count and the error are returned **together**, and that is the whole
/// point of the type. Returning `Result<u64>` loses the count on the error path,
/// which is not a cosmetic loss: a human who takes control, clicks through a
/// form and then closes the tab abruptly produces exactly that path, and the
/// export would record them as never having touched the box. Evidence must not
/// be discarded because a connection ended untidily.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Pump {
    /// Input frames that actually reached the page — so, frames sent while the
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
    pid: u32,
    stream_port: u16,
}

impl Forward {
    /// Bind on loopback and prepare to serve this box.
    ///
    /// Everything that can be known before a connection arrives is resolved
    /// here, so `h5i dev view` fails with an explanation instead of printing a
    /// URL that will not work.
    pub fn bind(
        env_dir: &Path,
        env_id: &str,
        policy_digest: &str,
        port: u16,
    ) -> Result<Forward, H5iError> {
        let token = read_token(env_dir).ok_or_else(|| {
            H5iError::Metadata(
                "this box has no viewer token — it was created before the viewer existed. \
                 Create a new box to get one."
                    .into(),
            )
        })?;
        let pid = box_pid(env_dir).ok_or_else(|| {
            H5iError::Metadata(
                "this box is not running, so there is no browser to watch. Start a session \
                 (`h5i dev shell <name>`) and try again."
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
        // Loopback only. Never an external address, on any code path.
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        Ok(Forward {
            listener,
            env_dir: env_dir.to_path_buf(),
            env_id: env_id.to_string(),
            policy_digest: policy_digest.to_string(),
            token,
            pid,
            stream_port,
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
            let _ = respond_html(&mut client, &viewer_page());
            return Ok(());
        }

        // Enter the box, connect, and relay the handshake verbatim so
        // agent-browser negotiates with the client directly (we never have to
        // know its subprotocol or its Sec-WebSocket-Accept).
        let mut upstream = connect_in_netns(self.pid, self.stream_port)?;
        upstream.write_all(rewrite_head_for_upstream(&head).as_bytes())?;
        upstream.flush()?;

        let mut client_out = client.try_clone()?;
        let mut upstream_out = upstream.try_clone()?;
        let env_dir = self.env_dir.clone();

        let opened = chrono::Utc::now();
        let holder_at_open = crate::control::read(&env_dir).holder;

        // Out: frames to the human, unconditionally. Watching never collides,
        // so this direction has no policy at all.
        let out = std::thread::spawn(move || {
            std::io::copy(&mut upstream, &mut client_out).unwrap_or(0)
        });
        // In: gated by the control lock.
        let pumped = pump_input(&mut client, &mut upstream_out, &env_dir);
        // Closing our end unblocks the outbound copy so the thread can finish.
        let _ = upstream_out.shutdown(std::net::Shutdown::Both);
        let bytes_out = out.join().unwrap_or(0);

        self.record_session(opened, holder_at_open, bytes_out, &pumped);
        Ok(())
    }

    /// Record the viewer session in the box's receipt log, so it reaches the
    /// export like any other observation.
    ///
    /// This lane is **host observed** in the strongest sense available: h5i owns
    /// the forward, so the box does not supply any of it and cannot suppress it.
    /// What it answers is the question an export otherwise cannot: was a human
    /// watching, did they take the controls, and for how long. A patch produced
    /// with a human driving is a different artifact from one an agent produced
    /// alone, and the receipt should not be silent about which it is.
    fn record_session(
        &self,
        opened: chrono::DateTime<chrono::Utc>,
        holder_at_open: crate::control::Holder,
        bytes_out: u64,
        pumped: &Pump,
    ) {
        let input_frames = pumped.input_frames;
        let closed = chrono::Utc::now();
        let holder_at_close = crate::control::read(&self.env_dir).holder;
        // Ground truth, not inference: input frames were forwarded, which the
        // forward only does for the lock holder. Comparing the holder at open
        // and close would miss the ordinary shape — take control, do the thing,
        // hand it back — which is exactly the session a reviewer most wants
        // flagged.
        let took_control = input_frames > 0;
        let seconds = (closed - opened).num_seconds().max(0);

        let mut body = String::new();
        body.push_str(&format!("viewer session, {seconds}s\n"));
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
        body.push_str(&format!("frames   {bytes_out} bytes streamed to the viewer\n"));
        body.push_str(&format!(
            "input    {input_frames} frame(s) forwarded to the page\n"
        ));
        if let Some(err) = &pumped.error {
            body.push_str(&format!("note     the connection ended abruptly: {err}\n"));
        }

        let input = crate::receipt::RecordInput {
            env_id: self.env_id.clone(),
            policy_digest: Some(self.policy_digest.clone()),
            // Its own lane: neither a command the box ran nor anything the box
            // claimed, but something h5i itself did on the human's behalf.
            source: "viewer".into(),
            cmd: Some(format!(
                "h5i dev view ({}, {seconds}s)",
                if took_control {
                    "human took control"
                } else {
                    holder_at_close.as_str()
                }
            )),
            wall_ms: u64::try_from(seconds * 1000).ok(),
            ..Default::default()
        };
        if let Err(e) = crate::receipt::append(&self.env_dir, input, body.as_bytes()) {
            eprintln!("viewer: could not record the session: {e}");
        }
    }
}

/// Read up to the end of the HTTP headers.
fn read_head(s: &mut TcpStream) -> std::io::Result<String> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    // Bounded: a header block this large is a client we do not want to serve.
    while buf.len() < 16 * 1024 {
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
    // the beginning of the client's first WebSocket frame — after which the
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
///
/// Deliberately one self-contained file with no external asset, because it is
/// served by a security boundary and every byte it can fetch is a byte someone
/// has to reason about.
fn viewer_page() -> String {
    include_str!("viewer.html").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

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
        let req = |q: &str| parse_request(&format!("GET /{q} HTTP/1.1\r\nHost: x\r\n\r\n")).unwrap();
        let origin = "http://127.0.0.1:9000";

        assert_eq!(gate(&req("?token=good"), "good", origin), Ok(()));
        assert_eq!(gate(&req(""), "good", origin), Err(Refusal::BadToken));
        assert_eq!(gate(&req("?token=bad"), "good", origin), Err(Refusal::BadToken));
        // A prefix must not pass: the comparison is length-checked first.
        assert_eq!(gate(&req("?token=goo"), "good", origin), Err(Refusal::BadToken));
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
        // A page the human happens to have open — the whole reason this check
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
        // matters most. An extra CRLF is not rejected by the server — it is
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
    fn input_is_dropped_while_the_agent_holds_control() {
        let td = TempDir::new().unwrap();
        // A fresh box is agent-held.
        let mut input = Vec::new();
        input.extend_from_slice(&client_frame(0x1, b"click"));
        input.extend_from_slice(&client_frame(0x9, b"ping"));

        let mut forwarded = Vec::new();
        let pumped = pump_input(&input[..], &mut forwarded, td.path());

        // The ping is forwarded — dropping it would stall the socket and kill a
        // viewer that is only watching.
        assert_eq!(forwarded, client_frame(0x9, b"ping"));
        // And nothing a human typed reached the page, which is what the
        // session record reports.
        assert_eq!(pumped.input_frames, 0);
    }

    #[test]
    fn input_flows_once_the_human_takes_control() {
        let td = TempDir::new().unwrap();
        crate::control::take(td.path()).unwrap();

        let input = client_frame(0x1, b"click");
        let mut forwarded = Vec::new();
        let pumped = pump_input(&input[..], &mut forwarded, td.path());
        assert_eq!(forwarded, input, "the holder's input passes through byte-identical");
        // Counted, because this is how the export learns a human drove.
        assert_eq!(pumped.input_frames, 1);

        // And handing control back stops it again, in the same session. The
        // count from the earlier stretch is what survives into the record, so
        // a take-then-release is still reported as a human having driven.
        crate::control::release(td.path()).unwrap();
        let mut after = Vec::new();
        assert_eq!(pump_input(&input[..], &mut after, td.path()).input_frames, 0);
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
        assert_eq!(pump_input(&f[..], &mut forwarded, td.path()).input_frames, 1);
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

        // A human drives, then the connection dies mid-frame — a closed tab, a
        // dropped link. The count must survive: recording that session as
        // "nobody touched the box" would be a false statement in the export,
        // and it is the exact shape a `Result<u64>` return would produce.
        let mut stream = client_frame(0x1, b"click");
        stream.extend_from_slice(&client_frame(0x1, b"type"));
        stream.extend_from_slice(&[0x81, 0x85, 0x01]); // truncated header

        let mut forwarded = Vec::new();
        let pumped = pump_input(&stream[..], &mut forwarded, td.path());
        assert_eq!(pumped.input_frames, 2, "the two complete frames still count");
        assert!(pumped.error.is_some(), "and the untidy end is still reported");
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
