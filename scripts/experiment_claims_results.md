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

## Results — N=10 trials per arm

**Run health:** both arms on `claude-opus-4-7`. CONTROL: 9/10 successful (one trial failed both attempts). TREATMENT: **10/10 successful — all primary attempts, no retries needed.** Total claude invocations: 22 (10 primary + 2 retries for CONTROL, 10 primary + 0 retries for TREATMENT).

**Successful trials only (9 CONTROL, 10 TREATMENT):**

| Metric                  | CONTROL (mean ± sd)      | TREATMENT (mean ± sd)    |      Δ% | Noise? |
|-------------------------|--------------------------|--------------------------|--------:|:------:|
| **Cache-read tokens**   | **510,284 ± 61,284**     | **117,433 ± 38,032**     | **−77.0%** |   ✓   |
| Output tokens           |      4,781 ± 1,363       |      1,702 ± 418         |  −64.4% |   ✓   |
| **Read tool calls**     |        **5.6 ± 1.0**     |        **1.0 ± 0**       |  −82.0% |   ✓   |
| Bash tool calls         |        7.0 ± 1.4         |        0.3 ± 0.9         |  −95.7% |   ✓   |
| Assistant turns         |       17.1 ± 1.8         |        4.8 ± 1.2         |  −71.9% |   ✓   |
| Wall time (sec)         |       51.9 ± 8.9         |       18.4 ± 4.6         |  −64.5% |   ✓   |
| Input tokens (uncached) |       27.1 ± 1.8         |       14.8 ± 1.2         |  −45.4% |   ✓   |
| Cache-write tokens      |     37,016 ± 4,660       |     29,584 ± 437         |  −20.1% | **⚠ noise** |
| Glob tool calls         |        0.2 ± 0.4         |        0 ± 0             | −100.0% | **⚠ noise** |

"Noise" flag fires when `2·max(sd_control, sd_treatment) ≥ |Δ|`. Only **cache-write tokens** and **Glob calls** fail that test (both on near-zero bases). Every primary metric clears the threshold by a comfortable margin — the cache-read delta is ~6× the max within-arm stdev.

**Fidelity across all 10 trials per arm (including failed retries):**

| Arm       | All-3-log-pairs | Wrong files edited | Timed out |
|-----------|----------------:|-------------------:|----------:|
| CONTROL   |            9/10 |               0/10 |      0/10 |
| TREATMENT |       **10/10** |               0/10 |      0/10 |

TREATMENT was **perfect on the first attempt every single time**. CONTROL had one trial that failed both its primary and retry — the agent got lost trying to find the right file, burning 749k cache-read tokens and 24 turns without landing a correct edit. This asymmetry (which the N=5 run missed at 4/5 vs 4/5) is part of the story: **claims don't just reduce tokens, they also reduce task-failure rate**.

### Per-trial raw data

| Trial | Arm        | Attempts | Success | Cache-read | Reads | Turns | Wall |
|------:|------------|---------:|:-------:|-----------:|------:|------:|-----:|
|     1 | CONTROL    |        1 |    ✓    |    568,705 |     7 |    19 |  62s |
|     1 | TREATMENT  |        1 |    ✓    |     93,060 |     1 |     4 |  22s |
|     2 | CONTROL    |        1 |    ✓    |    544,660 |     7 |    18 |  64s |
|     2 | TREATMENT  |        1 |    ✓    |     92,997 |     1 |     4 |  19s |
|     3 | CONTROL    |        2 |    ✖    |    749,504 |     6 |    24 |  70s |
|     3 | TREATMENT  |        1 |    ✓    |    216,983 |     1 |     8 |  30s |
|     4 | CONTROL    |        1 |    ✓    |    501,261 |     6 |    17 |  39s |
|     4 | TREATMENT  |        1 |    ✓    |     93,051 |     1 |     4 |  16s |
|     5 | CONTROL    |        2 |    ✓    |    538,174 |     6 |    18 |  46s |
|     5 | TREATMENT  |        1 |    ✓    |    123,046 |     1 |     5 |  16s |
|     6 | CONTROL    |        1 |    ✓    |    574,943 |     5 |    19 |  50s |
|     6 | TREATMENT  |        1 |    ✓    |    123,102 |     1 |     5 |  18s |
|     7 | CONTROL    |        1 |    ✓    |    449,874 |     5 |    15 |  49s |
|     7 | TREATMENT  |        1 |    ✓    |     92,989 |     1 |     4 |  14s |
|     8 | CONTROL    |        1 |    ✓    |    476,208 |     5 |    16 |  63s |
|     8 | TREATMENT  |        1 |    ✓    |     92,992 |     1 |     4 |  17s |
|     9 | CONTROL    |        1 |    ✓    |    547,631 |     5 |    18 |  48s |
|     9 | TREATMENT  |        1 |    ✓    |    123,059 |     1 |     5 |  17s |
|    10 | CONTROL    |        1 |    ✓    |    391,098 |     4 |    14 |  46s |
|    10 | TREATMENT  |        1 |    ✓    |    123,054 |     1 |     5 |  15s |

