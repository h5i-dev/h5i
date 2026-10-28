# Forum: authorization proven by the framework

A forum with drafts and admins, ported from Nyxis's bulletin board
(NyxFoundation/nyxis, `examples/bulletin-board`). Published posts are visible
to everyone in the organization, drafts only to their author and admins.
Users create their own posts; the author or an admin edits, publishes and
deletes them, and the author never changes. Users register themselves (the
first one may register as admin), and admins grant and revoke admin rights.

The point of the example: **it has no hand-written Lean**, and its
authorization is still proven, for every request sequence.

- `kernel/src/lib.rs` has handlers that check input and existence only, and
  one `Policy` impl that says who may do what, judging each disclosed row as
  stored (`stored` compares it with the row under its key). `Disclose` lists
  the rows each reply carries; `View` is the part of the state a caller may
  see.
- `h5i_app_std::authz_kernel!` writes `guarded_transition` (the handlers
  behind the policy, `guard_viewed`) and `App`, `h5i_app_core::Authz<Forum>`:
  the `Kernel` the server runs, whose `transition` is that same guard, called
  by the framework. The server writes no `Kernel` and no `Store`: it runs
  `App` on the store `schema!` generates.
- `h5i app extract` writes `proofs/generated/Authz.lean`, which
  instantiates the framework's theorem:
  `forum_kernel.guarded_transition.authorized` (every committed command makes
  only permitted writes and discloses only stored rows it may see; read-only commands
  depend on nothing outside the caller's view), `.kernel_apply_eq` (writes are
  checked in the kernel's own `apply`), `.read_only_sound` (the engine may run
  reads without locks), and `Committed.pg_stored` (PostgreSQL holds and loads
  back what that `apply` computes).

What is proven and what stays trusted: [`docs/design/design-app-authz.md`](../../../docs/design/design-app-authz.md).
To review the forum's authorization, read its `Policy`, `View` and `Disclose`
impls with `Write` and `apply_write`, the `authz_kernel!` call, the principal
function `HmacAuth` is given (`principal`) and `ReplyCodec::scope` in
`server/src/lib.rs`, the two `Cargo.toml`s and the engine line of
`server/src/main.rs`. The handlers may be wrong without breaking it, as long
as the server acts only through `H5iApp` (it does: `rpc_router`).

## Run it

From `examples/app/`, with a PostgreSQL:

```bash
docker run -d --rm --name forum-pg -e POSTGRES_PASSWORD=pw -p 127.0.0.1:5432:5432 postgres:16
export DATABASE_URL=postgres://postgres:pw@127.0.0.1:5432/postgres H5I_APP_SECRET=dev-secret
cargo run --release -p forum-server &
T1=$(H5I_APP_ISSUE=1:1 cargo run -q --release -p forum-server)
T2=$(H5I_APP_ISSUE=1:2 cargo run -q --release -p forum-server)
rpc() { curl -s localhost:8080/rpc -H "authorization: Bearer $1" -H 'content-type: application/json' -d "$2"; echo; }
rpc $T1 '{"cmd":"register","admin":true}'     # the first user may be admin
rpc $T2 '{"cmd":"register"}'
rpc $T2 '{"cmd":"create_post","title":"draft","body":"…"}'
rpc $T1 '{"cmd":"list_posts"}'                 # the admin sees the draft
rpc $T2 '{"cmd":"set_admin","user":2,"admin":true}'   # 403 forbidden
```

## Check it

```bash
h5i app prove          # extract, build, gate: guarded_transition.authorized
h5i app lint           # no hand-written Kernel or Store (hygiene)
cd kernel && cargo test --release                  # policy, view, handler bugs denied
cd ../server && H5I_APP_TEST_DATABASE_URL=$DATABASE_URL cargo test --release
```

`handler_bugs_are_denied` runs handlers with typical authorization bugs (a
post taken over on edit, others' drafts disclosed on publish, admin rights on
registration, a hidden draft relabelled as published in a reply) behind the
same policy: each is a denial, not a commit.

What the guard does not confine: a command that writes runs on the full
state, so a buggy handler can copy a draft into a post the caller may edit
(`writes_can_carry_what_the_caller_cannot_see` pins this down). Reads are
confined to the caller's view.

One thing the policy does not hide: on a command that writes, `not_found`
(the handler found no such post) and `forbidden` (the policy refused) differ,
so editing a draft one cannot see tells that it exists. Reads do not leak
this: a hidden draft is outside the view, so reading it is `not_found`.
