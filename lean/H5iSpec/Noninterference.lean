import H5iSpec.Landlock

/-!
The box-to-box interference checker: two confined processes on one host
influence each other through the filesystem exactly through paths one may
write and the other may read. `interferesCheck` decides the absence of such
a path from two boxes' compiled rulesets, and `interferesCheck_sound` is
the direction the product relies on: a clean check really means no shared
writable-readable path.

This file is the specification of the Rust `effective::interferes`
(`crates/h5i-sandbox/src/effective.rs`), which the receipt layer runs over
pairs of `policy.effective.json` files; `tests/interferes_drt.rs` holds the
two implementations to the same verdicts.

Scope, stated: `Interferes` is a predicate on rulesets — a path A may write
and B may read exists — not a trace property. The end-to-end statement
("without such a path, A's activity is invisible to B") moves to the H5iFs
authority machine (ROADMAP §V3), whose filesystem semantics has the object
identity a trace-level claim needs. The earlier path-level unwinding proof
lives in git history (`lean/H5iSpec/Noninterference.lean` before the
2026-08-16 pivot).
-/

namespace H5iSpec

/-- Interference: a path A may write and B may read. -/
def Interferes (rsA rsB : Ruleset) : Prop :=
  ∃ p, rsA.allows p .write = true ∧ rsB.allows p .read = true

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

/-- **Checker soundness**: a clean check implies no interference. (The
converse direction is deliberately not claimed: overlapping scopes need not
produce an actual shared path when a rule scope is unreachable — the
checker fails safe.) -/
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

/-- The agent profile's shape: the worktree plus host-shared `/tmp`
(`agent-profile-grants-shared-tmp` is policy, not accident). -/
def agentBox : Ruleset :=
  [⟨["work"], [.read, .write]⟩, ⟨["tmp"], [.read, .write]⟩]

/-- Two agent-profile boxes: the check fires on host-shared `/tmp`… -/
theorem agent_boxes_fail_check :
    interferesCheck agentBox agentBox = true := by decide

/-- …and the interference is real, not a checker artifact: `/tmp/x` is
writable by one and readable by the other. -/
theorem agent_boxes_interfere : Interferes agentBox agentBox :=
  ⟨["tmp", "x"], by decide, by decide⟩

/-- Two workspace-only boxes on distinct worktrees. -/
def boxOne : Ruleset := [⟨["boxes", "one", "work"], [.read, .write]⟩]
def boxTwo : Ruleset := [⟨["boxes", "two", "work"], [.read, .write]⟩]

/-- Their check is clean, in both directions, so by
`interferesCheck_sound` neither box can write a path the other reads. -/
theorem disjoint_boxes_pass_check :
    interferesCheck boxOne boxTwo = false
      ∧ interferesCheck boxTwo boxOne = false := by decide

end H5iSpec
