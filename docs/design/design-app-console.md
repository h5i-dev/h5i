# Apps in the console

Status: initial implementation, 2026-10-06. The sections below describe the
design; the implementation notes at the end identify the current boundaries.

The entry point is a user-visible guarantee, such as “bookings never overlap”,
not a count of Lean declarations. A reviewer should be able to answer:

1. What behavior does this application promise?
2. Which implementation, specification and theorem support that promise?
3. What must be true for it to apply, and what changed since the version I reviewed?
4. Which deliberately introduced bugs were detected, and which survived?

## Workspace

Add **Apps** to the existing rail. Discover projects through `h5i-app.toml`,
including framework projects. Keep the existing dark surfaces, typography,
resizable panes and hash navigation. Mutant failures and changed assumptions
use text and amber markers; filled red retains its existing refusal meaning.

The project list shows the name, purpose and concrete review reasons. Selecting
a project opens three views: **Guarantees**, **Changes**, **Mutations**. Source
inspection is a contextual pane, not a fourth place to rediscover the selection.

Guarantees opens on a readable list of behavior, conditions, exclusions and
available evidence. Selecting a guarantee opens its local relationship graph
and source inspector. An application flow is an alternative graph lens. Avoid
opening on a graph of every helper lemma in the repository.

Graph nodes have stable IDs and typed edges. A selected node exposes incoming
and outgoing relations, its description, source references and evidence. Rust
calls, data flow, extraction, proof dependencies and agent-described application
relationships are different edge kinds; an arrow must explain its meaning.
Support keyboard selection, search, fit, zoom, and a readable relationship list.

## Explanation and evidence

An agent-authored, versioned companion file describes guarantees, specification
clauses, theorem IDs, assumptions, exclusions, source anchors and graph views.
Agents choose the useful level of abstraction; a general Lean source parser
must not decide what the application promises.

Use structured nodes and edges as the canonical interactive graph. Mermaid is
a useful optional authored illustration, but is insufficient as the sole data
model: a node must link to a theorem, source, history and mutation evidence.

Descriptions and authored edges are explicitly attributed. Verification status
comes from tool records associated with specific inputs, never an agent's
`verified: true`. Missing descriptions, missing records and stale descriptions
remain visible. Projects without companion files still show manifest entries,
source files and recorded runs.

The CLI's existing Lean environment traversal in `check::gate_program` is the
natural source of theorem identity, elaborated type, axioms and declaration
dependencies. Extend this producer instead of parsing arbitrary Lean syntax in
the HTTP server. The catalog must cover built hand-written declarations as
well as manifest-required names; the manifest's theorem list is not exhaustive.

Separate these three kinds of condition:

- Logical hypotheses, including section variables and typeclass parameters.
- Definitions and dependencies that give those hypotheses and conclusions meaning.
- Conditions on the real system: authentication, clocks, database semantics and deployment.

The last category cannot be established merely by elaborating a theorem.
Descriptions carry source references and scope, and UI wording preserves that
distinction.

## Change review

Compare the working tree with HEAD, a chosen baseline, and historical versions.
Walk Git history for the same stable theorem/guarantee ID; do not limit the UI
to HEAD versus its parent. Renames need an explicit identity mapping; uncertain
matches should be presented for review, not silently equated.

Report additions, removals and changes to hypotheses, conclusions, referenced
definitions, application conditions and source anchors. Propagate changed
shared dependencies to the affected guarantees. Preserve before/after text,
commit identity and the relationship path explaining each warning.

For example, `booking_kernel.Theorems.no_double_booking` takes `Reachable s`.
A change to `Reachable` can change its meaning without changing the theorem's
signature. Watching only the theorem header or the companion file misses this.

“Changed” is an observation; “stronger assumption” or “weaker guarantee” needs
a semantic argument. An agent may propose such an interpretation, alongside
the source diff, but the UI must not present it as a mechanically established
ordering. Formatting-only changes should be distinguished where tool-derived
declaration data permits it. No historical catalog means unknown semantic
comparison, not unchanged assumptions.

Reading the UI never checks out revisions or executes old build scripts.
Historical tool records can supply exact semantic comparisons; absent those,
show source changes with their limits. State shallow-history and scan limits.

## Mutation evidence

Persist a run as it proceeds, including interrupted and failed runs. Each run
records its identity, timestamps, requested selection, baseline outcome,
toolchain, source revision and input digests. Each mutant records its actual
edit, origin (declared/generated), file location, stage results, duration and
diagnostics. Preserve bounded logs after temporary mutation directories vanish.

