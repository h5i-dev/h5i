import Spec
/-!
# The helpers as list functions

Each extracted helper computes its counterpart in `Spec.lean`. Loops use
`H5iAppLib.loop_search` and `H5iAppLib.loop_fold`.
-/
open Aeneas Aeneas.Std Result cratesio_kernel cratesio_kernel.Spec H5iAppLib

namespace cratesio_kernel.Lemmas

attribute [simp] u64_val_eq

/-! ## Lookups -/

@[step] theorem find_user_spec (v : alloc.vec.Vec User) (k : U64) :
    find_user v k ⦃ r => r = v.val.find? (·.id = k) ⦄ := by
  unfold find_user find_user_loop
  apply WP.spec_mono (loop_search v.val (fun x => decide (x.id = k)) id (fun _ x => some x) none _ ?_ 0#usize (by simp))
  · intro r hr; exact search_find _ _ _ hr
  · intro j hj; unfold find_user_loop.body; h5i_step

@[step] theorem find_session_spec (v : alloc.vec.Vec Session) (k : U64) :
    find_session v k ⦃ r => r = v.val.find? (·.id = k) ⦄ := by
  unfold find_session find_session_loop
  apply WP.spec_mono (loop_search v.val (fun x => decide (x.id = k)) id (fun _ x => some x) none _ ?_ 0#usize (by simp))
  · intro r hr; exact search_find _ _ _ hr
  · intro j hj; unfold find_session_loop.body; h5i_step

@[step] theorem find_token_spec (v : alloc.vec.Vec Token) (k : U64) :
    find_token v k ⦃ r => r = v.val.find? (·.id = k) ⦄ := by
  unfold find_token find_token_loop
  apply WP.spec_mono (loop_search v.val (fun x => decide (x.id = k)) id (fun _ x => some x) none _ ?_ 0#usize (by simp))
  · intro r hr; exact search_find _ _ _ hr
  · intro j hj; unfold find_token_loop.body; h5i_step

@[step] theorem find_crate_spec (v : alloc.vec.Vec Krate) (k : U64) :
    find_crate v k ⦃ r => r = v.val.find? (·.id = k) ⦄ := by
  unfold find_crate find_crate_loop
  apply WP.spec_mono (loop_search v.val (fun x => decide (x.id = k)) id (fun _ x => some x) none _ ?_ 0#usize (by simp))
  · intro r hr; exact search_find _ _ _ hr
  · intro j hj; unfold find_crate_loop.body; h5i_step

@[step] theorem find_version_spec (v : alloc.vec.Vec Version) (k n : U64) :
    find_version v k n ⦃ r => r = v.val.find? (fun x => x.krate = k ∧ x.num = n) ⦄ := by
  unfold find_version find_version_loop
  apply WP.spec_mono (loop_search v.val (fun x => decide (x.krate = k ∧ x.num = n)) id (fun _ x => some x) none _ ?_
    0#usize (by simp))
  · intro r hr; exact search_find _ _ _ hr
  · intro j hj; unfold find_version_loop.body; h5i_step

@[step] theorem find_invite_spec (v : alloc.vec.Vec Invite) (k u : U64) :
    find_invite v k u ⦃ r => r = v.val.find? (fun x => x.krate = k ∧ x.user = u) ⦄ := by
  unfold find_invite find_invite_loop
  apply WP.spec_mono (loop_search v.val (fun x => decide (x.krate = k ∧ x.user = u)) id (fun _ x => some x) none _ ?_
    0#usize (by simp))
  · intro r hr; exact search_find _ _ _ hr
  · intro j hj; unfold find_invite_loop.body; h5i_step

@[step] theorem has_owner_spec (v : alloc.vec.Vec Owner) (k o : U64) (t : Bool) :
    has_owner v k o t ⦃ b => b = v.val.any (fun x => decide (x.krate = k ∧ x.owner = o ∧ x.team = t)) ⦄ := by
  unfold has_owner has_owner_loop
  apply WP.spec_mono (loop_search v.val (fun x => decide (x.krate = k ∧ x.owner = o ∧ x.team = t)) id
    (fun _ _ => true) false _ ?_ 0#usize (by simp))
  · intro r hr; simp only [id] at hr; rw [search_bool _ _ _ _ _ hr]; split <;> simp_all
  · intro j hj; unfold has_owner_loop.body; h5i_step

@[step] theorem is_member_spec (v : alloc.vec.Vec U64) (t : U64) :
    is_member v t ⦃ b => b = decide (t ∈ v.val) ⦄ := by
  unfold is_member is_member_loop
  apply WP.spec_mono (loop_search v.val (fun x => decide (x = t)) id (fun _ _ => true) false _ ?_ 0#usize (by simp))
  · intro r hr; simp only [id] at hr; rw [search_bool _ _ _ _ _ hr]; split <;> simp_all
  · intro j hj; unfold is_member_loop.body; h5i_step

@[step] theorem team_owner_in_spec (v : alloc.vec.Vec Owner) (k : U64) (teams : alloc.vec.Vec U64) :
    team_owner_in v k teams ⦃ b => b = v.val.any (fun o => o.krate = k && o.team && decide (o.owner ∈ teams.val)) ⦄ := by
  unfold team_owner_in team_owner_in_loop
  apply WP.spec_mono (loop_search v.val (fun o => o.krate = k && o.team && decide (o.owner ∈ teams.val)) id
    (fun _ _ => true) false _ ?_ 0#usize (by simp))
  · intro r hr; simp only [id] at hr; rw [search_bool _ _ _ _ _ hr]; split <;> simp_all
  · intro j hj; unfold team_owner_in_loop.body; h5i_step

@[step] theorem has_user_owner_spec (v : alloc.vec.Vec Owner) (k : U64) :
    has_user_owner v k ⦃ b => b = v.val.any (fun o => decide (o.krate = k ∧ o.team = false)) ⦄ := by
  unfold has_user_owner has_user_owner_loop
  apply WP.spec_mono (loop_search v.val (fun o => decide (o.krate = k ∧ o.team = false)) id
    (fun _ _ => true) false _ ?_ 0#usize (by simp))
  · intro r hr; simp only [id] at hr; rw [search_bool _ _ _ _ _ hr]; split <;> simp_all
  · intro j hj; unfold has_user_owner_loop.body; h5i_step

@[step] theorem user_owner_besides_spec (v : alloc.vec.Vec Owner) (k o : U64) :
    user_owner_besides v k o ⦃ b => b = v.val.any (fun x => decide (x.krate = k ∧ x.team = false ∧ x.owner ≠ o)) ⦄ := by
  unfold user_owner_besides user_owner_besides_loop
  apply WP.spec_mono (loop_search v.val (fun x => decide (x.krate = k ∧ x.team = false ∧ x.owner ≠ o)) id
    (fun _ _ => true) false _ ?_ 0#usize (by simp))
  · intro r hr; simp only [id] at hr; rw [search_bool _ _ _ _ _ hr]; split <;> simp_all
  · intro j hj; unfold user_owner_besides_loop.body; h5i_step

/-- Some user owner of `k` other than (o, t) remains. -/
def otherUserOwner (l : List Owner) (k o : U64) (t : Bool) : Bool :=
  l.any (fun x => decide (x.krate = k ∧ x.team = false ∧ (t = true ∨ x.owner ≠ o)))

@[step] theorem other_user_owner_spec (v : alloc.vec.Vec Owner) (k o : U64) (t : Bool) :
    other_user_owner v k o t ⦃ b => b = otherUserOwner v.val k o t ⦄ := by
  unfold other_user_owner otherUserOwner
  cases t <;> step*

@[step] theorem count_owners_spec (v : alloc.vec.Vec Owner) (k : U64) :
    count_owners v k ⦃ n => n.val = (v.val.filter (·.krate = k)).length ⦄ := by
  unfold count_owners count_owners_loop
  apply WP.spec_mono (loop_fold v.val (fun n : Usize => n.val) (fun c x => c + if decide (x.krate = k) then 1 else 0)
    (fun n j => n.val ≤ j) (fun x => count_owners_loop.body v k x.1 x.2) ?_ 0#usize 0#usize (by simp) (by simp))
  · intro r hr; rw [hr, foldl_count]; simp
  · intro n j hj hn; have := v.len_ineq; unfold count_owners_loop.body; h5i_step

@[step] theorem has_reverse_dep_spec (v : alloc.vec.Vec Dep) (k : U64) :
    has_reverse_dep v k ⦃ b => b = v.val.any (fun d => decide (d.on = k ∧ d.krate ≠ k)) ⦄ := by
  unfold has_reverse_dep has_reverse_dep_loop
  apply WP.spec_mono (loop_search v.val (fun d => decide (d.on = k ∧ d.krate ≠ k)) id
    (fun _ _ => true) false _ ?_ 0#usize (by simp))
  · intro r hr; simp only [id] at hr; rw [search_bool _ _ _ _ _ hr]; split <;> simp_all
  · intro j hj; unfold has_reverse_dep_loop.body; h5i_step

@[step] theorem crate_exists_spec (v : alloc.vec.Vec Krate) (k : U64) :
    crate_exists v k ⦃ b => b = v.val.any (·.id = k) ⦄ := by
  unfold crate_exists crate_exists_loop
  apply WP.spec_mono (loop_search v.val (fun x => decide (x.id = k)) id (fun _ _ => true) false _ ?_ 0#usize (by simp))
  · intro r hr; simp only [id] at hr; rw [search_bool _ _ _ _ _ hr]; split <;> simp_all
  · intro j hj; unfold crate_exists_loop.body; h5i_step

@[step] theorem deps_known_spec (cs : alloc.vec.Vec Krate) (ds : alloc.vec.Vec U64) :
    deps_known cs ds ⦃ b => b = ds.val.all (fun d => cs.val.any (·.id = d)) ⦄ := by
  unfold deps_known deps_known_loop
  apply WP.spec_mono (loop_search ds.val (fun d => !cs.val.any (·.id = d)) id (fun _ _ => false) true _ ?_
    0#usize (by simp))
  · intro r hr; simp only [id] at hr; rw [search_bool _ _ _ _ _ hr]; split <;> simp_all
  · intro j hj; unfold deps_known_loop.body; h5i_step

/-! ## Constants -/

@[step] theorem invite_ttl_spec : INVITE_TTL ⦃ x => x.val = 2592000 ⦄ := by
  unfold INVITE_TTL DAY; step*

@[step] theorem delete_window_spec : DELETE_WINDOW ⦃ x => x.val = 72 * 3600 ⦄ := by
  unfold DELETE_WINDOW; step*

@[step] theorem invite_expiry_spec (now : U64) :
    invite_expiry now ⦃ e => now.val + 2592000 ≤ U64.max → e.val = now.val + 2592000 ⦄ := by
  unfold invite_expiry
  step*
  all_goals (try simp only [core.num.U64.MAX, U64.rMax] at *); scalar_tac

@[step] theorem age_of_spec (k : Krate) (now : U64) : age_of k now ⦃ a => a.val = age k now ⦄ := by
  unfold age_of age
  split
  · step*
  · simp only [WP.spec_ok]; scalar_tac

/-! ## Signing in, scopes and rights -/

@[step] theorem one_spec (w : Write) : one w ⦃ v => v.val = [w] ⦄ := by
  unfold one; step*

@[step] theorem two_spec (a b : Write) : two a b ⦃ v => v.val = [a, b] ⦄ := by
  unfold two; step*

@[step] theorem is_locked_spec (u : User) (now : U64) : is_locked u now ⦃ b => b = lockedAt u now ⦄ := by
  unfold is_locked lockedAt
  split_ifs <;> simp_all

@[step] theorem token_live_spec (t : Token) (now : U64) : token_live t now ⦃ b => b = live t now ⦄ := by
  unfold token_live live
  split_ifs <;> simp_all

theorem find_id {α} {l : List α} {f : α → U64} {k : U64} {x : α} (h : l.find? (f · = k) = some x) : f x = k := by
  simpa using List.find?_some h

@[step] theorem signed_in_spec (s : Snapshot) (p : Principal) (tok : Option Token) :
    signed_in s p tok ⦃ r => ∀ l, r = .Ok l →
      userOf (Snapshot.toSt s) p.user = some l.user ∧ lockedAt l.user p.now = false ∧ l.token = tok ⦄ := by
  unfold signed_in
  step*
  all_goals (intro l hl; simp_all [userOf, Snapshot.toSt])
  · subst hl; simp_all

@[step] theorem authenticate_spec (s : Snapshot) (p : Principal) :
    authenticate s p ⦃ r => ∀ l, r = .Ok l → SignedIn (Snapshot.toSt s) p l.user l.token ⦄ := by
  unfold authenticate
  split <;> step*
  · -- A session cookie.
    obtain ⟨h1, h2, h3⟩ := r_post _ r_post1
    refine ⟨h1, h2, ?_⟩
    rw [‹p.via = _›]
    refine ⟨h3, ses, by simpa [sessionOf, Snapshot.toSt] using o_post.symm.trans ‹o = some ses›, ?_⟩
    simpa using ‹¬(ses.user != p.user) = true›
  · -- An API token.
    obtain ⟨h1, h2, h3⟩ := r_post _ r_post1
    refine ⟨h1, h2, ?_⟩
    rw [‹p.via = _›]
    exact ⟨t, h3, by simpa [tokenOf, Snapshot.toSt] using o_post.symm.trans ‹o = some t›,
      by simpa using ‹¬(t.user != p.user) = true›, by simpa [b_post] using ‹b = true›⟩

/-- `check_scope` passed: no token, or one that is allowed here and scoped for `e` and `k`. -/
def scopeOk (tok : Option Token) (allow : Bool) (e : Endpoint) (k : Option U64) : Prop :=
  match tok with
  | none => True
  | some t => allow = true ∧ endpointOk t e = true ∧ crateOk t k = true

@[step] theorem endpoint_ok_spec (t : Token) (e : Endpoint) : endpoint_ok t e ⦃ b => b = endpointOk t e ⦄ := by
  unfold endpoint_ok endpointOk
  split_ifs <;> cases e <;> simp_all

@[step] theorem crate_ok_spec (t : Token) (k : Option U64) : crate_ok t k ⦃ b => b = crateOk t k ⦄ := by
  unfold crate_ok crateOk
  split_ifs <;> split <;> (try split) <;> simp_all

@[step] theorem check_scope_spec (tok : Option Token) (allow : Bool) (e : Endpoint) (k : Option U64) :
    check_scope tok allow e k ⦃ r => r = .Ok () → scopeOk tok allow e k ⦄ := by
  unfold check_scope scopeOk
  split <;> step*

@[step] theorem rights_spec (s : Snapshot) (k u : U64) (teams : alloc.vec.Vec U64) :
    rights s.owners k u teams ⦃ r => r = Spec.rights (Snapshot.toSt s) k u teams.val ⦄ := by
  unfold rights Spec.rights
  step* <;> simp only [Snapshot.toSt] <;> rw [← b_post] <;> simp_all

@[step] theorem need_full_spec (r : Rights) : need_full r ⦃ x => x = .Ok () → r = .Full ⦄ := by
  unfold need_full
  split <;> simp

@[step] theorem max_downloads_spec (a : U64) : max_downloads a ⦃ m => m.val = maxDownloads a.val ⦄ := by
  unfold max_downloads maxDownloads
  have : a.val / 86400 + 29 < 2 ^ 64 := by scalar_tac
  step*
  all_goals (simp only [DAY, DOWNLOADS_PER_MONTH] at *; try scalar_tac)

/-- The dependency rows of version `n` of crate `k`. -/
def depWrites (k n : U64) (ds : List U64) : List Write := ds.map (fun d => .PutDep ⟨k, n, d⟩)

@[step] theorem push_deps_spec (ws : alloc.vec.Vec Write) (k n : U64) (ds : alloc.vec.Vec U64)
    (h : ws.length + ds.length ≤ Usize.max) :
    push_deps ws k n ds ⦃ v => v.val = ws.val ++ depWrites k n ds.val ⦄ := by
  unfold push_deps push_deps_loop
  apply WP.spec_mono (loop_fold ds.val (fun v : alloc.vec.Vec Write => v.val) (fun acc d => acc ++ [.PutDep ⟨k, n, d⟩])
    (fun v j => v.length + (ds.length - j) ≤ Usize.max) (fun x => push_deps_loop.body k n ds x.1 x.2) ?_ ws 0#usize
    (by simp) (by simpa using h))
  · intro r hr; rw [hr, foldl_map]; simp [depWrites]
  · intro o j hj ho; unfold push_deps_loop.body; h5i_step

end cratesio_kernel.Lemmas
