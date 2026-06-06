# Token Reduction — the h5i object store

Large tool outputs (test logs, build output, big JSON payloads, traces) are the
biggest avoidable drain on an agent's context window. A 4 MB `pytest` log is
mostly noise: the agent needs the two failures and the summary line, not the
3,000 lines of `collected … PASSED`.

h5i's **object store** solves this the way git-annex and git-lfs solve large
files — by splitting one output into two artifacts:

| Artifact | Where it lives | Size | Travels with `git push`? |
| --- | --- | --- | --- |
| **Raw blob** | `.git/.h5i/objects/ab/cd/<sha256>` (local) | huge | No (stays local; `h5i share push` carries only pointers) |
| **Manifest** | `refs/h5i/objects` (git ref) | tiny | Yes, via `h5i share push` |

The agent reads only the manifest's **summary**. The full bytes are one command
away (`h5i recall object <id>`) but never sit in context unless asked for.

```
┌─ raw output (3029 tokens) ─┐         ┌─ manifest (257 tokens) ──────────────┐
│ compiling crate_1 …        │  filter │ kind=tool-output exit=1               │
│ … 298 more …               │ ───────▶│ raw_oid=sha256:82bf… raw_size=7485   │
│ error[E0382]: …            │  store  │ summary: "compiling… error[E0382]…    │
│ test result: FAILED        │         │           test result: FAILED"        │
└────────────────────────────┘         └──────────────────────────────────────┘
        ▼ stored at .git/.h5i/objects/82/bf/82bf…  (rehydrate any time)
```

## Structured output (the default)

`h5i capture run` emits a **normalized, AI-friendly structured result** —
one predictable schema across test runners, compilers, linters, and type checkers,
so an agent learns *one* shape instead of N free-text formats.

The **default render is `--format compact`**: one line per finding, token-minimal
(rtk-style), e.g.

```text
pytest test failed · 1 failed, 120 passed (exit 1)
  F tests/t.py::test_pay  assert 0 == 100
```

`--format structured` gives the full YAML (every field, for inspection):

```yaml
tool: pytest
kind: test
status: failed          # passed (tests) | ok (other tools) | failed | error | unknown
exit_code: 1
counts: { failed: 1, passed: 120 }
parser_confidence: parsed   # parsed | heuristic | generic
raw_oid: sha256:934f…       # full output, always recoverable
findings:
  - kind: test_failure      # test_failure | diagnostic | build_error | panic | generic
    severity: failure
    id: tests/t.py::test_pay
    message: assert 0 == 100
    location: tests/t.py:42
    fingerprint: 0bb827e4e61a   # stable across line shifts → dedupe/query
```

`--format json` returns the canonical JSON `ToolResult`; `--format summary` keeps
the legacy filtered text. (The `h5i_capture_run` MCP tool returns the full
`ToolResult` under a `structured` field, alongside `id`/`raw_*`/`hint`.) On
diagnostic-dense output (linters/type-checkers) `compact` is ~3× smaller than the
full YAML — the difference between a token *loss* and a *win*. The structured
result is stored in the manifest, so captures are **queryable**:

```bash
h5i recall objects --status failed     # everything that failed
h5i recall objects --tool pytest       # by tool  (compose with --branch/--file)
```

**Parser coverage.** Dedicated structured parsers (rich `findings`): **pytest,
cargo test, go test, tsc, eslint, ruff, mypy**. Every other tool falls back to a
**generic** result — correct `status` from the exit code (never claims success on
a nonzero exit) plus the reduced text in `body`. Each parser **declines to
generic when its anchors are missing**, so it never invents structure
(`parser_confidence` tells the agent how much to trust it). Safety, lossless raw,
and field caps from the object store all still apply.

## Everyday use

Wrap any command. h5i runs it, stores the full output, and prints **only** the
filtered summary. The child's exit code is passed straight through, so this is a
transparent drop-in for CI or a shell:

```bash
h5i capture run -- pytest -q
h5i capture run --kind log -- cargo build
h5i capture run --budget 40 -- ./flaky-integration-test.sh
```

Output below `--min-bytes` (default 2 KB) just passes through unstored, so it's
safe to wrap *any* command — wrapping is a no-op when there's nothing to reduce.
Use `--min-bytes 0` to force capture of small output.

`capture run` combines the command's stdout and stderr into one blob (stderr,
when present, follows a `----- stderr -----` marker) before filtering/storing —
so it is *not* a stream-separated passthrough. The exit code is always preserved.

**Making agents use it.** Run `h5i objects setup` once to wire token-reduction
guidance into the project's `.claude/h5i.md` and `AGENTS.md`, so agents know to
wrap large-output commands. In Claude Code, the **`h5i_capture_run` MCP tool**
exposes the same behavior with no shell-quoting — agents call it instead of the
Bash tool and get back just the summary + object id.

Ingest output you already have:

```bash
h5i objects put build.log
some-noisy-tool | h5i objects put -
```

Get the raw back (exact bytes), or just its summary / manifest:

