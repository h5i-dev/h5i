//! How the frames get there.
//!
//! A [`Transport`] is a *factory*, not a connection: `connect()` opens one
//! [`Channel`] and one channel carries one RPC (ROADMAP.md R4). That is the
//! whole reason this protocol needs no request ids, no channel numbers and no
//! multiplexer. Concurrency is several channels, and over SSH those are
//! several sessions on one TCP connection, multiplexed by OpenSSH's
//! ControlMaster in battle-tested C rather than by us.
//!
//! Both implementations are the same mechanism: spawn a child, speak frames on
//! its stdin and stdout, read its stderr for diagnosis. [`SshTransport`] puts
//! `ssh` in front of the worker; [`ChildProcessTransport`] runs the worker
//! directly, which is what makes the whole protocol testable in CI with no sshd
//! anywhere. Because the difference is only argv, the argv is built by pure
//! functions that a test can read without spawning anything. The same
//! discipline `container::build_run_argv` follows.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use thiserror::Error;

/// How much of a channel's stderr to keep. The tail, not the head: a failure is
/// at the end of a log.
const STDERR_TAIL: usize = 64 * 1024;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("could not start `{program}`: {source}{}", hint(.program, .source))]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },

    #[error("{what} exited with {status}{}", tail(.stderr))]
    Exit {
        what: String,
        status: String,
        stderr: String,
    },

    #[error("{what} did not answer within {}s — killed it", .after.as_secs())]
    Deadline { what: String, after: Duration },

    #[error("runner transport I/O: {0}")]
    Io(#[from] std::io::Error),
}

fn hint(program: &str, source: &std::io::Error) -> String {
    if source.kind() == std::io::ErrorKind::NotFound {
        format!(" — `{program}` is not on this machine's PATH")
    } else {
        String::new()
    }
}

fn tail(stderr: &str) -> String {
    let t = stderr.trim();
    if t.is_empty() {
        String::new()
    } else {
        format!(":\n{t}")
    }
}

/// Opens channels to one runner.
pub trait Transport: Send + Sync {
    /// One channel, one RPC.
    fn connect(&self) -> Result<Channel, TransportError>;
    /// What to call this in an error message.
    fn describe(&self) -> String;
}

/// One RPC's worth of connection: a child process we speak frames to.
pub struct Channel {
    what: String,
    child: Arc<Mutex<Child>>,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: Arc<Mutex<Vec<u8>>>,
    /// The thread draining stderr, until someone waits for it.
    ///
    /// Load-bearing: a child can write its diagnosis and exit while this thread
    /// is still between a `read` and the buffer, so reading the buffer without
    /// waiting returns an empty string exactly when the message matters most.
    /// That is a race that passes locally and fails under load, so
    /// [`Channel::stderr_tail`] waits.
    stderr_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// Set by the watchdog when it kills the child, so a read failure that is
    /// really a timeout is reported as one rather than as "the peer went away".
    timed_out: Arc<AtomicBool>,
    watchdog: Option<Watchdog>,
    deadline: Duration,
}

impl std::fmt::Debug for Channel {
    /// Deliberately hand-written and deliberately thin: a channel holds a live
    /// child, two pipes and a captured stderr buffer, none of which belongs in
    /// a panic message. What a reader of one needs is which runner it is and
    /// whether the clock ran out.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Channel")
            .field("what", &self.what)
            .field("timed_out", &self.timed_out())
            .finish_non_exhaustive()
    }
}

impl Channel {
    /// Take the two halves, so a caller can read and write at once. From two
    /// threads, when a later milestone streams an exec. Returns `None` on a
    /// second call.
    pub fn take_io(&mut self) -> Option<(ChildStdout, ChildStdin)> {
        match (self.stdout.take(), self.stdin.take()) {
            (Some(o), Some(i)) => Some((o, i)),
            // Put back whatever was still there, so a second caller sees a
            // consistent `None` rather than half a channel.
            (o, i) => {
                self.stdout = o;
                self.stdin = i;
                None
            }
        }
    }

