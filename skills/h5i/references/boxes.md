# The box lifecycle

## Sources

| Command | Base |
| --- | --- |
| `h5i box` | HEAD of this repository |
| `h5i box --from <rev>` | any revision in this repository |
| `h5i box <n>` / `#<n>` / PR URL | `refs/pull/<n>/head`, fetched and pinned to a local `pr/<n>` branch |

The base is **frozen at creation**. The parent branch moving afterwards is
*drift*, reported by `h5i box status`; `h5i box rebase <name>` is the sanctioned
re-pin (3-way, refuses conflicts).

Cloning an external repo URL and starting from an empty box are on the roadmap
(M2), not wired up yet.

## Naming

`--name` wins. Otherwise the name is the current branch, slugified, with a
numeric suffix if that name is taken. A box's full id is `env/<agent>/<name>`,
where the agent comes from `$H5I_AGENT` (`claude` in Claude Code, `codex` in
Codex, `human` on a bare shell).

## States

`created → running/idle → proposed → applied`, plus `aborted`.

- `h5i box propose` freezes the worktree with a **mediated commit**: h5i stages
  and commits, never the agent, and every path is validated against the
  canonicalized `$WORK` allowlist. Symlink escapes, nested `.git` directories
  and agent-introduced gitlinks are refused, and the whole commit fails closed.
- `h5i box export` runs that freeze and then writes the bundle.
- `h5i box abort` marks a box abandoned but keeps it for forensics.
- `h5i box gc` reclaims the worktrees of applied/aborted boxes.
- `h5i box rm <name>` removes a box permanently: worktree, branch, manifest and
  its lines in `refs/h5i/env/meta`. Only the append-only `removed` event
  survives. `--force` for a box that is still live.

## Concurrency

One read-write session per box (`run`, read-write `shell`, `propose`, `apply`,
`rebase`, `abort` serialize against each other), and any number of read-only
observers (`h5i box shell --readonly`). A teardown (`gc`, `rm`) refuses while an
observer is attached rather than pulling the directory out from under it.
