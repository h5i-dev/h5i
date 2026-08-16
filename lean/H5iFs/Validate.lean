import H5iFs.Theorems
import H5iFs.Attacks

/-!
The per-run translation validator (ROADMAP §VF.4), as a Lean specification. It
takes the **shipped plan** — the grant lists and ordered mounts h5i hands the
backend (`policy.effective.json`) — and the **measured world** (a finite
`FsState` the harness built by stat'ing the relevant paths; the Rust side's
`WorldEvidence`), resolves each grant to the object it actually reaches, and
checks the induced authority is a subset of the source policy. `validate_sound`
then says an accepted plan admits no effect the policy forbids, for every run
trace — directly from `every_effect_authorized`.

The point is that resolution happens against the *measured* world, so a grant
path that reaches a secret through a planted symlink or hard link (the
`Attacks` world) is caught on the shipped plan: `validate_rejects_symlink_grant`
below rejects the very escape `Attacks.symlink_escape` describes.

Scope: this is the `writes_confined` / `cache_readonly` core (§VF.4). The finite
`WorldEvidence` with its completeness witness, the backend-representability
rejection (#4), and the Rust port + checker-level DRT are the remaining step-2
work; here the semantics and its soundness are pinned.
-/

namespace H5iFs

open H5iSpec (FsPath Access)

/-- The shipped plan's filesystem authority: read-only and read-write grant
paths, plus the ordered mounts. Mirrors the Landlock grant lists and mount
manifest of `policy.effective.json`. -/
structure EffectivePlan where
  roGrants : List FsPath
  rwGrants : List FsPath
  mounts : List Mount
deriving Repr

/-- Resolve the plan's grants through the measured world into the object-level
authority it would install: read from ro+rw grants, write from rw grants only.
Grants that do not resolve are dropped (a plan cannot grant a missing object) —
the fail-closed direction. -/
def EffectivePlan.authority (plan : EffectivePlan) (fs : FsState) : Authority where
  readable := (plan.roGrants ++ plan.rwGrants).filterMap fs.resolve
  writable := plan.rwGrants.filterMap fs.resolve

/-- **The validator**: the plan's induced object authority is a subset of the
source policy. On a measured world where a grant path resolves — through a
planted symlink or alias — to a forbidden object, this is `false`: the plan is
rejected before the box runs. -/
def validate (pol : Policy) (fs : FsState) (plan : EffectivePlan) : Bool :=
  AuthoritySound (plan.authority fs) pol

/-- **Validator soundness** (§VF.4): a plan the validator accepts admits no
effect the source policy forbids, for every run trace. Immediate — an accepted
plan's authority is sound, and `every_effect_authorized` bounds the trace. -/
theorem validate_sound {pol : Policy} {fs : FsState} {plan : EffectivePlan}
    (ops : List Op) (h : validate pol fs plan = true) :
    ∀ e ∈ effectsOf fs (plan.authority fs) plan.mounts ops, PolicyAllows pol e = true :=
  every_effect_authorized h ops

/-- **Writes come only from rw grants** (the `cache_readonly` claim, static
half): a read-only grant contributes nothing to the writable set. -/
theorem writable_only_from_rw (plan : EffectivePlan) (fs : FsState) :
    (plan.authority fs).writable = plan.rwGrants.filterMap fs.resolve := rfl

/-- **No write happens under a non-rw mount** (the `cache_readonly` claim,
dynamic half): a write effect requires the governing mount to be read-write, so
a read-only remount denies the write regardless of the grant. -/
theorem write_effect_needs_rw {fs : FsState} {auth : Authority} {mounts : List Mount}
    {p : FsPath} {o : NodeId}
    (h : permit fs auth mounts (.write p) = some (.write o)) :
    permOf mounts p = Perm.rw := by
  simp only [permit] at h
  split at h
  · split at h
    · split at h
      · assumption
      · exact absurd h (by simp)
    · exact absurd h (by simp)
  · exact absurd h (by simp)

/-! ### Accept and reject on the adversarial world

The measured world is `Attacks.world`: granted `/work` beside a secret
`~/.ssh`, with the planted symlink and hard link. The policy grants the
legitimate `/work` objects (`Attacks.granted`, which excludes the secret). -/

def demoPolicy : Policy := ⟨Attacks.granted, Attacks.granted⟩

/-- A benign plan granting `/work` read-write is accepted: it resolves to the
work directory, which the policy permits. -/
theorem validate_accepts_benign :
    validate demoPolicy Attacks.world ⟨[], [["work"]], []⟩ = true := by decide

/-- A plan whose grant path resolves through the planted symlink to the secret
is **rejected** — the validator catches the escape on the shipped grant, the
same escape `Attacks.symlink_escape` exhibits at the access level. -/
theorem validate_rejects_symlink_grant :
    validate demoPolicy Attacks.world ⟨[["work", "evil"]], [], []⟩ = false := by decide

/-- A plan whose grant reaches the secret key through the planted hard link is
also rejected, by object identity — `/work/alias` resolves to the secret key,
which the policy denies. -/
theorem validate_rejects_hardlink_grant :
    validate demoPolicy Attacks.world ⟨[], [["work", "alias"]], []⟩ = false := by decide

end H5iFs
