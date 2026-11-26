# Verify temporal properties with h5i and Veil

Use `H5iAppLib.Run` and `ReachableT` for properties over kernel runs and
timed reachability. Enable the optional `H5iAppVeil` Lake package for Lentil
temporal formulas and fairness, and Veil's finite-state safety and liveness
checks. `KernelBridge.lean` and `RunBridge.lean` connect the extracted Rust
`transition` and h5i run semantics to Veil transition systems.

## Specify the verification scope

- For `kernel_implementation_level`, prove the property over the actual
  Aeneas-extracted kernel and its runs.
- For `compositional`, state which runtime and environment transitions are
  modeled alongside the extracted code. A claim about the full runtime
  needs a refinement argument or additional runtime evidence.
- For `model_level`, state the finite model, instantiation, and property
  explored. Report a passing exhaustive verdict within that scope.

For liveness, state the environment assumptions and fairness of individual
actions. The thermostat example's overtemperature theorem assumes persistent
overtemperature or sensor failure and weak fairness of the sensing and
actuation actions. Its conclusion concerns eventual heater shutdown in the
composed system.

## Run and read the gate

Use `examples/embedded/thermostat/h5i-app.toml` and `proofs/temporal/` under
that example as templates. Keep the temporal package on its checked-in Lean
and dependency pins. The Veil package in this repository uses Lean 4.35.0.

- Under `[verification]`, set `temporal_backend = "veil"`, `temporal_dir`,
  and the Lake modules in `temporal_targets`.
- List Lean obligations in `[[verification.temporal_theorems]]` with
  `name`, `scope`, and an expected `statement` where applicable. The gate
  checks theorem declarations and their axioms, and checks definitional
  equality when a statement is supplied. Review the declared scope against
  the theorem itself.
- List finite-model obligations in `[[verification.temporal_checks]]` with
  `name`, `module`, `kind` (`safety` or `temporal`), and
  `instantiation`. Name the `property` for a temporal check.

```bash
h5i app prove --temporal examples/embedded/thermostat
h5i app temporal-check --enable examples/embedded/thermostat
h5i app model-check crates/h5i-app-veil/proofs --target H5iAppVeil.Embedded.FlashStore
```

Use `prove --temporal` to refresh the extraction and run both proof paths,
or `temporal-check --enable` to check configured obligations on the existing
extraction. Projects can opt in through `[verification] temporal = true`.
`model-check` builds the chosen Veil package and its checks. For CI, follow
`.github/workflows/veil.yaml` and inspect the `temporal` section of
`.h5i/app/runs/*-temporal-*/run.json` for current inputs, passing status, and
the result and declared scope of every required property. Run the gate in a
trusted Lake project. Untrusted Lean submissions require external isolation
and a harness that owns the specification, gate, and build configuration.

## Replay a counterexample

Use `h5i app trace-replay <trace.json> --json` with a Veil JSON trace to
exercise the real Rust `FlashStore` or `DeviceEngine`. Check the replay mode
and verdict. Synthetic setup or appended steps make a regression probe.
For DeviceEngine lasso replay, `ProjectedCycleReproduced` means closure on
`(snapshot, notify_pending, actuator_motor_on)`. A finite prefix can
demonstrate partial behavior, and a full runtime cycle needs evidence about
the remaining concrete state. See `crates/h5i-app-veil/README.md` for the
bridges and embedded models.
