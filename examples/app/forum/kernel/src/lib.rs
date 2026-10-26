//! A forum with drafts and admins, ported from Nyxis's bulletin board
//! (NyxFoundation/nyxis, `examples/bulletin-board`).
//!
//! Published posts are visible to everyone in the organization; drafts only
//! to their author and admins. Users create their own posts; the author or
//! an admin edits, publishes and deletes them, and the author never changes.
//! Users register themselves (the first one may register as admin), and
//! admins grant and revoke admin rights.
//!
//! The split is the point of the example. The handlers below say *what* a
//! command does and check only its input; *who* may do it is [`Forum`]'s
//! `Policy` alone. The server runs [`guarded_transition`], and the framework
//! proves, once for every kernel, that a committed command makes only writes
//! the policy permits and discloses only rows it lets the caller see, and that
//! a read-only command's reply depends on nothing outside the caller's view
//! (`h5i_app_std.Specs.authz`). This crate has no hand-written proof of it:
//! `proofs/generated/Authz.lean` is one line, written by `h5i app extract`.

use h5i_app_std::authz::{Disclose, Guarded, Policy, Transition, View, guard_viewed};

pub type Text = Vec<u8>;

/// The longest title or body, in bytes.
pub const MAX_TEXT: usize = 4096;

/// Who is calling: an organization (the tenant) and a user in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Principal {
    pub org: u64,
    pub user: u64,
}

