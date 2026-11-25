import Veil.Core.Tools.ModelChecker.TransitionSystem
import Lentil

/-!
# Cross-State Refinement for Veil Transition Systems (`H5iAppVeil.Refinement`)

Veil's `Veil.RelationalTransitionSystem.reachable_inclusion` relates two
transition systems over the *same* state type `σ`. Connecting a concrete runtime
(such as `DeviceEngine`, whose state has phases, flash, pending buffers, and
waker versions) to an abstract `Kernel` requires mapping across *different*
state types `f : σ_c → σ_a` with stuttering.

This minimal module adds that cross-state stuttering refinement lemma directly
on top of Veil's `RelationalTransitionSystem` and Lentil's `Lentil.always`.
-/

namespace H5iAppVeil

/-- Concrete `Veil.RelationalTransitionSystem` `c` stuttering-refines abstract
`a` via state projection `f : σ_c → σ_a` and theory mapping `g : ρ_c → ρ_a`. -/
structure StutterRefines {ρ_c σ_c l_c ρ_a σ_a l_a : Type}
    (g : ρ_c → ρ_a) (f : σ_c → σ_a)
    (c : Veil.RelationalTransitionSystem ρ_c σ_c l_c)
    (a : Veil.RelationalTransitionSystem ρ_a σ_a l_a) : Prop where
  assumptions_map : ∀ th, c.assumptions th → a.assumptions (g th)
  init_map : ∀ th s, c.assumptions th → c.init th s → a.init (g th) (f s)
  step_map : ∀ th s s', c.reachable th s → c.next th s s' →
    a.next (g th) (f s) (f s') ∨ f s' = f s

/-- Stuttering refinement preserves `Veil.RelationalTransitionSystem.reachable`. -/
theorem StutterRefines.reachable {ρ_c σ_c l_c ρ_a σ_a l_a : Type}
    {g : ρ_c → ρ_a} {f : σ_c → σ_a}
    {c : Veil.RelationalTransitionSystem ρ_c σ_c l_c}
    {a : Veil.RelationalTransitionSystem ρ_a σ_a l_a}
    (href : StutterRefines g f c a) {th : ρ_c} {s : σ_c}
    (hr : c.reachable th s) : a.reachable (g th) (f s) := by
  induction hr with
  | init s hass hinit =>
    exact .init (f s) (href.assumptions_map th hass) (href.init_map th s hass hinit)
  | step s s' hr hn ih =>
    rcases href.step_map th s s' hr hn with hnext | heq
    · exact .step _ _ ih hnext
    · rw [heq]; exact ih

/-- Any invariant holding on all reachable states of the abstract Veil system
transfers to `Lentil.always (Lentil.state_pred (fun s => I (f s)))` on every
infinite execution of the concrete system. -/
theorem StutterRefines.lentil_always {ρ_c σ_c l_c ρ_a σ_a l_a : Type}
    {g : ρ_c → ρ_a} {f : σ_c → σ_a}
    {c : Veil.RelationalTransitionSystem ρ_c σ_c l_c}
    {a : Veil.RelationalTransitionSystem ρ_a σ_a l_a}
    (href : StutterRefines g f c a) {th : ρ_c} {I : σ_a → Prop}
    (hInv : ∀ s_a, a.reachable (g th) s_a → I s_a)
    (e : Lentil.exec σ_c)
    (hass : c.assumptions th)
    (hInit : c.init th (e 0))
    (hNext : ∀ n, c.next th (e n) (e (n + 1)) ∨ e (n + 1) = e n) :
    Lentil.always (Lentil.state_pred (fun s => I (f s))) e 0 := by
  have hreach : ∀ k, c.reachable th (e k) := by
    intro k
    induction k with
    | zero => exact .init _ hass hInit
    | succ n ih =>
      rcases hNext n with hn | heq
      · exact .step _ _ ih hn
      · rw [heq]; exact ih
  intro k
  simpa [Lentil.state_pred] using hInv (f (e k)) (href.reachable (hreach k))

end H5iAppVeil
