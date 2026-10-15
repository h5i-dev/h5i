import H5iAppLib.Basic
/-!
# Read-only commands

`h5i-app-pg` runs a command its kernel declares read-only (`Kernel::read_only`)
in a `SERIALIZABLE READ ONLY DEFERRABLE` transaction, without locks or
retries. That is sound when every accepted run of such a command writes
nothing: then its transaction changes no state, so the serial order the
database promises can place it anywhere among the commits whose effects its
snapshot shows, and the reply is `transition` of a state the commits reached.
`Sound` is that obligation, stated over the kernel's own `read_only`; a
server that overrides `Kernel::read_only` delegates to that function and
proves `Sound` for it (`h5i app lint` checks).
-/
open Aeneas Aeneas.Std Result

namespace H5iAppLib.ReadOnly

/-- Whenever `readOnly c` is `true` and `transition` accepts `c`, the write
set is empty (`writesOf ws = []`). For every actor and state, not only
reachable ones. -/
def Sound {P S C WS W R E : Type}
    (readOnly : C → Result Bool)
    (transition : P → S → C → Result (core.result.Result (WS × R) E))
    (writesOf : WS → List W) : Prop :=
  ∀ (a : P) (s : S) (c : C) (ws : WS) (r : R),
    readOnly c = ok true → transition a s c = .ok (.Ok (ws, r)) → writesOf ws = []

/-- `Sound` from a spec of `transition` that, for a read-only command, says
every accepted run writes nothing. -/
theorem sound_of_spec {P S C WS W R E : Type}
    {readOnly : C → Result Bool}
    {transition : P → S → C → Result (core.result.Result (WS × R) E)}
    {writesOf : WS → List W}
    (h : ∀ a s c, readOnly c = ok true → transition a s c ⦃ OnOk (fun ws _ => writesOf ws = []) ⦄) :
    Sound readOnly transition writesOf :=
  fun a s c _ _ hro heq => of_spec (h a s c hro) heq

/-- What a read-only run means for the state: applying its empty write set
changes nothing, for any `apply` that is a fold over the writes. -/
theorem apply_empty {S W : Type} (step : S → W → S) (s : S) (ws : List W) (h : ws = []) :
    ws.foldl step s = s := by
  subst h; rfl

end H5iAppLib.ReadOnly
