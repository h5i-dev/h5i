import H5iSpec.Refinement
import H5iSpec.Phase

/-!
L3, box-to-box noninterference (ROADMAP.md §V3): two confined processes on
one host influence each other exactly through paths one may write and the
other may read. When their compiled rulesets share no such path, box A's
activity is invisible to box B — proved by an unwinding argument over a
shared-filesystem semantics.

Three pieces:

- `noninterference`: the 2-safety theorem. B's observable filesystem is
  identical whether A's writes happened or not, provided `¬ Interferes`.
  This is the property no per-instance check can state — it quantifies over
  pairs of traces.
- `interferesCheck` + `interferesCheck_sound`: the side condition made
  *decidable*. Two rulesets interfere only through overlapping scopes, and
  scope overlap on path-beneath rules is prefix comparability — so a finite
  scan over rule pairs decides it. A host can run this over any two boxes'
  `policy.effective.json` files and cite the theorem.
- The instances: two agent-profile boxes fail the check through host-shared
  `/tmp` (and really do interfere — both directions shown); two
  workspace-only boxes with distinct worktrees pass it, so the
  noninterference theorem applies to them.

Scope, stated: observations are the B-readable projection of the final
filesystem; write values are fixed in the trace, so read-to-write feedback
is not modeled. That is the standard first unwinding altitude; the seL4-style
intransitive refinement is future work.
-/

namespace H5iSpec

/-- The shared filesystem: contents per path. `Nat` is opaque data. -/
abbrev SharedFs := FsPath → Option Nat

/-- Who acted. -/
inductive BoxId where
  | A
  | B
deriving Repr, DecidableEq

/-- One write event: box `who` attempts to write `v` at `p`. Reads are the
observation at the end, not events — see the module docstring. -/
structure WriteEv where
  who : BoxId
  p : FsPath
  v : Nat

/-- The kernel's answer to one write: applied iff the writer's ruleset
allows it; a denied write is a no-op (`EACCES`), never a partial effect. -/
def applyWrite (rs : Ruleset) (fs : SharedFs) (p : FsPath) (v : Nat) :
    SharedFs :=
  if rs.allows p .write then fun q => if q = p then some v else fs q else fs

/-- Run a trace of writes, each under its box's ruleset. -/
def runTrace (rsA rsB : Ruleset) (fs : SharedFs) : List WriteEv → SharedFs
  | [] => fs
  | e :: t =>
    runTrace rsA rsB
      (applyWrite (match e.who with | .A => rsA | .B => rsB) fs e.p e.v) t

/-- B's view: two filesystems agree wherever B may read. -/
def ObsEqB (rsB : Ruleset) (fs₁ fs₂ : SharedFs) : Prop :=
  ∀ p, rsB.allows p .read = true → fs₁ p = fs₂ p

/-- The world with box A erased. -/
def eraseA (t : List WriteEv) : List WriteEv :=
  t.filter fun e => e.who == .B

/-- Interference: a path A may write and B may read. -/
def Interferes (rsA rsB : Ruleset) : Prop :=
  ∃ p, rsA.allows p .write = true ∧ rsB.allows p .read = true

/-- A-invisibility: under `¬ Interferes`, an A-write changes nothing B can
read. -/
theorem applyWriteA_invisible {rsA rsB : Ruleset} {fs : SharedFs}
    {p : FsPath} {v : Nat} (hni : ¬ Interferes rsA rsB) :
    ObsEqB rsB (applyWrite rsA fs p v) fs := by
  intro q hq
  unfold applyWrite
  by_cases hw : rsA.allows p .write = true
  · simp only [if_pos hw]
    by_cases hqp : q = p
    · exact absurd ⟨p, hw, hqp ▸ hq⟩ hni
    · simp [hqp]
  · simp [hw]

/-- Congruence: the same guarded write on B-equivalent filesystems keeps
them B-equivalent. -/
theorem applyWrite_obs_eq {rs rsB : Ruleset} {fs₁ fs₂ : SharedFs}
    {p : FsPath} {v : Nat} (heq : ObsEqB rsB fs₁ fs₂) :
    ObsEqB rsB (applyWrite rs fs₁ p v) (applyWrite rs fs₂ p v) := by
  intro q hq
  unfold applyWrite
  by_cases hw : rs.allows p .write = true
  · simp only [if_pos hw]
    by_cases hqp : q = p
    · simp [hqp]
    · simpa [hqp] using heq q hq
  · simp only [if_neg hw]
    exact heq q hq

/-- The unwinding induction, generalized over ObsEqB-related starting
states: each event preserves B-equivalence between the full world and the
A-erased one. -/
theorem run_obs_eq {rsA rsB : Ruleset} (hni : ¬ Interferes rsA rsB) :
    ∀ (t : List WriteEv) (fs₁ fs₂ : SharedFs), ObsEqB rsB fs₁ fs₂ →
      ObsEqB rsB (runTrace rsA rsB fs₁ t)
        (runTrace rsA rsB fs₂ (eraseA t)) := by
  intro t
  induction t with
  | nil => intro fs₁ fs₂ h; exact h
  | cons e t ih =>
    intro fs₁ fs₂ heq
    obtain ⟨who, ep, ev⟩ := e
    cases who with
    | A =>
      -- The A-event vanishes from the erased trace; its write is invisible
      -- to B, so B-equivalence survives into the tail.
      show ObsEqB rsB (runTrace rsA rsB (applyWrite rsA fs₁ ep ev) t)
        (runTrace rsA rsB fs₂ (eraseA t))
      exact ih _ fs₂ fun q hq =>
        (applyWriteA_invisible hni q hq).trans (heq q hq)
    | B =>
      -- The B-event stays in both worlds and lands identically on both
      -- sides.
      show ObsEqB rsB (runTrace rsA rsB (applyWrite rsB fs₁ ep ev) t)
        (runTrace rsA rsB (applyWrite rsB fs₂ ep ev) (eraseA t))
      exact ih _ _ (applyWrite_obs_eq heq)

