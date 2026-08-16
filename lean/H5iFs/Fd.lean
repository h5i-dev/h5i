import H5iFs.Core

/-!
File descriptors as capabilities (ROADMAP §VF.2, amplifier 4). A descriptor's
rights are fixed at `open` and travel with it; a later restriction of the
path-level authority does not revisit them. So an object opened before
`restrict_self` stays readable through the saved fd even after the path check
would deny it — the fd-smuggling shape.

The consequence for setup: the invariant is `NoForbiddenFd` on the **ready**
state, after setup has closed inherited descriptors — never on the initial
state, where a forbidden fd is harmless if setup closes it, and where only
`FD_CLOEXEC` descriptors would close at `exec` on their own. The
counterexample that a path restriction alone is insufficient lives in
`Attacks.lean` (`fd_survives_restriction`); the invariant and the fact that
closing establishes it are here.
-/

namespace H5iFs

open H5iSpec (Access)

/-- An open descriptor: the object it points at and the rights bound at open. -/
structure Fd where
  obj : NodeId
  rights : List Access
deriving Repr, DecidableEq

/-- Can the process read `obj` through some open descriptor? This ignores any
later path-level restriction — that is the whole point. -/
def canReadViaFd (fds : List Fd) (obj : NodeId) : Bool :=
  fds.any (fun f => f.obj == obj && f.rights.contains .read)

/-- `NoForbiddenFd`: no open descriptor points at a forbidden object. Stated
over whatever fd list is current; the theorems apply it to the **ready**
state. -/
def NoForbiddenFd (fds : List Fd) (forbidden : NodeId → Bool) : Bool :=
  fds.all (fun f => ! forbidden f.obj)

/-- Closing the descriptors that point at forbidden objects. This is the setup
step that establishes the invariant; a restriction of path authority does not. -/
def closeForbidden (fds : List Fd) (forbidden : NodeId → Bool) : List Fd :=
  fds.filter (fun f => ! forbidden f.obj)

/-- **Closing establishes the invariant.** After `closeForbidden`, no
descriptor points at a forbidden object — for every choice of the forbidden
predicate. -/
theorem noForbiddenFd_closeForbidden (fds : List Fd) (forbidden : NodeId → Bool) :
    NoForbiddenFd (closeForbidden fds forbidden) forbidden = true := by
  simp only [NoForbiddenFd, closeForbidden, List.all_filter, List.all_eq_true]
  intro f _
  by_cases h : forbidden f.obj = true <;> simp [h]

/-- **A closed-out object is unreadable by fd.** After closing the forbidden
descriptors, no fd reads a forbidden object — so a saved fd cannot smuggle it
past the transition, which `Attacks.fd_survives_restriction` shows is exactly
what an *unclosed* fd does. -/
theorem not_canReadViaFd_of_closed (fds : List Fd) (forbidden : NodeId → Bool)
    (obj : NodeId) (hf : forbidden obj = true) :
    canReadViaFd (closeForbidden fds forbidden) obj = false := by
  simp only [canReadViaFd, closeForbidden, List.any_filter]
  rw [List.any_eq_false]
  intro f _
  by_cases h : (f.obj == obj) = true
  · simp only [beq_iff_eq] at h; subst h; simp [hf]
  · simp only [Bool.not_eq_true] at h; rw [h]; simp

end H5iFs
