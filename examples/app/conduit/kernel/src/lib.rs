//! The RealWorld "Conduit" backend of launchbadge/realworld-axum-sqlx (at
//! f1b2565) as an h5i-app kernel: users, profiles, follows, articles with slugs
//! and tags, favorites, comments and the feed.
//!
//! Upstream keeps several checks inside SQL (the author check of
//! `delete_article` in a CTE, `on delete cascade`, unique constraints, the
//! no-self-follow check constraint). Here they are ordinary code.
//! Password hashing, slugify and the clock stay in the shell, which passes
//! hashes, slugs and timestamps in commands.
//!
//! `transition` is the fixed kernel. `transition_upstream` computes the
//! `favorited` flag and the `?favorited=` filter the way upstream's SQL does
//! (issue #16): "the caller favorited some article", not this one.

pub type Text = Vec<u8>;

/// A caller. `user` 0 is anonymous: ids of registered users start at 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Principal {
    pub org: u64,
    pub user: u64,
}

h5i_app_schema::schema! {
    mapping conduit_tables for conduit_kernel, writes Write, lean "../proofs/generated/Schema.lean";

    /// The whole site. Conduit has a single tenant.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Snapshot {
        counter: Counter,
        users: Vec<User>,
        follows: Vec<Follow>,
        articles: Vec<Article>,
        tags: Vec<Tag>,
        favorites: Vec<Favorite>,
        comments: Vec<Comment>,
    }

    /// `password` is a hash made by the shell. An empty `image` means none.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct User in "conduit_users" {
        key { id: u64 }
        username: Text,
        email: Text,
        password: Text,
        bio: Text,
        image: Text,
    }

    /// `follower` follows `followed`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Follow in "conduit_follows" {
        key { follower: u64, followed: u64 }
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Article in "conduit_articles" {
        key { id: u64 }
        author: u64,
        slug: Text,
        title: Text,
        description: Text,
        body: Text,
        created: u64,
        updated: u64,
    }

    /// Upstream stores `tag_list` as an array column; here it is a table.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Tag in "conduit_tags" {
        key { article: u64, tag: Text }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Favorite in "conduit_favorites" {
        key { article: u64, user: u64 }
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Comment in "conduit_comments" {
        key { id: u64 }
        article: u64,
        author: u64,
        body: Text,
        created: u64,
    }

    /// The last id handed out per table; ids start at 1.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
    pub struct Counter in "conduit_counters" {
        key {}
        last_user: u64,
        last_article: u64,
        last_comment: u64,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Register { username: Text, email: Text, password: Text },
    Login { email: Text, password: Text },
    CurrentUser,
    UpdateUser {
        email: Option<Text>,
        username: Option<Text>,
        password: Option<Text>,
        bio: Option<Text>,
        image: Option<Text>,
    },
    GetProfile { username: Text },
    Follow { username: Text },
    Unfollow { username: Text },
    /// Filters by tag, author name and the name of a user who favorited.
    ListArticles { tag: Option<Text>, author: Option<Text>, favorited: Option<Text>, limit: u64, offset: u64 },
    Feed { limit: u64, offset: u64 },
    GetArticle { slug: Text },
    CreateArticle { slug: Text, title: Text, description: Text, body: Text, tags: Vec<Text>, now: u64 },
    /// `new_slug` is set by the shell when the title changes.
    UpdateArticle {
        slug: Text,
        new_slug: Option<Text>,
        title: Option<Text>,
        description: Option<Text>,
        body: Option<Text>,
        now: u64,
    },
    DeleteArticle { slug: Text },
    Favorite { slug: Text },
    Unfavorite { slug: Text },
    GetComments { slug: Text },
    AddComment { slug: Text, body: Text, now: u64 },
    DeleteComment { slug: Text, id: u64 },
    GetTags,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Write {
    PutUser(User),
    PutFollow(Follow),
    DelFollow(Follow),
    PutArticle(Article),
    DelArticle(u64),
    PutTag(Tag),
    /// Every tag of an article.
    DelTagsOf(u64),
    PutFavorite(Favorite),
    DelFavorite(Favorite),
    /// Every favorite of an article.
    DelFavoritesOf(u64),
    PutComment(Comment),
    DelComment(u64),
    /// Every comment on an article.
    DelCommentsOf(u64),
    SetCounter(Counter),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub id: u64,
    pub email: Text,
    pub username: Text,
    pub bio: Text,
    pub image: Text,
}

/// A user as seen by the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub username: Text,
    pub bio: Text,
    pub image: Text,
    pub following: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArticleView {
    pub article: Article,
    pub tags: Vec<Text>,
    pub favorited: bool,
    pub favorites: u64,
    pub author: Profile,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommentView {
    pub comment: Comment,
    pub author: Profile,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Account(Account),
    Profile(Profile),
    Article(ArticleView),
    Articles(Vec<ArticleView>),
    Comment(CommentView),
    Comments(Vec<CommentView>),
    Tags(Vec<Text>),
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Unauthorized,
    WrongPassword,
    UnknownEmail,
    NotFound,
    Forbidden,
    UsernameTaken,
    EmailTaken,
    SlugTaken,
    Overflow,
}

type Outcome = Result<(Vec<Write>, Reply), Error>;

fn one(w: Write) -> Vec<Write> {
    let mut ws = Vec::new();
    ws.push(w);
    ws
}

/* Lookups */

pub fn find_user(us: &Vec<User>, id: u64) -> Option<User> {
    let mut i = 0;
    while i < us.len() {
        if us[i].id == id {
            return Some(us[i].clone());
        }
        i += 1;
    }
    None
}

pub fn find_user_by_name(us: &Vec<User>, name: &Text) -> Option<User> {
    let mut i = 0;
    while i < us.len() {
        if us[i].username == *name {
            return Some(us[i].clone());
        }
        i += 1;
    }
    None
}

pub fn find_user_by_email(us: &Vec<User>, email: &Text) -> Option<User> {
    let mut i = 0;
    while i < us.len() {
        if us[i].email == *email {
            return Some(us[i].clone());
        }
        i += 1;
    }
    None
}

pub fn find_article(v: &Vec<Article>, slug: &Text) -> Option<Article> {
    let mut i = 0;
    while i < v.len() {
        if v[i].slug == *slug {
            return Some(v[i].clone());
        }
        i += 1;
    }
    None
}

pub fn find_comment(v: &Vec<Comment>, id: u64) -> Option<Comment> {
    let mut i = 0;
    while i < v.len() {
        if v[i].id == id {
            return Some(v[i].clone());
        }
        i += 1;
    }
    None
}

pub fn is_following(fs: &Vec<Follow>, follower: u64, followed: u64) -> bool {
    let mut i = 0;
    while i < fs.len() {
        if fs[i].follower == follower && fs[i].followed == followed {
            return true;
        }
        i += 1;
    }
    false
}

pub fn is_favorited(fs: &Vec<Favorite>, article: u64, user: u64) -> bool {
    let mut i = 0;
    while i < fs.len() {
        if fs[i].article == article && fs[i].user == user {
            return true;
        }
        i += 1;
    }
    false
}

/// Upstream's `exists(select 1 from article_favorite where user_id = $1)`.
pub fn has_favorite(fs: &Vec<Favorite>, user: u64) -> bool {
    let mut i = 0;
    while i < fs.len() {
        if fs[i].user == user {
            return true;
        }
        i += 1;
    }
    false
}

/// The same query after the caller's favorite of `except` is deleted.
pub fn has_favorite_except(fs: &Vec<Favorite>, user: u64, except: u64) -> bool {
    let mut i = 0;
    while i < fs.len() {
        if fs[i].user == user && fs[i].article != except {
            return true;
        }
        i += 1;
    }
    false
}

pub fn has_tag(ts: &Vec<Tag>, article: u64, tag: &Text) -> bool {
    let mut i = 0;
    while i < ts.len() {
        if ts[i].article == article && ts[i].tag == *tag {
            return true;
        }
        i += 1;
    }
    false
}

pub fn contains_text(v: &Vec<Text>, t: &Text) -> bool {
    let mut i = 0;
    while i < v.len() {
        if v[i] == *t {
            return true;
        }
        i += 1;
    }
    false
}

pub fn fav_count(fs: &Vec<Favorite>, article: u64) -> u64 {
    let mut n: usize = 0;
    let mut i = 0;
    while i < fs.len() {
        if fs[i].article == article {
            n += 1;
        }
        i += 1;
    }
    n as u64
}

pub fn tags_of(ts: &Vec<Tag>, article: u64) -> Vec<Text> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < ts.len() {
        if ts[i].article == article {
            out.push(ts[i].tag.clone());
        }
        i += 1;
    }
    out
}

/// Each tag once, first occurrence first.
pub fn dedup(v: &Vec<Text>) -> Vec<Text> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < v.len() {
        if !contains_text(&out, &v[i]) {
            out.push(v[i].clone());
        }
        i += 1;
    }
    out
}

/// Every tag in use, each once (`GET /api/tags`).
pub fn all_tags(ts: &Vec<Tag>) -> Vec<Text> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < ts.len() {
        if !contains_text(&out, &ts[i].tag) {
            out.push(ts[i].tag.clone());
        }
        i += 1;
    }
    out
}

