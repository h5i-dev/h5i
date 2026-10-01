//! Wastebin's paste rules (github.com/matze/wastebin at b27a2ab) as an h5i-app kernel.
//!
//! The shell puts the time, a random slug and password fingerprints in
//! `Principal`, never from the request body.
//! `transition` asks before burning a paste (issue #190, commit 632ddf2);
//! `transition_pre190` burns on any GET.

pub type Text = Vec<u8>;

/// The request context the shell fills in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Principal {
    /// Owner ids from the signed `uid` cookie; the first one owns new pastes.
    pub uids: Vec<u64>,
    /// Seconds since the epoch, read by the shell.
    pub now: u64,
    /// A random slug, used if this request creates a paste.
    pub fresh: u64,
}

h5i_app_schema::schema! {
    mapping wastebin_tables for wastebin_kernel, writes Write, lean "../proofs/generated/Schema.lean";

    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Snapshot {
        counter: Counter,
        pastes: Vec<Paste>,
    }

    #[derive(Debug, PartialEq, Eq)]
    pub struct Paste in "wastebin_pastes" {
        key { id: u64 }
        /// The random part of the URL.
        slug: u64,
        owner: u64,
        text: Text,
        /// Absolute expiry in seconds since the epoch.
        expires: Option<u64>,
        burn: bool,
        /// Fingerprint of the password, if the paste has one.
        lock: Option<u64>,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
    pub struct Counter in "wastebin_counters" {
        key {}
        next_id: u64,
        last_uid: u64,
    }
}

// Hand-written: the derived impl clones `Option<u64>`, which Aeneas models
// only as an axiom.
impl Clone for Paste {
    fn clone(&self) -> Self {
        Paste {
            id: self.id,
            slug: self.slug,
            owner: self.owner,
            text: self.text.clone(),
            expires: self.expires,
            burn: self.burn,
            lock: self.lock,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// `POST /` or `POST /new`. `expires_in` is in seconds.
    Create { text: Text, expires_in: Option<u32>, burn: bool, lock: Option<u64> },
    /// The paste page, `/{id}`. `confirm` is the `confirm_burn=1` form field.
    View { slug: u64, confirm: bool, key: Option<u64> },
    /// `/raw/{id}`, `/dl/{id}` and `/md/{id}`, which never ask for confirmation.
    Fetch { slug: u64, key: Option<u64> },
    /// `DELETE /{id}` and `POST /delete/{id}`.
    Delete { slug: u64 },
    /// The periodic purge of expired pastes.
    Purge,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Write {
    PutPaste(Paste),
    DelPaste(u64),
    SetCounter(Counter),
}

/// What a read shows. `burned` means this was the only time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shown {
    pub text: Text,
    pub expires: Option<u64>,
    pub burned: bool,
    pub owned: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// The new paste's slug and its owner, for the cookie.
    Created { slug: u64, uid: u64 },
    Shown(Shown),
    /// The "reveal and burn" page; it carries no content.
    ConfirmBurn,
    /// The paste had expired and was deleted; served as 404.
    Gone,
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    NotFound,
    NeedPassword,
    WrongPassword,
    /// Not the owner, or no such paste (Wastebin does not tell them apart).
    Forbidden,
    /// The random slug is in use; the shell retries with another.
    SlugTaken,
    BadExpiry,
    Overflow,
}

type Outcome = Result<(Vec<Write>, Reply), Error>;

fn one(w: Write) -> Vec<Write> {
    let mut ws = Vec::new();
    ws.push(w);
    ws
}

/// Wastebin's `expires < datetime('now')`.
pub fn expired(p: &Paste, now: u64) -> bool {
    match p.expires {
        Some(t) => t < now,
        None => false,
    }
}

pub fn find_slug(ps: &Vec<Paste>, slug: u64) -> Option<Paste> {
    let mut i = 0;
    while i < ps.len() {
        if ps[i].slug == slug {
            return Some(ps[i].clone());
        }
        i += 1;
    }
    None
}

pub fn has_uid(uids: &Vec<u64>, u: u64) -> bool {
    let mut i = 0;
    while i < uids.len() {
        if uids[i] == u {
            return true;
        }
        i += 1;
    }
    false
}

/// The absolute expiry for `expires_in` seconds from `now`.
fn deadline(now: u64, expires_in: Option<u32>) -> Result<Option<u64>, Error> {
    match expires_in {
        None => Ok(None),
        Some(secs) => {
            if secs == 0 {
                Err(Error::BadExpiry)
            } else if now > u64::MAX - (secs as u64) {
                Err(Error::Overflow)
            } else {
                Ok(Some(now + secs as u64))
            }
        }
    }
}

/// Owner of a new paste (the cookie's first uid, or a new one) and the next `last_uid`.
fn owner_for(uids: &Vec<u64>, last_uid: u64) -> Result<(u64, u64), Error> {
    if uids.len() > 0 {
        Ok((uids[0], last_uid))
    } else if last_uid == u64::MAX {
        Err(Error::Overflow)
    } else {
        Ok((last_uid + 1, last_uid + 1))
    }
}

fn create(a: &Principal, s: &Snapshot, text: &Text, expires_in: Option<u32>, burn: bool, lock: Option<u64>) -> Outcome {
    let expires = match deadline(a.now, expires_in) {
        Err(e) => return Err(e),
        Ok(t) => t,
    };
    if find_slug(&s.pastes, a.fresh).is_some() {
        return Err(Error::SlugTaken);
    }
    let id = s.counter.next_id;
    if id == u64::MAX {
        return Err(Error::Overflow);
    }
    let (owner, last_uid) = match owner_for(&a.uids, s.counter.last_uid) {
        Err(e) => return Err(e),
        Ok(o) => o,
    };
    let mut ws = Vec::new();
    ws.push(Write::PutPaste(Paste { id, slug: a.fresh, owner, text: text.clone(), expires, burn, lock }));
    ws.push(Write::SetCounter(Counter { next_id: id + 1, last_uid }));
    Ok((ws, Reply::Created { slug: a.fresh, uid: owner }))
}

/// `Database::get`: an expired paste is deleted and not shown, a locked one
/// needs its password, and a burn-after-reading one is deleted as it is shown.
fn read(a: &Principal, p: &Paste, key: Option<u64>) -> Outcome {
    if expired(p, a.now) {
        return Ok((one(Write::DelPaste(p.id)), Reply::Gone));
    }
    match p.lock {
        None => {}
        Some(l) => match key {
            None => return Err(Error::NeedPassword),
            Some(k) => {
                if k != l {
                    return Err(Error::WrongPassword);
                }
            }
        },
    }
    let shown = Shown { text: p.text.clone(), expires: p.expires, burned: p.burn, owned: has_uid(&a.uids, p.owner) };
    if p.burn {
        Ok((one(Write::DelPaste(p.id)), Reply::Shown(shown)))
    } else {
        Ok((Vec::new(), Reply::Shown(shown)))
    }
}

fn fetch(a: &Principal, s: &Snapshot, slug: u64, key: Option<u64>) -> Outcome {
    match find_slug(&s.pastes, slug) {
        None => Err(Error::NotFound),
        Some(p) => read(a, &p, key),
    }
}

/// The paste page since commit 632ddf2: a burn-after-reading paste is shown
/// only after the reader confirms, so link previews do not burn it.
fn view(a: &Principal, s: &Snapshot, slug: u64, confirm: bool, key: Option<u64>) -> Outcome {
    match find_slug(&s.pastes, slug) {
        None => Err(Error::NotFound),
        Some(p) => {
            if p.burn && !confirm {
                Ok((Vec::new(), Reply::ConfirmBurn))
            } else {
                read(a, &p, key)
            }
        }
    }
}

/// `delete_for`: only a paste owned by one of the caller's uids.
fn delete(a: &Principal, s: &Snapshot, slug: u64) -> Outcome {
    match find_slug(&s.pastes, slug) {
        None => Err(Error::Forbidden),
        Some(p) => {
            if has_uid(&a.uids, p.owner) {
                Ok((one(Write::DelPaste(p.id)), Reply::Done))
            } else {
                Err(Error::Forbidden)
            }
        }
    }
}

fn purge(now: u64, s: &Snapshot) -> Outcome {
    let mut ws = Vec::new();
    let mut i = 0;
    while i < s.pastes.len() {
        if expired(&s.pastes[i], now) {
            ws.push(Write::DelPaste(s.pastes[i].id));
        }
        i += 1;
    }
    Ok((ws, Reply::Done))
}

/// Wastebin today.
pub fn transition(a: &Principal, s: &Snapshot, cmd: &Command) -> Outcome {
    match cmd {
        Command::Create { text, expires_in, burn, lock } => create(a, s, text, *expires_in, *burn, *lock),
        Command::View { slug, confirm, key } => view(a, s, *slug, *confirm, *key),
        Command::Fetch { slug, key } => fetch(a, s, *slug, *key),
        Command::Delete { slug } => delete(a, s, *slug),
        Command::Purge => purge(a.now, s),
    }
}

/// Wastebin before commit 632ddf2: the paste page burns on any request.
pub fn transition_pre190(a: &Principal, s: &Snapshot, cmd: &Command) -> Outcome {
    match cmd {
        Command::View { slug, confirm: _, key } => fetch(a, s, *slug, *key),
        _ => transition(a, s, cmd),
    }
}

/// One write's effect; `schema!`'s `apply` runs it over a write set.
fn apply_write(s: &mut Snapshot, w: Write) {
    match w {
        Write::PutPaste(p) => Paste::put(&mut s.pastes, p),
        Write::DelPaste(id) => s.pastes = Paste::del(&s.pastes, id),
        Write::SetCounter(c) => s.counter = c,
    }
}

/// The table writes one write makes.
fn sql_write(w: &Write, out: &mut Vec<h5i_app_sql::Write>) {
    match w {
        Write::PutPaste(p) => out.push(p.sql_put()),
        Write::DelPaste(id) => out.push(Paste::sql_del(*id)),
        Write::SetCounter(c) => out.push(c.sql_put()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn who(uids: &[u64], now: u64, fresh: u64) -> Principal {
        Principal { uids: uids.to_vec(), now, fresh }
    }

    fn run(t: fn(&Principal, &Snapshot, &Command) -> Outcome, s: &Snapshot, a: &Principal, c: Command) -> (Snapshot, Result<Reply, Error>) {
        match t(a, s, &c) {
            Ok((ws, r)) => (apply(s, &ws), Ok(r)),
            Err(e) => (s.clone(), Err(e)),
        }
    }

    fn create(burn: bool, expires_in: Option<u32>, lock: Option<u64>) -> Command {
        Command::Create { text: b"hi".to_vec(), expires_in, burn, lock }
    }

    #[test]
    fn owner_deletes() {
        let s = Snapshot::default();
        let (s, r) = run(transition, &s, &who(&[], 10, 77), create(false, None, None));
        assert_eq!(r, Ok(Reply::Created { slug: 77, uid: 1 }));
        assert_eq!(run(transition, &s, &who(&[], 10, 78), create(false, None, None)).1, Ok(Reply::Created { slug: 78, uid: 2 }));
        assert_eq!(run(transition, &s, &who(&[5], 10, 77), create(false, None, None)).1, Err(Error::SlugTaken));
        assert_eq!(run(transition, &s, &who(&[2], 10, 0), Command::Delete { slug: 77 }).1, Err(Error::Forbidden));
        let (s, r) = run(transition, &s, &who(&[2, 1], 10, 0), Command::Delete { slug: 77 });
        assert_eq!(r, Ok(Reply::Done));
        assert!(s.pastes.is_empty());
    }

    #[test]
    fn expiry_and_password() {
        let s = Snapshot::default();
        let (s, _) = run(transition, &s, &who(&[1], 100, 5), create(false, Some(10), Some(42)));
        let fetch = |key| Command::Fetch { slug: 5, key };
        assert_eq!(run(transition, &s, &who(&[], 110, 0), fetch(None)).1, Err(Error::NeedPassword));
        assert_eq!(run(transition, &s, &who(&[], 110, 0), fetch(Some(1))).1, Err(Error::WrongPassword));
        let shown = Shown { text: b"hi".to_vec(), expires: Some(110), burned: false, owned: false };
        assert_eq!(run(transition, &s, &who(&[], 110, 0), fetch(Some(42))).1, Ok(Reply::Shown(shown)));
        let (s2, r) = run(transition, &s, &who(&[], 111, 0), fetch(Some(42)));
        assert_eq!(r, Ok(Reply::Gone));
        assert!(s2.pastes.is_empty());
        let (s3, _) = run(transition, &s, &who(&[], 111, 0), Command::Purge);
        assert!(s3.pastes.is_empty());
        let (s4, _) = run(transition, &s, &who(&[], 110, 0), Command::Purge);
        assert_eq!(s4, s);
    }

    #[test]
    fn preview_burns_only_before_the_fix() {
        let s = Snapshot::default();
        let (s, _) = run(transition, &s, &who(&[], 1, 9), create(true, None, None));
        let preview = Command::View { slug: 9, confirm: false, key: None };
        // Before the fix a link preview burns the paste.
        let (gone, r) = run(transition_pre190, &s, &who(&[], 2, 0), preview.clone());
        assert!(matches!(r, Ok(Reply::Shown(Shown { burned: true, .. }))));
        assert!(gone.pastes.is_empty());
        // After it, the preview gets the confirmation page and changes nothing.
        let (same, r) = run(transition, &s, &who(&[], 2, 0), preview);
        assert_eq!(r, Ok(Reply::ConfirmBurn));
        assert_eq!(same, s);
        let (after, r) = run(transition, &s, &who(&[], 2, 0), Command::View { slug: 9, confirm: true, key: None });
        assert!(matches!(r, Ok(Reply::Shown(Shown { burned: true, .. }))));
        assert_eq!(run(transition, &after, &who(&[], 2, 0), Command::View { slug: 9, confirm: true, key: None }).1, Err(Error::NotFound));
    }
}
