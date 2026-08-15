import H5iSpec.Effective

/-!
L0, the Landlock fragment (ROADMAP.md §V3): the first mechanized semantics of
the mechanism h5i's kernel tiers stand on. Modeled at the level of Landlock's
documented contract, not kernel C:

- A **ruleset** is an allowlist: rights over path-beneath scopes. There are
  no deny rules; what no rule grants is denied. (`fs_deny` in the policy
  layer is resolution metadata for exactly this reason.)
- A **domain** is a stack of rulesets. Nesting only intersects: an access
  must be allowed by *every* layer, so a new layer can restrict and can
  never widen. The empty stack is the unsandboxed process.
- **File-descriptor rights are fixed at `open`** and travel with the fd;
  later domain changes do not revisit them. This is the rule every phase
  theorem in `H5iSpec.Phase` turns on.

Everything is `Bool`-valued and computable, so concrete counterexamples close
by `decide` and the DRT world (`H5iSpec.Input`) can meet this layer later.

v1 scope, stated: two access rights (read/write) of Landlock's ~15, exact
component paths with prefix scoping, no symlinks — the same exclusions §V5
lists for the model at large.
-/

namespace H5iSpec

/-- An absolute path, as components: `/home/u/.aws` is `["home","u",".aws"]`. -/
abbrev FsPath := List String

/-- `FsPath.beneath p q`: `p` is `q` itself or below it — Landlock's
`path_beneath` scope. -/
def FsPath.beneath (p q : FsPath) : Bool :=
  q.isPrefixOf p

/-- The v1 access-right alphabet. -/
inductive Access where
  | read
  | write
deriving Repr, DecidableEq

/-- One Landlock rule: `access` rights granted beneath `path`. -/
structure Rule where
  path : FsPath
  access : List Access
deriving Repr, DecidableEq

/-- A ruleset: an allowlist of rules. No deny rules exist. -/
abbrev Ruleset := List Rule

/-- A ruleset allows `a` on `p` iff some rule scopes `p` and carries `a`. -/
def Ruleset.allows (rs : Ruleset) (p : FsPath) (a : Access) : Bool :=
  rs.any fun r => p.beneath r.path && r.access.contains a

/-- A domain: the stack of rulesets a process is confined by, innermost
first. `[]` is the unsandboxed process. -/
abbrev Domain := List Ruleset

/-- Nesting intersects: every layer must allow. -/
def Domain.allows (d : Domain) (p : FsPath) (a : Access) : Bool :=
  d.all fun rs => rs.allows p a

/-- **Restriction narrows**: whatever a stacked domain allows, the domain
below it already allowed. A layer can only take. -/
theorem Domain.restrict_narrows (rs : Ruleset) (d : Domain) (p : FsPath)
    (a : Access) (h : Domain.allows (rs :: d) p a = true) :
    Domain.allows d p a = true := by
  simp only [Domain.allows, List.all_cons, Bool.and_eq_true] at h
  exact h.2

/-- The contrapositive h5i leans on: **denial persists through restriction**.
Once a domain denies an access, no later `restrict_self` brings it back. -/
theorem Domain.deny_persists (rs : Ruleset) (d : Domain) (p : FsPath)
    (a : Access) (h : Domain.allows d p a = false) :
    Domain.allows (rs :: d) p a = false := by
  cases hall : Domain.allows (rs :: d) p a with
  | false => rfl
  | true =>
    exact absurd (Domain.restrict_narrows rs d p a hall) (by simp [h])

end H5iSpec
