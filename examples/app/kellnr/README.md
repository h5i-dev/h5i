# Kellnr

[Kellnr](https://github.com/kellnr/kellnr) is a private crate registry built on
axum. [PR #1243](https://github.com/kellnr/kellnr/pull/1243) (commit 45043ee)
fixed a privilege escalation: session logins hardcoded `is_read_only: false`,
and the crate-group endpoints skipped `check_can_modify`, so a read-only user
could change crate owners and access lists.

This example models `crates/registry/src/kellnr_api.rs` as an h5i-app kernel with
two variants. `transition` follows the code after the PR and
`transition_pre1243` the code before it, and the two share everything except
the lines the PR changed.

## The model

The kernel models the checks `check_ownership`, `check_can_modify` and
`check_download_auth`, how session and token logins build a `MaybeUser`, and
the endpoints for single owners, crate users, crate groups, yanking,
publishing (its authorization only) and downloads.

It leaves out `remove_owner` with several users at once, whose last-owner
check differs slightly, as well as `add_empty_crate`, required crate fields,
storage, webhooks, session expiry and the web UI routes. Names are numeric
ids. `apply`, which commits a write set, is extracted too, and `Apply.lean`
proves it computes `Spec.applyAll` as long as no table overflows a `Vec`.
`Spec.Reachable` is the set of states an empty registry reaches through
successful commands, with accounts, group members and settings free to change
between steps, since Kellnr manages them outside these endpoints.

## Theorems

The following hold for every principal, login path and command of the fixed
kernel, and for every state unless the statement says reachable:

| Theorem | Statement |
|---|---|
| `read_only_commits_nothing` | A read-only user who is not an admin commits no write at all. |
| `writes_authorized` | Every access-list or yank write comes from an admin or an owner of that crate, and publishing a new crate makes the publisher its first owner. |
| `owner_removal_needs_two` | An owner can only be removed from a crate with at least two owners, unless ownerless crates are allowed. |
| `owners_unique`, `owner_remains` | Owner rows are unique in every reachable state, so in a reachable state where ownerless crates are not allowed, a crate that has an owner still has one after any successful command. |
| `download_authorized` | A restricted crate is served only to an admin, an owner, a crate user or a member of a granted group. |
| `transition_total` | No input makes the kernel fail. |

For the code before the PR, `Counterexample.lean` proves the first theorem
false (`pre1243_violates_read_only`). In `pre1243_session_adds_owner`, user 1 is read-only, owns crate 7,
logs in with a session and adds an owner, and in `pre1243_token_adds_group`
the same user grants a group through a token. The fixed kernel refuses both
with `ReadOnlyModify` (`fixed_refuses_session`, `fixed_refuses_group`).
The same file shows the fixed kernel still grants what these theorems restrict,
so they do not hold by refusing everything: a writable owner adds and removes
owners, the last owner stays, a crate user downloads a restricted crate, and
publishing a new crate makes the publisher its first owner. Publishing into an
empty registry reaches a state where the crate has an owner
(`published_reachable`), so `owner_remains` applies to real states.

All theorems depend only on Lean's standard axioms.

## Building the proofs

```
../../../scripts/app/extract-kellnr.sh      # regenerates proofs/generated/KellnrKernel.lean
cd proofs && lake build
```
