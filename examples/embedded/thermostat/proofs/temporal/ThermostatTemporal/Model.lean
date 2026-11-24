import Veil
import Aeneas
import ThermostatKernel
import Proofs
import H5iAppVeil

/-!
# Optional Veil Temporal Verification for `examples/embedded/thermostat`

Complements the Aeneas safety proofs in `../Proofs.lean` (`transition_total`,
`inv_reachable`, `interlock`) with both model-level and implementation-level
temporal logic verification of the multi-task closed loop in
`firmware/src/main.rs` (`sense`, `actuate`, HTTP admin commands over
`DeviceEngine`):

1. **Model-Level Veil Closed-Loop Checks (`ThermostatClosedLoop`)**:
   - Finite-state Veil DSL abstraction of the thermostat kernel, `sense` task,
     and `actuate` task.
   - Exhaustively checks safety invariants via Veil **`#model_check`** and
     temporal liveness / fail-safe properties under Lentil weak fairness (`𝒲ℱ`)
     via Veil's Lasso **`#temporal_check`**.
2. **Implementation-Level Bridge & Liveness (`ThermostatTemporal.ExtractedBridge`)**:
   - Imports the **actual Aeneas-extracted** `thermostat_kernel.transition`
     (`../generated/ThermostatKernel.lean`) and existing safety proof
     `thermostat_kernel.Proofs.inv_reachable` (`../Proofs.lean`) directly —
     with no manual re-encoding of the thermostat kernel.
   - Lifts `inv_reachable` directly into Lentil's `□ ⌜ Inv ⌝` via
     `H5iAppVeil.lentil_always_of_run_inv`.
   - Proves `StutterRefines` from the concrete firmware closed-loop transition
     system (`extractedFirmwareRTS`, executing `thermostat_kernel.transition`)
     to `thermostatKernelRTS`.
   - Proves the nontrivial Lentil temporal liveness theorem
     `extracted_thermostat_overtemp_liveness` directly over the extracted
     implementation: under weak fairness of `senseReport` and `actuatePoll`,
     persistent physical overheating (`350 ≤ physTemp.val`) or sensor failure
     (`sensorFailed = true`) guarantees `◇ □ ⌜ pinHeater = false ⌝`.
-/

/-! ## 1. Closed-Loop Thermostat + `DeviceEngine` Veil Model (Model-Level) -/

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

/-! ## 2. Direct Bridge & Temporal Liveness over the Aeneas-Extracted Kernel -/

namespace ThermostatTemporal.ExtractedBridge

open Aeneas Aeneas.Std Result H5iAppVeil thermostat_kernel thermostat_kernel.Proofs

/-- The Veil `RelationalTransitionSystem` of the **actual Aeneas-extracted**
`thermostat_kernel.transition` (from `../generated/ThermostatKernel.lean`). -/
def thermostatKernelRTS :
    Veil.RelationalTransitionSystem Unit Snapshot
      (H5iAppLib.Event Device Command Reply thermostat_kernel.Error) :=
  H5iAppVeil.kernelRTS
    thermostat_kernel.transition
    id
    thermostat_kernel.Proofs.applyW
    thermostat_kernel.INITIAL

/-- Direct lift of the existing Aeneas safety theorem
`thermostat_kernel.Proofs.inv_reachable` (proven over `H5iAppLib.Run`) into
Lentil's `□ ⌜ Inv ⌝` on every infinite execution of the extracted kernel. -/
theorem extracted_thermostat_always_inv
    (e : Lentil.exec Snapshot)
    (hInit : thermostatKernelRTS.init () (e 0))
    (hNext : ∀ n, thermostatKernelRTS.next () (e n) (e (n + 1)) ∨ e (n + 1) = e n) :
    Lentil.always (Lentil.state_pred thermostat_kernel.Proofs.Inv) e 0 :=
  H5iAppVeil.lentil_always_of_run_inv
    (fun _ _ h => thermostat_kernel.Proofs.inv_reachable h) e hInit hNext

/-- Concrete firmware closed-loop state executing the **actual Aeneas-extracted**
`thermostat_kernel.transition` inside `DeviceEngine`
(`examples/embedded/thermostat/firmware/src/main.rs`). -/
structure FirmwareState where
  snap : Snapshot
  physTemp : Std.I16
  sensorFailed : Bool
  notifyPending : Bool
  pinHeater : Bool

