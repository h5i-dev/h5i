import Theorems
/-!
# Issue #190: link previews burn pastes

Before commit 632ddf2 a link preview's plain GET showed and deleted a
burn-after-reading paste; the fix asks for `confirm_burn=1` first.
`preview_fixed` proves `PreviewSafe` for today's kernel; `preview_broken`
refutes it for the old one on a state one `Create` reaches.
-/
open Aeneas Aeneas.Std Result wastebin_kernel wastebin_kernel.Spec wastebin_kernel.Commands
  wastebin_kernel.Theorems H5iAppLib

namespace wastebin_kernel.Preview

/-- The paste page without confirmation is safe for link previews. -/
theorem preview_fixed : PreviewSafe transition := by
  intro a s k key ws r hr h
  have hi := reachable_inv (.inl rfl) hr
  simp only [transition] at h
  obtain ⟨p, hp, hv⟩ := post_of_ok (view_spec a s k false key) h ws r rfl
  have hpm := (findSlug_mem hp).1
  rcases hv with ⟨-, -, hws, rfl⟩ | ⟨hb, hrp⟩
  · rw [hws]
    exact ⟨fun q hq _ => by simpa [applyAll] using hq, by intro v hv; cases hv⟩
  · have hb : p.burn = false := by simpa using hb
    rcases hrp with ⟨he, hws, rfl⟩ | ⟨-, -, v, rfl, hs, hws⟩
    · -- An expired paste is deleted; every other paste has another id.
      refine ⟨fun q hq hqe => ?_, by intro v hv; cases hv⟩
      rw [hws, show [Write.DelPaste p.id] = [p.id].map .DelPaste from rfl, applyAll_dels]
      refine List.mem_filter.2 ⟨hq, ?_⟩
      have hne : q.id ≠ p.id := by
        intro hid
        rw [List.inj_on_of_nodup_map hi.ids hq hpm hid, he] at hqe
        cases hqe
      simpa using hne
    · refine ⟨fun q hq _ => ?_, fun v' hv' => ?_⟩
      · rw [hws, hb]; simpa [applyAll] using hq
      · cases hv'; rw [hs.2.2.1, hb]

/-! ## The counterexample -/

/-- A burn-after-reading paste with slug 7, owned by uid 1. -/
def p0 : Paste := ⟨0#u64, 7#u64, 1#u64, vecOf [], none, true, none⟩

def empty : Snapshot := ⟨⟨0#u64, 0#u64⟩, vecOf []⟩
def s0 : Snapshot := ⟨⟨1#u64, 1#u64⟩, vecOf [p0]⟩

/-- A first-time visitor whose request draws slug 7. -/
def author : Principal := ⟨vecOf [], 100#u64, 7#u64⟩
/-- A link preview bot. -/
def bot : Principal := ⟨vecOf [], 100#u64, 0#u64⟩

def createP0 : Command := .Create (vecOf []) none true none

theorem create_p0 :
    transition author empty createP0 ⦃ o => ∃ ws r, o = .Ok (ws, r) ∧
      ws.val = [.PutPaste p0, .SetCounter ⟨1#u64, 1#u64⟩] ⦄ := by
  simp only [transition, createP0]
  unfold create deadline owner_for
  step*
  all_goals simp_all [empty, author, p0, vecOf, core.num.U64.MAX, U64.rMax]
  step*
  simp_all
  refine ⟨?_, ?_, ?_⟩ <;> (apply UScalar.eq_of_val_eq; simp_all)

/-- One `Create` from the empty state reaches `s0`, in either variant. -/
theorem s0_reachable {T : Kernel} (hT : Variant T) : Reachable T (Snapshot.toSt s0) := by
  obtain ⟨o, ho, ws, r, rfl, hws⟩ := (WP.spec_equiv_exists _ _).1 create_p0
  have ho' : T author empty createP0 = ok (.Ok (ws, r)) := by
    rcases hT with rfl | rfl
    · exact ho
    · rw [pre190_eq]; exact ho
  have h0 : Snapshot.toSt empty = init := by simp [Snapshot.toSt, empty, init, vecOf]
  have hr : Reachable T (Snapshot.toSt empty) := h0 ▸ Steps.refl
  have h1 : applyAll (Snapshot.toSt empty) ws.val = Snapshot.toSt s0 := by
    rw [hws]; simp [applyAll, applyWrite, upsert, Snapshot.toSt, empty, s0, vecOf]
  exact h1 ▸ Steps.step hr ho'

/-- Before the fix, the bot's plain request shows the paste and burns it. -/
theorem pre190_bot_burns :
    transition_pre190 bot s0 (.View 7#u64 false none) ⦃ o => ∃ ws v, o = .Ok (ws, .Shown v) ∧
      v.burned = true ∧ ws.val = [.DelPaste 0#u64] ⦄ := by
  rw [pre190_eq]
  simp only
  unfold fetch read
  step*
  all_goals simp_all [s0, p0, bot, vecOf, isExpired]

/-- `PreviewSafe` is false for the code before the fix. -/
theorem preview_broken : ¬ PreviewSafe transition_pre190 := by
  intro hsafe
  obtain ⟨o, ho, ws, v, rfl, hb, -⟩ := (WP.spec_equiv_exists _ _).1 pre190_bot_burns
  have := (hsafe bot s0 7#u64 none ws (.Shown v) (s0_reachable (.inr rfl)) ho).2 v rfl
  rw [hb] at this
  cases this

end wastebin_kernel.Preview
