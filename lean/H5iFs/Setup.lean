import H5iFs.Core

/-!
Setup as an attacker-interleaved schedule (ROADMAP §VF.2, amplifier 7). The
privileged setup builds the run's authority while the worktree writer can
mutate the filesystem *between* setup steps — the check-vs-mount TOCTOU that is
the runc-CVE class. So the model is not a fixed initial state plus later
attacker ops, but a `SetupEvent` schedule interleaving h5i's steps with
attacker mutations, and the theorem quantifies over the whole schedule.

The defense is **race-free, freeze-at-grant construction** (§VF.5b): each grant
resolves the path and validates the resulting object against the policy in one
step, so whatever it admits is validated against the object it admits — there
is no window. `runFrozen_sound` proves this holds over *any* interleaving,
adversarial initial state included. The counterexample `toctou_check_then_use`
shows the alternative — a separate check then mount, with an attacker swap in
between — hands out the secret.

Authority here is object-level (`NodeId` sets), which is the point of §VF.2:
paths are what the mechanisms name, objects are what is granted.
-/

namespace H5iFs

open H5iSpec (FsPath Access)

/-- The declared policy, as the object sets the user's grants denote. -/
structure Policy where
  mayRead : List NodeId
  mayWrite : List NodeId
deriving Repr

/-- The authority a run carries: the objects it may read and write. -/
structure Authority where
  readable : List NodeId
  writable : List NodeId
deriving Repr

def Authority.empty : Authority := ⟨[], []⟩
def Authority.addR (a : Authority) (o : NodeId) : Authority := { a with readable := o :: a.readable }
def Authority.addW (a : Authority) (o : NodeId) : Authority := { a with writable := o :: a.writable }

/-- **Authority soundness**: every object the run may read/write is one the
policy permits. Bool-valued, so it is decidable and `runFrozen_sound`
establishes it as the setup's postcondition. -/
def AuthoritySound (a : Authority) (pol : Policy) : Bool :=
  a.readable.all (fun o => pol.mayRead.contains o)
    && a.writable.all (fun o => pol.mayWrite.contains o)

theorem authSound_addR {a : Authority} {pol : Policy} {o : NodeId}
    (h : AuthoritySound a pol = true) (ho : pol.mayRead.contains o = true) :
    AuthoritySound (a.addR o) pol = true := by
  simp only [AuthoritySound, Authority.addR, List.all_cons, Bool.and_eq_true] at *
  exact ⟨⟨ho, h.1⟩, h.2⟩

theorem authSound_addW {a : Authority} {pol : Policy} {o : NodeId}
    (h : AuthoritySound a pol = true) (ho : pol.mayWrite.contains o = true) :
    AuthoritySound (a.addW o) pol = true := by
  simp only [AuthoritySound, Authority.addW, List.all_cons, Bool.and_eq_true] at *
  exact ⟨h.1, ho, h.2⟩

/-- Retarget a directory entry — the attacker's move: swap what a name points
at. Prepended so it wins in `childOf` (which takes the first match), modeling a
mid-setup swap. -/
def FsState.retarget (fs : FsState) (parent : NodeId) (name : String) (child : NodeId) :
    FsState :=
  { fs with entries :=
      ⟨parent, name, child⟩ :: fs.entries.filter (fun e => !(e.parent == parent && e.name == name)) }

/-! ### The schedule -/

inductive SetupOp where
  | grantR (p : FsPath)
  | grantW (p : FsPath)
deriving Repr

inductive SetupEvent where
  | h5i (op : SetupOp)
  | atk (parent : NodeId) (name : String) (child : NodeId)
deriving Repr

/-- The setup state as it is built: the (attacker-mutated) filesystem, the
authority so far, and whether setup is still accepting (`ok = false` is a
rejection that a later step cannot undo to grant more). -/
structure SetupState where
  fs : FsState
  auth : Authority
  ok : Bool
deriving Repr

/-- **Freeze-at-grant.** A grant resolves the path and validates the resolved
object against the policy in the same step; an attacker mutation is just
another event, and whatever a grant admits, it admits *because it validated
that object*. A resolution failure or an unpermitted object is a rejection. -/
def stepFrozen (pol : Policy) (s : SetupState) : SetupEvent → SetupState
  | .atk parent name child => { s with fs := s.fs.retarget parent name child }
  | .h5i (.grantR p) =>
    match s.ok with
    | false => s
    | true =>
      match s.fs.resolve p with
      | none => { s with ok := false }
      | some o =>
        match pol.mayRead.contains o with
        | true => { s with auth := s.auth.addR o }
        | false => { s with ok := false }
  | .h5i (.grantW p) =>
    match s.ok with
    | false => s
    | true =>
      match s.fs.resolve p with
      | none => { s with ok := false }
      | some o =>
        match pol.mayWrite.contains o with
        | true => { s with auth := s.auth.addW o }
        | false => { s with ok := false }

