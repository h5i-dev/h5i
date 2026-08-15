import H5iSpec.Model

/-!
First theorems against the model (ROADMAP.md §V3 — a down payment; the L0
mechanism semantics and the phase theorems are step 3). Each is stated over
*all* inputs, which is exactly what the per-instance DRT cannot say.

The readonly theorem is deliberately conditional, and the condition is the
finding: `work_readonly` alone does NOT keep `$WORK` out of the rw grants —
an `fs_write` entry that spells the workspace path survives, because the Rust
only strips the literal `"$WORK"` and the implicit grant. The caller
(`env::shell`) discharges the condition by stripping writable grants that
reach the worktree; this theorem is the written form of that obligation.
-/

namespace H5iSpec

/-- Denied egress always gets a fresh network namespace, whatever the shape. -/
theorem net_deny_forces_netns (i : DrtInput)
    (h : i.profile.net_mode = .deny) :
    (computeEffective i).namespaces.net = true := by
  show (i.profile.net_mode == .deny || i.shape.force_netns) = true
  rw [h]
  simp

/-- The supervised tier's shape (`force_netns`) gets one too. -/
theorem forced_netns_is_honored (i : DrtInput)
    (h : i.shape.force_netns = true) :
    (computeEffective i).namespaces.net = true := by
  show (i.profile.net_mode == .deny || i.shape.force_netns) = true
  rw [h]
  simp

/-- A readonly session keeps `$WORK` out of the rw grants — PROVIDED no
`fs_write` entry expands to the workspace path itself. The proviso is a real
caller obligation, not proof convenience; see the module docstring. -/
theorem readonly_work_not_rw (i : DrtInput)
    (hro : i.runtime.work_readonly = true)
    (hw : ∀ s ∈ i.profile.fs_write,
      expandTilde i.world.home s ≠ i.work) :
    i.work ∉ (computeEffective i).landlock.rw := by
  intro hmem
  have hmem' : i.work
      ∈ ((i.profile.fs_write.filter (· != "$WORK")).map
          (expandTilde i.world.home)).filter i.world.existsPath := by
    have : (computeEffective i).landlock.rw
        = ((i.profile.fs_write.filter (· != "$WORK")).map
            (expandTilde i.world.home)).filter i.world.existsPath := by
      show (landlockOf i).rw = _
      simp [landlockOf, hro]
    rwa [this] at hmem
  obtain ⟨s, hs, hexp⟩ := List.mem_map.mp (List.mem_filter.mp hmem').1
  exact hw s (List.mem_filter.mp hs).1 hexp

/-- A readonly session grants `$WORK` read-only instead: the observer can
still read the worktree, whatever the world contains. -/
theorem readonly_work_is_ro (i : DrtInput)
    (hro : i.runtime.work_readonly = true) :
    i.work ∈ (computeEffective i).landlock.ro := by
  show i.work ∈ (landlockOf i).ro
  simp [landlockOf, hro]

/-- Every write grant the profile asked for is either granted or recorded as
skipped — nothing is silently dropped (the dump says what was asked for and
not given). The read side is symmetric. -/
theorem write_grant_accounted (i : DrtInput) (s : String)
    (hs : s ∈ i.profile.fs_write) (hnw : s ≠ "$WORK") :
    expandTilde i.world.home s ∈ (computeEffective i).landlock.rw
      ∨ expandTilde i.world.home s
          ∈ (computeEffective i).landlock.skipped_missing := by
  have hmem : expandTilde i.world.home s
      ∈ (i.profile.fs_write.filter (· != "$WORK")).map
        (expandTilde i.world.home) :=
    List.mem_map.mpr ⟨s, List.mem_filter.mpr ⟨hs, by simpa using hnw⟩, rfl⟩
  by_cases hex : i.world.existsPath (expandTilde i.world.home s) = true
  · refine Or.inl ?_
    show _ ∈ (landlockOf i).rw
    simp only [landlockOf, List.mem_append, List.mem_filter]
    exact Or.inr ⟨hmem, hex⟩
  · refine Or.inr ?_
    show _ ∈ (landlockOf i).skipped_missing
    simp only [landlockOf, List.mem_append, List.mem_filter]
    exact Or.inl ⟨hmem, by simpa using hex⟩

end H5iSpec
