# Reviewing an app in `h5i ui`

Start `h5i ui` in the repository and open Apps. Projects are discovered by their
`h5i-app.toml`. The console only reads files and Git objects: opening a project
never runs its build scripts or checks out an old revision.

The console starts from a user-visible guarantee ("bookings never overlap"),
not a list of Lean declarations. Each project has three views:

- Guarantees: a list of behavior, conditions, exclusions and evidence.
  Selecting a guarantee opens its local graph and a source inspector. Nodes take
  keyboard focus and Enter/Space; Fit and zoom control the graph. The
  application-flow lens shows authored call and data relationships separately.
- Changes: compares the working tree with HEAD, a branch or a commit, and shows
  earlier changes along first-parent history. A warning means "review this",
  not that a guarantee got weaker or stronger.
- Mutations: recorded experiments with their edits, baseline, stages,
  diagnostics and input identity.

## Produce evidence

```sh
h5i app check examples/app/booking
h5i app mutate examples/app/booking
h5i app mutate examples/app/booking --auto --limit 20
```

Each run writes a record to `.h5i/app/runs/<id>/run.json` in the project
(`cargo app check`/`cargo app mutate` in the h5i repository do the same). The
console reads the newest 20 and refreshes every 30 seconds. Missing tools,
failed baselines and missing records are never shown as passes.

`h5i app check` records its build and gate stages and a catalog from Lean's
elaborated environment: types, definition bodies, direct dependencies and
axioms, for built hand-written modules as well as manifest-required names. A
check does not establish extraction freshness; run
`h5i app extract <project> --check` for that.

`h5i app mutate` records each mutant's edit, origin (declared or generated)
and stages: baseline, edit, Rust check, extraction, Lean build. A failing
`lake build` alone does not identify the theorem that rejected a mutant, and
none is named without diagnostics saying so. A survivor needs review; an
equivalent change or unspecified behavior can survive too. A kill rate is not
proof coverage, so the tested selection and invalid/unexecuted counts are
shown beside it.

## Author an explanation

Place `h5i-app.ui.json` next to the manifest. The map is written by hand (by an
agent or reviewer), not generated from the code, and the console says so. Start
from `examples/app/booking/h5i-app.ui.json` or this minimal example:

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

Nodes:

- Required: `id`, `kind`, `title`, `description`. Optional: `symbol`,
  `sources`, `excludes`. Keep ids stable when titles or symbols change.
- Kinds: `guarantee`, `theorem`, `specification`, `assumption`,
  `implementation`, `counterexample`, `boundary`.
- Limits: 200 nodes, 600 edges, 12 sources per node. Unknown fields (including
  invented status fields such as `verified`) and dangling edges are rejected.
  Verification status comes only from tool records.

Sources have a repository-relative `path`, an optional `anchor` (line text that
must occur once) and an optional `digest` (SHA-256 of the whole file at the
version you reviewed). The console warns when an anchor no longer resolves
uniquely or a digest no longer matches, and counts such nodes above the graph.
It shows up to 160 lines around the anchor, labels missing or ambiguous
anchors instead of guessing a line, and never follows a path outside the
repository. Update a digest only after rereading the explanation against the
changed source.

Edges have `from`, `to`, `kind` (readable text) and `view` (`proof`, the
default, or `flow`). A proof edge means `from` depends on `to`: if `to`
changes, `from` needs review, and Changes uses exactly this to find affected
guarantees. Out-of-scope behavior goes in a node's `excludes` or in
`exclusions`, not in an edge. Point a boundary at the code that realizes it
through its `sources`.

Lean-to-Lean proof edges come from the latest check's catalog. Authored proof
edges between two Lean-symbol nodes (`theorem`, `specification`, `assumption`,
`counterexample`) are ignored, even with no catalog. Write guarantee-to-theorem,
Rust, flow and deployment edges, and attach counterexamples to a guarantee.
Authored nodes are matched to catalog declarations by exact `symbol`;
unmapped declarations get generated nodes, and constants outside the catalog
are labeled leaves with no invented signature.

Keep logical hypotheses, the definitions that give them meaning, and
real-world conditions (authentication, clocks, database, deployment) apart;
elaborating a theorem cannot establish the last kind. Show acceptance next to
safety, since a service that rejects everything is safe. Booking pairs
`Theorems.no_double_booking` with `Theorems.book_accepted` and
`Clock.started_stays` with the `Clock.clock_back_cancels` counterexample.

With no map and no recorded declarations, the console offers a prompt for
asking a coding agent to write one.

## Reading the graph

Solid cyan edges are tool-recorded dependencies; dashed gray edges are
authored. The proof lens draws arrows from a dependency to its dependent, the
reverse of the JSON direction. The graph shows the check's date, status and
input freshness. A stale or failed record stays labeled as such, and a newer check with no
catalog does not fall back to an older one. Neighborhoods show up to 80 nodes;
Focus dependencies moves further. Dependency edges merge references in types
and bodies, so they do not tell a hypothesis from a proof helper. Projects
without a map can still browse recorded theorems and sources.

## Limits of the evidence

- History: at most 120 first-parent commits; shallow history and scan limits
  are shown. Old build scripts are never run.
- Source comparison is per referenced file, with a 50-line excerpt around the
  first difference, so an unrelated edit in the same file raises a review item.
  Shared authored dependencies propagate to guarantees. Without historical maps
  the current one is used, and renamed symbols are not matched.
- Declaration comparison needs a passing record on the clean baseline commit
  and one matching the current inputs; otherwise it is unknown, even if no
  watched file changed. It compares printed types and definitions and
  propagates recorded dependencies. It does not infer equivalence,
  strengthening or weakening.
- Input freshness covers the repository's Rust, Lean, TOML, lockfiles, JSON and
  Lean toolchain files, excluding build output, the map and web assets. It does
  not cover external path packages, installed tool binaries or deployment. A
  matching check record says the Lean build and gate passed, not that
  extraction was rechecked.
- A killed process can leave a record marked running/unfinished. Logs keep the
  last 16 KiB. Corrupt, unsupported or oversized records are reported.
- Records are local files, not attested results. Keep the source revision and
  input identity with any report you share.

Not implemented: Mermaid rendering, automatic matching of renamed symbols,
historical rebuilds, deployment attestation.
