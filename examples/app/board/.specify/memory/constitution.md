# Bulletin Board Constitution

## Core Principles

### I. Tutorial first

The board is tutorial 2 of `examples/app`. Each change keeps the tutorial
readable in order and the application runnable with one PostgreSQL container
and `cargo run`.

### II. Small kernel, readable proofs

Kernel functions use loops and plain matches that Charon and Aeneas translate
directly. A proof that needs a new Lean technique is explained in the tutorial.

## Verified Kernel Principles (h5i)

These principles apply to every feature built with h5i-app. A plan that breaks
one records the exception and its reason under Complexity Tracking.

### K1. Decisions live in the kernel

Authorization and business rules are decided by the pure Rust kernel:
`transition(actor, snapshot, command)` returns a write set and reply, or a
refusal. The HTTP shell authenticates, decodes and stores. It never decides
who may do what, and never trusts identity or privilege carried in a command
field.

### K2. Every invariant and authorization rule has an ID and a theorem

Each `INV-*` and `AUTH-*` requirement in a spec names a Lean theorem about the
extracted kernel. The theorem is listed in `[check].theorems` of
`h5i-app.toml`. A requirement without a theorem is marked out of scope, with a
reason, in the `exclusions` of `h5i-app.ui.json`.

### K3. The proof gate is not negotiable

Handwritten Lean contains no `sorry`, `native_decide` or `axiom`. Generated
Lean under `proofs/generated/` is never edited by hand. A failing proof is
fixed in the code, the specification or the proof, never by weakening the
statement until it is trivially true.

### K4. Every proof is tested with a mutant

Each `INV-*` and `AUTH-*` theorem has at least one `[[mutant]]` in
`h5i-app.toml` that removes or weakens the check it depends on. `h5i app
mutate` must report each one as caught. A surviving mutant means the
specification is too weak.

### K5. The trust boundary is written down

Behavior outside the theorems (authentication, clocks, JSON decoding, storage
engine, deployment, the Charon/Aeneas/Lean toolchain) is listed as an
assumption or boundary node in `h5i-app.ui.json`. Security rules enforced in
the shell are covered by an `h5i test` flow under `.h5i-tests/tests/` instead.

### K6. "Verified" has a precise meaning

A requirement is called verified only after its theorem passed `h5i app
prove` for the current kernel and its mutants were caught by `h5i app
mutate`. Reports name the theorem and its hypotheses.

## Governance

This constitution applies to `examples/app/board`. Amendments are made in the
same change as the code they affect.

**Version**: 1.0.0 | **Ratified**: 2026-11-15 | **Last Amended**: 2026-11-15
