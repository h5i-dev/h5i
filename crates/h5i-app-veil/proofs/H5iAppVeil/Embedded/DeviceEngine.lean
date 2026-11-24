module

public import Veil
public import H5iAppVeil.KernelBridge
public import H5iAppVeil.Refinement

/-!
# Veil Models & Refinement of `DeviceEngine` (`H5iAppVeil.Embedded.DeviceEngine`)

Specifies `crates/h5i-app-embedded/src/kernel.rs` directly in Veil's DSL and
checks both verification targets using Veil's `#model_check` and Lasso
`#temporal_check`, then proves `StutterRefines` directly on the **exact same**
Veil-generated `DeviceEngineLatched.relationalTransitionSystem` (no separate
handwritten transition system).

## Modeling Fidelity & Explicit Abstractions

1. **Real wrapping version counter (`kernel.rs:167`)**: `notify` executes plain
   wrapping addition `version := version + 1` on `Fin 4` — it does **not**
   artificially skip `observerSeen`. (Abstracts `u32::wrapping_add(1)` to 2 bits.)
2. **Multiple observers (`MultiWakerRegistration<4>`)**: two independent
   observers (`actuatorSeen` and `monitorSeen`) poll `changed()` asynchronously.
3. **Separated crash, restore, and actuator output**:
   - `powerLoss`: resets RAM (`snap` returns to initial, `version := 0`,
     observers reset `seen := 0`, `phase := booting`), while **leaving**
     `actuatorMotorOn` untouched (modeling an external relay/contactor that
     keeps its physical state across a brief MCU reset until explicitly driven).
   - `restore`: mirrors `DeviceEngine::restore()` (`kernel.rs:111-121`), loading
     `flash` into `snap`, calling `notify()` (`version := version + 1`), and
     returning to `idle` — **without** atomically updating `actuatorMotorOn`.
   - `actuatorPoll`: separately observes `version ≠ actuatorSeen` via `changed()`
     and drives `actuatorMotorOn := snapMotorOn`.
4. **Remaining abstractions**:
   - Single-writer serialization (`store: Mutex<...>`) is modeled by `phase`.
   - `version : Fin 4` compresses `u32` (wraparound happens after 4 commits
     instead of `2^32`). In `DeviceEngineLatched`, latching bounds uncommitted
     emergency-clear cycles to 1 commit while `actuatorMotorOn = true`, so
     even 2-bit wrapping never misses an emergency stop.
   - Liveness assumes the MCU is not stuck in an infinite power-loss reboot loop
     (`◇ □ ⌜ phase ≠ booting ⌝`) alongside weak fairness of runtime steps.
-/

@[expose] public section

namespace H5iAppVeil.Embedded.DeviceEngine

/-- Abstract motor snapshot matching `crates/h5i-app-embedded/src/replay.rs::MotorSnap`. -/
structure MotorSnap where
  emergency : Bool
  motorOn : Bool
deriving DecidableEq, Repr

inductive MotorCmd where
  | requestEmergency
  | clearEmergency
deriving DecidableEq, Repr

def motorTransition (_ : Unit) (_ : MotorSnap) : MotorCmd → Except Unit (MotorSnap × Unit)
  | .requestEmergency => .ok ({ emergency := true, motorOn := false }, ())
  | .clearEmergency => .ok ({ emergency := false, motorOn := true }, ())

def motorApply (_ : MotorSnap) (ws : MotorSnap) : MotorSnap := ws

def motorInit : MotorSnap := { emergency := false, motorOn := true }

/-- Abstract kernel RTS built via `H5iAppVeil.kernelRTS`. -/
def motorKernelRTS :
    Veil.RelationalTransitionSystem Unit MotorSnap (KernelEvent Unit MotorCmd Unit Unit) :=
  kernelRTS motorTransition id motorApply motorInit

end H5iAppVeil.Embedded.DeviceEngine

end

/-! ## 1. Unlatched Emergency Protocol — Veil finds `changed()` coalescing bug -/

veil module DeviceEngineUnlatched

open Lentil

enum EnginePhase = { idle, locked, computed, saving, persisted, published, notified }

individual snapEmergency : Bool
individual snapMotorOn : Bool
individual flashEmergency : Bool
individual flashMotorOn : Bool
individual pendingEmergency : Bool
individual pendingMotorOn : Bool
individual reqEmergency : Bool
individual version : Fin 4
individual actuatorSeen : Fin 4
individual monitorSeen : Fin 4
individual actuatorMotorOn : Bool
individual phase : EnginePhase

#gen_state

