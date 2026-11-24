import Aeneas
import H5iAppLib.Runs
import H5iAppLib.Time
import Veil.Core.Tools.ModelChecker.TransitionSystem

/-!
# Kernel Bridge (`H5iAppVeil.KernelBridge`)

Thin h5i-specific adapter that derives a `Veil.RelationalTransitionSystem`
directly from an **Aeneas-extracted** h5i `Kernel`
(`transition : P → S → C → Aeneas.Std.Result (core.result.Result (WS × R) E)`,
`Step = apply ∘ transition`, matching `MemoryEngine::execute`, `H5iAppLib.Run`,
and `H5iAppLib.ReachableT`).

Uses `H5iAppLib.Event` directly — no independent redefinition of events or runs.
-/

open Aeneas Aeneas.Std Result

namespace H5iAppVeil

/-- Single Aeneas-extracted kernel step (`Step = apply ∘ transition`) as a
labelled transition relation for `Veil.RelationalTransitionSystem`, labelled by
the actual `H5iAppLib.Event`. -/
inductive KernelStep {P S C WS R E St : Type}
    (transition : P → S → C → Result (core.result.Result (WS × R) E))
    (toSt : S → St) (applyAll : St → WS → St) :
    St → H5iAppLib.Event P C R E → St → Prop
  | ok {s a c ws r} :
      transition a s c = Result.ok (.Ok (ws, r)) →
      KernelStep transition toSt applyAll (toSt s) (.ok a c r) (applyAll (toSt s) ws)
  | err {s a c e} :
      transition a s c = Result.ok (.Err e) →
      KernelStep transition toSt applyAll (toSt s) (.err a c e) (toSt s)

/-- Derive a `Veil.RelationalTransitionSystem` directly from an Aeneas-extracted
h5i `Kernel` starting at `init`. Users do not rewrite `Next`. -/
def kernelRTS {P S C WS R E St : Type}
    (transition : P → S → C → Result (core.result.Result (WS × R) E))
    (toSt : S → St) (applyAll : St → WS → St) (init : St) :
    Veil.RelationalTransitionSystem Unit St (H5iAppLib.Event P C R E) where
  assumptions := fun _ => True
  init := fun _ st => st = init
  tr := fun _ st ev st' => KernelStep transition toSt applyAll st ev st'

/-- Timed kernel step matching `H5iAppLib.ReachableT` / `MemoryEngine::execute_at`:
state pairs the abstract snapshot with the latest commit timestamp, which never decreases. -/
inductive TimedKernelStep {P S C WS R E St : Type}
    (transition : P → S → C → Result (core.result.Result (WS × R) E))
    (time : P → Nat) (toSt : S → St) (applyAll : St → WS → St) :
    (St × Nat) → H5iAppLib.Event P C R E → (St × Nat) → Prop
  | ok {s t a c ws r} :
      t ≤ time a →
      transition a s c = Result.ok (.Ok (ws, r)) →
      TimedKernelStep transition time toSt applyAll
        (toSt s, t) (.ok a c r) (applyAll (toSt s) ws, time a)
  | err {s t a c e} :
      t ≤ time a →
      transition a s c = Result.ok (.Err e) →
      TimedKernelStep transition time toSt applyAll
        (toSt s, t) (.err a c e) (toSt s, t)

/-- Derive a timed `Veil.RelationalTransitionSystem` directly matching
`H5iAppLib.ReachableT`. -/
def timedKernelRTS {P S C WS R E St : Type}
    (transition : P → S → C → Result (core.result.Result (WS × R) E))
    (time : P → Nat) (toSt : S → St) (applyAll : St → WS → St) (init : St) :
    Veil.RelationalTransitionSystem Unit (St × Nat) (H5iAppLib.Event P C R E) where
  assumptions := fun _ => True
  init := fun _ st => st = (init, 0)
  tr := fun _ st ev st' => TimedKernelStep transition time toSt applyAll st ev st'

end H5iAppVeil
