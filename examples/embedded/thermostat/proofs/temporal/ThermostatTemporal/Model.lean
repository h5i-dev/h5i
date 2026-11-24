module

public import Veil
public import H5iAppVeil

/-!
# Optional Veil Temporal Verification for `examples/embedded/thermostat`

Complements the Aeneas safety proofs in `../Proofs.lean` (`transition_total`,
`inv_reachable`, `interlock`) with temporal logic verification of the
multi-task closed loop in `firmware/src/main.rs` (`sense`, `actuate`, HTTP
admin commands over `DeviceEngine`):

1. **Veil Closed-Loop Model (`ThermostatClosedLoop`)**:
   - Models the thermostat kernel (`Mode::{Off, Heat, Auto}`, `heater_on`,
     `settle`), the `sense` task (TMP108 readings + fail-safe `MAX_TEMP_DC`
     report when the sensor fails), and the `actuate` task (`DeviceEngine::changed`
     driving the physical PD12 heater GPIO pin).
   - Exhaustively checks safety invariants via Veil **`#model_check`**:
     * `kernel_interlock`: `snapHeater → snapTemp ≠ overtemp ∧ snapMode ≠ off`
     * `actuator_sync`: `version = actuatorSeen → pinHeater = snapHeater`
     * `auto_matches_setpoint`: in `auto` mode, `snapHeater` holds only when
       temperature is strictly below setpoint.
   - Exhaustively checks temporal liveness & fail-safe properties under Lentil
     weak fairness (`𝒲ℱ`) via Veil's Lasso **`#temporal_check`**:
     * `overtemp_eventually_stops_pin`: persistent physical overheating
       eventually turns the physical PD12 heater pin off and keeps it off.
     * `sensor_failure_fails_safe`: persistent I2C sensor failure triggers the
       fail-safe `MAX_TEMP_DC` report and eventually turns the physical heater
       pin off.
     * `off_mode_stops_pin`: switching to `Mode::Off` eventually turns the
       physical heater pin off.
     * `auto_heats_when_cold`: staying cold in `Mode::Auto` with a healthy
       sensor eventually turns the physical heater pin on.
2. **Kernel Bridge & Refinement (`ThermostatTemporal.KernelBridge`)**:
   - Connects the full-precision thermostat kernel (`Int` temperatures,
     `MIN_SETPOINT_DC = 50`, `MAX_SETPOINT_DC = 300`, `MAX_TEMP_DC = 350`,
     `RELAYS = 4`) to `H5iAppVeil.kernelRTS`, lifts `Inv` to Lentil's
     `□ ⌜ Inv ⌝` via `H5iAppVeil.lentil_always_of_run_inv`, and proves
     `StutterRefines` from the concrete `DeviceEngine` + actuator runtime
     transition system to the abstract `thermostatKernelRTS`.
-/

/-! ## 1. Closed-Loop Thermostat + `DeviceEngine` Veil Model -/

veil module ThermostatClosedLoop

open Lentil

enum HeatMode = { mOff, mHeat, mAuto }

-- Abstract temperature bands partitioning the kernel's exact decision regions:
--   cold     : temp_dc < 150  (below both spNormal and spWarm)
--   normal   : 150 ≤ temp_dc < 250  (at spNormal, below spWarm)
--   warm     : 250 ≤ temp_dc < 350  (at/above spWarm, below MAX_TEMP_DC)
--   overtemp : temp_dc ≥ 350  (≥ MAX_TEMP_DC, hardware interlock active)
enum TempBand = { cold, normal, warm, overtemp }

enum SetpointBand = { spNormal, spWarm }

-- Physical environment & TMP108 I2C sensor health
individual physTemp : TempBand
individual sensorFailed : Bool

-- Committed kernel Snapshot (`thermostat_kernel::Snapshot`)
individual snapMode : HeatMode
individual snapTemp : TempBand
individual snapSetpoint : SetpointBand
individual snapHeater : Bool

-- `DeviceEngine` change-notification flag (`version ≠ seen`) & PD12 GPIO pin
individual notifyPending : Bool
individual pinHeater : Bool

#gen_state

after_init {
  physTemp := normal
  sensorFailed := false
  snapMode := mOff
  snapTemp := normal
  snapSetpoint := spNormal
  snapHeater := false
  notifyPending := false
  pinHeater := false
}