after_init {
  snapEmergency := false
  snapMotorOn := true
  flashEmergency := false
  flashMotorOn := true
  pendingEmergency := false
  pendingMotorOn := true
  reqEmergency := false
  version := 0
  actuatorSeen := 0
  monitorSeen := 0
  actuatorMotorOn := true
  phase := idle
}

action startEmergency {
  require phase = idle
  require ¬snapEmergency
  reqEmergency := true
  phase := locked
}

action startClearEmergency {
  require phase = idle
  require snapEmergency
  reqEmergency := false
  phase := locked
}

action compute {
  require phase = locked
  pendingEmergency := reqEmergency
  pendingMotorOn := !reqEmergency
  phase := computed
}

action beginSave {
  require phase = computed
  phase := saving
}

action savePersist {
  require phase = saving
  flashEmergency := pendingEmergency
  flashMotorOn := pendingMotorOn
  phase := persisted
}

action publish {
  require phase = persisted
  snapEmergency := pendingEmergency
  snapMotorOn := pendingMotorOn
  phase := published
}

-- Real wrapping `w.version.wrapping_add(1)` (`kernel.rs:167`), no artificial skip.
action notify {
  require phase = published
  version := version + 1
  phase := notified
}

action finish {
  require phase = notified
  phase := idle
}

action actuatorPoll {
  require version ≠ actuatorSeen
  actuatorSeen := version
  actuatorMotorOn := snapMotorOn
}

action monitorPoll {
  require version ≠ monitorSeen
  monitorSeen := version
}

safety [durable_ack]
  (phase = published ∨ phase = notified) →
    (flashEmergency = snapEmergency ∧ flashMotorOn = snapMotorOn)

safety [kernel_interlock]
  snapEmergency → ¬snapMotorOn

-- Target B: notification coalescing violates emergency stop liveness when unlatched.
temporal [emergency_stop_liveness]
  (𝒲ℱ actuatorPoll ∧ 𝒲ℱ notify ∧ 𝒲ℱ finish) →
    □ (⌜ snapEmergency ⌝ → ◇ ⌜ ¬actuatorMotorOn ⌝)

#gen_spec

#model_check interpreted { EnginePhase := EnginePhase_IndT } {}

-- Expected violation: Veil's Lasso temporal checker discovers the coalescing lasso.
set_option veil.violationIsError false in
#temporal_check [emergency_stop_liveness] interpreted { EnginePhase := EnginePhase_IndT } {}

end DeviceEngineUnlatched

/-! ## 2. Latched Emergency Protocol — Target A + Target B + Direct Refinement Proof -/

veil module DeviceEngineLatched

open Lentil

enum EnginePhase = { idle, locked, computed, saving, persisted, published, notified, booting }

individual snapEmergency : Bool
individual snapMotorOn : Bool
individual flashEmergency : Bool
individual flashMotorOn : Bool
individual pendingEmergency : Bool
individual pendingMotorOn : Bool
individual reqEmergency : Bool
individual version : Fin 4
individual actuatorSeen : Fin 4
individual monitorSeen : Fin 4
individual actuatorMotorOn : Bool
individual phase : EnginePhase

#gen_state

after_init {
  snapEmergency := false
  snapMotorOn := true
  flashEmergency := false
  flashMotorOn := true
  pendingEmergency := false
  pendingMotorOn := true
  reqEmergency := false
  version := 0
  actuatorSeen := 0
  monitorSeen := 0
  actuatorMotorOn := true
  phase := idle
}

action startEmergency {
  require phase = idle
  require ¬snapEmergency
  reqEmergency := true
  phase := locked
}

-- Latched protocol: clearing emergency is refused until the actuator has
-- observed the stop (`¬actuatorMotorOn`).
action startClearEmergency {
  require phase = idle
  require snapEmergency
  require ¬actuatorMotorOn
  reqEmergency := false
  phase := locked
}

action compute {
  require phase = locked
  pendingEmergency := reqEmergency
  pendingMotorOn := !reqEmergency
  phase := computed
}

action beginSave {
  require phase = computed
  phase := saving
}

action savePersist {
  require phase = saving
  flashEmergency := pendingEmergency
  flashMotorOn := pendingMotorOn
  phase := persisted
}

action saveFailClean {
  require phase = saving
  phase := idle
}

-- `Store::save` contract: a failed/cancelled save may still land in flash while
-- returning `Err` (in-memory snapshot stays unchanged until reboot).
action saveFailAmbiguous {
  require phase = saving
  flashEmergency := pendingEmergency
  flashMotorOn := pendingMotorOn
  phase := idle
}

