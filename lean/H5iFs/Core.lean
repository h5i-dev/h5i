import H5iSpec.Landlock

/-!
H5iFs: the filesystem **authority machine** (ROADMAP §VF, the 2026-08-16
pivot). Not a filesystem model for its own sake — the semantics module of the
per-run validator (§VF.4) and the oracle for the backend attack suite (§VF.10).
The subject is authority: which host objects a confined, adversarial caller
can read or write, through every amplifier the real system offers — symlinks,
hard links, rename, ordered mounts with shadowing, and file descriptors opened
before restriction.

Design constraints, fixed here so every later file inherits them:

- **Object identity, not paths.** State is a graph over `NodeId`: hard links
  and rename make several paths name one object, and every theorem that
  quantifies only over paths misses exactly the attacks that matter. `NodeId`
  is a model-internal identity, **not** a Linux inode number (those are unique
  only within a device and are reused); when the validator touches the host it
  does so through an `ObjectKey` defined at that layer, never here.
- **Executable and decidable.** State is association lists, never functions:
  concrete attacks close by `decide`, and the same definitions compile into
  the validator and the test oracle. No mathlib (the lakefile's rule), no
  `Finset` — plain lists with well-formedness invariants.
- **Adversarial initial state.** Theorems quantify over any well-formed
  initial filesystem the previous run's agent could have left behind — planted
  symlinks included. A benign fixed initial state proves the wrong thing; the
  runc CVE class (2021–2025) lives in setup code walking attacker-shaped trees.
- **Attack-first.** A defense enters the model only after the attack it stops
  is a machine-checked counterexample against the undefended check.
  `Attacks.lean` is not an appendix; it is why the theorems mean something.

Module map (ROADMAP §VF.10):

- `Core.lean` (this file): state, well-formedness, symlink-aware resolution.
- `Mount.lean`: ordered mount tables, shadowing, effective permission.
- `Fd.lean`: descriptors as capabilities, rights fixed at open.
- `Setup.lean`: the setup state machine as an attacker-interleaved schedule.
- `Attacks.lean`: the counterexamples, one per amplifier.
- `Theorems.lean`: `every_effect_authorized`, then confidentiality.

v1 resolution scope, stated (the §VF.2 discipline: name what is abstracted).
Modeled here: absolute symlink targets, a symlink at any component (ancestors
included), and fuel-bounded loop cutoff. Not yet modeled, and listed rather
than silently dropped: relative symlink targets, `.`/`..` components,
`/proc/self/fd` magic links, and mount crossing (mounts live in `Mount.lean`,
composed with resolution there).
-/

namespace H5iFs

open H5iSpec (FsPath Access)

/-- An object identity: files and directories have identity independent of
every name that reaches them. Opaque; not an inode number (see the header). -/
abbrev NodeId := Nat

/-- What an object is. A symlink's target is an uninterpreted absolute path,
followed (fuel-bounded) at lookup, never at definition. -/
inductive NodeKind where
  | file
  | dir
  | symlink (target : FsPath)
deriving Repr, DecidableEq

/-- Metadata that integrity must protect beyond content: a `rename`, `chmod`,
or xattr edit breaks security with content unchanged, so the protected
projection (`Theorems.lean`) pins these too. -/
structure Meta where
  mode : Nat
  uid : Nat
  gid : Nat
  suid : Bool
  sgid : Bool
  exec : Bool
  nlink : Nat
  xattrs : List (String × Nat)
deriving Repr, DecidableEq

/-- One directory entry: `name` under directory `parent` names `child`. A flat
list, so a hard link is two entries with one `child` — the representation
makes the aliasing the theorems must survive. -/
structure Entry where
  parent : NodeId
  name : String
  child : NodeId
deriving Repr, DecidableEq

/-- The filesystem: an object table and a name graph, plus content and
metadata keyed by identity. Association lists by construction, so everything
is decidable and `#eval`-able. `root` is the identity absolute paths resolve
from. -/
structure FsState where
  nodes : List (NodeId × NodeKind)
  entries : List Entry
  content : List (NodeId × Nat)
  metas : List (NodeId × Meta)
  root : NodeId
deriving Repr

/-- The kind of an object, if it exists. -/
def FsState.kindOf (fs : FsState) (n : NodeId) : Option NodeKind :=
  (fs.nodes.find? (·.1 == n)).map (·.2)

/-- The opaque content of a file object (`none` for a directory/symlink or a
missing id). -/
def FsState.contentOf (fs : FsState) (n : NodeId) : Option Nat :=
  (fs.content.find? (·.1 == n)).map (·.2)

/-- The metadata of an object, if recorded. -/
def FsState.metaOf (fs : FsState) (n : NodeId) : Option Meta :=
  (fs.metas.find? (·.1 == n)).map (·.2)

/-- The child named `name` under directory `dir`, if any. -/
def FsState.childOf (fs : FsState) (dir : NodeId) (name : String) : Option NodeId :=
  (fs.entries.find? (fun e => e.parent == dir && e.name == name)).map (·.child)

/-- Default resolution fuel: the loop/depth cutoff. Beyond it, resolution
fails closed (`none`) rather than looping — a symlink cycle is denied, not
diverged. -/
def resolveFuel : Nat := 64

/-- Resolve `comps` starting at object `cur`, following symlinks to absolute
targets from `root`, with `fuel` bounding total steps. `none` means the path
does not resolve (missing component, non-directory in the middle, or fuel
exhausted by a loop). Every recursive call decreases `fuel`, so this is
structural and closes by `decide` on concrete states. -/
def FsState.resolveFrom (fs : FsState) : Nat → NodeId → FsPath → Option NodeId
  | 0, _, _ => none
  | _ + 1, cur, [] => some cur
  | fuel + 1, cur, c :: rest => do
    let child ← fs.childOf cur c
    match fs.kindOf child with
    | some (.symlink target) =>
      -- The link's target is resolved from the root, then the remaining
      -- components continue from wherever it lands. An ancestor symlink is
      -- just this case with `rest` non-empty.
      fs.resolveFrom fuel fs.root (target ++ rest)
    | _ => fs.resolveFrom fuel child rest

/-- Resolve an absolute path to the object it names. This is the map every
authority verdict is taken against: the mechanisms judge paths, the world is
objects, and resolution is the bridge the attacks exploit. -/
def FsState.resolve (fs : FsState) (path : FsPath) : Option NodeId :=
  fs.resolveFrom resolveFuel fs.root path

/-! ### Well-formedness

The invariant the setup machine must preserve and the theorems assume of any
adversarial initial state: the graph is internally consistent. An attacker
picks the *shape* (any planted symlinks, aliases, entries), but not a
dangling graph. -/

/-- Every entry's parent and child are real objects, and every parent is a
directory. -/
def FsState.entriesWf (fs : FsState) : Bool :=
  fs.entries.all fun e =>
    (fs.nodes.any (·.1 == e.parent))
      && (fs.nodes.any (·.1 == e.child))
      && (fs.kindOf e.parent == some .dir)

/-- The root exists and is a directory. -/
def FsState.rootWf (fs : FsState) : Bool :=
  fs.kindOf fs.root == some .dir

/-- Node identities are unique in the table (one kind per object). -/
def FsState.nodesUniq (fs : FsState) : Bool :=
  (fs.nodes.map (·.1)).Nodup

/-- The full well-formedness predicate, `Bool`-valued so it is decidable and
usable as a hypothesis that concrete states discharge by `decide`. -/
def FsState.WellFormed (fs : FsState) : Bool :=
  fs.nodesUniq && fs.rootWf && fs.entriesWf

end H5iFs
