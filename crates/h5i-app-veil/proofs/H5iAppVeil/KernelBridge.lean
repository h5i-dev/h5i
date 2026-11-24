module

public import Veil.Core.Tools.ModelChecker.TransitionSystem

@[expose] public section
/-!
# Kernel Bridge (`H5iAppVeil.KernelBridge`)

Thin h5i-specific adapter that derives a `Veil.RelationalTransitionSystem`
directly from a pure Lean `Kernel` specification (`Step = apply ∘ transition`,
matching the shape of `MemoryEngine::execute`, `H5iAppLib.Runs`, and
`H5iAppLib.Time`), without redefining transition systems or temporal logic.

## Current Integration Scope (Model-Level Only)

`h5i`'s Aeneas-extracted kernels and `H5iAppLib` currently pin Lean `v4.31.0`
(`crates/h5i-app-cli/src/pins.rs`, `Aeneas.Std.Result`), whereas Veil
(`verse-lab/veil @ v4.35.0`, providing Lentil & Lasso `#temporal_check`) pins
Lean `v4.35.0` (no `v4.31.0` tag exists; upstream Aeneas `main` is on `v4.36.0`).
Because the two toolchains cannot be imported in one Lake package today, this
module operates on pure `Except`-based Lean kernel specifications rather than
importing Aeneas-extracted modules directly.
-/

namespace H5iAppVeil

/-- Mirror of `H5iAppLib.Event`: one request and its outcome (accepted or refused). -/
inductive KernelEvent (P C R E : Type) where
  | ok (a : P) (c : C) (r : R)
  | err (a : P) (c : C) (e : E)
deriving DecidableEq, Repr

/-- Single kernel step (`Step = apply ∘ transition`) as a labelled transition
relation for `Veil.RelationalTransitionSystem`. -/
inductive KernelStep {P S C WS R E St : Type}
    (transition : P → S → C → Except E (WS × R))
    (toSt : S → St) (applyAll : St → WS → St) :
    St → KernelEvent P C R E → St → Prop
  | ok {s a c ws r} :
      transition a s c = .ok (ws, r) →
      KernelStep transition toSt applyAll (toSt s) (.ok a c r) (applyAll (toSt s) ws)
  | err {s a c e} :
      transition a s c = .error e →
      KernelStep transition toSt applyAll (toSt s) (.err a c e) (toSt s)

/-- Derive a `Veil.RelationalTransitionSystem` directly from a kernel transition
function from initial abstract state `init`. -/
def kernelRTS {P S C WS R E St : Type}
    (transition : P → S → C → Except E (WS × R))
    (toSt : S → St) (applyAll : St → WS → St) (init : St) :
    Veil.RelationalTransitionSystem Unit St (KernelEvent P C R E) where
  assumptions := fun _ => True
  init := fun _ st => st = init
  tr := fun _ st ev st' => KernelStep transition toSt applyAll st ev st'

/-- Timed kernel step matching the semantics of `H5iAppLib.Time.ReachableT` /
`MemoryEngine::execute_at`: state pairs the abstract snapshot with the latest
commit timestamp, which never decreases. -/
inductive TimedKernelStep {P S C WS R E St : Type}
    (transition : P → S → C → Except E (WS × R))
    (time : P → Nat) (toSt : S → St) (applyAll : St → WS → St) :
    (St × Nat) → KernelEvent P C R E → (St × Nat) → Prop
  | ok {s t a c ws r} :
      t ≤ time a →
      transition a s c = .ok (ws, r) →
      TimedKernelStep transition time toSt applyAll
        (toSt s, t) (.ok a c r) (applyAll (toSt s) ws, time a)
  | err {s t a c e} :
      t ≤ time a →
      transition a s c = .error e →
      TimedKernelStep transition time toSt applyAll
        (toSt s, t) (.err a c e) (toSt s, t)

/-- Derive a timed `Veil.RelationalTransitionSystem` (model-level mirror of
`H5iAppLib.Time.ReachableT`). -/
def timedKernelRTS {P S C WS R E St : Type}
    (transition : P → S → C → Except E (WS × R))
    (time : P → Nat) (toSt : S → St) (applyAll : St → WS → St) (init : St) :
    Veil.RelationalTransitionSystem Unit (St × Nat) (KernelEvent P C R E) where
  assumptions := fun _ => True
  init := fun _ st => st = (init, 0)
  tr := fun _ st ev st' => TimedKernelStep transition time toSt applyAll st ev st'

end H5iAppVeil
