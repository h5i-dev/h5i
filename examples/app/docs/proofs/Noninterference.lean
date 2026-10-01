import Transition
import Apply
open Aeneas Aeneas.Std Result docs_kernel docs_kernel.Spec docs_kernel.TransitionLemmas H5iAppLib

namespace docs_kernel.Theorems

/-! ## List facts: what the kernel reads for user `u` is fixed by `view` -/

/-- A role lookup in the user's view agrees with the full lookup, for the
user's own projects and for the user's own membership. -/
theorem roleOf_view (ms : List Member) (u p t : Nat)
    (h : (roleOf ms p u).isSome = true ∨ t = u) :
    roleOf (ms.filter (fun m => (roleOf ms m.project.val u).isSome)) p t = roleOf ms p t := by
  unfold roleOf
  rw [find?_filter_of_imp]
  intro m hm hpm
  simp only [decide_eq_true_eq] at hpm
  rw [hpm.1]
  rcases h with h | rfl
  · exact h
  · simp only [Option.isSome_map, List.find?_isSome]
    exact ⟨m, hm, by simp [hpm.1, hpm.2]⟩

theorem owners_view (ms : List Member) (u p : Nat) (h : (roleOf ms p u).isSome = true) :
    owners (ms.filter (fun m => (roleOf ms m.project.val u).isSome)) p = owners ms p := by
  unfold owners
  rw [List.filter_filter]
  congr 1
  apply List.filter_congr
  intro m _
  by_cases hp : m.project.val = p <;> simp [hp, h]

theorem docs_view (ms : List Member) (ds : List Document) (u p : Nat)
    (h : (roleOf ms p u).isSome = true) :
    (ds.filter (fun d => (roleOf ms d.project.val u).isSome)).filter (fun d => d.project.val = p) =
      ds.filter (fun d => d.project.val = p) := by
  rw [List.filter_filter]
  apply List.filter_congr
  intro d _
  by_cases hp : d.project.val = p <;> simp [hp, h]

theorem webhookOf_view (ms : List Member) (hs : List Webhook) (u p : Nat)
    (h : (roleOf ms p u).isSome = true) :
    webhookOf (hs.filter (fun w => (roleOf ms w.project.val u).isSome)) p = webhookOf hs p := by
  unfold webhookOf
  rw [find?_filter_of_imp]
  intro w _ hw
  simp only [decide_eq_true_eq] at hw
  rw [hw]; exact h

theorem allowed_read (s : St) (u p : Nat) :
    allowed s u p .Read = (roleOf s.members p u).isSome := by
  unfold allowed
  cases roleOf s.members p u with
  | none => rfl
  | some r => cases r <;> rfl

theorem isSome_of_allowed {s : St} {u p : Nat} {a : Action} (h : allowed s u p a = true) :
    (roleOf s.members p u).isSome = true := by
  unfold allowed at h
  split at h <;> rename_i hr <;> simp_all

/-- `find?` of a conjunction, when at most one element matches the key. -/
theorem find?_and_of_unique {α} {l : List α} {r p : α → Bool}
    (hu : ∀ x ∈ l, ∀ y ∈ l, p x = true → p y = true → x = y) :
    l.find? (fun a => decide (r a = true ∧ p a = true)) = (l.find? p).filter r := by
  cases hf : l.find? p with
  | none =>
    rw [show Option.filter r none = none from rfl, List.find?_eq_none]
    rw [List.find?_eq_none] at hf
    intro x hx
    simp [hf x hx]
  | some d =>
    have hd := List.mem_of_find?_eq_some hf
    have hpd := List.find?_some hf
    by_cases hr : r d = true
    · rw [show Option.filter r (some d) = some d by simp [Option.filter, hr]]
      cases hg : l.find? (fun a => decide (r a = true ∧ p a = true)) with
      | none =>
        rw [List.find?_eq_none] at hg
        exact absurd (by simp [hr, hpd]) (hg d hd)
      | some x =>
        have hx := List.mem_of_find?_eq_some hg
        have hpx := List.find?_some hg
        simp only [decide_eq_true_eq] at hpx
        rw [hu x hx d hd hpx.2 hpd]
    · rw [show Option.filter r (some d) = none by simp [Option.filter, hr], List.find?_eq_none]
      intro x hx hrx
      simp only [decide_eq_true_eq] at hrx
      rw [hu x hx d hd hrx.2 hpd] at hrx
      exact hr hrx.1