Display baseline → edit → Rust check → extraction → Lean build, with the
stopping stage visible. Distinguish survived, proof-stage failure, invalid,
infrastructure failure, skipped, cancelled and running. A nonzero `lake build`
alone does not identify a theorem that rejected the mutation. Only associate
a failed theorem when diagnostics provide that evidence; keep agent-authored
expected associations separately labeled.

Survival is a review candidate, not automatically a missing requirement: an
equivalent mutant or behavior intentionally outside the spec may survive.
Likewise, a killed-mutant percentage is not a proof-coverage percentage. Always
show the tested selection and invalid/unexecuted counts alongside any rate.

Records refer to their inputs. A past pass is not a current pass after the Rust,
proofs, extraction, manifest, local dependencies or relevant tool pins change.
No run is “not run”, not a green empty state. Concurrent runs need independent
records; a single mutable latest.json cannot be the history store.

## Booking as the first complete example

Use real declarations to exercise the interface:

- No double booking: `Theorems.no_double_booking`, `Spec.Reachable`,
  `Spec.Inv.compatible`, `Spec.Compatible`, `Spec.Apart`.
- Valid bookings are accepted: `Theorems.book_accepted`. The current declared
  touching-interval mutant exercises acceptance at the boundary; it should not
  be described automatically as a failure of the non-overlap theorem.
- Started bookings remain for runs by non-admins: `Clock.started_stays`, with
  monotonic time, and `Clock.clock_back_cancels` as the counterexample when
  that condition is removed.
- Persistence: `Storage.db_inv`, the generated storage bridge, and the trusted
  engine/PostgreSQL conditions documented in `docs/app/TRUST.md`.

Show preservation and acceptance together. A service that rejects every
request can satisfy a safety property while being useless to its users.

## Delivery order

1. Review the guarantee workspace using booking's actual source and a clear
   distinction between descriptions and measured evidence.
2. Define and validate the authored model and tool-produced records; add
   producers to `h5i app check` and `h5i app mutate`.
3. Connect the read-only Apps API, source inspector and evidence views.
4. Add historical comparison, changed-dependency propagation and graph lenses.

Port useful ideas from `improve-ui`, not its entire patch: discovery, the CLI
workflow and the example layout have changed on main. Test stale records,
changed indirect dependencies, missing history, invalid mutations, failed
baselines and incomplete runs, as well as the successful path.

## Implementation notes

The console now discovers `h5i-app.toml` projects and provides Guarantees,
Changes and Mutations. The booking project includes a source-fingerprinted
explanation of four guarantees, their conditions and the clock counterexample.
Other projects remain usable through their manifest theorem names and source
inventory, without inferring an explanation from Lean syntax.

`h5i app check` records its build/gate stages and a catalog from Lean's
elaborated environment: types, definition bodies, direct declaration dependencies
and axioms. `h5i app mutate` records its requested mutants, edits, baseline and
each execution stage. Records are local, under each project's
`.h5i/app/runs/<id>/run.json`; concurrent runs have separate IDs. The console
reads the newest 20 records. Existing terminal output is not imported.

Changes compares a working tree with a selected Git revision and scans up to
120 first-parent commits. Source changes are deliberately conservative,
file-level warnings, with 50-line excerpts around the first difference. Shared
authored dependencies propagate to guarantees. Historical explanations supply
stable identities when present; otherwise the current source map is used.
Missing/ambiguous anchors and mismatched authored source digests are reported.
Declaration comparison additionally needs a passing record on the clean
baseline commit and a passing record matching the current repository inputs.
It compares printed types/definitions and propagates recorded dependencies; it
does not prove logical strengthening, weakening, or equivalence.

Input freshness covers repository Rust, Lean, TOML, lockfiles, JSON and Lean
toolchain files, excluding build output, the authored UI description and web
assets. This is conservative and does not certify external path packages,
mutable installed tools, or deployed conditions. A matching check record says
that the recorded Lean build/gate passed, not that Rust extraction was freshly
checked. Logs retain their final 16 KiB. A force-killed process may leave a
running record; the UI labels it running/unfinished instead of asserting that
the process is live. No recorded proof-stage failure is automatically assigned
to a named theorem.

The authored format and workflow are documented in
[`app-console.md`](../app/app-console.md). Arbitrary Mermaid rendering, automatic
symbol-rename matching, historical rebuilds and deployment attestation are not
part of this implementation.
