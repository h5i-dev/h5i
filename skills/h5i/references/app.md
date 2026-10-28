# Build and verify an h5i-app

Read this for application development, Lean proofs, and their CI gate. The
framework is composable: use only the `h5i-app` features the application needs.
For the architecture and concrete code, inspect `crates/h5i-app/README.md`,
`examples/app/calculator/TUTORIAL.md`, and the nearest similar example. Check
current CLI flags with `h5i app <verb> --help`.

## Put decisions in the kernel

Start with `h5i app new ./ledger` for a kernel crate, a Lake proof project, and
`h5i-app.toml`. In an existing app, add the appropriate `h5i-app` dependency
and follow that layout. Keep the kernel's `transition` and state update pure:
take a principal or actor, a tenant-scoped snapshot, and a command; return the
write set and reply or a refusal. Express authorization and business rules
there, including the denied cases. Use the Rust subset Charon and Aeneas can
translate; run extraction early when introducing a new language feature.

The Axum/HTTP shell authenticates a request, constructs `Actor`, decodes the
command, and calls `H5iApp::respond` or `Engine::execute`. It must not infer
identity or privilege from client-set command fields. Keep database access
behind the engine and its `Store` contract; review any handler, background job,
or migration that could bypass the kernel. Run `h5i app lint` for its route and
command checks, then inspect its warnings and any `h5i-allow` exceptions. Lint
does not establish that authentication or authorization is correct.

## Translate and specify

```bash
h5i app doctor
h5i app lint
h5i app extract
h5i app check
```

`doctor` checks the pinned Charon, Aeneas, and Lean tools. `extract` translates
the Rust kernel through Charon's LLBC and Aeneas to `proofs/generated/`. List
the intended entry points and included crates in `h5i-app.toml`; do not edit
generated Lean to make a theorem pass. `h5i app prove` combines `extract` and
`check`. The manifest's `[check].theorems` names critical declarations that
must exist. Ensure the Lake build imports every handwritten module that should
be gated; `check` reports unbuilt modules, but a built proof of the wrong
property still passes.

For a nontrivial property, write a small, independent specification in
`proofs/Spec.lean`, then prove that extracted functions satisfy it in
`Proofs.lean` or relevant modules.
State who may read or write which rows, what a refusal changes, and which
invariants survive accepted transitions. For database claims, connect the
kernel result to `apply`, storage, and the modeled commit path. Name
preconditions such as successful execution, valid snapshots, capacity, or
tenant consistency in the theorem statement. Verify these conditions are
established by the request path, not supplied solely to make the proof easy.
The calculator tutorial shows `compute_spec`, `others_unchanged`, and a
concrete `set_then_get` witness.

## Check proof quality

`h5i app check` runs Lake and gates handwritten Lean against `sorry`,
`native_decide`, and nonstandard axiom dependencies; it rejects explicit
`axiom` declarations. Inspect any unbuilt-module notice. The gate does not
judge the specification or validate runtime assumptions. Before saying a
property is proven:

- Read the exact theorem and its hypotheses. Avoid a conclusion that merely
  repeats a premise, quantifies over no reachable cases, or guards all useful
  behavior behind an impossible condition. Add a concrete witness or
  representative test that satisfies important preconditions.
- Compare `Spec.lean` with the requested security or business rule. Ensure
  accepted and refused cases, identities, tenant boundaries, and state changes
  are actually constrained.
- Check `#print axioms <theorem>` or the gate record for unexpected
  dependencies. Do not add `sorry`, unchecked axioms, or stronger assumptions
  to silence a failed proof; fix the implementation, specification, or proof.
- Run `h5i app mutate` with manifest mutants, or `--auto` where useful. A
  mutant that survives shows behavior the current proofs leave open; strengthen
  the spec or explain why that behavior is outside the intended property.

State the result precisely: which theorem passed, on which extracted kernel,
under which hypotheses and trusted components. Consult `docs/design/design-app-trust.md`
for this repository's boundaries. Authentication inputs, clocks, parts of
HTTP/JSON integration, PostgreSQL semantics and engine behavior, deployment,
and the Charon/Aeneas/Lean toolchain need separate scrutiny. Optional GPU
execution does not inherit a kernel proof automatically.

## Gate changes in CI

For an application project, run Rust builds and tests, `h5i app lint`,
`h5i app extract --check`, and `h5i app check` against the project path.
`extract --check` verifies committed generated Lean matches the current Rust
without rewriting it; `check` builds and gates the proofs. Use `prove` locally
when changing kernel code or proofs, commit the generated output, then run the
read-only freshness check in CI. Add `mutate` to CI when its selected mutants
provide useful spec sensitivity coverage. The repository's
`.github/workflows/app.yaml` shows the pinned-tool, multi-project setup.
