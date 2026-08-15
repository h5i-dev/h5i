import H5iSpec.Refinement

/-!
The prediction layer for conformance probes (ROADMAP.md §V4, "model versus
kernel"), now bind-aware. A kernel-tier box is not Landlock alone: the child
bind-mounts before restricting itself (steps 1d–1h), so what sits at a path
inside the box can be a different subtree than on the host, and a read-only
remount denies writes regardless of any Landlock grant.

The model of one bind: accesses beneath the bind's *target* resolve inside
the *source* subtree, so the path Landlock judges is the probe path rebased
from target to source; and when the bind is not writable (`MS_RDONLY`
remount), every write beneath the target fails with `EROFS` before Landlock
is consulted. Later mounts shadow earlier ones at overlapping targets, so
the **last** matching bind in apply order wins.

Deliberately not modeled, same spirit as §V5: nested submounts beneath a
bind target, symlinks, and file *existence* — `predictAllows` answers
"would this access be permitted", never "is there a file there", which is
why the probe harness only probes paths whose existence it controls.
-/

namespace H5iSpec

/-- The last bind in apply order whose target scopes `p` — later mounts
shadow earlier ones, so a fold keeping the latest match is the semantics. -/
def lastMatchingBind (binds : List EffectiveBind) (p : FsPath) :
    Option EffectiveBind :=
  binds.foldl
    (fun acc b => if p.beneath (parsePath b.target) then some b else acc)
    none

/-- Rebase `p` from beneath `target` into the `source` subtree: the object
the kernel actually resolves once the bind is mounted. -/
def rebase (p target source : FsPath) : FsPath :=
  source ++ p.drop target.length

/-- The bind-aware verdict: what a probe of `a` on `p` must observe in a box
running under `cfg`. -/
def predictAllows (cfg : EffectiveConfig) (p : FsPath) (a : Access) : Bool :=
  let rs := compileLandlock cfg.landlock
  match lastMatchingBind cfg.binds p with
  | some b =>
    (a != .write || b.writable)
      && rs.allows (rebase p (parsePath b.target) (parsePath b.source)) a
  | none => rs.allows p a

/-- A read-only bind denies every write beneath its target, whatever
Landlock would have said — the `MS_RDONLY` remount answers first. This is
why the config-lock binds pin agent config against an in-box agent that
holds a perfectly valid rw grant on its own config directory. -/
theorem ro_bind_denies_write (cfg : EffectiveConfig) (p : FsPath)
    (b : EffectiveBind) (h : lastMatchingBind cfg.binds p = some b)
    (hro : b.writable = false) :
    predictAllows cfg p .write = false := by
  simp [predictAllows, h, hro]

/-- Away from every bind target, the prediction is exactly the compiled
Landlock ruleset — so `compile_sound` bounds it by the abstract policy. -/
theorem predict_unbound_is_landlock (cfg : EffectiveConfig) (p : FsPath)
    (a : Access) (h : lastMatchingBind cfg.binds p = none) :
    predictAllows cfg p a = (compileLandlock cfg.landlock).allows p a := by
  simp [predictAllows, h]

end H5iSpec
