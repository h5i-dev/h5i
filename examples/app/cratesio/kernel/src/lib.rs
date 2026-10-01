//! Crate ownership and publishing rules of crates.io
//! (github.com/rust-lang/crates.io at 067b45e) as an h5i-app kernel.
//!
//! Names are numbers: a crate's name is its id, a version is one number, and
//! a GitHub team is an id. The shell supplies the clock, the caller's GitHub
//! teams and a crate's download count; the kernel trusts them.
//!
//! `transition` follows the code after PR #14760, which rejects locked
//! accounts during the OAuth callback; `transition_pre14760` follows the code
//! before it. They differ only in `authorize`.
//!
//! Aeneas subset: no `?`, iterator adapters, `String` or `PartialEq` calls.

/// Seconds after publishing during which the owner may always delete a crate.
pub const DELETE_WINDOW: u64 = 72 * 3600;
pub const DAY: u64 = 86400;
/// `DOWNLOADS_PER_MONTH_LIMIT` in delete.rs.
pub const DOWNLOADS_PER_MONTH: u64 = 1000;
/// `ownership_invitations_expiration`, 30 days by default.
pub const INVITE_TTL: u64 = 30 * DAY;
/// `publish_limits.dependencies`, 500 by default.
pub const MAX_DEPS: usize = 500;

/// How the request was authenticated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    /// The OAuth callback: GitHub vouched for `user` (session.rs, `authorize_session`).
    GitHub,
    /// A session cookie with this session id.
    Cookie(u64),
    /// An API token with this id.
    Token(u64),
    /// The operator's admin tool, outside the HTTP API.
    Operator,
}

/// The caller, with the facts the shell supplies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Principal {
    pub registry: u64,
    pub user: u64,
    pub via: Via,
    /// Unix seconds.
    pub now: u64,
    /// The GitHub teams `user` belongs to, as GitHub reports them.
    pub teams: Vec<u64>,
}

h5i_app_schema::schema! {
    mapping cratesio_tables for cratesio_kernel, writes Write, lean "../proofs/generated/Schema.lean";

    /// The registry's state.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Snapshot {
        counter: Counter,
        users: Vec<User>,
        sessions: Vec<Session>,
        tokens: Vec<Token>,
        crates: Vec<Krate>,
        versions: Vec<Version>,
        owners: Vec<Owner>,
        invites: Vec<Invite>,
        deps: Vec<Dep>,
    }

    /// `lock_until == 0` with `locked` means locked indefinitely.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct User in "cratesio_users" {
        key { id: u64 }
        admin: bool,
        locked: bool,
        lock_until: u64,
        verified: bool,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Session in "cratesio_sessions" {
        key { id: u64 }
        user: u64,
    }

    /// A legacy token has no scopes and may do anything a cookie may, except
    /// where crates.io refuses tokens outright. `krate: None` means every crate;
    /// `expires == 0` means never.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Token in "cratesio_tokens" {
        key { id: u64 }
        user: u64,
        legacy: bool,
        publish_new: bool,
        publish_update: bool,
        yank: bool,
        change_owners: bool,
        krate: Option<u64>,
        expires: u64,
        revoked: bool,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Krate in "cratesio_crates" {
        key { id: u64 }
        created: u64,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Version in "cratesio_versions" {
        key { krate: u64, num: u64 }
        yanked: bool,
        publisher: u64,
    }

    /// A user owner (`team == false`, Full rights) or a GitHub team owner
    /// (`team == true`, Publish rights).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Owner in "cratesio_owners" {
        key { krate: u64, owner: u64, team: bool }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Invite in "cratesio_invites" {
        key { krate: u64, user: u64 }
        inviter: u64,
        expires: u64,
    }

    /// Version `num` of `krate` depends on crate `on`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Dep in "cratesio_deps" {
        key { krate: u64, num: u64, on: u64 }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
    pub struct Counter in "cratesio_counters" {
        key {}
        next_session: u64,
        next_token: u64,
    }
}

