# `h5i claims` — token-reduction experiment

Does pre-recording content-addressed claims actually reduce token usage on the next session, or is the saving purely theoretical? This is a controlled A/B test that tries to answer that.

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

**Rigor built into the v2 script** (`./scripts/experiment_claims.sh`):

- **Per-trial timeout** (`TRIAL_TIMEOUT=180s`) — wraps `claude --print` so a stalled trial doesn't hang the experiment.
- **Retry-and-cap** (`RETRY_CAP=1`) — a trial that times out, touches the wrong files, or fails the ENTER/EXIT log-pair correctness check is retried in a fresh workdir. Failures and retry counts are recorded, not hidden.
- **Interleaved arm order** per trial (odd: CONTROL→TREATMENT, even: TREATMENT→CONTROL) to mitigate serial drift from Anthropic-side caches or backend state.
- **Strict correctness check** — counts the HTTP helpers (0–3) that have **both** an `ENTER <fname>` and `EXIT <fname>` `log.info` line in the added-lines side of the diff. Only 3/3 counts as a successful trial.
- **Model ID logged per trial** — so a mid-experiment backend rollover would be visible, not hidden in the variance.
- **Aggregator reports mean ± stdev [min..max]** per arm, and flags any metric where `2·stdev ≥ |Δ|` as noise-dominated.

For each run the script parses the session JSONL under `~/.claude/projects/<encoded-workdir>/*.jsonl` and sums per-turn `usage.{input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens}` and counts `Read`/`Grep`/`Glob`/`Edit`/`Bash` tool calls.

**Task (identical for both arms):**

> Add `log.info("ENTER <func_name>")` and `log.info("EXIT <func_name>")` to every function that makes an HTTP request. Don't modify any function that doesn't make HTTP calls.

**Seeded codebase (4 Python files, 7 functions total):**

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

## Results — N=5 trials per arm

**Run health:** both arms on `claude-opus-4-7`, 4/5 trials successful per arm (one trial per arm failed both attempts — see *Failure mode* below). Total claude invocations: 13 (5 primary + 2 retries for CONTROL, 5 primary + 1 retry for TREATMENT).

**Successful trials only (4 per arm):**

| Metric                  | CONTROL (mean ± sd)      | TREATMENT (mean ± sd)    |      Δ% | Noise? |
|-------------------------|--------------------------|--------------------------|--------:|:------:|
| **Cache-read tokens**   | **611,624 ± 72,547**     | **158,393 ± 70,668**     | **−74.1%** |   ✓   |
| Output tokens           |      6,216 ± 1,535       |      2,432 ± 698         |  −60.9% |   ✓   |
| Read tool calls         |        6.0 ± 0.8         |        1.0 ± 0           |  −83.3% |   ✓   |
| Bash tool calls         |        8.8 ± 1.3         |        1.2 ± 2.5         |  −85.7% |   ✓   |
| Assistant turns         |         20 ± 2.2         |        6.2 ± 2.5         |  −68.8% |   ✓   |
| Wall time (sec)         |       69.2 ± 13.2        |         23 ± 8.3         |  −66.8% |   ✓   |
| Cache-write tokens      |     37,269 ± 1,631       |     34,041 ± 8,438       |   −8.7% | **⚠ noise** |
| Input tokens (uncached) |         30 ± 2.2         |       17.5 ± 5           |  −41.7% |   ✓   |

"Noise" flag fires when `2·max(sd_control, sd_treatment) ≥ |Δ|`. Only **cache-write tokens** fail that test — the 9% apparent drop is well within the stdev. Every other metric clears the threshold by a comfortable margin.

**Fidelity across all 5 trials per arm (including failed retries):**

| Arm       | All-3-log-pairs | Wrong files edited | Timed out |
|-----------|----------------:|-------------------:|----------:|
| CONTROL   |             4/5 |                0/5 |       0/5 |
| TREATMENT |             4/5 |                0/5 |       0/5 |

Both arms had one trial that failed both the primary attempt and the retry. This is expected LLM stochasticity — the task is not trivially deterministic. The failure rate is **symmetric**, so the headline numbers aren't biased by differential fidelity between arms.

### Per-trial raw data

| Trial | Arm        | Attempts | Success | Cache-read | Reads | Turns | Wall |
|------:|------------|---------:|:-------:|-----------:|------:|------:|-----:|
|     1 | CONTROL    |        1 |    ✓    |    511,918 |     5 |    17 |  61s |
|     1 | TREATMENT  |        1 |    ✓    |    264,395 |     1 |    10 |  35s |
|     2 | CONTROL    |        2 |    ✖    |    723,622 |     5 |    24 |  66s |
|     2 | TREATMENT  |        1 |    ✓    |    123,054 |     1 |     5 |  22s |
|     3 | CONTROL    |        1 |    ✓    |    609,131 |     6 |    20 |  85s |
|     3 | TREATMENT  |        2 |    ✖    |    344,805 |     1 |    12 |  41s |
|     4 | CONTROL    |        2 |    ✓    |    680,321 |     7 |    22 |  75s |
|     4 | TREATMENT  |        1 |    ✓    |    123,058 |     1 |     5 |  18s |
|     5 | CONTROL    |        1 |    ✓    |    645,126 |     6 |    21 |  56s |
|     5 | TREATMENT  |        1 |    ✓    |    123,066 |     1 |     5 |  17s |