/-- **Box-to-box noninterference** (§V3, L3): when no path is A-writable and
B-readable, B's observable filesystem after any trace equals its observable
filesystem in the world where A never acted. -/
theorem noninterference {rsA rsB : Ruleset} (hni : ¬ Interferes rsA rsB)
    (t : List WriteEv) (fs : SharedFs) :
    ObsEqB rsB (runTrace rsA rsB fs t) (runTrace rsA rsB fs (eraseA t)) :=
  run_obs_eq hni t fs fs fun _ _ => rfl

/-! ### The decidable side condition -/

/-- Two prefixes of one list are comparable. This is why scope overlap on
path-beneath rules reduces to a finite check. -/
theorem isPrefixOf_comparable :
    ∀ {l₁ l₂ l : List String}, l₁.isPrefixOf l = true →
      l₂.isPrefixOf l = true →
      l₁.isPrefixOf l₂ = true ∨ l₂.isPrefixOf l₁ = true := by
  intro l₁ l₂ l
  induction l generalizing l₁ l₂ with
  | nil =>
    intro h₁ h₂
    cases l₁ with
    | nil => exact Or.inl rfl
    | cons a as => simp [List.isPrefixOf] at h₁
  | cons c cs ih =>
    intro h₁ h₂
    cases l₁ with
    | nil => exact Or.inl (by cases l₂ <;> rfl)
    | cons a as =>
      cases l₂ with
      | nil => exact Or.inr rfl
      | cons b bs =>
        simp only [List.isPrefixOf, Bool.and_eq_true, beq_iff_eq] at h₁ h₂
        rcases ih h₁.2 h₂.2 with hab | hba
        · exact Or.inl (by
            simp only [List.isPrefixOf, Bool.and_eq_true, beq_iff_eq]
            exact ⟨h₁.1.trans h₂.1.symm, hab⟩)
        · exact Or.inr (by
            simp only [List.isPrefixOf, Bool.and_eq_true, beq_iff_eq]
            exact ⟨h₂.1.trans h₁.1.symm, hba⟩)

/-- Do two path-beneath scopes overlap (share any path)? -/
def scopesOverlap (g₁ g₂ : FsPath) : Bool :=
  g₁.isPrefixOf g₂ || g₂.isPrefixOf g₁

/-- The finite interference check: some A rule carrying write whose scope
overlaps some B rule carrying read. Runs over two boxes'
`policy.effective.json` grant lists via `compileLandlock`. -/
def interferesCheck (rsA rsB : Ruleset) : Bool :=
  (rsA.filter fun r => r.access.contains .write).any fun ra =>
    (rsB.filter fun r => r.access.contains .read).any fun rb =>
      scopesOverlap ra.path rb.path

/-- **Checker soundness**: a clean check implies no interference, so
`noninterference` applies. (The converse direction is deliberately not
claimed: overlapping scopes need not produce an actual shared path when a
rule scope is unreachable — the checker fails safe.) -/
theorem interferesCheck_sound {rsA rsB : Ruleset}
    (h : interferesCheck rsA rsB = false) : ¬ Interferes rsA rsB := by
  rintro ⟨p, hwA, hrB⟩
  simp only [Ruleset.allows, List.any_eq_true, Bool.and_eq_true] at hwA hrB
  obtain ⟨ra, hraMem, hraScope, hraW⟩ := hwA
  obtain ⟨rb, hrbMem, hrbScope, hrbR⟩ := hrB
  have hcomp := isPrefixOf_comparable hraScope hrbScope
  have : interferesCheck rsA rsB = true := by
    simp only [interferesCheck, List.any_eq_true, List.mem_filter]
    refine ⟨ra, ⟨hraMem, hraW⟩, rb, ⟨hrbMem, hrbR⟩, ?_⟩
    simp only [scopesOverlap, Bool.or_eq_true]
    rcases hcomp with hab | hba
    · exact Or.inl hab
    · exact Or.inr hba
  rw [h] at this
  cases this

/-! ### The instances -/

/-- Two agent-profile boxes: the check fires on host-shared `/tmp`… -/
theorem agent_boxes_fail_check :
    interferesCheck runAgent runAgent = true := by decide

/-- …and the interference is real, not a checker artifact: `/tmp/x` is
writable by one and readable by the other. -/
theorem agent_boxes_interfere : Interferes runAgent runAgent :=
  ⟨["tmp", "x"], by decide, by decide⟩

/-- Two workspace-only boxes on distinct worktrees. -/
def boxOne : Ruleset := [⟨["boxes", "one", "work"], [.read, .write]⟩]
def boxTwo : Ruleset := [⟨["boxes", "two", "work"], [.read, .write]⟩]

/-- Their check is clean, in both directions… -/
theorem disjoint_boxes_pass_check :
    interferesCheck boxOne boxTwo = false
      ∧ interferesCheck boxTwo boxOne = false := by decide

/-- …so the noninterference theorem applies to them: box one's activity is
invisible to box two, by `interferesCheck_sound` and `noninterference`. -/
theorem disjoint_boxes_noninterfere (t : List WriteEv) (fs : SharedFs) :
    ObsEqB boxTwo (runTrace boxOne boxTwo fs t)
      (runTrace boxOne boxTwo fs (eraseA t)) :=
  noninterference (interferesCheck_sound disjoint_boxes_pass_check.1) t fs

end H5iSpec