theorem doc_unique {ds : List Document} (hn : (ds.map (·.id)).Nodup) (id : Nat) :
    ∀ x ∈ ds, ∀ y ∈ ds, decide (x.id.val = id) = true → decide (y.id.val = id) = true → x = y := by
  intro x hx y hy hpx hpy
  simp only [decide_eq_true_eq] at hpx hpy
  exact List.inj_on_of_nodup_map hn hx hy (UScalar.eq_of_val_eq (hpx.trans hpy.symm))

/-- `authDoc` only looks at documents the caller may read. -/
theorem authDoc_view (s : St) (u id : Nat) (a : Action) (hn : (s.docs.map (·.id)).Nodup) :
    authDoc s u id a =
      match findDoc (s.docs.filter (fun d => allowed s u d.project.val .Read)) id with
      | none => .Err .NotFound
      | some d => if allowed s u d.project.val a then .Ok d else .Err .Forbidden := by
  unfold authDoc findDoc
  rw [List.find?_filter, find?_and_of_unique (doc_unique hn id)]
  cases s.docs.find? (fun d => decide (d.id.val = id)) with
  | none => rfl
  | some d => by_cases hr : allowed s u d.project.val .Read = true <;> simp [Option.filter, hr]

/-! ## From specs to equations -/

theorem can_ok (s : Snapshot) (u p : U64) (a : Action) :
    can s u p a = ok (allowed (Snapshot.toSt s) u.val p.val a) :=
  eq_ok_of_spec (Lemmas.can_spec s u p a)

theorem role_of_ok (ms : alloc.vec.Vec Member) (p u : U64) :
    role_of ms p u = ok (roleOf ms.val p.val u.val) :=
  eq_ok_of_spec (Lemmas.role_of_spec ms p u)

theorem authorized_doc_ok (s : Snapshot) (u id : U64) (a : Action) :
    authorized_doc s u id a = ok (authDoc (Snapshot.toSt s) u.val id.val a) :=
  eq_ok_of_spec (authorized_doc_spec s u id a)

/-! ## Two states with the same view -/

section
variable (a : Principal) (s₁ s₂ : Snapshot)
  (h₁ : Inv (Snapshot.toSt s₁)) (h₂ : Inv (Snapshot.toSt s₂))
  (hv : view (Snapshot.toSt s₁) a.user.val = view (Snapshot.toSt s₂) a.user.val)
include hv

theorem view_parts :
    s₁.counter.next_id.val = s₂.counter.next_id.val ∧
    s₁.members.val.filter (fun m => (roleOf s₁.members.val m.project.val a.user.val).isSome) =
      s₂.members.val.filter (fun m => (roleOf s₂.members.val m.project.val a.user.val).isSome) ∧
    s₁.documents.val.filter (fun d => (roleOf s₁.members.val d.project.val a.user.val).isSome) =
      s₂.documents.val.filter (fun d => (roleOf s₂.members.val d.project.val a.user.val).isSome) ∧
    s₁.webhooks.val.filter (fun w => (roleOf s₁.members.val w.project.val a.user.val).isSome) =
      s₂.webhooks.val.filter (fun w => (roleOf s₂.members.val w.project.val a.user.val).isSome) := by
  simpa [view, Snapshot.toSt] using hv

theorem role_same (p : Nat) :
    roleOf s₁.members.val p a.user.val = roleOf s₂.members.val p a.user.val := by
  have hm := (view_parts a s₁ s₂ hv).2.1
  rw [← roleOf_view _ _ p _ (Or.inr rfl), hm, roleOf_view _ _ p _ (Or.inr rfl)]

theorem allowed_same (p : Nat) (act : Action) :
    allowed (Snapshot.toSt s₁) a.user.val p act = allowed (Snapshot.toSt s₂) a.user.val p act := by
  simp only [allowed, Snapshot.toSt, role_same a s₁ s₂ hv p]

