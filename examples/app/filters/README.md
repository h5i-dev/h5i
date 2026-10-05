# Saved filters

A search service where an admin stores filter templates such as `owner='$'`
and users search records through them. The kernel puts the caller's name
into the template, escaped, and parses the text back into a filter. Names
are arbitrary bytes, so the search is confined to the caller's records only
if the parser undoes exactly the escaping the substitution does.

This is the bug class of a substituter and a parser that disagree on
escaping. It lives entirely in string handling. The example shows it can be
stated and proven about extracted Rust when text is kept as `Vec<u8>` and
loops are `for` loops over slices.

`transition` escapes `\` and `'`. `transition_pre` escapes `\` but not `'`,
so the name `x'|owner='bob` turns `owner='$'` into `owner='x'|owner='bob'`.

## The model

`kernel/src/lib.rs` has `escape_into`, `substitute`, a five-state `parse`,
record matching, and the commands `Search`, `Add` and `SetRule`. Every
loop is `for x in v.iter()`; there is no index arithmetic in the Rust.

`Spec.lean` models escaping and substitution as `List.flatMap` and the parser
as one step function per byte (`pstep`), run by `H5iAppLib.iterRun`.
`Lemmas.lean` proves that each extracted function computes its model:
`iter_fold` for the escape and substitute loops, and `iter_loop` for the parser
with its early returns. Each is one `h5i_for` call.

## Theorems

| Theorem | Statement |
|---|---|
| `parse_render` | Parsing a rendered filter gives back its clauses, for every value and every key without `=`, `'`, `\|` or `\`. |
| `parse_substitute` | For every well-formed template and every name, parsing the substituted text gives the template with the name in its holes. |
| `search_exact` | A successful search returns exactly the records matching that filter. |
| `search_own`, `search_all_own` | Through `owner='$'`, a search returns all of the caller's records and no one else's, whatever bytes the name contains. |
| `search_succeeds` | A search through a well-formed template of at most 4096 bytes never fails. |
| `writes_authorized` | Every write is allowed: records are owned by their writer, rules are set by admins (`H5iAppLib.WritesAuthorized`). |

`Counterexample.lean` runs `transition_pre` for Eve (`x'|owner='bob`) and
shows she gets Bob's record (`pre_leaks`), while `transition` returns her
nothing (`fixed_hides`).

## Building the proofs

```
h5i app prove examples/app/filters   # extract, build, gate
h5i app mutate examples/app/filters  # the bugs in h5i-app.toml must break a proof
```
