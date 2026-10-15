# Conduit

This example ports the [RealWorld](https://github.com/gothinkster/realworld)
"Conduit" backend of
[launchbadge/realworld-axum-sqlx](https://github.com/launchbadge/realworld-axum-sqlx)
(commit f1b2565) to an h5i-app kernel, serves it with the RealWorld JSON API,
and proves properties of the extracted code in Lean. Conduit is a Medium
clone: users follow each other, write articles with tags, favorite them and
comment on them, and read a feed of the authors they follow.

## The model

The kernel covers every route of the upstream server: registration, login,
the current user and its update, profiles with follow and unfollow, the
article listing with its tag, author and `favorited` filters, the feed, and
creating, editing, deleting, favoriting and commenting on articles. Upstream
keeps several of its rules inside SQL. `delete_article` and `delete_comment`
check the author in the `WHERE` clause of a CTE, a check constraint forbids
following oneself, unique constraints keep usernames, emails and slugs
distinct, and `on delete cascade` removes the favorites and comments of a
deleted article. In the kernel these are ordinary conditions and writes: the
delete command writes `DelTagsOf`, `DelFavoritesOf` and `DelCommentsOf`
before `DelArticle`, so the invariants below check that nothing is left
pointing at a deleted article. As upstream, only a comment's author may
delete it, not the author of the article.

A few things stay in the shell. The server hashes passwords (with an HMAC
keyed by the server secret, a stand-in for upstream's Argon2), turns titles
into slugs with upstream's `slugify`, sorts tag lists and reads the clock;
the kernel receives hashes, slugs and timestamps in its commands. A request
without a token is anonymous (user 0), and the kernel refuses it wherever an
account is needed. Tags are a table keyed by article and tag, where upstream
uses an array column, and ids are numbers handed out by counters. Usernames
and emails are compared byte for byte, while upstream compares them without
case. Listings are newest first, which upstream's feed query does not do
although the RealWorld spec asks for it, and `limit`/`offset` pick a page of
that order.

## Issue #16

[Issue #16](https://github.com/launchbadge/realworld-axum-sqlx/issues/16)
reports that favoriting one article makes every article show
`"favorited": true`. Every query that renders an article computes the flag
with `exists(select 1 from article_favorite where user_id = $1)`, which asks
whether the caller has favorited any article at all; the condition on
`article_id` is missing. The `?favorited=<name>` filter of the listing has
the same mistake: it keeps every article as soon as that user has favorited
one. The kernel therefore has two variants. `transition` computes both per
article, and `transition_upstream` computes them as upstream's SQL does.

For the fixed kernel, Lean proves that a reply's `favorited`,
`favoritesCount` and `following` fields are what the specification says for
the caller, and that a write replies with what a read would show right after
it. For upstream, it evaluates a state in which Bob has favorited article
"a": Bob sees "b" as favorited, which contradicts the statement proven for
the fixed kernel, and `?favorited=bob` lists both articles.

## Theorems

The specification is `proofs/Spec.lean`. Theorems about reachable states
hold in every state that successful commands can produce from an empty site;
the others hold for every caller, state and command.

| Theorem | Statement |
|---|---|
| `transition_total`, `transition_upstream_total` | The kernel never fails, in either variant. |
| `authorized` | Every write of a successful command is allowed by `Spec.allowed`, judged against the state before it. |
| `only_author_edits` | In a reachable state, only an article's author changes it, and the author stays the same. |
| `only_author_deletes` | In a reachable state, only an article's author deletes it or its tags, favorites and comments. |
| `only_comment_author_deletes` | In a reachable state, only a comment's author deletes it. |
| `own_rows` | Follow and favorite writes touch only the caller's own rows. |
| `anonymous_only_signs_up` | In a reachable state, a caller without an account can only sign up. |
| `inv_preserved`, `reachable_inv` | In every reachable state, user ids, usernames, emails and slugs are unique, follows and favorites are unique pairs, nobody follows themselves, ids are below their counters, and every follow, article, tag, favorite and comment points at an existing user and article. |
| `delete_cascades` | After an article is deleted, no tag, favorite or comment points at it. |
| `get_article_reply`, `list_reply`, `feed_reply`, `get_comments_reply`, `get_profile_reply`, `login_reply` | A read replies with exactly the specification's `view`, `listing`, `feedOf`, `commentsOf`, `profileOf` or account for the state and the caller. |
| `feed_only_followed` | The feed contains only articles by authors the caller follows. |
| `favorite_reply`, `unfavorite_reply`, `create_reply`, `update_reply`, `follow_reply`, `unfollow_reply`, `add_comment_reply` | A write replies with what the same read returns in the state after the write (for `unfavorite_reply` and `create_reply`, in a reachable state). |
| `apply_spec` | The kernel's `apply` computes `Spec.applyAll`. |
| `read_only_sound` | Every command `read_only` lists writes nothing when accepted (`H5iAppLib.ReadOnly.Sound`), so the engine may run it on a read-only snapshot. |
| `reads_abs`, `decode_load` | The extracted read plan `reads` is the plan `readsA` written in `H5iAppLib.Query` terms, and rows holding what each of its reads returns decode to `loadSnap plan s`. |
| `frame`, `load_for_sound`, `load_for_sound_inv` | For every command with a plan, `transition` on what the plan loads gives exactly its result on the whole site (with unique usernames, true in every reachable state). |
| `upstream_violates_reply_spec` | With upstream's query, the statement of `get_article_reply` is false. |
| `favorited_filter` | `?favorited=bob` lists one article in the fixed kernel and two in upstream's. |
| `s1_reachable` | Six commands lead from an empty site to the scenario state, so the theorems about reachable states apply to it. |
| `author_edits`, `other_cannot_edit`, `author_deletes`, `article_author_cannot_delete_comment`, `bob_favorites_b`, ... | In the scenario state, the permitted user can act and the others are refused. |

All of them depend only on Lean's standard axioms (`propext`,
`Classical.choice` and `Quot.sound`). `apply_spec` assumes that no table
reaches `usize::MAX` rows. The theorems describe the kernel; the server
around it is trusted to decode requests, and `server/tests/postgres.rs`
checks that the PostgreSQL store agrees with `apply` on random command
sequences. The listing's order relies on the store loading rows in key
order, which is id order.

## Loading less than the site

Conduit has one tenant, so loading the whole site for every request costs
as much as the site is big. `reads` (in the kernel, extracted) gives most
commands a read plan: one declarative read (`h5i_app_sql::Read`) per table.
Getting, favoriting and unfavoriting an article read the article by slug,
its tags and favorites, its author and the caller's follows; profiles,
follows, account commands and comments read the rows their keys name; the
listing and the feed read only the newest `offset + limit` matching articles
(the filters by tag, author name and favoriting user are `IN` sub-reads)
and what their views need. Sign-up, login, new, edited and deleted articles
and the tag list still load the whole site. `ConduitStore::load_for` runs
the plan (`schema_load_plan`), and `ddl` adds indexes for the reads.

`Frame.lean` proves the plans load enough: on what a plan loads,
`transition` returns what it returns on the whole site. What stays trusted
is that PostgreSQL returns, for each compiled read, the rows
`H5iAppLib.Query` says (differential-tested; see docs/app/TRUST.md). The
random-command test in `server/tests/postgres.rs` runs the real engine,
partial loads included, against `MemoryEngine`.

Writes lock only what they change (`lock_for`): a slug, a follow pair, a
user or the id counter.

## Running the server

```
cd examples/app  # its own Cargo workspace
export DATABASE_URL=postgres://h5i:h5i@127.0.0.1:55432/h5i H5I_APP_SECRET=dev-secret
cargo run -p conduit-server &

curl -s -H 'content-type: application/json' localhost:8080/api/users \
  -d '{"user":{"username":"alice","email":"alice@example.com","password":"pw"}}'
```

The reply carries a token for `Authorization: Token <token>`; the other
routes are those of the RealWorld spec.

## Load test

`server/examples/load.rs` seeds a RealWorld-sized site and measures the
API at several concurrencies; [docs/app/PERFORMANCE.md](../../../docs/app/PERFORMANCE.md)
has the method and the numbers.

```
cargo run --release -p conduit-server --example load -- 10   # seconds per row
```

## Building the proofs

```
h5i app prove examples/app/conduit   # extract, build, gate
h5i app mutate examples/app/conduit  # the bugs in h5i-app.toml must break a proof
```
