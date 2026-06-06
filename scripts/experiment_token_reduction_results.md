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

## Representative run

```
fixture                         raw  summary    cut  status
──────────────────────────  ───────  ───────  ─────  ──────
pytest (1 fail/124)            1424      132    91%  failed
cargo test (1 fail)            1397      179    87%  failed
tsc (2 errors)                  544      184    66%  failed
go build failure                624      113    82%  failed
noisy log (buried err)        17223      151    99%  ok
big JSON (402 items)           5625      125    98%  ok
ruff (26 issues)                574     1192  -108%  failed   (diagnostic)
mypy (26 errors)                531     1068  -101%  failed   (diagnostic)
TOTAL                         27942     3144  88.7%

Checks passed: 32 / 32   ·   88.7% tokens saved
```

(Numbers vary slightly with the tokenizer; the shape is stable.)

## Findings

- **Huge wins on noise-dominated output.** Tests, builds, and logs are mostly
  noise around a little signal: pytest **91%**, cargo **87%**, tsc **66%**,
  go build **82%**, a noisy service log **99%**, a 402-item JSON payload **98%**.
  This is the case you actually wrap, and the reduction is dramatic while every
  failure/error and its location survives.

- **An honest tradeoff on signal-dense diagnostics.** For linters / type checkers
  with many issues (ruff, mypy), the structured `ToolResult` is *larger* than the
  raw lines — each finding carries `rule`/`location`/`fingerprint`/severity. Here
  structured output trades tokens for **machine-actionable structure** (status,
  per-finding fields, dedupe fingerprints, `recall --status/--tool` queries),
  not fewer tokens. If raw token count is the only goal for such tools,
  `--format summary` (the legacy text filter) is the smaller option.

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
