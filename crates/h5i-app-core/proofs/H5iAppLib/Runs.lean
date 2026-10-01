import Aeneas
/-!
# Runs of requests

The engine runs each request's `transition` in one SERIALIZABLE transaction,
so whatever the database commits for concurrent requests equals some serial
order of them (trusted assumption A5). A `Run` is such an order: every
request with its outcome, successful or refused, and the state after them.
A theorem about every `Run` holds for every interleaving of every client's
requests, which is how multi-request properties are stated: invariants over
state evolution, "once X, never Y again", check-then-use across requests.

Apps instantiate `Run` with their `transition`, the map `toSt` from the
kernel's snapshot to the spec state, and `applyAll`, which each app proves the
extracted `apply` computes.
-/
open Aeneas Aeneas.Std Result

namespace H5iAppLib

/-- One request and its outcome. -/
inductive Event (P C R E : Type) where
  | ok (a : P) (c : C) (r : R)
  | err (a : P) (c : C) (e : E)

variable {P S C WS R E St : Type}

/-- Serial runs from `init`: the state after the events, and the events. -/
inductive Run (transition : P → S → C → Result (core.result.Result (WS × R) E))
    (toSt : S → St) (applyAll : St → WS → St) (init : St) : St → List (Event P C R E) → Prop
  | nil : Run transition toSt applyAll init init []
  | ok {st evs s a c ws r} : Run transition toSt applyAll init st evs → toSt s = st →
      transition a s c = Result.ok (.Ok (ws, r)) →
      Run transition toSt applyAll init (applyAll st ws) (evs ++ [.ok a c r])
  | err {st evs s a c e} : Run transition toSt applyAll init st evs → toSt s = st →
      transition a s c = Result.ok (.Err e) →
      Run transition toSt applyAll init st (evs ++ [.err a c e])

/-- What `transition` did on snapshot `s` to produce event `ev`. -/
def Fired (transition : P → S → C → Result (core.result.Result (WS × R) E)) (s : S) :
    Event P C R E → Prop
  | .ok a c r => ∃ ws, transition a s c = Result.ok (.Ok (ws, r))
  | .err a c e => transition a s c = Result.ok (.Err e)

variable {transition : P → S → C → Result (core.result.Result (WS × R) E)}
  {toSt : S → St} {applyAll : St → WS → St} {init : St}

/-- An invariant kept by every successful request holds after every run. -/
theorem Run.inv (I : St → Prop) (h0 : I init)
    (hs : ∀ a s c ws r, I (toSt s) → transition a s c = Result.ok (.Ok (ws, r)) → I (applyAll (toSt s) ws))
    {st evs} (h : Run transition toSt applyAll init st evs) : I st := by
  induction h with
  | nil => exact h0
  | ok _ hst ht ih => subst hst; exact hs _ _ _ _ _ ih ht
  | err _ _ _ ih => exact ih

/-- Every event of a run fired on a snapshot of a state some prefix of the run
reached. -/
theorem Run.fired {st evs} (h : Run transition toSt applyAll init st evs) :
    ∀ ev ∈ evs, ∃ s evs', Run transition toSt applyAll init (toSt s) evs' ∧ Fired transition s ev := by
  induction h with
  | nil => simp
  | @ok st evs s a c ws r hr hst ht ih =>
    intro ev hev
    rcases List.mem_append.1 hev with h | h
    · exact ih ev h
    · rw [List.mem_singleton] at h; subst h hst; exact ⟨s, evs, hr, ws, ht⟩
  | @err st evs s a c e hr hst ht ih =>
    intro ev hev
    rcases List.mem_append.1 hev with h | h
    · exact ih ev h
    · rw [List.mem_singleton] at h; subst h hst; exact ⟨s, evs, hr, ht⟩

/-- "Once `e`, then `Q` from then on": if `e` establishes `Q` and every
successful request keeps it, every later event fires on a state with `Q`,
reached by a prefix of the run (so `Run.inv` applies there too). -/
theorem Run.after (Q : St → Prop)
    (hkeep : ∀ a s c ws r, Q (toSt s) → transition a s c = Result.ok (.Ok (ws, r)) → Q (applyAll (toSt s) ws))
    {a₀ : P} {c₀ : C} {r₀ : R}
    (hest : ∀ s ws, transition a₀ s c₀ = Result.ok (.Ok (ws, r₀)) → Q (applyAll (toSt s) ws))
    {st evs} (h : Run transition toSt applyAll init st evs) :
    ∀ pre post, evs = pre ++ .ok a₀ c₀ r₀ :: post →
      Q st ∧ ∀ ev ∈ post, ∃ s evs', Run transition toSt applyAll init (toSt s) evs' ∧ Q (toSt s) ∧
        Fired transition s ev := by
  induction h with
  | nil => intro pre post he; simp at he
  | @ok st evs s a c ws r hr hst ht ih =>
    intro pre post he
    rcases List.eq_nil_or_concat post with rfl | ⟨post', ev, rfl⟩
    all_goals try simp only [List.concat_eq_append] at he ⊢
    · rw [List.append_cons pre, List.append_nil] at he
      obtain ⟨-, he⟩ := List.append_inj' he rfl
      simp only [List.cons.injEq, Event.ok.injEq, and_true] at he
      obtain ⟨rfl, rfl, rfl⟩ := he
      subst hst
      exact ⟨hest s ws ht, by simp⟩
    · rw [← List.cons_append, ← List.append_assoc] at he
      obtain ⟨he, hev⟩ := List.append_inj' he rfl
      simp only [List.cons.injEq, and_true] at hev
      obtain ⟨hq, hpost⟩ := ih pre post' he
      subst hst hev
      refine ⟨hkeep _ _ _ _ _ hq ht, fun ev' hev' => ?_⟩
      rcases List.mem_append.1 hev' with h' | h'
      · exact hpost ev' h'
      · rw [List.mem_singleton] at h'; subst h'; exact ⟨s, evs, hr, hq, ws, ht⟩
  | @err st evs s a c e hr hst ht ih =>
    intro pre post he
    rcases List.eq_nil_or_concat post with rfl | ⟨post', ev, rfl⟩
    all_goals try simp only [List.concat_eq_append] at he ⊢
    · rw [List.append_cons pre, List.append_nil] at he
      obtain ⟨-, he⟩ := List.append_inj' he rfl
      simp at he
    · rw [← List.cons_append, ← List.append_assoc] at he
      obtain ⟨he, hev⟩ := List.append_inj' he rfl
      simp only [List.cons.injEq, and_true] at hev
      obtain ⟨hq, hpost⟩ := ih pre post' he
      subst hst hev
      refine ⟨hq, fun ev' hev' => ?_⟩
      rcases List.mem_append.1 hev' with h' | h'
      · exact hpost ev' h'
      · rw [List.mem_singleton] at h'; subst h'; exact ⟨s, evs, hr, hq, ht⟩

end H5iAppLib
