# h5i-spec: the Lean model beside the Rust

The formal side of ROADMAP.md's verification chapter (sections V1 to V6).
Nothing here is linked into the `h5i` binary and nothing here runs inside a
box; the model is a sibling of the implementation, held to the same answers
by differential testing and by conformance probes against real boxes.

> **2026-08-16 pivot.** The effort moved off the hand-ported twin of
> `compute_effective` (verifying a re-implementation of the resolver added
> maintenance cost without catching the bugs that matter) and onto an
> attack-driven **filesystem authority machine** — see ROADMAP §V3 for the
> H5iFs design. The files below are what survived the pivot: the mechanism
> semantics the new machine builds on, plus the two specs the *product*
> actually runs against (the interference checker and the conformance
> predictions). The retired files — `Model`, `Input`, `Theorems`, `Phase`,
> `Refinement`, and the whole-config `effective_drt` harness — live in git
> history.

## What is in here

- `H5iSpec/Effective.lean`: the Lean mirror of the effective-config schema
  (`crates/h5i-sandbox/src/effective.rs`), field for field. Still the shared
  vocabulary the probe and interferes inputs are phrased in.
- `H5iSpec/Landlock.lean`: L0, the Landlock fragment — rulesets as
  allowlists over path-beneath scopes, domains as intersecting stacks,
  `restrict_narrows` and `deny_persists`, plus `parsePath`/`compileLandlock`,
  the bridge from a dump's grant strings to component-path rules. This is the
  base layer the H5iFs authority machine extends.
- `H5iSpec/Noninterference.lean`: the box-to-box interference checker.
  `interferesCheck` decides whether two compiled rulesets share a
  writable-by-one, readable-by-the-other path; `interferesCheck_sound` is
  the direction the product relies on. This is the **specification of the
  Rust `effective::interferes`** behind the `fs_overlap` receipt; the
  trace-level "no shared path ⇒ invisible" statement is deferred to H5iFs,
  which has the object identity such a claim needs.
- `H5iSpec/Predict.lean`: the probe verdicts — bind-, existence-, symlink-
  and procfs-aware. Resolution recurses through the bind stack
  (`resolveBinds`; the `nested_*` facts pin shadowing and chained
  source-under-target resolution), symlinks are chased fuel-bounded with
  the verdict taken at the resolved object (`symlink_no_smuggle`: a
  worktree link to an ungranted secret confers nothing), `/proc` under a
  pidns is the private procfs with its read-only re-grant
  (`pidns_proc_write_denied`/`pidns_proc_read_allowed`), read-only
  remounts deny writes outright (`ro_bind_denies_write`), and each verdict
  carries the resolved host path plus the existence check the harness must
  measure — the model owns the semantics, the harness owns the stat.
- `H5iSpec/Seatbelt.lean`: the macOS backend's own refinement — a model of
  the file-rule fragment `seatbelt::build_profile` emits, in its exact
  order, under SBPL's last-match-wins with `(deny default)`. The opposite
  regime from Landlock: denies exist here, and `fs_deny_wins` proves the
  generator's deny tail beats every grant — `fs.deny` is genuinely
  enforced on Seatbelt where on Linux it is a resolution lint. The
  generator is pure and compiles on Linux, so `tests/seatbelt_drt.rs`
  parses its SBPL text and diffs the file rules against this model's
  emission, structurally.
- `Main.lean`: the executable, three modes — `--predict` (a dump plus
  probes in, per-probe allow/deny out; `tests/effective_probes.rs` holds a
  real box to those verdicts), `--interferes` (config pairs in,
  `interferesCheck` verdicts out; the oracle for the Rust
  `effective::interferes` behind the `fs_overlap` receipt), and
  `--seatbelt` (a `SeatbeltInput` in, the modeled SBPL file rules out).

## Build and test

```
cd lean
lake build          # compiles the model and checks every theorem
cd ..
cargo test --test interferes_drt --test seatbelt_drt --test effective_probes
```

The harnesses skip loudly when this package has not been built; CI
(`.github/workflows/lean-drt.yml`) sets `H5I_DRT_REQUIRE=1` so absence fails
there. `H5I_DRT_SEED` reproduces a run.

## Rules of the house

- Lean core only. No mathlib, no external dependencies: this package builds
  in seconds from a bare toolchain, and the harnesses are part of the Rust
  test surface. (This constraint is load-bearing for H5iFs too — see §V3.)
- The kept files model the *mechanism* and specify what the product runs;
  they are not a re-derivation of the resolver. The new authority machine
  is built attack-first: a theorem holds only after a defense is added, not
  the instant its definitions unfold.
