import Lean

/-!
The Lean mirror of `crates/h5i-sandbox/src/effective.rs` (ROADMAP.md §V3, L1).

Field names ARE the JSON keys (Lean's derived `ToJson`/`FromJson` use them
verbatim), so they match the Rust serde output exactly: change a field there
and the DRT harness fails until this file follows. Enum encodings that serde
renames (`kebab-case` bind kinds, `lowercase` net modes) get manual instances.

One deliberate encoding difference: serde serializes `Option::None` as
`null`, Lean's derived `ToJson` omits the field. The DRT harness compares
null-stripped values, and nothing in this schema distinguishes null from
absent, so the difference is semantically empty.
-/

namespace H5iSpec

open Lean

/-- Mirror of Rust `NetMode` (serde `lowercase`). -/
-- `DecidableEq` only (no derived `BEq`): `==` then comes from
-- `instBEqOfDecidableEq`, which is lawful — the theorems need `beq_iff_eq`.
inductive NetMode where
  | deny
  | host
deriving Repr, DecidableEq

instance : ToJson NetMode where
  toJson
    | .deny => Json.str "deny"
    | .host => Json.str "host"

instance : FromJson NetMode where
  fromJson? j := do
    match (← j.getStr?) with
    | "deny" => pure .deny
    | "host" => pure .host
    | other => throw s!"unknown net mode '{other}'"

/-- Mirror of Rust `BindKind` (serde `kebab-case`), in child apply order. -/
inductive BindKind where
  | configLock
  | «private»
  | homeState
  | cacheRo
  | cacheRw
deriving Repr, DecidableEq

instance : ToJson BindKind where
  toJson
    | .configLock => Json.str "config-lock"
    | .«private» => Json.str "private"
    | .homeState => Json.str "home-state"
    | .cacheRo => Json.str "cache-ro"
    | .cacheRw => Json.str "cache-rw"

instance : FromJson BindKind where
  fromJson? j := do
    match (← j.getStr?) with
    | "config-lock" => pure .configLock
    | "private" => pure .«private»
    | "home-state" => pure .homeState
    | "cache-ro" => pure .cacheRo
    | "cache-rw" => pure .cacheRw
    | other => throw s!"unknown bind kind '{other}'"

/-- Mirror of Rust `RunShape`. -/
structure RunShape where
  force_netns : Bool
  notify : Bool
  egress : Bool
  pidns : Bool
  interactive : Bool
deriving Repr, DecidableEq, BEq, ToJson, FromJson

/-- Mirror of Rust `EffectiveBind`. -/
structure EffectiveBind where
  kind : BindKind
  source : String
  target : String
  writable : Bool
deriving Repr, DecidableEq, BEq, ToJson, FromJson

/-- Mirror of Rust `LandlockEffective`. -/
structure LandlockEffective where
  abi : Int
  ro : List String
  rw : List String
  skipped_missing : List String
deriving Repr, DecidableEq, BEq, ToJson, FromJson

/-- Mirror of Rust `NamespacesEffective`. -/
structure NamespacesEffective where
  user : Bool
  ipc : Bool
  uts : Bool
  net : Bool
  mount : Bool
  pid : Bool
  userns_map : String
deriving Repr, DecidableEq, BEq, ToJson, FromJson

/-- Mirror of Rust `NetEffective`. -/
structure NetEffective where
  mode : NetMode
  egress : List String
  user_egress_allow : List String
  loopback : List Nat
  loopback_runtime : List Nat
  unix_sockets : Bool
deriving Repr, DecidableEq, BEq, ToJson, FromJson

/-- Mirror of Rust `SeccompEffective`. -/
structure SeccompEffective where
  template : String
  notify : Bool
deriving Repr, DecidableEq, BEq, ToJson, FromJson

/-- Mirror of Rust `RlimitsEffective`. -/
structure RlimitsEffective where
  mem_bytes : Option Nat
  max_procs : Option Nat
  fsize_bytes : Option Nat
  cpu_secs : Option Nat
  wall_secs : Nat
deriving Repr, DecidableEq, BEq, ToJson, FromJson

/-- Mirror of Rust `ResolutionMeta`. `fs_deny` is resolution metadata, never
an enforcement rule — Landlock is allowlist-only (§V2). -/
structure ResolutionMeta where
  profile : String
  fs_deny : List String
deriving Repr, DecidableEq, BEq, ToJson, FromJson

/-- Mirror of Rust `EffectiveConfig`, schema v1. -/
structure EffectiveConfig where
  schema : Nat
  platform : String
  claim : String
  work : String
  work_readonly : Bool
  run : RunShape
  landlock : LandlockEffective
  binds : List EffectiveBind
  namespaces : NamespacesEffective
  net : NetEffective
  seccomp : SeccompEffective
  rlimits : RlimitsEffective
  env_pass : List String
  tools : List String
  resolution : ResolutionMeta
deriving Repr, DecidableEq, BEq, ToJson, FromJson

/-- Mirror of Rust `EFFECTIVE_SCHEMA`. -/
def effectiveSchema : Nat := 1

/-- Mirror of Rust `SECCOMP_DENY_TEMPLATE`. -/
def seccompDenyTemplate : String := "deny-v1"

end H5iSpec