```bash
h5i recall object 82bf3c51        # full raw bytes to stdout
h5i recall object 82bf --summary  # the reduced summary
h5i recall object 82bf --manifest # the JSON pointer record
h5i recall objects --limit 20     # list captures, newest first
```

Handles accept the short id, a full `sha256:<hex>`, or any unambiguous prefix.

### Tracking captures by branch / files / diff

Every capture is automatically associated with **the branch** it was taken on,
**the files it concerns** (explicit `--file` flags plus paths mentioned in the
output, e.g. `src/auth.rs:55` in a panic), and **the working-tree diff** at
capture time (the files you were editing). That makes captures queryable by the
work they belong to:

```bash
h5i capture run --file src/auth.rs -- pytest tests/test_auth.py   # tag explicitly
h5i recall objects --branch feature/login   # captures taken on a branch
h5i recall objects --file auth.rs           # captures touching a file (suffix match)
h5i recall objects --diff                    # captures relevant to your CURRENT edits
```

The listing shows each capture's branch (`⎇`) and associated files (`⊞`). This
lets an agent resuming work on a branch pull up exactly the test/build output
relevant to it, without rerunning anything.

## The filter

Deterministic and dependency-free (no model, no network) — the same input always
produces the same summary, which is what makes the stored `filter_version`
meaningful. The pipeline:

1. **Strip** ANSI escapes and collapse carriage-return progress bars.
2. **Classify** the payload: `test` / `log` / `json` / `diff` / `generic`
   (override with `--kind`).
3. **Score** every line — panics/exceptions/`error[...]` highest, failures and
   stack frames next, warnings, then summary/status lines, then file paths.
4. **Select** the head, the tail, and *every high-signal line* — so an error
   buried deep in otherwise-noisy output is never silently dropped.
5. **Fold** near-identical lines by *normalized template* (digits/hex → `#`)
   into one representative `(×N)` — so 800 `handled request <n> in <m>ms`
   log lines collapse to a single line. (Borrowed from headroom's
   `_dedupe_similar`, Apache-2.0.)
6. **Cap** to a line budget (`--budget`) and an optional token budget
   (`--token-budget`), marking elided spans explicitly: `… [N lines elided] …`.

High-signal lines (errors/warnings/panics) are always kept verbatim; only
lower-signal noise is folded, so a failure buried in a flood of logs survives.
On a 17k-token, 800-line service log with one buried error, this yields a
~70-token, 3-line summary.

`json` payloads get a structural skeleton (shape, key types, array lengths) with
error/status/message/code fields surfaced verbatim. `diff` payloads keep file and
hunk headers plus a bounded window of changed lines per hunk.

The filter **never invents text**. Elisions are always marked, and the raw is
always retrievable.

### Command-aware adapters

When `h5i capture run` knows the command argv, a thin **adapter layer** produces
a semantic summary for a few high-traffic tools, falling back to the generic
scorer for everything else (and whenever you pass an explicit `--kind`):

| Tool | All-pass | On failure |
| --- | --- | --- |
| **pytest** (`pytest`, `python -m pytest`) | `Pytest: 184 passed in 2.5s` | counts + the `FAILURES` headers, `E   ` assertion lines, and `FAILED`/`ERROR`/`XFAIL` summary lines (capped at 10 failures) |
| **cargo** (`test`/`check`/`clippy`/`build`) | `Cargo test: ok` + `test result:` tallies | strips `Compiling`/`Finished` noise, keeps each `error[...]`/`warning:` block with its `-->` span, aggregates results |
| **git diff** / `git show` | — | `git diff: N files changed, +A -B` header + file/hunk headers + bounded changed lines |

```text
$ h5i capture run -- pytest -q
Pytest: 184 passed in 2.53s        # 186 raw lines → 1
```

Adapters are deterministic, dependency-light, and covered by golden-style tests
(buried failure retained · all-pass shrinks aggressively · success never empties ·
raw rehydrate stays exact). Each may decline (`return None`) so a malformed or
unexpected output never produces a misleading summary — it just falls back.

### Declarative rules (the long tail)

Beyond the three coded adapters, h5i ships **74 declarative per-command rules**
covering the JS (npm/pnpm/yarn/tsc/eslint/jest/vitest), Python (pip/ruff/mypy/
black/flake8), Go (go/golangci-lint), and container (docker) ecosystems, plus
gcc, make, terraform, gradle, gcloud, rsync, and more.
Each rule is a small TOML pipeline matched against the command by regex:

```
strip_ansi → replace → match_output (with `unless` guard) →
strip/keep_lines_matching → truncate_lines_at → head/tail_lines →
max_lines → on_empty
```

```bash
h5i objects filters            # list all built-in rules
h5i objects filters --verify   # run every rule's inline golden tests
```

Precedence in `capture run`: **coded adapters → declarative rules → generic
scorer**. An explicit `--kind` opts out of all of them.

