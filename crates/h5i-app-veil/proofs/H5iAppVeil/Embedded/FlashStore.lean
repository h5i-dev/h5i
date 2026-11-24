import Veil

/-!
# Veil Models of `FlashStore` (`H5iAppVeil.Embedded.FlashStore`)

Specifies `crates/h5i-app-embedded/src/store.rs` in Veil's DSL and runs Veil's
native explicit-state model checker (`#model_check`) and Lasso temporal checker
(`#temporal_check`).

## Scope Distinction (Current Rust Implementation vs Proposed Fix)

- **Current Rust `FlashStore` (`crates/h5i-app-embedded/src/store.rs`)** uses
  plain `seq > s` comparison (`store.rs:169`) with `wrapping_add(1)` (`store.rs:235`):
  * `FlashStorePlain` models this plain `>` behavior on a 3-bit (`Fin 8`) sequence
    number and Veil's `#model_check` discovers the wraparound durability counterexample.
  * Real Rust counterexample replay (`h5i app trace-replay`,
    `crates/h5i-app-embedded/src/replay.rs`) reproduces this wraparound boundary
    directly against the unmodified Rust `FlashStore` (`u32::MAX → 0`).
- **Proposed modular-order algorithm (`FlashStoreModular` — NOT the current Rust
  `FlashStore` implementation)**:
  * Replaces plain `>` with modular half-space ordering (`0 < (s₁ - s₂) mod 8 < 4`,
    which is sound provided at most half the sequence space separates the newest
    record in the standby region from the newest record in the active region —
    guaranteed here because each region holds at most 2 records before switching
    and erasing the standby region).
  * Models the 2-slot append-per-region protocol, unchanged-save no-op,
    separated `writeLanded` / `ackSave`, partial erase, torn writes, and
    repeated power cuts/reboots.
  * Verifying `FlashStoreModular` verifies the **proposed** algorithm at the
    finite-state model level; changing the Rust implementation to modular
    comparison would require updating and re-verifying `store.rs` separately.

## Explicit Modeling Abstractions

- **Region capacity**: 2 record slots per region instead of `region / record_len`.
- **Sequence width**: 3-bit (`Fin 8`) instead of `u32` (modular half-space window
  is `< 4`, sufficient because at most 2 records are appended per region before
  erasing the standby region).
- **Payload**: 1-bit `Bool` instead of arbitrary encoded bytes.
- **CRC-32**: assumed collision-free (any torn write or partial erase yields
  `SlotState.sTorn`, never a spurious `SlotState.sValid`).
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

/-! ## 2. Append-Per-Region Protocol + Modular Order + Full Crash Modes -/

veil module FlashStoreModular

open Lentil

enum FlashPhase = { idle, erasing, writing, written, crashed }
enum SlotState = { sEmpty, sValid, sTorn }

-- Region A (2 append slots: a0, a1)
individual a0St : SlotState
individual a0Seq : Fin 8
individual a0Val : Bool
individual a1St : SlotState
individual a1Seq : Fin 8
individual a1Val : Bool

-- Region B (2 append slots: b0, b1)
individual b0St : SlotState
individual b0Seq : Fin 8
individual b0Val : Bool
individual b1St : SlotState
individual b1Seq : Fin 8
individual b1Val : Bool

-- In-RAM `FlashStore::pos` & `last`
individual activeB : Bool
individual appendSlot1 : Bool
individual posSeq : Fin 8

-- In-flight write state
individual targetB : Bool
individual targetSlot1 : Bool
individual targetSeq : Fin 8
individual pendingValid : Bool
individual pendingVal : Bool

-- Specification / ghost tracking for durability & crash consistency
individual ackedValid : Bool
individual ackedVal : Bool
individual crashConsistent : Bool
individual phase : FlashPhase

#gen_state

set_option maxRecDepth 8192

after_init {
  a0St := sEmpty
  a0Seq := 0
  a0Val := false
  a1St := sEmpty
  a1Seq := 0
  a1Val := false
  b0St := sEmpty
  b0Seq := 0
  b0Val := false
  b1St := sEmpty
  b1Seq := 0
  b1Val := false
  activeB := true
  appendSlot1 := false
  posSeq := 0
  targetB := false
  targetSlot1 := false
  targetSeq := 0
  pendingValid := false
  pendingVal := false
  ackedValid := false
  ackedVal := false
  crashConsistent := true
  phase := idle
}

