//! Mediating the browser daemon's control socket.
//!
//! The control lock was half-enforced: the viewer honoured it, but nothing stood
//! between the agent and `agent-browser`. A PATH shim is bypassed by an absolute
//! path and a convention enforces nothing, but every verb walks through the
//! socket, and the protocol is one line of JSON each way under a single mutex,
//! which is why a few hundred lines can sit in front of it.
//!
//! This is enforcement against an agent following the documented path: the CLI
//! reads `AGENT_BROWSER_SOCKET_DIR`, that directory is h5i's listener, and every
//! verb is decided here.
//!
//! It is not containment against a deliberately evasive agent, structurally: the
//! daemon runs inside the box and Landlock grants are per-box, so any socket the
//! daemon can bind the agent can connect to. Moving the daemon out would break
//! what boxes are for, since it could no longer reach the dev server on the box's
//! own loopback.
//!
//! Two things learned by driving the real daemon:
//!
//! 1. `__agent_browser_internal_shutdown` is an escape hatch, not an action. The
//!    CLI sends it when the daemon does not match the options it wants, then
//!    starts its own; forwarded, it kills the daemon we mediate and the next one
//!    is the agent's on a socket we do not own. Refused here always.
//! 2. The daemon's config fingerprint covers its options, not its path, which is
//!    what lets the real daemon run somewhere the box cannot reach: mirror
//!    `.version`/`.config` into the box-visible directory and the CLI is
//!    satisfied. Get the environment wrong and it decides the daemon is stale.

use std::collections::BTreeSet;
use std::io::{BufRead, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::control;

/// The action name the CLI uses to ask a daemon to exit.
const SHUTDOWN_ACTION: &str = "__agent_browser_internal_shutdown";

/// Verbs that only observe. Everything else is treated as mutating, so a verb
/// nobody classified is refused during a takeover rather than waved through.
const READ_ONLY_ACTIONS: &[&str] = &[
    // Not a page change: the CLI sends `launch` before *every* command as an
    // idempotent "make sure a browser exists", so classifying it as mutating
    // refuses it during a takeover and takes every read-only verb down with
    // it. The opposite of 5.4's rule that watching never collides. Found by
    // running the real CLI against the mediator; no amount of reading the
    // action list would have shown it.
    "launch",
    "read",
    "snapshot",
    "screenshot",
    "pdf",
    "url",
    "title",
    "content",
    "gettext",
    "getattribute",
    "innerhtml",
    "inputvalue",
    "boundingbox",
    "styles",
    "count",
    "isvisible",
    "isenabled",
    "ischecked",
    "tab_list",
    "cdp_url",
    "react_tree",
    "diff_snapshot",
    "diff_url",
    "console",
    "network",
];

/// What a profile permits the agent to do with the browser.
///
/// Modelled on agent-browser's own `ActionPolicy`, which is the right vocabulary
/// and about the right size. `deny` is the interesting half: `evaluate` is
/// arbitrary code in the page, and `credentials_*`/`state_*` reach the browser's
/// stored secrets, so a profile that wants a reading browser can say so.
///
/// The spelling is the *action* name, not the CLI verb: `evaluate`, which is
/// what `agent-browser eval` sends on the wire. Entries are checked against
/// `sandbox_policy::BROWSER_DENYABLE_ACTIONS` at create, because one that
/// matches nothing denies nothing while reading as if it did.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionPolicy {
    /// Actions refused outright. Matched on the action name, and on a `_`
    /// prefix so `state_*` can be spelled `state`.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub deny: BTreeSet<String>,
}

/// Verbs that reach the same capability as the one a profile named, and are
/// therefore denied with it.
///
/// The list exists because a denylist over a ~250-verb protocol is only as good
/// as its synonyms, and this one had none. `evaluate` is documented as the entry
/// "most profiles want, because it is arbitrary code in the page", and denying it
/// left `evalhandle` (a `Runtime.evaluate` under another name), `waitforfunction`,
/// `addscript`, `addinitscript`, `expose` and `setcontent` all forwarding, so a
/// profile that had asked for a reading browser got one that still ran whatever
/// the agent wrote. `credentials` was the same shape.
///
/// The left-hand name is what a profile writes; the right-hand names are what it
/// also means. Extending this is how a *new* daemon verb that reaches an
/// existing capability is covered without every profile being rewritten.
const DENY_ALSO_COVERS: &[(&str, &[&str])] = &[
    (
        "evaluate",
        &[
            // `Runtime.evaluate` with a `script` from the request.
            "evalhandle",
            // A JS expression, polled until it is true.
            "waitforfunction",
            // Script injected into the page, and into every future one.
            "addscript",
            "addinitscript",
            // A host callback the page can invoke.
            "expose",
            // Replacing the document with markup, script and all.
            "setcontent",
        ],
    ),
    // The stored-login family under its other name.
    ("credentials", &["auth"]),
];

/// Whether one deny entry covers one action, including the `name_*` family.
fn covers(entry: &str, action: &str) -> bool {
    action == entry || action.starts_with(&format!("{entry}_"))
}

impl ActionPolicy {
    pub fn deny_all_of<I: IntoIterator<Item = S>, S: Into<String>>(actions: I) -> Self {
        Self {
            deny: actions.into_iter().map(Into::into).collect(),
        }
    }

    fn denies(&self, action: &str) -> bool {
        self.deny.iter().any(|entry| {
            // The entry itself, and the `_` family under it: `state` denies
            // `state_save`/`state_load`, so a family is one entry.
            if covers(entry, action) {
                return true;
            }
            // And the verbs that reach the same capability by another name.
            DENY_ALSO_COVERS
                .iter()
                .filter(|(named, _)| named == entry)
                .flat_map(|(_, also)| also.iter())
                .any(|also| covers(also, action))
        })
    }
}

/// What the mediator decided about one request line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Forward,
    /// Refused, with the message the agent sees in place of a result.
    Refuse(String),
}

/// Whether a verb changes the page (or the browser), rather than reading it.
///
/// Unknown verbs count as mutating. agent-browser ships ~250 actions and grows
/// them faster than this list will be updated, so the default has to be the
/// safe one: a new verb is refused during a human takeover rather than
/// silently allowed to fight for the pointer.
pub fn is_mutating(action: &str) -> bool {
    !READ_ONLY_ACTIONS.contains(&action)
}

/// Whether completing this verb refreshes the agent's view of the page, and so
/// clears the stale-handle latch a human takeover set.
///
/// Deliberately just `snapshot`: that is the verb [`control::Verdict::explain`]
/// names, and the two have to agree or the refusal is advice the agent cannot
/// act on.
///
/// Because it is the *only* way out of the latch, a profile is not allowed to
/// deny it. See `sandbox_policy::validate_browser_deny`.
fn clears_resnapshot(action: &str) -> bool {
    action == "snapshot"
}

