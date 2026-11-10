//! Binding a verified kernel to the server, as `h5i-app-http` does for axum.
//!
//! [`DeviceEngine`] holds one tenant's snapshot and runs commands like
//! `h5i_app_core::MemoryEngine::execute_at`: [`Kernel::stamp`], `transition`,
//! then `apply`, one write at a time. Before a write is visible it is saved
//! to the engine's [`Store`] (flash, say); if saving fails, nothing changes.
//! Tasks on the device (an actuator, a sensor) see every committed state
//! through [`DeviceEngine::changed`] and run commands of their own through
//! [`DeviceEngine::execute`].
//!
//! A route runs a command through [`Calls::call`], which binds it to one
//! decoder: the router decides which decoder runs, so a path can only ever
//! produce the commands its own decoder produces. The kernel's theorems hold
//! for every command, so the trusted decoding can do no more than a client
//! could ask for.
//!
//! ```ignore
//! let k = Calls::<Thermostat, Wire, App>::new();
//! Router::new()
//!     .get(b"/state", k.call(open, |_| Ok(Command::Read)))
//!     .put(b"/mode", k.call(admin, decode_mode))
//! ```

use core::cell::RefCell;
use core::marker::PhantomData;
use core::task::Poll;

use critical_section::Mutex as CsMutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::waitqueue::MultiWakerRegistration;
use h5i_app_core::{Kernel, Timestamp};

use crate::{Handler, Request, Response};

/// Where committed snapshots are kept across resets.
pub trait Store<K: Kernel> {
    type Error: core::fmt::Debug;
    /// The last saved snapshot, if there is one.
    async fn load(&mut self) -> Result<Option<K::Snapshot>, Self::Error>;
    /// Save `snap`. When this returns `Ok`, a reset loads `snap` (or a
    /// later one). If it fails or is cancelled, a reset may load `snap` or
    /// the one before.
    async fn save(&mut self, snap: &K::Snapshot) -> Result<(), Self::Error>;
}

/// Keeps nothing: a reset starts from the initial snapshot.
pub struct NoStore;

impl<K: Kernel> Store<K> for NoStore {
    type Error = core::convert::Infallible;
    async fn load(&mut self) -> Result<Option<K::Snapshot>, Self::Error> {
        Ok(None)
    }
    async fn save(&mut self, _: &K::Snapshot) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Why a command was not committed.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused<E, S> {
    /// The kernel refused it.
    Kernel(E),
    /// The kernel accepted it, the store could not save the result: nothing
    /// changed.
    Store(S),
}

struct Watch {
    /// Committed writes since boot.
    version: u32,
    /// Time of the latest commit; a later `now` is never earlier.
    last: Timestamp,
    wakers: MultiWakerRegistration<4>,
}

/// One tenant's state on the device.
pub struct DeviceEngine<K: Kernel, St = NoStore> {
    /// The committed snapshot: what readers see and what the store holds.
    snap: CsMutex<RefCell<K::Snapshot>>,
    watch: CsMutex<RefCell<Watch>>,
    /// Held for the whole of a write, so writes are serialized; reads take
    /// only `snap`, briefly.
    store: Mutex<CriticalSectionRawMutex, St>,
}

impl<K: Kernel> DeviceEngine<K, NoStore> {
    pub const fn new(snap: K::Snapshot) -> Self {
        DeviceEngine::with_store(snap, NoStore)
    }
}

impl<K: Kernel, St: Store<K>> DeviceEngine<K, St> {
    /// Starts from `snap` until [`DeviceEngine::restore`] loads the stored
    /// one.
    pub const fn with_store(snap: K::Snapshot, store: St) -> Self {
        DeviceEngine {
            snap: CsMutex::new(RefCell::new(snap)),
            watch: CsMutex::new(RefCell::new(Watch {
                version: 0,
                last: Timestamp::EPOCH,
                wakers: MultiWakerRegistration::new(),
            })),
            store: Mutex::new(store),
        }
    }