def runFrozen (pol : Policy) (s : SetupState) (events : List SetupEvent) : SetupState :=
  events.foldl (stepFrozen pol) s

/-- Every step preserves authority soundness — an attacker event leaves the
authority untouched, and a grant only ever adds an object it validated. -/
theorem stepFrozen_preserves {pol : Policy} {s : SetupState}
    (hs : AuthoritySound s.auth pol = true) (e : SetupEvent) :
    AuthoritySound (stepFrozen pol s e).auth pol = true := by
  cases e with
  | atk parent name child => simpa [stepFrozen] using hs
  | h5i op =>
    cases op with
    | grantR p =>
      simp only [stepFrozen]
      split
      · simpa using hs
      · split
        · simpa using hs
        · split
          · exact authSound_addR hs (by assumption)
          · simpa using hs
    | grantW p =>
      simp only [stepFrozen]
      split
      · simpa using hs
      · split
        · simpa using hs
        · split
          · exact authSound_addW hs (by assumption)
          · simpa using hs

/-- **Setup is sound over any interleaving.** Whatever schedule of h5i grants
and attacker mutations runs, the resulting authority is sound: freeze-at-grant
admits only validated objects, no matter what the attacker does between steps.
This is the §VF.3 `setup_rejects_or_confines` shape — an attacker who could
force an unsafe grant forces a *rejection* (`ok := false`) instead. -/
theorem runFrozen_sound {pol : Policy} (s : SetupState)
    (hs : AuthoritySound s.auth pol = true) (events : List SetupEvent) :
    AuthoritySound (runFrozen pol s events).auth pol = true := by
  induction events generalizing s with
  | nil => simpa [runFrozen] using hs
  | cons e rest ih =>
    simp only [runFrozen, List.foldl_cons]
    exact ih _ (stepFrozen_preserves hs e)

/-! ### #7 — the TOCTOU counterexample

A world where `/work/link` initially names a safe file, and the attacker
retargets it to a symlink out to `/secret` between the check and the mount. -/

/-- 0 root, 5 work, 8 the safe file at `/work/link`, 3 the secret dir, 7 a
symlink to `/secret`. -/
def toctouWorld : FsState where
  nodes := [(0, .dir), (5, .dir), (8, .file), (3, .dir), (7, .symlink ["secret"])]
  entries := [⟨0, "work", 5⟩, ⟨5, "link", 8⟩, ⟨0, "secret", 3⟩]
  content := [(8, 1)]
  metas := []
  root := 0

/-- The policy grants the work subtree's safe objects, not the secret. -/
def toctouPolicy : Policy := ⟨[5, 8], [5, 8]⟩

/-- The attacker's swap: `/work/link` now points at the escaping symlink. -/
def toctouAfterSwap : FsState := toctouWorld.retarget 5 "link" 7

/-- A *separate* check then mount: validate against the pre-swap world, then
grant whatever the path resolves to in the post-swap world — no re-validation.
This is the vulnerable pattern freeze-at-grant refuses. -/
def naiveCheckThenUse (pol : Policy) (atCheck atMount : FsState) (p : FsPath) : Authority :=
  match atCheck.resolve p with
  | some o =>
    if pol.mayRead.contains o then
      match atMount.resolve p with
      | some o' => Authority.empty.addR o'
      | none => Authority.empty
    else Authority.empty
  | none => Authority.empty

/-- **The check passes on the safe object, the mount grants the secret.** The
attacker swap between the two steps escapes: `/work/link` checked as the safe
file (id 8) is mounted as the secret dir (id 3). -/
theorem toctou_check_then_use :
    (naiveCheckThenUse toctouPolicy toctouWorld toctouAfterSwap ["work", "link"]).readable.contains 3
      = true := by decide

/-- **Freeze-at-grant rejects the same schedule.** Resolving-and-validating in
one step on the post-swap world lands on the secret, which the policy denies,
so setup rejects rather than grants — sound, by `runFrozen_sound`, over this
very interleaving. -/
theorem toctou_frozen_rejects :
    (runFrozen toctouPolicy ⟨toctouWorld, Authority.empty, true⟩
      [.atk 5 "link" 7, .h5i (.grantR ["work", "link"])]).ok = false := by decide

end H5iFs
