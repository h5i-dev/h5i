import Theorems
/-!
# The extracted `apply` computes `applyAll`

So `Runs`, built on `applyAll`, describes the states the Rust `apply` commits,
as long as no table outgrows a `Vec`.
-/
open Aeneas Aeneas.Std Result H5iAppLib keys_kernel

namespace Keys

h5i_derive_clone Snapshot Snapshot.Insts.CoreCloneClone.clone

@[step] theorem without_spec (keys : Slice Key) (sec : alloc.vec.Vec U8) :
    without keys sec ⦃ o => o.val = keys.val.filter (fun k => k.secret ≠ sec) ⦄ := by
  h5i_for without using (iter_filter_map keys (fun k => k.secret ≠ sec) id _ ?_)

/-- Room for every write in every table. -/
def Fits (s : Snapshot) (n : Nat) : Prop :=
  s.keys.val.length + n ≤ Usize.max ∧ s.revoked.val.length + n ≤ Usize.max ∧
    s.sessions.val.length + n ≤ Usize.max

theorem apply_spec (s : Snapshot) (ws : Slice Write) (h : Fits s ws.val.length) :
    apply s ws ⦃ t => toSt t = ws.val.foldl applyW (toSt s) ⦄ := by
  h5i_for apply using (iter_fold ws toSt applyW (fun t i => Fits t (ws.val.length - i)) _ ?_ 0 _
    (by simp) (by simpa using h)) [Fits, toSt, applyW, vec_deref_val] []
  refine ⟨le_trans (Nat.add_le_add_right (List.length_filter_le _ _) _) (by omega), by omega, by omega⟩

end Keys
