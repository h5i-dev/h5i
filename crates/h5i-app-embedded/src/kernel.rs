//! Binding a verified kernel to the server, as `h5i-app-http` does for axum.
//!
//! [`DeviceEngine`] holds one tenant's snapshot and runs commands exactly
//! like `h5i_app_core::MemoryEngine::execute`: `transition`, then `apply`,
//! under a lock, so commands are serialized. The kernel's theorems hold for
//! every command, so the trusted decoding here ([`Api::decode_command`]) can
//! do no more than a client could ask for.

use core::cell::RefCell;

use critical_section::Mutex;
use h5i_app_core::Kernel;

use crate::{Request, Response};

/// One tenant's state on the device.
pub struct DeviceEngine<K: Kernel> {
    snap: Mutex<RefCell<K::Snapshot>>,
}

impl<K: Kernel> DeviceEngine<K> {
    pub const fn new(snap: K::Snapshot) -> Self {
        DeviceEngine { snap: Mutex::new(RefCell::new(snap)) }
    }

    /// `transition`, then `apply` on success, atomically.
    pub fn execute(&self, actor: &K::Principal, cmd: &K::Command) -> Result<K::Reply, K::Error> {
        critical_section::with(|cs| {
            let mut s = self.snap.borrow_ref_mut(cs);
            let (ws, reply) = K::transition(actor, &s, cmd)?;
            let next = K::apply(&s, &ws);
            *s = next;
            Ok(reply)
        })
    }

    pub fn snapshot(&self) -> K::Snapshot {
        critical_section::with(|cs| self.snap.borrow_ref(cs).clone())
    }
}

/// Who sent a request (A1 for the device: trusted).
pub trait Authenticator<K: Kernel> {
    /// The principal, or the status to answer with (401, 403).
    fn authenticate(&self, req: &Request<'_>) -> Result<K::Principal, u16>;
}

/// `Authorization: Bearer <key>` against one pre-shared key, compared in
/// constant time, giving one fixed principal.
pub struct PskAuth<P> {
    pub key: &'static [u8],
    pub principal: P,
}

impl<K: Kernel> Authenticator<K> for PskAuth<K::Principal> {
    fn authenticate(&self, req: &Request<'_>) -> Result<K::Principal, u16> {
        let Some(v) = req.header(b"authorization") else { return Err(401) };
        let Some(tok) = v.strip_prefix(b"Bearer ") else { return Err(401) };
        if ct_eq(tok, self.key) { Ok(self.principal.clone()) } else { Err(401) }
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

/// The wire format of a kernel's commands and replies (A2, A3: trusted).
pub trait Api<K: Kernel> {
    /// The command a request asks for, or the status to refuse it with.
    fn decode_command(req: &Request<'_>) -> Result<K::Command, u16>;
    /// The reply's body in `out`, or `None` if it does not fit.
    fn encode_reply(reply: &K::Reply, out: &mut [u8]) -> Option<usize>;
    /// A refusal's status and body.
    fn encode_error(err: &K::Error, out: &mut [u8]) -> (u16, usize);
    /// The content type of replies and errors.
    const CONTENT_TYPE: &'static [u8] = b"application/json";
}

/// Authenticate, decode, execute and encode: what a kernel route's handler
/// does.
pub fn respond<K: Kernel, A: Api<K>>(
    engine: &DeviceEngine<K>,
    auth: &impl Authenticator<K>,
    req: &Request<'_>,
    out: &mut [u8],
) -> Response {
    let actor = match auth.authenticate(req) {
        Ok(a) => a,
        Err(s) => return Response::status(s),
    };
    let cmd = match A::decode_command(req) {
        Ok(c) => c,
        Err(s) => return Response::status(s),
    };
    match engine.execute(&actor, &cmd) {
        Ok(reply) => match A::encode_reply(&reply, out) {
            Some(len) => Response { status: 200, content_type: A::CONTENT_TYPE, len, close: false },
            None => Response::status(500),
        },
        Err(e) => {
            let (status, len) = A::encode_error(&e, out);
            Response { status, content_type: A::CONTENT_TYPE, len: len.min(out.len()), close: false }
        }
    }
}
