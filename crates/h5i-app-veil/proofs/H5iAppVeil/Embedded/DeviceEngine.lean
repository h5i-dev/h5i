module

public import Veil
public import H5iAppVeil.KernelBridge
public import H5iAppVeil.Refinement

/-!
# Veil Models & Refinement of `DeviceEngine` (`H5iAppVeil.Embedded.DeviceEngine`)

Specifies `crates/h5i-app-embedded/src/kernel.rs` directly in Veil's DSL and
checks both verification targets using Veil's `#model_check` and Lasso
`#temporal_check`:

* **Target A (Persistence & Visibility Ordering)**:
  - `durable_ack`: once published or notified, flash holds the committed snapshot.
  - `kernel_interlock`: committed snapshot always satisfies `emergency → ¬motorOn`.
  - `flash_interlock`: flash snapshot (even after ambiguous `Store::save` failure)
    always satisfies `emergency → ¬motorOn`.
  - `deadlock_freedom`: under weak fairness of writer steps, every started write
    eventually returns to `idle`.
* **Target B (`changed()` Notification Coalescing & Emergency Stop)**:
  - `DeviceEngineUnlatched`: Veil's `#temporal_check` finds the coalescing lasso
    counterexample where `Normal → Emergency → Normal` happens before
    `actuatorPoll`, violating `□(emergencyRequested → ◇ ¬actuatorMotorOn)`.
  - `DeviceEngineLatched`: clearing emergency is refused until the actuator
    observes the stop; Veil's `#temporal_check` proves
    `□(emergencyRequested → ◇ ¬actuatorMotorOn)` under weak fairness.
* **Kernel Refinement**:
  - `deviceEngine_stutterRefines_motorKernel`: formal `StutterRefines` proof
    connecting the concrete `DeviceEngine` `Veil.RelationalTransitionSystem`
    (projected to `snapshot`) to the abstract `MotorKernel` `kernelRTS`.
-/

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
individual observerSeen : Fin 4
individual actuatorMotorOn : Bool
individual emergencyRequested : Bool
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
  observerSeen := 0
  actuatorMotorOn := true
  emergencyRequested := false
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
  if pendingEmergency then
    emergencyRequested := true
  phase := published
}

action notify {
  require phase = published
  let nextV : Fin 4 := version + 1
  if nextV = observerSeen then
    version := nextV + 1
  else
    version := nextV
  phase := notified
}

action finish {
  require phase = notified
  phase := idle
}

action actuatorPoll {
  require version ≠ observerSeen
  observerSeen := version
  actuatorMotorOn := snapMotorOn
  if !snapMotorOn then
    emergencyRequested := false
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

/-! ## 2. Latched Emergency Protocol — Target A + Target B pass in Veil -/

veil module DeviceEngineLatched

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
individual observerSeen : Fin 4
individual actuatorMotorOn : Bool
individual emergencyRequested : Bool
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
  observerSeen := 0
  actuatorMotorOn := true
  emergencyRequested := false
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
  if pendingEmergency then
    emergencyRequested := true
  phase := published
}

action notify {
  require phase = published
  let nextV : Fin 4 := version + 1
  if nextV = observerSeen then
    version := nextV + 1
  else
    version := nextV
  phase := notified
}

action finish {
  require phase = notified
  phase := idle
}

action actuatorPoll {
  require version ≠ observerSeen
  observerSeen := version
  actuatorMotorOn := snapMotorOn
  if !snapMotorOn then
    emergencyRequested := false
}

action crashAndRestore {
  snapEmergency := flashEmergency
  snapMotorOn := flashMotorOn
  version := 0
  observerSeen := 0
  actuatorMotorOn := flashMotorOn
  if !flashMotorOn then
    emergencyRequested := false
  phase := idle
}

safety [durable_ack]
  (phase = published ∨ phase = notified) →
    (flashEmergency = snapEmergency ∧ flashMotorOn = snapMotorOn)

safety [kernel_interlock]
  snapEmergency → ¬snapMotorOn

safety [flash_interlock]
  flashEmergency → ¬flashMotorOn

temporal [deadlock_freedom]
  (𝒲ℱ compute ∧ 𝒲ℱ beginSave ∧ 𝒲ℱ savePersist ∧ 𝒲ℱ publish ∧ 𝒲ℱ notify ∧ 𝒲ℱ finish) →
    □ (⌜ phase ≠ idle ⌝ → ◇ ⌜ phase = idle ⌝)

temporal [emergency_stop_liveness]
  (𝒲ℱ actuatorPoll ∧ 𝒲ℱ notify ∧ 𝒲ℱ finish) →
    □ (⌜ snapEmergency ⌝ → ◇ ⌜ ¬actuatorMotorOn ⌝)