/// The scopes of a new token (`NewApiTokenRequest`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NewToken {
    pub legacy: bool,
    pub publish_new: bool,
    pub publish_update: bool,
    pub yank: bool,
    pub change_owners: bool,
    pub krate: Option<u64>,
    pub expires: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Finish the OAuth flow: sign in, or sign up a new user.
    Authorize,
    /// The caller confirmed their email address.
    VerifyEmail,
    CreateToken { scopes: NewToken },
    RevokeToken { id: u64 },
    Publish { krate: u64, num: u64, deps: Vec<u64> },
    Yank { krate: u64, num: u64, yanked: bool },
    InviteOwner { krate: u64, user: u64 },
    AddTeam { krate: u64, team: u64 },
    RemoveOwner { krate: u64, owner: u64, team: bool }, // h5i-allow: privileged-field (owner to remove)
    HandleInvite { krate: u64, accept: bool },
    /// `downloads` is the crate's download count, from the shell.
    DeleteCrate { krate: u64, downloads: u64 },
    /// Operator commands. `until == 0` locks indefinitely.
    Lock { user: u64, until: u64 },
    Unlock { user: u64 },
    SetAdmin { user: u64, admin: bool }, // h5i-allow: privileged-field (operator command)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Write {
    PutUser(User),
    PutSession(Session),
    PutToken(Token),
    PutCrate(Krate),
    PutVersion(Version),
    PutOwner(Owner),
    DelOwner(Owner),
    PutInvite(Invite),
    /// (crate, user)
    DelInvite(u64, u64),
    PutDep(Dep),
    /// Remove the crate with its versions, owners, invitations and dependencies.
    DelCrate(u64),
    SetCounter(Counter),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply {
    Done,
    SignedIn { user: u64, session: u64 },
    TokenCreated(u64),
    AlreadyInvited,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Unauthenticated,
    AccountLocked,
    /// "this action can only be performed on the crates.io website"
    TokenNotAllowed,
    /// "this token does not have the required permissions"
    ScopeMismatch,
    NotOwner,
    /// "team members don't have permission to ..."
    TeamMember,
    NotFound,
    AlreadyUploaded,
    AlreadyOwner,
    EmailNotVerified,
    InviteExpired,
    LastUserOwner,
    NotTeamMember,
    TooManyDeps,
    UnknownDep,
    MultipleOwners,
    TooManyDownloads,
    HasReverseDeps,
    NotOperator,
    Overflow,
}

/// What an endpoint requires of a scoped token (`AuthCheck::with_endpoint_scope`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endpoint {
    /// No endpoint scope: only cookies and legacy tokens pass.
    Unscoped,
    PublishNew,
    PublishUpdate,
    Yank,
    ChangeOwners,
}

/// `controllers/helpers/authorization.rs`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rights {
    None,
    Publish,
    Full,
}

/// The authenticated caller and the token used, if any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Login {
    pub user: User,
    pub token: Option<Token>,
}

type Outcome = Result<(Vec<Write>, Reply), Error>;

fn one(w: Write) -> Vec<Write> {
    let mut ws = Vec::new();
    ws.push(w);
    ws
}

fn two(a: Write, b: Write) -> Vec<Write> {
    let mut ws = Vec::new();
    ws.push(a);
    ws.push(b);
    ws
}

/* ---------- lookups ---------- */

pub fn find_user(v: &Vec<User>, id: u64) -> Option<User> {
    let mut i = 0;
    while i < v.len() {
        if v[i].id == id {
            return Some(v[i]);
        }
        i += 1;
    }
    None
}

pub fn find_session(v: &Vec<Session>, id: u64) -> Option<Session> {
    let mut i = 0;
    while i < v.len() {
        if v[i].id == id {
            return Some(v[i]);
        }
        i += 1;
    }
    None
}

pub fn find_token(v: &Vec<Token>, id: u64) -> Option<Token> {
    let mut i = 0;
    while i < v.len() {
        if v[i].id == id {
            return Some(v[i]);
        }
        i += 1;
    }
    None
}

pub fn find_crate(v: &Vec<Krate>, id: u64) -> Option<Krate> {
    let mut i = 0;
    while i < v.len() {
        if v[i].id == id {
            return Some(v[i]);
        }
        i += 1;
    }
    None
}

pub fn find_version(v: &Vec<Version>, krate: u64, num: u64) -> Option<Version> {
    let mut i = 0;
    while i < v.len() {
        if v[i].krate == krate && v[i].num == num {
            return Some(v[i]);
        }
        i += 1;
    }
    None
}

pub fn find_invite(v: &Vec<Invite>, krate: u64, user: u64) -> Option<Invite> {
    let mut i = 0;
    while i < v.len() {
        if v[i].krate == krate && v[i].user == user {
            return Some(v[i]);
        }
        i += 1;
    }
    None
}

pub fn has_owner(v: &Vec<Owner>, krate: u64, owner: u64, team: bool) -> bool {
    let mut i = 0;
    while i < v.len() {
        if v[i].krate == krate && v[i].owner == owner && v[i].team == team {
            return true;
        }
        i += 1;
    }
    false
}

pub fn is_member(teams: &Vec<u64>, team: u64) -> bool {
    let mut i = 0;
    while i < teams.len() {
        if teams[i] == team {
            return true;
        }
        i += 1;
    }
    false
}

/// Some team owner of `krate` is one of `teams`.
pub fn team_owner_in(v: &Vec<Owner>, krate: u64, teams: &Vec<u64>) -> bool {
    let mut i = 0;
    while i < v.len() {
        if v[i].krate == krate && v[i].team && is_member(teams, v[i].owner) {
            return true;
        }
        i += 1;
    }
    false
}

/// A user owner of `krate` other than `owner`.
pub fn user_owner_besides(v: &Vec<Owner>, krate: u64, owner: u64) -> bool {
    let mut i = 0;
    while i < v.len() {
        if v[i].krate == krate && !v[i].team && v[i].owner != owner {
            return true;
        }
        i += 1;
    }
    false
}

/// A user owner of `krate` that is not the owner (owner, team). Split in
/// two because `team || ...` inside the loop makes Aeneas carry `team` as loop state.
pub fn other_user_owner(v: &Vec<Owner>, krate: u64, owner: u64, team: bool) -> bool {
    if team {
        has_user_owner(v, krate)
    } else {
        user_owner_besides(v, krate, owner)
    }
}