This engine and its rule set are ported from **rtk**
(<https://github.com/rtk-ai/rtk>, Apache-2.0, © Patrick Szymkowiak) with
modifications — see `assets/filters/NOTICE`. Each rule keeps its upstream inline
`[[tests.*]]` golden cases, which run as part of h5i's test suite (175 cases) to
prove the port stays faithful.

#### Project-local rules (trust-gated)

A repo can ship its own rules in `.h5i/filters.toml` (same schema), applied
*before* the built-ins so a project can override. Because that file is untrusted
input — a malicious rule could `match_output` a fixed `"ok"` and hide a real
failure — it is applied **only after you trust its current content**:

```bash
h5i objects trust            # review the rules (risky ones flagged) + trust them
h5i objects trust --status   # show: NoFile / Untrusted / Changed / Trusted
h5i objects trust --remove   # stop applying project rules
```

`capture run` prints a warning and falls back to built-ins when the file is
untrusted or has changed since it was trusted (any edit re-arms the gate). The
trusted content hash is stored in `.git/.h5i/trusted_filters.json` (local, never
shared). `H5I_TRUST_FILTERS=1` overrides the gate for CI. The review step flags
rules whose `match_output` lacks an `unless` guard, since those can replace real
output with a fixed message.

Still deferred: `git status`/`git log` adapters and npm/jest/vitest.

#### Provenance of borrowed ideas

The declarative engine and rule set come from **rtk**
(<https://github.com/rtk-ai/rtk>, Apache-2.0); the log template-folding technique
is from **headroom** (<https://github.com/chopratejas/headroom>, Apache-2.0). Both
are license-compatible
with h5i (Apache-2.0) and credited in `assets/filters/NOTICE` / module docs.
**context-mode** is under the Elastic License 2.0 (source-available, not
permissive), so no code was copied from it — only generic techniques h5i already
implements independently (byte-safe truncation, retrieval pointers).

## Lifetime & space

Manifests are immutable and kept forever — they're tiny and greppable. Only the
*local raw blobs* expire, and only when you ask:

```bash
h5i objects gc                 # remove orphan blobs (no manifest references them)
h5i objects gc --ttl 30d       # also evict referenced blobs older than 30 days
h5i objects gc --dry-run       # show what would go, delete nothing
h5i objects pin 82bf           # protect a blob from eviction
h5i objects unpin 82bf
h5i objects fsck               # report absent blobs and orphans
```

GC **never rewrites a summary**. Evicting a referenced blob just makes its raw
"absent" — the summary still works, and (once remote backends exist) the raw can
be rehydrated. Pinned blobs are never evicted.

## Sharing

`h5i share push` / `pull` carry `refs/h5i/objects` — the pointer records and their
summaries — alongside the other h5i refs. The manifest log is append-only, so a
divergence between two clones is **union-merged** on pull (no pointer is ever
lost). Raw blobs stay local; only their summaries travel today.

## Layout reference

```
.git/.h5i/objects/
  ab/cd/<sha256>     raw blob (uncompressed; codec="none" reserved for zstd)
  pins               one pinned digest per line

refs/h5i/objects     git ref → tree → manifests.jsonl (one JSON manifest per line)
```

A manifest record:

```json
{
  "id": "82bf3c51c5ed7204",
  "kind": "tool-output",
  "cmd": "pytest -q",
  "exit_code": 1,
  "git_tree": "…",
  "branch": "feature/login",
  "files": ["src/auth.rs"],
  "diff_files": ["src/auth.rs", "tests/test_auth.py"],
  "timestamp": "2026-06-05T…Z",
  "raw_oid": "sha256:82bf…",
  "raw_size": 7485,
  "raw_lines": 302,
  "filter_version": 1,
  "summary": "compiling crate_1 …\n… [278 lines elided] …\nerror[E0382]…",
  "highlights": ["error[E0382]: borrow of moved value at src/x.rs:10"],
  "store": "local",
  "codec": "none",
  "raw_tokens": 3029,
  "summary_tokens": 257
}
```

## Design notes

- **No new dependencies.** Blobs are stored uncompressed (git-lfs does the same);
  the `codec` field reserves room for a `zstd` codec later without changing the
  layout. Token counting reuses the existing `tiktoken-rs` dependency and is
  best-effort (a missing tokenizer degrades to no token stats, never an error).
- **Backends are trait-shaped** (`objects::Backend`) so an S3 / HTTP / LFS-style
  remote store can be added later; only `LocalStore` exists today.
- **Pointers carry the full digest.** Truncated digests would make cross-clone
  retrieval and sync brittle; every manifest records `sha256:<64 hex>`.
- **Manifest text is hard-capped.** Since manifests travel via `h5i push`, the
  git-tracked `summary` and `highlights` fields are bounded as a backstop on top
  of the filter's line/token budget: `summary` ≤ 16 KiB (UTF-8-safe, with a
  `… [summary truncated] …` marker), at most 20 `highlights`, each ≤ 500 bytes
  (`objects::MAX_SUMMARY_BYTES` / `MAX_HIGHLIGHTS` / `MAX_HIGHLIGHT_BYTES`). The
  full output is always recoverable from the raw blob regardless.
