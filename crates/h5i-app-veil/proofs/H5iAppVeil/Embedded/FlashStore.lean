module

public import Veil

/-!
# Veil Models of `FlashStore` (`H5iAppVeil.Embedded.FlashStore`)

Specifies `crates/h5i-app-embedded/src/store.rs` directly in Veil's DSL and
runs Veil's native explicit-state model checker (`#model_check`) and Lasso
temporal checker (`#temporal_check`):

* `FlashStorePlain`: mirrors `store.rs:235` (`wrapping_add(1)`) paired with
  `store.rs:169` (`seq > s`) on a 3-bit (`Fin 8`) sequence number. Veil's
  `#model_check` discovers the concrete wraparound durability counterexample
  (`seq: 7 → 0` causes `scan` to prefer the older record `7`).
* `FlashStoreModular`: replaces plain `>` with modular half-space comparison
  and models power cuts during erase/write + torn tails. Veil's `#model_check`
  verifies `crash_consistency` and `durability`, and `#temporal_check` verifies
  `recovery_progress` under weak fairness `𝒲ℱ rebootScan`.

Concrete counterexample replay against the real Rust `FlashStore` lives in
`crates/h5i-app-embedded/src/replay.rs` and `h5i app trace-replay`.
-/

/-! ## 1. Plain `>` comparison (`store.rs:169`) — Veil discovers wraparound bug -/

veil module FlashStorePlain

enum FlashPhase = { idle, erasing, writing, crashed }

individual aValid : Bool
individual aSeq : Fin 8
individual aVal : Bool
individual bValid : Bool
individual bSeq : Fin 8
individual bVal : Bool
individual activeB : Bool
individual posSeq : Fin 8
individual targetB : Bool
individual targetSeq : Fin 8
individual pendingValid : Bool
individual pendingVal : Bool
individual ackedValid : Bool
individual ackedVal : Bool
individual phase : FlashPhase

#gen_state

after_init {
  aValid := false
  aSeq := 0
  aVal := false
  bValid := false
  bSeq := 0
  bVal := false
  activeB := true
  posSeq := 0
  targetB := false
  targetSeq := 0
  pendingValid := false
  pendingVal := false
  ackedValid := false
  ackedVal := false
  phase := idle
}

action beginSave (v : Bool) {
  require phase = idle
  require ¬(ackedValid ∧ ackedVal = v)
  targetB := !activeB
  targetSeq := posSeq + 1
  pendingValid := true
  pendingVal := v
  phase := erasing
}

action eraseOther {
  require phase = erasing
  if targetB then
    bValid := false
  else
    aValid := false
  phase := writing
}

action writeComplete {
  require phase = writing
  if targetB then
    bValid := true
    bSeq := targetSeq
    bVal := pendingVal
  else
    aValid := true
    aSeq := targetSeq
    aVal := pendingVal
  activeB := targetB
  posSeq := targetSeq
  ackedValid := true
  ackedVal := pendingVal
  pendingValid := false
  phase := idle
}

-- Plain `>` scan matching `crates/h5i-app-embedded/src/store.rs:169`.
ghost relation scanSelectsB :=
  bValid ∧ (¬aValid ∨ bSeq.val > aSeq.val)

ghost relation scanValIs (v : Bool) :=
  (scanSelectsB ∧ bVal = v) ∨ (¬scanSelectsB ∧ aValid ∧ aVal = v)

-- Durability: when idle after an acknowledged save, scanning both regions must
-- recover the acknowledged value. Fails on 3-bit wraparound (`posSeq: 7 → 0`).
safety [durability]
  phase = idle ∧ ackedValid → scanValIs ackedVal

#gen_spec

-- Expected violation: Veil's model checker discovers the 3-bit wraparound bug.
set_option veil.violationIsError false in
#model_check interpreted { FlashPhase := FlashPhase_IndT } {}

end FlashStorePlain

/-! ## 2. Modular half-space comparison — crash consistency, durability & liveness -/

veil module FlashStoreModular

open Lentil

enum FlashPhase = { idle, erasing, writing, crashed }

individual aValid : Bool
individual aSeq : Fin 8
individual aVal : Bool
individual bValid : Bool
individual bSeq : Fin 8
individual bVal : Bool
individual activeB : Bool
individual posSeq : Fin 8
individual targetB : Bool
individual targetSeq : Fin 8
individual pendingValid : Bool
individual pendingVal : Bool
individual ackedValid : Bool
individual ackedVal : Bool
individual preCrashAckedValid : Bool
individual preCrashAckedVal : Bool
individual preCrashPendingValid : Bool
individual preCrashPendingVal : Bool
individual justRecovered : Bool
individual recoveredValid : Bool
individual recoveredVal : Bool
individual phase : FlashPhase