pub fn has_user_owner(v: &Vec<Owner>, krate: u64) -> bool {
    let mut i = 0;
    while i < v.len() {
        if v[i].krate == krate && !v[i].team {
            return true;
        }
        i += 1;
    }
    false
}

pub fn count_owners(v: &Vec<Owner>, krate: u64) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i < v.len() {
        if v[i].krate == krate {
            n += 1;
        }
        i += 1;
    }
    n
}

/// A version of another crate depends on `krate` (`RevDep::for_crate`).
pub fn has_reverse_dep(v: &Vec<Dep>, krate: u64) -> bool {
    let mut i = 0;
    while i < v.len() {
        if v[i].on == krate && v[i].krate != krate {
            return true;
        }
        i += 1;
    }
    false
}

pub fn crate_exists(v: &Vec<Krate>, id: u64) -> bool {
    let mut i = 0;
    while i < v.len() {
        if v[i].id == id {
            return true;
        }
        i += 1;
    }
    false
}

/// Every dependency names a known crate ("no known crate named ...").
pub fn deps_known(crates: &Vec<Krate>, deps: &Vec<u64>) -> bool {
    let mut i = 0;
    while i < deps.len() {
        if !crate_exists(crates, deps[i]) {
            return false;
        }
        i += 1;
    }
    true
}

/* ---------- authentication and rights ---------- */

/// `ensure_not_locked`: a lock with no end, or one that ends later.
pub fn is_locked(u: &User, now: u64) -> bool {
    u.locked && (u.lock_until == 0 || u.lock_until > now)
}

pub fn token_live(t: &Token, now: u64) -> bool {
    !t.revoked && (t.expires == 0 || t.expires > now)
}

fn signed_in(s: &Snapshot, p: &Principal, token: Option<Token>) -> Result<Login, Error> {
    match find_user(&s.users, p.user) {
        None => Err(Error::Unauthenticated),
        Some(u) => {
            if is_locked(&u, p.now) {
                Err(Error::AccountLocked)
            } else {
                Ok(Login { user: u, token })
            }
        }
    }
}

/// `authenticate` in auth.rs: a session cookie or a live API token, and an
/// account that is not locked.
pub fn authenticate(s: &Snapshot, p: &Principal) -> Result<Login, Error> {
    match p.via {
        Via::Cookie(sid) => match find_session(&s.sessions, sid) {
            None => Err(Error::Unauthenticated),
            Some(ses) => {
                if ses.user != p.user {
                    Err(Error::Unauthenticated)
                } else {
                    signed_in(s, p, None)
                }
            }
        },
        Via::Token(tid) => match find_token(&s.tokens, tid) {
            None => Err(Error::Unauthenticated),
            Some(t) => {
                if t.user != p.user || !token_live(&t, p.now) {
                    Err(Error::Unauthenticated)
                } else {
                    signed_in(s, p, Some(t))
                }
            }
        },
        Via::GitHub => Err(Error::Unauthenticated),
        Via::Operator => Err(Error::Unauthenticated),
    }
}

/// `AuthCheck::endpoint_scope_matches`.
pub fn endpoint_ok(t: &Token, e: Endpoint) -> bool {
    if t.legacy {
        return true;
    }
    match e {
        Endpoint::Unscoped => false,
        Endpoint::PublishNew => t.publish_new,
        Endpoint::PublishUpdate => t.publish_update,
        Endpoint::Yank => t.yank,
        Endpoint::ChangeOwners => t.change_owners,
    }
}

/// `AuthCheck::crate_scope_matches`, without `allow_any_crate_scope`.
pub fn crate_ok(t: &Token, krate: Option<u64>) -> bool {
    if t.legacy {
        return true;
    }
    match t.krate {
        None => true,
        Some(k) => match krate {
            None => false,
            Some(c) => k == c,
        },
    }
}

/// `AuthCheck::check` after authentication.
pub fn check_scope(token: Option<Token>, allow_token: bool, e: Endpoint, krate: Option<u64>) -> Result<(), Error> {
    match token {
        None => Ok(()),
        Some(t) => {
            if !allow_token {
                Err(Error::TokenNotAllowed)
            } else if !endpoint_ok(&t, e) || !crate_ok(&t, krate) {
                Err(Error::ScopeMismatch)
            } else {
                Ok(())
            }
        }
    }
}

/// `Rights::get`: Full for a user owner, Publish for a member of a team owner.
pub fn rights(owners: &Vec<Owner>, krate: u64, user: u64, teams: &Vec<u64>) -> Rights {
    if has_owner(owners, krate, user, false) {
        Rights::Full
    } else if team_owner_in(owners, krate, teams) {
        Rights::Publish
    } else {
        Rights::None
    }
}

/// `check_owner_permissions` and the owner check of `delete_crate`.
fn need_full(r: Rights) -> Result<(), Error> {
    match r {
        Rights::Full => Ok(()),
        Rights::Publish => Err(Error::TeamMember),
        Rights::None => Err(Error::NotOwner),
    }
}

