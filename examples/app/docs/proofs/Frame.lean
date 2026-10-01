import Noninterference
/-!
# Frame theorem

A command's result depends only on the rows its `read_scope` names: the id
counter, plus every row of one project. So the server may load just those
rows (`DocsStore::load_for`) instead of the whole tenant.
-/
open Aeneas Aeneas.Std Result docs_kernel docs_kernel.Spec docs_kernel.TransitionLemmas
  docs_kernel.Theorems H5iAppLib

namespace docs_kernel.Frame

def filterV {α} (p : α → Bool) (v : alloc.vec.Vec α) : alloc.vec.Vec α :=
  .from (v.val.filter p) ((List.length_filter_le _ _).trans v.len_ineq)

@[simp] theorem filterV_val {α} (p : α → Bool) (v : alloc.vec.Vec α) :
    (filterV p v).val = v.val.filter p :=
  alloc.vec.Vec.from_val _ _

/-- The counter and every row of project `p`. -/
def keep (p : Nat) (s : Snapshot) : Snapshot where
  counter := s.counter
  projects := filterV (fun q => q.id.val = p) s.projects
  members := filterV (fun m => m.project.val = p) s.members
  documents := filterV (fun d => d.project.val = p) s.documents
  webhooks := filterV (fun w => w.project.val = p) s.webhooks

/-- Only the counter. -/
def counterOnly (s : Snapshot) : Snapshot where
  counter := s.counter
  projects := filterV (fun _ => false) s.projects
  members := filterV (fun _ => false) s.members
  documents := filterV (fun _ => false) s.documents
  webhooks := filterV (fun _ => false) s.webhooks

/-- The rows a scope covers, as the server loads them. -/
def slice (s : Snapshot) : Scope → Snapshot
  | .Counter => counterOnly s
  | .Project p => keep p.val s
  | .Document d =>
    match findDoc s.documents.val d.val with
    | some doc => keep doc.project.val s
    | none => counterOnly s

/-! ## Each read gives the same answer on `keep p s` for project `p` -/

section
variable (s : Snapshot) (p : U64)

theorem roleOf_keep (t : Nat) :
    roleOf (s.members.val.filter (fun m => m.project.val = p.val)) p.val t = roleOf s.members.val p.val t := by
  unfold roleOf
  rw [find?_filter_of_imp]
  intro m _ h
  simp only [decide_eq_true_eq] at h ⊢
  exact h.1

theorem allowed_keep (u : Nat) (a : Action) :
    allowed (Snapshot.toSt (keep p.val s)) u p.val a = allowed (Snapshot.toSt s) u p.val a := by
  simp [allowed, Snapshot.toSt, keep, roleOf_keep]

theorem can_keep (u : U64) (a : Action) : can (keep p.val s) u p a = can s u p a := by
  rw [can_ok, can_ok, allowed_keep]

theorem role_of_keep (t : U64) : role_of (keep p.val s).members p t = role_of s.members p t := by
  rw [role_of_ok, role_of_ok]
  simp [keep, roleOf_keep]

theorem count_owners_keep : count_owners (keep p.val s).members p = count_owners s.members p := by
  obtain ⟨n₁, e₁, h₁⟩ := (WP.spec_equiv_exists _ _).1 (Lemmas.count_owners_spec (keep p.val s).members p)
  obtain ⟨n₂, e₂, h₂⟩ := (WP.spec_equiv_exists _ _).1 (Lemmas.count_owners_spec s.members p)
  rw [e₁, e₂]
  congr 1
  apply UScalar.eq_of_val_eq
  rw [h₁, h₂]
  simp only [keep, filterV_val, owners, List.filter_filter]
  congr 1
  apply List.filter_congr
  intro m _
  by_cases hp : m.project.val = p.val <;> simp [hp]

theorem documents_in_keep : documents_in (keep p.val s).documents p = documents_in s.documents p := by
  obtain ⟨v₁, e₁, h₁⟩ := (WP.spec_equiv_exists _ _).1 (Lemmas.documents_in_spec (keep p.val s).documents p)
  obtain ⟨v₂, e₂, h₂⟩ := (WP.spec_equiv_exists _ _).1 (Lemmas.documents_in_spec s.documents p)
  rw [e₁, e₂]
  congr 1
  apply alloc.vec.Vec.ext
  rw [h₁, h₂]
  simp only [keep, filterV_val, List.filter_filter]
  apply List.filter_congr
  intro d _
  by_cases hp : d.project.val = p.val <;> simp [hp]

theorem webhook_of_keep : webhook_of (keep p.val s).webhooks p = webhook_of s.webhooks p := by
  obtain ⟨r₁, e₁, h₁⟩ := (WP.spec_equiv_exists _ _).1 (Lemmas.webhook_of_spec (keep p.val s).webhooks p)
  obtain ⟨r₂, e₂, h₂⟩ := (WP.spec_equiv_exists _ _).1 (Lemmas.webhook_of_spec s.webhooks p)
  rw [e₁, e₂]
  congr 1
  apply Option.map_injective (fun x y hxy => UScalar.eq_of_val_eq hxy)
  rw [h₁, h₂]
  simp only [keep, filterV_val, webhookOf]
  rw [find?_filter_of_imp]
  intro w _ h
  simp only [decide_eq_true_eq] at h ⊢
  exact h