/* Views */

pub fn profile(s: &Snapshot, viewer: u64, u: &User) -> Profile {
    Profile {
        username: u.username.clone(),
        bio: u.bio.clone(),
        image: u.image.clone(),
        following: is_following(&s.follows, viewer, u.id),
    }
}

pub fn author_profile(s: &Snapshot, viewer: u64, id: u64) -> Profile {
    match find_user(&s.users, id) {
        Some(u) => profile(s, viewer, &u),
        None => Profile { username: Vec::new(), bio: Vec::new(), image: Vec::new(), following: false },
    }
}

fn favorited_flag(fs: &Vec<Favorite>, article: u64, viewer: u64, up: bool) -> bool {
    if up {
        has_favorite(fs, viewer)
    } else {
        is_favorited(fs, article, viewer)
    }
}

pub fn article_view(s: &Snapshot, viewer: u64, a: &Article, up: bool) -> ArticleView {
    ArticleView {
        article: a.clone(),
        tags: tags_of(&s.tags, a.id),
        favorited: favorited_flag(&s.favorites, a.id, viewer, up),
        favorites: fav_count(&s.favorites, a.id),
        author: author_profile(s, viewer, a.author),
    }
}

pub fn views(s: &Snapshot, viewer: u64, v: &Vec<Article>, up: bool) -> Vec<ArticleView> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < v.len() {
        out.push(article_view(s, viewer, &v[i], up));
        i += 1;
    }
    out
}

