# Parallel decision toy

Small ASCII bookmark-search and push-rule models, designed to expose per-row
compute independently of a real application's storage and API. The kernel is
under 150 lines and allocates nothing during a decision. See [GPU experiment](../../../docs/app/GPU.md)
for semantics, proofs, fixed-capacity layouts, CPU measurements and the device
compiler failure. This is not a linkding or Matrix implementation.

From the repository root, `cargo app extract examples/app/parallel` regenerates
the Aeneas and schema fixtures; `cargo app check examples/app/parallel` builds
the proofs and checks their axiom dependencies. Use the workspace's isolated
cache/CPU constraints. Device wrappers live in the separately built
`crates/h5i-app-gpu` workspace, behind `gpu`; Charon never sees them.
