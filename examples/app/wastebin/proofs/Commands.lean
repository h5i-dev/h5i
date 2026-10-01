import Spec
/-!
# What each command does

List specs for the helpers, then one lemma per command giving its writes and
reply on success. The theorems use only these lemmas.
-/
open Aeneas Aeneas.Std Result wastebin_kernel wastebin_kernel.Spec H5iAppLib

namespace wastebin_kernel.Commands

attribute [simp] u64_val_eq

/-! ## Helpers -/

theorem paste_clone (p : Paste) : Paste.Insts.CoreCloneClone.clone p = ok p := by
  simp [Paste.Insts.CoreCloneClone.clone, u8vec_clone]

@[step] theorem paste_clone_spec (p : Paste) : Paste.Insts.CoreCloneClone.clone p ⦃ q => q = p ⦄ := by
  simp [paste_clone]

@[step] theorem one_spec (w : Write) : one w ⦃ v => v.val = [w] ⦄ := by
  unfold one; step*

@[step] theorem expired_spec (p : Paste) (now : U64) : expired p now ⦃ b => b = isExpired now.val p ⦄ := by
  unfold expired isExpired
  split <;> simp_all

@[step] theorem find_slug_spec (ps : alloc.vec.Vec Paste) (k : U64) :
    find_slug ps k ⦃ o => o = ps.val.find? (fun p => p.slug.val = k.val) ⦄ := by
  unfold find_slug find_slug_loop
  apply WP.spec_mono (loop_search ps.val (fun p => decide (p.slug = k)) (fun o : Option Paste => o)
    (fun _ p => some p) none _ ?_ 0#usize (by simp))
  · intro r hr; rw [search_find _ _ _ hr]; simp
  · intro j hj; unfold find_slug_loop.body; h5i_step

@[step] theorem has_uid_spec (uids : alloc.vec.Vec U64) (u : U64) :
    has_uid uids u ⦃ b => b = true ↔ u ∈ uids.val ⦄ := by
  unfold has_uid has_uid_loop
  apply WP.spec_mono (loop_search uids.val (fun x => decide (x = u)) (fun b : Bool => b)
    (fun _ _ => true) false _ ?_ 0#usize (by simp))
  · intro r hr
    rw [hr, searchFrom_const]
    simp
  · intro j hj; unfold has_uid_loop.body; h5i_step

theorem findSlug_eq (s : Snapshot) (k : U64) :
    findSlug (Snapshot.toSt s) k.val = s.pastes.val.find? (fun p => p.slug.val = k.val) := rfl

/-! ## Creating a paste -/

@[step] theorem deadline_spec (now : U64) (e : Option U32) :
    deadline now e ⦃ r => ∀ t, r = .Ok t → t.map (·.val) = e.map (fun x => now.val + x.val) ⦄ := by
  unfold deadline
  split
  · simp
  · step*
    all_goals subst_vars; simp_all [core.num.U64.MAX, U64.rMax] <;> scalar_tac

@[step] theorem owner_for_spec (uids : alloc.vec.Vec U64) (last : U64) :
    owner_for uids last ⦃ r => ∀ o l, r = .Ok (o, l) →
      (uids.val.head? = some o ∧ l = last) ∨ (uids.val = [] ∧ o.val = last.val + 1 ∧ l = o) ⦄ := by
  unfold owner_for
  step*
  · intro o l h; obtain ⟨rfl, rfl⟩ := ok_inj h
    left
    refine ⟨?_, rfl⟩
    have hl : 0 < uids.val.length := by have := ‹uids.len > 0#usize›; scalar_tac
    cases hu : uids.val with
    | nil => simp [hu] at hl
    | cons x xs => simp_all
  · simp_all [core.num.U64.MAX, U64.rMax]; scalar_tac
  · intro o l h; obtain ⟨rfl, rfl⟩ := ok_inj h
    right
    refine ⟨?_, by scalar_tac, rfl⟩
    have hl : uids.val.length = 0 := by have := ‹¬uids.len > 0#usize›; scalar_tac
    exact List.eq_nil_of_length_eq_zero hl

/-- A paste under the next id and the request's unused slug, and the counter moved on. -/
theorem create_spec (a : Principal) (s : Snapshot) (t : alloc.vec.Vec U8) (e : Option U32) (b : Bool)
    (lock : Option U64) :
    create a s t e b lock ⦃ r => ∀ ws rep, r = .Ok (ws, rep) →
      ∃ p c, p.id = s.counter.next_id ∧ p.slug = a.fresh ∧ p.text = t ∧ p.burn = b ∧ p.lock = lock ∧
        p.expires.map (·.val) = e.map (fun x => a.now.val + x.val) ∧
        findSlug (Snapshot.toSt s) a.fresh.val = none ∧
        ((a.uids.val.head? = some p.owner ∧ c.last_uid = s.counter.last_uid) ∨
          (a.uids.val = [] ∧ p.owner.val = s.counter.last_uid.val + 1 ∧ c.last_uid = p.owner)) ∧
        c.next_id.val = s.counter.next_id.val + 1 ∧
        ws.val = [.PutPaste p, .SetCounter c] ∧ rep = .Created a.fresh p.owner ⦄ := by
  unfold create
  step*
  obtain ⟨owner, lu⟩ := o1
  step*
  · simp only [core.num.U64.MAX, U64.rMax] at *; scalar_tac
  intro ws rep h; obtain ⟨rfl, rfl⟩ := ok_inj h
  have ho := r1_post owner lu ‹_›
  refine ⟨⟨s.counter.next_id, a.fresh, owner, v, t, b, lock⟩, ⟨i, lu⟩, rfl, rfl, v_post, rfl, rfl, r_post t ‹_›, ?_, ?_, by scalar_tac,
    by simp [ws1_post, ws_post], rfl⟩
  · have := ‹¬o.isSome = true›
    rw [findSlug_eq, ← o_post]
    simpa using this
  · rcases ho with ⟨h1, rfl⟩ | ⟨h1, h2, rfl⟩
    · exact .inl ⟨h1, rfl⟩
    · exact .inr ⟨h1, h2, rfl⟩

/-! ## Reading a paste -/

/-- What a read of `p` does: an expired paste is deleted and not shown;
otherwise, past the password check, it is shown, and deleted if it burns. -/
def ReadPost (a : Principal) (p : Paste) (key : Option U64) (ws : List Write) (rep : Reply) : Prop :=
  (isExpired a.now.val p = true ∧ ws = [.DelPaste p.id] ∧ rep = .Gone) ∨
  (isExpired a.now.val p = false ∧ Unlocked p key ∧ ∃ v, rep = .Shown v ∧ Shows a p v ∧
    ws = if p.burn then [.DelPaste p.id] else [])

theorem read_spec (a : Principal) (p : Paste) (key : Option U64) :
    read a p key ⦃ r => ∀ ws rep, r = .Ok (ws, rep) → ReadPost a p key ws.val rep ⦄ := by
  unfold read
  step*
  all_goals
    intro ws rep h; obtain ⟨rfl, rfl⟩ := ok_inj h
    simp_all [ReadPost, Shows, Unlocked]

theorem fetch_spec (a : Principal) (s : Snapshot) (k : U64) (key : Option U64) :
    fetch a s k key ⦃ r => ∀ ws rep, r = .Ok (ws, rep) →
      ∃ p, findSlug (Snapshot.toSt s) k.val = some p ∧ ReadPost a p key ws.val rep ⦄ := by
  unfold fetch
  step*
  exact WP.spec_mono (read_spec a _ key)
    (fun r hr ws rep h => ⟨_, by rw [findSlug_eq, ← o_post]; assumption, hr ws rep h⟩)

/-- The paste page: a burn-after-reading paste without confirmation gets the
confirmation page and nothing else. -/
theorem view_spec (a : Principal) (s : Snapshot) (k : U64) (confirm : Bool) (key : Option U64) :
    view a s k confirm key ⦃ r => ∀ ws rep, r = .Ok (ws, rep) →
      ∃ p, findSlug (Snapshot.toSt s) k.val = some p ∧
        ((p.burn = true ∧ confirm = false ∧ ws.val = [] ∧ rep = .ConfirmBurn) ∨
          ((p.burn = false ∨ confirm = true) ∧ ReadPost a p key ws.val rep)) ⦄ := by
  unfold view
  step*
  all_goals first
    | (simp; done)
    | exact WP.spec_mono (read_spec a _ key)
        (fun r hr ws rep h => ⟨_, by rw [findSlug_eq, ← o_post]; assumption, .inr ⟨by simp_all, hr ws rep h⟩⟩)
    | (intro ws rep h
       simp only [core.result.Result.Ok.injEq, Prod.mk.injEq] at h
       obtain ⟨rfl, rfl⟩ := h
       exact ⟨_, by rw [findSlug_eq, ← o_post]; assumption, .inl (by simp_all)⟩)

/-! ## Deleting and purging -/

theorem delete_spec (a : Principal) (s : Snapshot) (k : U64) :
    delete a s k ⦃ r => ∀ ws rep, r = .Ok (ws, rep) →
      ∃ p, findSlug (Snapshot.toSt s) k.val = some p ∧ p.owner ∈ a.uids.val ∧
        ws.val = [.DelPaste p.id] ∧ rep = .Done ⦄ := by
  unfold delete
  step*
  intro ws rep h; obtain ⟨rfl, rfl⟩ := ok_inj h
  exact ⟨_, by rw [findSlug_eq, ← o_post]; assumption, b_post.1 ‹_›, v_post, rfl⟩

def purgeStep (now : Nat) (acc : List Write) (p : Paste) : List Write :=
  if isExpired now p then acc ++ [.DelPaste p.id] else acc

theorem foldl_purgeStep (now : Nat) (l : List Paste) (acc : List Write) :
    l.foldl (purgeStep now) acc = acc ++ (l.filter (isExpired now)).map (fun p => .DelPaste p.id) :=
  foldl_filter_map _ _ l acc

/-- Purge deletes exactly the expired pastes. -/
theorem purge_spec (now : U64) (s : Snapshot) :
    purge now s ⦃ r => ∃ ws, r = .Ok (ws, .Done) ∧
      ws.val = (s.pastes.val.filter (isExpired now.val)).map (fun p => .DelPaste p.id) ⦄ := by
  unfold purge purge_loop
  have hl := loop_fold s.pastes.val (fun w : alloc.vec.Vec Write => w.val) (purgeStep now.val)
    (fun w j => w.length ≤ j) (fun x => purge_loop.body now s x.1 x.2) ?_ (alloc.vec.Vec.new Write) 0#usize
    (by simp) (by simp)
  · step*
    exact ⟨_, rfl, by simp_all [foldl_purgeStep]⟩
  · intro o j hj ho; have := s.pastes.len_ineq; unfold purge_loop.body; h5i_step
    all_goals first
      | exact ⟨by scalar_tac, by simp [purgeStep, *]⟩
      | exact ⟨⟨by scalar_tac, by simp [purgeStep, *]⟩, by scalar_tac⟩

end wastebin_kernel.Commands
