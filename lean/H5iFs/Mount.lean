import H5iFs.Core

/-!
Ordered mounts and the permission a path sees through them (ROADMAP §VF.2,
amplifier 3). A mount redirects a target prefix and carries a read-only or
read-write flag; mounts are applied in order, and a later mount whose target
scopes a path sits **on top** of an earlier one — so mount *order* decides the
effective permission, and reversing it can turn a read-only overlay back into
read-write. That is not hypothetical: `home_binds_in_mount_order` in
`sandbox.rs` sorts binds by hand precisely to keep the config-lock's
read-only overlay on top of the `$WORK` read-write mount.

The rule, matching `Predict.lean`'s bind resolution: the mount that governs a
path is the **last one applied** whose target is a prefix of it. The
counterexample that the order is load-bearing lives in `Attacks.lean`
(`rw_shadows_ro`); the positive lemma that permission is read from the top
mount is here.
-/

namespace H5iFs

open H5iSpec (FsPath Access)

/-- One mount: a target prefix and whether it is writable. (Source subtree is
omitted at the permission layer; it returns when resolution composes with
mounts.) -/
structure Mount where
  target : FsPath
  rw : Bool
deriving Repr, DecidableEq

/-- A permission a path can carry through the mount stack. -/
inductive Perm where
  | denied
  | ro
  | rw
deriving Repr, DecidableEq

/-- `p ≤ ro`: the permission is read-only or less. The protected-overlay
theorem's conclusion. -/
def Perm.leRo : Perm → Bool
  | .denied => true
  | .ro => true
  | .rw => false

/-- The mount governing `p`: the last-applied mount whose target is a prefix
of `p`. `mounts` is in apply order (head applied first), so `getLast?` of the
order-preserving filter is the topmost mount. -/
def effectiveMount (mounts : List Mount) (p : FsPath) : Option Mount :=
  (mounts.filter (fun m => p.beneath m.target)).getLast?

/-- The permission `p` sees: read from the top mount, `denied` if no mount
scopes it (the base layer is composed separately). -/
def permOf (mounts : List Mount) (p : FsPath) : Perm :=
  match effectiveMount mounts p with
  | some m => if m.rw then .rw else .ro
  | none => .denied

/-- **Permission is taken from the top mount.** If the mount governing `p` is
read-only, `p` is read-only — the defense the hand-sort in `sandbox.rs`
maintains and the property `Attacks.rw_shadows_ro` shows a wrong order breaks. -/
theorem perm_ro_of_top_ro {mounts : List Mount} {p : FsPath} {m : Mount}
    (h : effectiveMount mounts p = some m) (hro : m.rw = false) :
    (permOf mounts p).leRo = true := by
  simp [permOf, h, hro, Perm.leRo]

end H5iFs
