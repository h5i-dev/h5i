import H5iSpec.Input

/-!
The executable model of `compute_effective`
(`crates/h5i-sandbox/src/effective.rs`), mirrored decision for decision. The
DRT harness (`tests/effective_drt.rs`) holds this function and the Rust one to
the same answers; the theorems in `H5iSpec.Theorems` are proved against this
definition, so a DRT mismatch is exactly a gap between what was proved and
what ships.

Factored into named pieces (grants, binds, namespaces) so proofs can unfold
one decision at a time instead of one thousand.
-/

namespace H5iSpec

/-- Rust `expand_tilde`: a leading `~` expands to HOME when HOME is set;
anything else (including `$WORK`) is left as-is. -/
def expandTilde (home : Option String) (path : String) : String :=
  if path == "~" || path.startsWith "~/" then
    match home with
    | some h => h ++ path.drop 1
    | none => path
  else path

/-- Rust `PathBuf::join` for the private-bind target: an absolute `rel`
replaces the base wholesale (`private_paths` validation refuses those, but the
model mirrors the mechanism, not the validator). -/
def joinPath (base rel : String) : String :=
  if rel.startsWith "/" then rel else base ++ "/" ++ rel

/-- The Landlock grant computation: rw = `$WORK` (unless readonly) + existing
`fs_write` entries; ro = existing `fs_read` entries (+ `$WORK` when readonly);
a granted path absent from the world is *skipped* — the sandbox narrows, the
fail-closed direction — and recorded. Skips accumulate in scan order:
`fs_write` first, then `fs_read`, matching the Rust loop order. -/
def landlockOf (i : DrtInput) : LandlockEffective :=
  let home := i.world.home
  let writeGrants :=
    (i.profile.fs_write.filter (· != "$WORK")).map (expandTilde home)
  let readGrants := i.profile.fs_read.map (expandTilde home)
  let rw :=
    (if i.runtime.work_readonly then [] else [i.work])
      ++ writeGrants.filter i.world.existsPath
  let ro :=
    readGrants.filter i.world.existsPath
      ++ (if i.runtime.work_readonly then [i.work] else [])
  let skipped :=
    writeGrants.filter (fun p => !i.world.existsPath p)
      ++ readGrants.filter (fun p => !i.world.existsPath p)
  { abi := i.landlock_abi, ro := ro, rw := rw, skipped_missing := skipped }

/-- Rust `config_lock_paths`: the project-scope agent config dirs that exist
under `$WORK`, then the user-scope config files that exist under HOME. -/
def configLockPaths (i : DrtInput) : List String :=
  let projectDirs :=
    ([".claude", ".codex"].map (joinPath i.work)).filter i.world.isDir
  let homeFiles :=
    match i.world.home with
    | some h =>
      ([".claude/settings.json", ".codex/config.toml"].map (joinPath h)).filter
        i.world.isFile
    | none => []
  projectDirs ++ homeFiles

/-- Rust `home_binds_in_mount_order`: the private `/tmp` bind mounts last (its
backing can live below a repository in `/tmp`; mounting it first would hide
the other binds' sources). A stable sort on `target == "/tmp"`, i.e. keep
order, `/tmp` to the back. -/
def homeBindsInMountOrder (binds : List PathPair) : List PathPair :=
  binds.filter (fun b => b.target != "/tmp")
    ++ binds.filter (fun b => b.target == "/tmp")

/-- The bind list, in the child's apply order (steps 1d–1h): config-lock,
private, home-state, cache-ro, cache-rw. -/
def bindsOf (i : DrtInput) : List EffectiveBind :=
  let configLock :=
    if i.shape.interactive then
      (configLockPaths i).map fun p =>
        { kind := .configLock, source := p, target := p, writable := false }
    else []
  let «private» := i.runtime.private_binds.map fun b =>
    { kind := .«private», source := b.backing,
      target := joinPath i.work b.rel, writable := true }
  let home := (homeBindsInMountOrder i.runtime.home_binds).map fun b =>
    { kind := .homeState, source := b.backing, target := b.target,
      writable := true }
  let cacheRo := i.runtime.ro_binds.map fun b =>
    { kind := .cacheRo, source := b.backing, target := b.target,
      writable := false }
  let cacheRw :=
    match i.runtime.cache_write with
    | some b =>
      [{ kind := .cacheRw, source := b.backing, target := b.target,
         writable := true }]
    | none => []
  configLock ++ «private» ++ home ++ cacheRo ++ cacheRw

/-- The namespace set: user/ipc/uts always; net when egress is denied or the
tier forces it; mount when anything needs a private mount table (pidns's
procfs, the egress `/etc/hosts` pin, any bind); pid per shape. The userns maps
to root exactly on the egress path (`nft` needs root-in-userns). -/
def namespacesOf (i : DrtInput) (binds : List EffectiveBind) :
    NamespacesEffective :=
  { user := true
    ipc := true
    uts := true
    net := i.profile.net_mode == .deny || i.shape.force_netns
    mount := i.shape.pidns || i.shape.egress || !binds.isEmpty
    pid := i.shape.pidns
    userns_map := if i.shape.egress then "root" else "identity" }

/-- The model: what `build_confined_command` applies for this input. -/
def computeEffective (i : DrtInput) : EffectiveConfig :=
  let binds := bindsOf i
  { schema := effectiveSchema
    platform := "linux"
    claim := i.claim
    work := i.work
    work_readonly := i.runtime.work_readonly
    run := i.shape
    landlock := landlockOf i
    binds := binds
    namespaces := namespacesOf i binds
    net :=
      { mode := i.profile.net_mode
        egress := i.profile.net_egress
        user_egress_allow := i.runtime.user_egress_allow
        loopback := i.profile.loopback_ports
        loopback_runtime := i.runtime.loopback_ports
        unix_sockets := i.profile.unix_sockets }
    seccomp := { template := seccompDenyTemplate, notify := i.shape.notify }
    rlimits :=
      { mem_bytes := i.profile.mem_bytes
        max_procs := i.profile.max_procs
        fsize_bytes := i.profile.fsize_bytes
        cpu_secs := i.profile.cpu_secs
        wall_secs := i.profile.wall_secs }
    env_pass := i.profile.env_pass
    tools := i.profile.tools
    resolution := { profile := i.profile.name, fs_deny := i.profile.fs_deny } }

end H5iSpec