**TREATMENT cache-read distribution is bimodal + one outlier:**
- 6 trials at **~93k** (1, 2, 4, 7, 8, and very close)
- 3 trials at **~123k** (5, 6, 9, 10)
- 1 outlier at **217k** (trial 3 — agent took 8 turns instead of the typical 4-5)

**CONTROL cache-read distribution is tight around ~500k**, ranging 391k–575k across 9 successful trials. The failed CONTROL trial 3 blew 749k before giving up.

### Convergence of the headline across sample sizes

| N  | Cache-read Δ | Read-calls Δ | Turns Δ | Wall Δ | TREATMENT fidelity |
|---:|-------------:|-------------:|--------:|-------:|-------------------:|
|  2 |         −81% |         −85% |    −76% |   −71% |              2/2 ✓ |
|  5 |         −74% |         −83% |    −69% |   −67% |              4/5 ✓ |
| 10 |     **−77%** |     **−82%** | **−72%** | **−65%** |         **10/10 ✓** |

The N=5 and N=10 runs agree within ~3 pp across all metrics, so the effect has stabilized. The N=2 numbers were directionally right but imprecise — a reminder that 2 trials aren't enough to separate signal from LLM stochasticity.

### The cleanest single-number summary

**Read tool calls: 5.6 ± 1.0 → 1.0 ± 0.0**. All 10 successful TREATMENT trials read exactly one file — the one the claims point at. Zero within-arm variance on the treatment side, and the control distribution is tight. **Interpretation: claims make file-reading behaviour deterministic and ~5.6× less work per session.** That's a cleaner story than any token-percentage, because it doesn't require the reader to understand prompt caching to appreciate.

---

## Honest caveats

1. **Experimental bias toward the mechanism working.** The claims were designed to cover exactly the facts the task needs. A task where claims only partially overlap with what's needed wouldn't show this delta. The honest claim is: *when claims cover the grounding the agent would otherwise do, the agent skips that work and spends ~77% fewer cache-read tokens.* Not *"claims always save tokens."*
2. **One session shape only.** Cold start establishing repo structure. If the prompt-cache was already warm from a prior session, the delta would be smaller.
3. **Cache-read tokens bill at ~10% of uncached input** under Anthropic's current pricing. The headline 77% drop on a cached-dominant input workload translates to a real but not 77%-of-total-invoice saving — estimate somewhere in the 50–70% range of the true $ cost of the input side.
4. **TREATMENT's perfect 10/10 fidelity is partly lucky.** At N=10 one more trial could easily have failed without contradicting the underlying pattern. The robust claim is "at least as reliable as CONTROL, probably somewhat better." Not "never fails."
5. **Model: `claude-opus-4-7`.** Smaller/older models would likely show a smaller absolute delta because they explore less aggressively to begin with.

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

# Numbers matching this writeup (N=10, ~25-30 min):
N_TRIALS=10 ./scripts/experiment_claims.sh

# Faster iteration during script development:
N_TRIALS=2 TRIAL_TIMEOUT=60 RETRY_CAP=0 ./scripts/experiment_claims.sh

# Override the temp-workdir prefix if you want to inspect them outside /tmp:
WORKDIR_BASE=$PWD/h5i-claims-exp N_TRIALS=10 ./scripts/experiment_claims.sh
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

The mechanism works and the magnitude is substantial at N=10 with proper variance accounting: **~77% fewer cache-read tokens**, **~5.6× fewer file reads**, **~72% fewer assistant turns**, and **10/10 task fidelity vs 9/10**. The effect is robust — every primary metric clears the `2·stdev` threshold by a comfortable margin, and the N=5 → N=10 deltas agree within 3 percentage points on every metric. The "with claims" distribution is remarkably deterministic: 9 of 10 TREATMENT trials cluster in 93k–123k cache-read tokens, while the CONTROL runs spread from 391k–575k.

Next experiments worth running: (a) counter-experiment with *irrelevant* claims (does the preamble overhead cost tokens when claims don't apply?); (b) partial-coverage grid (at 1/5, 3/5, 5/5 relevant claims, where's the break-even point?); (c) this same protocol on a larger codebase to see whether the effect scales with repo size.
