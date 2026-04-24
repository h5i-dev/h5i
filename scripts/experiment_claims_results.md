# `h5i claims` — token-reduction experiment

Does pre-recording content-addressed claims actually reduce token usage on the next session, or is the saving purely theoretical? This is a small A/B test that tries to answer that.

---

## Hypothesis

When claims about the codebase are injected into `h5i context prompt` as pre-verified facts, the agent should skip re-exploring the repo and therefore consume fewer input tokens, fewer cache-read tokens, and fewer file-read tool calls than an identical run with no claims recorded.

## Method

Two arms run on **identical seeded codebases**, receiving the **identical user task**:

| Arm | Setup |
|---|---|
| **CONTROL** | `h5i context init` only. No claims recorded. `h5i context prompt` has no `## Known facts` section. |
| **TREATMENT** | `h5i context init` + **5 pre-recorded claims** describing where the HTTP helpers live and which files to leave alone. `h5i context prompt` emits a `## Known facts` preamble. |

Both arms prepend the output of `h5i context prompt` to the user task, then pipe it into `claude --print`. The only variable is whether claims were recorded beforehand.

For each run, the script parses the session JSONL under `~/.claude/projects/<encoded-workdir>/*.jsonl`, sums per-turn `usage.{input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens}`, and counts `Read`/`Grep`/`Glob`/`Edit`/`Bash` tool calls. A fidelity check (`git diff --name-only`) verifies the agent edited the expected file.

**Task (identical for both arms):**

> Add `log.info("ENTER <func_name>")` and `log.info("EXIT <func_name>")` to every function that makes an HTTP request. Don't modify any function that doesn't make HTTP calls.

**Seeded codebase (4 Python files, 6 functions total):**

```
src/api/client.py       # 3 HTTP helpers: fetch_user, create_post, delete_post
src/utils/format.py     # 2 pure helpers (no I/O)
src/utils/validate.py   # 2 pure helpers (no I/O)
main.py                 # wiring; no HTTP itself
```

**Claims recorded in TREATMENT** (5 total):

1. HTTP helpers live only in `src/api/client.py`. The three functions are `fetch_user`, `create_post`, `delete_post`.
2. `src/utils/format.py` contains formatting helpers; neither makes HTTP calls.
3. `src/utils/validate.py` contains validation helpers; neither makes HTTP calls.
4. `main.py` wires helpers together and does not itself make HTTP calls.
5. The logger is exposed as `log` at the top of `src/api/client.py`.

---

## Results

**Run:** `N_TRIALS=2` (4 total Claude sessions), ~3 minutes wall-clock.

| Metric                  | CONTROL avg | TREATMENT avg |         Δ |      Δ% |
|-------------------------|------------:|--------------:|----------:|--------:|
| **Cache-read tokens**   |     577,334 |       108,060 |  −469,274 |  **−81.3%** |
| Output tokens           |       5,762 |         1,768 |    −3,994 |  −69.3% |
| Read tool calls         |         6.5 |           1.0 |      −5.5 |  −84.6% |
| Bash tool calls         |           8 |             0 |        −8 | −100.0% |
| Assistant turns         |          19 |           4.5 |     −14.5 |  −76.3% |
| Wall time (sec)         |        69.5 |          20.5 |       −49 |  −70.5% |
| Cache-write tokens      |      36,426 |        29,572 |    −6,854 |  −18.8% |
| Input tokens (uncached) |          29 |          14.5 |     −14.5 |  −50.0% |

**Fidelity** (did the agent actually complete the task?):

| Arm       | `client.py` edited | `utils/*.py` wrongly edited |
|-----------|-------------------:|----------------------------:|
| CONTROL   |               1/2 |                         0/2 |
| TREATMENT |               2/2 |                         0/2 |

CONTROL trial 2 ran for 72 s but never committed the edit — a fidelity failure that makes the CONTROL cost numbers **understated** relative to what a fully-completing run would cost.

