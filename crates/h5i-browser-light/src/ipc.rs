//! The wire between the two halves.
//!
//! `h5i browser open` runs one process today and two after this: a **broker**
//! that holds the policy, the receipts, the jar, the budget and the secrets,
//! and a **renderer** that parses the page and holds none of them. The renderer
//! reaches the broker through [`crate::broker::Broker`], and this module is
//! that trait spoken over a socket.
//!
//! # Which process is which
//!
//! The broker is the parent. `h5i browser` spawns it exactly as it always
//! spawned the engine, and the broker spawns the renderer as a child with the
//! socket as its **standard input** — an ordinary inherited file descriptor, so
//! no library is needed to pass one, and no port exists for anything else on
//! the machine to connect to. Neither half is a subcommand: `h5i browser open`
//! is unchanged and there is nothing new to type, which is the same conclusion
//! §"The id is not the interface" reached about session ids, applied to
//! processes.
//!
//! # What a hostile renderer can do here
//!
//! Everything this protocol allows, in any order, with any arguments — so what
//! it allows is the whole of the security argument. It can ask for a URL and be
//! refused. It can ask what is in the log. It can ask for a cookie header it is
//! already entitled to (`document.cookie`'s non-`HttpOnly` subset). It cannot
//! edit the policy, silence the sink, read an `HttpOnly` cookie, enumerate a
//! secret's value, or reach the network without a receipt being written first,
//! because none of those is a message.
//!
//! What it *can* still do is lie about what it renders, which is why the split
//! is described as moving the recorder out of reach rather than as making the
//! renderer trustworthy.
//!
//! # Framing
//!
//! Each message is a length-prefixed JSON header and an optional blob:
//!
//! ```text
//! u32 header_len | u32 blob_len | header | blob
//! ```
//!
//! The blob carries request and response bodies. They are the only large thing
//! that crosses, and base64 inside the JSON would pay a third again in bytes
//! and all of it in allocation, on exactly the path a page's images take.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex};

use h5i_error::H5iError;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::broker::{Allowance, Broker, Channel, Fetch};
use crate::net::{FetchOutcome, LocalBroker};
use crate::receipt::RequestRecord;
use crate::secrets::Resolved;
use crate::wsclient::Event;

/// Callers waiting for an answer, by the id they asked under.
///
/// The reply thread is the only reader of the socket, so every caller parks a
/// one-shot here and takes its own answer off it. Dropping the map is how a
/// broker that went away wakes everybody at once.
type Waiting = Arc<Mutex<HashMap<u64, SyncSender<(Said, Vec<u8>)>>>>;

/// The largest header or body one message may carry.
///
/// A ceiling on what either half can make the other allocate. Response bodies
/// are already capped far below this by the page budget
/// (`--max-wire-bytes`), so this is the backstop for a message that got past
/// that or was never subject to it, not the working limit.
const MAX_FRAME: usize = 256 * 1024 * 1024;