-- Admin HTTP command: `Command::SetMode { mode: Mode::Off }`
action setModeOff {
  require snapMode ≠ mOff
  snapMode := mOff
  snapHeater := false
  notifyPending := true
}

-- Admin HTTP command: `Command::SetMode { mode: Mode::Heat }`
action setModeHeat {
  require snapMode ≠ mHeat
  snapMode := mHeat
  if snapTemp = overtemp then
    snapHeater := false
  else
    snapHeater := true
  notifyPending := true
}

-- Admin HTTP command: `Command::SetMode { mode: Mode::Auto }`
action setModeAuto {
  require snapMode ≠ mAuto
  snapMode := mAuto
  if snapTemp = overtemp then
    snapHeater := false
  else if snapTemp = cold then
    snapHeater := true
  else if snapSetpoint = spWarm ∧ snapTemp = normal then
    snapHeater := true
  else
    snapHeater := false
  notifyPending := true
}

-- Admin HTTP command: `Command::SetSetpoint` (low valid setpoint)
action setSetpointNormal {
  require snapSetpoint ≠ spNormal
  snapSetpoint := spNormal
  if snapTemp = overtemp then
    snapHeater := false
  else if snapMode = mOff then
    snapHeater := false
  else if snapMode = mHeat then
    snapHeater := true
  else if snapTemp = cold then
    snapHeater := true
  else
    snapHeater := false
  notifyPending := true
}

-- Admin HTTP command: `Command::SetSetpoint` (high valid setpoint)
action setSetpointWarm {
  require snapSetpoint ≠ spWarm
  snapSetpoint := spWarm
  if snapTemp = overtemp then
    snapHeater := false
  else if snapMode = mOff then
    snapHeater := false
  else if snapMode = mHeat then
    snapHeater := true
  else if snapTemp = cold ∨ snapTemp = normal then
    snapHeater := true
  else
    snapHeater := false
  notifyPending := true
}

-- `sense` task (`firmware/src/main.rs:120`): reads TMP108 and reports the
-- reading when it differs from the committed snapshot; an unreadable sensor
-- reports `overtemp` (`MAX_TEMP_DC`), engaging the interlock.
action senseTick {
  let reported : TempBand := if sensorFailed then overtemp else physTemp
  require snapTemp ≠ reported
  snapTemp := reported
  if reported = overtemp then
    snapHeater := false
  else if snapMode = mOff then
    snapHeater := false
  else if snapMode = mHeat then
    snapHeater := true
  else if reported = cold then
    snapHeater := true
  else if snapSetpoint = spWarm ∧ reported = normal then
    snapHeater := true
  else
    snapHeater := false
  notifyPending := true
}

-- `actuate` task (`firmware/src/main.rs:147`): awaits `DeviceEngine::changed`
-- and drives the PD12 heater pin from the latest committed snapshot.
action actuatePoll {
  require notifyPending
  notifyPending := false
  pinHeater := snapHeater
}

-- Environment: physical temperature drifts up or down by one band.
action envTempRise {
  require physTemp ≠ overtemp
  if physTemp = cold then
    physTemp := normal
  else if physTemp = normal then
    physTemp := warm
  else
    physTemp := overtemp
}

action envTempCool {
  require physTemp ≠ cold
  if physTemp = overtemp then
    physTemp := warm
  else if physTemp = warm then
    physTemp := normal
  else
    physTemp := cold
}

-- Environment: TMP108 I2C sensor fails or recovers.
action sensorFail {
  require ¬sensorFailed
  sensorFailed := true
}

action sensorRecover {
  require sensorFailed
  sensorFailed := false
}

-- Safety 1: Hardware interlock on the committed kernel snapshot.
safety [kernel_interlock]
  snapHeater → (snapTemp ≠ overtemp ∧ snapMode ≠ mOff)

-- Safety 2: Whenever no change notification is pending, PD12 matches the kernel.
safety [actuator_sync]
  ¬notifyPending → pinHeater = snapHeater

-- Safety 3: In `auto` mode, the heater runs only when temperature is below setpoint.
safety [auto_matches_setpoint]
  (snapMode = mAuto ∧ snapHeater) →
    ((snapSetpoint = spNormal ∧ snapTemp = cold) ∨
     (snapSetpoint = spWarm ∧ (snapTemp = cold ∨ snapTemp = normal)))

