import H5iFs.Mount
import H5iFs.Fd

/-!
The attack menu, machine-checked (ROADMAP §VF.3). Each amplifier is a concrete
counterexample: an undefended check says *allow*, and the resolved,
object-level verdict says *deny* — or the amplifier makes one host object
reachable under both a granted and a forbidden name. These are the reason the
theorems in `Theorems.lean` are not vacuous; a defense is admitted only after
the attack it stops is one of the `by decide` facts below.

Coverage this file: #1 (prevented by construction), #2 symlink escape, #3 fd
smuggling, #5 mount-order shadowing, #6 hardlink amplification. Deferred to
their layers and named rather than faked: #4 parent-allow/child-deny lands in
the validator's representability check (§VF.4), and #7 setup-TOCTOU lands in
`Setup.lean`'s interleaved schedule (§VF.2).

The world: a granted `/work` beside a secret `/home/user/.ssh`, with the
adversary's plants already in place — a symlink `/work/evil → …/.ssh` and a
hard link `/work/alias` onto the secret key. This is the worktree the previous
run could have left (§VF.2, adversarial initial state).
-/

namespace H5iFs.Attacks

open H5iFs
open H5iSpec (FsPath Access)

/-! ### The adversarial world -/

def secretDir : NodeId := 3
def secretKey : NodeId := 4
def workDir : NodeId := 5

/-- Ids: 0 root, 1 home, 2 user, 3 .ssh, 4 id_rsa (secret), 5 work, 6 main.rs,
7 the planted symlink. -/
def world : FsState where
  nodes :=
    [(0, .dir), (1, .dir), (2, .dir), (3, .dir), (4, .file), (5, .dir),
     (6, .file), (7, .symlink ["home", "user", ".ssh"])]
  entries :=
    [⟨0, "home", 1⟩, ⟨1, "user", 2⟩, ⟨2, ".ssh", 3⟩, ⟨3, "id_rsa", 4⟩,
     ⟨0, "work", 5⟩, ⟨5, "main.rs", 6⟩,
     ⟨5, "evil", 7⟩,          -- planted symlink out of the grant
     ⟨5, "alias", 4⟩]         -- planted hard link onto the secret key
  content := [(4, 42), (6, 7)]
  metas := []
  root := 0

/-- The world is well-formed: the adversary picks the shape, not a dangling
graph. -/
theorem world_wellFormed : world.WellFormed = true := by decide

/-- The policy grants the legitimate `/work` objects — and, crucially, **not**
the secret key, even though a hard link puts it under `/work`. Object-level, by
identity, which is the whole point of #6. -/
def granted : List NodeId := [workDir, 6, 7]

/-- The vulnerable verdict: a byte/component prefix test on the path, no
resolution. -/
def naiveRead (grant : FsPath) (p : FsPath) : Bool := p.beneath grant

/-- The defended verdict: resolve to an object, then check the object grant. -/
def resolvedRead (fs : FsState) (grant : List NodeId) (p : FsPath) : Bool :=
  match fs.resolve p with
  | some n => grant.contains n
  | none => false

/-! ### #1 — string-prefix escape, prevented by construction

The classic `/work` vs `/worktools` confusion needs a byte-substring prefix
test. Paths here are **component lists**, and a component prefix does not
confuse siblings — so the bug cannot be written. -/

theorem component_paths_dont_confuse_siblings :
    naiveRead ["work"] ["worktools"] = false
      ∧ naiveRead ["work"] ["work", "main.rs"] = true := by decide

/-! ### #2 — symlink escape

`/work/evil/id_rsa` is a string-child of the grant, so `naiveRead` allows it;
it *resolves* to the secret key, which the object grant denies. -/

theorem symlink_escape :
    naiveRead ["work"] ["work", "evil", "id_rsa"] = true
      ∧ resolvedRead world granted ["work", "evil", "id_rsa"] = false := by decide

/-- And concretely: the escape lands on the secret key object. -/
theorem symlink_resolves_to_secret :
    world.resolve ["work", "evil", "id_rsa"] = some secretKey := by decide

/-! ### #6 — hardlink authority amplification

`/work/alias` and `/home/user/.ssh/id_rsa` are the **same object**. A grant
that collected the `/work` subtree by path would hand out the secret; the
object grant (which excludes id `secretKey`) denies it despite the alias. -/

theorem hardlink_aliases_secret :
    world.resolve ["work", "alias"]
      = world.resolve ["home", "user", ".ssh", "id_rsa"] := by decide

theorem hardlink_defended :
    naiveRead ["work"] ["work", "alias"] = true
      ∧ resolvedRead world granted ["work", "alias"] = false := by decide

/-! ### #5 — mount order shadows a read-only overlay

The config-lock overlay is read-only over a read-write `/work`. Applied on top
it stays read-only; reverse the order and the later `/work` rw mount governs
the path and it becomes writable. -/

def settingsPath : FsPath := ["work", ".claude", "settings.json"]

/-- Correct order: `/work` rw first, the settings overlay ro last. -/
def goodOrder : List Mount := [⟨["work"], true⟩, ⟨settingsPath, false⟩]

/-- Reversed: the ro overlay first, `/work` rw last — the bug. -/
def badOrder : List Mount := [⟨settingsPath, false⟩, ⟨["work"], true⟩]

theorem rw_shadows_ro :
    permOf goodOrder settingsPath = .ro
      ∧ permOf badOrder settingsPath = .rw := by decide

theorem mount_order_is_load_bearing :
    (permOf goodOrder settingsPath).leRo = true
      ∧ (permOf badOrder settingsPath).leRo = false := by decide

/-! ### #3 — fd smuggling past a restriction

Open the secret key (an fd with read), then restrict path authority: the fd
still reads it. The defense is closing the fd during setup (`closeForbidden`),
after which the same read is denied — `Fd.not_canReadViaFd_of_closed` in
general, here on the witness. -/

def openedSecret : List Fd := [⟨secretKey, [.read]⟩]

/-- Forbidden by the run policy: the secret key object. -/
def forbidden (n : NodeId) : Bool := n == secretKey

theorem fd_survives_restriction :
    canReadViaFd openedSecret secretKey = true
      ∧ canReadViaFd (closeForbidden openedSecret forbidden) secretKey = false := by
  decide

end H5iFs.Attacks
