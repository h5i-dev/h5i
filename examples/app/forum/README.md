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
  one `Policy` impl that says who may do what. `Disclose` lists the rows each
  reply carries; `View` is the part of the state a caller may see.
- `guarded_transition` runs the handlers behind the policy
  (`h5i_app_std::authz::guard_viewed`), and the server runs it as
  `Kernel::transition`.
- `h5i app extract` writes `proofs/generated/Authz.lean`, one line that
  instantiates the framework's theorem:
  `forum_kernel.guarded_transition.authorized` (every committed command makes
  only permitted writes and discloses only visible rows; read-only commands
  depend on nothing outside the caller's view) and `.read_only_sound` (the
  engine may run reads without locks).

What is proven and what stays trusted: [`docs/app/AUTHZ.md`](../../../docs/app/AUTHZ.md).

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
h5i app lint           # Kernel::transition runs a guarded function
cd kernel && cargo test --release                  # policy, view, handler bugs denied
cd ../server && H5I_APP_TEST_DATABASE_URL=$DATABASE_URL cargo test --release
```

`handler_bugs_are_denied` runs handlers with typical authorization bugs (a
post taken over on edit, others' drafts disclosed on publish, admin rights on
registration) behind the same policy: each is a denial, not a commit.