end

theorem fresh_id_keep (s : Snapshot) (p : Nat) : fresh_id (keep p s) = fresh_id s := rfl

theorem fresh_id_counterOnly (s : Snapshot) : fresh_id (counterOnly s) = fresh_id s := rfl

/-- On the slice of the document's own project, authorization finds the same
document. Ids are unique, so no other project hides a duplicate. -/
theorem authDoc_keep (s : Snapshot) (u : Nat) (d : U64) (a : Action) (doc : Document)
    (hn : (s.documents.val.map (·.id)).Nodup) (hf : findDoc s.documents.val d.val = some doc) :
    authDoc (Snapshot.toSt (keep doc.project.val s)) u d.val a = authDoc (Snapshot.toSt s) u d.val a := by
  have hsame : findDoc (s.documents.val.filter (fun e => e.project.val = doc.project.val)) d.val = some doc := by
    unfold findDoc at hf ⊢
    rw [List.find?_filter, find?_and_of_unique (doc_unique hn d.val), hf]
    simp [Option.filter]
  have hr : ∀ a', allowed (Snapshot.toSt (keep doc.project.val s)) u doc.project.val a' =
      allowed (Snapshot.toSt s) u doc.project.val a' := allowed_keep s doc.project u
  simp only [authDoc]
  rw [show (Snapshot.toSt (keep doc.project.val s)).docs =
      s.documents.val.filter (fun e => e.project.val = doc.project.val) by simp [Snapshot.toSt, keep]]
  rw [hsame, show (Snapshot.toSt s).docs = s.documents.val from rfl, hf]
  simp only [hr]

theorem authDoc_counterOnly (s : Snapshot) (u : Nat) (d : U64) (a : Action)
    (hf : findDoc s.documents.val d.val = none) :
    authDoc (Snapshot.toSt (counterOnly s)) u d.val a = authDoc (Snapshot.toSt s) u d.val a := by
  have h0 : findDoc (Snapshot.toSt (counterOnly s)).docs d.val = none := by
    simp [Snapshot.toSt, counterOnly, findDoc]
  simp only [authDoc, h0, show (Snapshot.toSt s).docs = s.documents.val from rfl, hf]

theorem authDoc_none (st : St) (u id : Nat) (a : Action) (hf : findDoc st.docs id = none) :
    authDoc st u id a = .Err .NotFound := by
  simp [authDoc, hf]

-- Proof for a command that authorizes document `d`.
set_option hygiene false in
local macro "doc_case " d:ident : tactic => `(tactic| (
    cases hf : findDoc s.documents.val ($d).val with
    | none =>
      simp only [transition, authorized_doc_ok, authDoc_counterOnly _ _ _ _ hf,
        authDoc_none (Snapshot.toSt s) _ _ _ hf, bind_ok]
    | some doc =>
      simp only [transition, authorized_doc_ok, authDoc_keep s _ $d _ doc hn hf]
      try (
        cases ha : authDoc (Snapshot.toSt s) a.user.val ($d).val .Write with
        | Err e => simp only [bind_ok]
        | Ok v =>
          have hv : v = doc := by
            have := (authDoc_ok ha).1
            simp only [Snapshot.toSt] at this
            rw [hf] at this; exact (Option.some.inj this).symm
          subst hv
          simp only [bind_ok, with_status]
          split_ifs
          · simp only [bind_ok]
          · simp only [bind_ok, bind_assoc_eq, Std.bind_assoc]
            rw [webhook_of_keep s v.project])))

/-- The frame theorem: on the rows `read_scope` names, `transition` gives the
same result as on the whole tenant. -/
theorem transition_frame (a : Principal) (s : Snapshot) (c : Command)
    (hi : Inv (Snapshot.toSt s)) (sc : Scope) (hs : read_scope c = ok sc) :
    transition a (slice s sc) c = transition a s c := by
  have hn : (s.documents.val.map (·.id)).Nodup := hi.doc_keys
  cases c <;> simp only [read_scope, ok.injEq] at hs <;> subst hs <;> simp only [slice]
  case CreateProject n => simp only [transition, fresh_id_counterOnly]
  case SetMember p t r => simp only [transition, can_keep, role_of_keep, count_owners_keep]
  case RemoveMember p t => simp only [transition, can_keep, role_of_keep, count_owners_keep]
  case CreateDocument p ti bo => simp only [transition, can_keep, fresh_id_keep]
  case SetWebhook p w => simp only [transition, can_keep]
  case ListDocuments p => simp only [transition, can_keep, documents_in_keep]
  case EditDocument d bo v => doc_case d
  case Submit d => doc_case d
  case Approve d => doc_case d
  case Publish d => doc_case d
  case DeleteDocument d => doc_case d
  case GetDocument d => doc_case d
end docs_kernel.Frame