-- `store.rs:159-177`: scanning stops at the first `sEmpty` or `sTorn` slot in each region.
ghost relation a0Reachable := a0St = sValid
ghost relation a1Reachable := a0St = sValid ∧ a1St = sValid
ghost relation b0Reachable := b0St = sValid
ghost relation b1Reachable := b0St = sValid ∧ b1St = sValid

ghost relation modGt (x y : Fin 8) :=
  (x.val + 8 - y.val) % 8 > 0 ∧ (x.val + 8 - y.val) % 8 < 4

ghost relation aBestValid := a0Reachable
ghost relation aBestIs1 := a1Reachable ∧ modGt a1Seq a0Seq
ghost relation bBestValid := b0Reachable
ghost relation bBestIs1 := b1Reachable ∧ modGt b1Seq b0Seq

ghost relation aBestSeqVal (s : Fin 8) (v : Bool) :=
  (aBestIs1 ∧ a1Seq = s ∧ a1Val = v) ∨ (¬aBestIs1 ∧ a0Reachable ∧ a0Seq = s ∧ a0Val = v)

ghost relation bBestSeqVal (s : Fin 8) (v : Bool) :=
  (bBestIs1 ∧ b1Seq = s ∧ b1Val = v) ∨ (¬bBestIs1 ∧ b0Reachable ∧ b0Seq = s ∧ b0Val = v)

ghost relation scanValIs (v : Bool) :=
  (∃ sb vb, bBestSeqVal sb vb ∧
    (¬aBestValid ∨ (∃ sa va, aBestSeqVal sa va ∧ modGt sb sa)) ∧ vb = v) ∨
  (∃ sa va, aBestSeqVal sa va ∧
    (¬bBestValid ∨ (∃ sb vb, bBestSeqVal sb vb ∧ ¬modGt sb sa)) ∧ va = v)

-- `store.rs:228-230`: saving the already-stored value is an explicit no-op.
action saveUnchanged (v : Bool) {
  require phase = idle
  require ackedValid ∧ ackedVal = v
}

-- `store.rs:246-248`: append directly to slot 1 of the active region (no erase).
action beginSaveAppend (v : Bool) {
  require phase = idle
  require ¬(ackedValid ∧ ackedVal = v)
  require appendSlot1
  targetB := activeB
  targetSlot1 := true
  targetSeq := posSeq + 1
  pendingValid := true
  pendingVal := v
  -- `store.rs:260`: clear `append` before write so a torn write forces switch.
  appendSlot1 := false
  phase := writing
}

-- `store.rs:249-258`: active region is full or torn; erase the standby region.
action beginSaveSwitch (v : Bool) {
  require phase = idle
  require ¬(ackedValid ∧ ackedVal = v)
  require ¬appendSlot1
  targetB := !activeB
  targetSlot1 := false
  targetSeq := posSeq + 1
  pendingValid := true
  pendingVal := v
  phase := erasing
}

-- Erase of standby region completes cleanly (`store.rs:255`).
action eraseOther {
  require phase = erasing
  if targetB then
    b0St := sEmpty
    b0Seq := 0
    b0Val := false
    b1St := sEmpty
    b1Seq := 0
    b1Val := false
  else
    a0St := sEmpty
    a0Seq := 0
    a0Val := false
    a1St := sEmpty
    a1Seq := 0
    a1Val := false
  phase := writing
}

-- Flash write completes validly (`store.rs:263`), BEFORE `save()` updates RAM
-- `pos`/`last` and returns `Ok(())` to the caller (models post-write/pre-ack crash window).
action writeLanded {
  require phase = writing
  if targetB then
    if targetSlot1 then
      b1St := sValid
      b1Seq := targetSeq
      b1Val := pendingVal
    else
      b0St := sValid
      b0Seq := targetSeq
      b0Val := pendingVal
  else
    if targetSlot1 then
      a1St := sValid
      a1Seq := targetSeq
      a1Val := pendingVal
    else
      a0St := sValid
      a0Seq := targetSeq
      a0Val := pendingVal
  phase := written
}

-- Caller receives `Ok(())` and RAM state advances (`store.rs:264-267`).
action ackSave {
  require phase = written
  activeB := targetB
  appendSlot1 := !targetSlot1
  posSeq := targetSeq
  ackedValid := true
  ackedVal := pendingVal
  pendingValid := false
  phase := idle
}

-- Power cut while idle or after `writeLanded` (before `ackSave`: ambiguous commit!).
action powerCutClean {
  require phase = idle ∨ phase = written
  phase := crashed
}