/// Decide one action.
///
/// Order matters: the shutdown escape hatch is refused before anything else,
/// then profile policy, then the control lock. Policy before the lock so a
/// denied verb reads as denied whoever is driving.
pub fn decide(action: &str, env_dir: &Path, policy: &ActionPolicy) -> Decision {
    if action == SHUTDOWN_ACTION {
        return Decision::Refuse(
            "h5i mediates this browser session; the daemon's lifecycle is h5i's. \
             Use `h5i box close` rather than shutting the daemon down."
                .to_string(),
        );
    }

    if policy.denies(action) {
        return Decision::Refuse(format!(
            "`{action}` is denied for this box by its profile's browser action policy (fail-closed)"
        ));
    }

    match control::check(env_dir, is_mutating(action)) {
        control::Verdict::Allowed => Decision::Forward,
        other => Decision::Refuse(
            other
                .explain()
                .unwrap_or_else(|| "refused by the control lock".to_string()),
        ),
    }
}

/// One mediated action, for the receipt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionRecord {
    pub action: String,
    pub forwarded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused_because: Option<String>,
}

impl ActionRecord {
    pub fn render(&self) -> String {
        match &self.refused_because {
            Some(why) => format!("REFUSED {} — {why}", self.action),
            None => format!("        {}", self.action),
        }
    }
}

/// What one mediated connection did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Mediation {
    pub actions: Vec<ActionRecord>,
    /// Set when the pump stopped on an I/O or protocol problem. Kept rather
    /// than returned as an error so the actions already mediated still reach
    /// the receipt. The same reason `view::pump_input` returns its count on
    /// the error path.
    pub error: Option<String>,
}

impl Mediation {
    pub fn forwarded(&self) -> usize {
        self.actions.iter().filter(|a| a.forwarded).count()
    }

    pub fn refused(&self) -> usize {
        self.actions.iter().filter(|a| !a.forwarded).count()
    }
}

/// Pump one client connection through to the daemon, deciding every line.
///
/// Generic over the four streams so the whole policy path is testable without
/// a socket, a daemon, or a box. Only the accept loop needs those.
///
/// A line that is not JSON, or carries no action, is forwarded untouched: this
/// is a mediator, not a parser of record, and inventing a refusal for a shape
/// we failed to understand would break sessions upstream can handle.
pub fn mediate(
    client_in: impl BufRead,
    client_out: impl Write,
    daemon_in: impl BufRead,
    daemon_out: impl Write,
    env_dir: &Path,
    policy: &ActionPolicy,
) -> Mediation {
    mediate_observed(
        client_in,
        client_out,
        daemon_in,
        daemon_out,
        env_dir,
        policy,
        &mut |_| {},
    )
}

