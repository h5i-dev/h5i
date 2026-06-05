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

## Everyday use

Wrap any command. h5i runs it, stores the full output, and prints **only** the
filtered summary. The child's exit code is passed straight through, so this is a
transparent drop-in for CI or a shell:

```bash
h5i capture run -- pytest -q
h5i capture run --kind log -- cargo build
h5i capture run --budget 40 -- ./flaky-integration-test.sh
```

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
5. **Dedup** runs of identical lines into a `(×N)` marker.
6. **Cap** to a line budget (`--budget`) and an optional token budget
   (`--token-budget`), marking elided spans explicitly: `… [N lines elided] …`.

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

Beyond the three coded adapters, h5i ships **59 declarative per-command rules**
covering gcc, make, terraform, eslint, biome, gcloud, docker, rsync, and more.
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
`[[tests.*]]` golden cases, which run as part of h5i's test suite (145 cases) to
prove the port stays faithful.

Still deferred: `git status`/`git log` adapters, npm/jest/vitest, and a
trust-gated project-local `.h5i/filters.toml` for user-defined rules.

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