#gen_state

after_init {
  aValid := false
  aSeq := 0
  aVal := false
  bValid := false
  bSeq := 0
  bVal := false
  activeB := true
  posSeq := 0
  targetB := false
  targetSeq := 0
  pendingValid := false
  pendingVal := false
  ackedValid := false
  ackedVal := false
  preCrashAckedValid := false
  preCrashAckedVal := false
  preCrashPendingValid := false
  preCrashPendingVal := false
  justRecovered := false
  recoveredValid := false
  recoveredVal := false
  phase := idle
}

-- Modular half-space order on 3-bit sequence numbers: `b` is newer than `a`
-- iff `0 < (b - a) mod 8 < 4`.
ghost relation modNewerB :=
  (bSeq.val + 8 - aSeq.val) % 8 > 0 ∧ (bSeq.val + 8 - aSeq.val) % 8 < 4

ghost relation scanSelectsB :=
  bValid ∧ (¬aValid ∨ modNewerB)

ghost relation scanValIs (v : Bool) :=
  (scanSelectsB ∧ bVal = v) ∨ (¬scanSelectsB ∧ aValid ∧ aVal = v)

action beginSave (v : Bool) {
  require phase = idle
  require ¬(ackedValid ∧ ackedVal = v)
  justRecovered := false
  targetB := !activeB
  targetSeq := posSeq + 1
  pendingValid := true
  pendingVal := v
  phase := erasing
}

action eraseOther {
  require phase = erasing
  if targetB then
    bValid := false
  else
    aValid := false
  phase := writing
}

action writeComplete {
  require phase = writing
  if targetB then
    bValid := true
    bSeq := targetSeq
    bVal := pendingVal
  else
    aValid := true
    aSeq := targetSeq
    aVal := pendingVal
  activeB := targetB
  posSeq := targetSeq
  ackedValid := true
  ackedVal := pendingVal
  pendingValid := false
  phase := idle
}

-- Power cut during idle, erase, or torn mid-write (target record remains invalid).
action powerCut {
  require phase ≠ crashed
  justRecovered := false
  preCrashAckedValid := ackedValid
  preCrashAckedVal := ackedVal
  preCrashPendingValid := pendingValid
  preCrashPendingVal := pendingVal
  phase := crashed
}

-- Reboot and scan both NOR flash regions using modular sequence ordering.
action rebootScan {
  require phase = crashed
  if bValid ∧ (¬aValid ∨ ((bSeq.val + 8 - aSeq.val) % 8 > 0 ∧ (bSeq.val + 8 - aSeq.val) % 8 < 4)) then
    activeB := true
    posSeq := bSeq
    ackedValid := true
    ackedVal := bVal
    recoveredValid := true
    recoveredVal := bVal
  else if aValid then
    activeB := false
    posSeq := aSeq
    ackedValid := true
    ackedVal := aVal
    recoveredValid := true
    recoveredVal := aVal
  else
    activeB := true
    posSeq := 0
    ackedValid := false
    recoveredValid := false
  pendingValid := false
  justRecovered := true
  phase := idle
}

-- Durability: when idle after an acknowledged save, scanning recovers `ackedVal`.
safety [durability]
  phase = idle ∧ ackedValid → scanValIs ackedVal

-- Crash consistency: after a power cut + reboot scan, recovered state equals
-- either pre-crash acknowledged value or pre-crash in-flight pending value.
safety [crash_consistency]
  justRecovered →
    ((recoveredValid = preCrashAckedValid ∧ (recoveredValid → recoveredVal = preCrashAckedVal)) ∨
     (preCrashPendingValid ∧ recoveredValid ∧ recoveredVal = preCrashPendingVal))

-- Recovery progress (Liveness under Weak Fairness of `rebootScan`).
temporal [recovery_progress]
  (𝒲ℱ rebootScan) → □ (⌜ phase = crashed ⌝ → ◇ ⌜ phase = idle ⌝)

#gen_spec

#model_check interpreted { FlashPhase := FlashPhase_IndT } {}
#temporal_check [recovery_progress] interpreted { FlashPhase := FlashPhase_IndT } {}

end FlashStoreModular