h5i_app_schema::schema! {
    mapping forum_tables for forum_kernel, writes Write, lean "../proofs/generated/Schema.lean";

    /// One organization's state.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Snapshot {
        counter: Counter,
        users: Vec<User>,
        posts: Vec<Post>,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct User in "users" {
        key { user: u64 }
        admin: bool,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Post in "posts" {
        key { id: u64 }
        author: u64,
        title: Text,
        body: Text,
        published: bool,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
    pub struct Counter in "counters" {
        key {}
        next_id: u64,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Register the caller, as admin if asked; the policy refuses unless nobody is registered yet.
    Register { admin: bool }, // h5i-allow: privileged-field (Policy::permits decides who may)
    CreatePost { title: Text, body: Text, published: bool },
    EditPost { id: u64, title: Text, body: Text },
    SetPublished { id: u64, published: bool },
    DeletePost { id: u64 },
    SetAdmin { user: u64, admin: bool }, // h5i-allow: privileged-field (admins only, by Policy::permits)
    ReadPost { id: u64 },
    ListPosts,
    ReadUser { user: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Write {
    PutUser(User),
    PutPost(Post),
    DelPost(u64),
    SetCounter(Counter),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Created(u64),
    Done,
    Found(Post),
    Posts(Vec<Post>),
    Account(User),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    NotFound,
    BadText,
    Exists,
    Overflow,
}

/// A row a reply discloses, one variant per table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    OfUser(User),
    OfPost(Post),
}

type Outcome = Result<(Vec<Write>, Reply), Error>;

fn one(w: Write) -> Vec<Write> {
    let mut ws = Vec::new();
    ws.push(w);
    ws
}

pub fn text_ok(t: &Text) -> bool {
    t.len() > 0 && t.len() <= MAX_TEXT
}

pub fn find_post(ps: &Vec<Post>, id: u64) -> Option<Post> {
    let mut i = 0;
    while i < ps.len() {
        if ps[i].id == id {
            return Some(ps[i].clone());
        }
        i += 1;
    }
    None
}

pub fn find_user(us: &Vec<User>, user: u64) -> Option<User> {
    let mut i = 0;
    while i < us.len() {
        if us[i].user == user {
            return Some(us[i]);
        }
        i += 1;
    }
    None
}

pub fn is_admin(s: &Snapshot, user: u64) -> bool {
    match find_user(&s.users, user) {
        Some(u) => u.admin,
        None => false,
    }
}

/* ---------------------------------------------------------------------------
 * Handlers: what each command does. No authorization here.
 * ------------------------------------------------------------------------- */

fn register(user: u64, s: &Snapshot, admin: bool) -> Outcome {
    match find_user(&s.users, user) {
        Some(_) => Err(Error::Exists),
        None => Ok((one(Write::PutUser(User { user, admin })), Reply::Done)),
    }
}

fn create_post(user: u64, s: &Snapshot, title: &Text, body: &Text, published: bool) -> Outcome {
    if !text_ok(title) || !text_ok(body) {
        return Err(Error::BadText);
    }
    let id = s.counter.next_id;
    if id == u64::MAX {
        return Err(Error::Overflow);
    }
    let mut ws = Vec::new();
    ws.push(Write::PutPost(Post { id, author: user, title: title.clone(), body: body.clone(), published }));
    ws.push(Write::SetCounter(Counter { next_id: id + 1 }));
    Ok((ws, Reply::Created(id)))
}

fn edit_post(s: &Snapshot, id: u64, title: &Text, body: &Text) -> Outcome {
    if !text_ok(title) || !text_ok(body) {
        return Err(Error::BadText);
    }
    match find_post(&s.posts, id) {
        None => Err(Error::NotFound),
        Some(p) => {
            let q = Post { id, author: p.author, title: title.clone(), body: body.clone(), published: p.published };
            Ok((one(Write::PutPost(q)), Reply::Done))
        }
    }
}

fn set_published(s: &Snapshot, id: u64, published: bool) -> Outcome {
    match find_post(&s.posts, id) {
        None => Err(Error::NotFound),
        Some(p) => {
            let q = Post { id, author: p.author, title: p.title, body: p.body, published };
            Ok((one(Write::PutPost(q)), Reply::Done))
        }
    }
}

fn delete_post(s: &Snapshot, id: u64) -> Outcome {
    match find_post(&s.posts, id) {
        None => Err(Error::NotFound),
        Some(_) => Ok((one(Write::DelPost(id)), Reply::Done)),
    }
}

fn set_admin(s: &Snapshot, user: u64, admin: bool) -> Outcome {
    match find_user(&s.users, user) {
        None => Err(Error::NotFound),
        Some(_) => Ok((one(Write::PutUser(User { user, admin })), Reply::Done)),
    }
}

fn read_post(s: &Snapshot, id: u64) -> Outcome {
    match find_post(&s.posts, id) {
        None => Err(Error::NotFound),
        Some(p) => Ok((Vec::new(), Reply::Found(p))),
    }
}

fn read_user(s: &Snapshot, user: u64) -> Outcome {
    match find_user(&s.users, user) {
        None => Err(Error::NotFound),
        Some(u) => Ok((Vec::new(), Reply::Account(u))),
    }
}

/// The app logic: a command's writes and reply, or a refusal that commits
/// nothing. Read-only commands get the caller's view as `s`.
pub fn transition(actor: &Principal, s: &Snapshot, cmd: &Command) -> Outcome {
    match cmd {
        Command::Register { admin } => register(actor.user, s, *admin),
        Command::CreatePost { title, body, published } => create_post(actor.user, s, title, body, *published),
        Command::EditPost { id, title, body } => edit_post(s, *id, title, body),
        Command::SetPublished { id, published } => set_published(s, *id, *published),
        Command::DeletePost { id } => delete_post(s, *id),
        Command::SetAdmin { user, admin } => set_admin(s, *user, *admin),
        Command::ReadPost { id } => read_post(s, *id),
        Command::ListPosts => Ok((Vec::new(), Reply::Posts(s.posts.clone()))),
        Command::ReadUser { user } => read_user(s, *user),
    }
}

/// What one write does to the state; `schema!` builds `apply` from it.
fn apply_write(s: &mut Snapshot, w: Write) {
    match w {
        Write::PutUser(u) => User::put(&mut s.users, u),
        Write::PutPost(p) => Post::put(&mut s.posts, p),
        Write::DelPost(id) => s.posts = Post::del(&s.posts, id),
        Write::SetCounter(c) => s.counter = c,
    }
}

/// The table writes one write makes.
fn sql_write(w: &Write, out: &mut Vec<h5i_app_sql::Write>) {
    match w {
        Write::PutUser(u) => out.push(u.sql_put()),
        Write::PutPost(p) => out.push(p.sql_put()),
        Write::DelPost(id) => out.push(Post::sql_del(*id)),
        Write::SetCounter(c) => out.push(c.sql_put()),
    }
}

/* ---------------------------------------------------------------------------
 * The policy: who may write and see what. This, and `Disclose` and `View`
 * below, is what a reviewer reads; the framework proves the rest.
 * ------------------------------------------------------------------------- */

/// Marker type the policy, the view and the kernel hang off.
pub struct Forum;

/// May `user` see post `p` in `s`? Published posts: everyone. Drafts: the
/// author and admins.
pub fn post_visible(s: &Snapshot, user: u64, p: &Post) -> bool {
    p.published || p.author == user || is_admin(s, user)
}

/// May `user` see user row `u` in `s`? Their own, and admins every one.
pub fn user_visible(s: &Snapshot, user: u64, u: &User) -> bool {
    u.user == user || is_admin(s, user)
}

impl Policy<Principal, Snapshot, Write, Row> for Forum {
    fn permits(actor: &Principal, s: &Snapshot, w: &Write) -> bool {
        match w {
            Write::PutUser(u) => match find_user(&s.users, u.user) {
                // Register oneself, not as admin unless nobody is registered yet.
                None => u.user == actor.user && (!u.admin || s.users.len() == 0),
                // Grant or revoke admin rights: admins only.
                Some(_) => is_admin(s, actor.user),
            },
            Write::PutPost(p) => match find_post(&s.posts, p.id) {
                // Create a post as oneself.
                None => p.author == actor.user,
                // Edit or (un)publish: the author or an admin; the author stays.
                Some(old) => (old.author == actor.user || is_admin(s, actor.user)) && p.author == old.author,
            },
            // Delete: the author or an admin.
            Write::DelPost(id) => match find_post(&s.posts, *id) {
                None => false,
                Some(old) => old.author == actor.user || is_admin(s, actor.user),
            },
            // Ids only move forward.
            Write::SetCounter(c) => c.next_id > s.counter.next_id,
        }
    }

    fn visible(actor: &Principal, s: &Snapshot, d: &Row) -> bool {
        match d {
            Row::OfUser(u) => user_visible(s, actor.user, u),
            Row::OfPost(p) => post_visible(s, actor.user, p),
        }
    }
}

impl Disclose<Row> for Reply {
    /// Every row a reply carries.
    fn disclosed(&self) -> Vec<Row> {
        let mut out = Vec::new();
        match self {
            Reply::Created(_) | Reply::Done => {}
            Reply::Found(p) => out.push(Row::OfPost(p.clone())),
            Reply::Account(u) => out.push(Row::OfUser(*u)),
            Reply::Posts(ps) => {
                let mut i = 0;
                while i < ps.len() {
                    out.push(Row::OfPost(ps[i].clone()));
                    i += 1;
                }
            }
        }
        out
    }
}

impl View<Principal, Snapshot, Command> for Forum {
    fn read_only(cmd: &Command) -> bool {
        match cmd {
            Command::ReadPost { .. } | Command::ListPosts | Command::ReadUser { .. } => true,
            _ => false,
        }
    }

    /// The posts and users the caller may see. The counter is left out: the
    /// next id would tell how many posts exist, drafts included.
    fn view(actor: &Principal, s: &Snapshot) -> Snapshot {
        let mut users = Vec::new();
        let mut i = 0;
        while i < s.users.len() {
            if user_visible(s, actor.user, &s.users[i]) {
                users.push(s.users[i]);
            }
            i += 1;
        }
        let mut posts = Vec::new();
        let mut j = 0;
        while j < s.posts.len() {
            if post_visible(s, actor.user, &s.posts[j]) {
                posts.push(s.posts[j].clone());
            }
            j += 1;
        }
        Snapshot { counter: Counter::default(), users, posts }
    }
}

impl Transition<Principal, Snapshot, Command, Write, Reply, Error> for Forum {
    fn kernel_transition(actor: &Principal, s: &Snapshot, cmd: &Command) -> Outcome {
        transition(actor, s, cmd)
    }

    fn kernel_apply(s: &Snapshot, ws: &Vec<Write>) -> Snapshot {
        apply(s, ws)
    }
}

/// What the server runs: `transition` behind the policy.
pub fn guarded_transition(
    actor: &Principal,
    s: &Snapshot,
    cmd: &Command,
) -> Result<(Vec<Write>, Reply), Guarded<Error>> {
    guard_viewed::<Principal, Snapshot, Command, Write, Reply, Error, Row, Forum, Forum, Forum>(actor, s, cmd)
}

/// `Kernel::read_only` for the server: the view's.
pub fn read_only(cmd: &Command) -> bool {
    <Forum as View<Principal, Snapshot, Command>>::read_only(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(s: &Snapshot, user: u64, c: Command) -> (Snapshot, Result<Reply, Guarded<Error>>) {
        match guarded_transition(&Principal { org: 1, user }, s, &c) {
            Ok((ws, r)) => (apply(s, &ws), Ok(r)),
            Err(e) => (s.clone(), Err(e)),
        }
    }

    fn post(title: &str, published: bool) -> Command {
        Command::CreatePost { title: title.as_bytes().to_vec(), body: b"body".to_vec(), published }
    }

    #[test]
    fn drafts_admins_and_registration() {
        let s = Snapshot::default();
        let (s, r) = run(&s, 1, Command::Register { admin: true });
        assert_eq!(r, Ok(Reply::Done), "the first user may be admin");
        assert_eq!(run(&s, 2, Command::Register { admin: true }).1, Err(Guarded::Denied));
        let (s, _) = run(&s, 2, Command::Register { admin: false });
        let (s, _) = run(&s, 3, Command::Register { admin: false });

        let (s, r) = run(&s, 2, post("draft", false));
        assert_eq!(r, Ok(Reply::Created(0)));
        let (s, _) = run(&s, 2, post("public", true));

        // A draft is invisible to others, as if it did not exist; admins see it.
        assert_eq!(run(&s, 3, Command::ReadPost { id: 0 }).1, Err(Guarded::App(Error::NotFound)));
        assert!(matches!(run(&s, 1, Command::ReadPost { id: 0 }).1, Ok(Reply::Found(_))));
        match run(&s, 3, Command::ListPosts).1 {
            Ok(Reply::Posts(ps)) => assert_eq!(ps.len(), 1),
            r => panic!("{r:?}"),
        }

        // Only the author or an admin manages a post.
        let edit = Command::EditPost { id: 1, title: b"mine".to_vec(), body: b"b".to_vec() };
        assert_eq!(run(&s, 3, edit.clone()).1, Err(Guarded::Denied));
        assert_eq!(run(&s, 1, edit).1, Ok(Reply::Done));
        assert_eq!(run(&s, 3, Command::DeletePost { id: 1 }).1, Err(Guarded::Denied));
        assert_eq!(run(&s, 3, Command::SetPublished { id: 0, published: true }).1, Err(Guarded::Denied));

        // Admin rights: admins only; users read only their own row.
        assert_eq!(run(&s, 3, Command::SetAdmin { user: 3, admin: true }).1, Err(Guarded::Denied));
        let (s, r) = run(&s, 1, Command::SetAdmin { user: 3, admin: true });
        assert_eq!(r, Ok(Reply::Done));
        assert!(matches!(run(&s, 3, Command::ReadPost { id: 0 }).1, Ok(Reply::Found(_))));
        assert_eq!(run(&s, 2, Command::ReadUser { user: 3 }).1, Err(Guarded::App(Error::NotFound)));
    }

    /// Handlers with the authorization bugs an unreviewed (or generated)
    /// handler makes, run behind the same policy. Each bug is a denial, not a
    /// commit: the guard does not trust the handler.
    enum Bug {
        /// Editing a post makes the caller its author.
        StealOnEdit,
        /// Publishing replies with every post, drafts included.
        LeakOnPublish,
        /// Registration ignores what was asked and makes everyone admin.
        AdminOnRegister,
    }

    fn buggy(bug: &Bug, actor: &Principal, s: &Snapshot, cmd: &Command) -> Outcome {
        match (bug, cmd) {
            (Bug::StealOnEdit, Command::EditPost { id, title, body }) => match find_post(&s.posts, *id) {
                None => Err(Error::NotFound),
                Some(p) => {
                    let q = Post { id: *id, author: actor.user, title: title.clone(), body: body.clone(), published: p.published };
                    Ok((one(Write::PutPost(q)), Reply::Done))
                }
            },
            (Bug::LeakOnPublish, Command::SetPublished { id, published }) => {
                let (ws, _) = set_published(s, *id, *published)?;
                Ok((ws, Reply::Posts(s.posts.clone())))
            }
            (Bug::AdminOnRegister, Command::Register { .. }) => register(actor.user, s, true),
            _ => transition(actor, s, cmd),
        }
    }

    macro_rules! buggy_kernel {
        ($name:ident, $bug:expr) => {
            struct $name;
            impl Transition<Principal, Snapshot, Command, Write, Reply, Error> for $name {
                fn kernel_transition(actor: &Principal, s: &Snapshot, cmd: &Command) -> Outcome {
                    buggy(&$bug, actor, s, cmd)
                }
                fn kernel_apply(s: &Snapshot, ws: &Vec<Write>) -> Snapshot {
                    apply(s, ws)
                }
            }
        };
    }
    buggy_kernel!(Steal, Bug::StealOnEdit);
    buggy_kernel!(Leak, Bug::LeakOnPublish);
    buggy_kernel!(Promote, Bug::AdminOnRegister);

    fn with<T: Transition<Principal, Snapshot, Command, Write, Reply, Error>>(
        s: &Snapshot,
        user: u64,
        c: Command,
    ) -> Result<(Vec<Write>, Reply), Guarded<Error>> {
        guard_viewed::<Principal, Snapshot, Command, Write, Reply, Error, Row, T, Forum, Forum>(
            &Principal { org: 1, user },
            s,
            &c,
        )
    }

    #[test]
    fn handler_bugs_are_denied() {
        let (s, _) = run(&Snapshot::default(), 1, Command::Register { admin: true });
        let (s, _) = run(&s, 2, Command::Register { admin: false });
        let (s, _) = run(&s, 3, Command::Register { admin: false });
        let (s, _) = run(&s, 2, post("draft", false));
        let (s, _) = run(&s, 3, post("mine", false));

        // Taking over someone's post is a change of author: denied, even for the author's edit.
        let edit = |id| Command::EditPost { id, title: b"t".to_vec(), body: b"b".to_vec() };
        assert_eq!(with::<Steal>(&s, 3, edit(0)).err(), Some(Guarded::Denied));
        assert_eq!(with::<Steal>(&s, 1, edit(0)).err(), Some(Guarded::Denied), "admins may edit, not take over");
        assert!(with::<Steal>(&s, 2, edit(0)).is_ok(), "the author's own edit keeps the author");

        // Publishing one's own post must not disclose another user's draft.
        let publish = Command::SetPublished { id: 1, published: true };
        assert_eq!(with::<Leak>(&s, 3, publish.clone()).err(), Some(Guarded::Denied));
        assert!(with::<Leak>(&s, 1, publish).is_ok(), "an admin may see every draft");

        // Self-promotion through registration.
        assert_eq!(with::<Promote>(&s, 4, Command::Register { admin: false }).err(), Some(Guarded::Denied));
        assert!(with::<Promote>(&Snapshot::default(), 4, Command::Register { admin: false }).is_ok(), "the first user may be admin");
    }

    /// The guard's cost per command against `transition` alone, on a tenant
    /// of `users` users and `posts` posts. `cargo test --release -- --ignored
    /// guard_overhead --nocapture`; `docs/app/NUMBERS.md` records a run.
    #[test]
    #[ignore]
    fn guard_overhead() {
        for (users, posts) in [(10u64, 100u64), (100, 1_000), (1_000, 10_000)] {
            let mut s = Snapshot::default();
            for u in 0..users {
                s.users.push(User { user: u, admin: u == 0 });
            }
            for id in 0..posts {
                s.posts.push(Post { id, author: id % users, title: b"t".to_vec(), body: b"b".to_vec(), published: id % 3 != 0 });
            }
            s.counter.next_id = posts;
            let cmds = [
                (users - 1, Command::EditPost { id: posts - 1, title: b"x".to_vec(), body: b"y".to_vec() }),
                (users - 1, Command::CreatePost { title: b"x".to_vec(), body: b"y".to_vec(), published: true }),
                (users - 1, Command::ListPosts),
            ];
            for (user, c) in &cmds {
                let a = Principal { org: 1, user: *user };
                let n = (200_000 / posts).max(20) as u32;
                let t = std::time::Instant::now();
                for _ in 0..n {
                    std::hint::black_box(transition(&a, &s, c).ok());
                }
                let plain = t.elapsed() / n;
                let t = std::time::Instant::now();
                for _ in 0..n {
                    std::hint::black_box(guarded_transition(&a, &s, c).ok());
                }
                let guarded = t.elapsed() / n;
                let name = format!("{c:?}");
                let name = name.split([' ', '{']).next().unwrap();
                println!("{users:>5} users {posts:>6} posts  {name:<11} transition {plain:>10.2?}  guarded {guarded:>10.2?}");
            }
        }
    }

    #[test]
    fn read_only_matches_the_view() {
        for c in [Command::ListPosts, Command::ReadPost { id: 0 }, Command::ReadUser { user: 1 }] {
            assert!(read_only(&c));
        }
        assert!(!read_only(&Command::DeletePost { id: 0 }));
    }
}