/// As [`mediate`], but reporting each action the moment it is decided.
///
/// A session lasts as long as the agent keeps the socket open, so collecting
/// only on return would mean a receipt that stays empty until the box shuts
/// down. Evidence that arrives after the thing it describes is not evidence a
/// run can use.
#[allow(clippy::too_many_arguments)]
pub fn mediate_observed(
    mut client_in: impl BufRead,
    mut client_out: impl Write,
    mut daemon_in: impl BufRead,
    mut daemon_out: impl Write,
    env_dir: &Path,
    policy: &ActionPolicy,
    on_action: &mut dyn FnMut(&ActionRecord),
) -> Mediation {
    let mut mediation = Mediation::default();

    loop {
        let line = match read_capped_line(&mut client_in, "client") {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(e) => {
                mediation.error = Some(e);
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }

        let parsed: Option<Value> = serde_json::from_str(&line).ok();
        let action = parsed
            .as_ref()
            .and_then(|v| v.get("action"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        if action.is_empty() {
            if let Err(e) = forward(&line, &mut daemon_out, &mut daemon_in, &mut client_out) {
                mediation.error = Some(e);
                break;
            }
            continue;
        }

        match decide(&action, env_dir, policy) {
            Decision::Forward => {
                if let Err(e) = forward(&line, &mut daemon_out, &mut daemon_in, &mut client_out) {
                    mediation.error = Some(e);
                    let record = ActionRecord {
                        action,
                        forwarded: true,
                        refused_because: None,
                    };
                    on_action(&record);
                    mediation.actions.push(record);
                    break;
                }
                // A completed snapshot is what clears the stale-handle latch a
                // takeover set. Nothing else in the tree calls `snapshotted`,
                // so without this the refusal tells the agent to run
                // `agent-browser snapshot` and running it changes nothing.
                // Every mutating verb stays refused for the life of the box.
                // Cleared only after the daemon answered, because a snapshot
                // that never reached the page did not refresh anything.
                if clears_resnapshot(&action) {
                    let _ = control::snapshotted(env_dir);
                }

                let record = ActionRecord {
                    action,
                    forwarded: true,
                    refused_because: None,
                };
                on_action(&record);
                mediation.actions.push(record);
            }
            Decision::Refuse(why) => {
                // Answer in the daemon's own shape, carrying the request id, so
                // the CLI reports a refusal rather than hanging on a reply that
                // never comes.
                let id = parsed
                    .as_ref()
                    .and_then(|v| v.get("id"))
                    .cloned()
                    .unwrap_or(Value::Null);
                let refusal = serde_json::json!({
                    "id": id,
                    "success": false,
                    "error": why,
                });
                if let Err(e) = writeln!(client_out, "{refusal}").and_then(|_| client_out.flush()) {
                    mediation.error = Some(format!("writing a refusal to the client: {e}"));
                    break;
                }
                let record = ActionRecord {
                    action,
                    forwarded: false,
                    refused_because: Some(why),
                };
                on_action(&record);
                mediation.actions.push(record);
            }
        }
    }

    mediation
}

/// Send one request and relay exactly one response line back, and the longest
/// single protocol line either side may send.
///
/// The protocol is one line of JSON each way. `BufRead::lines` and `read_line`
/// grow a `String` until they meet a newline, so without a ceiling a peer that
/// sends a gigabyte and no `\n` makes *this host process* allocate a gigabyte,
/// and both peers here are inside the box: the client is the agent's CLI on a
/// socket in `<env>/tmp`, and the daemon is a process in the box too.
///
/// `refuse_no_daemon` has capped its one read since it was written. It is the
/// path taken when no daemon answered; the path taken the rest of the time had
/// no cap at all.
///
/// 8 MiB, well past any real line and far below what a host notices.
const MAX_LINE: u64 = 8 * 1024 * 1024;

/// Read one `\n`-terminated line, refusing one that outruns [`MAX_LINE`].
///
/// `Ok(None)` is end of stream. An overlong line is an error rather than a
/// truncation: half a JSON object forwarded onward is a line the other side
/// would try to answer.
fn read_capped_line(r: &mut impl BufRead, what: &str) -> Result<Option<String>, String> {
    let mut buf: Vec<u8> = Vec::new();
    // `Take` over a borrow, rebuilt per line, so the cap is per line and the
    // reader's buffered bytes survive into the next one.
    let read = std::io::Read::take(&mut *r, MAX_LINE)
        .read_until(b'\n', &mut buf)
        .map_err(|e| format!("reading from the {what}: {e}"))?;
    if read == 0 {
        return Ok(None);
    }
    if !buf.ends_with(b"\n") && read as u64 == MAX_LINE {
        return Err(format!(
            "the {what} sent a line longer than {MAX_LINE} bytes with no newline in it — \
             refusing it rather than growing to meet it"
        ));
    }
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

fn forward(
    line: &str,
    daemon_out: &mut impl Write,
    daemon_in: &mut impl BufRead,
    client_out: &mut impl Write,
) -> Result<(), String> {
    writeln!(daemon_out, "{line}")
        .and_then(|_| daemon_out.flush())
        .map_err(|e| format!("writing to the daemon: {e}"))?;

    let Some(response) = read_capped_line(daemon_in, "daemon")? else {
        return Err("the daemon closed the connection".to_string());
    };

    client_out
        .write_all(response.as_bytes())
        .and_then(|_| client_out.flush())
        .map_err(|e| format!("writing to the client: {e}"))
}

/// A running mediator: an `AF_UNIX` listener on the path the box can reach,
/// forwarding to the daemon on a path it cannot.
///
/// Shaped like `container::ProxyHandle` (a stop flag, a polling accept loop,
/// and a `Drop` that joins) because the run lifecycle already knows how to
/// hold a handle for the length of a run and drop it at the end.
pub struct MediatorHandle {
    socket_path: std::path::PathBuf,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    actions: std::sync::Arc<std::sync::Mutex<Vec<ActionRecord>>>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl MediatorHandle {
    /// Everything mediated so far, across every connection.
    pub fn actions(&self) -> Vec<ActionRecord> {
        self.actions.lock().map(|a| a.clone()).unwrap_or_default()
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

impl Drop for MediatorHandle {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        // Poke the listener so a blocked accept wakes up and sees the flag.
        #[cfg(unix)]
        let _ = std::os::unix::net::UnixStream::connect(&self.socket_path);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// How long a connection waits for the daemon to appear before giving up.
///
/// The box's shim starts the daemon and connects in quick succession, so the
/// mediator can be bound before an upstream exists. Waiting a little beats
/// refusing a request that was always going to be servable a moment later.
#[cfg(unix)] // only the unix-gated listener references this
const UPSTREAM_WAIT: std::time::Duration = std::time::Duration::from_secs(15);

/// Consecutive `accept` failures before the mediator gives up.
///
/// Anything less than "give up eventually" risks a hot loop on a permanently
/// broken listener; anything that gives up on the first error hands the rest
/// of the run to an unmediated daemon.
#[cfg(unix)] // only the unix-gated listener references this
const MAX_ACCEPT_ERRORS: usize = 20;

/// Connections served at once. The accept loop spawns one thread per connection
/// and the socket sits in `<env>/tmp`, one of the two paths the box can write,
/// so without a ceiling a loop of `connect()` calls inside the box spawns
/// unbounded *host* threads. `auth_proxy` has carried this bound since it was
/// written; this sibling, reachable the same way and by the same actor, had
/// none.
///
/// 64, matching that sibling. The box's browser CLI opens one connection per
/// command and nothing legitimate approaches it.
#[cfg(unix)]
const MAX_IN_FLIGHT: usize = 64;

/// Holds one of the [`MAX_IN_FLIGHT`] slots and releases it on drop, including
/// when the worker unwinds. A bare decrement at the end of the closure is
/// skipped by a panic, and the slot is then held forever by a thread that no
/// longer exists.
#[cfg(unix)]
struct InFlightSlot(std::sync::Arc<std::sync::atomic::AtomicUsize>);

#[cfg(unix)]
impl Drop for InFlightSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Start mediating `socket_path` in front of `upstream`.
///
/// Both are `AF_UNIX` paths. `socket_path` is what the box is told to use
/// (`AGENT_BROWSER_SOCKET_DIR`); `upstream` is where the box's shim starts the
/// real daemon.
///
/// The upstream need not exist yet: the mediator has to own its socket *before*
/// the box runs, or the box's first `agent-browser` call finds nothing there and
/// starts an unmediated daemon of its own on the very path we meant to hold.
#[cfg(unix)]
pub fn spawn(
    socket_path: &Path,
    upstream: &Path,
    env_dir: &Path,
    policy: ActionPolicy,
) -> Result<MediatorHandle, h5i_error::H5iError> {
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};

    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| h5i_error::H5iError::with_path(e, parent))?;
    }
    // A stale socket file from a previous run would make bind fail with
    // EADDRINUSE even though nothing is listening.
    let _ = std::fs::remove_file(socket_path);

    let listener = UnixListener::bind(socket_path).map_err(|e| {
        h5i_error::H5iError::Metadata(format!(
            "could not listen at {}: {e}",
            socket_path.display()
        ))
    })?;
    // Whoever can connect here drives the browser: every verb the mediator
    // forwards, which includes reading the page and, subject to the policy
    // above, running script in it. Linux checks write permission on the socket
    // file at `connect`, so the mode is the access control, and leaving it to
    // the umask made it 0755 on a default one. The path is
    // `AGENT_BROWSER_SOCKET_DIR`, under a box's `/tmp`, shared with the host
    // under the `agent` profile.
    //
    // This does not make the mediator containment against a deliberately
    // evasive agent inside the box. It stops the socket from being everyone
    // *else* on the machine's, which is a different question.
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600));
    }
    listener
        .set_nonblocking(true)
        .map_err(h5i_error::H5iError::Io)?;

    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let actions: Arc<Mutex<Vec<ActionRecord>>> = Arc::new(Mutex::new(Vec::new()));

    let join = {
        let stop = stop.clone();
        let actions = actions.clone();
        let upstream = upstream.to_path_buf();
        let env_dir = env_dir.to_path_buf();
        let in_flight = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        std::thread::spawn(move || {
            let mut consecutive_errors = 0usize;
            while !stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((client, _)) => {
                        consecutive_errors = 0;
                        if stop.load(Ordering::SeqCst) {
                            break;
                        }
                        // Refuse rather than queue: a queued connection still holds a
                        // descriptor, and the actor filling these slots is a box in a
                        // loop. Not counted as an accept error, since the listener is
                        // healthy and it is the load that is not. Claimed with one
                        // atomic and released by the guard, the shape `container`'s
                        // proxy uses: a `load` followed by a `fetch_add` is correct
                        // only because exactly one thread accepts.
                        let taken = in_flight.fetch_add(1, Ordering::SeqCst);
                        let slot = InFlightSlot(in_flight.clone());
                        if taken >= MAX_IN_FLIGHT {
                            drop(slot);
                            drop(client);
                            continue;
                        }
                        let actions = actions.clone();
                        let upstream = upstream.clone();
                        let env_dir = env_dir.clone();
                        let policy = policy.clone();
                        // `Builder::spawn`, not `thread::spawn`: the latter
                        // *panics* when the OS refuses a thread, and that panic
                        // is in this loop, which lands exactly where the
                        // comment on the error arm below says it must not.
                        // A box running a browser and a build is the case that
                        // exhausts threads, and it is also the case this
                        // mediator exists for.
                        let spawned = std::thread::Builder::new().spawn(move || {
                            // Moved in, so the slot is released when this
                            // worker ends however it ends.
                            let _slot = slot;
                            let _ = client.set_nonblocking(false);
                            let Some(daemon) = connect_upstream(&upstream) else {
                                // Nothing came up. Answer in the daemon's own
                                // shape rather than hanging, but carry the
                                // request's id, or a CLI that correlates
                                // replies by id ignores this line and stalls
                                // until its own timeout, which is exactly the
                                // hang this branch exists to avoid. So we read
                                // the first request to learn its id first.
                                refuse_no_daemon(&client);
                                return;
                            };
                            let (Ok(client_read), Ok(daemon_read)) =
                                (client.try_clone(), daemon.try_clone())
                            else {
                                return;
                            };
                            mediate_observed(
                                std::io::BufReader::new(client_read),
                                &client,
                                std::io::BufReader::new(daemon_read),
                                &daemon,
                                &env_dir,
                                &policy,
                                &mut |record| {
                                    if let Ok(mut sink) = actions.lock() {
                                        sink.push(record.clone());
                                    }
                                },
                            );
                        });
                        if spawned.is_err() {
                            // Counted with the accept failures: it is the same
                            // exhaustion, it recovers the same way, and if it
                            // never does the operator gets the same sentence
                            // rather than a loop spinning on it.
                            consecutive_errors += 1;
                            if consecutive_errors >= MAX_ACCEPT_ERRORS {
                                eprintln!(
                                    "h5i: browser mediation stopped after {consecutive_errors} \
                                     consecutive failures to serve a connection; the control \
                                     lock is no longer enforced for this session."
                                );
                                break;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(50));
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        // A healthy idle poll. It has to clear the counter, or
                        // "consecutive" means "total since the session began"
                        // and sporadic errors hours apart add up to a listener
                        // that stops enforcing.
                        consecutive_errors = 0;
                        std::thread::sleep(std::time::Duration::from_millis(25));
                    }
                    // A transient accept failure (EMFILE while the box runs a
                    // browser and a build, ECONNABORTED from a client that hung
                    // up between connect and accept) must not end enforcement
                    // for the rest of the run. Breaking here leaves the socket
                    // file on disk with nothing listening, so the box's next
                    // command gets ECONNREFUSED and starts its own unmediated
                    // daemon, silently. Back off and keep accepting; give up
                    // only if it never recovers, and say so when we do.
                    Err(e) => {
                        consecutive_errors += 1;
                        if consecutive_errors >= MAX_ACCEPT_ERRORS {
                            eprintln!(
                                "h5i: browser mediation stopped after {consecutive_errors} \
                                 consecutive accept failures ({e}); the control lock is no \
                                 longer enforced for this session."
                            );
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                }
            }
        })
    };

    Ok(MediatorHandle {
        socket_path: socket_path.to_path_buf(),
        stop,
        actions,
        join: Some(join),
    })
}

/// The whole mediator is an `AF_UNIX` listener in front of an `AF_UNIX`
/// daemon, so on a platform without them there is nothing to sit in front of.
/// Returning an error rather than a silent `None` keeps the caller's existing
/// "mediation could not start, the lock is advisory" message. The same thing
/// h5i says on a Unix host whose bind fails, which is the honest report.
#[cfg(not(unix))]
pub fn spawn(
    _socket_path: &Path,
    _upstream: &Path,
    _env_dir: &Path,
    _policy: ActionPolicy,
) -> Result<MediatorHandle, h5i_error::H5iError> {
    Err(h5i_error::H5iError::Metadata(
        "browser mediation needs AF_UNIX sockets, which this platform does not provide".into(),
    ))
}

/// Write what the mediator saw into the receipt, in its own lane.
///
/// Its own lane (`browser-proxy`) rather than a field on `BrowserEvidence`,
/// because the two are different kinds of claim: the existing evidence is
/// drained from the page *after* a run and is box-claimed, while this is
/// host-observed, h5i having sat on the socket and watched each verb go by. A
/// reviewer should be able to tell those apart without reading the code, which
/// is what `HOST_OBSERVED_LANES` is for.
///
/// Records nothing when nothing happened. Where the structured copy lives is a
/// sibling of `receipt.jsonl` and deliberately *not* under `<env>/spool` or
/// `<env>/tmp`, the two paths a box can write. It exists because the receipt
/// carries these actions as *rendered text*, and a reader that wanted them back
/// as data would have to parse a display format, which is the quiet-wrong-answer
/// shape this codebase keeps getting bitten by.
pub fn actions_log(env_dir: &Path) -> std::path::PathBuf {
    env_dir.join("browser-actions.jsonl")
}

pub fn record_actions(
    env_dir: &Path,
    env_id: &str,
    policy_digest: &str,
    actions: &[ActionRecord],
) {
    if actions.is_empty() {
        return;
    }

    // Best effort, and deliberately before the receipt: the receipt is the
    // record of account and must not be lost because a viewer's convenience
    // copy could not be written. A failure here is reported and dropped.
    if let Err(e) = append_actions_log(env_dir, actions) {
        eprintln!("browser mediation: could not write the action log: {e}");
    }

    let refused = actions.iter().filter(|a| !a.forwarded).count();
    let mut body = String::new();
    body.push_str(&format!(
        "browser actions mediated by h5i: {} forwarded, {refused} refused\n",
        actions.len() - refused
    ));
    for record in actions {
        body.push_str(&record.render());
        body.push('\n');
    }

    let input = crate::receipt::RecordInput {
        env_id: env_id.to_string(),
        policy_digest: Some(policy_digest.to_string()),
        source: "browser-proxy".into(),
        cmd: Some(format!(
            "h5i browser mediation ({} action(s), {refused} refused)",
            actions.len()
        )),
        ..Default::default()
    };
    if let Err(e) = crate::receipt::append(env_dir, input, body.as_bytes()) {
        eprintln!("browser mediation: could not record the actions: {e}");
    }
}

/// Append the actions as JSON lines. Opened per call rather than held: a run
/// writes this once at the end, and a long-lived handle on a path in the env
/// directory is one more thing to reason about across a `Drop`.
fn append_actions_log(env_dir: &Path, actions: &[ActionRecord]) -> std::io::Result<()> {
    use std::io::Write as _;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(actions_log(env_dir))?;
    for record in actions {
        // A record that will not serialize is skipped rather than aborting the
        // rest: one unrenderable action must not cost a reviewer the others.
        if let Ok(line) = serde_json::to_string(record) {
            writeln!(file, "{line}")?;
        }
    }
    file.flush()
}

/// Read one line from `client`, bounded three ways: per `read()`, in total
/// bytes, and, the one that does not follow from the other two, in wall
/// clock. Its own function so a test can drive the deadline in milliseconds
/// instead of waiting out the production one, which is the shape
/// `view::read_head_within` already uses for the same reason.
///
/// Returns whatever arrived. The caller only parses it as JSON, so a partial
/// line, a timed-out one and an empty one all fail the same way.
#[cfg(unix)]
fn read_first_line_within(
    client: &std::os::unix::net::UnixStream,
    per_read: std::time::Duration,
    whole: std::time::Duration,
    cap: u64,
) -> String {
    use std::io::Read;
    let deadline = std::time::Instant::now() + whole;
    let _ = client.set_read_timeout(Some(per_read));
    let mut raw: Vec<u8> = Vec::new();
    {
        let mut reader = std::io::BufReader::new(client.take(cap));
        let mut byte = [0u8; 1];
        while std::time::Instant::now() < deadline {
            match reader.read(&mut byte) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    raw.push(byte[0]);
                    if byte[0] == b'\n' {
                        break;
                    }
                }
            }
        }
    }
    let _ = client.set_read_timeout(None);
    // Lossy rather than `read_line`'s "error on invalid UTF-8": this is only
    // ever parsed as JSON, and a replacement character fails that parse the
    // same way the bytes would have. Nothing here is echoed anywhere.
    String::from_utf8_lossy(&raw).into_owned()
}

/// Tell a client that no daemon answered, echoing the id of its first request
/// so a CLI correlating replies by id matches the reply rather than hanging.
#[cfg(unix)]
fn refuse_no_daemon(client: &std::os::unix::net::UnixStream) {
    use std::io::Write;

    // Read one line: the request the client is waiting on a reply to. Best
    // effort, and *bounded*, because a client that connects to probe liveness
    // and waits for the peer to speak first would otherwise block this thread
    // forever and never receive the refusal. Threads here are detached, so an
    // unbounded wait also leaks one per retry while a daemon is failing to
    // start.
    //
    // Bounded in *bytes* as well as time, which is the half the timeout does
    // not cover: `SO_RCVTIMEO` ends one `read()`, while `read_line` keeps
    // calling it and growing its `String` until a newline arrives.
    const MAX_FIRST_LINE: u64 = 64 * 1024;
    // And bounded overall, which the two above still do not add up to. The
    // timeout ends one `read()` and the cap ends the *bytes*; the loop between
    // them is ended by neither. A peer that lets every read *succeed*, one byte
    // just inside each interval, never trips the timeout, because a timeout is
    // an error and a byte is not, so it walks the whole cap at its own pace: 64
    // Ki reads of up to two seconds each is a day and a half holding a host
    // thread, from a socket in `<env>/tmp` that the box writes.
    const FIRST_LINE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);
    const PER_READ: std::time::Duration = std::time::Duration::from_secs(2);
    let first = read_first_line_within(client, PER_READ, FIRST_LINE_DEADLINE, MAX_FIRST_LINE);
    let _ = client.set_read_timeout(None);
    let id = serde_json::from_str::<Value>(&first)
        .ok()
        .and_then(|v| v.get("id").cloned())
        .unwrap_or(Value::Null);

    let refusal = serde_json::json!({
        "id": id,
        "success": false,
        "error": "h5i: no browser daemon answered; is one starting?",
    });
    // `&UnixStream` implements `Write`; a mutable binding lets `writeln!`
    // autoref it.
    let mut out = client;
    let _ = writeln!(out, "{refusal}");
}