/// `max_downloads` in delete.rs: 1000 per started month of 30 days.
pub fn max_downloads(age: u64) -> u64 {
    let days = age / DAY;
    DOWNLOADS_PER_MONTH * ((days + 29) / 30)
}

/// `INVITE_TTL` from now, saturating. A separate function so the proof can
/// split its `if`.
pub fn invite_expiry(now: u64) -> u64 {
    if now <= u64::MAX - INVITE_TTL {
        now + INVITE_TTL
    } else {
        u64::MAX
    }
}

/// Seconds since the crate was published; 0 if the clock is behind.
pub fn age_of(k: &Krate, now: u64) -> u64 {
    if now > k.created {
        now - k.created
    } else {
        0
    }
}

/* ---------- commands ---------- */

/// `authorize_session`. Before PR #14760 the session was written without
/// checking the account lock.
fn authorize(p: &Principal, s: &Snapshot, fixed: bool) -> Outcome {
    match p.via {
        Via::GitHub => {}
        _ => return Err(Error::Unauthenticated),
    }
    let sid = s.counter.next_session;
    if sid == u64::MAX {
        return Err(Error::Overflow);
    }
    let session = Write::PutSession(Session { id: sid, user: p.user });
    let counter = Write::SetCounter(Counter { next_session: sid + 1, next_token: s.counter.next_token });
    let reply = Reply::SignedIn { user: p.user, session: sid };
    match find_user(&s.users, p.user) {
        None => {
            let u = User { id: p.user, admin: false, locked: false, lock_until: 0, verified: false };
            let mut ws = Vec::new();
            ws.push(Write::PutUser(u));
            ws.push(session);
            ws.push(counter);
            Ok((ws, reply))
        }
        Some(u) => {
            if fixed && is_locked(&u, p.now) {
                Err(Error::AccountLocked)
            } else {
                Ok((two(session, counter), reply))
            }
        }
    }
}

fn verify_email(p: &Principal, s: &Snapshot) -> Outcome {
    let login = match authenticate(s, p) {
        Ok(l) => l,
        Err(e) => return Err(e),
    };
    match check_scope(login.token, false, Endpoint::Unscoped, None) {
        Ok(()) => {}
        Err(e) => return Err(e),
    }
    let u = login.user;
    Ok((one(Write::PutUser(User { id: u.id, admin: u.admin, locked: u.locked, lock_until: u.lock_until, verified: true })), Reply::Done))
}

/// `create_api_token`: a scoped token fails the endpoint check, a legacy
/// token is refused afterwards.
fn create_token(p: &Principal, s: &Snapshot, nt: &NewToken) -> Outcome {
    let login = match authenticate(s, p) {
        Ok(l) => l,
        Err(e) => return Err(e),
    };
    match check_scope(login.token, true, Endpoint::Unscoped, None) {
        Ok(()) => {}
        Err(e) => return Err(e),
    }
    match login.token {
        None => {}
        Some(_) => return Err(Error::TokenNotAllowed),
    }
    let tid = s.counter.next_token;
    if tid == u64::MAX {
        return Err(Error::Overflow);
    }
    let t = Token {
        id: tid,
        user: login.user.id,
        legacy: nt.legacy,
        publish_new: nt.publish_new,
        publish_update: nt.publish_update,
        yank: nt.yank,
        change_owners: nt.change_owners,
        krate: nt.krate,
        expires: nt.expires,
        revoked: false,
    };
    let c = Counter { next_session: s.counter.next_session, next_token: tid + 1 };
    Ok((two(Write::PutToken(t), Write::SetCounter(c)), Reply::TokenCreated(tid)))
}

/// `revoke_api_token`: only the caller's own tokens; an unknown id changes nothing.
fn revoke_token(p: &Principal, s: &Snapshot, id: u64) -> Outcome {
    let login = match authenticate(s, p) {
        Ok(l) => l,
        Err(e) => return Err(e),
    };
    match check_scope(login.token, true, Endpoint::Unscoped, None) {
        Ok(()) => {}
        Err(e) => return Err(e),
    }
    match find_token(&s.tokens, id) {
        Some(t) => {
            if t.user == login.user.id {
                let r = Token {
                    id: t.id,
                    user: t.user,
                    legacy: t.legacy,
                    publish_new: t.publish_new,
                    publish_update: t.publish_update,
                    yank: t.yank,
                    change_owners: t.change_owners,
                    krate: t.krate,
                    expires: t.expires,
                    revoked: true,
                };
                Ok((one(Write::PutToken(r)), Reply::Done))
            } else {
                Ok((Vec::new(), Reply::Done))
            }
        }
        None => Ok((Vec::new(), Reply::Done)),
    }
}

fn push_deps(mut ws: Vec<Write>, krate: u64, num: u64, deps: &Vec<u64>) -> Vec<Write> {
    let mut i = 0;
    while i < deps.len() {
        ws.push(Write::PutDep(Dep { krate, num, on: deps[i] }));
        i += 1;
    }
    ws
}

