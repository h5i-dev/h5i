# Experiment: token-reduction effectiveness

Run with:

```bash
./scripts/experiment_token_reduction.sh            # deterministic, no agent
./scripts/experiment_token_reduction.sh --with-agent   # + claude-in-the-loop check
```

The harness runs realistic (faked) tool commands through `h5i capture run` and,
for each, measures the four properties that define the feature's value against
the **default agent-facing output** (the structured YAML), tokenized with h5i's
own tokenizer:

1. **Token economics** — token cut vs the raw output.
2. **Info retained** — the signal an agent needs survives into the output.
3. **Structured** — the normalized `ToolResult` has the right `status`
   (never `ok`/`passed` on a nonzero exit) and a real parser.
4. **Lossless** — `h5i recall object <id>` returns the raw bytes exactly.

## Representative run (default `--format compact`)

```
fixture                         raw  summary    cut  status
──────────────────────────  ───────  ───────  ─────  ──────
pytest (1 fail/124)            1424       30    98%  failed
cargo test (1 fail)            1397       39    97%  failed
tsc (2 errors)                  544       63    88%  failed
go build failure                624       21    97%  failed
noisy log (buried err)        17223       79   100%  ok
big JSON (402 items)           5625       61    99%  ok
ruff (26 issues)                574      493    14%  failed   (diagnostic)
mypy (26 errors)                531      390    27%  failed   (diagnostic)
TOTAL                         27942     1176  95.8%

Checks passed: 32 / 32   ·   95.8% tokens saved
```

(Numbers vary slightly with the tokenizer; the shape is stable.)

The default output format is `compact` — one line per finding, e.g.
`F tests/t.py::test_pay  assert 0 == 100` — which is token-minimal (rtk-style)
while keeping the structured signal. Earlier the default was the full YAML
render, which *inflated* diagnostic-dense output (ruff −108%, mypy −101%) because
each finding cost ~6 keyed lines; `compact` turned those into net wins (+14%,
+27%) and tightened every other fixture too (e.g. pytest 91→98%, tsc 66→88%).
`--format structured` still emits the full YAML, `--format json` the canonical
JSON, `--format summary` the legacy text.

## Findings

- **Huge wins on noise-dominated output.** Tests, builds, and logs are mostly
  noise around a little signal: pytest **91%**, cargo **87%**, tsc **66%**,
  go build **82%**, a noisy service log **99%**, a 402-item JSON payload **98%**.
  This is the case you actually wrap, and the reduction is dramatic while every
  failure/error and its location survives.

- **Modest wins on signal-dense diagnostics.** For linters / type checkers with
  many issues (ruff, mypy), output is *all signal* — there's little noise to drop,
  so the ceiling is low. The default `compact` render (one line per finding) still
  beats raw (+14% / +27%) by capping the finding list and dropping summary noise,
  while keeping `status`, per-finding `rule`/`location`, dedupe `fingerprint`s, and
  `recall --status/--tool` queryability. (The full `--format structured` YAML is
  *larger* than raw here — ~6 keyed lines per finding — which is why `compact` is
  the default. `--format summary` is the absolute-smallest text option.)

- **Status is always honest.** Every failing run reports `status: failed`
  (never `ok`/`passed`) — derived from the exit code, never guessed from text.

- **Nothing is ever lost.** `recall object <id>` returns the raw bytes exactly
  for every fixture; the structured summary is a *view*, not the source of truth.

- **Safe to wrap anything.** With the default `--min-bytes`, tiny output
  (`echo hi`) passes straight through unstored — no object, no inflation — so
  wrapping commands liberally is free.

## Takeaway

Token reduction is a **large net win precisely where it matters** (the verbose
test/build/log output that floods an agent's context), and a **structure-for-tokens
trade** on already-compact diagnostic output. Combined with lossless recovery and
honest status, `h5i capture run` lets an agent keep noisy tool output out of its
window without losing the ability to see — or query — what actually happened.