    /// Everything the child wrote to stderr.
    ///
    /// Waits for the drain thread the first time it is called: the thread ends
    /// when the write end of the pipe closes, so this blocks only until the
    /// child is really finished with it, and returning early would mean
    /// returning an empty diagnosis for a peer that had just explained itself.
    pub fn stderr_tail(&self) -> String {
        if let Ok(mut slot) = self.stderr_thread.lock()
            && let Some(handle) = slot.take()
        {
            let _ = handle.join();
        }
        let buf = self.stderr.lock().unwrap_or_else(|e| e.into_inner());
        String::from_utf8_lossy(&buf).to_string()
    }

    /// Close the channel: stop the clock, let the peer see end of input, and
    /// collect its exit status.
    ///
    /// Dropping the write half is what ends the exchange, the worker reads
    /// until EOF, so this must happen before the wait, or the wait is a
    /// deadlock waiting for a peer that is waiting for us.
    pub fn finish(mut self) -> Result<(), TransportError> {
        if let Some(w) = self.watchdog.take() {
            w.disarm();
        }
        drop(self.stdin.take());
        drop(self.stdout.take());

        let status = {
            let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
            child.wait()?
        };

        if self.timed_out.load(Ordering::SeqCst) {
            return Err(TransportError::Deadline {
                what: self.what.clone(),
                after: self.deadline,
            });
        }
        if !status.success() {
            return Err(TransportError::Exit {
                what: self.what.clone(),
                status: status.to_string(),
                stderr: self.stderr_tail(),
            });
        }
        Ok(())
    }

    /// Kill the child and give up on it. For the error paths, where the exit
    /// status is not interesting and hanging is the only real risk.
    pub fn abandon(mut self) {
        if let Some(w) = self.watchdog.take() {
            w.disarm();
        }
        drop(self.stdin.take());
        drop(self.stdout.take());
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Did the watchdog fire? A read that fails after this is a deadline, not a
    /// peer that went away, and the two want opposite advice in the message.
    pub fn timed_out(&self) -> bool {
        self.timed_out.load(Ordering::SeqCst)
    }

    /// Start a new clock for the next phase of this exchange.
    ///
    /// This is what makes the handshake clock and the request clock genuinely
    /// separate rather than nominally so (ROADMAP.md R5): a channel opens under
    /// a short deadline covering connect and handshake, and the caller re-arms
    /// for the longer request once the peer has proved it is there. A phase
    /// that has already timed out cannot be re-armed. The child is dead and
    /// pretending otherwise would only move the error somewhere less clear.
    pub fn rearm(&mut self, after: Duration) {
        if self.timed_out.load(Ordering::SeqCst) {
            return;
        }
        if let Some(w) = self.watchdog.take() {
            w.disarm();
        }
        self.deadline = after;
        self.watchdog = Some(Watchdog::arm(
            Arc::clone(&self.child),
            Arc::clone(&self.timed_out),
            after,
        ));
    }

    fn spawn(
        what: String,
        mut command: Command,
        program: &str,
        deadline: Duration,
    ) -> Result<Self, TransportError> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command.spawn().map_err(|source| TransportError::Spawn {
            program: program.to_string(),
            source,
        })?;

        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr_pipe = child.stderr.take();

        let child = Arc::new(Mutex::new(child));
        let stderr = Arc::new(Mutex::new(Vec::new()));

        // Drained on a thread rather than at the end. A peer that fills the
        // stderr pipe while we are blocked reading stdout would otherwise wedge
        // both of us, and "it hangs only when the worker is chatty" is a bug
        // that surfaces in production and never in a test.
        let stderr_thread = stderr_pipe.map(|mut pipe| {
            let sink = Arc::clone(&stderr);
            std::thread::spawn(move || {
                let mut chunk = [0u8; 8192];
                loop {
                    match pipe.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let mut buf = sink.lock().unwrap_or_else(|e| e.into_inner());
                            buf.extend_from_slice(&chunk[..n]);
                            if buf.len() > STDERR_TAIL {
                                let cut = buf.len() - STDERR_TAIL;
                                buf.drain(..cut);
                            }
                        }
                    }
                }
            })
        });

        let timed_out = Arc::new(AtomicBool::new(false));
        let watchdog = Watchdog::arm(
            Arc::clone(&child),
            Arc::clone(&timed_out),
            deadline,
        );

        Ok(Self {
            what,
            child,
            stdin,
            stdout,
            stderr,
            stderr_thread: Mutex::new(stderr_thread),
            timed_out,
            watchdog: Some(watchdog),
            deadline,
        })
    }
}