### What the rigor caught that N=2 didn't

A previous N=2 run reported **81%** cache-read reduction. The N=5 run finds **74%**. The ~7 pp shift is within the rigorous version's stdev band, so it isn't a contradiction — it's the same effect observed with better precision.

Three things the N=2 pass hid:

1. **TREATMENT variance is higher than it appeared.** Three of the four successful TREATMENT trials clustered tightly around 123k cache-read tokens (123,054 / 123,058 / 123,066 — nearly identical). Trial 1, however, used **264,395** — roughly 2× the typical. Without N≥5, this outlier would dominate or be hidden entirely depending on draw.
2. **Fidelity failures are bilateral.** The earlier "CONTROL fails more" story was a 1/2 sample artifact. At N=5 both arms had exactly one failure.
3. **Cache-write tokens aren't actually different.** The N=2 run suggested a 19% drop. With proper variance, that's noise.

### What the most defensible single-number summary is

The table's cleanest number is **Read tool calls: 6.0 ± 0.8 → 1.0 ± 0**. Zero stdev in TREATMENT (all 4 successful runs read exactly one file — `src/api/client.py`, which the claims point at), and the CONTROL distribution is tight. **Interpretation: the claim-informed agent does ~6× less file-reading work per session.** That's a cleaner story than any token-percentage, because it doesn't require the reader to understand prompt caching to appreciate.

---

## Honest caveats

1. **N=4 successful trials per arm is small.** Stdev estimates are themselves noisy at N=4. `N_TRIALS=10` would give more trustworthy percentiles — worth running before making a firmer claim than "~70%".
2. **Experimental bias toward the mechanism working.** The claims were designed to cover exactly the facts the task needs. A task where claims only partially overlap with what's needed wouldn't show this delta. The honest claim is: *when claims cover the grounding the agent would otherwise do, the agent skips that work and spends ~70–80% fewer cache-read tokens.* Not *"claims always save tokens."*
3. **One session shape only.** Cold start establishing repo structure. If the prompt-cache was already warm from a prior session (unlikely in the current Claude Code product, but possible), the delta would be smaller.
4. **Cache-read tokens bill at ~10% of uncached input** under Anthropic's current pricing. The headline 74% drop on a cached-dominant input workload translates to a real but not 74%-of-total-invoice saving — estimate somewhere in the 50-70% range of the true $ cost of the input side, assuming cache hit rates hold.

---

## Reproduce

```bash
# 1. Install a build of h5i that includes the `claims` subcommand (added in 8d4eb3e).
cargo install --path . --force

# 2. Confirm tooling.
which h5i claude timeout
h5i claims --help

# 3. Run the experiment. Default is N=5 (~10-15 min wall-clock).
./scripts/experiment_claims.sh

# Pitch-grade numbers (N=10, ~25-30 min):
N_TRIALS=10 ./scripts/experiment_claims.sh

# Faster iteration during script development:
N_TRIALS=2 TRIAL_TIMEOUT=60 RETRY_CAP=0 ./scripts/experiment_claims.sh

# Override the temp-workdir prefix if you want to inspect them outside /tmp:
WORKDIR_BASE=$PWD/h5i-claims-exp N_TRIALS=5 ./scripts/experiment_claims.sh
```

Each run writes raw per-trial JSON records to `${WORKDIR_BASE}-results.jsonl.filtered`, and preserves the workdirs under `${WORKDIR_BASE}-{CONTROL,TREATMENT}-<trial>/` (plus `-retry2`, `-retry3` for retried attempts) so you can re-inspect:

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
- `timeout(1)` from GNU coreutils
- `git`, `python3`, `bash`

---

## Verdict

The mechanism works, and the magnitude is substantial at N=5 with proper variance accounting: **~74% fewer cache-read tokens** and **6× fewer file reads** on a task where the claims cover the grounding the agent would otherwise do. The effect is robust — every primary metric except cache-write passes the `2·stdev` threshold. The previous N=2 "81%" number was directionally right but imprecise; N=5 tightens it to a more defensible 74% (±10 pp margin at this sample size).

Next experiments worth running: (a) repeat at N=10 for tighter percentiles; (b) counter-experiment with *irrelevant* claims (does the preamble overhead cost tokens when claims don't apply?); (c) partial-coverage grid (at 1/5, 3/5, 5/5 relevant claims, where's the break-even point?).