pub fn comment_views(s: &Snapshot, viewer: u64, article: u64) -> Vec<CommentView> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.comments.len() {
        if s.comments[i].article == article {
            out.push(CommentView {
                comment: s.comments[i].clone(),
                author: author_profile(s, viewer, s.comments[i].author),
            });
        }
        i += 1;
    }
    out
}

/* Listing */

/// The filters of `GET /api/articles`, with names resolved to user ids.
pub fn matches(s: &Snapshot, a: &Article, tag: &Option<Text>, author: Option<u64>, fav: Option<u64>, up: bool) -> bool {
    let tag_ok = match tag {
        None => true,
        Some(t) => has_tag(&s.tags, a.id, t),
    };
    let author_ok = match author {
        None => true,
        Some(u) => a.author == u,
    };
    let fav_ok = match fav {
        None => true,
        // Upstream: "the named user favorited something".
        Some(u) => favorited_flag(&s.favorites, a.id, u, up),
    };
    tag_ok && author_ok && fav_ok
}

pub fn select(s: &Snapshot, tag: &Option<Text>, author: Option<u64>, fav: Option<u64>, up: bool) -> Vec<Article> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.articles.len() {
        if matches(s, &s.articles[i], tag, author, fav, up) {
            out.push(s.articles[i].clone());
        }
        i += 1;
    }
    out
}

/// Articles by authors `user` follows.
pub fn feed_select(s: &Snapshot, user: u64) -> Vec<Article> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.articles.len() {
        if is_following(&s.follows, user, s.articles[i].author) {
            out.push(s.articles[i].clone());
        }
        i += 1;
    }
    out
}

/// Newest first, skipping `offset`, at most `limit`. Articles are stored in
/// id order, which is creation order.
pub fn page(v: &Vec<Article>, offset: u64, limit: u64) -> Vec<Article> {
    let mut out = Vec::new();
    let n = v.len();
    if offset >= n as u64 {
        return out;
    }
    let start = n - (offset as usize);
    let mut j: usize = 0;
    while j < start && (j as u64) < limit {
        out.push(v[start - 1 - j].clone());
        j += 1;
    }
    out
}

/// Resolves a username filter: `None` if the user does not exist, in which
/// case nothing matches.
fn resolve(us: &Vec<User>, name: &Option<Text>) -> Option<Option<u64>> {
    match name {
        None => Some(None),
        Some(n) => match find_user_by_name(us, n) {
            Some(u) => Some(Some(u.id)),
            None => None,
        },
    }
}

/* Commands */

fn account(u: &User) -> Account {
    Account {
        id: u.id,
        email: u.email.clone(),
        username: u.username.clone(),
        bio: u.bio.clone(),
        image: u.image.clone(),
    }
}