/// Kills a channel's child if the exchange outlives its deadline.
///
/// The clock has to live somewhere, and a pipe has no read timeout. Killing the
/// child turns "the peer stopped talking" into an ordinary end of stream, which
/// every reader above already handles, so the deadline needs no special case
/// anywhere else, only [`Channel::timed_out`] to explain it afterwards.
///
/// It kills the child, not a process group. A reader unblocks when the last
/// holder of the write end of the pipe closes it, so if the child had left a
/// grandchild holding that end, the read would keep blocking past the kill.
/// Both transports here are single-process by construction (`ssh` is one
/// process locally, and the worker is one `h5i`) so the kill is sufficient. A
/// transport that ever spawns a shell wrapper would need the child in its own
/// process group and a group-wide signal; a test that wraps one in `sh` must
/// `exec` so no wrapper survives to hold the pipe.
struct Watchdog {
    disarm: mpsc::Sender<()>,
}

impl Watchdog {
    fn arm(child: Arc<Mutex<Child>>, timed_out: Arc<AtomicBool>, after: Duration) -> Self {
        let (tx, rx) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            // Disconnected means the Channel was dropped without a disarm,
            // which is also a reason to stop waiting.
            if let Err(mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(after) {
                timed_out.store(true, Ordering::SeqCst);
                if let Ok(mut c) = child.lock() {
                    let _ = c.kill();
                }
            }
        });
        Self { disarm: tx }
    }

    fn disarm(self) {
        let _ = self.disarm.send(());
    }
}

/// How long an exchange may take before the watchdog ends it.
///
/// Three clocks, never one (ROADMAP.md R5): this is the outer one. The
/// handshake clock and the idle clock live in [`crate::client`], where there is
/// something to be idle *about*.
#[derive(Debug, Clone, Copy)]
pub struct Deadlines {
    /// Connect plus handshake: the peer proving it is there and speaks this
    /// protocol. Short, because everything it covers is either fast or broken.
    pub handshake: Duration,
    /// One request and its answer, once the peer has proved it is there. Re-armed
    /// by [`Channel::rearm`] after the handshake lands.
    pub control: Duration,
}

impl Default for Deadlines {
    fn default() -> Self {
        Self {
            // Generous, because it covers TCP setup and SSH authentication on a
            // link we know nothing about. It is a backstop against hanging
            // forever, not a latency budget.
            handshake: Duration::from_secs(30),
            control: Duration::from_secs(60),
        }
    }
}

/// Reach a runner over SSH.
#[derive(Debug, Clone)]
pub struct SshTransport {
    pub host: String,
    pub user: Option<String>,
    pub port: Option<u16>,
    /// The dedicated pair key. `IdentitiesOnly=yes` goes with it, so the user's
    /// agent cannot quietly offer a different key that happens to work.
    pub identity: PathBuf,
    /// This runner's own `known_hosts`, holding the one host key pinned at pair
    /// time. Not the user's file: pinning per runner is what makes
    /// `StrictHostKeyChecking=yes` a statement about *this* machine.
    pub known_hosts: PathBuf,
    /// Where OpenSSH keeps the multiplexing socket. `None` disables
    /// multiplexing, which is what a bulk transfer wants so it cannot block an
    /// interactive session behind it.
    pub control_path: Option<PathBuf>,
    /// The command to run remotely. A forced-command key ignores this and runs
    /// its own, which is the point; sending it anyway means the same client
    /// works against a runner whose key is not forced, and against one where it
    /// is.
    pub remote_command: String,
    pub deadlines: Deadlines,
}

impl SshTransport {
    /// The destination as `ssh` wants it.
    pub fn destination(&self) -> String {
        match &self.user {
            Some(u) => format!("{u}@{}", self.host),
            None => self.host.clone(),
        }
    }

