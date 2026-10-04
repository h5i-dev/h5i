import H5iAppLib.Basic
/-!
# The authorization schema

Two reusable universal-authorization properties, one for what a command
writes and one for what its reply discloses. Each is quantified over every
actor, state and command, so no command the decoder builds escapes the
policy. `cargo app-verify` reports which apps state a theorem of this shape.
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

/-- Every row the reply of a successful command discloses satisfies
`visible`, for every actor, state and command. `readsOf` lists what the reply
discloses (the rows it returns, or their ids). The confidentiality
counterpart of `WritesAuthorized`: a cross-tenant read (BOLA, IDOR) breaks
it, with no write to blame. -/
def ReadsAuthorized {P S C WS R E St D : Type}
    (transition : P → S → C → Result (core.result.Result (WS × R) E))
    (toSt : S → St) (readsOf : R → List D) (visible : St → P → D → Prop) : Prop :=
  ∀ (a : P) (s : S) (c : C) (ws : WS) (r : R),
    transition a s c = .ok (.Ok (ws, r)) → ∀ d ∈ readsOf r, visible (toSt s) a d

theorem readsAuthorized_of_spec {P S C WS R E St D : Type}
    {transition : P → S → C → Result (core.result.Result (WS × R) E)}
    {toSt : S → St} {readsOf : R → List D} {visible : St → P → D → Prop}
    (h : ∀ a s c, transition a s c ⦃ OnOk (fun _ r => ∀ d ∈ readsOf r, visible (toSt s) a d) ⦄) :
    ReadsAuthorized transition toSt readsOf visible :=
  fun a s c _ _ heq => of_spec (h a s c) heq

/-- A reply built by filtering the state's rows with a check that implies
`visible` discloses only visible rows. The usual shape of a list endpoint. -/
theorem reads_of_filter {St P D : Type} {visible : St → P → D → Prop} {st : St} {a : P}
    (rows : List D) (keep : D → Bool) (hkeep : ∀ d ∈ rows, keep d = true → visible st a d)
    {out : List D} (hout : out = rows.filter keep) : ∀ d ∈ out, visible st a d := by
  subst hout; intro d hd
  rw [List.mem_filter] at hd
  exact hkeep d hd.1 hd.2

/-- Both: every write allowed and every disclosed row visible. -/
def Authorized {P S C WS W R E St D : Type}
    (transition : P → S → C → Result (core.result.Result (WS × R) E)) (toSt : S → St)
    (writesOf : WS → List W) (allowed : St → P → W → Prop)
    (readsOf : R → List D) (visible : St → P → D → Prop) : Prop :=
  WritesAuthorized transition toSt writesOf allowed ∧ ReadsAuthorized transition toSt readsOf visible

end H5iAppLib