fn register(s: &Snapshot, username: &Text, email: &Text, password: &Text) -> Outcome {
    if find_user_by_name(&s.users, username).is_some() {
        return Err(Error::UsernameTaken);
    }
    if find_user_by_email(&s.users, email).is_some() {
        return Err(Error::EmailTaken);
    }
    let last = s.counter.last_user;
    if last == u64::MAX {
        return Err(Error::Overflow);
    }
    let u = User {
        id: last + 1,
        username: username.clone(),
        email: email.clone(),
        password: password.clone(),
        bio: Vec::new(),
        image: Vec::new(),
    };
    let c = Counter { last_user: last + 1, last_article: s.counter.last_article, last_comment: s.counter.last_comment };
    let mut ws = Vec::new();
    ws.push(Write::SetCounter(c));
    ws.push(Write::PutUser(u.clone()));
    Ok((ws, Reply::Account(account(&u))))
}

fn login(s: &Snapshot, email: &Text, password: &Text) -> Outcome {
    match find_user_by_email(&s.users, email) {
        None => Err(Error::UnknownEmail),
        Some(u) => {
            if u.password == *password {
                Ok((Vec::new(), Reply::Account(account(&u))))
            } else {
                Err(Error::WrongPassword)
            }
        }
    }
}

fn current_user(s: &Snapshot, user: u64) -> Outcome {
    match find_user(&s.users, user) {
        None => Err(Error::Unauthorized),
        Some(me) => Ok((Vec::new(), Reply::Account(account(&me)))),
    }
}

/// The new value of a field, or the old one.
fn or_keep(new: &Option<Text>, old: &Text) -> Text {
    match new {
        Some(t) => t.clone(),
        None => old.clone(),
    }
}

/// Does a user other than `me` have the name `name`, if given?
fn name_taken(us: &Vec<User>, name: &Option<Text>, me: u64) -> bool {
    match name {
        None => false,
        Some(n) => match find_user_by_name(us, n) {
            None => false,
            Some(o) => o.id != me,
        },
    }
}

fn email_taken(us: &Vec<User>, email: &Option<Text>, me: u64) -> bool {
    match email {
        None => false,
        Some(e) => match find_user_by_email(us, e) {
            None => false,
            Some(o) => o.id != me,
        },
    }
}

/// Does an article other than `id` have the slug `slug`, if given?
fn slug_taken(v: &Vec<Article>, slug: &Option<Text>, id: u64) -> bool {
    match slug {
        None => false,
        Some(n) => match find_article(v, n) {
            None => false,
            Some(o) => o.id != id,
        },
    }
}

fn update_user(
    s: &Snapshot,
    user: u64,
    email: &Option<Text>,
    username: &Option<Text>,
    password: &Option<Text>,
    bio: &Option<Text>,
    image: &Option<Text>,
) -> Outcome {
    let me = match find_user(&s.users, user) {
        None => return Err(Error::Unauthorized),
        Some(me) => me,
    };
    if name_taken(&s.users, username, me.id) {
        return Err(Error::UsernameTaken);
    }
    if email_taken(&s.users, email, me.id) {
        return Err(Error::EmailTaken);
    }
    let u = User {
        id: me.id,
        username: or_keep(username, &me.username),
        email: or_keep(email, &me.email),
        password: or_keep(password, &me.password),
        bio: or_keep(bio, &me.bio),
        image: or_keep(image, &me.image),
    };
    Ok((one(Write::PutUser(u.clone())), Reply::Account(account(&u))))
}

fn get_profile(s: &Snapshot, viewer: u64, username: &Text) -> Outcome {
    match find_user_by_name(&s.users, username) {
        None => Err(Error::NotFound),
        Some(u) => Ok((Vec::new(), Reply::Profile(profile(s, viewer, &u)))),
    }
}

fn follow(s: &Snapshot, user: u64, username: &Text) -> Outcome {
    let me = match find_user(&s.users, user) {
        None => return Err(Error::Unauthorized),
        Some(me) => me,
    };
    match find_user_by_name(&s.users, username) {
        None => Err(Error::NotFound),
        Some(t) => {
            // Upstream: check constraint `user_cannot_follow_self`.
            if t.id == me.id {
                return Err(Error::Forbidden);
            }
            let p = Profile { username: t.username.clone(), bio: t.bio.clone(), image: t.image.clone(), following: true };
            Ok((one(Write::PutFollow(Follow { follower: me.id, followed: t.id })), Reply::Profile(p)))
        }
    }
}

fn unfollow(s: &Snapshot, user: u64, username: &Text) -> Outcome {
    let me = match find_user(&s.users, user) {
        None => return Err(Error::Unauthorized),
        Some(me) => me,
    };
    match find_user_by_name(&s.users, username) {
        None => Err(Error::NotFound),
        Some(t) => {
            let p = Profile { username: t.username.clone(), bio: t.bio.clone(), image: t.image.clone(), following: false };
            Ok((one(Write::DelFollow(Follow { follower: me.id, followed: t.id })), Reply::Profile(p)))
        }
    }
}

