import Aeneas
import H5iAppLib.Runs
import H5iAppLib.Time
import H5iAppVeil.KernelBridge
import Veil.Core.Tools.ModelChecker.Trace
import Lentil

/-!
# Formal Run Bridge (`H5iAppVeil.RunBridge`)

Establishes the direct formal equivalence between the **actual**
`H5iAppLib.Run` / `H5iAppLib.ReachableT` (operating on Aeneas-extracted
`Aeneas.Std.Result` functions) and Veil's `Veil.RelationalTransitionSystem`,
`Veil.ModelChecker.Trace`, and Lentil's infinite-execution temporal operators
(`Lentil.exec`, `Lentil.always`, `Lentil.state_pred`).

Any existing Aeneas-extracted safety theorem proven over `H5iAppLib.Run`
(such as `thermostat_kernel.Proofs.inv_reachable`) lifts directly — without
reproving or translating the kernel — into Veil reachability and Lentil `□`.
-/

open Aeneas Aeneas.Std Result

namespace H5iAppVeil

variable {P S C WS R E St : Type}

/-- Exact equivalence between the **actual** `H5iAppLib.Run` and Veil's
`RelationalTransitionSystem.reachable` on `kernelRTS`. -/
theorem run_iff_veil_reachable
    {transition : P → S → C → Result (core.result.Result (WS × R) E)}
    {toSt : S → St} {applyAll : St → WS → St} {init st : St} :
    (∃ evs, H5iAppLib.Run transition toSt applyAll init st evs) ↔
      (kernelRTS transition toSt applyAll init).reachable () st := by
  constructor
  · rintro ⟨evs, hr⟩
    induction hr with
    | nil => exact .init _ trivial rfl
    | @ok st evs s a c ws r _ hst ht ih =>
      subst hst
      exact .step _ _ ih ⟨.ok a c r, KernelStep.ok ht⟩
    | @err st evs s a c e _ hst ht ih =>
      subst hst
      exact .step _ _ ih ⟨.err a c e, KernelStep.err ht⟩
  · intro hr
    induction hr with
    | init s _ hinit =>
      subst hinit
      exact ⟨[], H5iAppLib.Run.nil⟩
    | step s s' _ hnext ih =>
      obtain ⟨evs, hrun⟩ := ih
      obtain ⟨ev, hstep⟩ := hnext
      cases hstep with
      | @ok s a c ws r ht => exact ⟨evs ++ [.ok a c r], H5iAppLib.Run.ok hrun rfl ht⟩
      | @err s a c e ht => exact ⟨evs ++ [.err a c e], H5iAppLib.Run.err hrun rfl ht⟩

private theorem run_of_stepList
    {transition : P → S → C → Result (core.result.Result (WS × R) E)}
    {toSt : S → St} {applyAll : St → WS → St} {init : St} :
    ∀ (steps : List (Veil.ModelChecker.Step St (H5iAppLib.Event P C R E))) (st : St)
      (evs : List (H5iAppLib.Event P C R E)),
      H5iAppLib.Run transition toSt applyAll init st evs →
      Veil.ModelChecker.StepList.validFrom (kernelRTS transition toSt applyAll init) () st steps →
      H5iAppLib.Run transition toSt applyAll init
        (Veil.ModelChecker.Steps.getLastStateD (Array.mk steps) st)
        (evs ++ steps.map (·.transitionLabel))
  | [], st, evs, hrun, _ => by
    simpa [Veil.ModelChecker.Steps.getLastStateD] using hrun
  | ⟨lbl, nxt⟩ :: rest, st, evs, hrun, ⟨hstep, hrest⟩ => by
    have hrun' : H5iAppLib.Run transition toSt applyAll init nxt (evs ++ [lbl]) := by
      cases hstep with
      | @ok s a c ws r ht => exact H5iAppLib.Run.ok hrun rfl ht
      | @err s a c e ht => exact H5iAppLib.Run.err hrun rfl ht
    have hrec := run_of_stepList rest nxt (evs ++ [lbl]) hrun' hrest
    simpa [Veil.ModelChecker.Steps.getLastD_prepend_same, List.append_assoc] using hrec