-- Temporal 1 (Overheat fail-safe liveness): if physical temperature stays at
-- `overtemp`, weak fairness of `sense` and `actuate` guarantees the physical
-- PD12 heater pin eventually turns off and stays off.
temporal [overtemp_eventually_stops_pin]
  (𝒲ℱ senseTick ∧ 𝒲ℱ actuatePoll) →
    (◇ □ ⌜ physTemp = overtemp ⌝ → ◇ □ ⌜ ¬pinHeater ⌝)

-- Temporal 2 (Sensor fault fail-safe liveness): if the TMP108 sensor stays
-- unreadable, `sense` reports `MAX_TEMP_DC` and the physical heater pin
-- eventually turns off and stays off.
temporal [sensor_failure_fails_safe]
  (𝒲ℱ senseTick ∧ 𝒲ℱ actuatePoll) →
    (◇ □ ⌜ sensorFailed ⌝ → ◇ □ ⌜ ¬pinHeater ⌝)

-- Temporal 3 (Off-mode responsiveness): once switched to `Mode::Off` and kept
-- off, weak fairness of `actuate` guarantees the physical heater pin eventually
-- turns off and stays off.
temporal [off_mode_stops_pin]
  (𝒲ℱ actuatePoll) →
    (◇ □ ⌜ snapMode = mOff ⌝ → ◇ □ ⌜ ¬pinHeater ⌝)

-- Temporal 4 (Auto-mode heating responsiveness): if kept in `Mode::Auto` while
-- cold with a healthy sensor, the physical heater pin eventually turns on and
-- stays on.
temporal [auto_heats_when_cold]
  (𝒲ℱ senseTick ∧ 𝒲ℱ actuatePoll) →
    (◇ □ ⌜ snapMode = mAuto ∧ physTemp = cold ∧ ¬sensorFailed ⌝ → ◇ □ ⌜ pinHeater ⌝)

#gen_spec

#model_check interpreted { HeatMode := HeatMode_IndT, TempBand := TempBand_IndT, SetpointBand := SetpointBand_IndT } {}
#temporal_check [overtemp_eventually_stops_pin] interpreted { HeatMode := HeatMode_IndT, TempBand := TempBand_IndT, SetpointBand := SetpointBand_IndT } {}
#temporal_check [sensor_failure_fails_safe] interpreted { HeatMode := HeatMode_IndT, TempBand := TempBand_IndT, SetpointBand := SetpointBand_IndT } {}
#temporal_check [off_mode_stops_pin] interpreted { HeatMode := HeatMode_IndT, TempBand := TempBand_IndT, SetpointBand := SetpointBand_IndT } {}
#temporal_check [auto_heats_when_cold] interpreted { HeatMode := HeatMode_IndT, TempBand := TempBand_IndT, SetpointBand := SetpointBand_IndT } {}

end ThermostatClosedLoop

/-! ## 2. Full-Precision Kernel Bridge & Refinement (`H5iAppVeil`) -/

@[expose] public section

namespace ThermostatTemporal.KernelBridge

open H5iAppVeil

def minSetpointDc : Int := 50
def maxSetpointDc : Int := 300
def maxTempDc : Int := 350
def relaysCount : Nat := 4

structure Device where
  admin : Bool
deriving DecidableEq, Repr

inductive KMode where
  | off
  | heat
  | auto
deriving DecidableEq, Repr

structure Snapshot where
  setpointDc : Int
  tempDc : Int
  heater : Bool
  relays : Fin relaysCount → Bool
  mode : KMode

inductive Command where
  | setSetpoint (dc : Int)
  | reportTemp (dc : Int)
  | setRelay (idx : Nat) (on : Bool)
  | setMode (mode : KMode)
  | read
deriving DecidableEq, Repr

inductive Error where
  | setpointOutOfRange
  | noSuchRelay
  | forbidden
deriving DecidableEq, Repr

def initialSnapshot : Snapshot where
  setpointDc := 200
  tempDc := 200
  heater := false
  relays := fun _ => false
  mode := .off

def heaterOn (m : KMode) (tempDc setpointDc : Int) : Bool :=
  if tempDc ≥ maxTempDc then
    false
  else
    match m with
    | .off => false
    | .heat => true
    | .auto => decide (tempDc < setpointDc)