fn list_articles(
    s: &Snapshot,
    viewer: u64,
    tag: &Option<Text>,
    author: &Option<Text>,
    favorited: &Option<Text>,
    limit: u64,
    offset: u64,
    up: bool,
) -> Outcome {
    let a = match resolve(&s.users, author) {
        None => return Ok((Vec::new(), Reply::Articles(Vec::new()))),
        Some(a) => a,
    };
    let f = match resolve(&s.users, favorited) {
        None => return Ok((Vec::new(), Reply::Articles(Vec::new()))),
        Some(f) => f,
    };
    let sel = select(s, tag, a, f, up);
    Ok((Vec::new(), Reply::Articles(views(s, viewer, &page(&sel, offset, limit), up))))
}

fn feed(s: &Snapshot, user: u64, limit: u64, offset: u64, up: bool) -> Outcome {
    let me = match find_user(&s.users, user) {
        None => return Err(Error::Unauthorized),
        Some(me) => me,
    };
    let sel = feed_select(s, me.id);
    Ok((Vec::new(), Reply::Articles(views(s, me.id, &page(&sel, offset, limit), up))))
}

fn get_article(s: &Snapshot, viewer: u64, slug: &Text, up: bool) -> Outcome {
    match find_article(&s.articles, slug) {
        None => Err(Error::NotFound),
        Some(a) => Ok((Vec::new(), Reply::Article(article_view(s, viewer, &a, up)))),
    }
}

/// `PutTag` for each tag, after `ws`.
pub fn tag_writes(ws: Vec<Write>, article: u64, tags: &Vec<Text>) -> Vec<Write> {
    let mut out = ws;
    let mut i = 0;
    while i < tags.len() {
        out.push(Write::PutTag(Tag { article, tag: tags[i].clone() }));
        i += 1;
    }
    out
}

fn create_article(
    s: &Snapshot,
    user: u64,
    slug: &Text,
    title: &Text,
    description: &Text,
    body: &Text,
    tags: &Vec<Text>,
    now: u64,
) -> Outcome {
    let me = match find_user(&s.users, user) {
        None => return Err(Error::Unauthorized),
        Some(me) => me,
    };
    // Upstream: unique constraint `article_slug_key`.
    if find_article(&s.articles, slug).is_some() {
        return Err(Error::SlugTaken);
    }
    let last = s.counter.last_article;
    if last == u64::MAX {
        return Err(Error::Overflow);
    }
    let a = Article {
        id: last + 1,
        author: me.id,
        slug: slug.clone(),
        title: title.clone(),
        description: description.clone(),
        body: body.clone(),
        created: now,
        updated: now,
    };
    let c = Counter { last_user: s.counter.last_user, last_article: last + 1, last_comment: s.counter.last_comment };
    let ts = dedup(tags);
    // Room for the two other writes.
    if ts.len() > usize::MAX - 2 {
        return Err(Error::Overflow);
    }
    let mut ws = Vec::new();
    ws.push(Write::SetCounter(c));
    ws.push(Write::PutArticle(a.clone()));
    let ws = tag_writes(ws, last + 1, &ts);
    // Upstream returns `false`, 0 and `false` here.
    let v = ArticleView { article: a, tags: ts, favorited: false, favorites: 0, author: profile(s, me.id, &me) };
    Ok((ws, Reply::Article(v)))
}

fn update_article(
    s: &Snapshot,
    user: u64,
    slug: &Text,
    new_slug: &Option<Text>,
    title: &Option<Text>,
    description: &Option<Text>,
    body: &Option<Text>,
    now: u64,
    up: bool,
) -> Outcome {
    let me = match find_user(&s.users, user) {
        None => return Err(Error::Unauthorized),
        Some(me) => me,
    };
    let a = match find_article(&s.articles, slug) {
        None => return Err(Error::NotFound),
        Some(a) => a,
    };
    if a.author != me.id {
        return Err(Error::Forbidden);
    }
    if slug_taken(&s.articles, new_slug, a.id) {
        return Err(Error::SlugTaken);
    }
    let a2 = Article {
        id: a.id,
        author: a.author,
        slug: or_keep(new_slug, &a.slug),
        title: or_keep(title, &a.title),
        description: or_keep(description, &a.description),
        body: or_keep(body, &a.body),
        created: a.created,
        updated: now,
    };
    let v = article_view(s, me.id, &a2, up);
    Ok((one(Write::PutArticle(a2)), Reply::Article(v)))
}

