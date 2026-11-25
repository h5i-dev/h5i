import Aeneas
import H5iAppEmbedded
import NorFlash
import FlashStoreProofs
import H5iAppVeil.RunBridge
import Veil
import Lentil

/-!
# Veil / Lentil Temporal Bridge for Extracted Production `FlashStore`

Connects the **actual Aeneas-extracted production `FlashStore` core**
(`crates/h5i-app-embedded/src/store_core.rs` → `H5iAppEmbedded.lean`, proved in
`NorFlash.lean` and `FlashStoreProofs.lean`) directly to Veil's
`RelationalTransitionSystem` and Lentil's temporal operators (`□`, `◇`, `𝒲ℱ`).

## Scope Classification
- **`kernel_implementation_level`** (`extracted_flashstore_recovery_always_inv`,
  `extracted_flashstore_recovery_liveness`): proved purely over the actual
  Aeneas-extracted `store_core.scan_step` state machine without any handwritten
  flash transition model.
- **`compositional`** (`extracted_flashstore_always_crash_consistent`,
  `extracted_flashstore_save_liveness`): proved over the actual Aeneas-extracted
  `store_core.{plan_save, encode_record, pos_after_write, seq_newer}` composed
  with the reusable handwritten `NorFlash` hardware/power-loss specification
  (`NorFlash.lean`).
-/

open Aeneas Aeneas.Std Result h5i_app_embedded h5i_app_embedded.store_core
  h5i_app_embedded.FlashStoreProofs

namespace H5iAppVeil.Embedded.FlashStoreExtracted

/-! ## 1. Compositional `FlashStore` + `NorFlash` System in Veil / Lentil -/

/-- Veil `RelationalTransitionSystem` wrapping the compositional extracted
`store_core` + `NorFlash` transition relation `ComposedStep g`. -/
def extractedFlashStoreRTS (g : FlashGeometry) :
    Veil.RelationalTransitionSystem Unit ComposedState Unit where
  assumptions := fun _ => True
  init := fun _ cs => cs = composedInit
  tr := fun _ cs _ cs' => ComposedStep g cs cs'

/-- Exact equivalence between `ComposedReachable g` and Veil reachability on
`extractedFlashStoreRTS g`. -/
theorem composedReachable_iff_veil_reachable (g : FlashGeometry) (cs : ComposedState) :
    ComposedReachable g cs ↔ (extractedFlashStoreRTS g).reachable () cs := by
  constructor
  · intro hr
    induction hr with
    | init => exact .init _ trivial rfl
    | step _ hstep ih => exact .step _ _ ih ⟨(), hstep⟩
  · intro hr
    induction hr with
    | init s _ hinit =>
      subst hinit
      exact .init
    | step s s' _ hnext ih =>
      obtain ⟨_, hstep⟩ := hnext
      exact .step ih hstep

/-- **Compositional Safety (`□ CrashConsistentInv`)**:
Lifts `FlashStoreProofs.extracted_flashstore_crash_consistent` directly into
Lentil's `□ ⌜ CrashConsistentInv g ⌝` over every infinite execution of the
extracted `store_core` + `NorFlash` system (including partial erases, torn
writes, post-write/pre-ack crashes, wraparound, and repeated reboots). -/
theorem extracted_flashstore_always_crash_consistent
    (g : FlashGeometry) (hlay : layout_ok g = ok true) (hdiv : AlignDivides g)
    (e : Lentil.exec ComposedState)
    (hInit : (extractedFlashStoreRTS g).init () (e 0))
    (hNext : ∀ n, (extractedFlashStoreRTS g).next () (e n) (e (n + 1)) ∨ e (n + 1) = e n) :
    Lentil.always (Lentil.state_pred (CrashConsistentInv g)) e 0 := by
  have hreach : ∀ k, (extractedFlashStoreRTS g).reachable () (e k) := by
    intro k
    induction k with
    | zero => exact .init _ trivial hInit
    | succ n ih =>
      rcases hNext n with hn | heq
      · exact .step _ _ ih hn
      · rw [heq]; exact ih
  intro k
  have hcr := (composedReachable_iff_veil_reachable g (e k)).mpr (hreach k)
  simpa [Lentil.state_pred, Nat.zero_add] using
    extracted_flashstore_crash_consistent g hlay hdiv hcr