/-- Every valid Veil model-checker trace (`Veil.ModelChecker.Trace`) of
`kernelRTS` corresponds to an actual `H5iAppLib.Run` whose event list is the
trace's action labels. -/
theorem run_of_veil_trace
    {transition : P → S → C → Result (core.result.Result (WS × R) E)}
    {toSt : S → St} {applyAll : St → WS → St} {init : St}
    (tr : Veil.ModelChecker.Trace Unit St (H5iAppLib.Event P C R E))
    (hvalid : tr.isValid (kernelRTS transition toSt applyAll init)) :
    H5iAppLib.Run transition toSt applyAll init tr.lastState
      (tr.steps.toList.map (·.transitionLabel)) := by
  have hinit : tr.initialState = init := hvalid.initialStateSatisfiesInit
  have hrun0 : H5iAppLib.Run transition toSt applyAll init tr.initialState [] :=
    hinit ▸ H5iAppLib.Run.nil
  have h := run_of_stepList tr.steps.toList tr.initialState [] hrun0 hvalid.stepsValid
  simpa [Veil.ModelChecker.Trace.lastState] using h

/-- Lift any invariant proven over the actual `H5iAppLib.Run` (such as an
existing Aeneas proof `inv_reachable`) directly into Lentil's `□ ⌜ I ⌝` over
every infinite execution `e : Lentil.exec St` of `kernelRTS`. -/
theorem lentil_always_of_run_inv
    {transition : P → S → C → Result (core.result.Result (WS × R) E)}
    {toSt : S → St} {applyAll : St → WS → St} {init : St} {I : St → Prop}
    (hInv : ∀ st evs, H5iAppLib.Run transition toSt applyAll init st evs → I st)
    (e : Lentil.exec St)
    (hInit : (kernelRTS transition toSt applyAll init).init () (e 0))
    (hNext : ∀ n, (kernelRTS transition toSt applyAll init).next () (e n) (e (n + 1)) ∨ e (n + 1) = e n) :
    Lentil.always (Lentil.state_pred I) e 0 := by
  have hreach : ∀ k, (kernelRTS transition toSt applyAll init).reachable () (e k) := by
    intro k
    induction k with
    | zero => exact .init _ trivial hInit
    | succ n ih =>
      rcases hNext n with hn | heq
      · exact .step _ _ ih hn
      · rw [heq]; exact ih
  intro k
  obtain ⟨evs, hrun⟩ := run_iff_veil_reachable.mpr (hreach k)
  simpa [Lentil.state_pred] using hInv (e k) evs hrun

/-- Exact equivalence between `H5iAppLib.ReachableT` and Veil reachability on
`timedKernelRTS` (error steps leave `(st, t)` unchanged). -/
theorem reachableT_iff_timed_veil_reachable
    {transition : P → S → C → Result (core.result.Result (WS × R) E)}
    {time : P → Nat} {toSt : S → St} {applyAll : St → WS → St} {init st : St} {t : Nat} :
    H5iAppLib.ReachableT transition time toSt applyAll init st t ↔
      (timedKernelRTS transition time toSt applyAll init).reachable () (st, t) := by
  constructor
  · intro hr
    induction hr with
    | init => exact .init _ trivial rfl
    | @step s t a c ws r _ hle ht ih =>
      exact .step _ _ ih ⟨.ok a c r, TimedKernelStep.ok hle ht⟩
  · intro hr
    generalize hp : (st, t) = p at hr
    induction hr generalizing st t with
    | init s _ hinit =>
      subst hp
      cases hinit
      exact H5iAppLib.ReachableT.init
    | step s s' _ hnext ih =>
      subst hp
      obtain ⟨ev, hstep⟩ := hnext
      cases hstep with
      | @ok s0 t0 a c ws r hle ht =>
        exact H5iAppLib.ReachableT.step (ih rfl) hle ht
      | @err s0 t0 a c e _ _ =>
        exact ih rfl

/-- Timestamps along any infinite execution `e : Lentil.exec (St × Nat)` of
`timedKernelRTS` are monotonic (never decrease). -/
theorem timed_exec_monotonic
    {transition : P → S → C → Result (core.result.Result (WS × R) E)}
    {time : P → Nat} {toSt : S → St} {applyAll : St → WS → St} {init : St}
    (e : Lentil.exec (St × Nat))
    (hNext : ∀ n, (timedKernelRTS transition time toSt applyAll init).next () (e n) (e (n + 1)) ∨ e (n + 1) = e n)
    {m n : Nat} (hle : m ≤ n) :
    (e m).2 ≤ (e n).2 := by
  obtain ⟨d, rfl⟩ := Nat.exists_eq_add_of_le hle
  clear hle
  induction d with
  | zero => exact Nat.le_refl _
  | succ k ih =>
    have hstep : (e (m + k)).2 ≤ (e (m + k + 1)).2 := by
      rcases hNext (m + k) with ⟨_, htr⟩ | heq
      · generalize hcur : e (m + k) = cur at htr
        generalize hnxt : e (m + k + 1) = nxt at htr
        cases htr with
        | ok hle' _ => exact hle'
        | err _ _ => exact Nat.le_refl _
      · rw [heq]
    exact Nat.le_trans ih hstep

end H5iAppVeil
