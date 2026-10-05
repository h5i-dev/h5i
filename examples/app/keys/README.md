# API keys

An admin issues API keys, each with a list of permissions, and revokes
them. A client acts with a key, or opens a session with one and acts through
the session. Keys are byte strings.

The property is about many requests: once a key is revoked, no later request
acts on its behalf, in any order of requests by any clients. A kernel that
checks the key when a session opens and trusts the session afterwards
breaks it. The check happens in one request and the use in another, and a
revocation can come in between. `transition_pre` is that kernel;
`transition` looks the key up again on every request.

## The model

`kernel/src/lib.rs` has permissions as `Vec<Perm>` compared with the derived
`==`, keys, sessions and revocation, the commands `Issue`, `Revoke`, `Open`
and `Act`, and `apply`, which commits a write set.

`Spec.lean` derives decidable equality for the extracted enums with
`h5i_derive_eq` and models the database as lists. `Theorems.lean` states the
property over `H5iAppLib.Run`, the serial runs of requests. The engine runs each
request in one SERIALIZABLE transaction, so any interleaving the database
commits is one of these runs (trusted assumption A5). `Apply.lean` proves
the extracted `apply` computes the `applyAll` the runs use.

## Theorems

| Theorem | Statement |
|---|---|
| `grants_spec` | `grants` checks the whole permission list: it allows an action if some permission in it does. |
| `act_live` | An action runs only on behalf of a key that is live in the current state and whose permissions allow it. |
| `inv_step` | No revoked secret belongs to a key, after every successful command. |
| `no_act_after_revoke` | In every run from an empty database, after a key is revoked no later request acts on its behalf, whether it presents the key or a session opened earlier. |
| `apply_spec` | `apply` computes `applyAll` while every table fits in a `Vec`. |

`Counterexample.lean` gives the run for `transition_pre`: issue `k`, open a
session with it, revoke `k`, then act through the session, which succeeds
(`pre_run`, `pre_acts_after_revoke`). The fixed kernel refuses that last
request (`fixed_refuses`) and serves the same session before the revocation
(`fixed_run`), so the theorem does not hold by refusing everything.

## Building the proofs

```
h5i app prove examples/app/keys   # extract, build, gate
h5i app mutate examples/app/keys  # the bugs in h5i-app.toml must break a proof
```