/-- Progress rank of an in-flight `SavePhase`: every non-stuttering
`ComposedStep` from a non-idle state strictly decreases `phaseRank`. -/
def phaseRank : SavePhase → Nat
  | .idle => 0
  | .writeLanded _ _ _ _ _ => 1
  | .writing _ _ _ _ _ _ => 2
  | .erasing _ _ _ _ _ _ _ => 3

/-- Lentil action predicate for any non-idle `ComposedStep` (flash I/O
completion, fault termination, or reboot recovery). -/
def saveProgressAction (g : FlashGeometry) : Lentil.action ComposedState :=
  fun cs cs' => cs.phase ≠ .idle ∧ ComposedStep g cs cs'

private theorem composedStep_nonidle_decr_rank {g : FlashGeometry} {cs cs' : ComposedState}
    (hne : cs.phase ≠ .idle) (hstep : ComposedStep g cs cs') :
    phaseRank cs'.phase < phaseRank cs.phase := by
  cases hstep with
  | beginSaveAppend hidle _ _ _ _ => exact False.elim (hne hidle)
  | beginSaveSwitch hidle _ _ _ _ => exact False.elim (hne hidle)
  | eraseOk hph _ _ => simp [hph, phaseRank]
  | eraseFailPowerCut hph _ _ => simp [hph, phaseRank]
  | writeOk hph _ _ _ _ _ _ => simp [hph, phaseRank]
  | writeFailPartial hph _ _ _ => simp [hph, phaseRank]
  | ackSave hph _ => simp [hph, phaseRank]
  | rebootKeepAcked =>
    cases hp : cs.phase with
    | idle => exact False.elim (hne hp)
    | erasing _ _ _ _ _ _ _ => simp [phaseRank]
    | writing _ _ _ _ _ _ => simp [phaseRank]
    | writeLanded _ _ _ _ _ => simp [phaseRank]
  | rebootRecoverLanded hph => simp [hph, phaseRank]

/-- **Compositional Save Termination Liveness (`□ (¬idle → ◇ idle)`)**:
Under Lentil weak fairness of `saveProgressAction g`, every in-flight save
operation in the composed extracted `store_core` + `NorFlash` system eventually
completes (either committing or cleanly aborting/rebooting to `.idle`) in at
most 3 non-stuttering transitions. -/
theorem extracted_flashstore_save_liveness
    (g : FlashGeometry)
    (e : Lentil.exec ComposedState)
    (hNext : ∀ n, (extractedFlashStoreRTS g).next () (e n) (e (n + 1)) ∨ e (n + 1) = e n)
    (hFair : Lentil.weak_fairness (saveProgressAction g) e 0) :
    Lentil.always
      (fun e i => (e i).phase ≠ .idle →
        Lentil.eventually (Lentil.state_pred (fun cs => cs.phase = .idle)) e i)
      e 0 := by
  have haux : ∀ r k, phaseRank (e k).phase ≤ r →
      Lentil.eventually (Lentil.state_pred (fun cs => cs.phase = .idle)) e k := by
    intro r
    induction r with
    | zero =>
      intro k hr
      refine ⟨0, ?_⟩
      simp only [Lentil.state_pred, Nat.add_zero]
      cases hp : (e k).phase with
      | idle => rfl
      | erasing _ _ _ _ _ _ _ => simp [hp, phaseRank] at hr
      | writing _ _ _ _ _ _ => simp [hp, phaseRank] at hr
      | writeLanded _ _ _ _ _ => simp [hp, phaseRank] at hr
    | succ r ih =>
      intro k hr
      by_cases hidle : (e k).phase = .idle
      · exact ⟨0, by simpa [Lentil.state_pred] using hidle⟩
      · -- While remaining non-idle, `saveProgressAction g` is always enabled
        -- (e.g. via `ComposedStep.rebootKeepAcked`), so by weak fairness a
        -- `saveProgressAction` step must eventually fire, strictly decreasing
        -- `phaseRank` to `≤ r`.
        by_contra hNeverIdle
        have hNeAll : ∀ d, (e (k + d)).phase ≠ .idle := by
          intro d heq
          exact hNeverIdle ⟨d, by simpa [Lentil.state_pred] using heq⟩
        have hEnAll : ∀ d, Lentil.tl_enabled (saveProgressAction g) e (k + d) := by
          intro d
          exact ⟨{ e (k + d) with pos := { (e (k + d)).pos with append := none }, phase := .idle },
            hNeAll d, ComposedStep.rebootKeepAcked⟩
        have hWF := (congrFun (congrFun Lentil.wf_as_implies e) 0).mp hFair
        have hInfOften : Lentil.always (Lentil.eventually (Lentil.action_pred (saveProgressAction g))) e 0 :=
          hWF ⟨k, fun d => by simpa [Nat.zero_add] using hEnAll d⟩
        obtain ⟨d, hAct⟩ := hInfOften k
        have hStep : saveProgressAction g (e (k + d)) (e (k + d + 1)) := by
          simpa [Lentil.action_pred, Nat.zero_add] using hAct
        -- `phaseRank` never increases while staying non-idle from `k` to `k + d`.
        have hRankMono : ∀ m, phaseRank (e (k + m)).phase ≤ phaseRank (e k).phase := by
          intro m
          induction m with
          | zero => exact Nat.le_refl _
          | succ j ihj =>
            rcases hNext (k + j) with ⟨_, hst⟩ | heq
            · have hdec := composedStep_nonidle_decr_rank (hNeAll j) hst
              have : k + (j + 1) = k + j + 1 := by omega
              rw [this]; omega
            · have : k + (j + 1) = k + j + 1 := by omega
              rw [this, heq]; exact ihj
        have hDec := composedStep_nonidle_decr_rank hStep.1 hStep.2
        have hLe : phaseRank (e (k + d + 1)).phase ≤ r := by
          have := hRankMono d
          omega
        obtain ⟨m, hm⟩ := ih (k + d + 1) hLe
        exact hNeverIdle ⟨d + 1 + m, by simpa [Lentil.state_pred, Nat.add_assoc] using hm⟩
  intro k _
  exact haux (phaseRank (e (0 + k)).phase) (0 + k) (Nat.le_refl _)

/-! ## 2. Kernel-Implementation-Level Recovery Liveness (`store_core.scan_step`) -/

/-- Purely implementation-level step relation executing the **actual
Aeneas-extracted `store_core.scan_step`** on arbitrary flash read buffers. -/
def scanStepAction (g : FlashGeometry) : Lentil.action ScanState :=
  fun st st' => st.phase ≠ .Done ∧ ∃ (buf : Slice U8) (res : ScanState × ScanAction),
    scan_step g st buf = ok res ∧ res.1 = st'

/-- **Kernel-Implementation-Level Recovery Invariant (`□ ScanInv`)**:
Every infinite execution driven by extracted `store_core.scan_step` (with
stuttering) starting from `ScanInv g (e 0)` maintains `ScanInv g` at every
step. -/
theorem extracted_flashstore_recovery_always_inv
    (g : FlashGeometry) (hlay : layout_ok g = ok true) (hdiv : AlignDivides g)
    (e : Lentil.exec ScanState)
    (hInit : ScanInv g (e 0))
    (hNext : ∀ n, scanStepAction g (e n) (e (n + 1)) ∨ e (n + 1) = e n) :
    Lentil.always (Lentil.state_pred (ScanInv g)) e 0 := by
  intro k
  simp only [Lentil.state_pred, Nat.zero_add]
  induction k with
  | zero => exact hInit
  | succ n ih =>
    rcases hNext n with ⟨hnd, buf, res, hok, hst'⟩ | heq
    · have hp := H5iAppLib.post_of_ok
        (scan_step_safe_and_terminates g (e n) buf hlay hdiv ih hnd) hok
      rw [← hst']
      exact hp.1
    · rw [heq]; exact ih

/-- **Kernel-Implementation-Level Recovery Liveness (`◇ (phase = Done)`)**:
Purely over the **actual Aeneas-extracted `store_core.scan_step`**, starting
from any well-formed `ScanState` (`ScanInv g (e 0)`, such as the output of
`scan_init g`), Lentil weak fairness of `scanStepAction g` guarantees that
recovery scanning eventually reaches `ScanPhase.Done` (terminating in bounded
steps via the strictly decreasing `scanMeasure g`). -/
theorem extracted_flashstore_recovery_liveness
    (g : FlashGeometry) (hlay : layout_ok g = ok true) (hdiv : AlignDivides g)
    (buf0 : Slice U8)
    (e : Lentil.exec ScanState)
    (hInit : ScanInv g (e 0))
    (hNext : ∀ n, scanStepAction g (e n) (e (n + 1)) ∨ e (n + 1) = e n)
    (hFair : Lentil.weak_fairness (scanStepAction g) e 0) :
    Lentil.eventually (Lentil.state_pred (fun st => st.phase = .Done)) e 0 := by
  have hInvAll := extracted_flashstore_recovery_always_inv g hlay hdiv e hInit hNext
  have haux : ∀ m k, scanMeasure g (e k) ≤ m →
      Lentil.eventually (Lentil.state_pred (fun st => st.phase = .Done)) e k := by
    intro m
    induction m with
    | zero =>
      intro k hm
      refine ⟨0, ?_⟩
      simp only [Lentil.state_pred, Nat.add_zero]
      cases hp : (e k).phase with
      | Done => rfl
      | ReloadBest => simp [scanMeasure, hp] at hm
      | ReadBody _ => simp [scanMeasure, hp] at hm
      | ReadHeader => simp [scanMeasure, hp] at hm
    | succ m ih =>
      intro k hm
      by_cases hdone : (e k).phase = .Done
      · exact ⟨0, by simpa [Lentil.state_pred] using hdone⟩
      · by_contra hNeverDone
        have hNdAll : ∀ d, (e (k + d)).phase ≠ .Done := by
          intro d heq
          exact hNeverDone ⟨d, by simpa [Lentil.state_pred] using heq⟩
        have hEnAll : ∀ d, Lentil.tl_enabled (scanStepAction g) e (k + d) := by
          intro d
          have hinv_d : ScanInv g (e (k + d)) := by
            simpa [Lentil.state_pred, Nat.zero_add] using hInvAll (k + d)
          obtain ⟨res, hok⟩ := H5iAppLib.ok_of
            (scan_step_safe_and_terminates g (e (k + d)) buf0 hlay hdiv hinv_d (hNdAll d))
          exact ⟨res.1, hNdAll d, buf0, res, hok, rfl⟩
        have hWF := (congrFun (congrFun Lentil.wf_as_implies e) 0).mp hFair
        have hInfOften : Lentil.always (Lentil.eventually (Lentil.action_pred (scanStepAction g))) e 0 :=
          hWF ⟨k, fun d => by simpa [Nat.zero_add] using hEnAll d⟩
        obtain ⟨d, hAct⟩ := hInfOften k
        have hStep : scanStepAction g (e (k + d)) (e (k + d + 1)) := by
          simpa [Lentil.action_pred, Nat.zero_add] using hAct
        have hMeasMono : ∀ j, scanMeasure g (e (k + j)) ≤ scanMeasure g (e k) := by
          intro j
          induction j with
          | zero => exact Nat.le_refl _
          | succ t iht =>
            rcases hNext (k + t) with ⟨hnd, buf, res, hok, hst'⟩ | heq
            · have hinv_t : ScanInv g (e (k + t)) := by
                simpa [Lentil.state_pred, Nat.zero_add] using hInvAll (k + t)
              have hp := H5iAppLib.post_of_ok
                (scan_step_safe_and_terminates g (e (k + t)) buf hlay hdiv hinv_t hnd) hok
              have hdec : scanMeasure g (e (k + t + 1)) < scanMeasure g (e (k + t)) := by
                rw [← hst']; exact hp.2.2
              have : k + (t + 1) = k + t + 1 := by omega
              rw [this]; omega
            · have : k + (t + 1) = k + t + 1 := by omega
              rw [this, heq]; exact iht
        obtain ⟨hnd, buf, res, hok, hst'⟩ := hStep
        have hinv_d : ScanInv g (e (k + d)) := by
          simpa [Lentil.state_pred, Nat.zero_add] using hInvAll (k + d)
        have hp := H5iAppLib.post_of_ok
          (scan_step_safe_and_terminates g (e (k + d)) buf hlay hdiv hinv_d hnd) hok
        have hDec : scanMeasure g (e (k + d + 1)) < scanMeasure g (e (k + d)) := by
          rw [← hst']; exact hp.2.2
        have hLe : scanMeasure g (e (k + d + 1)) ≤ m := by
          have := hMeasMono d
          omega
        obtain ⟨j, hj⟩ := ih (k + d + 1) hLe
        exact hNeverDone ⟨d + 1 + j, by simpa [Lentil.state_pred, Nat.add_assoc] using hj⟩
  exact haux (scanMeasure g (e 0)) 0 (Nat.le_refl _)

end H5iAppVeil.Embedded.FlashStoreExtracted
