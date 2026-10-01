import Spec
/-!
# What the extracted functions compute

Each loop is a `for` over a slice, proven with `iter_fold`, `iter_loop` or a
list form from `H5iAppLib.Iter`.
-/
open Aeneas Aeneas.Std Result H5iAppLib filters_kernel

namespace Filters

@[step] theorem escape_into_spec (out : alloc.vec.Vec U8) (v : Slice U8)
    (h : out.val.length + 2 * v.val.length ≤ Usize.max) :
    escape_into out v ⦃ o => o.val = out.val ++ escM v.val ⦄ := by
  h5i_for escape_into using (iter_fold v (fun (o : alloc.vec.Vec U8) => o.val) (fun acc b => acc ++ escB b)
    (fun o i => o.val.length ≤ out.val.length + 2 * i) _ ?_ 0 out (by simp) (by simp))
    [escB] [escM, foldl_append_flatMap]

@[step] theorem escape_into_pre_spec (out : alloc.vec.Vec U8) (v : Slice U8)
    (h : out.val.length + 2 * v.val.length ≤ Usize.max) :
    escape_into_pre out v ⦃ o => o.val = out.val ++ escPreM v.val ⦄ := by
  h5i_for escape_into_pre using (iter_fold v (fun (o : alloc.vec.Vec U8) => o.val) (fun acc b => acc ++ escPreB b)
    (fun o i => o.val.length ≤ out.val.length + 2 * i) _ ?_ 0 out (by simp) (by simp))
    [escPreB] [escPreM, foldl_append_flatMap]

theorem max_len_val : MAX_LEN.val = 4096 := by unfold MAX_LEN; rfl

theorem usize_max_ge : 2 ^ 32 - 1 ≤ Usize.max := by
  rw [Usize.max_def]; cases System.Platform.numBits_eq <;> simp_all [Usize.numBits]

@[step] theorem substitute_spec (tpl nm : Slice U8) (ht : tpl.val.length ≤ 4096) (hn : nm.val.length ≤ 4096) :
    substitute tpl nm ⦃ o => o.val = substM escM nm.val tpl.val ⦄ := by
  have := usize_max_ge; have := escM_length nm.val
  h5i_for substitute using (iter_fold tpl (fun (o : alloc.vec.Vec U8) => o.val)
    (fun acc b => acc ++ (if b = HOLE then escM nm.val else [b]))
    (fun o i => o.val.length ≤ 8193 * i) _ ?_ 0 _ (by simp) (by simp)) [substM, foldl_append_flatMap]

@[step] theorem substitute_pre_spec (tpl nm : Slice U8) (ht : tpl.val.length ≤ 4096) (hn : nm.val.length ≤ 4096) :
    substitute_pre tpl nm ⦃ o => o.val = substM escPreM nm.val tpl.val ⦄ := by
  have := usize_max_ge; have := escPreM_length nm.val
  h5i_for substitute_pre using (iter_fold tpl (fun (o : alloc.vec.Vec U8) => o.val)
    (fun acc b => acc ++ (if b = HOLE then escPreM nm.val else [b]))
    (fun o i => o.val.length ≤ 8193 * i) _ ?_ 0 _ (by simp) (by simp)) [substM, foldl_append_flatMap]

/-- The parser's loop state read as a model state. -/
def absPS (x : alloc.vec.Vec Clause × alloc.vec.Vec U8 × alloc.vec.Vec U8 × St) : PS :=
  (x.1.val.map toCL, x.2.1.val, x.2.2.1.val, x.2.2.2)

@[step] theorem parse_spec (s : Slice U8) :
    parse s ⦃ o => parseM s.val = o.map (fun v => v.val.map toCL) ⦄ := by
  h5i_for parse using (iter_loop s absPS (fun (o : Option (alloc.vec.Vec Clause)) => o.map (fun v => v.val.map toCL))
    pstep pfin (fun x i => x.1.val.length ≤ i ∧ x.2.1.val.length ≤ i ∧ x.2.2.1.val.length ≤ i)
    _ ?_ 0 (alloc.vec.Vec.new Clause, alloc.vec.Vec.new U8, alloc.vec.Vec.new U8, St.Key) (by simp) (by simp))
    [absPS, pstep, pfin, toCL, usize_ofNatCore_eq_zero] []

@[step] theorem field_spec (r : Record) (k : Slice U8) :
    field r k ⦃ o => fieldM r k.val = o.map (·.val) ⦄ := by
  unfold field
  step*
  all_goals simp_all [fieldM, Slice.eq_iff, ownerKey, tagKey]

@[step] theorem matches_spec (r : Record) (f : Slice Clause) :
    «matches» r f ⦃ b => b = matchesM (f.val.map toCL) r ⦄ := by
  h5i_for «matches» using (iter_any f (fun c => decide (fieldM r c.key.val = some c.val.val)) true false _ ?_)
    [vec_deref_val, alloc.vec.Vec.eq_iff] [matchesM, List.any_map, toCL, Function.comp_def]

h5i_derive_clone Record Record.Insts.CoreCloneClone.clone

@[step] theorem select_spec (recs : Slice Record) (f : Slice Clause) :
    select recs f ⦃ o => o.val = recs.val.filter (matchesM (f.val.map toCL)) ⦄ := by
  h5i_for select using (iter_filter_map recs (matchesM (f.val.map toCL)) id _ ?_)

@[step] theorem find_rule_spec (rules : Slice Rule) (id : U64) :
    find_rule rules id ⦃ o => rules.val.find? (fun r => r.id = id) = o ⦄ := by
  h5i_for find_rule using (iter_find rules (fun r => r.id = id) _ ?_)

@[step] theorem too_long_spec (tpl nm : Slice U8) :
    too_long tpl nm ⦃ b => b = decide (4096 < tpl.val.length ∨ 4096 < nm.val.length) ⦄ := by
  unfold too_long
  step*
  all_goals simp_all [max_len_val]

/-- What a search through a template computes, before or after the fix. -/
def RunPost (pre : Bool) (nm tpl : List U8) (recs : List Record) :
    core.result.Result (alloc.vec.Vec Write × Reply) filters_kernel.Error → Prop := fun res =>
  if 4096 < tpl.length ∨ 4096 < nm.length then res = .Err .TooLong
  else match parseM (substM (if pre then escPreM else escM) nm tpl) with
    | none => res = .Err .BadFilter
    | some cs => ∃ rs, res = .Ok (alloc.vec.Vec.new Write, .Records rs) ∧ rs.val = recs.filter (matchesM cs)

@[step] theorem run_rule_spec (nm : Slice U8) (recs : Slice Record) (tpl : Slice U8) (pre : Bool) :
    run_rule nm recs tpl pre ⦃ RunPost pre nm.val tpl.val recs.val ⦄ := by
  unfold run_rule RunPost
  h5i_steps
  all_goals simp_all [vec_deref_val]

end Filters