/// Connect to the daemon, waiting for it to appear.
#[cfg(unix)]
fn connect_upstream(path: &Path) -> Option<std::os::unix::net::UnixStream> {
    let deadline = std::time::Instant::now() + UPSTREAM_WAIT;
    loop {
        if let Ok(stream) = std::os::unix::net::UnixStream::connect(path) {
            return Some(stream);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tempfile::TempDir;

    fn req(id: &str, action: &str) -> String {
        format!(r#"{{"id":"{id}","action":"{action}"}}"#)
    }

    /// A denylist over a ~250-verb protocol is only as good as its synonyms,
    /// and this one had none. `evaluate` is documented as the entry "most
    /// profiles want, because it is arbitrary code in the page", and denying
    /// it left `evalhandle`, which is a `Runtime.evaluate` with a script from
    /// the request under another name, forwarding. So did `addscript`,
    /// `addinitscript`, `waitforfunction`, `expose` and `setcontent`: a profile
    /// that had asked for a reading browser got one that still ran whatever
    /// the agent wrote.
    #[test]
    fn denying_evaluate_denies_every_verb_that_is_evaluate_under_another_name() {
        let policy = ActionPolicy::deny_all_of(["evaluate"]);
        for verb in [
            "evaluate",
            "evalhandle",
            "waitforfunction",
            "addscript",
            "addinitscript",
            "expose",
            "setcontent",
        ] {
            assert!(
                policy.denies(verb),
                "`{verb}` runs page script and survived a profile that denied `evaluate`"
            );
        }
        // And it is a denylist still, not a mood: reading is untouched.
        for verb in ["read", "snapshot", "screenshot", "click"] {
            assert!(!policy.denies(verb), "`{verb}` is not evaluate");
        }
    }

    /// The same shape on the other capability: `credentials` was denied while
    /// `auth_show` and `auth_login` read and used the same stored logins.
    #[test]
    fn denying_credentials_denies_the_stored_logins_under_their_other_name() {
        let policy = ActionPolicy::deny_all_of(["credentials"]);
        for verb in [
            "credentials_get",
            "credentials_set",
            "auth_show",
            "auth_login",
            "auth_list",
            "auth_delete",
            "auth_save",
        ] {
            assert!(policy.denies(verb), "`{verb}` reaches the stored logins");
        }
        // `auth` on its own is nameable too, and covers only its own family.
        let narrow = ActionPolicy::deny_all_of(["auth"]);
        assert!(narrow.denies("auth_show"));
        assert!(!narrow.denies("credentials_get"));
    }

    /// Every name the equivalence table hands out has to be one a profile
    /// could also have written itself, or the policy would enforce a
    /// vocabulary its own validator rejects.
    #[test]
    fn every_covered_verb_is_a_name_a_profile_may_write() {
        for (named, also) in DENY_ALSO_COVERS {
            assert!(
                h5i_sandbox::sandbox_policy::validate_browser_deny(named).is_ok(),
                "`{named}` is not accepted by the validator"
            );
            for verb in *also {
                assert!(
                    h5i_sandbox::sandbox_policy::validate_browser_deny(verb).is_ok(),
                    "`{verb}` is covered by `{named}` and is not itself nameable"
                );
            }
        }
    }

    /// A daemon that answers every request with the same success line.
    fn fake_daemon(responses: usize) -> Cursor<Vec<u8>> {
        let body = (0..responses)
            .map(|i| format!(r#"{{"id":"{i}","success":true,"data":"ok"}}"#))
            .collect::<Vec<_>>()
            .join("\n");
        Cursor::new(format!("{body}\n").into_bytes())
    }

    fn run(
        requests: &[String],
        env_dir: &Path,
        policy: &ActionPolicy,
    ) -> (Mediation, String, String) {
        let input = requests.join("\n") + "\n";
        let mut to_client = Vec::new();
        let mut to_daemon = Vec::new();
        let mediation = mediate(
            Cursor::new(input.into_bytes()),
            &mut to_client,
            fake_daemon(requests.len()),
            &mut to_daemon,
            env_dir,
            policy,
        );
        (
            mediation,
            String::from_utf8_lossy(&to_client).into_owned(),
            String::from_utf8_lossy(&to_daemon).into_owned(),
        )
    }

    #[test]
    fn with_the_agent_holding_control_everything_passes() {
        let td = TempDir::new().unwrap();
        let (m, to_client, to_daemon) = run(
            &[req("1", "click"), req("2", "snapshot")],
            td.path(),
            &ActionPolicy::default(),
        );
        assert_eq!(m.forwarded(), 2, "{m:?}");
        assert_eq!(m.refused(), 0);
        assert!(to_daemon.contains("click"), "the daemon must see it");
        assert!(to_client.contains("\"success\":true"));
    }

    #[test]
    fn during_a_human_takeover_mutating_verbs_are_refused_and_reads_still_pass() {
        // This is the hole M8 exists to close: today an agent clicking during a
        // takeover is not refused, only told if it asks.
        let td = TempDir::new().unwrap();
        control::take(td.path()).expect("human takes control");

        let (m, to_client, to_daemon) = run(
            &[req("1", "click"), req("2", "snapshot")],
            td.path(),
            &ActionPolicy::default(),
        );

        assert_eq!(m.refused(), 1, "{:?}", m.actions);
        assert_eq!(m.actions[0].action, "click");
        assert!(!m.actions[0].forwarded);
        assert!(!to_daemon.contains("click"), "the click must not reach the daemon");

        // Watching never collides, so reads are unaffected...
        assert!(to_daemon.contains("snapshot"), "reads must still pass");
        // ...and the agent gets a refusal in the daemon's own shape, with its id.
        assert!(to_client.contains("\"success\":false"), "{to_client}");
        assert!(to_client.contains("\"id\":\"1\""), "{to_client}");
    }

    #[test]
    fn the_shutdown_escape_hatch_is_always_refused() {
        // Found by driving the real CLI: forwarded, this kills the daemon we
        // mediate and the CLI starts its own on a socket we do not own, so
        // mediation disappears with no error anywhere.
        let td = TempDir::new().unwrap();
        let (m, to_client, to_daemon) = run(
            &[req("1", SHUTDOWN_ACTION)],
            td.path(),
            &ActionPolicy::default(),
        );

        assert_eq!(m.refused(), 1);
        assert!(
            !to_daemon.contains(SHUTDOWN_ACTION),
            "the daemon must never see it"
        );
        assert!(to_client.contains("h5i box close"), "{to_client}");
    }

    #[test]
    fn a_denied_action_is_refused_even_while_the_agent_holds_control() {
        let td = TempDir::new().unwrap();
        let policy = ActionPolicy::deny_all_of(["evaluate"]);
        let (m, to_client, to_daemon) = run(&[req("1", "evaluate")], td.path(), &policy);

        assert_eq!(m.refused(), 1);
        assert!(!to_daemon.contains("evaluate"));
        assert!(to_client.contains("fail-closed"), "{to_client}");
    }

    #[test]
    fn denying_a_family_denies_its_members() {
        let td = TempDir::new().unwrap();
        let policy = ActionPolicy::deny_all_of(["state", "credentials"]);
        let (m, _, to_daemon) = run(
            &[req("1", "state_save"), req("2", "credentials_list")],
            td.path(),
            &policy,
        );
        assert_eq!(m.refused(), 2, "{:?}", m.actions);
        assert!(to_daemon.trim().is_empty(), "nothing should be forwarded");
    }

    #[test]
    fn launch_is_not_a_page_change_so_reads_survive_a_takeover() {
        // The CLI prefixes commands with `launch`; refusing it during a
        // takeover would make every read-only verb fail too.
        assert!(!is_mutating("launch"));
    }

    #[test]
    fn an_unknown_verb_counts_as_mutating() {
        // agent-browser grows verbs faster than this list will be updated, so
        // the default has to be the safe one.
        assert!(is_mutating("some_new_verb_from_a_future_release"));
        assert!(is_mutating("click"));
        assert!(!is_mutating("snapshot"));
    }

    #[test]
    fn after_a_takeover_ends_the_agent_must_resnapshot_before_acting() {
        let td = TempDir::new().unwrap();
        control::take(td.path()).unwrap();
        control::release(td.path()).unwrap();

        // The DOM it remembers is stale, so a mutating verb is still refused...
        let (m, _, to_daemon) = run(&[req("1", "click")], td.path(), &ActionPolicy::default());
        assert_eq!(m.refused(), 1, "{:?}", m.actions);
        assert!(!to_daemon.contains("click"));

        // ...until it takes a fresh snapshot.
        control::snapshotted(td.path()).unwrap();
        let (m, _, to_daemon) = run(&[req("2", "click")], td.path(), &ActionPolicy::default());
        assert_eq!(m.forwarded(), 1, "{:?}", m.actions);
        assert!(to_daemon.contains("click"));
    }

    #[test]
    fn taking_the_snapshot_the_refusal_asks_for_actually_unblocks_the_agent() {
        // The whole cycle, with nobody calling `control::snapshotted` by hand:
        // the mediator is the only thing that can clear the latch, and if it
        // does not, the refusal names a remedy that does nothing and the
        // browser is bricked for the agent for the life of the box.
        let td = TempDir::new().unwrap();
        control::take(td.path()).unwrap();
        control::release(td.path()).unwrap();

        // Refused, and told to snapshot.
        let (m, to_client, _) = run(&[req("1", "click")], td.path(), &ActionPolicy::default());
        assert_eq!(m.refused(), 1);
        assert!(to_client.contains("snapshot"), "{to_client}");

        // Do exactly that, through the mediator.
        let (m, _, to_daemon) = run(&[req("2", "snapshot")], td.path(), &ActionPolicy::default());
        assert_eq!(m.forwarded(), 1, "a snapshot is read-only and must pass");
        assert!(to_daemon.contains("snapshot"));

        // Now acting works again.
        let (m, _, to_daemon) = run(&[req("3", "click")], td.path(), &ActionPolicy::default());
        assert_eq!(
            m.forwarded(),
            1,
            "the snapshot must have cleared the latch: {:?}",
            m.actions
        );
        assert!(to_daemon.contains("click"));
    }

    #[test]
    fn a_snapshot_that_never_reached_the_daemon_does_not_clear_the_latch() {
        // The daemon hangs up before answering; nothing was refreshed, so the
        // agent must still be told to re-snapshot.
        let td = TempDir::new().unwrap();
        control::take(td.path()).unwrap();
        control::release(td.path()).unwrap();

        let mut to_client = Vec::new();
        let mut to_daemon = Vec::new();
        let mediation = mediate(
            Cursor::new((req("1", "snapshot") + "\n").into_bytes()),
            &mut to_client,
            // Truly empty: the daemon hung up without answering. (`fake_daemon(0)`
            // would still emit a bare newline, which `read_line` reads as a
            // successful, if empty, response.)
            Cursor::new(Vec::new()),
            &mut to_daemon,
            td.path(),
            &ActionPolicy::default(),
        );
        assert!(mediation.error.is_some(), "the hangup should be reported");

        assert!(
            control::read(td.path()).needs_resnapshot,
            "an unanswered snapshot must not clear the latch"
        );
    }

    #[test]
    fn a_human_still_holding_control_is_not_unblocked_by_a_snapshot() {
        // Clearing the stale-handle latch must not be a way around the lock
        // itself.
        let td = TempDir::new().unwrap();
        control::take(td.path()).unwrap();

        let (_, _, _) = run(&[req("1", "snapshot")], td.path(), &ActionPolicy::default());
        let (m, _, to_daemon) = run(&[req("2", "click")], td.path(), &ActionPolicy::default());
        assert_eq!(m.refused(), 1, "the human still holds control");
        assert!(!to_daemon.contains("click"));
    }

    #[test]
    fn a_line_that_is_not_json_is_passed_through_rather_than_invented_upon() {
        let td = TempDir::new().unwrap();
        let (m, _, to_daemon) = run(&["not json at all".to_string()], td.path(), &ActionPolicy::default());
        assert!(m.actions.is_empty(), "nothing to record about it");
        assert!(to_daemon.contains("not json"));
    }

    #[test]
    fn a_daemon_that_hangs_up_mid_session_keeps_the_actions_already_mediated() {
        // Evidence must survive an untidy disconnect, the same way the viewer's
        // input count does.
        let td = TempDir::new().unwrap();
        let input = format!("{}\n{}\n", req("1", "click"), req("2", "click"));
        let mut to_client = Vec::new();
        let mut to_daemon = Vec::new();
        let m = mediate(
            Cursor::new(input.into_bytes()),
            &mut to_client,
            fake_daemon(1), // only one response for two requests
            &mut to_daemon,
            td.path(),
            &ActionPolicy::default(),
        );

        assert!(m.error.is_some(), "the hangup should be reported");
        assert_eq!(m.actions.len(), 2, "both attempts are still recorded");
    }

    /// Stand up a fake daemon on a real socket, mediate in front of it, and
    /// drive it with a real client. The same shape as production, minus
    /// agent-browser.
    #[test]
    #[cfg(unix)]
    fn the_listener_mediates_over_real_sockets() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::{UnixListener, UnixStream};

        let td = TempDir::new().unwrap();
        // Short paths: `sun_path` is 108 bytes and a temp dir plus a session
        // name gets there faster than anyone expects.
        let upstream_path = td.path().join("d.sock");
        let front_path = td.path().join("f.sock");

        let upstream = UnixListener::bind(&upstream_path).expect("fake daemon binds");
        std::thread::spawn(move || {
            for conn in upstream.incoming().flatten() {
                let reader = BufReader::new(conn.try_clone().unwrap());
                let mut writer = conn;
                for line in reader.lines().map_while(Result::ok) {
                    let _ = line;
                    let _ = writeln!(writer, r#"{{"success":true,"data":"from the daemon"}}"#);
                    let _ = writer.flush();
                }
            }
        });

        let handle = spawn(
            &front_path,
            &upstream_path,
            td.path(),
            ActionPolicy::deny_all_of(["evaluate"]),
        )
        .expect("mediator starts");

        let client = UnixStream::connect(&front_path).expect("client connects");
        let mut reader = BufReader::new(client.try_clone().unwrap());
        let mut writer = client;

        let mut ask = |action: &str| {
            writeln!(writer, r#"{{"id":"1","action":"{action}"}}"#).unwrap();
            writer.flush().unwrap();
            let mut response = String::new();
            reader.read_line(&mut response).unwrap();
            response
        };

        assert!(ask("snapshot").contains("from the daemon"), "reads pass through");
        let refused = ask("evaluate");
        assert!(refused.contains("\"success\":false"), "{refused}");
        assert!(refused.contains("fail-closed"), "{refused}");

        // Give the connection thread a moment to record before we look.
        std::thread::sleep(std::time::Duration::from_millis(150));
        let actions = handle.actions();
        assert!(
            actions.iter().any(|a| a.action == "snapshot" && a.forwarded),
            "{actions:?}"
        );
        assert!(
            actions.iter().any(|a| a.action == "evaluate" && !a.forwarded),
            "{actions:?}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn the_no_daemon_refusal_echoes_the_request_id() {
        // Without the id, a CLI that correlates replies by id ignores this
        // line and hangs until its own timeout. The exact failure this
        // refusal exists to prevent.
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixStream;

        let (client, server) = UnixStream::pair().expect("socketpair");
        // The mediator's side reads the request and answers; drive it on a
        // thread because both halves are synchronous.
        let handle = std::thread::spawn(move || refuse_no_daemon(&server));

        let mut writer = client.try_clone().unwrap();
        writeln!(writer, r#"{{"id":"42","action":"launch"}}"#).unwrap();
        writer.flush().unwrap();

        let mut reply = String::new();
        BufReader::new(client).read_line(&mut reply).unwrap();
        handle.join().unwrap();

        let parsed: Value = serde_json::from_str(&reply).expect("a JSON reply");
        assert_eq!(parsed["id"], "42", "the reply must carry the request id");
        assert_eq!(parsed["success"], false);
    }

    /// `lines()` and `read_line` grow a `String` until they meet a newline, and
    /// both peers on this socket are inside the box. The client is the agent's
    /// CLI on a path in `<env>/tmp`, and the daemon is a process in the box too.
    /// So a line with no newline in it is the box choosing how much memory this
    /// *host* process allocates.
    ///
    /// Driven with a line four times the cap in each direction, which is enough
    /// to prove the ceiling exists without allocating anything a test notices.
    #[test]
    fn a_line_with_no_newline_in_it_cannot_grow_without_end() {
        let over = "x".repeat((MAX_LINE as usize) * 4);

        // Client side: the agent's CLI sends the flood.
        let mut out = Vec::new();
        let m = mediate(
            Cursor::new(over.clone().into_bytes()),
            &mut out,
            Cursor::new(Vec::new()),
            Vec::new(),
            Path::new("/nonexistent"),
            &ActionPolicy::default(),
        );
        let said = m.error.expect("an overlong client line must be refused");
        assert!(said.contains("no newline in it"), "{said}");
        assert!(said.contains("client"), "{said}");

        // Daemon side: a real request goes out and the answer is the flood.
        let mut out = Vec::new();
        let m = mediate(
            Cursor::new(req("1", "read").into_bytes()),
            &mut out,
            Cursor::new(over.into_bytes()),
            Vec::new(),
            Path::new("/nonexistent"),
            &ActionPolicy::default(),
        );
        let said = m.error.expect("an overlong daemon line must be refused");
        assert!(said.contains("no newline in it"), "{said}");
        assert!(said.contains("daemon"), "{said}");

        // And an ordinary line still round-trips, or the cap has eaten the
        // protocol rather than bounded it.
        let mut out = Vec::new();
        let m = mediate(
            Cursor::new(req("1", "read").into_bytes()),
            &mut out,
            fake_daemon(1),
            Vec::new(),
            Path::new("/nonexistent"),
            &ActionPolicy::default(),
        );
        assert_eq!(m.error, None, "a normal line must still pass");
        assert_eq!(m.forwarded(), 1);
    }

    /// The dribble the byte cap and the per-read timeout both miss.
    ///
    /// A peer that lets every `read()` *succeed*, one byte comfortably inside
    /// the per-read interval, forever, never trips that timeout, because a
    /// timeout is an error and a byte is not. `read_line` keeps going, and the
    /// only other bound is 64 KiB of bytes: at this rate that is a day and a
    /// half of a host thread.
    ///
    /// Driven at production's shape with the clock scaled down, which is why the
    /// bounds are parameters.
    #[test]
    #[cfg(unix)]
    fn a_dribbling_peer_cannot_hold_the_refusal_thread() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        let (client, server) = UnixStream::pair().expect("socketpair");
        let mut writer = client.try_clone().unwrap();
        let feeding = std::thread::spawn(move || {
            // Never a newline, and never a pause long enough to time a read
            // out. Stops well after the deadline should have fired.
            for _ in 0..200 {
                if writer.write_all(b"x").is_err() {
                    return;
                }
                let _ = writer.flush();
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        });

        let started = std::time::Instant::now();
        let line = read_first_line_within(
            &server,
            std::time::Duration::from_millis(100),
            std::time::Duration::from_millis(400),
            64 * 1024,
        );
        let took = started.elapsed();
        drop(server);
        let _ = feeding.join();

        assert!(
            took < std::time::Duration::from_secs(2),
            "the read ran for {took:?} against a 400ms deadline — the loop is bounded by \
             neither the per-read timeout nor the byte cap, and now by nothing else either"
        );
        // It read *something*. Otherwise the test would pass on a read that
        // never started, which proves nothing about the deadline.
        assert!(!line.is_empty(), "the dribble should have been read, not skipped");
        assert!(!line.contains('\n'), "the peer never sent one");
    }

    /// One thread per connection, and the socket is in `<env>/tmp`, which the
    /// box writes. Without a ceiling a loop of `connect()` inside the box
    /// spawns unbounded *host* threads; `auth_proxy` has bounded this since it
    /// was written and this sibling did not.
    #[test]
    #[cfg(unix)]
    fn the_front_serves_a_bounded_number_of_connections_at_once() {
        let td = TempDir::new().unwrap();
        let upstream = td.path().join("u.sock");
        let front = td.path().join("f.sock");
        // No daemon behind it: every accepted connection lands in the refusal
        // path, which is where a box would park them.
        let handle = spawn(&front, &upstream, td.path(), ActionPolicy::default()).unwrap();

        // Open more than the cap and keep them open. The ones past the ceiling
        // are closed by the front rather than queued, which is what the read
        // below observes: EOF with nothing written.
        let mut held = Vec::new();
        for _ in 0..(MAX_IN_FLIGHT + 16) {
            if let Ok(c) = std::os::unix::net::UnixStream::connect(&front) {
                held.push(c);
            }
        }
        assert!(
            held.len() >= MAX_IN_FLIGHT,
            "the front should accept up to its cap, got {}",
            held.len()
        );
        drop(held);
        drop(handle);
    }

    #[test]
    #[cfg(unix)]
    fn dropping_the_handle_removes_the_socket_it_bound() {
        let td = TempDir::new().unwrap();
        let upstream = td.path().join("u.sock");
        let front = td.path().join("f.sock");
        let _keep = std::os::unix::net::UnixListener::bind(&upstream).unwrap();

        let handle = spawn(&front, &upstream, td.path(), ActionPolicy::default()).unwrap();
        assert!(front.exists());
        drop(handle);
        assert!(!front.exists(), "a stale socket would fail the next bind");
    }

    #[test]
    fn mediated_actions_land_in_their_own_receipt_lane() {
        let td = TempDir::new().unwrap();
        record_actions(
            td.path(),
            "env/human/b",
            "digest123",
            &[
                ActionRecord {
                    action: "click".into(),
                    forwarded: true,
                    refused_because: None,
                },
                ActionRecord {
                    action: "evaluate".into(),
                    forwarded: false,
                    refused_because: Some("denied by policy".into()),
                },
            ],
        );

        let log = std::fs::read_to_string(td.path().join("receipt.jsonl"))
            .expect("a receipt was written");
        assert!(log.contains("\"source\":\"browser-proxy\""), "{log}");
        assert!(log.contains("1 refused"), "{log}");
    }

    #[test]
    fn a_run_that_never_touched_the_browser_writes_no_receipt() {
        let td = TempDir::new().unwrap();
        record_actions(td.path(), "env/human/b", "d", &[]);
        assert!(
            !td.path().join("receipt.jsonl").exists(),
            "an empty mediation must not grow a receipt per run"
        );
    }

    #[test]
    fn records_render_for_a_human_reading_the_receipt() {
        let forwarded = ActionRecord {
            action: "click".into(),
            forwarded: true,
            refused_because: None,
        };
        let refused = ActionRecord {
            action: "evaluate".into(),
            forwarded: false,
            refused_because: Some("denied by policy".into()),
        };
        assert!(forwarded.render().contains("click"));
        assert!(refused.render().starts_with("REFUSED"));
        assert!(refused.render().contains("denied by policy"));
    }
}
