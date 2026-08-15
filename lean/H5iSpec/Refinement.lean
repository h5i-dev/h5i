import H5iSpec.Model
import H5iSpec.Landlock

/-!
L2, the refinement layer (ROADMAP.md §V3): the bridge from the executable
model's output (`EffectiveConfig`, grant lists as strings) to the L0 Landlock
semantics (`Ruleset` over component paths).

`compileLandlock` mirrors what `build_confined_command` does with the same
lists: ro grants become read rules, rw grants become read+write rules, one
ruleset, one `restrict_self`. Against it, two theorems:

- `compile_sound` — **the safety direction**: every (path, access) the
  compiled ruleset allows, the abstract policy allows. The world can only
  *narrow* the compiled ruleset (a missing grant path is skipped), and the
  rights mapping never invents access, so the sandbox never exceeds the
  policy. This holds for every world.
- `compile_complete_of_world_full` — **the usability direction**, and
  deliberately conditional: when every granted path exists on the host, the
  compiled ruleset allows everything the policy allows. On a host with
  missing grant paths completeness genuinely fails (the run is narrower than
  the policy), which is the fail-closed direction and exactly what
  `skipped_missing` reports.

The abstract denotation (`abstractAllows`) is the *resolved* policy's intent:
world-independent except for tilde expansion, which is resolution against the
environment, not enforcement.
-/

namespace H5iSpec

/-- Parse an absolute path string into components: `"/home/u"` becomes
`["home","u"]`. Repeated separators collapse; the kernel's inode-level view
is the L0-fidelity assumption §V5 states. -/
def parsePath (s : String) : FsPath :=
  (s.splitOn "/").filter (· != "")

/-- What `build_confined_command` builds from the dump's grant lists:
read rules for `ro`, read+write rules for `rw` — `path_beneath_rules(ro,
from_read)` and `path_beneath_rules(rw, from_all)`, in Lean. -/
def compileLandlock (ll : LandlockEffective) : Ruleset :=
  ll.ro.map (fun g => ⟨parsePath g, [.read]⟩)
    ++ ll.rw.map (fun g => ⟨parsePath g, [.read, .write]⟩)

/-- The paths the resolved policy grants write on: `$WORK` (unless the
session is readonly) plus every `fs_write` entry, tilde-expanded. No
exists-filter: this is intent, not enforcement. -/
def writeGrantPaths (i : DrtInput) : List String :=
  (if i.runtime.work_readonly then [] else [i.work])
    ++ (i.profile.fs_write.filter (· != "$WORK")).map (expandTilde i.world.home)

/-- The paths the resolved policy grants read on: everything writable (a
Landlock rw grant carries read), every `fs_read` entry, and `$WORK`
read-only when the session is readonly. -/
def readGrantPaths (i : DrtInput) : List String :=
  writeGrantPaths i
    ++ i.profile.fs_read.map (expandTilde i.world.home)
    ++ (if i.runtime.work_readonly then [i.work] else [])

/-- The abstract policy denotation: `a` on `p` is allowed iff `p` lies
beneath a path the policy grants that access on. -/
def abstractAllows (i : DrtInput) (p : FsPath) (a : Access) : Bool :=
  match a with
  | .read => (readGrantPaths i).any fun g => p.beneath (parsePath g)
  | .write => (writeGrantPaths i).any fun g => p.beneath (parsePath g)

/-- A granted ro path in the model's output came from the policy's read
grants: the exists-filter only ever removes. -/
theorem ro_grant_from_policy {i : DrtInput} {g : String}
    (h : g ∈ (computeEffective i).landlock.ro) : g ∈ readGrantPaths i := by
  have h' : g ∈ (landlockOf i).ro := h
  simp only [landlockOf, List.mem_append, List.mem_filter] at h'
  simp only [readGrantPaths, List.mem_append]
  rcases h' with ⟨hmem, _⟩ | hwork
  · exact Or.inl (Or.inr hmem)
  · exact Or.inr hwork

/-- A granted rw path came from the policy's write grants. -/
theorem rw_grant_from_policy {i : DrtInput} {g : String}
    (h : g ∈ (computeEffective i).landlock.rw) : g ∈ writeGrantPaths i := by
  have h' : g ∈ (landlockOf i).rw := h
  simp only [landlockOf, List.mem_append, List.mem_filter] at h'
  simp only [writeGrantPaths, List.mem_append]
  rcases h' with hwork | ⟨hmem, _⟩
  · exact Or.inl hwork
  · exact Or.inr hmem

/-- Write grants carry read, abstractly. -/
theorem abstract_read_of_write {i : DrtInput} {g : String}
    (h : g ∈ writeGrantPaths i) : g ∈ readGrantPaths i :=
  List.mem_append.mpr (Or.inl (List.mem_append.mpr (Or.inl h)))

