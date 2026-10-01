import Theorems
import Invariants
import Frame
/-!
# Scenarios

The main theorems on reachable states, plus concrete runs of the extracted
kernel showing the hypotheses are satisfiable (`published_reachable`,
`view_hides`, `slice_drops`).
-/
open Aeneas Aeneas.Std Result docs_kernel docs_kernel.Spec docs_kernel.Theorems H5iAppLib

namespace docs_kernel.Scenarios

/-! ## On reachable states -/

theorem authorized_reachable (a : Principal) (s : Snapshot) (c : Command) ws r
    (hr : Reachable (Snapshot.toSt s)) (h : transition a s c = .ok (.Ok (ws, r))) :
    ∀ w ∈ ws.val, writeAllowed (Snapshot.toSt s) a.user.val w :=
  authorized a s c ws r (reachable_inv hr) h

theorem noninterference_reachable (a : Principal) (s₁ s₂ : Snapshot) (c : Command)
    (h₁ : Reachable (Snapshot.toSt s₁)) (h₂ : Reachable (Snapshot.toSt s₂))
    (hv : view (Snapshot.toSt s₁) a.user.val = view (Snapshot.toSt s₂) a.user.val) :
    transition a s₁ c = transition a s₂ c :=
  noninterference a s₁ s₂ c (reachable_inv h₁) (reachable_inv h₂) hv

theorem transition_frame_reachable (a : Principal) (s : Snapshot) (c : Command)
    (hr : Reachable (Snapshot.toSt s)) (sc : Scope) (hs : read_scope c = ok sc) :
    transition a (Frame.slice s sc) c = transition a s c :=
  Frame.transition_frame a s c (reachable_inv hr) sc hs

/-! ## A run -/

theorem push_eq {α} (v : alloc.vec.Vec α) (x : α) (h : v.val.length < 3) :
    v.push x = ok (alloc.vec.Vec.from (v.val ++ [x]) (by simp; scalar_tac)) := by
  obtain ⟨v1, e, hv⟩ := (WP.spec_equiv_exists _ _).1 (alloc.vec.Vec.push_spec v x (by scalar_tac))
  rw [e]; congr 1; apply alloc.vec.Vec.ext; simp [hv]

theorem new_val (α) : (alloc.vec.Vec.new α).val = [] := rfl

theorem webhook_of_eq (hs : alloc.vec.Vec Webhook) (p : U64) :
    webhook_of hs p = ok ((hs.val.find? (fun w => w.project.val = p.val)).map (·.dest)) := by
  obtain ⟨r, e, h⟩ := (WP.spec_equiv_exists _ _).1 (Lemmas.webhook_of_spec hs p)
  rw [e]; congr 1
  apply Option.map_injective (fun x y hxy => UScalar.eq_of_val_eq hxy)
  rw [h]; simp [webhookOf, Option.map_map, Function.comp_def]

