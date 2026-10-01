# crates.io

This example ports the crate ownership and publishing rules of
[crates.io](https://github.com/rust-lang/crates.io) (at commit 067b45e) to an
h5i-app kernel with a PostgreSQL server, and proves properties of the extracted
code in Lean. The rules come from `src/auth.rs`, the `Rights` enum in
`src/controllers/helpers/authorization.rs`, the token scopes in
`crates_io_database`, and the controllers for publishing, yanking, owners,
owner invitations, tokens and crate deletion.

## The model

Users sign in through the OAuth callback, which creates a session, and may
then create API tokens. A token is either legacy (no scopes) or has endpoint
scopes (`publish-new`, `publish-update`, `yank`, `change-owners`), an optional
crate scope and an optional expiry, and it can be revoked. Every request is
authenticated as crates.io does it: a session cookie or a live token of the
caller's own, and an account that is not locked, where a lock may have an end
date. The endpoint then checks the token's scopes, and some endpoints refuse
tokens altogether.

A crate has user owners, who have Full rights, and GitHub team owners, whose
members have Publish rights. Publishing a crate that does not exist needs the
`publish-new` scope and a verified email address, and makes the caller its
first owner; publishing a new version needs `publish-update` and Publish
rights, and a version number is never reused. Every dependency must name a
known crate. Yanking and unyanking need Publish rights, or an admin. Only user
owners invite users (who must accept before the invitation expires), add a
team (which the caller must belong to), or remove owners, and a crate always
keeps at least one user owner. Only a user owner may delete a crate, from a
session and never with a token: within 72 hours of publishing always, later
only if the crate has a single owner and at most 1000 downloads per started
month, and never while another crate depends on it. Deleting a crate removes
its versions, owners, invitations and dependencies. The operator locks and
unlocks accounts and sets the admin flag.

The shell supplies three facts that the kernel trusts: the current time, the
caller's GitHub teams, and a crate's download count. The time comes from the
engine, which reads the database's clock on every attempt and never goes back
within a registry (`monotonic`), so a lock that has ended or an invitation
that has expired stays so. crates.io asks GitHub for
team membership on each request; here the server reads it from its
configuration (`H5I_APP_TEAMS`) and passes it in the principal, so the proofs hold
for whatever teams the shell reports, and the report itself is not verified.
Download counts come from `H5I_APP_DOWNLOADS` for the same reason.

Some parts of crates.io are left out. Names are numbers: a crate's name is its
id, a version is one number, a team is an id, and a token names at most one
crate, so wildcard crate scopes (`tokio-*`) and name normalization are not
modeled. Trusted publishing, rate limits, the 500-token limit, reserved names,
the 24-hour hold on a deleted crate's name, crate metadata, emails, index
updates and the GitHub API calls are left out. Owners are added and removed
one at a time, and a GitHub organization admin cannot add a team they are not
in. crates.io keeps the session in a signed cookie; here it is a row, so the
OAuth callback's write is visible to the proofs. The emailed links that
confirm an address or accept an invitation run without authentication in
crates.io; here both need a signed-in user.

## PR #14760

Before [PR #14760](https://github.com/rust-lang/crates.io/pull/14760), the
OAuth callback wrote the session for a locked account and reported success,
and only the next request was refused. The kernel has two variants that
differ only in `authorize`: `transition` follows the code after the PR, and
`transition_pre14760` the code before it.

Two related paths remain open in crates.io after the PR. The callback still
refreshes the user's GitHub token in its own transaction before it checks the
lock, so a locked account's row is written on every sign-in attempt, and the
emailed invitation link (`accept_crate_owner_invitation_with_token`) makes a
locked user an owner without authenticating them. The kernel runs the whole
callback in one transaction and requires a session to accept an invitation,
so neither happens here.

## Theorems

Unless noted otherwise, the theorems hold for every principal, state and
command of the fixed kernel, and those that depend on the state hold in every
state reachable from an empty registry.

| Theorem | Statement |
|---|---|
| `locked_commits_nothing` | A locked user commits no write at all, whatever they send and however they sign in. Only the operator acts on a locked account. |
| `pre14760_violates_lock` | Before the PR the same statement is false: `pre14760_signs_in` gives a locked user a session, and `fixed_refuses` shows the fixed kernel refuses them. |
| `authorized` | Every write is allowed by the policy in `Spec.lean`, which is stated in terms of rights: Full for owner changes and deletion, Publish for new versions and yanks. |
| `only_full_changes_owners` | A caller without Full rights on a crate (such as a member of a team owner) never invites, adds a team or removes an owner there. |
| `token_scoped` | A request made with a token writes only within the token's endpoint and crate scopes, and only while it is live. No token can delete a crate. |
| `crate_has_user_owner` | Every crate has a user owner, who has Full rights. |
| `reachable_inv` | Keys are unique, new sessions and tokens never replace old ones, user owners are registered users, versions, owners, invitations and dependencies belong to existing crates, and every dependency names an existing crate. |
| `versions_kept` | A version is never unpublished: it survives every command with its publisher, unless the command deletes its crate. |
| `delete_rules` | A crate is deleted only by `DeleteCrate` from a session, by a user owner, within the age, owner and download limits, and when no other crate depends on it. |
| `transition_total` | The kernel never fails: no panic, overflow or bad index. |
| `apply_spec` | The kernel's `apply` computes `Spec.applyAll`, given room for the new rows. |

`Scenarios.lean` runs the extracted kernel on a small registry to show that
each guarded action happens for the right caller and is refused for the wrong
one: a team member publishes a version but cannot invite an owner, the owner
invites, the last user owner cannot leave, a token scoped to another crate
cannot yank while one scoped to this crate can, a locked user's session is
refused, a new crate can be deleted and an old shared one cannot.
`locked_reachable` builds a reachable state with a locked account, so the
hypothesis of `locked_commits_nothing` is not vacuous.

All of them depend only on Lean's standard axioms (`propext`,
`Classical.choice` and `Quot.sound`).

## Running the server

With PostgreSQL running as in the [tutorials](../tutorials), start the server
with users 1 and 2 in GitHub team 7, and sign in both. `H5I_APP_ISSUE=github:<user>`
stands in for the OAuth exchange with GitHub:

```
cd examples  # its own Cargo workspace; run from the repository root
export DATABASE_URL=postgres://h5i:h5i@127.0.0.1:55432/h5i H5I_APP_SECRET=dev-secret H5I_APP_TEAMS='1:7;2:7'
cargo run -p cratesio-server &

signin() { curl -s -D - -o /dev/null -X POST -H "X-GitHub-Login: $(H5I_APP_ISSUE=github:$1 cargo run -q -p cratesio-server)" \
  localhost:8080/session/authorize | sed -n 's/^set-cookie: \(session=[^;]*\).*/\1/p'; }
rpc() { curl -s -H "Cookie: $1" -H 'content-type: application/json' -d "$2" localhost:8080/rpc; echo; }
ALICE=$(signin 1) BOB=$(signin 2)
```

Alice publishes crate 10 once her address is verified and adds team 7. Bob,
who is in the team, can publish a version but cannot change the owners:

```
rpc $ALICE '{"cmd":"publish","crate":10,"vers":1}'       # {"error":"a verified email address is required"}
rpc $ALICE '{"cmd":"verify_email"}'                       # {"ok":true}
rpc $ALICE '{"cmd":"publish","crate":10,"vers":1}'       # {"ok":true}
rpc $ALICE '{"cmd":"add_team","crate":10,"team":7}'      # {"ok":true}
rpc $BOB   '{"cmd":"verify_email"}'
rpc $BOB   '{"cmd":"publish","crate":10,"vers":2}'       # {"ok":true}
rpc $BOB   '{"cmd":"invite_owner","crate":10,"user":2}'  # {"error":"team members don't have permission to do this"}
rpc $ALICE '{"cmd":"remove_owner","crate":10,"owner":1}' # {"error":"cannot remove all individual owners of a crate"}
```

A token scoped to another crate cannot yank crate 10, and no token can delete
a crate:

```
T=$(curl -s -H "Cookie: $ALICE" -H 'content-type: application/json' \
  -d '{"endpoint_scopes":["yank"],"crate":11}' localhost:8080/tokens | sed 's/.*"token":"\([^"]*\)".*/\1/')
curl -s -H "Authorization: $T" -d '{"cmd":"yank","crate":10,"vers":1}' -H 'content-type: application/json' localhost:8080/rpc
# {"error":"this token does not have the required permissions to perform this action"}
```

`server/tests/postgres.rs` runs random command sequences through the
PostgreSQL engine and the in-memory reference engine and checks that they
give the same replies and the same final state.

## Building the proofs

```
scripts/app/extract-cratesio.sh        # regenerates proofs/generated/CratesioKernel.lean
cd examples/app/cratesio/proofs && lake build
```
