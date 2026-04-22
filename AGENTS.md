
## h5i Integration

This repository uses **h5i** (a Git sidecar for AI-era version control).

Codex should use `h5i context` as shared cross-session memory and `h5i commit` to record AI provenance on code commits.

### Required workflow

At the start of a non-trivial task:
```bash
h5i codex prelude
# If no workspace exists yet, initialize it once:
h5i context init --goal "<one-line task summary>"
```

While working:
```bash
h5i context relevant <file>   # before editing a file when relevant
h5i codex sync                # after a burst of reads/edits to backfill OBSERVE/ACT traces
h5i context trace --kind THINK "<chosen approach> over <rejected alternative> because <reason>"
h5i context trace --kind NOTE "TODO: … / LIMITATION: … / RISK: …"
```

After a logical milestone:
```bash
h5i codex finish --summary "<milestone summary>"
```

For code commits:
```bash
git add <exact paths>
h5i commit -m "…" --agent codex --prompt "…"
```