theorem role_same_of (p t : Nat) (h : (roleOf s₂.members.val p a.user.val).isSome = true) :
    roleOf s₁.members.val p t = roleOf s₂.members.val p t := by
  have hm := (view_parts a s₁ s₂ hv).2.1
  have h' : (roleOf s₁.members.val p a.user.val).isSome = true := by
    rw [role_same a s₁ s₂ hv]; exact h
  rw [← roleOf_view _ _ p _ (Or.inl h'), hm, roleOf_view _ _ p _ (Or.inl h)]

theorem owners_same (p : Nat) (h : (roleOf s₂.members.val p a.user.val).isSome = true) :
    owners s₁.members.val p = owners s₂.members.val p := by
  have hm := (view_parts a s₁ s₂ hv).2.1
  have h' : (roleOf s₁.members.val p a.user.val).isSome = true := by
    rw [role_same a s₁ s₂ hv]; exact h
  rw [← owners_view _ _ p h', hm, owners_view _ _ p h]

theorem mine_same :
    (fun d : Document => (roleOf s₁.members.val d.project.val a.user.val).isSome) =
      (fun d : Document => (roleOf s₂.members.val d.project.val a.user.val).isSome) := by
  funext d
  rw [role_same a s₁ s₂ hv]

theorem readable_same :
    (Snapshot.toSt s₁).docs.filter (fun d => allowed (Snapshot.toSt s₁) a.user.val d.project.val .Read) =
      (Snapshot.toSt s₂).docs.filter (fun d => allowed (Snapshot.toSt s₂) a.user.val d.project.val .Read) := by
  simp only [allowed_read]
  exact (view_parts a s₁ s₂ hv).2.2.1

theorem docs_same (p : Nat) (h : (roleOf s₂.members.val p a.user.val).isSome = true) :
    s₁.documents.val.filter (fun d => d.project.val = p) =
      s₂.documents.val.filter (fun d => d.project.val = p) := by
  have hd := (view_parts a s₁ s₂ hv).2.2.1
  have h' : (roleOf s₁.members.val p a.user.val).isSome = true := by
    rw [role_same a s₁ s₂ hv]; exact h
  rw [← docs_view _ _ _ p h', hd, docs_view _ _ _ p h]

theorem webhook_of_same (p : U64) (h : (roleOf s₂.members.val p.val a.user.val).isSome = true) :
    webhook_of s₁.webhooks p = webhook_of s₂.webhooks p := by
  have hw := (view_parts a s₁ s₂ hv).2.2.2
  have h' : (roleOf s₁.members.val p.val a.user.val).isSome = true := by
    rw [role_same a s₁ s₂ hv]; exact h
  have hsame : webhookOf s₁.webhooks.val p.val = webhookOf s₂.webhooks.val p.val := by
    rw [← webhookOf_view _ _ _ _ h', hw, webhookOf_view _ _ _ _ h]
  obtain ⟨r₁, e₁, hr₁⟩ := (WP.spec_equiv_exists _ _).1 (Lemmas.webhook_of_spec s₁.webhooks p)
  obtain ⟨r₂, e₂, hr₂⟩ := (WP.spec_equiv_exists _ _).1 (Lemmas.webhook_of_spec s₂.webhooks p)
  rw [e₁, e₂]
  congr 1
  exact Option.map_injective (fun x y hxy => UScalar.eq_of_val_eq hxy) (hr₁.trans (hsame.trans hr₂.symm))

include h₁ h₂ in
theorem authorized_doc_same (id : U64) (act : Action) :
    authorized_doc s₁ a.user id act = authorized_doc s₂ a.user id act := by
  rw [authorized_doc_ok, authorized_doc_ok, authDoc_view _ _ _ _ h₁.doc_keys,
    authDoc_view _ _ _ _ h₂.doc_keys]
  rw [readable_same a s₁ s₂ hv]
  simp only [allowed_same a s₁ s₂ hv]

theorem fresh_id_same : fresh_id s₁ = fresh_id s₂ := by
  have hc : s₁.counter.next_id = s₂.counter.next_id :=
    UScalar.eq_of_val_eq (view_parts a s₁ s₂ hv).1
  unfold fresh_id
  rw [hc]

theorem count_owners_same (p : U64) (h : (roleOf s₂.members.val p.val a.user.val).isSome = true) :
    count_owners s₁.members p = count_owners s₂.members p := by
  obtain ⟨n₁, e₁, hn₁⟩ := (WP.spec_equiv_exists _ _).1 (Lemmas.count_owners_spec s₁.members p)
  obtain ⟨n₂, e₂, hn₂⟩ := (WP.spec_equiv_exists _ _).1 (Lemmas.count_owners_spec s₂.members p)
  rw [e₁, e₂, UScalar.eq_of_val_eq (hn₁.trans ((owners_same a s₁ s₂ hv _ h).trans hn₂.symm))]

theorem documents_in_same (p : U64) (h : (roleOf s₂.members.val p.val a.user.val).isSome = true) :
    documents_in s₁.documents p = documents_in s₂.documents p := by
  obtain ⟨v₁, e₁, hv₁⟩ := (WP.spec_equiv_exists _ _).1 (Lemmas.documents_in_spec s₁.documents p)
  obtain ⟨v₂, e₂, hv₂⟩ := (WP.spec_equiv_exists _ _).1 (Lemmas.documents_in_spec s₂.documents p)
  rw [e₁, e₂, alloc.vec.Vec.ext v₁ v₂ (hv₁.trans ((docs_same a s₁ s₂ hv _ h).trans hv₂.symm))]

end

/-- A user's result depends only on what that user may see. In particular,
error codes do not reveal hidden documents. -/
theorem noninterference (a : Principal) (s₁ s₂ : Snapshot) (c : Command)
    (h₁ : Inv (Snapshot.toSt s₁)) (h₂ : Inv (Snapshot.toSt s₂))
    (hv : view (Snapshot.toSt s₁) a.user.val = view (Snapshot.toSt s₂) a.user.val) :
    transition a s₁ c = transition a s₂ c := by
  have hdoc := authorized_doc_same a s₁ s₂ h₁ h₂ hv
  cases c with
  | CreateProject n =>
    simp only [transition, fresh_id_same a s₁ s₂ hv]
  | SetMember p t r =>
    simp only [transition, can_ok, allowed_same a s₁ s₂ hv]
    by_cases hc : allowed (Snapshot.toSt s₂) a.user.val p.val .Manage = true
    · have hs := isSome_of_allowed hc
      simp only [hc, role_of_ok, role_same_of a s₁ s₂ hv _ _ hs,
        count_owners_same a s₁ s₂ hv _ hs]
    · simp [hc]
  | RemoveMember p t =>
    simp only [transition, can_ok, allowed_same a s₁ s₂ hv]
    by_cases hc : allowed (Snapshot.toSt s₂) a.user.val p.val .Manage = true
    · have hs := isSome_of_allowed hc
      simp only [hc, role_of_ok, role_same_of a s₁ s₂ hv _ _ hs,
        count_owners_same a s₁ s₂ hv _ hs]
    · simp [hc]
  | CreateDocument p ti bo =>
    simp only [transition, can_ok, allowed_same a s₁ s₂ hv,
      fresh_id_same a s₁ s₂ hv]
  | EditDocument d bo v => simp only [transition, hdoc]
  | Submit d => simp only [transition, hdoc]
  | Approve d => simp only [transition, hdoc]
  | Publish d =>
    simp only [transition]
    rw [hdoc d .Write, authorized_doc_ok]
    cases hd : authDoc (Snapshot.toSt s₂) a.user.val d.val .Write with
    | Err e => simp only [bind_ok]
    | Ok v =>
      -- The writer can read the project, so its webhook is in the user's view.
      have hs := isSome_of_allowed (authDoc_ok hd).2.2
      simp only [bind_ok, with_status]
      split_ifs
      · simp only [bind_ok]
      · simp only [bind_ok, bind_assoc_eq, Std.bind_assoc]
        rw [webhook_of_same a s₁ s₂ hv v.project hs]
  | DeleteDocument d => simp only [transition, hdoc]
  | GetDocument d => simp only [transition, hdoc]
  | SetWebhook p w =>
    simp only [transition, can_ok, allowed_same a s₁ s₂ hv]
  | ListDocuments p =>
    simp only [transition, can_ok, allowed_same a s₁ s₂ hv]
    by_cases hc : allowed (Snapshot.toSt s₂) a.user.val p.val .Read = true
    · simp only [hc, documents_in_same a s₁ s₂ hv _ (isSome_of_allowed hc)]
    · simp [hc]

end docs_kernel.Theorems