/// `publish`: a new crate needs the publish-new scope and makes the caller
/// its owner; a new version needs publish-update and Publish rights.
fn publish(p: &Principal, s: &Snapshot, krate: u64, num: u64, deps: &Vec<u64>) -> Outcome {
    let login = match authenticate(s, p) {
        Ok(l) => l,
        Err(e) => return Err(e),
    };
    if deps.len() > MAX_DEPS {
        return Err(Error::TooManyDeps);
    }
    match find_crate(&s.crates, krate) {
        None => publish_new(p, s, &login, krate, num, deps),
        Some(_) => publish_update(p, s, &login, krate, num, deps),
    }
}

/// The checks both kinds of publish make: the token scope, a verified email
/// address, and known dependencies.
fn publish_checks(s: &Snapshot, login: &Login, e: Endpoint, krate: u64, deps: &Vec<u64>) -> Result<(), Error> {
    match check_scope(login.token, true, e, Some(krate)) {
        Ok(()) => {}
        Err(e) => return Err(e),
    }
    if !login.user.verified {
        return Err(Error::EmailNotVerified);
    }
    if !deps_known(&s.crates, deps) {
        return Err(Error::UnknownDep);
    }
    Ok(())
}

fn publish_new(p: &Principal, s: &Snapshot, login: &Login, krate: u64, num: u64, deps: &Vec<u64>) -> Outcome {
    match publish_checks(s, login, Endpoint::PublishNew, krate, deps) {
        Ok(()) => {}
        Err(e) => return Err(e),
    }
    let user = login.user.id;
    let mut ws = Vec::new();
    ws.push(Write::PutCrate(Krate { id: krate, created: p.now }));
    ws.push(Write::PutOwner(Owner { krate, owner: user, team: false }));
    ws.push(Write::PutVersion(Version { krate, num, yanked: false, publisher: user }));
    Ok((push_deps(ws, krate, num, deps), Reply::Done))
}

fn publish_update(p: &Principal, s: &Snapshot, login: &Login, krate: u64, num: u64, deps: &Vec<u64>) -> Outcome {
    match publish_checks(s, login, Endpoint::PublishUpdate, krate, deps) {
        Ok(()) => {}
        Err(e) => return Err(e),
    }
    let user = login.user.id;
    match rights(&s.owners, krate, user, &p.teams) {
        Rights::None => return Err(Error::NotOwner),
        _ => {}
    }
    match find_version(&s.versions, krate, num) {
        Some(_) => Err(Error::AlreadyUploaded),
        None => {
            let v = Version { krate, num, yanked: false, publisher: user };
            Ok((push_deps(one(Write::PutVersion(v)), krate, num, deps), Reply::Done))
        }
    }
}

/// `modify_yank`: Publish rights, or an admin.
fn yank(p: &Principal, s: &Snapshot, krate: u64, num: u64, yanked: bool) -> Outcome {
    let login = match authenticate(s, p) {
        Ok(l) => l,
        Err(e) => return Err(e),
    };
    let v = match find_version(&s.versions, krate, num) {
        Some(v) => v,
        None => return Err(Error::NotFound),
    };
    match check_scope(login.token, true, Endpoint::Yank, Some(krate)) {
        Ok(()) => {}
        Err(e) => return Err(e),
    }
    match rights(&s.owners, krate, login.user.id, &p.teams) {
        Rights::None => {
            if !login.user.admin {
                return Err(Error::NotOwner);
            }
        }
        _ => {}
    }
    if v.yanked == yanked {
        Ok((Vec::new(), Reply::Done))
    } else {
        Ok((one(Write::PutVersion(Version { krate, num, yanked, publisher: v.publisher })), Reply::Done))
    }
}

/// The checks shared by the owner endpoints: change-owners scope, the crate
/// exists, and Full rights.
fn owner_guard(p: &Principal, s: &Snapshot, krate: u64) -> Result<Login, Error> {
    let login = match authenticate(s, p) {
        Ok(l) => l,
        Err(e) => return Err(e),
    };
    match check_scope(login.token, true, Endpoint::ChangeOwners, Some(krate)) {
        Ok(()) => {}
        Err(e) => return Err(e),
    }
    if !crate_exists(&s.crates, krate) {
        return Err(Error::NotFound);
    }
    match need_full(rights(&s.owners, krate, login.user.id, &p.teams)) {
        Ok(()) => Ok(login),
        Err(e) => Err(e),
    }
}

/// `add_owner` for a user: an invitation the user must accept.
fn invite_owner(p: &Principal, s: &Snapshot, krate: u64, target: u64) -> Outcome {
    let login = match owner_guard(p, s, krate) {
        Ok(l) => l,
        Err(e) => return Err(e),
    };
    match find_user(&s.users, target) {
        None => return Err(Error::NotFound),
        Some(_) => {}
    }
    if has_owner(&s.owners, krate, target, false) {
        return Err(Error::AlreadyOwner);
    }
    match find_invite(&s.invites, krate, target) {
        Some(i) => {
            if i.expires > p.now {
                return Ok((Vec::new(), Reply::AlreadyInvited));
            }
        }
        None => {}
    }
    let expires = invite_expiry(p.now);
    Ok((one(Write::PutInvite(Invite { krate, user: target, inviter: login.user.id, expires })), Reply::Done))
}