def firmwareInit : FirmwareState where
  snap := thermostat_kernel.INITIAL
  physTemp := 200#i16
  sensorFailed := false
  notifyPending := false
  pinHeater := false

/-- The temperature reported by `firmware/src/main.rs`'s `sense` task:
`MAX_TEMP_DC` (350) when the TMP108 I2C sensor is unreadable, else `physTemp`. -/
def reportedTemp (s : FirmwareState) : Std.I16 :=
  if s.sensorFailed then thermostat_kernel.MAX_TEMP_DC else s.physTemp

inductive FirmwareLabel where
  | adminCmd (actor : Device) (cmd : Command)
  | senseReport
  | actuatePoll
  | envTemp (t' : Std.I16)
  | envSensor (failed' : Bool)

/-- Firmware closed-loop step relation (`firmware/src/main.rs`):
- `adminOk`/`adminErr`: HTTP routes (`SetSetpoint`, `SetMode`, `SetRelay`,
  `Read`; HTTP routes never send `ReportTemp`, see `app/src/lib.rs`) executing
  the actual Aeneas-extracted `thermostat_kernel.transition`.
- `senseReport`: `sense` task reporting `reportedTemp s` via the actual
  extracted `thermostat_kernel.transition`.
- `actuatePoll`: `actuate` task waking from `DeviceEngine::changed` and driving
  the physical PD12 heater GPIO pin.
- `envTemp`/`envSensor`: physical temperature and I2C sensor health changes. -/
inductive FirmwareStep : FirmwareLabel → FirmwareState → FirmwareState → Prop where
  | adminOk {actor : Device} {cmd : Command} {s : FirmwareState}
      {ws : Option Snapshot} {r : Reply}
      (hNotTemp : ∀ dc, cmd ≠ Command.ReportTemp dc)
      (ht : thermostat_kernel.transition actor s.snap cmd = ok (.Ok (ws, r))) :
      FirmwareStep (.adminCmd actor cmd) s
        { s with
          snap := applyW s.snap ws
          notifyPending := match ws with | some _ => true | none => s.notifyPending }
  | adminErr {actor : Device} {cmd : Command} {s : FirmwareState} {err : thermostat_kernel.Error}
      (hNotTemp : ∀ dc, cmd ≠ Command.ReportTemp dc)
      (ht : thermostat_kernel.transition actor s.snap cmd = ok (.Err err)) :
      FirmwareStep (.adminCmd actor cmd) s s
  | senseReport {s : FirmwareState} {ws : Option Snapshot} {r : Reply}
      (hDiff : s.snap.temp_dc ≠ reportedTemp s)
      (ht : thermostat_kernel.transition { admin := false } s.snap
              (Command.ReportTemp (reportedTemp s)) = ok (.Ok (ws, r))) :
      FirmwareStep .senseReport s
        { s with snap := applyW s.snap ws, notifyPending := true }
  | actuatePoll {s : FirmwareState}
      (hPend : s.notifyPending = true) :
      FirmwareStep .actuatePoll s
        { s with notifyPending := false, pinHeater := s.snap.heater }
  | envTemp {s : FirmwareState} {t' : Std.I16} :
      FirmwareStep (.envTemp t') s { s with physTemp := t' }
  | envSensor {s : FirmwareState} {failed' : Bool} :
      FirmwareStep (.envSensor failed') s { s with sensorFailed := failed' }

/-- Veil `RelationalTransitionSystem` for the firmware closed loop executing
the actual Aeneas-extracted `thermostat_kernel.transition`. -/
def extractedFirmwareRTS :
    Veil.RelationalTransitionSystem Unit FirmwareState FirmwareLabel where
  assumptions := fun _ => True
  init := fun _ s => s = firmwareInit
  tr := fun _ s lbl s' => FirmwareStep lbl s s'

/-- Lentil action predicate for `senseReport`. -/
def senseAction : Lentil.action FirmwareState :=
  fun s s' => FirmwareStep .senseReport s s'

/-- Lentil action predicate for `actuatePoll`. -/
def actuateAction : Lentil.action FirmwareState :=
  fun s s' => FirmwareStep .actuatePoll s s'

/-- Stuttering refinement from the firmware closed loop directly to the
Aeneas-extracted `thermostatKernelRTS`. -/
theorem extractedFirmware_stutterRefines_kernel :
    H5iAppVeil.StutterRefines id (fun fs => fs.snap) extractedFirmwareRTS thermostatKernelRTS where
  assumptions_map := fun _ _ => trivial
  init_map := fun _ _ _ hinit => by
    subst hinit
    rfl
  step_map := fun _ _ _ _ hnext => by
    obtain ⟨lbl, hstep⟩ := hnext
    cases hstep with
    | adminOk _ ht => exact Or.inl ⟨.ok _ _ _, H5iAppVeil.KernelStep.ok ht⟩
    | adminErr _ ht => exact Or.inl ⟨.err _ _ _, H5iAppVeil.KernelStep.err ht⟩
    | senseReport _ ht => exact Or.inl ⟨.ok _ _ _, H5iAppVeil.KernelStep.ok ht⟩
    | actuatePoll _ => exact Or.inr rfl
    | envTemp => exact Or.inr rfl
    | envSensor => exact Or.inr rfl

/-- Safety transfer: every infinite execution of the firmware closed loop
preserves `thermostat_kernel.Proofs.Inv` (interlock + setpoint bounds) on the
committed snapshot at every step. -/
theorem extracted_firmware_always_inv
    (e : Lentil.exec FirmwareState)
    (hInit : extractedFirmwareRTS.init () (e 0))
    (hNext : ∀ n, extractedFirmwareRTS.next () (e n) (e (n + 1)) ∨ e (n + 1) = e n) :
    Lentil.always (Lentil.state_pred (fun fs => thermostat_kernel.Proofs.Inv fs.snap)) e 0 :=
  extractedFirmware_stutterRefines_kernel.lentil_always
    (fun _ hr =>
      let ⟨_, hrun⟩ := H5iAppVeil.run_iff_veil_reachable.mpr hr
      thermostat_kernel.Proofs.inv_reachable hrun)
    e trivial hInit hNext

/-- Non-`ReportTemp` commands in the extracted `thermostat_kernel.transition`
never alter `temp_dc`. -/
private theorem adminCmd_preserves_temp
    (actor : Device) (s : Snapshot) (cmd : Command)
    (ws : Option Snapshot) (r : Reply)
    (hNotTemp : ∀ dc, cmd ≠ Command.ReportTemp dc)
    (ht : thermostat_kernel.transition actor s cmd = ok (.Ok (ws, r))) :
    (applyW s ws).temp_dc = s.temp_dc := by
  have h : thermostat_kernel.transition actor s cmd
      ⦃ res => ∀ ws r, res = .Ok (ws, r) → (applyW s ws).temp_dc = s.temp_dc ⦄ := by
    unfold thermostat_kernel.transition
    have := relays_val
    cases cmd with
    | ReportTemp dc => exact False.elim (hNotTemp dc rfl)
    | Read =>
      step*
      intro ws' r' heq
      cases heq
      rfl
    | SetSetpoint dc =>
      step*
      all_goals (intro ws' r' heq; cases heq; simp [applyW, *])
    | SetMode m =>
      step*
      all_goals (intro ws' r' heq; cases heq; simp [applyW, *])
    | SetRelay idx on =>
      step*
      all_goals (intro ws' r' heq; cases heq; simp [applyW, *])
  exact H5iAppLib.post_of_ok h ht ws r rfl

/-- Reporting temperature `dc` via the extracted `thermostat_kernel.transition`
always succeeds and sets `temp_dc = dc`. -/
private theorem reportTemp_succeeds (actor : Device) (s : Snapshot) (dc : Std.I16) :
    ∃ n, thermostat_kernel.transition actor s (.ReportTemp dc) = ok (.Ok (some n, .State n)) ∧
      n.temp_dc = dc := by
  have h : thermostat_kernel.transition actor s (.ReportTemp dc)
      ⦃ res => ∃ n, res = .Ok (some n, .State n) ∧ n.temp_dc = dc ⦄ := by
    unfold thermostat_kernel.transition
    step*
  obtain ⟨res, hok⟩ := H5iAppLib.ok_of h
  obtain ⟨n, hres, htemp⟩ := H5iAppLib.post_of_ok h hok
  subst hres
  exact ⟨n, hok, htemp⟩

/-- Whenever `senseReport` runs via the extracted `thermostat_kernel.transition`,
the committed snapshot's `temp_dc` becomes `dc`. -/
private theorem reportTemp_sets_temp (actor : Device) (s : Snapshot) (dc : Std.I16)
    (ws : Option Snapshot) (r : Reply)
    (ht : thermostat_kernel.transition actor s (.ReportTemp dc) = ok (.Ok (ws, r))) :
    (applyW s ws).temp_dc = dc := by
  have h : thermostat_kernel.transition actor s (.ReportTemp dc)
      ⦃ res => ∀ ws r, res = .Ok (ws, r) → (applyW s ws).temp_dc = dc ⦄ := by
    unfold thermostat_kernel.transition
    step*
    intro ws' r' heq
    cases heq
    simp [applyW, *]
  exact H5iAppLib.post_of_ok h ht ws r rfl

/-- Whenever no `DeviceEngine` notification is pending, the physical PD12
heater pin equals the committed kernel snapshot's `heater` field. -/
theorem extracted_firmware_actuator_sync
    (e : Lentil.exec FirmwareState)
    (hInit : extractedFirmwareRTS.init () (e 0))
    (hNext : ∀ n, extractedFirmwareRTS.next () (e n) (e (n + 1)) ∨ e (n + 1) = e n) :
    Lentil.always (Lentil.state_pred (fun fs => fs.notifyPending = false → fs.pinHeater = fs.snap.heater)) e 0 := by
  intro k
  simp only [Lentil.state_pred, Nat.zero_add]
  induction k with
  | zero =>
    intro _
    have h0 : e 0 = firmwareInit := hInit
    rw [h0]
    unfold firmwareInit thermostat_kernel.INITIAL
    rfl
  | succ n ih =>
    rcases hNext n with ⟨lbl, hstep⟩ | heq
    · generalize hs : e n = s at ih hstep
      generalize hs' : e (n + 1) = s' at hstep
      cases hstep with
      | @adminOk actor cmd s ws r hNotTemp ht =>
        cases ws with
        | none => simpa [applyW] using ih
        | some n' => intro hfalse; cases hfalse
      | adminErr _ _ => exact ih
      | senseReport _ _ => intro hfalse; cases hfalse
      | actuatePoll _ => intro _; rfl
      | envTemp => exact ih
      | envSensor => exact ih
    · rw [heq]; exact ih

/-- **Implementation-Level Temporal Liveness Theorem**:
Along any infinite execution of `extractedFirmwareRTS` (running the **actual
Aeneas-extracted `thermostat_kernel.transition`**), under Lentil weak fairness
of `senseReport` and `actuatePoll`, if physical temperature persistently
reaches/exceeds `MAX_TEMP_DC` (`350`) or the TMP108 sensor persistently fails,
then the physical PD12 heater pin eventually turns off and stays off forever:
`◇ □ ⌜ pinHeater = false ⌝`. -/
theorem extracted_thermostat_overtemp_liveness
    (e : Lentil.exec FirmwareState)
    (hInit : extractedFirmwareRTS.init () (e 0))
    (hNext : ∀ n, extractedFirmwareRTS.next () (e n) (e (n + 1)) ∨ e (n + 1) = e n)
    (hFairSense : Lentil.weak_fairness senseAction e 0)
    (hFairActuate : Lentil.weak_fairness actuateAction e 0)
    (hOvertemp : Lentil.eventually
      (Lentil.always (Lentil.state_pred (fun fs => 350 ≤ fs.physTemp.val ∨ fs.sensorFailed = true))) e 0) :
    Lentil.eventually (Lentil.always (Lentil.state_pred (fun fs => fs.pinHeater = false))) e 0 := by
  have hInvAll := extracted_firmware_always_inv e hInit hNext
  have hSyncAll := extracted_firmware_actuator_sync e hInit hNext
  obtain ⟨K, hK⟩ := hOvertemp
  have hRepGe : ∀ j, K ≤ j → 350 ≤ (reportedTemp (e j)).val := by
    intro j hj
    obtain ⟨d, rfl⟩ := Nat.exists_eq_add_of_le hj
    have hcond : 350 ≤ (e (K + d)).physTemp.val ∨ (e (K + d)).sensorFailed = true := by
      simpa [Lentil.state_pred, Nat.zero_add] using hK d
    unfold reportedTemp
    cases hf : (e (K + d)).sensorFailed with
    | true => simp [max_temp_val]
    | false =>
      simp only [Bool.false_eq_true, ↓reduceIte]
      rcases hcond with hge | htrue
      · exact hge
      · rw [hf] at htrue; cases htrue
  -- Phase 1: Under weak fairness of `senseAction`, there exists `K₁ ≥ K` with
  -- `350 ≤ (e K₁).snap.temp_dc.val`.
  have hSnapOvertemp : ∃ K₁, K ≤ K₁ ∧ 350 ≤ (e K₁).snap.temp_dc.val := by
    by_contra hNo
    push Not at hNo
    have hEnabledAll : ∀ d, Lentil.tl_enabled senseAction e (K + d) := by
      intro d
      have hge := hRepGe (K + d) (Nat.le_add_right K d)
      have hlt := hNo (K + d) (Nat.le_add_right K d)
      have hDiff : (e (K + d)).snap.temp_dc ≠ reportedTemp (e (K + d)) := by
        intro heq
        rw [heq] at hlt
        omega
      obtain ⟨n', hok, _⟩ := reportTemp_succeeds { admin := false } (e (K + d)).snap (reportedTemp (e (K + d)))
      exact ⟨{ e (K + d) with snap := applyW (e (K + d)).snap (some n'), notifyPending := true },
        FirmwareStep.senseReport hDiff hok⟩
    have hWF := (congrFun (congrFun Lentil.wf_as_implies e) 0).mp hFairSense
    have hInfOften : Lentil.always (Lentil.eventually (Lentil.action_pred senseAction)) e 0 :=
      hWF ⟨K, fun d => by simpa [Nat.zero_add] using hEnabledAll d⟩
    obtain ⟨d, hSense⟩ := hInfOften K
    have hStep : FirmwareStep .senseReport (e (K + d)) (e (K + d + 1)) := by
      simpa [Lentil.action_pred, senseAction, Nat.zero_add] using hSense
    generalize hs : e (K + d) = s at hStep
    generalize hs' : e (K + d + 1) = s' at hStep
    cases hStep with
    | senseReport _ ht =>
      have hTempEq : (applyW s.snap _).temp_dc = reportedTemp s :=
        reportTemp_sets_temp _ _ _ _ _ ht
      have hge := hRepGe (K + d) (Nat.le_add_right K d)
      have hlt := hNo (K + d + 1) (by omega)
      rw [hs'] at hlt
      rw [hs] at hge
      rw [hTempEq] at hlt
      omega
  obtain ⟨K₁, hKK₁, hTempK₁⟩ := hSnapOvertemp
  -- Phase 2: Once `350 ≤ snap.temp_dc.val` at `K₁ ≥ K`, it persists forever,
  -- so by `Inv`, `snap.heater = false` for all `j ≥ K₁`.
  have hTempForever : ∀ d, 350 ≤ (e (K₁ + d)).snap.temp_dc.val := by
    intro d
    induction d with
    | zero => simpa using hTempK₁
    | succ m ih =>
      rcases hNext (K₁ + m) with ⟨lbl, hstep⟩ | heq
      · generalize hs : e (K₁ + m) = s at ih hstep
        generalize hs' : e (K₁ + m + 1) = s' at hstep
        cases hstep with
        | adminOk hNotTemp ht =>
          have hpres := adminCmd_preserves_temp _ _ _ _ _ hNotTemp ht
          simpa [Nat.add_assoc, ← hs', hpres] using ih
        | adminErr _ _ =>
          simpa [Nat.add_assoc, ← hs'] using ih
        | senseReport _ ht =>
          have hTempEq := reportTemp_sets_temp _ _ _ _ _ ht
          have hge := hRepGe (K₁ + m) (by omega)
          rw [hs] at hge
          simpa [Nat.add_assoc, ← hs', hTempEq] using hge
        | actuatePoll _ =>
          simpa [Nat.add_assoc, ← hs'] using ih
        | envTemp =>
          simpa [Nat.add_assoc, ← hs'] using ih
        | envSensor =>
          simpa [Nat.add_assoc, ← hs'] using ih
      · rw [← Nat.add_assoc, heq]; exact ih
  have hSnapHeaterOff : ∀ d, (e (K₁ + d)).snap.heater = false := by
    intro d
    have hinv : thermostat_kernel.Proofs.Inv (e (K₁ + d)).snap := by
      simpa [Lentil.state_pred, Nat.zero_add] using hInvAll (K₁ + d)
    have hge := hTempForever d
    cases hh : (e (K₁ + d)).snap.heater with
    | false => rfl
    | true =>
      have hlt := hinv.1 hh
      omega
  -- Phase 3: Under weak fairness of `actuateAction`, `pinHeater` reaches `false`
  -- at some `K₂ ≥ K₁`.
  have hPinTurnsOff : ∃ d₀, (e (K₁ + d₀)).pinHeater = false := by
    by_contra hAllOn
    push Not at hAllOn
    have hPendAll : ∀ d, Lentil.tl_enabled actuateAction e (K₁ + d) := by
      intro d
      have hsync : (e (K₁ + d)).notifyPending = false →
          (e (K₁ + d)).pinHeater = (e (K₁ + d)).snap.heater := by
        simpa [Lentil.state_pred, Nat.zero_add] using hSyncAll (K₁ + d)
      have hPend : (e (K₁ + d)).notifyPending = true := by
        cases hp : (e (K₁ + d)).notifyPending with
        | true => rfl
        | false =>
          have hpeq := hsync hp
          rw [hSnapHeaterOff d] at hpeq
          exact False.elim (hAllOn d hpeq)
      exact ⟨{ e (K₁ + d) with notifyPending := false, pinHeater := (e (K₁ + d)).snap.heater },
        FirmwareStep.actuatePoll hPend⟩
    have hWF := (congrFun (congrFun Lentil.wf_as_implies e) 0).mp hFairActuate
    have hInfOften : Lentil.always (Lentil.eventually (Lentil.action_pred actuateAction)) e 0 :=
      hWF ⟨K₁, fun d => by simpa [Nat.zero_add] using hPendAll d⟩
    obtain ⟨d, hAct⟩ := hInfOften K₁
    have hStep : FirmwareStep .actuatePoll (e (K₁ + d)) (e (K₁ + d + 1)) := by
      simpa [Lentil.action_pred, actuateAction, Nat.zero_add] using hAct
    generalize hs : e (K₁ + d) = s at hStep
    generalize hs' : e (K₁ + d + 1) = s' at hStep
    cases hStep with
    | actuatePoll _ =>
      have hoff : s.snap.heater = false := by
        rw [← hs]; exact hSnapHeaterOff d
      have hpin : (e (K₁ + (d + 1))).pinHeater = false := by
        rw [← Nat.add_assoc, hs']
        exact hoff
      exact hAllOn (d + 1) hpin
  obtain ⟨d₀, hPinOff0⟩ := hPinTurnsOff
  -- Phase 4: Once `pinHeater = false` at `K₁ + d₀`, it stays `false` forever
  -- because `snap.heater = false` for all `j ≥ K₁`.
  refine ⟨K₁ + d₀, fun m => ?_⟩
  change (e (0 + (K₁ + d₀) + m)).pinHeater = false
  rw [Nat.zero_add]
  induction m with
  | zero => simpa using hPinOff0
  | succ k ih =>
    rcases hNext (K₁ + d₀ + k) with ⟨lbl, hstep⟩ | heq
    · generalize hs : e (K₁ + d₀ + k) = s at ih hstep
      generalize hs' : e (K₁ + d₀ + k + 1) = s' at hstep
      cases hstep with
      | adminOk _ _ => simpa [Nat.add_assoc, ← hs'] using ih
      | adminErr _ _ => simpa [Nat.add_assoc, ← hs'] using ih
      | senseReport _ _ => simpa [Nat.add_assoc, ← hs'] using ih
      | actuatePoll _ =>
        have hoff : s.snap.heater = false := by
          rw [← hs]
          simpa [Nat.add_assoc] using hSnapHeaterOff (d₀ + k)
        simpa [Nat.add_assoc, ← hs'] using hoff
      | envTemp => simpa [Nat.add_assoc, ← hs'] using ih
      | envSensor => simpa [Nat.add_assoc, ← hs'] using ih
    · rw [← Nat.add_assoc, heq]; exact ih

end ThermostatTemporal.ExtractedBridge
