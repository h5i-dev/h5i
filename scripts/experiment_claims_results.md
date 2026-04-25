# `h5i` token-reduction experiment

Do `h5i claims` and `h5i summary` actually reduce the tokens an agent burns on a real task, or is the saving theoretical? This is a controlled A/B/C/D test that tries to answer that, with everything held identical except the per-arm pre-seeded artifact.

---

## TL;DR

At N=10 trials per arm, single model (`claude-opus-4-7`), 10/10 fidelity per arm:

| Arm | Cache-read tokens | vs CONTROL | Reads | Turns | Wall |
|---|--:|--:|--:|--:|--:|
| **CONTROL** (no claims, no summaries) | 528,136 ±102K | — | 5.2 | 16.5 | 45.5s |
| **TREATMENT** (5 hand-curated claims pre-seeded) | 165,722 ±105K | **−68.6%** ✓ | **1.0** | 6.1 | 20.2s |
| **AUTO_CLAIMS** (`H5I_CLAIMS_FREQUENCY=high`, agent records during run) | 804,070 ±128K | **+52.2%** ✓ | 7.1 | 24.5 | 55.5s |
| **SUMMARIES** (4 file summaries pre-cached, eagerly inlined) | 283,174 ±113K | **−46.4%** ✓ | **1.0** | 9.9 | 35.0s |

✓ = effect exceeds 2·max(stdev) — not noise.

**Headline reads:**

1. **Pre-curated claims work.** −68.6% cache-read, deterministic 1-Read sessions, 10/10 fidelity. Robust at N=10.
2. **Pre-cached summaries work too**, once their content is *inlined* in the prompt rather than fetched per-file. −46.4% cache-read, same 1-Read pattern as TREATMENT. (Lazy-fetch summaries had been a wash before this.)
3. **Letting the agent record claims mid-session is a net cost** for a single session: +52.2% cache-read overhead, with break-even at ~0.76 future sessions of TREATMENT-level savings. Real cost; arguably worth it, but don't default the knob to `high` blindly.

---

## Method

Four arms, identical seeded codebase, identical user task. The only between-arm differences are pre-seeded artifacts and the `H5I_CLAIMS_FREQUENCY` env var.

### The seeded codebase

A 4-file Python toy project:

```
src/api/client.py       # 3 HTTP helpers: fetch_user, create_post, delete_post
src/utils/format.py     # 2 pure helpers (no I/O)
src/utils/validate.py   # 2 pure helpers (no I/O)
main.py                 # wiring; no HTTP itself
```

### The task (identical for all four arms)

> Add `log.info("ENTER <func_name>")` and `log.info("EXIT <func_name>")` to every function that makes an HTTP request. Don't modify any function that doesn't make HTTP calls.

A successful trial requires **both** ENTER and EXIT log lines in the diff for **all 3** HTTP helpers, **and** zero edits to any non-HTTP file, **and** no timeout.

### The arms

| Arm | Pre-seeded artifacts | `H5I_CLAIMS_FREQUENCY` |
|---|---|---|
| **CONTROL** | none | `off` |
| **TREATMENT** | 5 hand-curated claims (caveman style, ~30 tokens each) | `off` |
| **AUTO_CLAIMS** | none — agent records during the session | **`high`** |
| **SUMMARIES** | 4 hand-curated file summaries (caveman style, ~80 tokens each) | `off` |

The TREATMENT seed claims:

1. `HTTP only src/api/client.py: fetch_user, create_post, delete_post.`
2. `src/utils/format.py: format_date, truncate. Pure, no HTTP.`
3. `src/utils/validate.py: validate_email, validate_id. Pure, no HTTP.`
4. `main.py wires helpers. No direct HTTP.`
5. `Logger \`log\` at top src/api/client.py via \`from logging import getLogger; log = getLogger(__name__)\`.`

