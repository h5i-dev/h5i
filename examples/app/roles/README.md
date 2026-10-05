# Roles

Documents behind inherited roles and path patterns, written with
`h5i-app-std`. A user holds roles directly, a role inherits other roles, and a
role reads or writes the paths a pattern covers (`docs/*` covers everything
under `docs/`). Kernel and proofs only; no server.

It is also the check that a kernel can use `h5i-app-std` as intended: every
helper upstream code would take from `std` comes from the crate, and the
proofs never open its loops.

| In the kernel | From `h5i-app-std` | In the proofs |
|---|---|---|
| a user's direct roles | `map::get` | `mapGet` |
| roles by inheritance (recursion upstream) | `graph::reachable` | `Reach` |
| a role in the list | `set::contains` | `∈` |
| a pattern covers a path | `bytes::star_match` | `starMatch` |
| the stored path, trimmed | `bytes::trim` | `trimB` |

## Extraction

`h5i app extract` extracts the kernel with `--include h5i_app_std` (the
`include` in `h5i-app.toml`), so `generated/RolesKernel.lean` holds the crate
functions the kernel calls, and, because of `std-specs = true`, copies the
crate's specs to `generated/StdSpecs.lean` against this extraction. CI fails
if either file differs from what extraction produces, so the copy follows
`crates/h5i-app-std/proofs/StdSpecs.lean`.

## Theorems

`Spec.lean` defines `Holds s u r` (user `u` holds role `r`: `Reach` from the
direct roles) and `May s t u p` (an entry of table `t` is for a held role and
its pattern covers `p`), and proves the kernel's own loops. `roles` refuses
more than `LIMIT` roles or edges, which gives the search its `usize` bound.

| Theorem | Statement |
|---|---|
| `writes_authorized` | Every document written is at a path the actor may write (`H5iAppLib.WritesAuthorized`). |
| `reads_authorized` | Every document a reply shows is stored and readable by the actor (`H5iAppLib.ReadsAuthorized`). |

Both depend on the standard axioms only. `apply` is ordinary Rust and not
part of the proofs.