/// `add_github_team_owner`: added at once, if the caller is in the team.
fn add_team(p: &Principal, s: &Snapshot, krate: u64, team: u64) -> Outcome {
    match owner_guard(p, s, krate) {
        Ok(_) => {}
        Err(e) => return Err(e),
    }
    if has_owner(&s.owners, krate, team, true) {
        return Err(Error::AlreadyOwner);
    }
    if !is_member(&p.teams, team) {
        return Err(Error::NotTeamMember);
    }
    Ok((one(Write::PutOwner(Owner { krate, owner: team, team: true })), Reply::Done))
}

/// `remove_owners`: at least one user owner must remain.
fn remove_owner(p: &Principal, s: &Snapshot, krate: u64, owner: u64, team: bool) -> Outcome {
    match owner_guard(p, s, krate) {
        Ok(_) => {}
        Err(e) => return Err(e),
    }
    if !has_owner(&s.owners, krate, owner, team) {
        return Err(Error::NotFound);
    }
    if !other_user_owner(&s.owners, krate, owner, team) {
        return Err(Error::LastUserOwner);
    }
    Ok((one(Write::DelOwner(Owner { krate, owner, team })), Reply::Done))
}

/// `handle_crate_owner_invitation`.
fn handle_invite(p: &Principal, s: &Snapshot, krate: u64, accept: bool) -> Outcome {
    let login = match authenticate(s, p) {
        Ok(l) => l,
        Err(e) => return Err(e),
    };
    match check_scope(login.token, true, Endpoint::Unscoped, None) {
        Ok(()) => {}
        Err(e) => return Err(e),
    }
    let user = login.user.id;
    let inv = match find_invite(&s.invites, krate, user) {
        Some(i) => i,
        None => return Err(Error::NotFound),
    };
    if !accept {
        return Ok((one(Write::DelInvite(krate, user)), Reply::Done));
    }
    if inv.expires <= p.now {
        return Err(Error::InviteExpired);
    }
    if !login.user.verified {
        return Err(Error::EmailNotVerified);
    }
    Ok((two(Write::PutOwner(Owner { krate, owner: user, team: false }), Write::DelInvite(krate, user)), Reply::Done))
}

/// `delete_crate`: cookie only, Full rights; after 72 hours only with a single
/// owner and few downloads; never with reverse dependencies.
fn delete_crate(p: &Principal, s: &Snapshot, krate: u64, downloads: u64) -> Outcome {
    let login = match authenticate(s, p) {
        Ok(l) => l,
        Err(e) => return Err(e),
    };
    match check_scope(login.token, false, Endpoint::Unscoped, Some(krate)) {
        Ok(()) => {}
        Err(e) => return Err(e),
    }
    let k = match find_crate(&s.crates, krate) {
        Some(k) => k,
        None => return Err(Error::NotFound),
    };
    match need_full(rights(&s.owners, krate, login.user.id, &p.teams)) {
        Ok(()) => {}
        Err(e) => return Err(e),
    }
    let age = age_of(&k, p.now);
    if age > DELETE_WINDOW {
        if count_owners(&s.owners, krate) > 1 {
            return Err(Error::MultipleOwners);
        }
        if downloads > max_downloads(age) {
            return Err(Error::TooManyDownloads);
        }
    }
    if has_reverse_dep(&s.deps, krate) {
        return Err(Error::HasReverseDeps);
    }
    Ok((one(Write::DelCrate(krate)), Reply::Done))
}

/// Lock, unlock or promote a user, as the operator's tooling does.
fn operator(p: &Principal, s: &Snapshot, user: u64, admin: bool, locked: bool, until: u64, set_admin: bool) -> Outcome {
    match p.via {
        Via::Operator => {}
        _ => return Err(Error::NotOperator),
    }
    match find_user(&s.users, user) {
        None => Err(Error::NotFound),
        Some(u) => {
            if set_admin {
                let x = User { id: u.id, admin, locked: u.locked, lock_until: u.lock_until, verified: u.verified };
                Ok((one(Write::PutUser(x)), Reply::Done))
            } else {
                let x = User { id: u.id, admin: u.admin, locked, lock_until: until, verified: u.verified };
                Ok((one(Write::PutUser(x)), Reply::Done))
            }
        }
    }
}

fn run(p: &Principal, s: &Snapshot, cmd: &Command, fixed: bool) -> Outcome {
    match cmd {
        Command::Authorize => authorize(p, s, fixed),
        Command::VerifyEmail => verify_email(p, s),
        Command::CreateToken { scopes } => create_token(p, s, scopes),
        Command::RevokeToken { id } => revoke_token(p, s, *id),
        Command::Publish { krate, num, deps } => publish(p, s, *krate, *num, deps),
        Command::Yank { krate, num, yanked } => yank(p, s, *krate, *num, *yanked),
        Command::InviteOwner { krate, user } => invite_owner(p, s, *krate, *user),
        Command::AddTeam { krate, team } => add_team(p, s, *krate, *team),
        Command::RemoveOwner { krate, owner, team } => remove_owner(p, s, *krate, *owner, *team),
        Command::HandleInvite { krate, accept } => handle_invite(p, s, *krate, *accept),
        Command::DeleteCrate { krate, downloads } => delete_crate(p, s, *krate, *downloads),
        Command::Lock { user, until } => operator(p, s, *user, false, true, *until, false),
        Command::Unlock { user } => operator(p, s, *user, false, false, 0, false),
        Command::SetAdmin { user, admin } => operator(p, s, *user, *admin, false, 0, true),
    }
}