The SUMMARIES seed has one summary per source file. Both seeds use **caveman-style** text — drop articles + copulas, keep paths/identifiers/types/numbers exact. See [Lessons from earlier iterations](#lessons-from-earlier-iterations) for why this matters.

### Pipeline per trial

1. Seed the workdir (`git init`, write the 4 source files, `git commit`).
2. `h5i init` and `h5i context init`.
3. Per-arm pre-seeding: TREATMENT records claims, SUMMARIES records summaries.
4. Build the prompt: `h5i context prompt` (with `H5I_CLAIMS_FREQUENCY=$freq` set so the prelude policy hint is correct) → append the task.
5. `claude --print --mcp-config <tmp>.json --strict-mcp-config --allowedTools "mcp__h5i__*,Read,Write,Edit,Bash,Grep,Glob"` consumes that prompt.
6. Parse the session JSONL under `~/.claude/projects/...` for token usage + tool call counts.
7. Score correctness from `git diff <seed-oid>` (catches both committed and uncommitted edits).

### Rigor

- **Per-trial timeout** (`TRIAL_TIMEOUT=240s` in this run) — `timeout --kill-after=10` ensures a stalled `claude` doesn't hang the experiment.
- **Retry-and-cap** (`RETRY_CAP=1`) — a trial that times out, edits the wrong files, or misses an ENTER/EXIT pair is retried in a fresh workdir; failures + retry counts are recorded, not hidden.
- **Cyclic 4-arm rotation per trial** — each arm appears in each within-trial position over N≥4 trials (Latin-square-ish), mitigating Anthropic-side cache warmup that would otherwise favour later arms.
- **MCP server mounted** via `--mcp-config` so MCP tools (`mcp__h5i__h5i_claims_*`, `mcp__h5i__h5i_summary_*`, etc.) are actually reachable; without this the AUTO_CLAIMS arm would be artificially Bash-only.
- **Correctness uses `git diff <seed-oid>`** not the working-tree diff, so committed edits count too (the agent under TREATMENT often runs `h5i commit` before returning).
- **Workdir → JSONL encoding** matches Claude's `_` → `-` substitution; a previous bug here silently lost AUTO_CLAIMS sessions.
- **Aggregator reports mean ± stdev [min..max] per arm** and flags any metric where `2·max(sd) ≥ |Δ|` as noise-dominated.

### What's measured

For each session JSONL we sum across all assistant turns:

- `usage.input_tokens` (uncached input)
- `usage.output_tokens`
- `usage.cache_read_input_tokens` (the cached prefix is read on every turn — totals scale with turn count)
- `usage.cache_creation_input_tokens` (new content written to cache this turn)

Tool calls counted per session: `Read`, `Grep`, `Glob`, `Edit`, `Write`, `Bash`, plus `h5i_claims_add`, `h5i_summary_get`, `h5i_summary_set` (Bash + MCP forms both detected).

---

## Headline results — N=10

**Run health:** 40/40 successful trials, 0 timeouts, 0 retries fired, single model (`claude-opus-4-7`) seen across every trial. Workdir base: `/tmp/h5i-claims-exp-136017-`.

### Comparison table

| metric | CONTROL | TREATMENT | AUTO_CLAIMS | SUMMARIES | TREAT Δ% | AUTO Δ% | SUM Δ% |
|---|--:|--:|--:|--:|--:|--:|--:|
| Cache-read tokens | 528,136 ±102K | 165,722 ±105K | 804,070 ±128K | 283,174 ±113K | **−68.6%** ✓ | **+52.2%** ✓ | **−46.4%** ✓ |
| Output tokens | 4,100 ±1,115 | 2,306 ±1,034 | 8,383 ±2,705 | 4,873 ±2,195 | −43.7% ⚠ | +104.5% ⚠ | +18.8% ⚠ |
| Cache-write tokens | 59,651 ±21K | 44,316 ±10K | 117,746 ±26K | 65,929 ±24K | −25.7% ⚠ | +97.4% | +10.5% ⚠ |
| Read tool calls | 5.2 ±1.1 | **1.0 ±0** | 7.1 ±1.4 | **1.0 ±0** | −80.8% | +36.5% ⚠ | −80.8% |
| Bash tool calls | 6.5 ±3.1 | 1.4 ±3.0 | 8.8 ±2.8 | 3.1 ±2.6 | −78.5% ⚠ | +35.4% ⚠ | −52.3% ⚠ |
| Claim `add` calls | 0 | 0 | **1.0 ±0** | 0 | — | — | — |
| Summary `get` calls | 0 | 0 | 0 | **0 ±0** | — | — | — |
| Assistant turns | 16.5 ±2.8 | 6.1 ±3.2 | 24.5 ±2.5 | 9.9 ±3.5 | −63.0% | +48.5% | −40.0% ⚠ |
| Wall time (sec) | 45.5 ±15.0 | 20.2 ±6.6 | 55.5 ±9.9 | 35.0 ±21.2 | −55.6% ⚠ | +22.0% ⚠ | −23.1% ⚠ |
| Fidelity (10 trials) | 10/10 | 10/10 | 10/10 | 10/10 | — | — | — |

### Per-arm verdicts

```
✔  TREATMENT (pre-curated claims):              −68.6% cache-read tokens vs CONTROL (retrieval savings from pre-seeded claims)
✔  AUTO_CLAIMS (freq=high, single-session):     +52.2% cache-read tokens vs CONTROL (recording overhead — future sessions must recover this much)
✔  SUMMARIES (pre-cached file summaries):       −46.4% cache-read tokens vs CONTROL (orientation savings from blob-keyed file summaries)

Break-even estimate: AUTO_CLAIMS pays back its 275,934-token overhead after ~0.76 future sessions at TREATMENT-level savings.
```

### Per-trial raw data

```
=== CONTROL ===
trial  cache-read  reads  turns  wall
    1     628,789      5     19   50s
    2     404,080      4     13   29s
    3     483,598      4     15   42s
    4     600,248      7     18   46s
    5     586,022      5     18   50s
    6     379,156      5     12   29s
    7     659,840      7     20   59s
    8     610,968      4     19   43s
    9     500,427      5     17   77s
   10     428,234      6     14   30s

=== TREATMENT ===
trial  cache-read  reads  turns  wall
    1     362,249      1     12   33s   ← outlier; agent ran longer
    2     100,153      1      4   15s
    3     100,086      1      4   18s
    4     361,290      1     12   31s   ← outlier
    5     100,084      1      4   17s
    6     167,195      1      6   18s
    7     100,010      1      4   13s
    8     133,571      1      5   19s
    9     116,313      1      5   17s
   10     116,264      1      5   21s

=== AUTO_CLAIMS ===
trial  cache-read  reads  turns  wall
    1     789,877      7     25   49s
    2     822,778      8     26   51s
    3     518,144      4     18   40s
    4   1,031,387      7     27   59s
    5     790,908      8     25   56s
    6     845,900      8     26   73s
    7     867,465      9     26   52s
    8     726,880      8     23   46s
    9     817,993      6     24   66s
   10     829,368      6     25   63s

=== SUMMARIES (eagerly inlined) ===
trial  cache-read  reads  sum_get  turns  wall
    1     288,719      1        0     10   28s
    2     100,479      1        0      4   17s
    3     356,467      1        0     13   37s
    4     253,769      1        0      9   28s
    5     116,595      1        0      5   17s
    6     327,555      1        0     11   90s
    7     253,769      1        0      9   23s
    8     291,086      1        0     10   28s
    9     479,530      1        0     16   40s
   10     363,767      1        0     12   42s
```

### What the data says

**TREATMENT.** Cache-read drops by 68.6% with the effect easily exceeding 2·stdev. Read count is **exactly 1 in every trial** — the file the agent edits, no exploration. Two trials (#1, #4) ran longer (12 turns instead of the usual 4–6); the cause isn't clear from the JSONL, but even those count toward TREATMENT's mean. The TREATMENT distribution is roughly bimodal: 5 tight trials at ~100K cache-read, 5 at 116K–360K.

**SUMMARIES.** Once the summary content is inlined into the prelude (rather than fetched per-file), Read drops to 1 — same as TREATMENT — and `summary_get` calls are 0 across all 10 trials. The agent correctly used the inlined content. Cache-read is 46.4% lower than CONTROL with the effect non-noisy. Slightly higher mean than TREATMENT because the SUMMARIES seed is ~170 tokens of inlined text vs TREATMENT's ~80 tokens, and SUMMARIES doesn't shortcut the h5i workflow as aggressively as TREATMENT does.

**AUTO_CLAIMS.** The recording overhead is real and statistically robust. Every trial recorded exactly 1 claim (the rule of thumb "most good claims cite 1 file" held). Turn counts are 18–27 across trials; the agent does the full h5i workflow plus the claim-recording roundtrip plus extra deliberation under the `freq=high` policy hint. The +52.2% overhead, divided by TREATMENT's ~363K savings, gives the 0.76-session break-even.

**CONTROL.** A wide spread (379K–660K) reflects how sensitive the agent is to small differences in exploration order. The 6.5 average Bash calls captures the h5i workflow steps (context init, traces, commit, notes analyze).

---

## Lessons from earlier iterations

The numbers above are from the **caveman-seeds + eager-summaries** version of the experiment. Two earlier interventions changed the picture meaningfully:

### 1. Caveman-style compression

Original TREATMENT claims were verbose (~50 tokens each). After rewriting them caveman-style (drop articles/copulas/fluff, keep paths and identifiers exact) the same arms became measurably more efficient:

| metric | pre-caveman | caveman | shift |
|---|--:|--:|--:|
| TREATMENT cache-read vs CONTROL | −54.6% ⚠ noise | **−66.8%** ✓ robust | larger savings |
| AUTO_CLAIMS cache-read vs CONTROL | **+66.8%** ✓ | +44.2% ⚠ | overhead **halved** |
| AUTO_CLAIMS break-even | 1.22 sessions | 0.69 sessions | < 1 session |
| Avg agent-recorded claim length | ~50 tokens | ~25 tokens | matches target |

The CLAUDE.md instruction added an "≈30 tokens, drop articles + copulas + fluff" rule with a before/after example. Agents obey when shown the pattern. The compounding mechanism: live claim text sits in every future session's cached prefix, re-read on every turn — every word costs forever, so brevity at write time pays back forever.

### 2. Eager summary rendering (the bigger win)

In the first version, `h5i context prompt` only listed *which* files had summaries; the agent had to call `h5i_summary_get(path)` once per file to read content. That's 4 round-trip turns × ~33K cached prefix per turn ≈ +130K cache-read tokens.

After switching to eager rendering — inlining summary content directly in the prelude when the count fits a budget (`EAGER_COUNT_CAP=10`, `EAGER_CHAR_CAP=2000`) — the agent makes 0 `summary_get` calls in 10/10 trials and uses the inlined content directly:

| metric | lazy fetch | eager render | shift |
|---|--:|--:|--:|
| SUMMARIES cache-read vs CONTROL | +21.0% ⚠ | **−46.4%** ✓ | sign flipped + noise resolved |
| SUMMARIES turns | 23.5 ±3.2 | **9.9 ±3.5** | −58% |
| SUMMARIES Read calls | 2.0 ±1.1 | **1.0 ±0** | matches TREATMENT |
| SUMMARIES `summary_get` calls | 4.1 | **0** | round trips eliminated |

Generalisable lesson: **for any cached "known" content, pre-load into the prompt rather than forcing per-item fetches**. Each fetch is a separate turn; each turn re-reads the entire cached prefix. The math heavily favours one bigger prefix over N round-trips.

For larger codebases (`> EAGER_COUNT_CAP` summaries or `> EAGER_CHAR_CAP` chars total), the renderer falls back to listing-only and the agent must lazy-fetch — which is the right tradeoff at scale, just not at the size we tested.

---

## Honest caveats

Some of these were raised in conversation; all of them apply to the headline numbers above.

1. **The task profile favours claims.** Single-file edit on a 4-file codebase rewards "which file matters?" (claims) more than "what's in this file?" (summaries). On a 50-file codebase where the agent must orient on many files but only edit a few, summaries could plausibly close or overtake the gap with claims. We did not test that.

2. **TREATMENT seed (~80 tokens total) and SUMMARIES seed (~170 tokens total) aren't matched on size.** SUMMARIES carries about 2× more cached-prefix cost upfront, which slightly handicaps it vs TREATMENT. A token-matched seed would tighten the comparison; we'd expect SUMMARIES to look modestly better.

3. **TREATMENT's measured savings include skipped h5i workflow steps.** TREATMENT runs 6.1 turns vs CONTROL's 16.5 — far below the h5i CLAUDE.md workflow's stated minimum. The agent under TREATMENT shortcuts some context traces / NOTE entries because the prompt feels "decisive" rather than "exploratory". Some of the savings is real cognitive savings; some is the agent cutting corners. Hard to disentangle from the numbers we have. SUMMARIES (9.9 turns) is closer to following the workflow and is therefore a *more apples-to-apples* feature value measurement vs CONTROL.

4. **AUTO_CLAIMS' break-even (0.76 sessions) is generous.** It assumes the 1 claim AUTO records gives TREATMENT-level future-session savings, but TREATMENT was seeded with 5 hand-curated claims. A 1-claim future session almost certainly saves less than a 5-claim one, so the realistic break-even is somewhat worse than 0.76. The exact figure isn't measured here.

5. **CLAUDE.md documentation overhead is paid by all arms, including CONTROL.** Adding the claims/summaries instruction tables (~1.5K tokens with the caveman examples) lives in every cached prefix, even when CONTROL can't use those features. We measure "feature vs h5i-without-feature", not "feature vs vanilla agent".

6. **Cache-read tokens bill at ~10% of uncached input** under Anthropic's current pricing, so a 68.6% cache-read drop translates to a real but smaller fraction of total input-side billing.

7. **Single model, single prompt template.** All trials used `claude-opus-4-7`. Smaller/older models likely show smaller absolute deltas because they explore less aggressively to begin with.

8. **N=10 is enough to firm up the four cache-read deltas, but tighter sub-claims (e.g. variance within TREATMENT) would benefit from more trials.** Most secondary metrics in the table above are still noise-flagged.

---

## Reproducing

```bash
# 1. Build h5i with claims + summaries support.
cargo build
H5I_BIN=$PWD/target/debug/h5i

# 2. Confirm tooling.
which claude timeout
$H5I_BIN claims --help
$H5I_BIN summary --help

# 3. The canonical run (4 arms × 10 trials = 40 sessions, ~25-40 min wall clock):
H5I_BIN=$H5I_BIN N_TRIALS=10 TRIAL_TIMEOUT=240 RETRY_CAP=1 ./scripts/experiment_claims.sh

# Quick sanity check (4 arms × 1 trial, ~3-5 min):
H5I_BIN=$H5I_BIN N_TRIALS=1 ./scripts/experiment_claims.sh
```

Each run:

- writes raw per-trial JSON records to `${WORKDIR_BASE}-results.jsonl.filtered`,
- preserves workdirs at `${WORKDIR_BASE}-{CONTROL,TREATMENT,AUTO_CLAIMS,SUMMARIES}-<trial>/`,
- and prints the comparison table + verdicts at the end.

Inspect a trial:

```bash
W=/tmp/h5i-claims-exp-136017-TREATMENT-1     # any preserved workdir

# What claims were active during the trial:
ls $W/.git/.h5i/claims/ && cat $W/.git/.h5i/claims/*.json

# What summaries were active:
ls $W/.git/.h5i/summaries/ 2>/dev/null

# What the agent actually saw at session start:
(cd $W && $H5I_BIN context prompt)

# What the agent edited:
git -C $W diff $(git -C $W rev-list --max-parents=0 HEAD)
```

### Requirements

- `h5i` CLI built from this commit (claims + summaries + eager rendering)
- `claude` CLI in `PATH`
- `timeout(1)` from GNU coreutils
- `git`, `python3`, `bash`

---

## Verdict

The hypothesis lands at N=10. **Pre-curated artifacts — both claims and summaries — measurably reduce cache-read tokens on a real task** (−68.6% and −46.4% respectively, both robust), with no fidelity loss (10/10 in every arm). The mechanism is the same in both cases: pre-load known content into the prompt's cached prefix so the agent doesn't pay tokens to re-derive it.

**Operational recommendations:**

- **Default frequency stays at `low`.** The single-session `freq=high` overhead is a real +52.2%, and the 0.76-session break-even depends on assumptions (TREATMENT-level future savings, multi-session use of the same workdir) that don't hold for one-shot tasks.
- **Caveman-style claim/summary text is mandatory, not optional.** The CLAUDE.md tables enforcing this make a measurable difference.
- **Eager-render summary content** when the count fits the budget (≤10 files / ≤2K chars total). For larger codebases, fall back to listing-only and let the agent fetch lazily.
- **Both claims and summaries pay off; pick by what you can write well.** Claims if you can name a sharp cross-file fact in 30 tokens; summaries if you have a non-trivial file whose orientation is reusable. Neither replaces the other.

**Experiments worth running next:**

1. **Larger codebase** (50+ files) where the orientation use case actually exercises summaries — would the SUMMARIES vs TREATMENT gap close or invert?
2. **Token-matched seeds** so TREATMENT and SUMMARIES carry identical upfront cache cost.
3. **Mixed arm** (claims + summaries together) to see whether the savings stack or saturate.
4. **Strict-workflow gate** that filters trials skipping required h5i context steps, isolating retrieval savings from workflow shortcuts.