#gen_spec

#model_check interpreted { EnginePhase := EnginePhase_IndT } {}
#temporal_check [deadlock_freedom] interpreted { EnginePhase := EnginePhase_IndT } {}
#temporal_check [emergency_stop_liveness] interpreted { EnginePhase := EnginePhase_IndT } {}

end DeviceEngineLatched

/-! ## 3. Refinement Proof: Concrete `DeviceEngine` → Abstract `Kernel` RTS -/

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

/-- Concrete runtime state of `DeviceEngine` executing `motorTransition`. -/
structure EngineRTSState where
  snapshot : MotorSnap
  flash : MotorSnap
  pending : Option (MotorCmd × MotorSnap)
  version : Nat
  observerSeen : Nat
  actuatorMotorOn : Bool
deriving DecidableEq, Repr

inductive EngineLabel where
  | compute (cmd : MotorCmd)
  | savePersist
  | saveFailClean
  | publish
  | notify
  | actuatorPoll
deriving DecidableEq, Repr

inductive EngineStep : EngineRTSState → EngineLabel → EngineRTSState → Prop
  | compute {s cmd ws} :
      motorTransition () s.snapshot cmd = .ok (ws, ()) →
      EngineStep s (.compute cmd) { s with pending := some (cmd, motorApply s.snapshot ws) }
  | savePersist {s cmd nxt} :
      s.pending = some (cmd, nxt) →
      EngineStep s .savePersist { s with flash := nxt }
  | saveFailClean {s} :
      EngineStep s .saveFailClean { s with pending := none }
  | publish {s cmd nxt} :
      s.pending = some (cmd, nxt) →
      s.flash = nxt →
      EngineStep s .publish { s with snapshot := nxt, pending := none }
  | notify {s} :
      EngineStep s .notify { s with version := s.version + 1 }
  | actuatorPoll {s} :
      s.version ≠ s.observerSeen →
      EngineStep s .actuatorPoll
        { s with observerSeen := s.version, actuatorMotorOn := s.snapshot.motorOn }

def engineRTS : Veil.RelationalTransitionSystem Unit EngineRTSState EngineLabel where
  assumptions := fun _ => True
  init := fun _ s =>
    s = { snapshot := motorInit, flash := motorInit, pending := none,
          version := 0, observerSeen := 0, actuatorMotorOn := true }
  tr := fun _ s lbl s' => EngineStep s lbl s'

/-- Every pending state in `engineRTS` was produced by a valid `motorTransition`
step from the current committed `snapshot`. -/
def PendingValid (s : EngineRTSState) : Prop :=
  ∀ cmd nxt, s.pending = some (cmd, nxt) →
    ∃ ws, motorTransition () s.snapshot cmd = .ok (ws, ()) ∧ nxt = motorApply s.snapshot ws

theorem engineRTS_pendingValid {s : EngineRTSState} (hr : engineRTS.reachable () s) :
    PendingValid s := by
  induction hr with
  | init s _ hinit =>
    subst hinit
    intro cmd nxt hp
    cases hp
  | step s s' _ hnext ih =>
    obtain ⟨lbl, hstep⟩ := hnext
    cases hstep with
    | compute ht =>
      intro cmd' nxt' hp
      cases hp
      exact ⟨_, ht, rfl⟩
    | savePersist _ => exact ih
    | saveFailClean =>
      intro cmd' nxt' hp
      cases hp
    | publish _ _ =>
      intro cmd' nxt' hp
      cases hp
    | notify => exact ih
    | actuatorPoll _ => exact ih

/-- **Refinement theorem**: the concrete `DeviceEngine` transition system
stuttering-refines the abstract `motorKernelRTS` via the committed snapshot
projection `EngineRTSState.snapshot`. -/
theorem deviceEngine_stutterRefines_motorKernel :
    StutterRefines id EngineRTSState.snapshot engineRTS motorKernelRTS where
  assumptions_map := fun _ h => h
  init_map := fun _ s _ hinit => by
    subst hinit
    rfl
  step_map := fun _ s s' hr hnext => by
    obtain ⟨lbl, hstep⟩ := hnext
    cases hstep with
    | compute _ => exact Or.inr rfl
    | savePersist _ => exact Or.inr rfl
    | saveFailClean => exact Or.inr rfl
    | @publish cmd nxt hp _ =>
      obtain ⟨ws, ht, hnxt⟩ := engineRTS_pendingValid hr cmd nxt hp
      subst hnxt
      left
      exact ⟨.ok () cmd (), KernelStep.ok ht⟩
    | notify => exact Or.inr rfl
    | actuatorPoll _ => exact Or.inr rfl

end H5iAppVeil.Embedded.DeviceEngine
