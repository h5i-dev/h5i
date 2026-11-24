# H5iAppVeil (Optional Veil Temporal Verification Backend)

`H5iAppVeil` is an **optional** standalone Lake package that connects h5i's
Aeneas-extracted kernels and `H5iAppLib.Run` / `H5iAppLib.ReachableT` directly
to [Veil](https://github.com/verse-lab/veil) (`v4.35.0`, including Lentil &
Lasso) as a thin adapter — without reimplementing temporal logic, fairness, or
model checking inside h5i.

## Formal Integration Scope

- **Shared `v4.35.0` Lean toolchain**: `H5iAppVeil` pins both
  `verse-lab/veil @ v4.35.0` and `AeneasVerif/aeneas @ 1238879f9ea550512096e9bf8bd6fe7407b3eaef`
  (`leanprover/lean4:v4.35.0`), sharing identical transitive dependencies.
- **Direct bridge to `H5iAppLib` & Aeneas-extracted kernels**:
  - Standard h5i builds and `crates/h5i-app-core/proofs` remain completely
    unaffected (still on `v4.31.0`, zero Veil dependency).
  - When Veil is enabled, `H5iAppVeil` compiles the actual
    `crates/h5i-app-core/proofs/H5iAppLib/{Basic,Runs,Time}.lean` alongside Veil
    and bridges directly to `H5iAppLib.Run`, `H5iAppLib.Event`, and
    `H5iAppLib.ReachableT` (no duplicated `Run` or `Event` definitions).
  - Application temporal packages (such as
    `examples/embedded/thermostat/proofs/temporal`) import the actual
    Aeneas-extracted Rust kernel (`ThermostatKernel.lean`) and its safety proofs
    (`Proofs.lean`), lifting `H5iAppLib.Run` invariants and proving temporal
    liveness theorems directly over the extracted transition function.
- **Explicit distinction between scopes**:
  - **Implementation-level theorems**: proved in Lean over the actual
    Aeneas-extracted kernel (`thermostat_kernel.transition`,
    `H5iAppLib.Run`, `H5iAppVeil.kernelRTS`, `StutterRefines`, `Lentil`).
  - **Model-level finite-state checks**: explored exhaustively via Veil's
    explicit-state model checker (`#model_check`) and Lasso temporal checker
    (`#temporal_check`) over abstract finite-state DSL models.
  - **Runtime replay**: Veil counterexample traces can be replayed against real
    Rust implementations (`FlashStore`, `DeviceEngine`, `MemoryEngine`) via
    `h5i app trace-replay`. For liveness lasso traces on `DeviceEngine`, replay
    checks closure over the projected observable state
    `(snapshot, notify_pending, actuator_motor_on)` (`ProjectedCycleReproduced`).

## Responsibility Split

- **h5i (`H5iAppLib`, Rust `h5i-app-*`)**: verified application framework,
  Aeneas-extracted kernel proofs, `Runs.lean`, `Time.lean`, and strict
  counterexample replay against real Rust implementations.
- **Veil (`verse-lab/veil`, `v4.35.0`)**: state transition systems
  (`RelationalTransitionSystem`), temporal logic & fairness (`Lentil`),
  explicit-state model checking (`#model_check`), temporal/liveness model
  checking (`#temporal_check` via Lasso), and SMT invariant checking
  (`#check_invariants`).
- **H5iAppVeil (`crates/h5i-app-veil/proofs`)**: thin optional integration layer:
  - `KernelBridge.lean`: Aeneas-extracted `transition` (`Aeneas.Std.Result`) →
    `Veil.RelationalTransitionSystem` (`kernelRTS`, `timedKernelRTS`)
  - `RunBridge.lean`: `H5iAppLib.Run` ↔ `Veil.RelationalTransitionSystem.reachable`,
    `Veil.ModelChecker.Trace`, `Lentil.always`, and `H5iAppLib.ReachableT` ↔
    `timedKernelRTS.reachable`
  - `Refinement.lean`: cross-state stuttering refinement (`StutterRefines`) on
    top of `Veil.RelationalTransitionSystem`
  - `Embedded/FlashStore.lean`: Veil DSL models comparing the current Rust
    `FlashStore` wraparound design (`FlashStorePlain`, discovering 3-bit
    wraparound via `#model_check`) and a proposed modular multi-slot design
    (`FlashStoreModular`, modeling 2-slot append per region, unchanged-save
    no-op, partial erase, torn writes, post-write/pre-ack crashes, and repeated
    reboots)
  - `Embedded/DeviceEngine.lean`: Veil DSL models of `kernel.rs` (real wrapping
    `version := version + 1`, two independent observers, separate `powerLoss` /
    `restore` / `actuatorPoll` steps) plus a `StutterRefines` proof directly on
    the Veil-generated `DeviceEngineLatched.relationalTransitionSystem`.

## Usage

```bash
# Build & run Veil model checks and temporal checks (requires elan + Lean v4.35.0)
cd crates/h5i-app-veil/proofs
lake exe cache get
lake build

# Or via h5i CLI (requires explicit obligations in h5i-app.toml)
h5i app temporal-check --enable examples/embedded/thermostat
h5i app model-check
h5i app trace-replay <veil-trace.json>
```
