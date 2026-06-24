## feat: add h5i decision list --stale command

### Summary

Adds a `decisions` subcommand that queries recorded design decisions across commit history and flags those whose referenced code has drifted significantly since the decision was recorded.

- `h5i decisions list` — lists all decisions across history with commit OID, timestamp, location, choice, reason, and alternatives
- `h5i decisions list --stale` — filters to decisions where the referenced code has changed significantly, surfacing reasoning that may no longer apply
- `h5i decisions list --limit N` — controls how many commits to scan (default: 50)

### Motivation

The `--decisions` flag at commit time solves the write side of capturing design rationale. But recorded reasoning has a shelf life in that once the code it refers to changes significantly, the reasoning becomes incomplete and also potentially misleading. `decisions list --stale` fixes this by turning the decisions archive into an active signal that can be incorporated into `h5i resume` briefings alongside uncertainty signals and blind edits.

### Implementation

**New types in `metadata.rs`** (not serialized — computed at query time only):
- `StalenessStatus` — `Fresh` / `Stale { similarity }` / `Modified { similarity }` / `Unresolvable { reason }`
- `DecisionEntry` — decision + commit OID + timestamp + optional staleness

**Staleness detection** uses a two-tier strategy to keep the common case fast:
1. Compare ±5 lines of context around the recorded location. If unchanged → `Fresh`, done.
2. If context changed, compute AST similarity (0.0–1.0). Below 0.5 → `Stale`, at or above → `Modified`. Falls back to whole-file content comparison for file types without an AST parser.

**`decisions_list()`** in `H5iRepository` returns all entries unfiltered — display filtering is left to the caller so the method remains reusable for future MCP tool exposure.

### Design decisions

- `Decision.location` stays a free-form string; `FileLine` / `FileOnly` / `NonPath` classification happens at query time, not at record time — keeps the JSON schema simple and human-writable
- `decisions_list()` returns all entries; the CLI filters for `--stale` — avoids baking display logic into the data layer
- `StalenessStatus` and `DecisionEntry` do not derive `Serialize`/`Deserialize` — these types are never persisted

---

## feat: stale decision commit warnings

Directly extends decisions --stale feature. When `h5i commit` runs, check whether the staged diff touches a line near a previously recorded stale decision. If so, print an inline warning before the commit completes:

  ⚠  Commit touches src/repository.rs:88 near a stale decision from abc123: "chose JSON over binary"
     reason was:  reduces parse overhead at startup
     Has this reasoning changed? Run `h5i decisions --stale` for details.

### Design

- **Line-level matching, not file-level.** Uses diff hunk ranges from `git2::Patch` against the decision's recorded `file:line` location (±`CONTEXT_LINES` proximity). File-level matching produces too many false positives on large files.
- **Stale-only signal.** `Modified` and `Fresh` entries are dropped. Only decisions with similarity < 0.5 produce a warning.
- **Non-blocking.** Errors from the staleness check are swallowed so a bug here never breaks the commit path.

### Testing

Two unit tests added:
- `fires_when_line_touched` — hunk spanning the decision line triggers the warning
- `silent_when_far` — hunk far from the decision line produces no warning