/// What the renderer asks for. One variant per operation on
/// [`crate::broker::Broker`], and nothing that is not one.
#[derive(Debug, Serialize, Deserialize)]
enum Ask {
    /// The request body travels in the blob, never in this.
    Send(Fetch),
    Records,
    HighWater,
    Since {
        mark: Option<u64>,
    },
    Budget,
    ResetBudget,
    CookieCount,
    DocumentCookie {
        url: Url,
    },
    StoreCookie {
        url: Url,
        header: String,
    },
    KeepOnlyOrigin {
        origin: Url,
    },
    OpenSocket {
        url: Url,
        document: Option<Url>,
    },
    OpenEventStream {
        url: Url,
        document: Option<Url>,
    },
    ChannelSend {
        channel: u64,
        text: String,
    },
    ChannelDrain {
        channel: u64,
    },
    ChannelClose {
        channel: u64,
    },
    SecretNames,
    Substitute {
        text: String,
    },
    Redact {
        text: String,
    },
    RedactAll {
        texts: Vec<String>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct Question {
    id: u64,
    ask: Ask,
}

/// What the broker answers. Never volunteered: there is one of these per
/// question, which is what lets the renderer poll rather than hold a queue a
/// chatty server could grow.
#[derive(Debug, Serialize, Deserialize)]
enum Said {
    /// The response body travels in the blob.
    Outcome(FetchOutcome),
    Records(Vec<RequestRecord>),
    Seqs(Vec<u64>),
    Mark(Option<u64>),
    Budget(Allowance),
    Count(usize),
    Text(String),
    Flag(bool),
    Texts(Vec<String>),
    Resolved(Resolved),
    /// A channel id, or why there is none.
    Channel(Result<u64, String>),
    Events(Vec<Event>),
    Sent(Result<(), String>),
    Done,
}

#[derive(Debug, Serialize, Deserialize)]
struct Answer {
    id: u64,
    said: Said,
}

// ── framing ──────────────────────────────────────────────────────────────────

fn write_frame(out: &mut impl Write, header: &[u8], blob: &[u8]) -> std::io::Result<()> {
    let mut prefix = [0u8; 8];
    prefix[..4].copy_from_slice(&(header.len() as u32).to_be_bytes());
    prefix[4..].copy_from_slice(&(blob.len() as u32).to_be_bytes());
    out.write_all(&prefix)?;
    out.write_all(header)?;
    out.write_all(blob)?;
    out.flush()
}

fn read_frame(input: &mut impl Read) -> std::io::Result<(Vec<u8>, Vec<u8>)> {
    let mut prefix = [0u8; 8];
    input.read_exact(&mut prefix)?;
    let header_len = u32::from_be_bytes(prefix[..4].try_into().unwrap()) as usize;
    let blob_len = u32::from_be_bytes(prefix[4..].try_into().unwrap()) as usize;
    if header_len > MAX_FRAME || blob_len > MAX_FRAME {
        return Err(std::io::Error::other(format!(
            "a message of {header_len}+{blob_len} bytes is over the {MAX_FRAME}-byte ceiling"
        )));
    }
    let mut header = vec![0u8; header_len];
    input.read_exact(&mut header)?;
    let mut blob = vec![0u8; blob_len];
    input.read_exact(&mut blob)?;
    Ok((header, blob))
}

// ── the renderer's side ──────────────────────────────────────────────────────

/// What the renderer holds instead of a broker.
///
/// Every method is a round trip. Calls are multiplexed rather than serialized:
/// the script realm starts several fetches at once and a lock held across a
/// whole request would turn them back into a queue — and worse, would let one
/// slow fetch stall the drain of a socket that was answering fine.
pub struct BrokerClient {
    /// A handle to itself, for the two operations that hand back something the
    /// caller keeps: an open connection needs the client for as long as the
    /// page holds it, and the trait method that opens one has only `&self`.
    me: std::sync::Weak<BrokerClient>,
    out: Mutex<Box<dyn Write + Send>>,
    next: AtomicU64,
    waiting: Waiting,
    gone: Arc<AtomicBool>,
}

impl BrokerClient {
    /// The renderer's broker, on the descriptor it was handed.
    ///
    /// `fatal` is what a real renderer wants: a renderer whose broker has gone
    /// away cannot fetch, cannot receipt, and has a parent that is no longer
    /// waiting for it, so it stops rather than carrying on as a browser that
    /// can no longer say what it did. Tests pass `false` and get an error per
    /// call instead of an exit.
    fn over(
        reader: Box<dyn Read + Send>,
        writer: Box<dyn Write + Send>,
        fatal: bool,
    ) -> Arc<Self> {
        let waiting: Waiting = Arc::new(Mutex::new(HashMap::new()));
        let gone = Arc::new(AtomicBool::new(false));
        let client = Arc::new_cyclic(|me| Self {
            me: me.clone(),
            out: Mutex::new(writer),
            next: AtomicU64::new(0),
            waiting: waiting.clone(),
            gone: gone.clone(),
        });

        let _ = std::thread::Builder::new()
            .name("h5i-broker-replies".to_string())
            .spawn(move || {
                let mut reader = reader;
                while let Ok((header, blob)) = read_frame(&mut reader) {
                    let Ok(answer) = serde_json::from_slice::<Answer>(&header) else {
                        break;
                    };
                    let waiter = waiting.lock().ok().and_then(|mut w| w.remove(&answer.id));
                    if let Some(waiter) = waiter {
                        let _ = waiter.send((answer.said, blob));
                    }
                }
                gone.store(true, Ordering::SeqCst);
                // Waking every caller: dropping the senders makes each blocked
                // `recv` return an error rather than wait for an answer that is
                // never coming.
                if let Ok(mut waiting) = waiting.lock() {
                    waiting.clear();
                }
                if fatal {
                    eprintln!(
                        "h5i-browser-light: the broker process ended; this renderer cannot fetch \
                         or receipt without it, so it is stopping."
                    );
                    std::process::exit(70);
                }
            });

        client
    }

    /// The renderer's broker is its standard input: the broker spawned this
    /// process with one end of a socket pair there.
    #[cfg(unix)]
    pub fn on_stdin() -> Result<Arc<Self>, H5iError> {
        use std::os::unix::io::{AsRawFd, FromRawFd};
        // Borrowed, never owned: closing stdin out from under the process would
        // hand fd 0 to whatever opened a file next.
        let stdin = std::io::stdin();
        let socket = unsafe { std::os::unix::net::UnixStream::from_raw_fd(stdin.as_raw_fd()) };
        let reader = socket.try_clone().map_err(|e| {
            H5iError::Metadata(format!("the broker socket could not be cloned: {e}"))
        })?;
        // The original is deliberately leaked rather than dropped, for the same
        // reason: it *is* fd 0, and its destructor would close it.
        std::mem::forget(socket);
        let writer = reader.try_clone().map_err(|e| {
            H5iError::Metadata(format!("the broker socket could not be cloned: {e}"))
        })?;
        Ok(Self::over(Box::new(reader), Box::new(writer), true))
    }

    /// Ask, and wait for the one answer.
    ///
    /// `None` when the broker is gone. Every caller turns that into the
    /// refusal its own signature can express, because "the broker is gone" is
    /// not a state any of them can render around.
    fn ask(&self, ask: Ask, blob: &[u8]) -> Option<(Said, Vec<u8>)> {
        if self.gone.load(Ordering::SeqCst) {
            return None;
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = sync_channel(1);
        self.waiting.lock().ok()?.insert(id, tx);

        let header = serde_json::to_vec(&Question { id, ask }).ok()?;
        let written = {
            let mut out = self.out.lock().ok()?;
            write_frame(&mut *out, &header, blob)
        };
        if written.is_err() {
            self.waiting.lock().ok()?.remove(&id);
            return None;
        }
        rx.recv().ok()
    }

    /// The answers whose shape is a single value, unwrapped.
    fn said(&self, ask: Ask) -> Option<Said> {
        self.ask(ask, &[]).map(|(said, _)| said)
    }
}

/// A connection the broker holds and this process may use.
struct RemoteChannel {
    client: Arc<BrokerClient>,
    id: u64,
}

impl Channel for RemoteChannel {
    fn send(&self, text: &str) -> Result<(), String> {
        match self.client.said(Ask::ChannelSend {
            channel: self.id,
            text: text.to_string(),
        }) {
            Some(Said::Sent(result)) => result,
            _ => Err("the broker is no longer running".to_string()),
        }
    }

    fn drain(&self) -> Vec<Event> {
        match self.client.said(Ask::ChannelDrain { channel: self.id }) {
            Some(Said::Events(events)) => events,
            _ => Vec::new(),
        }
    }

    fn close(&self) {
        let _ = self.client.said(Ask::ChannelClose { channel: self.id });
    }
}

/// Dropping the page's last handle closes the connection, which is what the
/// same handle did when the socket lived in this process. Without it the broker
/// would hold an open socket for a page that has navigated away.
impl Drop for RemoteChannel {
    fn drop(&mut self) {
        self.close();
    }
}

impl Broker for BrokerClient {
    fn send(&self, fetch: &Fetch) -> FetchOutcome {
        match self.ask(Ask::Send(fetch.clone()), &fetch.body) {
            Some((Said::Outcome(mut outcome), body)) => {
                outcome.body = body;
                outcome
            }
            _ => FetchOutcome::refused(
                fetch.url.clone(),
                "the broker is no longer running, so nothing was fetched".to_string(),
            ),
        }
    }

    fn records(&self) -> Vec<RequestRecord> {
        match self.said(Ask::Records) {
            Some(Said::Records(records)) => records,
            _ => Vec::new(),
        }
    }

    fn high_water(&self) -> Option<u64> {
        match self.said(Ask::HighWater) {
            Some(Said::Mark(mark)) => mark,
            _ => None,
        }
    }

    fn since(&self, mark: Option<u64>) -> Vec<u64> {
        match self.said(Ask::Since { mark }) {
            Some(Said::Seqs(seqs)) => seqs,
            _ => Vec::new(),
        }
    }

    fn budget(&self) -> Allowance {
        match self.said(Ask::Budget) {
            Some(Said::Budget(allowance)) => allowance,
            _ => Allowance {
                spent: crate::budget::Budget::default().spent(),
                limits: crate::budget::Limits::default(),
            },
        }
    }

    fn reset_budget(&self) {
        let _ = self.said(Ask::ResetBudget);
    }

    fn cookie_count(&self) -> usize {
        match self.said(Ask::CookieCount) {
            Some(Said::Count(n)) => n,
            _ => 0,
        }
    }

    fn document_cookie(&self, url: &Url) -> String {
        match self.said(Ask::DocumentCookie { url: url.clone() }) {
            Some(Said::Text(text)) => text,
            _ => String::new(),
        }
    }

    fn store_cookie(&self, url: &Url, header: &str) -> usize {
        match self.said(Ask::StoreCookie {
            url: url.clone(),
            header: header.to_string(),
        }) {
            Some(Said::Count(n)) => n,
            _ => 0,
        }
    }

    fn keep_only_origin(&self, origin: &Url) -> bool {
        match self.said(Ask::KeepOnlyOrigin {
            origin: origin.clone(),
        }) {
            Some(Said::Flag(dropped)) => dropped,
            _ => false,
        }
    }

    fn open_socket(&self, url: &Url, document: Option<&Url>) -> Result<Arc<dyn Channel>, String> {
        self.channel(Ask::OpenSocket {
            url: url.clone(),
            document: document.cloned(),
        })
    }

    fn open_event_stream(
        &self,
        url: &Url,
        document: Option<&Url>,
    ) -> Result<Arc<dyn Channel>, String> {
        self.channel(Ask::OpenEventStream {
            url: url.clone(),
            document: document.cloned(),
        })
    }

    fn secret_names(&self) -> Vec<String> {
        match self.said(Ask::SecretNames) {
            Some(Said::Texts(names)) => names,
            _ => Vec::new(),
        }
    }

    fn substitute(&self, text: &str) -> Resolved {
        match self.said(Ask::Substitute {
            text: text.to_string(),
        }) {
            Some(Said::Resolved(resolved)) => resolved,
            // The text unchanged, and nothing claimed as used. A placeholder
            // that resolved to nothing is already a case this engine handles —
            // it is left as written — so a broker that cannot answer degrades
            // into it rather than into a value nobody checked.
            _ => Resolved {
                text: text.to_string(),
                used: Vec::new(),
                missing: Vec::new(),
            },
        }
    }

    fn redact(&self, text: &str) -> String {
        match self.said(Ask::Redact {
            text: text.to_string(),
        }) {
            Some(Said::Text(redacted)) => redacted,
            _ => text.to_string(),
        }
    }

    fn redact_all(&self, texts: &[String]) -> Vec<String> {
        match self.said(Ask::RedactAll {
            texts: texts.to_vec(),
        }) {
            // Length-checked: the strings are put back into a reply by
            // position, and a short answer would shift every value after the
            // gap into the wrong field.
            Some(Said::Texts(redacted)) if redacted.len() == texts.len() => redacted,
            _ => texts.to_vec(),
        }
    }
}

impl BrokerClient {
    /// Open one of the two kinds of connection. Both answer the same shape, so
    /// there is one place that turns an id into a handle.
    fn channel(&self, ask: Ask) -> Result<Arc<dyn Channel>, String> {
        // A second `Arc` around this client would not do: the reply thread
        // belongs to the first one, so the handle has to reach the same client
        // the caller already holds. See `me`.
        let client = self
            .me
            .upgrade()
            .ok_or_else(|| "the broker connection is closing".to_string())?;
        match self.said(ask) {
            Some(Said::Channel(Ok(id))) => Ok(Arc::new(RemoteChannel { client, id })),
            Some(Said::Channel(Err(why))) => Err(why),
            _ => Err("the broker is no longer running".to_string()),
        }
    }
}

// ── the broker's side ────────────────────────────────────────────────────────

/// Answer questions until the renderer stops asking.
///
/// Returns when the socket closes, which is what a renderer that exited looks
/// like from here. Cheap answers are given on this thread; the ones that touch
/// the wire get one of their own, because a fetch that took thirty seconds
/// would otherwise stall every drain and every cookie read behind it.
pub fn serve(broker: Arc<LocalBroker>, socket: std::os::unix::net::UnixStream) {
    let Ok(reader) = socket.try_clone() else {
        return;
    };
    let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(Box::new(socket)));
    let desk = Arc::new(Desk {
        broker,
        channels: Mutex::new(HashMap::new()),
        next_channel: AtomicU64::new(0),
    });
    serve_over(desk, Box::new(reader), writer);
}

/// The transport-independent half, so the protocol can be tested over a pipe.
fn serve_over(
    desk: Arc<Desk>,
    mut reader: Box<dyn Read + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
) {
    let mut workers: Vec<std::thread::JoinHandle<()>> = Vec::new();
    while let Ok((header, blob)) = read_frame(&mut reader) {
        let Ok(question) = serde_json::from_slice::<Question>(&header) else {
            break;
        };
        if slow(&question.ask) {
            let hand = (desk.clone(), writer.clone());
            match std::thread::Builder::new()
                .name("h5i-broker-call".to_string())
                .spawn(move || answer(&hand.0, question, blob, &hand.1))
            {
                Ok(worker) => workers.push(worker),
                // Out of threads is not a reason to leave the renderer waiting
                // for an answer that will never come. The question is gone with
                // the closure, so the renderer is told that rather than left to
                // time out on a message nobody will reply to.
                Err(e) => eprintln!(
                    "h5i-browser-light: the broker could not start a worker thread: {e}"
                ),
            }
            workers.retain(|w| !w.is_finished());
        } else {
            answer(&desk, question, blob, &writer);
        }
    }
    // A renderer that has gone away takes its connections with it: nothing can
    // drain them, and a socket nobody reads is a server left talking to itself.
    if let Ok(mut channels) = desk.channels.lock() {
        for (_, channel) in channels.drain() {
            channel.close();
        }
    }
    for worker in workers {
        let _ = worker.join();
    }
}

/// Whether answering this means waiting on the network.
fn slow(ask: &Ask) -> bool {
    matches!(
        ask,
        Ask::Send(_) | Ask::OpenSocket { .. } | Ask::OpenEventStream { .. } | Ask::ChannelSend { .. }
    )
}

/// What the broker keeps on behalf of one renderer.
struct Desk {
    broker: Arc<LocalBroker>,
    /// Connections opened for the renderer, by the id it holds. The renderer
    /// names an id; it never holds a socket.
    channels: Mutex<HashMap<u64, Arc<dyn Channel>>>,
    next_channel: AtomicU64,
}

fn answer(
    desk: &Desk,
    question: Question,
    blob: Vec<u8>,
    writer: &Mutex<Box<dyn Write + Send>>,
) {
    let broker: &dyn Broker = desk.broker.as_ref();
    let mut body = Vec::new();
    let said = match question.ask {
        Ask::Send(mut fetch) => {
            fetch.body = blob;
            let mut outcome = broker.send(&fetch);
            body = std::mem::take(&mut outcome.body);
            Said::Outcome(outcome)
        }
        Ask::Records => Said::Records(broker.records()),
        Ask::HighWater => Said::Mark(broker.high_water()),
        Ask::Since { mark } => Said::Seqs(broker.since(mark)),
        Ask::Budget => Said::Budget(broker.budget()),
        Ask::ResetBudget => {
            broker.reset_budget();
            Said::Done
        }
        Ask::CookieCount => Said::Count(broker.cookie_count()),
        Ask::DocumentCookie { url } => Said::Text(broker.document_cookie(&url)),
        Ask::StoreCookie { url, header } => Said::Count(broker.store_cookie(&url, &header)),
        Ask::KeepOnlyOrigin { origin } => Said::Flag(broker.keep_only_origin(&origin)),
        Ask::OpenSocket { url, document } => {
            Said::Channel(desk.open(broker.open_socket(&url, document.as_ref())))
        }
        Ask::OpenEventStream { url, document } => {
            Said::Channel(desk.open(broker.open_event_stream(&url, document.as_ref())))
        }
        Ask::ChannelSend { channel, text } => Said::Sent(match desk.channel(channel) {
            Some(channel) => channel.send(&text),
            None => Err("that connection is not open".to_string()),
        }),
        Ask::ChannelDrain { channel } => Said::Events(match desk.channel(channel) {
            Some(channel) => channel.drain(),
            None => Vec::new(),
        }),
        Ask::ChannelClose { channel } => {
            let taken = desk.channels.lock().ok().and_then(|mut c| c.remove(&channel));
            if let Some(channel) = taken {
                channel.close();
            }
            Said::Done
        }
        Ask::SecretNames => Said::Texts(broker.secret_names()),
        Ask::Substitute { text } => Said::Resolved(broker.substitute(&text)),
        Ask::Redact { text } => Said::Text(broker.redact(&text)),
        Ask::RedactAll { texts } => Said::Texts(broker.redact_all(&texts)),
    };

    let Ok(header) = serde_json::to_vec(&Answer {
        id: question.id,
        said,
    }) else {
        return;
    };
    if let Ok(mut writer) = writer.lock() {
        let _ = write_frame(&mut *writer, &header, &body);
    }
}

impl Desk {
    fn open(&self, opened: Result<Arc<dyn Channel>, String>) -> Result<u64, String> {
        let channel = opened?;
        let id = self.next_channel.fetch_add(1, Ordering::Relaxed);
        self.channels
            .lock()
            .map_err(|_| "the broker's connection table is poisoned".to_string())?
            .insert(id, channel);
        Ok(id)
    }

    fn channel(&self, id: u64) -> Option<Arc<dyn Channel>> {
        self.channels.lock().ok()?.get(&id).cloned()
    }
}

// ── starting the renderer ────────────────────────────────────────────────────

/// The subcommand `h5i` hides the engine behind. The broker re-execs the binary
/// it is already running, so there is no second file to find, no version skew
/// between the halves, and no path to get wrong.
const ENGINE_SUBCOMMAND: &str = "__engine";

/// The flag that tells the child it is the renderer. Hidden, and not an
/// interface: nobody types it, and `h5i browser open` is unchanged.
pub const RENDERER_FLAG: &str = "--brokered";

/// Set to anything to run the engine as one process, the way it ran before the
/// split. An escape hatch for a host where spawning is the problem, and a way
/// to compare the two halves against one.
pub const NO_SPLIT_VAR: &str = "H5I_BROWSER_NO_SPLIT";

/// Environment the renderer does not get.
///
/// The concrete half of what the split buys, and the part that is true the
/// moment the second process exists. A compromised engine used to read every
/// `H5I_SECRET_*` on the machine because it *was* the process holding them;
/// now it reads the values it was handed for the fields it was told to fill.
/// The other three are configuration a renderer has no use for: the receipts
/// path names a file only the broker writes, the proxy is the broker's wire,
/// and the allowlist is the broker's decision.
fn scrubbed(name: &str) -> bool {
    name.starts_with(crate::secrets::PREFIX)
        || matches!(
            name,
            "H5I_BROWSER_RECEIPTS" | "H5I_EGRESS_PROXY" | "H5I_BROWSER_ALLOW"
        )
}

/// Whether this host and this invocation will run the two halves as two
/// processes.
#[cfg(unix)]
pub fn splitting() -> bool {
    std::env::var_os(NO_SPLIT_VAR).is_none()
}

#[cfg(not(unix))]
pub fn splitting() -> bool {
    false
}

/// Start the renderer, and hand back the socket to speak to it over.
///
/// `argv` is the engine's own command line, program name included: the child runs
/// the same command this process was given, with the flag that says which half
/// it is. Passing the arguments through unchanged is deliberate — two halves
/// that parsed different command lines would be two engines that could disagree
/// about what was asked for.
#[cfg(unix)]
pub fn spawn_renderer(
    argv: &[std::ffi::OsString],
) -> Result<(std::process::Child, std::os::unix::net::UnixStream), H5iError> {
    use std::os::unix::net::UnixStream;
    use std::process::{Command, Stdio};

    let exe = std::env::current_exe()
        .map_err(|e| H5iError::Metadata(format!("this binary could not name itself: {e}")))?;
    let (mine, theirs) = UnixStream::pair()
        .map_err(|e| H5iError::Metadata(format!("the broker socket could not be made: {e}")))?;

    let mut command = Command::new(exe);
    command.arg(ENGINE_SUBCOMMAND).arg(RENDERER_FLAG);
    command.args(argv.iter().skip(1));
    // The socket arrives as the renderer's standard input. An inherited
    // descriptor rather than a path or a port: there is nothing on the machine
    // that could connect to it, and nothing to clean up if either half dies.
    command.stdin(Stdio::from(std::os::fd::OwnedFd::from(theirs)));
    for (name, _) in std::env::vars_os() {
        if scrubbed(&name.to_string_lossy()) {
            command.env_remove(&name);
        }
    }

    let child = command
        .spawn()
        .map_err(|e| H5iError::Metadata(format!("the renderer could not be started: {e}")))?;
    Ok((child, mine))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::Fetch;
    use crate::policy::Policy;
    use crate::receipt::{Initiator, MemorySink};

    /// A client and a broker in one process, talking over two pipes.
    ///
    /// Pipes rather than a socket pair, so the framing is exercised on a
    /// transport with no message boundaries of its own — which is what a
    /// stream socket is too, and what a protocol that read one message per
    /// `read` would get away with here and fail on under load.
    fn paired() -> (Arc<BrokerClient>, Arc<LocalBroker>, std::thread::JoinHandle<()>) {
        let (to_broker_r, to_broker_w) = std::io::pipe().expect("pipe");
        let (to_client_r, to_client_w) = std::io::pipe().expect("pipe");

        let broker = LocalBroker::new(Policy::new(), Arc::new(MemorySink::new()), None)
            .expect("broker builds");
        let desk = Arc::new(Desk {
            broker: broker.clone(),
            channels: Mutex::new(HashMap::new()),
            next_channel: AtomicU64::new(0),
        });
        let served = std::thread::spawn(move || {
            let writer: Arc<Mutex<Box<dyn Write + Send>>> =
                Arc::new(Mutex::new(Box::new(to_client_w)));
            serve_over(desk, Box::new(to_broker_r), writer);
        });

        let client = BrokerClient::over(Box::new(to_client_r), Box::new(to_broker_w), false);
        (client, broker, served)
    }

    #[test]
    fn a_refusal_crosses_the_wire_as_a_refusal() {
        let (client, broker, _served) = paired();
        // The allowlist is empty, so this is denied — and the point is that the
        // *broker* denied it: the renderer asked, and what came back is the
        // refusal plus a receipt written on the far side.
        let url = Url::parse("https://denied.test/page").expect("url");
        let outcome = client.send(&Fetch::get(&url, Initiator::Navigation));
        assert!(!outcome.is_ok(), "{outcome:?}");
        assert!(
            outcome.error.as_deref().unwrap_or_default().contains("policy"),
            "{outcome:?}"
        );

        // Recorded where the recorder lives, and readable from the renderer's
        // side only by asking.
        let records = Broker::records(broker.as_ref());
        assert!(!records.is_empty(), "the broker recorded the refusal");
        assert_eq!(client.records().len(), records.len());
    }

    #[test]
    fn the_jar_answers_operations_and_not_a_reference() {
        let (client, broker, _served) = paired();
        let url = Url::parse("https://acme.test/").expect("url");
        broker
            .jar()
            .store(&url, ["sid=secret; HttpOnly", "theme=dark"]);

        // Two cookies are held; one is visible to script. The renderer can
        // learn the count and the visible subset, and there is no message that
        // would hand it `sid`.
        assert_eq!(client.cookie_count(), 2);
        let visible = client.document_cookie(&url);
        assert!(visible.contains("theme=dark"), "{visible}");
        assert!(!visible.contains("secret"), "{visible}");
    }

    #[test]
    fn secrets_resolve_on_the_broker_side() {
        let (client, _broker, _served) = paired();
        // Nothing is set in this process's environment, so the placeholder
        // resolves to nothing and is left as written — the documented shape for
        // a name that means nothing here.
        let resolved = client.substitute("$H5I_SECRET_NOTHING");
        assert_eq!(resolved.text, "$H5I_SECRET_NOTHING");
        assert!(!resolved.substituted());
    }

    #[test]
    fn a_broker_that_went_away_refuses_rather_than_hangs() {
        // A client whose broker never answers: the read end sees EOF the
        // moment the write end is dropped. A renderer must not park on an
        // answer that is not coming — in the real one this is fatal and the
        // process stops, which is why the refusal has to be reachable at all.
        let (reader, writer) = std::io::pipe().expect("pipe");
        drop(writer);
        let (_unread, sink) = std::io::pipe().expect("pipe");
        let client = BrokerClient::over(Box::new(reader), Box::new(sink), false);

        let url = Url::parse("https://docs.test/").expect("url");
        let outcome = client.send(&Fetch::get(&url, Initiator::Navigation));
        assert!(!outcome.is_ok());
        assert!(
            outcome
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("no longer running"),
            "{outcome:?}"
        );
        // And nothing was fetched, which is the half that matters: a renderer
        // with no broker is a renderer with no network.
        assert!(outcome.body.is_empty());
    }

    #[test]
    fn a_connection_the_renderer_invented_is_not_one_it_holds() {
        let (client, _broker, _served) = paired();
        // Channel ids come from the broker. A renderer naming one it was never
        // given is the shape of every "guess the handle" attack, and the answer
        // is that there is nothing at that id rather than a panic or a stray
        // socket.
        let invented = RemoteChannel {
            client: client.clone(),
            id: 4242,
        };
        assert!(invented.send("hello").is_err());
        assert!(invented.drain().is_empty());
        // Closing one that does not exist is not an error either: a page may
        // close a socket the broker already dropped.
        invented.close();
    }

    #[test]
    fn a_socket_is_the_brokers_and_the_renderer_holds_a_handle() {
        // The streaming half of the seam, which is the part that could not be
        // transported as a call. The socket is dialled, held and receipted on
        // the broker's side; the renderer sends on it and drains it by naming
        // an id, and every frame is in the broker's log because that is where
        // the wire is.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept()
                && crate::ws::accept(&mut stream).is_ok()
            {
                let _ = crate::ws::send_text(&mut stream, "hello from the server");
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
        });

        let (client, broker, _served) = paired();
        let url = Url::parse(&format!("ws://127.0.0.1:{port}/hmr")).expect("url");
        let channel = client.open_socket(&url, None).expect("the socket opened");

        // The greeting arrives on real time, so this waits for it rather than
        // assuming a schedule.
        let mut seen: Vec<Event> = Vec::new();
        for _ in 0..40 {
            seen.extend(channel.drain());
            if seen.iter().any(|e| matches!(e, Event::Message(_))) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert!(
            seen.contains(&Event::Message("hello from the server".to_string())),
            "{seen:?}"
        );
        assert!(channel.send("and back").is_ok());
        channel.close();
        let _ = server.join();

        // WS-OPEN, then a frame in each direction: the receipts are the
        // broker's, written where the bytes actually crossed.
        let methods: Vec<String> = Broker::records(broker.as_ref())
            .iter()
            .map(|r| r.method.clone())
            .collect();
        assert!(methods.iter().any(|m| m == "WS-OPEN"), "{methods:?}");
        assert!(methods.iter().any(|m| m == "WS-RECV"), "{methods:?}");
        assert!(methods.iter().any(|m| m == "WS-SEND"), "{methods:?}");
    }

    #[test]
    fn a_credential_is_not_in_the_renderers_environment() {
        // The concrete thing the second process buys, pinned as a list. Every
        // name here is one a compromised parser used to be able to read simply
        // by being the process that held it.
        assert!(scrubbed("H5I_SECRET_ACME_PASSWORD"));
        assert!(scrubbed("H5I_BROWSER_RECEIPTS"));
        assert!(scrubbed("H5I_EGRESS_PROXY"));
        assert!(scrubbed("H5I_BROWSER_ALLOW"));

        // And the ones the renderer still needs, because it is the half that
        // draws the page and serves the control channel.
        assert!(!scrubbed("H5I_BROWSER_STREAM_FILE"));
        assert!(!scrubbed("H5I_BROWSER_CONTROL_SOCKET"));
        assert!(!scrubbed("HOME"));
        assert!(!scrubbed("PATH"));
    }
}
