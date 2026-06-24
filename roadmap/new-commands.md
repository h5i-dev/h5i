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

---

## Editing Message of Previous Commits


Currently, `h5i` does not provide functionality for editing the message of previously-made commits. As projects develop, and as different aspects of past commits grow more relevant to current development, this immutability may be an inconvenience.

To address this issue, the `edit-message` function provides functionality for editing the message of a previously-made commit. The function may be called as follows:

`h5i edit-message <oid> --edit <new message>`

The function returns the new OID of the commit whose message was edited.

Commits with or without dependencies may be edited. If the commit whose message was edited has dependencies, the parent OID of each dependency is updated accordingly.

Example usage:

```
$ h5i log
commit 8ae70031018fc596eacede251391cf69130feac4
Author:    Srivarun Kankanala <sk5767@columbia.edu>

    test commit 2, dependent on test commit 1

────────────────────────────────────────────────────────────
commit b945b7bc3f1c6c409b193f3122a437755dfe7d06
Author:    Srivarun Kankanala <sk5767@columbia.edu>

    test commit 1

────────────────────────────────────────────────────────────

$ h5i edit-message b945b7bc3f1c6c409b193f3122a437755dfe7d06 --edit "new message"
 Editing commit message for b945b7bc
 New commit OID: f2afb521550377ef1c8928e5338d46877866d25c

$ h5i log
commit 0c4f991ce3c7be437aabfdfc90ff1c343006cf04
Author:    Srivarun Kankanala <sk5767@columbia.edu>

    test commit 2, dependent on test commit 1

────────────────────────────────────────────────────────────
commit f2afb521550377ef1c8928e5338d46877866d25c
Author:    Srivarun Kankanala <sk5767@columbia.edu>

    new message

────────────────────────────────────────────────────────────

$ h5i edit-message 0c4f991ce3c7be437aabfdfc90ff1c343006cf04 --edit "test commit 2, dependent on new message commit"
 Editing commit message for 0c4f991c
 New commit OID: 209ab5640f7e7439ae6730eafedfa505b96da784

$ h5i log
commit 209ab5640f7e7439ae6730eafedfa505b96da784
Author:    Srivarun Kankanala <sk5767@columbia.edu>

    test commit 2, dependent on new message commit

────────────────────────────────────────────────────────────
commit f2afb521550377ef1c8928e5338d46877866d25c
Author:    Srivarun Kankanala <sk5767@columbia.edu>

    new message

────────────────────────────────────────────────────────────
```
Attached are screenshots of the h5i UI after each of the calls to edit-message in the example above.

Intial:
<img width="953" height="482" alt="First Call" src="https://github.com/user-attachments/assets/bdd1cfdc-0b12-4720-ba8f-d2106ba698df" />


First Call:
<img width="953" height="482" alt="Second Call" src="https://github.com/user-attachments/assets/f49f018a-70f7-4763-9077-614f373d40a7" />


Second Call:
<img width="955" height="481" alt="Third Call" src="https://github.com/user-attachments/assets/919f9d56-3ee3-4553-8f5b-a66b06670e17" />


Comprehensive testcases are included in CI.


UPDATE:
Instead of using `filter-branch`, which poses several stability concerns, this new implementation of `edit-message` employs a Breadth-First Search to find the target commit from the HEAD commit. The process for editing the message of a commit is as follows:

- Each commit stores `caused_by` and `causes` fields in its `h5i Note`, maintaining bidirectional causal links.
- A BFS from `HEAD` through Git parent links locates the target commit, recording the path from target to `HEAD` as an ordered sequence.
- The target commit is rewritten with the new message, producing a new OID. Its `h5i Note` is moved to the new OID.
- The path from target to `HEAD` is iterated linearly. Each commit is rewritten with remapped parent OIDs, and its `h5i Note` is moved to the new OID.
- A second linear pass over the rewritten commits updates any stale OID references in their `caused_by` and `causes` Note fields.
- Commits outside the rewritten path whose `caused_by` referenced an old OID have their Notes patched in place, without OID changes.
- `HEAD` is advanced to the new tip commit.