fn delete_article(s: &Snapshot, user: u64, slug: &Text) -> Outcome {
    let me = match find_user(&s.users, user) {
        None => return Err(Error::Unauthorized),
        Some(me) => me,
    };
    match find_article(&s.articles, slug) {
        None => Err(Error::NotFound),
        Some(a) => {
            // Upstream: `delete ... where slug = $1 and user_id = $2` in a CTE.
            if a.author != me.id {
                return Err(Error::Forbidden);
            }
            // Upstream: `on delete cascade`.
            let mut ws = Vec::new();
            ws.push(Write::DelTagsOf(a.id));
            ws.push(Write::DelFavoritesOf(a.id));
            ws.push(Write::DelCommentsOf(a.id));
            ws.push(Write::DelArticle(a.id));
            Ok((ws, Reply::Done))
        }
    }
}

fn favorite(s: &Snapshot, user: u64, slug: &Text) -> Outcome {
    let me = match find_user(&s.users, user) {
        None => return Err(Error::Unauthorized),
        Some(me) => me,
    };
    match find_article(&s.articles, slug) {
        None => Err(Error::NotFound),
        Some(a) => {
            let already = is_favorited(&s.favorites, a.id, me.id);
            let n = fav_count(&s.favorites, a.id);
            let count = if already {
                n
            } else {
                if n == u64::MAX {
                    return Err(Error::Overflow);
                }
                n + 1
            };
            let v = ArticleView {
                article: a.clone(),
                tags: tags_of(&s.tags, a.id),
                favorited: true,
                favorites: count,
                author: author_profile(s, me.id, a.author),
            };
            Ok((one(Write::PutFavorite(Favorite { article: a.id, user: me.id })), Reply::Article(v)))
        }
    }
}

/// `n - 1` if `b` (and `n > 0`), else `n`.
fn dec_if(b: bool, n: u64) -> u64 {
    if b && n > 0 {
        n - 1
    } else {
        n
    }
}

/// The flag after `viewer` unfavorites `article`: upstream's query still
/// sees the caller's other favorites.
fn unfavorited_flag(fs: &Vec<Favorite>, article: u64, viewer: u64, up: bool) -> bool {
    if up {
        has_favorite_except(fs, viewer, article)
    } else {
        false
    }
}

fn unfavorite(s: &Snapshot, user: u64, slug: &Text, up: bool) -> Outcome {
    let me = match find_user(&s.users, user) {
        None => return Err(Error::Unauthorized),
        Some(me) => me,
    };
    match find_article(&s.articles, slug) {
        None => Err(Error::NotFound),
        Some(a) => {
            let already = is_favorited(&s.favorites, a.id, me.id);
            let n = fav_count(&s.favorites, a.id);
            let count = dec_if(already, n);
            let v = ArticleView {
                article: a.clone(),
                tags: tags_of(&s.tags, a.id),
                favorited: unfavorited_flag(&s.favorites, a.id, me.id, up),
                favorites: count,
                author: author_profile(s, me.id, a.author),
            };
            Ok((one(Write::DelFavorite(Favorite { article: a.id, user: me.id })), Reply::Article(v)))
        }
    }
}

fn get_comments(s: &Snapshot, viewer: u64, slug: &Text) -> Outcome {
    match find_article(&s.articles, slug) {
        None => Err(Error::NotFound),
        Some(a) => Ok((Vec::new(), Reply::Comments(comment_views(s, viewer, a.id)))),
    }
}

fn add_comment(s: &Snapshot, user: u64, slug: &Text, body: &Text, now: u64) -> Outcome {
    let me = match find_user(&s.users, user) {
        None => return Err(Error::Unauthorized),
        Some(me) => me,
    };
    match find_article(&s.articles, slug) {
        None => Err(Error::NotFound),
        Some(a) => {
            let last = s.counter.last_comment;
            if last == u64::MAX {
                return Err(Error::Overflow);
            }
            let c = Comment { id: last + 1, article: a.id, author: me.id, body: body.clone(), created: now };
            let ctr = Counter { last_user: s.counter.last_user, last_article: s.counter.last_article, last_comment: last + 1 };
            let mut ws = Vec::new();
            ws.push(Write::SetCounter(ctr));
            ws.push(Write::PutComment(c.clone()));
            Ok((ws, Reply::Comment(CommentView { comment: c, author: profile(s, me.id, &me) })))
        }
    }
}

fn delete_comment(s: &Snapshot, user: u64, slug: &Text, id: u64) -> Outcome {
    let me = match find_user(&s.users, user) {
        None => return Err(Error::Unauthorized),
        Some(me) => me,
    };
    let a = match find_article(&s.articles, slug) {
        None => return Err(Error::NotFound),
        Some(a) => a,
    };
    match find_comment(&s.comments, id) {
        None => Err(Error::NotFound),
        Some(c) => {
            if c.article != a.id {
                Err(Error::NotFound)
            } else if c.author != me.id {
                // Only the comment's author, not the article's.
                Err(Error::Forbidden)
            } else {
                Ok((one(Write::DelComment(id)), Reply::Done))
            }
        }
    }
}

