import H5iAppLib.Basic
/-!
# The authorization schema

One reusable universal-authorization property. Quantified over every actor,
state and command, so no command the decoder builds escapes the policy.
`cargo app-verify` reports which apps state a theorem of this shape.
-/
open Aeneas Aeneas.Std Result

namespace H5iAppLib

/-- Every write of a successful command satisfies `allowed`, for every actor,
state and command. `writesOf` reads the write set as a list. -/
def WritesAuthorized {P S C WS W R E St : Type}
    (transition : P → S → C → Result (core.result.Result (WS × R) E))
    (toSt : S → St) (writesOf : WS → List W) (allowed : St → P → W → Prop) : Prop :=
  ∀ (a : P) (s : S) (c : C) (ws : WS) (r : R),
    transition a s c = .ok (.Ok (ws, r)) → ∀ w ∈ writesOf ws, allowed (toSt s) a w

/-- Reduces `WritesAuthorized` to the `OnOk` postcondition the kernel proofs
already produce. -/
theorem writesAuthorized_of_spec {P S C WS W R E St : Type}
    {transition : P → S → C → Result (core.result.Result (WS × R) E)}
    {toSt : S → St} {writesOf : WS → List W} {allowed : St → P → W → Prop}
    (h : ∀ a s c, transition a s c ⦃ OnOk (fun ws _ => ∀ w ∈ writesOf ws, allowed (toSt s) a w) ⦄) :
    WritesAuthorized transition toSt writesOf allowed :=
  fun a s c _ _ heq => of_spec (h a s c) heq

end H5iAppLib