-- Power cut mid-erase: standby region's slot 0 becomes `sTorn` while slot 1 may
-- retain old bytes (partial erase).
action powerCutPartialErase {
  require phase = erasing
  if targetB then
    b0St := sTorn
  else
    a0St := sTorn
  phase := crashed
}

-- Power cut mid-write: target slot becomes `sTorn` (torn tail).
action powerCutTornWrite {
  require phase = writing
  if targetB then
    if targetSlot1 then
      b1St := sTorn
    else
      b0St := sTorn
  else
    if targetSlot1 then
      a1St := sTorn
    else
      a0St := sTorn
  phase := crashed
}

-- Reboot and scan both NOR flash regions (`store.rs:153-195`) using modular order.
-- Verifies crash consistency inline: recovered value must match either the
-- pre-crash `ackedVal` or the pre-crash in-flight `pendingVal`.
action rebootScan {
  require phase = crashed
  let aHas0 : Bool := a0St = sValid
  let aHas1 : Bool := a0St = sValid ∧ a1St = sValid
  let a1Newer : Bool := aHas1 ∧ ((a1Seq.val + 8 - a0Seq.val) % 8 > 0 ∧ (a1Seq.val + 8 - a0Seq.val) % 8 < 4)
  let aSeqBest : Fin 8 := if a1Newer then a1Seq else a0Seq
  let aValBest : Bool := if a1Newer then a1Val else a0Val
  let aCanAppend : Bool := aHas0 ∧ !a1Newer ∧ a1St = sEmpty

  let bHas0 : Bool := b0St = sValid
  let bHas1 : Bool := b0St = sValid ∧ b1St = sValid
  let b1Newer : Bool := bHas1 ∧ ((b1Seq.val + 8 - b0Seq.val) % 8 > 0 ∧ (b1Seq.val + 8 - b0Seq.val) % 8 < 4)
  let bSeqBest : Fin 8 := if b1Newer then b1Seq else b0Seq
  let bValBest : Bool := if b1Newer then b1Val else b0Val
  let bCanAppend : Bool := bHas0 ∧ !b1Newer ∧ b1St = sEmpty

  let pickB : Bool :=
    bHas0 ∧ (!aHas0 ∨ ((bSeqBest.val + 8 - aSeqBest.val) % 8 > 0 ∧ (bSeqBest.val + 8 - aSeqBest.val) % 8 < 4))

  if pickB then
    let okAck : Bool := ackedValid ∧ bValBest = ackedVal
    let okPend : Bool := pendingValid ∧ bValBest = pendingVal
    crashConsistent := crashConsistent ∧ (okAck ∨ okPend)
    activeB := true
    appendSlot1 := bCanAppend
    posSeq := bSeqBest
    ackedValid := true
    ackedVal := bValBest
  else if aHas0 then
    let okAck : Bool := ackedValid ∧ aValBest = ackedVal
    let okPend : Bool := pendingValid ∧ aValBest = pendingVal
    crashConsistent := crashConsistent ∧ (okAck ∨ okPend)
    activeB := false
    appendSlot1 := aCanAppend
    posSeq := aSeqBest
    ackedValid := true
    ackedVal := aValBest
  else
    crashConsistent := crashConsistent ∧ !ackedValid
    activeB := true
    appendSlot1 := false
    posSeq := 0
    ackedValid := false
  pendingValid := false
  phase := idle
}

-- Durability: when idle after an acknowledged save, scanning recovers `ackedVal`.
safety [durability]
  phase = idle ∧ ackedValid → scanValIs ackedVal

-- Crash consistency: every reboot after ANY power cut (clean, partial erase,
-- torn write, post-write/pre-ack, or repeated cuts) recovers either the
-- pre-crash acknowledged value or the pre-crash in-flight pending value.
safety [crash_consistency]
  crashConsistent

-- Recovery progress (Liveness under Weak Fairness of `rebootScan`).
temporal [recovery_progress]
  (𝒲ℱ rebootScan) → □ (⌜ phase = crashed ⌝ → ◇ ⌜ phase = idle ⌝)

#gen_spec

#model_check interpreted { FlashPhase := FlashPhase_IndT, SlotState := SlotState_IndT } {}
#temporal_check [recovery_progress] interpreted { FlashPhase := FlashPhase_IndT, SlotState := SlotState_IndT } {}

end FlashStoreModular