    /// The whole argv, as a pure function of the configuration.
    ///
    /// Every option here is load-bearing, and none of it comes from the user's
    /// `~/.ssh/config`, because a runner's security property must not depend on
    /// a file this code did not write.
    ///
    /// That last sentence used to be false, which is why it is now enforced by
    /// `-F /dev/null` rather than asserted by a comment. Passing `-o` does not
    /// stop ssh reading the config: it stops the config winning *for the
    /// options named*. `GlobalKnownHostsFile` was not named, ssh consults both
    /// host-key files, and a `~/.ssh/config` setting it alongside a
    /// `ProxyCommand` sent every RPC to another machine with
    /// `StrictHostKeyChecking=yes` and the pinned `UserKnownHostsFile` both
    /// still in force, while `runner list` went on showing the honestly paired
    /// fingerprint. That breaks the attestation, not merely the transport:
    /// `runner_id` is what a manifest and a receipt record. `Match exec` and
    /// `LocalCommand` in the same file are local execution at parse time.
    ///
    /// The cost is real and worth naming: a user's `ProxyJump` for this host is
    /// not picked up either. A runner that needs one should carry it in its own
    /// record rather than in a file anything else can edit.
    ///
    /// - `BatchMode=yes`: never prompt. A prompt on a non-interactive channel
    ///   is a hang, and a password prompt would mean the pair key is not being
    ///   used at all.
    /// - `IdentitiesOnly=yes` with `IdentityFile` (offer the pair key and
    ///   nothing else, so the agent cannot substitute a key with wider rights.
    /// - `UserKnownHostsFile` + `StrictHostKeyChecking=yes`) the runner
    ///   authenticates to us with the key pinned at pair time. This is the half
    ///   of mutual authentication that the pair key does not provide.
    /// - `ControlMaster`/`ControlPersist`, session-per-RPC is only cheap
    ///   because these make the second session free.
    /// - `ServerAlive*`, a dead link becomes an error rather than a channel
    ///   that never ends.
    pub fn argv(&self) -> Vec<String> {
        let mut a: Vec<String> = vec!["ssh".into()];
        // No configuration file, at all. See the note above.
        a.push("-F".into());
        a.push("/dev/null".into());
        let mut opt = |k: &str, v: &str| {
            a.push("-o".into());
            a.push(format!("{k}={v}"));
        };
        opt("BatchMode", "yes");
        // Both host-key files, not just the user one: ssh consults each, and
        // pinning one of two is pinning neither.
        opt("GlobalKnownHostsFile", "/dev/null");
        opt("IdentitiesOnly", "yes");
        opt("IdentityFile", &self.identity.display().to_string());
        opt("UserKnownHostsFile", &self.known_hosts.display().to_string());
        opt("StrictHostKeyChecking", "yes");
        opt("RequestTTY", "no");
        opt("ServerAliveInterval", "15");
        opt("ServerAliveCountMax", "4");
        match &self.control_path {
            Some(p) => {
                opt("ControlMaster", "auto");
                opt("ControlPath", &p.display().to_string());
                opt("ControlPersist", "60");
            }
            None => opt("ControlPath", "none"),
        }
        if let Some(port) = self.port {
            a.push("-p".into());
            a.push(port.to_string());
        }
        a.push(self.destination());
        a.push(self.remote_command.clone());
        a
    }
}

impl Transport for SshTransport {
    fn connect(&self) -> Result<Channel, TransportError> {
        let argv = self.argv();
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..]);
        // The channel opens under the handshake clock; the caller re-arms for
        // the longer request clock once the peer has answered.
        Channel::spawn(self.describe(), cmd, &argv[0], self.deadlines.handshake)
    }

    fn describe(&self) -> String {
        format!("ssh {}", self.destination())
    }
}

/// Run the worker as a child of this process.
///
/// The point of this transport is that every failure mode of the protocol
/// (oversized frames, truncation, an unknown type, a peer that stops answering)
/// can be exercised in CI on a machine with no sshd, no second host and no
/// network. It is also how a developer drives a worker under a debugger.
#[derive(Debug, Clone)]
pub struct ChildProcessTransport {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Environment to set on the child, for a test that needs the worker to
    /// believe something about its host.
    pub env: Vec<(String, String)>,
    pub deadlines: Deadlines,
}

