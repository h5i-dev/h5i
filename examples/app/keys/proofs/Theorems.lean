import Lemmas
/-!
# Revocation holds across requests

`no_act_after_revoke`: in every serial run of requests from an empty database,
in any order and by any clients, once a key is revoked no later request acts
on its behalf, whether it presents the key or a session opened before the
revocation. The proof combines three one-step facts: an action needs a live key
(`act_live`), no revoked secret is live (`Inv`, kept by every command), and
revoked secrets stay revoked.
-/
open Aeneas Aeneas.Std Result H5iAppLib keys_kernel

namespace Keys

theorem step_spec (a : Cred) (s : Snapshot) (c : Command) (pre : Bool) (R : Snapshot → Cred → Option Key)
    (hR : ∀ c, resolve s c pre ⦃ o => o = R s c ⦄) :
    step a s c pre ⦃ OnOk (fun ws r => Effect R s a c ws.val r) ⦄ := by
  unfold step
  cases c with
  | Issue sec perms =>
    cases a <;> step* <;> simp_all [OnOk]
    exact .issue (by simp_all)
  | Revoke sec =>
    cases a <;> step* <;> simp_all [OnOk]
    exact .revoke
  | Open =>
    step with hR as ⟨o, ho⟩
    cases o with
    | none => simp [OnOk]
    | some k =>
      step*
      all_goals simp_all [OnOk]
      exact .open_ (by simp_all) (fun he => ‹¬ (s.next_session.val = U64.rMax)› (by rw [he]; simp [core.num.U64.MAX]))
  | Act act =>
    step with hR as ⟨o, ho⟩
    cases o with
    | none => simp [OnOk]
    | some k =>
      step*
      all_goals simp_all [OnOk, vec_deref_val]
      exact .act (by simp_all) (by simp_all)

theorem transition_effect {a : Cred} {s : Snapshot} {c : Command} {ws : alloc.vec.Vec Write} {r : Reply}
    (h : transition a s c = ok (.Ok (ws, r))) : Effect resolveM s a c ws.val r :=
  of_spec (step_spec a s c false resolveM (resolve_spec s)) h

theorem transition_pre_effect {a : Cred} {s : Snapshot} {c : Command} {ws : alloc.vec.Vec Write} {r : Reply}
    (h : transition_pre a s c = ok (.Ok (ws, r))) : Effect resolvePreM s a c ws.val r :=
  of_spec (step_spec a s c true resolvePreM (resolve_pre_spec s)) h

/-! ## One step -/

theorem findKey_mem {keys : List Key} {sec : alloc.vec.Vec U8} {k : Key} (h : findKey keys sec = some k) :
    k ∈ keys ∧ k.secret = sec := by
  have := find?_mem h; simp_all

/-- An action runs only on behalf of a live key whose permissions allow it. -/
theorem act_live {a : Cred} {s : Snapshot} {act : Action} {ws : alloc.vec.Vec Write} {k : alloc.vec.Vec U8}
    (h : transition a s (.Act act) = ok (.Ok (ws, .Acted k))) :
    ∃ key ∈ s.keys.val, key.secret = k ∧ grantsM key.perms.val act = true := by
  have he := transition_effect h
  generalize ws.val = l at he
  cases he with
  | act hr hg =>
    rename_i key
    cases a with
    | Admin => simp [resolveM] at hr
    | Key sec => obtain ⟨hm, -⟩ := findKey_mem hr; exact ⟨key, hm, rfl, hg⟩
    | Session id =>
      simp only [resolveM] at hr
      split at hr
      · cases hr
      · obtain ⟨hm, -⟩ := findKey_mem hr; exact ⟨key, hm, rfl, hg⟩

theorem applyAll_revoked (st : St) (ws : List Write) (r : List U8) (h : r ∈ st.revoked) :
    r ∈ (ws.foldl applyW st).revoked := by
  induction ws generalizing st with
  | nil => exact h
  | cons w ws ih => apply ih; cases w <;> simp [applyW, h]

/-- Revoked secrets stay revoked. -/
theorem revoked_stays (st : St) (ws : alloc.vec.Vec Write) (r : List U8) (h : r ∈ st.revoked) :
    r ∈ (applyAll st ws).revoked :=
  applyAll_revoked st ws.val r h

theorem inv_step (a : Cred) (s : Snapshot) (c : Command) (ws : alloc.vec.Vec Write) (r : Reply)
    (hi : Inv (toSt s)) (h : transition a s c = ok (.Ok (ws, r))) : Inv (applyAll (toSt s) ws) := by
  unfold applyAll
  have he := transition_effect h
  generalize ws.val = l at he
  cases he with
  | @issue sec perms ht =>
    simp only [takenM, Bool.or_eq_false_iff, Option.isSome_eq_false_iff, List.any_eq_false] at ht
    intro r hr k hk
    simp only [List.foldl, applyW, List.mem_append, List.mem_singleton] at hr hk
    rcases hk with hk | rfl
    · exact hi r hr k hk
    · simp only [toSt, List.mem_map] at hr
      obtain ⟨r', hr', rfl⟩ := hr
      intro he
      exact ht.2 r' hr' (by simp [he, alloc.vec.Vec.eq_iff])
  | @revoke sec =>
    intro r hr k hk
    simp only [List.foldl, applyW, List.mem_append, List.mem_singleton, List.mem_filter] at hr hk
    rcases hr with hr | rfl
    · exact hi r hr k hk.1
    · intro he
      have h2 : k.secret ≠ sec := by simpa using hk.2
      exact h2 ((alloc.vec.Vec.eq_iff _ _).2 he)
  | open_ => exact hi
  | act => exact hi

theorem inv_init : Inv init := by simp [Inv, init]

theorem revoke_revokes (s : Snapshot) (sec : alloc.vec.Vec U8) (ws : alloc.vec.Vec Write) (r : Reply)
    (h : transition .Admin s (.Revoke sec) = ok (.Ok (ws, r))) : sec.val ∈ (applyAll (toSt s) ws).revoked := by
  have he := transition_effect h
  unfold applyAll
  generalize ws.val = l at he
  cases he
  simp [applyW]

/-! ## Every run -/

/-- The runs of the kernel from an empty database. -/
abbrev Runs := Run transition toSt applyAll init

/-- Once a key is revoked, no later request acts on its behalf, in every run. -/
theorem no_act_after_revoke {st : St} {evs : List (Event Cred Command Reply keys_kernel.Error)}
    (h : Runs st evs) {pre post : List (Event Cred Command Reply keys_kernel.Error)}
    {sec : alloc.vec.Vec U8} {r : Reply} (he : evs = pre ++ .ok .Admin (.Revoke sec) r :: post)
    {a : Cred} {act : Action} {k : alloc.vec.Vec U8} (hin : .ok a (.Act act) (.Acted k) ∈ post) : k ≠ sec := by
  obtain ⟨-, hpost⟩ := Run.after (fun st => sec.val ∈ st.revoked)
    (fun _ _ _ ws _ hq _ => revoked_stays _ ws _ hq)
    (fun s ws ht => revoke_revokes s sec ws r ht) h pre post he
  obtain ⟨s, evs', hr, hq, ws, ht⟩ := hpost _ hin
  have hinv := Run.inv Inv inv_init inv_step hr
  obtain ⟨key, hkm, hks, -⟩ := act_live ht
  rintro rfl
  exact hinv _ hq key hkm (by rw [hks])

end Keys