def settle (s : Snapshot) : Snapshot :=
  { s with heater := heaterOn s.mode s.tempDc s.setpointDc }

/-- Pure Lean specification of `examples/embedded/thermostat/kernel/src/lib.rs::transition`. -/
def thermostatTransition (actor : Device) (s : Snapshot) :
    Command → Except Error (Option Snapshot × Snapshot)
  | .read => .ok (none, s)
  | .reportTemp dc =>
      let n := settle { s with tempDc := dc }
      .ok (some n, n)
  | .setSetpoint dc =>
      if !actor.admin then
        .error .forbidden
      else if dc < minSetpointDc ∨ dc > maxSetpointDc then
        .error .setpointOutOfRange
      else
        let n := settle { s with setpointDc := dc }
        .ok (some n, n)
  | .setRelay idx on =>
      if !actor.admin then
        .error .forbidden
      else if idx < relaysCount then
        let n := { s with relays := fun i => if i.val = idx then on else s.relays i }
        .ok (some n, n)
      else
        .error .noSuchRelay
  | .setMode m =>
      if !actor.admin then
        .error .forbidden
      else
        let n := settle { s with mode := m }
        .ok (some n, n)

def applyW (s : Snapshot) : Option Snapshot → Snapshot
  | none => s
  | some n => n

/-- Abstract `Veil.RelationalTransitionSystem` derived directly from `thermostatTransition`
+ `applyW` via `H5iAppVeil.kernelRTS`. -/
def thermostatKernelRTS :
    Veil.RelationalTransitionSystem Unit Snapshot (KernelEvent Device Command Snapshot Error) :=
  kernelRTS thermostatTransition id applyW initialSnapshot

/-- Safety invariant matching `examples/embedded/thermostat/proofs/Proofs.lean::Inv`. -/
def Inv (s : Snapshot) : Prop :=
  (s.heater = true → s.tempDc < maxTempDc) ∧ minSetpointDc ≤ s.setpointDc ∧ s.setpointDc ≤ maxSetpointDc

private theorem heaterOn_interlock (m : KMode) (t sp : Int) (h : heaterOn m t sp = true) :
    t < maxTempDc := by
  unfold heaterOn at h
  by_cases hge : t ≥ maxTempDc
  · simp [hge] at h
  · omega

theorem inv_initial : Inv initialSnapshot := by
  simp [Inv, initialSnapshot, minSetpointDc, maxSetpointDc]

theorem inv_step {a : Device} {s : Snapshot} {c : Command} {ws : Option Snapshot} {r : Snapshot}
    (hs : Inv s) (ht : thermostatTransition a s c = .ok (ws, r)) :
    Inv (applyW s ws) := by
  obtain ⟨hh, hlo, hhi⟩ := hs
  cases c with
  | read =>
    cases ht
    exact ⟨hh, hlo, hhi⟩
  | reportTemp dc =>
    cases ht
    exact ⟨fun h => heaterOn_interlock _ _ _ h, hlo, hhi⟩
  | setSetpoint dc =>
    unfold thermostatTransition at ht
    by_cases hadm : a.admin = true
    · simp only [hadm, Bool.not_true, Bool.false_eq_true, ↓reduceIte] at ht
      by_cases hbad : dc < minSetpointDc ∨ dc > maxSetpointDc
      · simp [hbad] at ht
      · simp only [hbad, ↓reduceIte, Except.ok.injEq, Prod.mk.injEq] at ht
        rcases ht with ⟨rfl, _⟩
        refine ⟨fun h => heaterOn_interlock _ _ _ h, ?_, ?_⟩ <;> (simp [applyW, settle]; omega)
    · have hfalse : a.admin = false := by cases h : a.admin <;> simp_all
      simp [hfalse] at ht
  | setRelay idx on =>
    unfold thermostatTransition at ht
    by_cases hadm : a.admin = true
    · simp only [hadm, Bool.not_true, Bool.false_eq_true, ↓reduceIte] at ht
      by_cases hlt : idx < relaysCount
      · simp only [hlt, ↓reduceIte, Except.ok.injEq, Prod.mk.injEq] at ht
        rcases ht with ⟨rfl, _⟩
        exact ⟨hh, hlo, hhi⟩
      · simp [hlt] at ht
    · have hfalse : a.admin = false := by cases h : a.admin <;> simp_all
      simp [hfalse] at ht
  | setMode m =>
    unfold thermostatTransition at ht
    by_cases hadm : a.admin = true
    · simp only [hadm, Bool.not_true, Bool.false_eq_true, ↓reduceIte, Except.ok.injEq, Prod.mk.injEq] at ht
      rcases ht with ⟨rfl, _⟩
      exact ⟨fun h => heaterOn_interlock _ _ _ h, hlo, hhi⟩
    · have hfalse : a.admin = false := by cases h : a.admin <;> simp_all
      simp [hfalse] at ht