/-- **Compile soundness** (§V3, L2): every access the compiled Landlock
ruleset admits, the abstract policy admits — for every input and every
world. The sandbox `build_confined_command` installs from these lists never
exceeds the resolved policy. -/
theorem compile_sound (i : DrtInput) (p : FsPath) (a : Access)
    (h : (compileLandlock (computeEffective i).landlock).allows p a = true) :
    abstractAllows i p a = true := by
  simp only [compileLandlock, Ruleset.allows, List.any_eq_true,
    Bool.and_eq_true] at h
  obtain ⟨r, hr, hbeneath, haccess⟩ := h
  rcases List.mem_append.mp hr with hro | hrw
  · -- A read rule from an ro grant: its sole right is read.
    obtain ⟨g, hg, rfl⟩ := List.mem_map.mp hro
    cases a with
    | write =>
      have haccess' : ([Access.read] : List Access).contains Access.write = true :=
        haccess
      exact absurd haccess' (by decide)
    | read =>
      simp only [abstractAllows, List.any_eq_true]
      exact ⟨g, ro_grant_from_policy hg, hbeneath⟩
  · -- A read+write rule from an rw grant.
    obtain ⟨g, hg, rfl⟩ := List.mem_map.mp hrw
    cases a with
    | write =>
      simp only [abstractAllows, List.any_eq_true]
      exact ⟨g, rw_grant_from_policy hg, hbeneath⟩
    | read =>
      simp only [abstractAllows, List.any_eq_true]
      exact ⟨g, abstract_read_of_write (rw_grant_from_policy hg), hbeneath⟩

/-- **Conditional completeness**: when every granted path exists on the
host, the compiled ruleset admits everything the policy admits. The
hypothesis fails exactly when grants were skipped — the run is then narrower
than the policy, on purpose, and `skipped_missing` says so. -/
theorem compile_complete_of_world_full (i : DrtInput) (p : FsPath) (a : Access)
    (hfull : ∀ g ∈ readGrantPaths i, i.world.existsPath g = true)
    (h : abstractAllows i p a = true) :
    (compileLandlock (computeEffective i).landlock).allows p a = true := by
  -- A compiled rule covering `p` settles the goal.
  have compiled_allows :
      ∀ g : String,
        (g ∈ (computeEffective i).landlock.ro ∧ a = .read)
          ∨ g ∈ (computeEffective i).landlock.rw →
        p.beneath (parsePath g) = true →
        (compileLandlock (computeEffective i).landlock).allows p a = true := by
    rintro g (⟨hg, rfl⟩ | hg) hb
    · simp only [compileLandlock, Ruleset.allows, List.any_eq_true]
      refine ⟨⟨parsePath g, [.read]⟩,
        List.mem_append.mpr (Or.inl (List.mem_map.mpr ⟨g, hg, rfl⟩)), ?_⟩
      show (p.beneath (parsePath g)
        && ([Access.read] : List Access).contains .read) = true
      rw [hb]; decide
    · simp only [compileLandlock, Ruleset.allows, List.any_eq_true]
      refine ⟨⟨parsePath g, [.read, .write]⟩,
        List.mem_append.mpr (Or.inr (List.mem_map.mpr ⟨g, hg, rfl⟩)), ?_⟩
      cases a with
      | read =>
        show (p.beneath (parsePath g)
          && ([Access.read, Access.write] : List Access).contains .read) = true
        rw [hb]; decide
      | write =>
        show (p.beneath (parsePath g)
          && ([Access.read, Access.write] : List Access).contains .write) = true
        rw [hb]; decide
  -- Route every abstract grant to its compiled home; `hfull` keeps the
  -- exists-filter from dropping it.
  have to_rw : ∀ g ∈ writeGrantPaths i,
      g ∈ (computeEffective i).landlock.rw := by
    intro g hg
    have hex := hfull g (abstract_read_of_write hg)
    show g ∈ (landlockOf i).rw
    simp only [writeGrantPaths, List.mem_append] at hg
    simp only [landlockOf, List.mem_append, List.mem_filter]
    rcases hg with hwork | hf
    · exact Or.inl hwork
    · exact Or.inr ⟨hf, hex⟩
  have to_ro : ∀ g ∈ i.profile.fs_read.map (expandTilde i.world.home),
      g ∈ (computeEffective i).landlock.ro := by
    intro g hg
    have hex := hfull g
      (List.mem_append.mpr (Or.inl (List.mem_append.mpr (Or.inr hg))))
    show g ∈ (landlockOf i).ro
    simp only [landlockOf, List.mem_append, List.mem_filter]
    exact Or.inl ⟨hg, hex⟩
  have work_ro : i.runtime.work_readonly = true →
      i.work ∈ (computeEffective i).landlock.ro := by
    intro hro
    show i.work ∈ (landlockOf i).ro
    simp [landlockOf, hro]
  cases a with
  | write =>
    simp only [abstractAllows, List.any_eq_true] at h
    obtain ⟨g, hg, hb⟩ := h
    exact compiled_allows g (Or.inr (to_rw g hg)) hb
  | read =>
    simp only [abstractAllows, List.any_eq_true] at h
    obtain ⟨g, hg, hb⟩ := h
    simp only [readGrantPaths, List.mem_append] at hg
    rcases hg with (hw | hr) | hwork
    · exact compiled_allows g (Or.inr (to_rw g hw)) hb
    · exact compiled_allows g (Or.inl ⟨to_ro g hr, rfl⟩) hb
    · by_cases hro : i.runtime.work_readonly
      · have hgw : g = i.work := by
          rw [if_pos hro] at hwork
          simpa using hwork
        subst hgw
        exact compiled_allows i.work (Or.inl ⟨work_ro hro, rfl⟩) hb
      · rw [if_neg hro] at hwork
        simp at hwork

end H5iSpec
