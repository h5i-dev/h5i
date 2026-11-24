module

public import H5iAppVeil.KernelBridge
public import Veil.Core.Tools.ModelChecker.Trace
public import Lentil

@[expose] public section
/-!
# Run Bridge (`H5iAppVeil.RunBridge`)

Thin h5i-specific bridge connecting h5i's finite `Run` / `ReachableT`
(`H5iAppLib.Runs` / `H5iAppLib.Time`) directly to Veil's
`Veil.RelationalTransitionSystem`, `Veil.ModelChecker.Trace`, and Lentil's
infinite-execution temporal operators (`Lentil.exec`, `Lentil.always`,
`Lentil.state_pred`).

All temporal logic (`Lentil`) and transition-system (`Veil`) definitions are
reused directly without reimplementation.
-/

namespace H5iAppVeil

variable {P S C WS R E St : Type}

/-- Finite serial run matching `H5iAppLib.Run` (with pure `Except` so it
operates on Veil's `v4.35.0` toolchain without pulling Aeneas `v4.31.0`). -/
inductive Run (transition : P → S → C → Except E (WS × R))
    (toSt : S → St) (applyAll : St → WS → St) (init : St) :
    St → List (KernelEvent P C R E) → Prop
  | nil : Run transition toSt applyAll init init []
  | ok {st evs s a c ws r} :
      Run transition toSt applyAll init st evs →
      toSt s = st →
      transition a s c = .ok (ws, r) →
      Run transition toSt applyAll init (applyAll st ws) (evs ++ [.ok a c r])
  | err {st evs s a c e} :
      Run transition toSt applyAll init st evs →
      toSt s = st →
      transition a s c = .error e →
      Run transition toSt applyAll init st (evs ++ [.err a c e])

/-- Exact equivalence between h5i's `Run` and Veil's `RelationalTransitionSystem.reachable`. -/
theorem run_iff_veil_reachable
    {transition : P → S → C → Except E (WS × R)}
    {toSt : S → St} {applyAll : St → WS → St} {init st : St} :
    (∃ evs, Run transition toSt applyAll init st evs) ↔
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
      exact ⟨[], Run.nil⟩
    | step s s' _ hnext ih =>
      obtain ⟨evs, hrun⟩ := ih
      obtain ⟨ev, hstep⟩ := hnext
      cases hstep with
      | @ok s a c ws r ht => exact ⟨evs ++ [.ok a c r], Run.ok hrun rfl ht⟩
      | @err s a c e ht => exact ⟨evs ++ [.err a c e], Run.err hrun rfl ht⟩

private theorem run_of_stepList
    {transition : P → S → C → Except E (WS × R)}
    {toSt : S → St} {applyAll : St → WS → St} {init : St} :
    ∀ (steps : List (Veil.ModelChecker.Step St (KernelEvent P C R E))) (st : St)
      (evs : List (KernelEvent P C R E)),
      Run transition toSt applyAll init st evs →
      Veil.ModelChecker.StepList.validFrom (kernelRTS transition toSt applyAll init) () st steps →
      Run transition toSt applyAll init
        (Veil.ModelChecker.Steps.getLastStateD (Array.mk steps) st)
        (evs ++ steps.map (·.transitionLabel))
  | [], st, evs, hrun, _ => by
    simpa [Veil.ModelChecker.Steps.getLastStateD] using hrun
  | ⟨lbl, nxt⟩ :: rest, st, evs, hrun, ⟨hstep, hrest⟩ => by
    have hrun' : Run transition toSt applyAll init nxt (evs ++ [lbl]) := by
      cases hstep with
      | @ok s a c ws r ht => exact Run.ok hrun rfl ht
      | @err s a c e ht => exact Run.err hrun rfl ht
    have hrec := run_of_stepList rest nxt (evs ++ [lbl]) hrun' hrest
    simpa [Veil.ModelChecker.Steps.getLastD_prepend_same, List.append_assoc] using hrec

/-- Every valid Veil model-checker trace (`Veil.ModelChecker.Trace`) of `kernelRTS`
corresponds to a valid h5i `Run` whose event list is the trace's action labels. -/
theorem run_of_veil_trace
    {transition : P → S → C → Except E (WS × R)}
    {toSt : S → St} {applyAll : St → WS → St} {init : St}
    (tr : Veil.ModelChecker.Trace Unit St (KernelEvent P C R E))
    (hvalid : tr.isValid (kernelRTS transition toSt applyAll init)) :
    Run transition toSt applyAll init tr.lastState (tr.steps.toList.map (·.transitionLabel)) := by
  have hinit : tr.initialState = init := hvalid.initialStateSatisfiesInit
  have hrun0 : Run transition toSt applyAll init tr.initialState [] := hinit ▸ Run.nil
  have h := run_of_stepList tr.steps.toList tr.initialState [] hrun0 hvalid.stepsValid
  simpa [Veil.ModelChecker.Trace.lastState] using h

/-- Lift any invariant proven over finite `Run`s (`Run.inv`) to Lentil's `□ ⌜ I ⌝`
over every infinite execution `e : Lentil.exec St` of `kernelRTS`. -/
theorem lentil_always_of_run_inv
    {transition : P → S → C → Except E (WS × R)}
    {toSt : S → St} {applyAll : St → WS → St} {init : St} {I : St → Prop}
    (hInv : ∀ st evs, Run transition toSt applyAll init st evs → I st)
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

/-- Timestamps along any infinite execution `e : Lentil.exec (St × Nat)` of
`timedKernelRTS` are monotonic (never decrease). -/
theorem timed_exec_monotonic
    {transition : P → S → C → Except E (WS × R)}
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
      · rw [heq]; exact Nat.le_refl _
    exact Nat.le_trans ih hstep

end H5iAppVeil
