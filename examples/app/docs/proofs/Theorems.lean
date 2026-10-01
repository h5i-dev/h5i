import Transition
import Apply
/-!
# Main theorems, about the extracted kernel

Each theorem quantifies over every actor and every command, so a faulty JSON
decoder cannot produce a command these theorems do not cover.
-/
open Aeneas Aeneas.Std Result docs_kernel docs_kernel.Spec docs_kernel.TransitionLemmas H5iAppLib

namespace docs_kernel.Theorems

/-- The kernel's permission table is the spec's. -/
theorem allows_eq (r : Role) (a : Action) : allows r a = .ok (policy r a) := by
  cases r <;> cases a <;> rfl

/-- No input makes the kernel fail: no panic, overflow, or bad index. -/
theorem transition_total (a : Principal) (s : Snapshot) (c : Command) :
    ∃ r, transition a s c = .ok r := by
  have h : transition a s c ⦃ _ => True ⦄ := by walk transition
  obtain ⟨r, hr, _⟩ := (WP.spec_equiv_exists _ _).1 h
  exact ⟨r, hr⟩

/-- `apply` computes the list semantics of a write set, as long as no vector
overflows `usize`. -/
theorem apply_eq (s : Snapshot) (ws : alloc.vec.Vec Write)
    (h : s.projects.length + s.members.length + s.documents.length + s.webhooks.length + ws.length < Usize.max) :
    ∃ s', apply s ws = .ok s' ∧ Snapshot.toSt s' = applyAll (Snapshot.toSt s) ws.val :=
  (WP.spec_equiv_exists _ _).1 (ApplyLemmas.apply_spec s ws h)

theorem findDoc_fresh {s : St} (h : ∀ d ∈ s.docs, d.id.val < s.next) :
    findDoc s.docs s.next = none := by
  unfold findDoc
  rw [List.find?_eq_none]
  intro d hd
  have := h d hd
  simp only [decide_eq_true_eq]
  omega

/-- Every write of a successful transition is permitted by the policy. -/
theorem authorized (a : Principal) (s : Snapshot) (c : Command) ws r
    (hinv : Inv (Snapshot.toSt s)) (h : transition a s c = .ok (.Ok (ws, r))) :
    ∀ w ∈ ws.val, writeAllowed (Snapshot.toSt s) a.user.val w := by
  refine of_spec (P := fun ws _ => ∀ w ∈ ws.val, writeAllowed (Snapshot.toSt s) a.user.val w) ?_ h
  walk transition
  all_goals (simp only [OnOk]; try trivial)
  all_goals (try (simp_all [writeAllowed, Snapshot.toSt]; done))
  all_goals (try (
    have hfresh := findDoc_fresh hinv.doc_fresh
    intro w hw
    simp [ws1_post, ws_post] at hw
    rcases hw with rfl | rfl <;> simp_all [writeAllowed, Snapshot.toSt]))
  all_goals (try subst r_post)
  all_goals (try obtain ⟨hf, hread, hact⟩ := authDoc_ok ‹authDoc _ _ _ _ = .Ok _›)
  all_goals (try have hid := (findDoc_some hf).2)
  all_goals (try rw [← hid] at hf)
  all_goals (try (rcases r1_post with ⟨_, h'⟩ | ⟨ver, hver, h'⟩))
  all_goals (try have hwr := allowed_write_of (.inl rfl) hact)
  all_goals (try have hwr := allowed_write_of (.inr rfl) hact)
  all_goals (try (simp only [reduceCtorEq, core.result.Result.Ok.injEq] at h'))
  all_goals (try subst h')
  all_goals (
    intro w hw
    simp [*] at hw
    (first | subst hw | (rcases hw with hw | hw <;> subst hw)) <;>
    (simp_all [writeAllowed, statusStep, Snapshot.toSt]
     try (have e := ‹_ = v1›; subst e; simp_all [writeAllowed, statusStep, Snapshot.toSt])))

/-- An effect is emitted only together with the write that publishes its
document. With `authorized`, only a writer publishing an approved document
triggers one. -/
theorem emit_publishes (a : Principal) (s : Snapshot) (c : Command) ws r
    (h : transition a s c = .ok (.Ok (ws, r))) :
    ∀ e, Write.Emit e ∈ ws.val → ∃ d, Write.PutDocument d ∈ ws.val ∧ d.id = e.doc ∧
      d.project = e.project ∧ d.status = .Published := by
  refine of_spec (P := fun ws _ => ∀ e, Write.Emit e ∈ ws.val → ∃ d, Write.PutDocument d ∈ ws.val ∧
    d.id = e.doc ∧ d.project = e.project ∧ d.status = .Published) ?_ h
  walk transition
  all_goals (simp only [OnOk]; try trivial)
  all_goals (intro e he; simp_all)
  obtain ⟨_, _, rfl⟩ := ‹∃ _, _ ∧ _›
  rfl

/-- Replies contain only documents the caller may read. -/
theorem reply_confined (a : Principal) (s : Snapshot) (c : Command) ws r
    (h : transition a s c = .ok (.Ok (ws, r))) :
    replyAllowed (Snapshot.toSt s) a.user.val r := by
  have hs : transition a s c ⦃ res => ∀ ws r, res = .Ok (ws, r) →
      replyAllowed (Snapshot.toSt s) a.user.val r ⦄ := by
    walk transition
    all_goals (intro ws' r' hr; cases hr; try simp only [replyAllowed])
    · -- GetDocument
      rename_i hr; rw [r_post] at hr
      obtain ⟨hf, hread, _⟩ := authDoc_ok hr
      exact ⟨(findDoc_some hf).1, hread⟩
    · -- ListDocuments
      intro d hd
      rw [v_post, List.mem_filter] at hd
      obtain ⟨hmem, hp⟩ := hd
      simp only [decide_eq_true_eq] at hp
      exact ⟨hmem, by rw [hp]; simp_all⟩
  obtain ⟨res, hres, hp⟩ := (WP.spec_equiv_exists _ _).1 hs
  rw [h, Result.ok.injEq] at hres
  exact hp _ _ hres.symm

end docs_kernel.Theorems