impl ChildProcessTransport {
    /// The usual shape: this binary, serving on its own stdio.
    pub fn serve_stdio(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: vec!["runner".into(), "serve-stdio".into()],
            env: Vec::new(),
            deadlines: Deadlines::default(),
        }
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Set both clocks at once. For a test that wants a deadline it can wait
    /// for without slowing the suite down.
    pub fn with_deadline(mut self, d: Duration) -> Self {
        self.deadlines.handshake = d;
        self.deadlines.control = d;
        self
    }
}

impl Transport for ChildProcessTransport {
    fn connect(&self) -> Result<Channel, TransportError> {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args);
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        Channel::spawn(
            self.describe(),
            cmd,
            &self.program.display().to_string(),
            self.deadlines.handshake,
        )
    }

    fn describe(&self) -> String {
        format!("{} {}", self.program.display(), self.args.join(" "))
    }
}

/// The forced-command line a runner's `authorized_keys` should carry.
///
/// `restrict` is the whole security argument in one word: it removes shell,
/// port forwarding, agent forwarding, X11 and pty allocation, so the pair key
/// can do exactly one thing. The command is absolute rather than bare, because
/// a `$PATH` on the far side is not something we control.
pub fn authorized_keys_line(worker_path: &Path, public_key: &str) -> String {
    format!(
        "restrict,command=\"{} runner serve-stdio\" {}",
        worker_path.display(),
        public_key.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssh() -> SshTransport {
        SshTransport {
            host: "pi.local".into(),
            user: Some("h5i".into()),
            port: None,
            identity: PathBuf::from("/home/dev/.config/h5i/runners/pi/id_ed25519"),
            known_hosts: PathBuf::from("/home/dev/.config/h5i/runners/pi/known_hosts"),
            control_path: Some(PathBuf::from("/home/dev/.config/h5i/runners/pi/ctl.sock")),
            remote_command: "h5i runner serve-stdio".into(),
            deadlines: Deadlines::default(),
        }
    }

    fn opt_value<'a>(argv: &'a [String], key: &str) -> Option<&'a str> {
        argv.windows(2).find_map(|w| {
            (w[0] == "-o")
                .then(|| w[1].split_once('='))
                .flatten()
                .and_then(|(k, v)| (k == key).then_some(v))
        })
    }

    #[test]
    fn the_argv_pins_every_option_that_carries_a_security_property() {
        // Each of these is load-bearing, and each would fail open if the user's
        // ~/.ssh/config were allowed to supply it instead.
        let argv = ssh().argv();
        assert_eq!(opt_value(&argv, "BatchMode"), Some("yes"));
        assert_eq!(opt_value(&argv, "IdentitiesOnly"), Some("yes"));
        assert_eq!(opt_value(&argv, "StrictHostKeyChecking"), Some("yes"));
        assert_eq!(
            opt_value(&argv, "UserKnownHostsFile"),
            Some("/home/dev/.config/h5i/runners/pi/known_hosts"),
            "the pinned per-runner file, never the user's"
        );
        assert_eq!(
            opt_value(&argv, "IdentityFile"),
            Some("/home/dev/.config/h5i/runners/pi/id_ed25519")
        );
        assert_eq!(opt_value(&argv, "RequestTTY"), Some("no"));

        // The two that make the rest of them mean anything. Without `-F` the
        // user's config is still read, and without the global file the pin
        // covers one of the two places ssh looks.
        let i = argv.iter().position(|a| a == "-F").expect("-F");
        assert_eq!(argv[i + 1], "/dev/null");
        assert_eq!(opt_value(&argv, "GlobalKnownHostsFile"), Some("/dev/null"));
    }

    #[test]
    fn the_destination_and_command_come_last_and_in_that_order() {
        let argv = ssh().argv();
        assert_eq!(argv[argv.len() - 2], "h5i@pi.local");
        assert_eq!(argv[argv.len() - 1], "h5i runner serve-stdio");
        assert_eq!(argv[0], "ssh");
    }

    #[test]
    fn a_port_is_passed_only_when_it_is_set() {
        let mut t = ssh();
        assert!(!t.argv().contains(&"-p".to_string()));
        t.port = Some(2222);
        let argv = t.argv();
        let i = argv.iter().position(|a| a == "-p").expect("-p");
        assert_eq!(argv[i + 1], "2222");
    }

    #[test]
    fn multiplexing_is_off_when_no_control_path_is_given() {
        // A bulk transfer asks for this: sharing one TCP connection means a
        // large DATA stream can hold up an interactive session behind it.
        let mut t = ssh();
        t.control_path = None;
        let argv = t.argv();
        assert_eq!(opt_value(&argv, "ControlPath"), Some("none"));
        assert!(opt_value(&argv, "ControlMaster").is_none());
    }

    #[test]
    fn a_destination_without_a_user_is_the_bare_host() {
        let mut t = ssh();
        t.user = None;
        assert_eq!(t.destination(), "pi.local");
        assert_eq!(t.argv().last().unwrap(), "h5i runner serve-stdio");
    }

    #[test]
    fn the_authorized_keys_line_restricts_and_uses_an_absolute_command() {
        let line = authorized_keys_line(
            Path::new("/usr/local/bin/h5i"),
            "ssh-ed25519 AAAAC3Nz h5i-runner\n",
        );
        assert!(line.starts_with("restrict,command=\""));
        assert!(line.contains("/usr/local/bin/h5i runner serve-stdio"));
        assert!(line.ends_with("ssh-ed25519 AAAAC3Nz h5i-runner"));
        assert!(!line.contains('\n'), "one key is one line, always");
    }

    #[test]
    fn a_missing_program_names_itself() {
        let t = ChildProcessTransport {
            program: PathBuf::from("/nonexistent/h5i-does-not-exist"),
            args: vec![],
            env: vec![],
            deadlines: Deadlines::default(),
        };
        match t.connect() {
            Err(TransportError::Spawn { program, .. }) => {
                assert!(program.contains("h5i-does-not-exist"));
            }
            other => panic!("expected Spawn, got {other:?}"),
        }
    }

    #[test]
    fn a_child_that_never_answers_is_killed_by_the_deadline() {
        // The clock has to be real: a pipe has no read timeout, so without the
        // watchdog a silent peer hangs the CLI forever.
        let t = ChildProcessTransport {
            program: PathBuf::from("/bin/sh"),
            // `exec`, so no shell survives to hold the pipe's write end open
            // past the kill. See the note on `Watchdog`.
            args: vec!["-c".into(), "exec sleep 120".into()],
            env: vec![],
            deadlines: Deadlines {
                handshake: Duration::from_millis(250),
                control: Duration::from_millis(250),
            },
        };
        let mut ch = t.connect().expect("spawn");
        let (mut out, _in) = ch.take_io().expect("io");
        let mut buf = Vec::new();
        // The kill turns a hang into an ordinary end of stream.
        let _ = out.read_to_end(&mut buf);
        assert!(ch.timed_out(), "the watchdog should have fired");
        match ch.finish() {
            Err(TransportError::Deadline { .. }) => {}
            other => panic!("expected Deadline, got {other:?}"),
        }
    }

    #[test]
    fn a_failing_child_reports_its_stderr() {
        let t = ChildProcessTransport {
            program: PathBuf::from("/bin/sh"),
            args: vec!["-c".into(), "echo 'no such runner' >&2; exit 3".into()],
            env: vec![],
            deadlines: Deadlines::default(),
        };
        let mut ch = t.connect().expect("spawn");
        let (mut out, stdin) = ch.take_io().expect("io");
        drop(stdin);
        let mut buf = Vec::new();
        let _ = out.read_to_end(&mut buf);
        match ch.finish() {
            Err(TransportError::Exit { stderr, status, .. }) => {
                assert!(stderr.contains("no such runner"), "stderr was {stderr:?}");
                assert!(status.contains('3'), "status was {status:?}");
            }
            other => panic!("expected Exit, got {other:?}"),
        }
    }

    #[test]
    fn io_is_handed_out_once() {
        let t = ChildProcessTransport {
            program: PathBuf::from("/bin/sh"),
            args: vec!["-c".into(), "exit 0".into()],
            env: vec![],
            deadlines: Deadlines::default(),
        };
        let mut ch = t.connect().expect("spawn");
        assert!(ch.take_io().is_some());
        assert!(ch.take_io().is_none(), "the halves are not re-issued");
        ch.abandon();
    }
}