theorem inv_run {st : Snapshot} {evs : List (KernelEvent Device Command Snapshot Error)}
    (hr : H5iAppVeil.Run thermostatTransition id applyW initialSnapshot st evs) :
    Inv st := by
  induction hr with
  | nil => exact inv_initial
  | ok _ hst ht ih =>
    subst hst
    exact inv_step ih ht
  | err _ _ _ ih => exact ih

/-- **Lentil lifting**: every infinite execution of `thermostatKernelRTS`
satisfies `□ ⌜ Inv ⌝` (interlock and setpoint bounds at every step). -/
theorem thermostat_always_inv
    (e : Lentil.exec Snapshot)
    (hInit : thermostatKernelRTS.init () (e 0))
    (hNext : ∀ n, thermostatKernelRTS.next () (e n) (e (n + 1)) ∨ e (n + 1) = e n) :
    Lentil.always (Lentil.state_pred Inv) e 0 :=
  lentil_always_of_run_inv (fun _ _ hr => inv_run hr) e hInit hNext

/-- Concrete runtime state of `firmware/src/main.rs` (`DeviceEngine` + `actuate` task). -/
structure FirmwareState where
  snapshot : Snapshot
  version : Nat
  actuatorSeen : Nat
  pinHeater : Bool

inductive FirmwareLabel where
  | exec (a : Device) (c : Command)
  | actuatePoll
deriving DecidableEq, Repr

inductive FirmwareStep : FirmwareState → FirmwareLabel → FirmwareState → Prop
  | execOk {s a c ws r} :
      thermostatTransition a s.snapshot c = .ok (ws, r) →
      FirmwareStep s (.exec a c)
        { s with snapshot := applyW s.snapshot ws, version := s.version + 1 }
  | execErr {s a c e} :
      thermostatTransition a s.snapshot c = .error e →
      FirmwareStep s (.exec a c) s
  | actuatePoll {s} :
      s.version ≠ s.actuatorSeen →
      FirmwareStep s .actuatePoll
        { s with actuatorSeen := s.version, pinHeater := s.snapshot.heater }

def firmwareRTS : Veil.RelationalTransitionSystem Unit FirmwareState FirmwareLabel where
  assumptions := fun _ => True
  init := fun _ s =>
    s = { snapshot := initialSnapshot, version := 0, actuatorSeen := 0, pinHeater := false }
  tr := fun _ s lbl s' => FirmwareStep s lbl s'

/-- **Refinement theorem**: the concrete thermostat firmware (`DeviceEngine` +
`actuate` task) stuttering-refines `thermostatKernelRTS` via committed snapshot
projection, so every kernel safety invariant lifts directly to the firmware. -/
theorem firmware_stutterRefines_thermostatKernel :
    StutterRefines id FirmwareState.snapshot firmwareRTS thermostatKernelRTS where
  assumptions_map := fun _ h => h
  init_map := fun _ s _ hinit => by
    subst hinit
    rfl
  step_map := fun _ s s' _ hnext => by
    obtain ⟨lbl, hstep⟩ := hnext
    cases hstep with
    | execOk ht =>
      left
      exact ⟨.ok _ _ _, KernelStep.ok ht⟩
    | execErr ht =>
      left
      exact ⟨.err _ _ _, KernelStep.err ht⟩
    | actuatePoll _ =>
      right
      rfl

/-- Concrete consequence of refinement: every reachable firmware state satisfies
the kernel's interlock and setpoint range invariants. -/
theorem firmware_reachable_inv {s : FirmwareState} (hr : firmwareRTS.reachable () s) :
    Inv s.snapshot := by
  have habs := firmware_stutterRefines_thermostatKernel.reachable hr
  obtain ⟨evs, hrun⟩ := run_iff_veil_reachable.mpr habs
  exact inv_run hrun

end ThermostatTemporal.KernelBridge