/// The registry after PR #14760.
pub fn transition(p: &Principal, s: &Snapshot, cmd: &Command) -> Outcome {
    run(p, s, cmd, true)
}

/// The registry before PR #14760: the OAuth callback ignores account locks.
pub fn transition_pre14760(p: &Principal, s: &Snapshot, cmd: &Command) -> Outcome {
    run(p, s, cmd, false)
}

/* ---------- apply ---------- */

/// The column every cascade deletes by: the crate, first in each child table.
const KRATE: u32 = 0;

/// What one write does to the state. `schema!` runs it over a write set
/// (`apply`).
fn apply_write(s: &mut Snapshot, w: Write) {
    match w {
        Write::PutUser(x) => User::put(&mut s.users, x),
        Write::PutSession(x) => Session::put(&mut s.sessions, x),
        Write::PutToken(x) => Token::put(&mut s.tokens, x),
        Write::PutCrate(x) => Krate::put(&mut s.crates, x),
        Write::PutVersion(x) => Version::put(&mut s.versions, x),
        Write::PutOwner(x) => Owner::put(&mut s.owners, x),
        Write::DelOwner(x) => s.owners = Owner::del(&s.owners, x.krate, x.owner, x.team),
        Write::PutInvite(x) => Invite::put(&mut s.invites, x),
        Write::DelInvite(k, u) => s.invites = Invite::del(&s.invites, k, u),
        Write::PutDep(x) => Dep::put(&mut s.deps, x),
        Write::DelCrate(k) => {
            let key = h5i_app_sql::Column::to_val(&k);
            s.crates = Krate::del(&s.crates, k);
            s.versions = Version::del_where(&s.versions, KRATE, &key);
            s.owners = Owner::del_where(&s.owners, KRATE, &key);
            s.invites = Invite::del_where(&s.invites, KRATE, &key);
            s.deps = Dep::del_where(&s.deps, KRATE, &key);
        }
        Write::SetCounter(c) => s.counter = c,
    }
}

