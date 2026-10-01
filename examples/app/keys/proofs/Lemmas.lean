import Spec
/-!
# What the extracted functions compute

Every loop is a `for` over a slice; each lemma is a list form from
`H5iAppLib.Iter` and one `h5i_iter`.
-/
open Aeneas Aeneas.Std Result H5iAppLib keys_kernel

namespace Keys

@[step] theorem permits_spec (p : Perm) (a : Action) : permits p a ⦃ b => b = permitsM p a ⦄ := by
  unfold permits
  cases p <;> cases a <;> step* <;> simp_all [permitsM]

h5i_derive_clone Perm Perm.Insts.CoreCloneClone.clone
h5i_derive_clone Key Key.Insts.CoreCloneClone.clone
h5i_derive_clone Session Session.Insts.CoreCloneClone.clone

/-- `grants` looks at the whole permission list. -/
@[step] theorem grants_spec (ps : Slice Perm) (a : Action) : grants ps a ⦃ b => b = grantsM ps.val a ⦄ := by
  h5i_for grants using (iter_any ps (permitsM · a) true false _ ?_) [grantsM]

@[step] theorem find_key_spec (keys : Slice Key) (sec : alloc.vec.Vec U8) :
    find_key keys sec ⦃ o => findKey keys.val sec = o ⦄ := by
  h5i_for find_key using (iter_find keys (fun k => k.secret = sec) _ ?_) [findKey]

@[step] theorem is_revoked_spec (revoked : Slice (alloc.vec.Vec U8)) (sec : alloc.vec.Vec U8) :
    is_revoked revoked sec ⦃ b => b = revoked.val.any (fun r => r = sec) ⦄ := by
  h5i_for is_revoked using (iter_any revoked (fun r => r = sec) true false _ ?_)

@[step] theorem find_session_spec (sessions : Slice Session) (id : U64) :
    find_session sessions id ⦃ o => sessions.val.find? (fun x => x.id = id) = o ⦄ := by
  h5i_for find_session using (iter_find sessions (fun x => x.id = id) _ ?_)

@[step] theorem taken_spec (s : Snapshot) (sec : alloc.vec.Vec U8) :
    taken s sec ⦃ b => b = takenM s sec ⦄ := by
  unfold taken
  step*
  all_goals simp_all [takenM, vec_deref_val]

@[step] theorem live_spec (keys : Slice Key) (sec : alloc.vec.Vec U8) :
    live keys sec ⦃ o => o = findKey keys.val sec ⦄ := by
  unfold live
  step*

@[step] theorem resolve_spec (s : Snapshot) (c : Cred) : resolve s c false ⦃ o => o = resolveM s c ⦄ := by
  unfold resolve
  cases c <;> step* <;> simp_all [resolveM, vec_deref_val, -List.find?_eq_none]

@[step] theorem resolve_pre_spec (s : Snapshot) (c : Cred) : resolve s c true ⦃ o => o = resolvePreM s c ⦄ := by
  unfold resolve
  cases c <;> step* <;> simp_all [resolvePreM, vec_deref_val, -List.find?_eq_none]

end Keys