action publish {
  require phase = persisted
  snapEmergency := pendingEmergency
  snapMotorOn := pendingMotorOn
  phase := published
}

-- Real wrapping `w.version.wrapping_add(1)` (`kernel.rs:167`), no artificial skip.
action notify {
  require phase = published
  version := version + 1
  phase := notified
}

action finish {
  require phase = notified
  phase := idle
}

-- Observer 1: actuator task (`changed()`).
action actuatorPoll {
  require phase ≠ booting
  require version ≠ actuatorSeen
  actuatorSeen := version
  actuatorMotorOn := snapMotorOn
}

-- Observer 2: independent telemetry/monitor task (`MultiWakerRegistration<4>`).
action monitorPoll {
  require phase ≠ booting
  require version ≠ monitorSeen
  monitorSeen := version
}

-- MCU reset / brownout: RAM resets to initial snapshot and version 0, while
-- `actuatorMotorOn` is NOT atomically changed (external contactor may stay energized
-- until `restore` + `actuatorPoll` explicitly drive the GPIO pin).
action powerLoss {
  require phase ≠ booting
  snapEmergency := false
  snapMotorOn := true
  version := 0
  actuatorSeen := 0
  monitorSeen := 0
  phase := booting
}

-- `DeviceEngine::restore()` (`kernel.rs:111-121`): loads flash into `snap`,
-- calls `notify()` (`version := version + 1`), and enters `idle` WITHOUT
-- directly mutating `actuatorMotorOn`.
action restore {
  require phase = booting
  snapEmergency := flashEmergency
  snapMotorOn := flashMotorOn
  version := version + 1
  phase := idle
}

safety [durable_ack]
  (phase = published ∨ phase = notified) →
    (flashEmergency = snapEmergency ∧ flashMotorOn = snapMotorOn)

safety [kernel_interlock]
  snapEmergency → ¬snapMotorOn

safety [flash_interlock]
  flashEmergency → ¬flashMotorOn

safety [snap_consistent]
  snapMotorOn = !snapEmergency

safety [flash_consistent]
  flashMotorOn = !flashEmergency

temporal [deadlock_freedom]
  (𝒲ℱ compute ∧ 𝒲ℱ beginSave ∧ 𝒲ℱ savePersist ∧ 𝒲ℱ publish ∧ 𝒲ℱ notify ∧ 𝒲ℱ finish ∧ 𝒲ℱ restore ∧ ◇ □ ⌜ phase ≠ booting ⌝) →
    □ (⌜ phase ≠ idle ⌝ → ◇ ⌜ phase = idle ⌝)

temporal [emergency_stop_liveness]
  (𝒲ℱ actuatorPoll ∧ 𝒲ℱ notify ∧ 𝒲ℱ finish ∧ 𝒲ℱ restore ∧ ◇ □ ⌜ phase ≠ booting ⌝) →
    □ (⌜ snapEmergency ⌝ → ◇ ⌜ ¬actuatorMotorOn ⌝)

#gen_spec

#model_check interpreted { EnginePhase := EnginePhase_IndT } {}
#temporal_check [deadlock_freedom] interpreted { EnginePhase := EnginePhase_IndT } {}
#temporal_check [emergency_stop_liveness] interpreted { EnginePhase := EnginePhase_IndT } {}

-- Directly prove stuttering refinement on the EXACT Veil-generated
-- `relationalTransitionSystem` checked above (zero duplicate handwritten RTS).
open H5iAppVeil H5iAppVeil.Embedded.DeviceEngine in
def projectSnap {EnginePhase : Type} (s : State (FieldAbstractType EnginePhase)) : MotorSnap :=
  { emergency := s.snapEmergency, motorOn := s.snapMotorOn }

private def Consistent {EnginePhase : Type} (s : State (FieldAbstractType EnginePhase)) : Prop :=
  s.snapMotorOn = !s.snapEmergency ∧
  s.flashMotorOn = !s.flashEmergency ∧
  s.pendingMotorOn = !s.pendingEmergency

private theorem consistent_reachable
    {EnginePhase : Type} [Inhabited EnginePhase] [DecidableEq EnginePhase] [EnginePhase_EnumClass EnginePhase]
    {th : Theory EnginePhase} {s : State (FieldAbstractType EnginePhase)}
    (hr : (relationalTransitionSystem EnginePhase).reachable th s) :
    Consistent s := by
  induction hr with
  | init s _ hinit =>
    revert hinit
    simp only [nextSimp, actSimp]
    veil_wp
    intro hinit
    rcases hinit with ⟨_, rfl⟩
    simp [Consistent]
  | step s s' _ hnext ih =>
    obtain ⟨hsnap, hflash, hpend⟩ := ih
    obtain ⟨lbl, htr⟩ := hnext
    cases lbl <;> simp only [nextSimp] at htr
    all_goals (rcases htr with ⟨_, _, _, _, rfl⟩; simp [Consistent, hsnap, hflash, hpend])