    /// Load the stored snapshot, if any, in place of the initial one; call
    /// once at boot, before serving. Whether one was loaded.
    pub async fn restore(&self) -> Result<bool, St::Error> {
        let mut st = self.store.lock().await;
        match st.load().await? {
            Some(s) => {
                critical_section::with(|cs| *self.snap.borrow_ref_mut(cs) = s);
                self.notify();
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Run `cmd` as `actor`, stamped with `now` if given (raised to the
    /// last commit's time, as `MemoryEngine::execute_at` does). A
    /// [`Kernel::read_only`] command runs on the current snapshot without
    /// waiting for writes.
    pub async fn execute(
        &self,
        actor: &K::Principal,
        cmd: &K::Command,
        now: Option<Timestamp>,
    ) -> Result<K::Reply, Refused<K::Error, St::Error>> {
        if K::read_only(cmd) {
            let (actor, _) = self.stamped(actor, now);
            let snap = self.snapshot();
            return K::transition(&actor, &snap, cmd).map(|(_, r)| r).map_err(Refused::Kernel);
        }
        let mut st = self.store.lock().await;
        let (actor, now) = self.stamped(actor, now);
        let snap = self.snapshot();
        let (ws, reply) = K::transition(&actor, &snap, cmd).map_err(Refused::Kernel)?;
        let next = K::apply(&snap, &ws);
        st.save(&next).await.map_err(Refused::Store)?;
        critical_section::with(|cs| {
            *self.snap.borrow_ref_mut(cs) = next;
            if let Some(t) = now {
                self.watch.borrow_ref_mut(cs).last = t;
            }
        });
        drop(st);
        self.notify();
        Ok(reply)
    }

    fn stamped(&self, actor: &K::Principal, now: Option<Timestamp>) -> (K::Principal, Option<Timestamp>) {
        let mut actor = actor.clone();
        let now = now.map(|t| t.max(critical_section::with(|cs| self.watch.borrow_ref(cs).last)));
        if let Some(t) = now {
            K::stamp(&mut actor, t);
        }
        (actor, now)
    }

    fn notify(&self) {
        critical_section::with(|cs| {
            let mut w = self.watch.borrow_ref_mut(cs);
            w.version = w.version.wrapping_add(1);
            w.wakers.wake();
        });
    }

    /// The committed snapshot.
    pub fn snapshot(&self) -> K::Snapshot {
        critical_section::with(|cs| self.snap.borrow_ref(cs).clone())
    }

    /// How many writes (and restores) have committed since boot.
    pub fn version(&self) -> u32 {
        critical_section::with(|cs| self.watch.borrow_ref(cs).version)
    }

    /// Wait until the committed version differs from `*seen`, then return
    /// the snapshot and update `*seen`. Start with `seen = engine.version()`
    /// after acting on `engine.snapshot()`. Commits that land while the
    /// caller is busy are coalesced: it sees the latest one.
    pub async fn changed(&self, seen: &mut u32) -> K::Snapshot {
        core::future::poll_fn(|cx| {
            critical_section::with(|cs| {
                let mut w = self.watch.borrow_ref_mut(cs);
                if w.version != *seen {
                    *seen = w.version;
                    Poll::Ready(self.snap.borrow_ref(cs).clone())
                } else {
                    w.wakers.register(cx.waker());
                    Poll::Pending
                }
            })
        })
        .await
    }
}

/// Who sent a request (A1 for the device: trusted).
pub trait Authenticator<K: Kernel> {
    /// The principal, or the response to refuse with (401 with a
    /// `WWW-Authenticate` challenge, 403).
    #[allow(clippy::result_large_err)]
    fn authenticate(&self, req: &Request<'_>) -> Result<K::Principal, Response>;
}

/// Everyone, as one fixed principal: for routes any client may use.
pub struct Anyone<P>(pub P);

impl<K: Kernel> Authenticator<K> for Anyone<K::Principal> {
    #[allow(clippy::result_large_err)]
    fn authenticate(&self, _: &Request<'_>) -> Result<K::Principal, Response> {
        Ok(self.0.clone())
    }
}

/// `Authorization: Bearer <key>` against one pre-shared key, compared in
/// constant time, giving one fixed principal. The key crosses the link in
/// the clear on every request: use it only on a link nobody else can read
/// (a UART behind its gateway); on a network use [`crate::hmac::HmacAuth`]
/// (feature `hmac`), whose requests carry a MAC and not the key.
pub struct PskAuth<P> {
    pub key: &'static [u8],
    pub principal: P,
}

impl<K: Kernel> Authenticator<K> for PskAuth<K::Principal> {
    #[allow(clippy::result_large_err)]
    fn authenticate(&self, req: &Request<'_>) -> Result<K::Principal, Response> {
        let refuse = || Response::status(401).header(b"WWW-Authenticate", b"Bearer");
        let Some(v) = req.header(b"authorization") else {
            return Err(refuse());
        };
        let Some(tok) = v.strip_prefix(b"Bearer ") else {
            return Err(refuse());
        };
        if ct_eq(tok, self.key) { Ok(self.principal.clone()) } else { Err(refuse()) }
    }
}

/// Equal length and bytes, without an early exit on the first difference.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut d = 0u8;
    for i in 0..a.len() {
        d |= a[i] ^ b[i];
    }
    core::hint::black_box(d) == 0
}

/// The wire format of a kernel's replies and refusals (A3: trusted).
/// Commands are decoded per route ([`Calls::call`]).
pub trait Codec<K: Kernel> {
    /// The reply's body in `out`, or `None` if it does not fit.
    fn encode_reply(reply: &K::Reply, out: &mut [u8]) -> Option<usize>;
    /// A refusal's status and body.
    fn encode_error(err: &K::Error, out: &mut [u8]) -> (u16, usize);
    /// The content type of replies and errors.
    const CONTENT_TYPE: &'static [u8] = b"application/json";
}

/// What a kernel route's state gives it: the engine, and the time.
pub trait Device<K: Kernel> {
    type Store: Store<K>;
    fn engine(&self) -> &DeviceEngine<K, Self::Store>;
    /// The time to stamp commands with ([`Kernel::stamp`]); `None` (the
    /// default) runs them unstamped.
    fn now(&self) -> Option<Timestamp> {
        None
    }
}

/// A route's command decoder (A2: trusted).
pub type Decode<K> = for<'a, 'r> fn(&'a Request<'r>) -> Result<<K as Kernel>::Command, u16>;

type Marker<K, C, S> = PhantomData<fn() -> (K, C, S)>;

/// Builds kernel routes for kernel `K`, codec `C` and server state `S`.
pub struct Calls<K, C, S>(Marker<K, C, S>);

impl<K: Kernel, C: Codec<K>, S: Device<K>> Calls<K, C, S> {
    pub const fn new() -> Self {
        Calls(PhantomData)
    }

    /// A handler that authenticates with `auth(state)`, decodes with
    /// `decode`, runs the command and encodes the result.
    pub fn call<A: Authenticator<K>>(&self, auth: fn(&S) -> &A, decode: Decode<K>) -> Call<K, C, S, A> {
        Call { auth, decode, _k: PhantomData }
    }
}

impl<K: Kernel, C: Codec<K>, S: Device<K>> Default for Calls<K, C, S> {
    fn default() -> Self {
        Calls::new()
    }
}

/// A kernel route; see [`Calls::call`].
pub struct Call<K: Kernel, C, S, A> {
    auth: fn(&S) -> &A,
    decode: Decode<K>,
    _k: PhantomData<fn() -> C>,
}

impl<K, C, S, A> Handler<S> for Call<K, C, S, A>
where
    K: Kernel,
    C: Codec<K>,
    S: Device<K>,
    A: Authenticator<K>,
{
    async fn call(&self, req: &Request<'_>, state: &S, out: &mut [u8]) -> Response {
        respond::<K, C, S, A>(state, (self.auth)(state), self.decode, req, out).await
    }
}

/// Authenticate, decode, execute and encode: what a kernel route does. A
/// store failure is a 503.
pub async fn respond<K, C, S, A>(state: &S, auth: &A, decode: Decode<K>, req: &Request<'_>, out: &mut [u8]) -> Response
where
    K: Kernel,
    C: Codec<K>,
    S: Device<K>,
    A: Authenticator<K>,
{
    let actor = match auth.authenticate(req) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let cmd = match decode(req) {
        Ok(c) => c,
        Err(s) => return Response::status(s),
    };
    match state.engine().execute(&actor, &cmd, state.now()).await {
        Ok(reply) => match C::encode_reply(&reply, out) {
            Some(len) => Response::new(200, C::CONTENT_TYPE, len),
            None => Response::status(500),
        },
        Err(Refused::Kernel(e)) => {
            let (status, len) = C::encode_error(&e, out);
            Response::new(status, C::CONTENT_TYPE, len.min(out.len()))
        }
        Err(Refused::Store(_)) => Response::status(503),
    }
}