/// The table writes one write makes. Deleting a crate deletes its rows in
/// the child tables by column value.
fn sql_write(w: &Write, out: &mut Vec<h5i_app_sql::Write>) {
    match w {
        Write::PutUser(x) => out.push(x.sql_put()),
        Write::PutSession(x) => out.push(x.sql_put()),
        Write::PutToken(x) => out.push(x.sql_put()),
        Write::PutCrate(x) => out.push(x.sql_put()),
        Write::PutVersion(x) => out.push(x.sql_put()),
        Write::PutOwner(x) => out.push(x.sql_put()),
        Write::DelOwner(x) => out.push(Owner::sql_del(x.krate, x.owner, x.team)),
        Write::PutInvite(x) => out.push(x.sql_put()),
        Write::DelInvite(k, u) => out.push(Invite::sql_del(*k, *u)),
        Write::PutDep(x) => out.push(x.sql_put()),
        Write::DelCrate(k) => {
            out.push(Krate::sql_del(*k));
            out.push(Version::sql_del_where(KRATE, h5i_app_sql::Column::to_val(k)));
            out.push(Owner::sql_del_where(KRATE, h5i_app_sql::Column::to_val(k)));
            out.push(Invite::sql_del_where(KRATE, h5i_app_sql::Column::to_val(k)));
            out.push(Dep::sql_del_where(KRATE, h5i_app_sql::Column::to_val(k)));
        }
        Write::SetCounter(c) => out.push(c.sql_put()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_000_000_000;

    fn who(user: u64, via: Via, teams: Vec<u64>) -> Principal {
        Principal { registry: 1, user, via, now: NOW, teams }
    }

    fn run_ok(s: &Snapshot, p: &Principal, c: Command) -> (Snapshot, Reply) {
        let (ws, r) = transition(p, s, &c).unwrap();
        (apply(s, &ws), r)
    }

    /// Sign in `user` and verify their email; returns the cookie principal.
    fn sign_up(s: &Snapshot, user: u64, teams: Vec<u64>) -> (Snapshot, Principal) {
        let (s, r) = run_ok(s, &who(user, Via::GitHub, vec![]), Command::Authorize);
        let Reply::SignedIn { session, .. } = r else { panic!() };
        let p = who(user, Via::Cookie(session), teams);
        let (s, _) = run_ok(&s, &p, Command::VerifyEmail);
        (s, p)
    }

    fn scopes(yank: bool, krate: Option<u64>) -> NewToken {
        NewToken { legacy: false, publish_new: false, publish_update: true, yank, change_owners: false, krate, expires: 0 }
    }

    #[test]
    fn owners_teams_and_tokens() {
        let s = Snapshot::default();
        let (s, alice) = sign_up(&s, 1, vec![]);
        let (s, bob) = sign_up(&s, 2, vec![7]);
        let (s, _) = run_ok(&s, &alice, Command::Publish { krate: 10, num: 1, deps: vec![] });
        // Bob is not an owner yet.
        assert_eq!(transition(&bob, &s, &Command::Publish { krate: 10, num: 2, deps: vec![] }), Err(Error::NotOwner));
        // Alice adds team 7 only if she is in it.
        assert_eq!(transition(&alice, &s, &Command::AddTeam { krate: 10, team: 7 }), Err(Error::NotTeamMember));
        let alice7 = Principal { teams: vec![7], ..alice.clone() };
        let (s, _) = run_ok(&s, &alice7, Command::AddTeam { krate: 10, team: 7 });
        // Bob now has Publish rights through the team, but not Full.
        let (s, _) = run_ok(&s, &bob, Command::Publish { krate: 10, num: 2, deps: vec![10] });
        assert_eq!(transition(&bob, &s, &Command::InviteOwner { krate: 10, user: 2 }), Err(Error::TeamMember));
        assert_eq!(transition(&bob, &s, &Command::DeleteCrate { krate: 10, downloads: 0 }), Err(Error::TeamMember));
        // The last user owner stays.
        assert_eq!(
            transition(&alice, &s, &Command::RemoveOwner { krate: 10, owner: 1, team: false }),
            Err(Error::LastUserOwner)
        );
        // A token scoped to crate 11 cannot yank crate 10.
        let (s, r) = run_ok(&s, &alice, Command::CreateToken { scopes: scopes(true, Some(11)) });
        let Reply::TokenCreated(t) = r else { panic!() };
        let tok = who(1, Via::Token(t), vec![]);
        assert_eq!(transition(&tok, &s, &Command::Yank { krate: 10, num: 1, yanked: true }), Err(Error::ScopeMismatch));
        let (s, r) = run_ok(&s, &alice, Command::CreateToken { scopes: scopes(true, Some(10)) });
        let Reply::TokenCreated(t) = r else { panic!() };
        let tok = who(1, Via::Token(t), vec![]);
        let (s, _) = run_ok(&s, &tok, Command::Yank { krate: 10, num: 1, yanked: true });
        assert!(s.versions.iter().any(|v| v.num == 1 && v.yanked));
        // Tokens cannot delete crates, and crate 10 has a dependent version of itself only.
        assert_eq!(transition(&tok, &s, &Command::DeleteCrate { krate: 10, downloads: 0 }), Err(Error::TokenNotAllowed));
        let (s, _) = run_ok(&s, &alice, Command::DeleteCrate { krate: 10, downloads: 0 });
        assert!(s.crates.is_empty() && s.versions.is_empty() && s.owners.is_empty() && s.deps.is_empty());
    }

    #[test]
    fn invitations_and_deletion_rules() {
        let s = Snapshot::default();
        let (s, alice) = sign_up(&s, 1, vec![]);
        let (s, bob) = sign_up(&s, 2, vec![]);
        let (s, _) = run_ok(&s, &alice, Command::Publish { krate: 10, num: 1, deps: vec![] });
        let (s, _) = run_ok(&s, &bob, Command::Publish { krate: 20, num: 1, deps: vec![10] });
        let (s, _) = run_ok(&s, &alice, Command::InviteOwner { krate: 10, user: 2 });
        assert_eq!(run_ok(&s, &alice, Command::InviteOwner { krate: 10, user: 2 }).1, Reply::AlreadyInvited);
        let (s, _) = run_ok(&s, &bob, Command::HandleInvite { krate: 10, accept: true });
        let (s, _) = run_ok(&s, &bob, Command::RemoveOwner { krate: 10, owner: 1, team: false });
        // Crate 20 depends on crate 10.
        assert_eq!(transition(&bob, &s, &Command::DeleteCrate { krate: 10, downloads: 0 }), Err(Error::HasReverseDeps));
        // After 72 hours, downloads count: 40 days is two months, so 2000.
        let later = Principal { now: NOW + 40 * DAY, ..bob.clone() };
        assert_eq!(
            transition(&later, &s, &Command::DeleteCrate { krate: 20, downloads: 2001 }),
            Err(Error::TooManyDownloads)
        );
        let (s, _) = run_ok(&s, &later, Command::DeleteCrate { krate: 20, downloads: 2000 });
        assert!(s.deps.is_empty());
    }

    #[test]
    fn locked_accounts() {
        let s = Snapshot::default();
        let (s, alice) = sign_up(&s, 1, vec![]);
        let op = who(0, Via::Operator, vec![]);
        let (s, _) = run_ok(&s, &op, Command::Lock { user: 1, until: 0 });
        let gh = who(1, Via::GitHub, vec![]);
        assert_eq!(transition(&gh, &s, &Command::Authorize), Err(Error::AccountLocked));
        assert!(transition_pre14760(&gh, &s, &Command::Authorize).is_ok());
        assert_eq!(transition(&alice, &s, &Command::Publish { krate: 1, num: 1, deps: vec![] }), Err(Error::AccountLocked));
        // A lock that has ended no longer applies.
        let (s, _) = run_ok(&s, &op, Command::Lock { user: 1, until: NOW });
        assert!(transition(&gh, &s, &Command::Authorize).is_ok());
    }
}