### Per-trial raw data

Both TREATMENT trials are nearly identical to each other, which makes the small sample more convincing than usual:

| Trial | Arm       | Reads | Bash | Turns | Cache-read | Wall |
|-------|-----------|------:|-----:|------:|-----------:|-----:|
| 1     | CONTROL   |     7 |    9 |    21 |    640,784 |  67s |
| 1     | TREATMENT |     1 |    0 |     5 |    123,106 |  20s |
| 2     | CONTROL   |     6 |    7 |    17 |    513,883 |  72s |
| 2     | TREATMENT |     1 |    0 |     4 |     93,014 |  21s |

### What actually happened in each arm

- **CONTROL:** the agent grepped directories via Bash, read `main.py`, read `client.py`, read both `utils/*.py` files, then edited. ~17–21 turns of exploration before committing.
- **TREATMENT:** the agent went straight to `src/api/client.py` because the claims told it exactly where HTTP helpers live and which files not to touch. 1 read, 1 edit, done in 4–5 turns.

---

## Honest caveats

1. **"Input tokens" (29 → 14.5) is misleading as a headline.** Under prompt caching, almost all input is billed through `cache_read_input_tokens` (~10% of the uncached rate). The 81% drop in cache-read tokens is what actually shows up on the Anthropic invoice.
2. **CONTROL trial 2 didn't finish the edit.** That's a fidelity failure, not a cost — so CONTROL's already-higher cost is an undercount.
3. **N=2 cannot distinguish "80% reduction" from "60% reduction".** The directional story is solid, but tighter estimates need `N_TRIALS=5` or more.
4. **Experimental bias toward the mechanism working:** the claims were designed to cover exactly the facts the task needed. A task where claims don't overlap with what's needed wouldn't show this delta. The honest claim is *"when claims cover the grounding the agent would otherwise do, you save ~5× on reads and ~80% on cache-read tokens,"* not *"claims always save tokens."*
5. **One session shape only.** The agent had to establish repo structure from a cold start. If the prompt-cache was already warm from a prior session, the delta would be smaller.

---

## Reproduce

```bash
# 1. Install a build of h5i that includes the `claims` subcommand (added in 8d4eb3e).
cargo install --path . --force

# 2. Confirm tooling.
which h5i claude
h5i claims --help

# 3. Run the experiment. N_TRIALS controls trials per arm (CONTROL + TREATMENT run for each).
N_TRIALS=2 ./scripts/experiment_claims.sh

# Or run more trials for tighter numbers:
N_TRIALS=5 ./scripts/experiment_claims.sh

# Override the temp-workdir prefix if you want to inspect them outside /tmp:
WORKDIR_BASE=$PWD/h5i-claims-exp N_TRIALS=3 ./scripts/experiment_claims.sh
```

Each run writes raw per-trial JSON records to `${WORKDIR_BASE}-results.jsonl.filtered`, and preserves the workdirs under `${WORKDIR_BASE}-{CONTROL,TREATMENT}-<trial>/` so you can re-inspect:

```bash
# See what claims were recorded in a trial.
cat ${WORKDIR_BASE}-TREATMENT-1/.git/.h5i/claims/*.json

# See what the agent actually saw.
(cd ${WORKDIR_BASE}-TREATMENT-1 && h5i context prompt)

# See what the agent edited.
git -C ${WORKDIR_BASE}-TREATMENT-1 diff
```

### Requirements

- `h5i` CLI with `claims` subcommand (commit `8d4eb3e` or later)
- `claude` CLI in `PATH`
- `git`, `python3`, `bash`

---

## Verdict

The mechanism works, and the magnitude is substantial: **~5× fewer file reads and ~80% drop in cache-read tokens** on a task where the claims cover the grounding the agent would otherwise do. Both trials tell the same story, which makes the directional conclusion robust even at N=2. The remaining question is how performance degrades as claim coverage drops below 100% — worth a follow-up experiment with partially-covering claim sets.
