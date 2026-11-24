# H5iAppVeil (Optional Veil Temporal Verification Backend)

`H5iAppVeil` is an **optional** standalone Lake package that connects h5i to
[Veil](https://github.com/verse-lab/veil) (`v4.35.0`, including Lentil & Lasso)
as a thin adapter — without reimplementing temporal logic, fairness, or model
checking inside h5i.

## Responsibility Split

- **h5i (`H5iAppLib`, Rust `h5i-app-*`)**: verified application framework,
  Aeneas-extracted kernel proofs (`v4.31.0`), `Runs.lean`, `Time.lean`, and
  counterexample replay directly against the real Rust implementations
  (`MemoryEngine`, `FlashStore`, `DeviceEngine`).
- **Veil (`verse-lab/veil`, `v4.35.0`)**: state transition systems
  (`RelationalTransitionSystem`), temporal logic & fairness (`Lentil`),
  explicit-state model checking (`#model_check`), temporal/liveness model
  checking (`#temporal_check` via Lasso), and SMT invariant checking
  (`#check_invariants`).
- **H5iAppVeil (`crates/h5i-app-veil/proofs`)**: thin optional integration layer:
  - `KernelBridge.lean`: `Kernel` (`Step = apply ∘ transition`) → `Veil.RelationalTransitionSystem`
  - `RunBridge.lean`: finite `Run` ↔ `Veil.RelationalTransitionSystem.reachable`,
    `Veil.ModelChecker.Trace`, and `Lentil.always`
  - `Refinement.lean`: cross-state stuttering refinement (`StutterRefines`) on
    top of `Veil.RelationalTransitionSystem` (Veil provides same-state
    `reachable_inclusion`; cross-state refinement bridges concrete runtimes to
    abstract kernels)
  - `Embedded/FlashStore.lean`: Veil DSL models (`FlashStorePlain` discovering
    3-bit wraparound via `#model_check`, `FlashStoreModular` passing
    `#model_check` + `#temporal_check` under `𝒲ℱ`)
  - `Embedded/DeviceEngine.lean`: Veil DSL models (`DeviceEngineUnlatched`
    discovering `changed()` coalescing via `#temporal_check`,
    `DeviceEngineLatched` passing `#model_check` + `#temporal_check` under `𝒲ℱ`)
    plus a `StutterRefines` proof to the abstract motor kernel.

## Why a separate Lake package?

1. `H5iAppLib` (`crates/h5i-app-core/proofs`) has **zero** dependency on Veil.
2. Aeneas pins Lean `v4.31.0`, while Veil (`v4.35.0` with Lentil/Lasso) targets
   Lean `v4.35.0`. Isolating `H5iAppVeil` in its own Lake package keeps normal
   `h5i app prove`, `h5i app check`, root `lakefile.lean`, and CI completely
   unaffected.

## Usage

```bash
# Build & run Veil model checks and temporal checks (requires elan + Lean v4.35.0)
cd crates/h5i-app-veil/proofs
lake build

# Or via h5i CLI
h5i app model-check
h5i app trace-replay <veil-trace.json>
```
