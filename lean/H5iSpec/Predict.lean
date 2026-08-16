import H5iSpec.Landlock

/-!
The prediction layer for conformance probes (ROADMAP.md §V4, "model versus
kernel"), bind-aware and now existence-aware. A kernel-tier box is not
Landlock alone: the child bind-mounts before restricting itself (steps
1d–1h), so what sits at a path inside the box can be a different subtree
than on the host, a read-only remount denies writes regardless of any
Landlock grant — and a *permitted* access still fails when no object exists
at the resolved place. The first probe harness broke on exactly that: a
host file under `/tmp` is invisible behind the private-tmp bind, `ENOENT`
not `EACCES`.

**Resolution** (`resolveBinds`): mounts stack, so the *latest* bind whose
target scopes the path wins at each step, the path rebases into that bind's
source subtree, and resolution continues through the *earlier* binds — a
later bind's source may itself lie beneath an earlier bind's target (the
mount captured that subtree when it was made). This handles nesting in both
directions: a later, deeper bind refines an earlier one; a later, shallower
bind hides an earlier, deeper one entirely. See the machine-checked
`nested_*` examples below.

**Verdict** (`predictVerdict`): not a bare Bool but (allow, real, check) —
whether the mechanisms permit the access, which host object the access
actually reaches, and what must exist there for the probe to succeed
(`exists` for reads; `creatable` for writes, where an existing file or an
existing parent directory both do, matching `open(O_CREAT|O_WRONLY)`). The
harness supplies the existence facts by stat'ing `real` on the host — the
model owns the semantics, the harness owns the measurement.

Mount flags do not accumulate through resolution steps: only the top-level
winning bind's read-only remount gates writes. That is faithful because
h5i's bind sources are never taken from its own read-only remounted targets
(the mount ordering in `build_confined_command` maintains it). Still not
modeled, and named: the pidns procfs mount (`/proc` stays out of probes),
symlinks, and the env lifecycle above the mounts — notably the per-run wipe
of the private-tmp scratch, which the probe suite discovered the honest
way: the model resolved a previously seeded file to an existing backing
path and the next run's wipe removed it first. Existence facts are valid
only within the invocation that measured them.
-/

namespace H5iSpec

/-- The last bind in apply order whose target scopes `p` — later mounts
shadow earlier ones, so a fold keeping the latest match is the semantics. -/
def lastMatchingBind (binds : List EffectiveBind) (p : FsPath) :
    Option EffectiveBind :=
  binds.foldl
    (fun acc b => if p.beneath (parsePath b.target) then some b else acc)
    none

/-- Rebase `p` from beneath `target` into the `source` subtree. -/
def rebase (p target source : FsPath) : FsPath :=
  source ++ p.drop target.length

/-- Resolution through a latest-first bind list: the first (i.e. latest)
matching bind rebases, and resolution continues through the earlier binds —
the mount captured its source subtree in the state those earlier binds had
already built. -/
def resolveThrough : List EffectiveBind → FsPath → FsPath
  | [], p => p
  | b :: earlier, p =>
    if p.beneath (parsePath b.target) then
      resolveThrough earlier (rebase p (parsePath b.target) (parsePath b.source))
    else
      resolveThrough earlier p

/-- The host object an in-box access of `p` reaches, given the binds in
apply order. -/
def resolveBinds (binds : List EffectiveBind) (p : FsPath) : FsPath :=
  resolveThrough binds.reverse p

/-- What must exist at the resolved path for the probe to succeed. -/
inductive ExistCheck where
  | «exists»
  /-- An existing file or an existing parent directory both suffice —
  `open(O_CREAT|O_WRONLY)` semantics. -/
  | creatable
  /-- The object lives in a namespace-local filesystem (the pidns's private
  procfs): the host cannot be stat'd for it, so the harness must know the
  in-box existence a priori (`/proc/self/…` exists by definition; a host
  pid's entry does not, in a private pid namespace). -/
  | boxLocal
deriving Repr, DecidableEq

/-- One symlink fact the harness measured: the object at `link` (a host
path, i.e. post-bind-resolution) is a symbolic link whose text is `target`
(an absolute in-box path, re-walked from the box's root through the binds —
which is exactly what makes a link a potential smuggling vector). -/
structure SymlinkFact where
  link : String
  target : String
deriving Repr, DecidableEq

/-- Chase symlinks: at each step the current in-box path resolves through
the binds to a host object; if the world says that object is a link, its
target re-enters in-box resolution from the top. Fuel-bounded — `none` is a
loop (`ELOOP`), and the verdict for a loop is failure. Returns the final
**in-box** path, so the caller's bind gates apply where the `open` finally
lands. -/
def chaseLinks (binds : List EffectiveBind) (links : List SymlinkFact) :
    Nat → FsPath → Option FsPath
  | 0, _ => none
  | fuel + 1, p =>
    let real := resolveBinds binds p
    match links.find? (fun l => parsePath l.link == real) with
    | some l => chaseLinks binds links fuel (parsePath l.target)
    | none => some p

/-- The full verdict for one probe. -/
structure PredictVerdict where
  /-- The mechanisms permit the access: the top-level bind's write gate,
  then Landlock on the resolved host path. -/
  allow : Bool
  /-- The host object the access reaches. -/
  real : FsPath
  check : ExistCheck
deriving Repr, DecidableEq

/-- The bind-and-existence-aware verdict for a probe of `a` on `p` in a box
running under `cfg`, before symlinks and procfs. -/
def predictVerdictAt (cfg : EffectiveConfig) (p : FsPath) (a : Access) :
    PredictVerdict :=
  let rs := compileLandlock cfg.landlock
  let real := resolveBinds cfg.binds p
  let roGate :=
    match lastMatchingBind cfg.binds p with
    | some b => a != .write || b.writable
    | none => true
  { allow := roGate && rs.allows real a
    real := real
    check := match a with | .read => .«exists» | .write => .creatable }

/-- The link-chasing budget — Linux's own `MAXSYMLINKS` is 40. -/
def maxSymlinks : Nat := 40

/-- The full verdict: procfs first, then symlink chasing, then the bind-
and-Landlock judgment at wherever the chase lands.

**procfs** (`shape.pidns`): the child mounts a fresh private procfs over
`/proc` and re-grants it read-only through Landlock
(`build_confined_command` step 1c) — the pre-fork grant pinned the *host*
procfs inode, which the new mount shadows, so under a pidns the `/proc`
verdict comes from the re-grant alone: reads allowed, writes denied,
whatever the grant lists say about the host's `/proc`. Existence is
namespace-local (`boxLocal`). As everywhere in this layer, `allow` means
the SANDBOX MECHANISMS permit the access — DAC and procfs's own rules
still apply on top (`/proc/<pid>/mem` fails its ptrace check regardless),
which is why the probes touch only `/proc/self` entries whose non-sandbox
answer is known.

**symlinks**: Landlock judges the *resolved* object, so the verdict is
taken at the end of the chase — which is exactly why a symlink planted in
the worktree cannot smuggle access to a path the policy never granted (see
`symlink_no_smuggle` below). A chase that exhausts its fuel is `ELOOP`:
denied, nothing to stat. -/
def predictVerdict (cfg : EffectiveConfig) (links : List SymlinkFact)
    (p : FsPath) (a : Access) : PredictVerdict :=
  if cfg.run.pidns && p.beneath ["proc"] then
    { allow := a == .read, real := p, check := .boxLocal }
  else
    match chaseLinks cfg.binds links maxSymlinks p with
    | some q => predictVerdictAt cfg q a
    | none => { allow := false, real := p, check := .«exists» }

/-- The permission half of the bind layer alone, for the theorems below. -/
def predictAllows (cfg : EffectiveConfig) (p : FsPath) (a : Access) : Bool :=
  (predictVerdictAt cfg p a).allow

/-- A read-only bind denies every write beneath its target, whatever
Landlock would have said — the `MS_RDONLY` remount answers first. This is
why the config-lock binds pin agent config against an in-box agent that
holds a perfectly valid rw grant on its own config directory. -/
theorem ro_bind_denies_write (cfg : EffectiveConfig) (p : FsPath)
    (b : EffectiveBind) (h : lastMatchingBind cfg.binds p = some b)
    (hro : b.writable = false) :
    predictAllows cfg p .write = false := by
  simp [predictAllows, predictVerdictAt, h, hro]

/-- With no matching bind anywhere in the list, resolution is the identity. -/
theorem resolveThrough_id (p : FsPath) :
    ∀ binds : List EffectiveBind,
      (∀ b ∈ binds, p.beneath (parsePath b.target) = false) →
      resolveThrough binds p = p := by
  intro binds
  induction binds with
  | nil => intro _; rfl
  | cons b rest ih =>
    intro h
    have hb := h b (List.mem_cons_self ..)
    simp only [resolveThrough, hb]
    exact ih fun x hx => h x (List.mem_cons_of_mem _ hx)

/-- A fold whose guard never fires keeps its accumulator. -/
private theorem foldl_keeps_acc (p : FsPath) :
    ∀ (binds : List EffectiveBind) (acc : Option EffectiveBind),
      (∀ b ∈ binds, p.beneath (parsePath b.target) = false) →
      List.foldl
        (fun acc b =>
          if p.beneath (parsePath b.target) then some b else acc)
        acc binds = acc := by
  intro binds
  induction binds with
  | nil => intro acc _; rfl
  | cons b rest ih =>
    intro acc h
    have hb := h b (List.mem_cons_self ..)
    simp only [List.foldl_cons, hb]
    exact ih acc fun x hx => h x (List.mem_cons_of_mem _ hx)

theorem lastMatchingBind_none (binds : List EffectiveBind) (p : FsPath)
    (h : ∀ b ∈ binds, p.beneath (parsePath b.target) = false) :
    lastMatchingBind binds p = none :=
  foldl_keeps_acc p binds none h

/-- Away from every bind target, the verdict is exactly the compiled
Landlock ruleset on the probe path itself — so `compile_sound` bounds it. -/
theorem predict_unbound_is_landlock (cfg : EffectiveConfig) (p : FsPath)
    (a : Access) (h : ∀ b ∈ cfg.binds, p.beneath (parsePath b.target) = false) :
    predictAllows cfg p a = (compileLandlock cfg.landlock).allows p a := by
  have hres : resolveBinds cfg.binds p = p :=
    resolveThrough_id p cfg.binds.reverse fun b hb =>
      h b (List.mem_reverse.mp hb)
  have hlast := lastMatchingBind_none cfg.binds p h
  simp [predictAllows, predictVerdictAt, hlast, hres]

/-- Under a pid namespace, `/proc` writes are always denied: the private
procfs is re-granted read-only, and the host `/proc`'s grants pin an inode
the fresh mount shadows. -/
theorem pidns_proc_write_denied (cfg : EffectiveConfig)
    (links : List SymlinkFact) (p : FsPath) (hp : cfg.run.pidns = true)
    (hproc : p.beneath ["proc"] = true) :
    (predictVerdict cfg links p .write).allow = false := by
  simp [predictVerdict, hp, hproc]

/-- …and reads pass the sandbox mechanisms — that is what the re-grant is
for. (Procfs's own rules and DAC still apply beyond this model; see the
module docstring.) -/
theorem pidns_proc_read_allowed (cfg : EffectiveConfig)
    (links : List SymlinkFact) (p : FsPath) (hp : cfg.run.pidns = true)
    (hproc : p.beneath ["proc"] = true) :
    (predictVerdict cfg links p .read).allow = true := by
  simp [predictVerdict, hp, hproc]

section SymlinkExamples

/-! Symlinks judged at the resolved object, machine-checked on a concrete
worktree-only config (`native_decide`, like the nested-bind facts): a link
planted in the granted worktree pointing at an ungranted secret confers
nothing, while a link to a granted file works — Landlock semantics, in the
model, without a mac or a kernel. -/

private def workOnlyCfg : EffectiveConfig :=
  { schema := 1, platform := "linux", claim := "process", work := "/w"
    work_readonly := false
    run := { force_netns := false, notify := false, egress := false,
             pidns := false, interactive := false }
    landlock := { abi := 3, ro := [], rw := ["/w"], skipped_missing := [] }
    binds := []
    namespaces := { user := true, ipc := true, uts := true, net := true,
                    mount := false, pid := false, userns_map := "identity" }
    net := { mode := .deny, egress := [], user_egress_allow := [],
             loopback := [], loopback_runtime := [], unix_sockets := false }
    seccomp := { template := "deny-v1", notify := false }
    rlimits := { mem_bytes := none, max_procs := none, fsize_bytes := none,
                 cpu_secs := none, wall_secs := 1 }
    env_pass := [], tools := []
    resolution := { profile := "default", fs_deny := [] } }

/-- A worktree symlink to an ungranted secret is denied: the verdict lands
on the chase's end, not on the link's granted parent. -/
theorem symlink_no_smuggle :
    (predictVerdict workOnlyCfg
      [{ link := "/w/evil", target := "/home/u/.ssh/key" }]
      ["w", "evil"] .read).allow = false := by native_decide

/-- The control: a link to a granted file confers exactly that file. -/
theorem symlink_within_grants_ok :
    (predictVerdict workOnlyCfg
      [{ link := "/w/alias", target := "/w/data.txt" }]
      ["w", "alias"] .read).allow = true := by native_decide

/-- A link cycle is `ELOOP`: denied, whatever the grants say. -/
theorem symlink_loop_denied :
    (predictVerdict workOnlyCfg
      [{ link := "/w/a", target := "/w/b" },
       { link := "/w/b", target := "/w/a" }]
      ["w", "a"] .read).allow = false := by native_decide

end SymlinkExamples

section NestedExamples

/-! Nesting, machine-checked in both directions plus the chained case. All
paths are concrete; `native_decide` evaluates them (kernel reduction stalls
on `String.splitOn`, so these four facts additionally trust the compiler —
the same trust the DRT harness already places in it by running the
extracted binary). -/

private def bindAt (target source : String) (writable : Bool) :
    EffectiveBind :=
  { kind := .homeState, source := source, target := target,
    writable := writable }

/-- A later, deeper bind refines an earlier, shallower one: `/t/sub` wins
for paths beneath it. -/
theorem nested_deeper_later_wins :
    resolveBinds [bindAt "/t" "/a" true, bindAt "/t/sub" "/b" true]
      ["t", "sub", "x"] = ["b", "x"] := by native_decide

/-- A later, shallower bind hides an earlier, deeper one: everything under
`/t` — including the old `/t/sub` mountpoint — now resolves inside `/a`. -/
theorem nested_shallower_later_shadows :
    resolveBinds [bindAt "/t/sub" "/b" true, bindAt "/t" "/a" true]
      ["t", "sub", "x"] = ["a", "sub", "x"] := by native_decide

/-- Chained resolution: the later bind's source lies beneath the earlier
bind's target, so the mount captured the earlier bind's subtree and the
path resolves through both. -/
theorem nested_source_under_earlier_target :
    resolveBinds [bindAt "/c" "/host" true, bindAt "/d" "/c/inner" true]
      ["d", "x"] = ["host", "inner", "x"] := by native_decide

/-- Sibling paths beneath the outer target are untouched by the inner
bind. -/
theorem nested_sibling_unaffected :
    resolveBinds [bindAt "/t" "/a" true, bindAt "/t/sub" "/b" true]
      ["t", "other"] = ["a", "other"] := by native_decide

end NestedExamples

end H5iSpec
