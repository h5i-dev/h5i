import H5iFs.Setup
import H5iFs.Mount

/-!
The run and its guarantee (ROADMAP §VF.3). Setup produces a sound authority
(`Setup.runFrozen_sound`, over any attacker interleaving); the run then permits
an operation only when the resolved object is in that authority. The central
theorem is **trace-level**: every effect a run produces is one the policy
permits — not merely final-state equality, which a write-then-restore attack
would slip past. Integrity of objects outside the writable grants is the
corollary.

Effects are filesystem effects only (`Read`/`Write` here). Network and secret
effects are a separate `AuthorityEffect` layer (§VF.4) and out of scope. The
run resolves each operation's path through the same object semantics as setup,
so a symlink placed in the writable area is judged at its resolved target — the
`Attacks` counterexamples are what make "resolved object" load-bearing.
-/

namespace H5iFs

open H5iSpec (FsPath Access)

/-- A filesystem effect the run actually caused, on a resolved object. -/
inductive Effect where
  | read (o : NodeId)
  | write (o : NodeId)
deriving Repr, DecidableEq

/-- What the policy permits, at the object level. -/
def PolicyAllows (pol : Policy) : Effect → Bool
  | .read o => pol.mayRead.contains o
  | .write o => pol.mayWrite.contains o

/-- An operation the confined process attempts: a kind and a path it names. -/
inductive Op where
  | read (p : FsPath)
  | write (p : FsPath)
deriving Repr

/-- The sandbox's verdict on one operation: resolve the path to an object, then
check it against the run authority (and, for a write, that the governing mount
is read-write). A denied operation is a no-op (`none`), never a partial effect. -/
def permit (fs : FsState) (auth : Authority) (mounts : List Mount) : Op → Option Effect
  | .read p =>
    match fs.resolve p with
    | some o => if o ∈ auth.readable then some (.read o) else none
    | none => none
  | .write p =>
    match fs.resolve p with
    | some o =>
      if o ∈ auth.writable then
        (if permOf mounts p = Perm.rw then some (.write o) else none)
      else none
    | none => none

/-- The effects a trace of operations actually causes. -/
def effectsOf (fs : FsState) (auth : Authority) (mounts : List Mount) (ops : List Op) :
    List Effect :=
  ops.filterMap (permit fs auth mounts)

/-- A permitted effect is a policy-permitted effect — the per-operation core of
the trace theorem. `permit` only emits `.read o` for `o ∈ auth.readable`, and a
sound authority is a policy subset, so the policy permits it. -/
theorem permit_authorized {fs : FsState} {auth : Authority} {mounts : List Mount}
    {pol : Policy} {op : Op} {e : Effect}
    (hs : AuthoritySound auth pol = true) (h : permit fs auth mounts op = some e) :
    PolicyAllows pol e = true := by
  simp only [AuthoritySound, Bool.and_eq_true] at hs
  have hr : ∀ o ∈ auth.readable, pol.mayRead.contains o = true := List.all_eq_true.mp hs.1
  have hw : ∀ o ∈ auth.writable, pol.mayWrite.contains o = true := List.all_eq_true.mp hs.2
  cases op with
  | read p =>
    simp only [permit] at h
    split at h
    · split at h
      · injection h with h; subst h
        exact hr _ (by assumption)
      · exact absurd h (by simp)
    · exact absurd h (by simp)
  | write p =>
    simp only [permit] at h
    split at h
    · split at h
      · split at h
        · injection h with h; subst h
          exact hw _ (by assumption)
        · exact absurd h (by simp)
      · exact absurd h (by simp)
    · exact absurd h (by simp)

/-- **Every effect a run causes is authorized** (§VF.3, the central theorem).
Trace-level: it bounds *what happened*, so a write-then-restore trace cannot
launder an unauthorized effect through an unchanged final state. The content is
carried by `AuthoritySound`, which `Setup.runFrozen_sound` establishes over any
adversarial interleaving. -/
theorem every_effect_authorized {fs : FsState} {auth : Authority} {mounts : List Mount}
    {pol : Policy} (hs : AuthoritySound auth pol = true) (ops : List Op) :
    ∀ e ∈ effectsOf fs auth mounts ops, PolicyAllows pol e = true := by
  intro e he
  rw [effectsOf, List.mem_filterMap] at he
  obtain ⟨op, _, hpe⟩ := he
  exact permit_authorized hs hpe

/-! ### Integrity: objects outside the writable grants do not change

The `ProtectedProjection` (§VF.3) pins more than content; this corollary proves
the content component, over the run's authorized effects. Applying an
authorized trace leaves every non-writable object's content untouched — writes
land only on `mayWrite` objects, and a non-writable object is not among them. -/

/-- A write records new (opaque) content for its object. -/
def applyEffect (fs : FsState) : Effect → FsState
  | .read _ => fs
  | .write o => { fs with content := (o, 0) :: fs.content }

def applyEffects (fs : FsState) (es : List Effect) : FsState :=
  es.foldl applyEffect fs

/-- One effect that never writes `obj` leaves `obj`'s content unchanged. -/
theorem contentOf_applyEffect_ne {fs : FsState} {e : Effect} {obj : NodeId}
    (h : ∀ o, e = .write o → o ≠ obj) :
    (applyEffect fs e).contentOf obj = fs.contentOf obj := by
  cases e with
  | read _ => rfl
  | write o =>
    have hne : o ≠ obj := h o rfl
    have hbeq : ((o, (0 : Nat)).1 == obj) = false := by
      simp only [beq_eq_false_iff_ne, ne_eq]; exact hne
    simp only [applyEffect, FsState.contentOf, List.find?_cons, hbeq]

/-- A whole trace that never writes `obj` leaves `obj`'s content unchanged. -/
theorem contentOf_applyEffects {fs : FsState} {es : List Effect} {obj : NodeId}
    (h : ∀ e ∈ es, ∀ o, e = .write o → o ≠ obj) :
    (applyEffects fs es).contentOf obj = fs.contentOf obj := by
  induction es generalizing fs with
  | nil => rfl
  | cons e rest ih =>
    have hrest : ∀ e' ∈ rest, ∀ o, e' = .write o → o ≠ obj :=
      fun e' he' => h e' (List.mem_cons_of_mem _ he')
    show (applyEffects (applyEffect fs e) rest).contentOf obj = fs.contentOf obj
    rw [ih hrest]
    exact contentOf_applyEffect_ne (h e List.mem_cons_self)

/-- **Integrity outside the writable grants.** For an object the policy does
not grant write on, applying a run's authorized effects does not change its
content — the content half of the `ProtectedProjection` corollary. -/
theorem integrity_outside_writable {fs : FsState} {auth : Authority} {mounts : List Mount}
    {pol : Policy} (hs : AuthoritySound auth pol = true) (ops : List Op) {obj : NodeId}
    (hobj : pol.mayWrite.contains obj = false) :
    (applyEffects fs (effectsOf fs auth mounts ops)).contentOf obj = fs.contentOf obj := by
  apply contentOf_applyEffects
  intro e he o hew
  -- an authorized write targets a `mayWrite` object; `obj` is not one.
  have hauth : PolicyAllows pol e = true := every_effect_authorized hs ops e he
  subst hew
  simp only [PolicyAllows] at hauth
  intro hcontra; subst hcontra
  rw [hauth] at hobj
  exact absurd hobj (by simp)

end H5iFs
