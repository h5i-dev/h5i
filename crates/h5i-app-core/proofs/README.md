# H5iAppLib

Lemmas and tactics for proofs about Aeneas-extracted h5i-app kernels.
`h5i app new` writes a `Theorems.lean` that starts:

```lean
import MyKernel
import H5iAppLib
open Aeneas Aeneas.Std Result Aeneas.Std.WP H5iAppLib

h5i_derive_all
```

With those `open`s every name below is written as shown. `Result` is an
interaction tree: `rfl`, `cases h` and `injection h` on `f x = ok y` usually
fail. Use the tactics and lemmas here instead.

## Tactics

| Tactic | Does |
|---|---|
| `h5i_invert h` | `h : f x = ok y` with `f` unfolded: one goal per successful path. `let x ← m` leaves `x` and `hx : m = ok x`, a branch condition `hc` (`h5i_invert h with hcond` renames it), `r?` splits into `Ok`/`Err`, and a final `h : x = e` is substituted |
| `h5i_arith` | `h5i_ok_facts`, then `scalar_tac`: goals about values computed by `i + 1#usize = ok j` and the like |
| `h5i_ok_facts` | add `h_val` for each `h : x + y = ok z` (`-`, `*`, `/`, `%`, `index_usize`, `Vec.index`) |
| `h5i_steps` | `step*`, also through `if`/`match` on calls |
| `h5i_search_any l P` | after `unfold f f_loop`, a loop that returns `true` at the first `x ∈ l` with `P x`: it returns `l.any P` |
| `h5i_search_all l P` | ...`false` at the first match: `l.all (fun x => !P x)` |
| `h5i_search_find l P` | ...`some x` at the first match: `l.find? P` |
| `h5i_total idx n` | `loop body x ⦃ _ ⦄` over an index `idx` bounded by `n` (measure `n - idx`); `h5i_total (fun i => i) v.length` |
| `h5i_measure_induction e with ih` | strong induction on the number `e`, everything else generalized: for functions recursing on slice indexes, `e := p.length - pi + (n.length - ni)` |
| `h5i_bv` | bit goals (`&&&`, `|||`, `^^^`, `~~~`, `<<<`, `MAX`, `=`) on unsigned scalars, by `bvify` and `bv_decide` |
| `h5i_derive_all` | (command) `h5i_derive_eq`/`h5i_derive_clone` for every extracted `PartialEq`/`Clone` |
| `h5i_derive_eq T f`, `h5i_derive_clone T f` | (commands) the same for one type |
| `@[h5i_spec]` | on `thm : f xs ⦃ r => P r ⦄`: adds `thm.inv : f xs = ok r → P r` and, for `r = g`, `thm.eq : f xs = ok g` |
| `h5i_eval (f g) [lemmas]` | evaluate a concrete call. For a concrete enum argument use `simp [f]`, not `rfl` |

## Lemmas by the hypothesis you have

| You have | Lemma | You get |
|---|---|---|
| `ok a = ok b` | `Result.ok.inj`, `result_ok_inj` | `a = b` |
| `ok a = fail e` | `ok_ne_fail`, `fail_ne_ok` | `False` |
| `.Ok (a, b) = .Ok (a', b')` | `ok_inj` | `a = a' ∧ b = b'` |
| `(do let a ← x; f a) = ok y` | `bind_tc_eq_ok`, `bind_eq_ok` (`Std.bind`) | `∃ a, x = ok a ∧ f a = ok y` |
| `x + y = ok z` | `add_ok_val`, `add_ok_bound` | `z.val = x.val + y.val`, `≤ max` |
| `x - y = ok z` | `sub_ok_val` | `z.val = x.val - y.val ∧ y.val ≤ x.val` |
| `x * y`, `x / y`, `x % y` `= ok z` | `mul_ok_val`, `div_ok_val`, `rem_ok_val` | the value (and `y.val ≠ 0`) |
| `saturating_sub x y`, `saturating_add x y` | `saturating_sub_val`, `saturating_add_val` | `x.val - y.val`, `min (x.val + y.val) max` |
| `UScalar.cast tgt x` | `cast_val` (`x.val ≤ max tgt`), `usize_cast_u64` | `x.val` |
| `v.index_usize i = ok x` | `vec_index_ok`, `vec_index_ok_get?`, `vec_index_ok_mem` | `v.val[i.val] = x`, `x ∈ v.val` |
| `Vec.index (SliceIndexUsizeSlice _) v i = ok x` | `vec_index_slice_ok_get?`, `vec_index_slice_ok_mem` | the same |
| `s.index_usize i = ok x` (slice) | `slice_index_ok`, `slice_index_ok_mem` | the same |
| `branch r` then `match` (`r?`) | `bind_branch`, `bind_tc_branch`, `from_residual_same` | a `match` on `r` |
| `m ⦃ P ⦄` and `m = ok x` | `post_of_ok` | `P x` |
| `m ⦃ x => x = v ⦄` | `eq_ok_of_spec` | `m = ok v` |
| `m ⦃ P ⦄` | `ok_of` | `∃ r, m = ok r` |
| `ok x ⦃ P ⦄` | `spec_ok` (from `WP`) | `P x` |

## Loops

Aeneas extracts `while i < v.len() { ... }` as `loop body i`.

| Goal | Use |
|---|---|
| a spec, the loop stops at the first match | `loop_search` + `search_any`/`search_all`/`search_find`/`search_findIdx`, or the `h5i_search_*` tactics |
| a spec, the loop runs to the end | `loop_fold`, `loop_fold2` (`foldl_count`, `foldl_filter`, `foldl_map`, ...) |
| `for x in v.iter()` | `iter_loop`, `iter_fold`, `iter_search`, `iter_any`, `iter_find`, `iter_filter_map` (`h5i_iter`) |
| from `loop body x = ok y` | `loop_ok` (any measure), `loop_idx_ok` (measure `n - idx`) |
| from `loop ... = ok true` | `loop_true_witness`: each `done true` step has a witness |
| from `loop ... = ok false` | `loop_false_all`: each `done false` step establishes the claim |
| the loop does not fail | `loop_idx_spec`, `h5i_total` |

To prove that a function's result does not depend on part of the database,
first prove its spec as a list function (`= l.any P`, `= l.find? P`). Then
prove the property on lists, where `List.any_filter` and its relatives
already exist. Do not compare the two runs directly.

## Bits

| Goal | Use |
|---|---|
| `held &&& req = req` (bitflags) | `Covers`, `covers_iff_testBit`, `covers_or_iff`, `Covers.trans` |
| `x &&& m = y &&& m` with `m = MAX <<< s` (CIDR) | `u32_masked_eq_iff`, `u64_masked_eq_iff`, `masked_eq_iff`: `x.val / 2^s = y.val / 2^s` |
| `MAX.bv` | `u32_max_bv` and friends: `BitVec.allOnes _` |
| anything else on bits | `h5i_bv` |