fn step(actor: &Principal, s: &Snapshot, cmd: &Command, up: bool) -> Outcome {
    let u = actor.user;
    match cmd {
        Command::Register { username, email, password } => register(s, username, email, password),
        Command::Login { email, password } => login(s, email, password),
        Command::CurrentUser => current_user(s, u),
        Command::UpdateUser { email, username, password, bio, image } => {
            update_user(s, u, email, username, password, bio, image)
        }
        Command::GetProfile { username } => get_profile(s, u, username),
        Command::Follow { username } => follow(s, u, username),
        Command::Unfollow { username } => unfollow(s, u, username),
        Command::ListArticles { tag, author, favorited, limit, offset } => {
            list_articles(s, u, tag, author, favorited, *limit, *offset, up)
        }
        Command::Feed { limit, offset } => feed(s, u, *limit, *offset, up),
        Command::GetArticle { slug } => get_article(s, u, slug, up),
        Command::CreateArticle { slug, title, description, body, tags, now } => {
            create_article(s, u, slug, title, description, body, tags, *now)
        }
        Command::UpdateArticle { slug, new_slug, title, description, body, now } => {
            update_article(s, u, slug, new_slug, title, description, body, *now, up)
        }
        Command::DeleteArticle { slug } => delete_article(s, u, slug),
        Command::Favorite { slug } => favorite(s, u, slug),
        Command::Unfavorite { slug } => unfavorite(s, u, slug, up),
        Command::GetComments { slug } => get_comments(s, u, slug),
        Command::AddComment { slug, body, now } => add_comment(s, u, slug, body, *now),
        Command::DeleteComment { slug, id } => delete_comment(s, u, slug, *id),
        Command::GetTags => Ok((Vec::new(), Reply::Tags(all_tags(&s.tags)))),
    }
}

/// The kernel: what a command writes and replies, or why it is refused.
pub fn transition(actor: &Principal, s: &Snapshot, cmd: &Command) -> Outcome {
    step(actor, s, cmd, false)
}

/// Upstream's replies, with the `favorited` bug of issue #16.
pub fn transition_upstream(actor: &Principal, s: &Snapshot, cmd: &Command) -> Outcome {
    step(actor, s, cmd, true)
}

/* Apply */

// Column numbers, in `to_row` order, that the cascades delete by.
const TAG_ARTICLE: u32 = 0;
const FAVORITE_ARTICLE: u32 = 0;
const COMMENT_ARTICLE: u32 = 1;

/// What one write does to the state. `schema!` runs it over a write set
/// (`apply`).
fn apply_write(s: &mut Snapshot, w: Write) {
    match w {
        Write::PutUser(x) => User::put(&mut s.users, x),
        Write::PutFollow(x) => Follow::put(&mut s.follows, x),
        Write::DelFollow(x) => s.follows = Follow::del(&s.follows, x.follower, x.followed),
        Write::PutArticle(x) => Article::put(&mut s.articles, x),
        Write::DelArticle(id) => s.articles = Article::del(&s.articles, id),
        Write::PutTag(x) => Tag::put(&mut s.tags, x),
        Write::DelTagsOf(id) => s.tags = Tag::del_where(&s.tags, TAG_ARTICLE, &h5i_app_sql::Column::to_val(&id)),
        Write::PutFavorite(x) => Favorite::put(&mut s.favorites, x),
        Write::DelFavorite(x) => s.favorites = Favorite::del(&s.favorites, x.article, x.user),
        Write::DelFavoritesOf(id) => {
            s.favorites = Favorite::del_where(&s.favorites, FAVORITE_ARTICLE, &h5i_app_sql::Column::to_val(&id))
        }
        Write::PutComment(x) => Comment::put(&mut s.comments, x),
        Write::DelComment(id) => s.comments = Comment::del(&s.comments, id),
        Write::DelCommentsOf(id) => {
            s.comments = Comment::del_where(&s.comments, COMMENT_ARTICLE, &h5i_app_sql::Column::to_val(&id))
        }
        Write::SetCounter(c) => s.counter = c,
    }
}