theorem add01 : (0#u64 + 1#u64 : Result U64) = ok 1#u64 := by with_unfolding_all rfl
theorem add11 : (1#u64 + 1#u64 : Result U64) = ok 2#u64 := by with_unfolding_all rfl
theorem add21 : (2#u64 + 1#u64 : Result U64) = ok 3#u64 := by with_unfolding_all rfl
theorem add31 : (3#u64 + 1#u64 : Result U64) = ok 4#u64 := by with_unfolding_all rfl

/-- One committed step extends reachability. -/
theorem reach {s t : Snapshot} {a c ws r} (hr : Reachable (Snapshot.toSt s))
    (ht : transition a s c = ok (.Ok (ws, r))) (he : applyAll (Snapshot.toSt s) ws.val = Snapshot.toSt t) :
    Reachable (Snapshot.toSt t) := he ▸ Reachable.step hr ht

/-! Users 1 (owner) and 2 (editor) in project 0; document 1 by user 2. -/

def owner : Principal := ⟨0#u64, 1#u64⟩
def editor : Principal := ⟨0#u64, 2#u64⟩
def outsider : Principal := ⟨0#u64, 3#u64⟩

def p0 : Project := ⟨0#u64, vecOf []⟩
def m1 : Member := ⟨0#u64, 1#u64, .Owner⟩
def m2 : Member := ⟨0#u64, 2#u64, .Editor⟩
def hook : Webhook := ⟨0#u64, 7#u64⟩
def doc (st : Status) (ap : Option U64) (v : U64) : Document := ⟨1#u64, 0#u64, 2#u64, vecOf [], vecOf [], st, ap, v⟩

def s0 : Snapshot := ⟨⟨0#u64⟩, vecOf [], vecOf [], vecOf [], vecOf []⟩
def s1 : Snapshot := ⟨⟨1#u64⟩, vecOf [p0], vecOf [m1], vecOf [], vecOf []⟩
def s2 : Snapshot := ⟨⟨1#u64⟩, vecOf [p0], vecOf [m1, m2], vecOf [], vecOf []⟩
def s3 : Snapshot := ⟨⟨1#u64⟩, vecOf [p0], vecOf [m1, m2], vecOf [], vecOf [hook]⟩
def s4 : Snapshot := ⟨⟨2#u64⟩, vecOf [p0], vecOf [m1, m2], vecOf [doc .Draft none 1#u64], vecOf [hook]⟩
def s5 : Snapshot := ⟨⟨2#u64⟩, vecOf [p0], vecOf [m1, m2], vecOf [doc .InReview none 2#u64], vecOf [hook]⟩
def s6 : Snapshot := ⟨⟨2#u64⟩, vecOf [p0], vecOf [m1, m2], vecOf [doc .Approved (some 1#u64) 3#u64], vecOf [hook]⟩
def s7 : Snapshot := ⟨⟨2#u64⟩, vecOf [p0], vecOf [m1, m2], vecOf [doc .Published (some 1#u64) 4#u64], vecOf [hook]⟩

-- Evaluates a concrete `transition`: loops through their list specs, the rest by `simp`.
macro "eval_kernel" : tactic => `(tactic| (
  simp (disch := simp [new_val, vecOf]) only [transition, can_ok, role_of_ok, authorized_doc_ok,
    webhook_of_eq, fresh_id, with_status, one, push_eq, u8vec_clone]
  simp [s0, s1, s2, s3, s4, s5, s6, s7, owner, editor, outsider, p0, m1, m2, hook, doc, vecOf, allowed, roleOf, policy,
    Snapshot.toSt, TransitionLemmas.authDoc, findDoc, U64.rMax, add01, add11, add21, add31,
    core.cmp.PartialEq.ne.trait_default, core.cmp.PartialEq.ne.default,
    Role.Insts.CoreCmpPartialEqRole.eq, Role.read_discriminant,
    Status.Insts.CoreCmpPartialEqStatus.eq, Status.read_discriminant]
  try (simp (disch := simp [new_val, vecOf]) only [push_eq, bind_ok, new_val])
  try rfl))

theorem t1 : transition owner s0 (.CreateProject (vecOf [])) = ok (.Ok (vecOf
    [.SetCounter ⟨1#u64⟩, .PutProject p0, .PutMember m1], .Created 0#u64)) := by eval_kernel

theorem t2 : transition owner s1 (.SetMember 0#u64 2#u64 .Editor) =
    ok (.Ok (vecOf [.PutMember m2], .Done)) := by eval_kernel

theorem t3 : transition owner s2 (.SetWebhook 0#u64 (some 7#u64)) =
    ok (.Ok (vecOf [.PutWebhook hook], .Done)) := by eval_kernel

theorem t4 : transition editor s3 (.CreateDocument 0#u64 (vecOf []) (vecOf [])) =
    ok (.Ok (vecOf [.SetCounter ⟨2#u64⟩, .PutDocument (doc .Draft none 1#u64)], .Created 1#u64)) := by
  eval_kernel

theorem t5 : transition editor s4 (.Submit 1#u64) =
    ok (.Ok (vecOf [.PutDocument (doc .InReview none 2#u64)], .Version 2#u64)) := by eval_kernel

theorem t6 : transition owner s5 (.Approve 1#u64) =
    ok (.Ok (vecOf [.PutDocument (doc .Approved (some 1#u64) 3#u64)], .Version 3#u64)) := by eval_kernel

/-- Publishing emits one effect to the registered destination. -/
theorem t7 : transition editor s6 (.Publish 1#u64) =
    ok (.Ok (vecOf [.PutDocument (doc .Published (some 1#u64) 4#u64), .Emit ⟨7#u64, 0#u64, 1#u64, 4#u64⟩],
      .Version 4#u64)) := by eval_kernel

/-- A document published under the four-eyes rule is reachable. -/
theorem published_reachable : Reachable (Snapshot.toSt s7) := by
  refine reach (reach (reach (reach (reach (reach (reach (s := s0) ?_ t1 ?_) t2 ?_) t3 ?_) t4 ?_) t5 ?_) t6 ?_) t7 ?_
  · exact Reachable.init
  all_goals rfl

theorem published_four_eyes : ∃ d ∈ (Snapshot.toSt s7).docs,
    d.status = .Published ∧ ∃ a, d.approver = some a ∧ a ≠ d.author :=
  ⟨doc .Published (some 1#u64) 4#u64, by simp [s7, Snapshot.toSt, vecOf], rfl, 1#u64, rfl, by decide⟩

/-- The author's editor role cannot approve. -/
theorem editor_cannot_approve : transition editor s5 (.Approve 1#u64) = ok (.Err .Forbidden) := by
  eval_kernel

/-- A hidden document and a missing one give the same error. -/
theorem hidden_not_found : transition outsider s7 (.GetDocument 1#u64) = ok (.Err .NotFound) ∧
    transition outsider s7 (.GetDocument 9#u64) = ok (.Err .NotFound) := by
  constructor <;> eval_kernel

/-- Only the counter, which the view declares. -/
def e2 : Snapshot := ⟨⟨2#u64⟩, vecOf [], vecOf [], vecOf [], vecOf []⟩

/-- The view hides rows: `s7` and `e2` differ but look the same to an outsider,
so `noninterference` gives the outsider the same result on both. -/
theorem view_hides : Snapshot.toSt s7 ≠ Snapshot.toSt e2 ∧
    view (Snapshot.toSt s7) outsider.user.val = view (Snapshot.toSt e2) outsider.user.val ∧
    ∀ c, transition outsider s7 c = transition outsider e2 c := by
  have hv : view (Snapshot.toSt s7) outsider.user.val = view (Snapshot.toSt e2) outsider.user.val := by
    simp [view, Snapshot.toSt, s7, e2, vecOf, roleOf, m1, m2, doc, hook, outsider]
  refine ⟨?_, hv, fun c => noninterference _ _ _ c (reachable_inv published_reachable) ?_ hv⟩
  · intro h; have := congrArg (·.docs) h; simp [Snapshot.toSt, s7, e2, vecOf] at this
  · constructor <;> simp [Snapshot.toSt, e2, vecOf]

/-- A project slice drops the other projects' rows. -/
theorem slice_drops : (Frame.slice s7 (.Project 5#u64)).documents.val = [] ∧ s7.documents.val ≠ [] := by
  simp [Frame.slice, Frame.keep, s7, vecOf, doc]

end docs_kernel.Scenarios
