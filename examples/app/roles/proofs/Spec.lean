import StdSpecs
/-!
# The roles specification

`Holds s u r`: user `u` holds role `r`, directly or by inheritance, which is
`Reach` over the inheritance edges from `u`'s direct roles. `May s t u p`:
an entry of table `t` is for a role `u` holds and its pattern covers path
`p`. The kernel computes these with `map::get`, `graph::reachable` and
`bytes::star_match` from `h5i-app-std`; their copied specs let `step*` go
through the calls, so the lemmas below are about the kernel's own loops.
-/
open Aeneas Aeneas.Std Result H5iAppLib roles_kernel

namespace Roles

h5i_derive_clone Doc Doc.Insts.CoreCloneClone.clone

/-- The roles `u` holds directly. -/
def direct (s : Snapshot) (u : U64) : List U64 := ((mapGet u s.members.val).map (·.val)).getD []

/-- `u` holds role `r`, directly or by inheritance. -/
def Holds (s : Snapshot) (u r : U64) : Prop := Reach s.inherits.val (direct s u) r

/-- Some entry of `t` is for a role `u` holds, and its pattern covers `p`. -/
def May (s : Snapshot) (t : List (U64 × alloc.vec.Vec U8)) (u : U64) (p : List U8) : Prop :=
  ∃ e ∈ t, Holds s u e.1 ∧ starMatch STAR e.2.val p

/-- What a reply discloses. -/
def readsOf : Reply → List Doc
  | .Doc d => [d]
  | .Docs ds => ds.val
  | .Done => []

/-- The check `may` makes, for the role list it is given. -/
def covers (t : List (U64 × alloc.vec.Vec U8)) (rs : List U64) (p : List U8) : Prop :=
  ∃ e ∈ t, e.1 ∈ rs ∧ starMatch STAR e.2.val p

instance (t : List (U64 × alloc.vec.Vec U8)) (rs : List U64) (p : List U8) : Decidable (covers t rs p) := by
  unfold covers; infer_instance

/-! ## The kernel's loops -/

@[step] theorem roles_spec (s : Snapshot) (u : U64) :
    roles s u ⦃ o => ∀ rs, o = some rs → ∀ r, r ∈ rs.val ↔ Holds s u r ⦄ := by
  unfold roles
  h5i_steps
  all_goals try (simp_all [Holds, direct]; done)
  -- the search's bound: both lists are under `LIMIT`
  have := usize_max_ge
  simp only [ge_iff_le, not_le, LIMIT] at *
  scalar_tac

@[step] theorem may_spec (t : alloc.vec.Vec (U64 × alloc.vec.Vec U8)) (rs : alloc.vec.Vec U64) (p : alloc.vec.Vec U8) :
    may t rs p ⦃ b => b = decide (covers t.val rs.val p.val) ⦄ := by
  unfold may may_loop
  apply WP.spec_mono (loop_search t.val (fun e => decide (e.1 ∈ rs.val ∧ starMatch STAR e.2.val p.val))
    (fun r : Bool => r) (fun _ _ => true) false _ ?_ 0#usize (by simp))
  · intro r hr; rw [search_any _ _ _ hr, Bool.eq_iff_iff]; simp [covers]
  · intro j hj; unfold may_loop.body; h5i_step [Prod.ext_iff]

@[step] theorem find_doc_spec (docs : alloc.vec.Vec Doc) (k : U64) :
    find_doc docs k ⦃ o => docs.val.find? (fun d => decide (d.id = k)) = o ⦄ := by
  unfold find_doc find_doc_loop
  apply WP.spec_mono (loop_search docs.val (fun d => decide (d.id = k)) id (fun _ d => some d) none _ ?_
    0#usize (by simp))
  · intro r hr; rw [id_eq] at hr; exact (search_find _ _ _ hr).symm
  · intro j hj; unfold find_doc_loop.body; h5i_step

@[step] theorem readable_spec (t : alloc.vec.Vec (U64 × alloc.vec.Vec U8)) (rs : alloc.vec.Vec U64)
    (docs : alloc.vec.Vec Doc) :
    readable t rs docs ⦃ out => out.val = docs.val.filter (fun d => decide (covers t.val rs.val d.path.val)) ⦄ := by
  unfold readable readable_loop
  apply WP.spec_mono (loop_fold docs.val (fun o : alloc.vec.Vec Doc => o.val)
    (fun o d => if decide (covers t.val rs.val d.path.val) then o ++ [d] else o) (fun o j => o.length ≤ j)
    (fun st => readable_loop.body t rs docs st.1 st.2) ?_ (alloc.vec.Vec.new Doc) 0#usize (by simp) (by simp))
  · intro o ho; rw [ho, foldl_filter]; simp
  · intro o i hi hinv; unfold readable_loop.body; h5i_steps
    all_goals simp_all [FoldStep]
    all_goals scalar_tac

end Roles
