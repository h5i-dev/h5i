# Reviewing an app in `h5i ui`

Start `h5i ui` in the repository and open **Apps**. Projects are discovered by
their `h5i-app.toml`. The console only reads files and Git objects; opening a
project never runs its build scripts or checks out an old revision.

- **Guarantees** connects behavior to specifications, theorems, conditions and
  code. Select a guarantee, then a graph node or connection to inspect it.
  Nodes support keyboard focus and Enter/Space; Fit and zoom control the graph.
  The application-flow lens shows authored call/data relationships separately.
- **Changes** compares the working tree with HEAD, a branch or a commit and
  shows earlier changes along the first-parent history. Warnings indicate
  changes requiring review, not automatically a weaker or stronger guarantee.
- **Mutations** shows a recorded selection of experiments, their actual edits,
  baseline, processing stages, diagnostics and input identity. Surviving mutants
  need review; equivalent changes and intentionally unspecified behavior can
  survive too. A failing Lean build alone does not identify a rejecting theorem.

## Produce evidence

```sh
h5i app check examples/app/booking
h5i app mutate examples/app/booking
h5i app mutate examples/app/booking --auto --limit 20
```

Check and mutation runs automatically write versioned records beneath the
project's `.h5i/app/runs/`. In the h5i repository, `cargo app check` and
`cargo app mutate` use the same producers. The console refreshes a selected
project every 30 seconds; Refresh also reloads the project list. Tool absence,
failed baselines and missing records are not passes.

Each check catalogs declarations from built hand-written Lean modules and the
manifest's required names. A check does not itself establish extraction
freshness: use `h5i app extract <project> --check` for that separate check.
Recorded input matching is repository-wide and conservative; it does not
attest external dependencies, current installed tool binaries, or deployment.

## Author an explanation

Place `h5i-app.ui.json` next to the manifest. Start from
`examples/app/booking/h5i-app.ui.json`, or this minimal example (adjust the
repository-relative source path):

```json
{
  "version": 1,
  "title": "My application",
  "description": "A short description of the behavior under review.",
  "author": "Agent or reviewer attribution",
  "exclusions": ["Authentication of external callers is a shell condition."],
  "nodes": [
    {
      "id": "balance-nonnegative",
      "kind": "guarantee",
      "title": "Balances stay nonnegative",
      "description": "In every state reachable from the initial state."
    },
    {
      "id": "balance-theorem",
      "kind": "theorem",
      "title": "Reachable balance invariant",
      "description": "State the hypotheses and conclusion in readable terms.",
      "symbol": "MyApp.balance_nonnegative",
      "sources": [{"path": "proofs/Theorems.lean", "anchor": "theorem balance_nonnegative"}]
    }
  ],
  "edges": [{"from": "balance-nonnegative", "to": "balance-theorem", "kind": "proved by", "view": "proof"}]
}
```

Keep IDs stable when titles or symbols change. Every node needs `id`, `kind`,
`title` and `description`. Optional fields are `symbol`, `sources` and
`excludes`. Kinds are `guarantee`, `theorem`, `specification`, `assumption`,
`implementation`, `counterexample` and `boundary`. Model limits are 200 nodes,
600 edges and 12 source references per node. Unknown fields and dangling edges
are rejected, including invented verification-status fields.

A source has a repository-relative `path`, optional unique line-text `anchor`,
and optional `digest`: SHA-256 of the complete source file at the version you
reviewed. The console warns when an anchor no longer resolves uniquely or the
fingerprint no longer matches. It shows up to 160 source lines around the
anchor; it never follows a source outside the repository. Update a digest only
after reviewing the corresponding explanation against the changed source.

Edges have `from`, `to`, `kind` (a readable relationship) and `view` (`proof`,
the default, or `flow`). In proof views, point from the guarantee toward what
it depends on; this direction also drives changed-dependency propagation.
Authored relationships do not establish proof status. Actual declaration types,
dependencies and axioms come from the check record.

Represent logical hypotheses, referenced definitions and real-world conditions
separately. Include acceptance guarantees alongside safety guarantees, and
counterexamples where removing a condition changes the result. For example,
booking shows both non-overlap and acceptance of touching intervals, plus the
clock-reversal counterexample to preserving a started booking.

## Reading incomplete evidence

The console shows at most 20 recent check/mutation records and 120 first-parent
commits. Shallow history and scan limits are displayed. Source comparison uses
whole referenced files; a change elsewhere in the file can raise a review item.
Without historical explanations the current mapping is used. Renamed symbols
are not automatically equated. Historical build scripts are never executed.

Elaborated declaration comparison needs a clean-baseline check record and one
matching current repository inputs. Without those, semantic comparison remains
unknown even if no watched source changed. Definition/type diffs are textual;
logical equivalence and implication are not inferred.

Ordinary error returns preserve completed stages. Hard termination can leave a
run marked running/unfinished, with no completion timestamp. Logs are bounded
tails, and missing, corrupt, unsupported or oversized records are reported.
Records are local files produced by tools, not cryptographically attested
results. Keep the source revision and input identity with any report you share.
