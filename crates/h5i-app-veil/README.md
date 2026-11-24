# H5iAppVeil (Optional Veil Temporal Verification Backend)

`H5iAppVeil` is an **optional** standalone Lake package that connects h5i to
[Veil](https://github.com/verse-lab/veil) (`v4.35.0`, including Lentil & Lasso)
as a thin adapter — without reimplementing temporal logic, fairness, or model
checking inside h5i.

## Current Integration Scope (Model-Level Only)

> **Toolchain boundary**: `h5i`'s Aeneas-extracted kernels and `H5iAppLib` pin
> Lean `v4.31.0` (`crates/h5i-app-cli/src/pins.rs`, `Aeneas.Std.Result`), whereas
> Veil `v4.35.0` (which provides Lentil and Lasso `#temporal_check`) requires
> Lean `v4.35.0` (no `v4.31.0` Veil tag exists; upstream Aeneas `main` is on
> `v4.36.0`).
>
> Because the two toolchains cannot be imported into a single Lake build today:
> - `H5iAppVeil` currently performs **model-level** temporal verification and
>   refinement on Lean `v4.35.0` specifications (`H5iAppVeil.ModelRun`), rather
>   than directly importing Aeneas-extracted `H5iAppLib.Run` proofs (`v4.31.0`).
> - Runtime counterexamples discovered by Veil are validated against the real
>   unmodified Rust implementations (`FlashStore`, `DeviceEngine`, `MemoryEngine`)
>   via `h5i app trace-replay` and `crates/h5i-app-embedded/src/replay.rs`.
> - Direct formal composition of Aeneas-extracted kernels with Veil will become
>   possible once Aeneas and Veil align on a shared Lean toolchain.

## Responsibility Split

- **h5i (`H5iAppLib`, Rust `h5i-app-*`)**: verified application framework,
  Aeneas-extracted kernel proofs (`v4.31.0`), `Runs.lean`, `Time.lean`, and
  strict counterexample/lasso replay directly against the real Rust
  implementations (`MemoryEngine`, `FlashStore`, `DeviceEngine`).
- **Veil (`verse-lab/veil`, `v4.35.0`)**: state transition systems
  (`RelationalTransitionSystem`), temporal logic & fairness (`Lentil`),
  explicit-state model checking (`#model_check`), temporal/liveness model
  checking (`#temporal_check` via Lasso), and SMT invariant checking
  (`#check_invariants`).
- **H5iAppVeil (`crates/h5i-app-veil/proofs`)**: thin optional integration layer:
  - `KernelBridge.lean`: pure Lean `Kernel` (`Step = apply ∘ transition`) →
    `Veil.RelationalTransitionSystem`
  - `RunBridge.lean`: `ModelRun` ↔ `Veil.RelationalTransitionSystem.reachable`,
    `Veil.ModelChecker.Trace`, and `Lentil.always`
  - `Refinement.lean`: cross-state stuttering refinement (`StutterRefines`) on
    top of `Veil.RelationalTransitionSystem`
  - `Embedded/FlashStore.lean`: Veil DSL models of `store.rs` (`FlashStorePlain`
    discovering 3-bit wraparound via `#model_check`, `FlashStoreModular` modeling
    2-slot append per region, unchanged-save no-op, partial erase, torn writes,
    post-write/pre-ack crashes, and repeated reboots)
  - `Embedded/DeviceEngine.lean`: Veil DSL models of `kernel.rs` (real wrapping
    `version := version + 1`, two independent observers, separate `powerLoss` /
    `restore` / `actuatorPoll` steps) plus a `StutterRefines` proof directly on
    the **same** Veil-generated `DeviceEngineLatched.relationalTransitionSystem`.

## Usage

```bash
# Build & run Veil model checks and temporal checks (requires elan + Lean v4.35.0)
cd crates/h5i-app-veil/proofs
lake build

# Or via h5i CLI (requires explicit obligations in h5i-app.toml)
h5i app temporal-check --enable examples/embedded/thermostat
h5i app model-check
h5i app trace-replay <veil-trace.json>
```
