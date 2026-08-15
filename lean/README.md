# h5i-spec: the Lean model beside the Rust

The formal side of ROADMAP.md's verification chapter (sections V1 to V6).
Nothing here is linked into the `h5i` binary and nothing here runs inside a
box; the model is a sibling of the implementation, held to the same answers
by differential testing.

## What is in here

- `H5iSpec/Effective.lean`: the Lean mirror of the effective-config schema
  (`crates/h5i-sandbox/src/effective.rs`), field for field. The JSON keys are
  the field names, so serde and Lean stay aligned or the harness fails.
- `H5iSpec/Input.lean`: the DRT input, including the explicit **world**
  (which paths exist, what HOME is). The Rust side reads the host; the model
  reads the world; the harness makes them coincide by materializing the world
  in a tempdir.
- `H5iSpec/Model.lean`: `computeEffective`, the executable model of the Rust
  `compute_effective` — the function `build_confined_command` enforces from.
- `H5iSpec/Theorems.lean`: theorems over all inputs, machine-checked on every
  `lake build`. Notably `readonly_work_not_rw` is *conditional*, and its
  side condition is a real caller obligation the Rust comments only state in
  prose.
- `H5iSpec/Landlock.lean`: L0, the Landlock fragment — rulesets as
  allowlists over path-beneath scopes, domains as intersecting stacks,
  `restrict_narrows` and `deny_persists`.
- `H5iSpec/Phase.lean`: the phase machine (fds as capabilities with rights
  fixed at open, `restrict_self` as the transition) and the conditional
  phase theorem in both directions: `phase_confidentiality` (install-phase
  denial confines forever) and `run_deny_insufficient` (run-phase denial
  alone does not — the fd-smuggle trace is the machine-checked witness),
  plus `shared_tmp_survives`, the agent profile's `/tmp` footgun as a
  `decide`-closed fact.
- `Main.lean`: the DRT executable — `DrtInput` array on stdin, the model's
  `EffectiveConfig` array on stdout.

## Build and test

```
cd lean
lake build          # compiles the model and checks every theorem
cd ..
cargo test --test effective_drt
```

The harness (`tests/effective_drt.rs`) generates policies, materializes
their filesystem world, runs both sides, and diffs. It skips loudly when
this package has not been built; CI (`.github/workflows/lean-drt.yml`) sets
`H5I_DRT_REQUIRE=1` so absence fails there. `H5I_DRT_SEED` and
`H5I_DRT_CASES` reproduce and scale a run.

## Rules of the house

- Lean core only. No mathlib, no external dependencies: this package builds
  in seconds from a bare toolchain, and the DRT harness is part of the Rust
  test surface.
- The model mirrors the *mechanism*, not the validator: what
  `compute_effective` computes, including its warts, with theorems making the
  warts explicit rather than modeling them away.
- A DRT mismatch is a finding, never noise: either the model is wrong (fix it
  here) or the Rust changed meaning (the failure is the review).