open H5iAppVeil H5iAppVeil.Embedded.DeviceEngine in
/-- **Refinement theorem**: the **exact** Veil-generated
`DeviceEngineLatched.relationalTransitionSystem` that `#model_check` and
`#temporal_check` verify above stuttering-refines `motorKernelRTS` via
`projectSnap`. -/
theorem stutterRefines_motorKernel
    (EnginePhase : Type) [Inhabited EnginePhase] [DecidableEq EnginePhase] [EnginePhase_EnumClass EnginePhase] :
    StutterRefines (fun _ => ()) (projectSnap (EnginePhase := EnginePhase))
      (relationalTransitionSystem EnginePhase)
      motorKernelRTS where
  assumptions_map := fun _ _ => trivial
  init_map := fun _ s _ hinit => by
    revert hinit
    simp only [nextSimp, actSimp]
    veil_wp
    intro hinit
    rcases hinit with ⟨_, rfl⟩
    simp [motorKernelRTS, kernelRTS, projectSnap, motorInit]
  step_map := fun th s s' hr hnext => by
    obtain ⟨_, hflash, hpend⟩ := consistent_reachable hr
    obtain ⟨lbl, htr⟩ := hnext
    cases lbl with
    | publish =>
      simp only [nextSimp] at htr
      rcases htr with ⟨_, _, rfl⟩
      left
      cases hpe : s.pendingEmergency
      · refine ⟨.ok () .clearEmergency (), ?_⟩
        have hpm : s.pendingMotorOn = true := by simp [hpend, hpe]
        simpa [motorKernelRTS, kernelRTS, projectSnap, motorApply, hpe, hpm] using
          (KernelStep.ok (s := projectSnap s) (toSt := id) (applyAll := motorApply)
            (rfl : motorTransition () (projectSnap s) .clearEmergency = .ok ({ emergency := false, motorOn := true }, ())))
      · refine ⟨.ok () .requestEmergency (), ?_⟩
        have hpm : s.pendingMotorOn = false := by simp [hpend, hpe]
        simpa [motorKernelRTS, kernelRTS, projectSnap, motorApply, hpe, hpm] using
          (KernelStep.ok (s := projectSnap s) (toSt := id) (applyAll := motorApply)
            (rfl : motorTransition () (projectSnap s) .requestEmergency = .ok ({ emergency := true, motorOn := false }, ())))
    | powerLoss =>
      simp only [nextSimp] at htr
      rcases htr with ⟨_, _, rfl⟩
      left
      refine ⟨.ok () .clearEmergency (), ?_⟩
      simpa [motorKernelRTS, kernelRTS, projectSnap, motorApply] using
        (KernelStep.ok (s := projectSnap s) (toSt := id) (applyAll := motorApply)
          (rfl : motorTransition () (projectSnap s) .clearEmergency = .ok ({ emergency := false, motorOn := true }, ())))
    | restore =>
      simp only [nextSimp] at htr
      rcases htr with ⟨_, _, rfl⟩
      left
      cases hfe : s.flashEmergency
      · refine ⟨.ok () .clearEmergency (), ?_⟩
        have hfm : s.flashMotorOn = true := by simp [hflash, hfe]
        simpa [motorKernelRTS, kernelRTS, projectSnap, motorApply, hfe, hfm] using
          (KernelStep.ok (s := projectSnap s) (toSt := id) (applyAll := motorApply)
            (rfl : motorTransition () (projectSnap s) .clearEmergency = .ok ({ emergency := false, motorOn := true }, ())))
      · refine ⟨.ok () .requestEmergency (), ?_⟩
        have hfm : s.flashMotorOn = false := by simp [hflash, hfe]
        simpa [motorKernelRTS, kernelRTS, projectSnap, motorApply, hfe, hfm] using
          (KernelStep.ok (s := projectSnap s) (toSt := id) (applyAll := motorApply)
            (rfl : motorTransition () (projectSnap s) .requestEmergency = .ok ({ emergency := true, motorOn := false }, ())))
    | _ =>
      simp only [nextSimp] at htr
      right
      rcases htr with ⟨_, _, _, _, rfl⟩
      rfl

end DeviceEngineLatched
