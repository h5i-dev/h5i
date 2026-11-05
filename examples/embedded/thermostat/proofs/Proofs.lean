import ThermostatKernel
import H5iAppLib.Basic
import H5iAppLib.Runs
/-!
# Proofs about the extracted thermostat kernel

* `transition_total`: no command panics, on any state.
* `set_relay_refused`: a relay index of 4 or more is refused before any
  array access.
* `inv_reachable`: the interlock (`heater → temp_dc < MAX_TEMP_DC`) and the
  setpoint range hold in every state a run of successful commands reaches.
-/
open Aeneas Aeneas.Std Result thermostat_kernel

namespace thermostat_kernel.Proofs

theorem max_temp_val : MAX_TEMP_DC.val = 350 := by unfold MAX_TEMP_DC; rfl
theorem min_sp_val : MIN_SETPOINT_DC.val = 50 := by unfold MIN_SETPOINT_DC; rfl
theorem max_sp_val : MAX_SETPOINT_DC.val = 300 := by unfold MAX_SETPOINT_DC; rfl
theorem relays_val : RELAYS.val = 4 := by unfold RELAYS; rfl

/-- The safety property of a state. -/
def Inv (s : Snapshot) : Prop :=
  (s.heater = true → s.temp_dc.val < 350) ∧ 50 ≤ s.setpoint_dc.val ∧ s.setpoint_dc.val ≤ 300

/-- What committing a write set means. -/
def applyW (s : Snapshot) : Option Snapshot → Snapshot
  | none => s
  | some n => n

theorem apply_eq (s : Snapshot) (w : Option Snapshot) : apply s w = ok (applyW s w) := by
  cases w <;> rfl

@[step]
theorem heater_on_spec (m : Mode) (t sp : I16) :
    heater_on m t sp ⦃ b => b = true → t.val < 350 ⦄ := by
  unfold heater_on
  have := max_temp_val
  split
  · simp
  · rename_i h
    have ht : t.val < 350 := by simp only [ge_iff_le, IScalar.le_equiv, not_le] at h; omega
    cases m <;> simp [ht]

@[step]
theorem settle_spec (s : Snapshot) :
    settle s ⦃ n => n.setpoint_dc = s.setpoint_dc ∧ n.temp_dc = s.temp_dc ∧ n.relays = s.relays ∧
      n.mode = s.mode ∧ (n.heater = true → s.temp_dc.val < 350) ⦄ := by
  unfold settle
  step*

/-- No command panics, for any actor and state. -/
theorem transition_total (a : Device) (s : Snapshot) (c : Command) : ∃ r, transition a s c = ok r := by
  have h : transition a s c ⦃ _ => True ⦄ := by
    unfold transition
    have := relays_val
    cases c <;> step* <;> scalar_tac
  obtain ⟨r, hr, -⟩ := (WP.spec_equiv_exists _ _).1 h
  exact ⟨r, hr⟩

/-- A relay index of 4 or more is refused; it never reaches the array. -/
theorem set_relay_refused (a : Device) (s : Snapshot) (idx : U8) (on : Bool) (ha : a.admin = true)
    (hi : 4 ≤ idx.val) : transition a s (.SetRelay idx on) = ok (.Err .NoSuchRelay) := by
  unfold transition
  simp only [ha, if_true]
  have h : (UScalar.cast UScalarTy.Usize idx).val = idx.val := by simp
  simp only [lift, ge_iff_le, UScalar.le_equiv, relays_val, h]
  simp [h, show 4 ≤ idx.val from hi]

/-- Every successful command keeps `Inv`. -/
theorem inv_step (a : Device) (s : Snapshot) (c : Command) (ws : Option Snapshot) (r : Reply)
    (hs : Inv s) (ht : transition a s c = ok (.Ok (ws, r))) : Inv (applyW s ws) := by
  have h : transition a s c ⦃ res => ∀ ws r, res = .Ok (ws, r) → Inv (applyW s ws) ⦄ := by
    unfold transition
    have := relays_val
    have := min_sp_val
    have := max_sp_val
    obtain ⟨hh, hl, hu⟩ := hs
    cases c <;> step* <;> (try (intro ws r h; cases h; simp only [applyW, Inv]))
    all_goals (try (simp only [IScalar.lt_equiv, gt_iff_lt, not_lt] at *))
    all_goals (try (refine ⟨by simp_all, by simp_all, by simp_all⟩; done))
    all_goals (try (refine ⟨by simp_all, by simp_all; omega, by simp_all; omega⟩; done))
    all_goals (try (intro ws r h; simp at h; done))
    all_goals (try (refine ⟨by simp_all, hl, hu⟩; done))
    all_goals (try (simp_all [Inv]; done))
    all_goals scalar_tac
  exact H5iAppLib.post_of_ok h ht ws r rfl

theorem inv_initial : Inv INITIAL := by
  unfold INITIAL Inv; simp

/-- The interlock and the setpoint range hold in every reachable state:
after any sequence of commands from any actors, successful or refused. -/
theorem inv_reachable {st : Snapshot} {evs : List (H5iAppLib.Event Device Command Reply Error)}
    (h : H5iAppLib.Run transition id applyW INITIAL st evs) : Inv st :=
  H5iAppLib.Run.inv Inv inv_initial (fun a s c ws r hs ht => inv_step a s c ws r hs ht) h

/-- In particular the heater is never on at or above `MAX_TEMP_DC`. -/
theorem interlock {st : Snapshot} {evs : List (H5iAppLib.Event Device Command Reply Error)}
    (h : H5iAppLib.Run transition id applyW INITIAL st evs) (hh : st.heater = true) : st.temp_dc.val < 350 :=
  (inv_reachable h).1 hh

end thermostat_kernel.Proofs
