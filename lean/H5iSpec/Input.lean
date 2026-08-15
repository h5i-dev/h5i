import H5iSpec.Effective

/-!
The DRT input: one invocation's policy, runtime state, shape — and its
**world**. The Rust `compute_effective` consults the host (`Path::exists`,
`is_dir`/`is_file`, `$HOME`); a pure model cannot, so the harness makes the
world explicit: which paths exist (split by kind, because the Rust checks
differ), and what HOME is. The harness materializes exactly this world in a
tempdir before running the Rust side, so "exists on the host" and "member of
the world" coincide by construction.

This is not just test plumbing: L0's mechanism semantics needs the filesystem
as explicit state anyway, and this `World` is its first, smallest form.
-/

namespace H5iSpec

open Lean

/-- The profile subset `compute_effective` reads (mirrors Rust `Profile`). -/
structure ProfileInput where
  name : String
  fs_read : List String
  fs_write : List String
  fs_deny : List String
  net_mode : NetMode
  net_egress : List String
  loopback_ports : List Nat
  unix_sockets : Bool
  mem_bytes : Option Nat
  max_procs : Option Nat
  fsize_bytes : Option Nat
  cpu_secs : Option Nat
  wall_secs : Nat
  env_pass : List String
  tools : List String
deriving Repr, DecidableEq, BEq, ToJson, FromJson

/-- A (backing, target) bind pair (Rust `HomeBind`/`RoBind`/cache bind). -/
structure PathPair where
  backing : String
  target : String
deriving Repr, DecidableEq, BEq, ToJson, FromJson

/-- A private-path bind: backing dir shadowing `$WORK/<rel>` (Rust
`PrivateBind`). `rel` is workspace-relative by construction. -/
structure PrivateBindInput where
  backing : String
  rel : String
deriving Repr, DecidableEq, BEq, ToJson, FromJson

/-- The runtime-only `ResolvedPolicy` fields `compute_effective` reads. -/
structure RuntimeInput where
  work_readonly : Bool
  private_binds : List PrivateBindInput
  home_binds : List PathPair
  ro_binds : List PathPair
  cache_write : Option PathPair
  user_egress_allow : List String
  loopback_ports : List Nat
deriving Repr, DecidableEq, BEq, ToJson, FromJson

/-- The filesystem facts the Rust side reads from the host. -/
structure World where
  /-- Paths that exist as regular files. -/
  files : List String
  /-- Paths that exist as directories. -/
  dirs : List String
  /-- `$HOME`, when set. -/
  home : Option String
deriving Repr, DecidableEq, BEq, ToJson, FromJson

/-- Does `p` exist at all (Rust `Path::exists`)? -/
def World.existsPath (w : World) (p : String) : Bool :=
  w.files.contains p || w.dirs.contains p

/-- Is `p` a directory (Rust `Path::is_dir`)? -/
def World.isDir (w : World) (p : String) : Bool :=
  w.dirs.contains p

/-- Is `p` a regular file (Rust `Path::is_file`)? -/
def World.isFile (w : World) (p : String) : Bool :=
  w.files.contains p

/-- One DRT case: everything `compute_effective` reads, made explicit. -/
structure DrtInput where
  claim : String
  profile : ProfileInput
  runtime : RuntimeInput
  /-- Canonicalized workspace path. -/
  work : String
  landlock_abi : Int
  shape : RunShape
  world : World
deriving Repr, DecidableEq, BEq, ToJson, FromJson

end H5iSpec
