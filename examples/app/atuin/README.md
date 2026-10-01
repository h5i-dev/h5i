# Atuin

This example ports the account and record rules of the
[Atuin](https://github.com/atuinsh/atuin) sync server (`crates/atuin-server`
at commit 5b10eb0) to an h5i-app kernel and proves properties of the extracted
code in Lean.

## The model

The kernel covers registration, password changes, account deletion and
records. Registration honors the closed-registration switch, restricts
usernames to letters, digits and `-`, keeps usernames unique and gives each
user one session. Changing the password requires the current one, and deleting
an account removes the user's session and records along with the account.
Records can be uploaded under the size cap (where 0 means no cap), read in
pages for one series, listed with the status command, and deleted as a whole.

Some parts of the server are left out. Password hashing and token checks stay
in the shell, which passes the kernel hashes and "password matched" flags, and
the kernel does not model the history table, which the current server only
deletes, nor metrics, webhooks or rate limits. Host ids and record tags are
numbers rather than strings.

## Issue #3297

In Atuin today, a valid session alone is enough to delete an account, although
changing the password requires the current one. The kernel therefore has two
variants: `transition` requires the password to delete an account, which is
the change the issue asks for, while `transition_current` matches Atuin's
current behavior.

## Theorems

Unless noted otherwise, the theorems hold for both variants and for every
principal, state and command.

| Theorem | Statement |
|---|---|
| `step_total` | The kernel never fails. |
| `isolation` | Every write touches only the caller's own account or the account being created (the next id, which the invariants keep unused). |
| `reply_confined` | `NextRecords` and `Status` return only the caller's records. |
| `inv_preserved`, `reachable_inv` | In every reachable state, sessions and records belong to existing users (so deleting an account leaves nothing behind), ids and usernames are unique, ids are fresh, and stored records respect the size cap. |
| `registration_closed` | Closed registration refuses every sign-up. |
| `change_needs_password` | A password change without the current password fails. |
| `delete_needs_password` | In `transition`, deleting an account requires the password. |
| `current_deletes_without_password` | In `transition_current`, any signed-in user can delete their account without the password. |
| `deletes_with_password`, `register_opens` | The fixed kernel still deletes an account given the password, and an open server accepts a sign-up, so the theorems above do not hold by refusing everything. |

All of them depend only on Lean's standard axioms (`propext`,
`Classical.choice` and `Quot.sound`).

## Building the proofs

```
scripts/app/extract-atuin.sh        # regenerates proofs/generated/AtuinKernel.lean
cd examples/app/atuin/proofs && lake build
```