/// The table writes one write makes. A cascade deletes by column value.
fn sql_write(w: &Write, out: &mut Vec<h5i_app_sql::Write>) {
    match w {
        Write::PutUser(x) => out.push(x.sql_put()),
        Write::PutFollow(x) => out.push(x.sql_put()),
        Write::DelFollow(x) => out.push(Follow::sql_del(x.follower, x.followed)),
        Write::PutArticle(x) => out.push(x.sql_put()),
        Write::DelArticle(id) => out.push(Article::sql_del(*id)),
        Write::PutTag(x) => out.push(x.sql_put()),
        Write::DelTagsOf(id) => out.push(Tag::sql_del_where(TAG_ARTICLE, h5i_app_sql::Column::to_val(id))),
        Write::PutFavorite(x) => out.push(x.sql_put()),
        Write::DelFavorite(x) => out.push(Favorite::sql_del(x.article, x.user)),
        Write::DelFavoritesOf(id) => out.push(Favorite::sql_del_where(FAVORITE_ARTICLE, h5i_app_sql::Column::to_val(id))),
        Write::PutComment(x) => out.push(x.sql_put()),
        Write::DelComment(id) => out.push(Comment::sql_del(*id)),
        Write::DelCommentsOf(id) => out.push(Comment::sql_del_where(COMMENT_ARTICLE, h5i_app_sql::Column::to_val(id))),
        Write::SetCounter(c) => out.push(c.sql_put()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Text {
        s.as_bytes().to_vec()
    }

    fn run(s: &Snapshot, user: u64, c: Command) -> (Snapshot, Result<Reply, Error>) {
        match transition(&Principal { org: 1, user }, s, &c) {
            Ok((ws, r)) => (apply(s, &ws), Ok(r)),
            Err(e) => (s.clone(), Err(e)),
        }
    }

    fn reg(s: &Snapshot, name: &str) -> Snapshot {
        let c = Command::Register { username: t(name), email: t(&format!("{name}@x")), password: t("h") };
        run(s, 0, c).0
    }

    fn create(slug: &str, tags: &[&str]) -> Command {
        Command::CreateArticle {
            slug: t(slug),
            title: t(slug),
            description: t("d"),
            body: t("b"),
            tags: tags.iter().map(|x| t(x)).collect(),
            now: 1,
        }
    }

    fn view(r: Result<Reply, Error>) -> ArticleView {
        match r {
            Ok(Reply::Article(v)) => v,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn authors_favorites_and_the_feed() {
        let s = reg(&reg(&Snapshot::default(), "alice"), "bob");
        let (s, r) = run(&s, 1, create("hello", &["a", "b", "a"]));
        assert_eq!(view(r).tags, vec![t("a"), t("b")]);
        let (s, _) = run(&s, 1, create("second", &[]));
        assert_eq!(run(&s, 1, create("hello", &[])).1, Err(Error::SlugTaken));
        let edit = Command::UpdateArticle { slug: t("hello"), new_slug: None, title: None, description: None, body: Some(t("x")), now: 2 };
        assert_eq!(run(&s, 2, edit).1, Err(Error::Forbidden));
        assert_eq!(run(&s, 2, Command::DeleteArticle { slug: t("hello") }).1, Err(Error::Forbidden));
        assert_eq!(run(&s, 0, Command::Favorite { slug: t("hello") }).1, Err(Error::Unauthorized));

        let (s, r) = run(&s, 2, Command::Favorite { slug: t("hello") });
        let v = view(r);
        assert!(v.favorited);
        assert_eq!(v.favorites, 1);
        // Issue #16: bob favorited "hello", not "second".
        let get = Command::GetArticle { slug: t("second") };
        assert!(!view(run(&s, 2, get.clone()).1).favorited);
        match transition_upstream(&Principal { org: 1, user: 2 }, &s, &get) {
            Ok((_, Reply::Article(v))) => assert!(v.favorited),
            other => panic!("{other:?}"),
        }

        let (s, _) = run(&s, 2, Command::Follow { username: t("alice") });
        match run(&s, 2, Command::Feed { limit: 20, offset: 0 }).1 {
            Ok(Reply::Articles(vs)) => {
                let slugs: Vec<Text> = vs.iter().map(|v| v.article.slug.clone()).collect();
                assert_eq!(slugs, vec![t("second"), t("hello")]);
            }
            other => panic!("{other:?}"),
        }

        let (s, _) = run(&s, 2, Command::AddComment { slug: t("hello"), body: t("hi"), now: 3 });
        assert_eq!(run(&s, 1, Command::DeleteComment { slug: t("hello"), id: 1 }).1, Err(Error::Forbidden));
        let (s, r) = run(&s, 1, Command::DeleteArticle { slug: t("hello") });
        assert_eq!(r, Ok(Reply::Done));
        assert!(s.comments.is_empty() && s.favorites.is_empty());
        assert!(s.tags.is_empty());
    }
}
