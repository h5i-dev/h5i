import Aeneas
/-!
# Runs with monotonic time

Trusted: with `EngineConfig::monotonic`, the engine never commits a tenant's
command at an earlier time than its previous commit. `ReachableT` pairs each
reachable state with its latest commit time; `StepsT` is the same from a given
state, over steps whose principal passes `allow`. Apps instantiate both in `Spec.lean`.
-/
open Aeneas Aeneas.Std Result

namespace H5iAppLib

variable {P S C WS R E St : Type}

/-- States reachable from `init` when commit times never decrease, with the
time of the latest commit (0 before the first). -/
inductive ReachableT (transition : P → S → C → Result (core.result.Result (WS × R) E)) (time : P → Nat)
    (toSt : S → St) (applyAll : St → WS → St) (init : St) : St → Nat → Prop
  | init : ReachableT transition time toSt applyAll init init 0
  | step {s t a c ws r} : ReachableT transition time toSt applyAll init (toSt s) t → t ≤ time a →
      transition a s c = ok (.Ok (ws, r)) → ReachableT transition time toSt applyAll init (applyAll (toSt s) ws) (time a)

/-- Runs from `(st, t)` of steps by principals that pass `ok` in the state
before the step, never going back in time. -/
inductive StepsT (transition : P → S → C → Result (core.result.Result (WS × R) E)) (time : P → Nat)
    (toSt : S → St) (applyAll : St → WS → St) (allow : P → St → Prop) (st : St) (t : Nat) : St → Nat → Prop
  | refl : StepsT transition time toSt applyAll allow st t st t
  | step {s t' a c ws r} : StepsT transition time toSt applyAll allow st t (toSt s) t' → t' ≤ time a →
      allow a (toSt s) → transition a s c = ok (.Ok (ws, r)) →
      StepsT transition time toSt applyAll allow st t (applyAll (toSt s) ws) (time a)

variable {transition : P → S → C → Result (core.result.Result (WS × R) E)} {time : P → Nat}
  {toSt : S → St} {applyAll : St → WS → St} {init : St} {allow : P → St → Prop}

/-- Invariants proven for untimed `Reachable` hold on timed runs. -/
theorem ReachableT.forget (Reach : St → Prop) (h0 : Reach init)
    (hs : ∀ a s c ws r, Reach (toSt s) → transition a s c = ok (.Ok (ws, r)) → Reach (applyAll (toSt s) ws))
    {st t} (h : ReachableT transition time toSt applyAll init st t) : Reach st := by
  induction h with
  | init => exact h0
  | step _ _ ht ih => exact hs _ _ _ _ _ ih ht

/-- A timed run from a reachable state ends in a reachable state. -/
theorem ReachableT.steps {st t st' t'} (hr : ReachableT transition time toSt applyAll init st t)
    (h : StepsT transition time toSt applyAll allow st t st' t') : ReachableT transition time toSt applyAll init st' t' := by
  induction h with
  | refl => exact hr
  | step _ hle _ ht ih => exact .step ih hle ht

/-- Time never goes back along a run. -/
theorem StepsT.time_le {st t st' t'} (h : StepsT transition time toSt applyAll allow st t st' t') : t ≤ t' := by
  induction h with
  | refl => exact Nat.le_refl _
  | step _ hle _ _ ih => exact Nat.le_trans ih hle

/-- Induction over a timed run; used for "once ..., never again" theorems. -/
theorem StepsT.preserve (Q : St → Nat → Prop)
    (hstep : ∀ a s c ws r t, ReachableT transition time toSt applyAll init (toSt s) t → Q (toSt s) t →
      t ≤ time a → allow a (toSt s) → transition a s c = ok (.Ok (ws, r)) → Q (applyAll (toSt s) ws) (time a))
    {st t st' t'} (hr : ReachableT transition time toSt applyAll init st t) (hq : Q st t)
    (h : StepsT transition time toSt applyAll allow st t st' t') : Q st' t' := by
  induction h with
  | refl => exact hq
  | step hs hle hok ht ih => exact hstep _ _ _ _ _ _ (hr.steps hs) ih hle hok ht

end H5iAppLib
