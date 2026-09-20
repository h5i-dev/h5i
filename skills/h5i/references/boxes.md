# The box lifecycle

```bash
h5i box --name review
h5i box status review
h5i box run review -- <command>
h5i box diff review
h5i box export review
```

Outside a box you create, drive, inspect and export them. Inside one
(`$H5I_ENV_ID` is set) you work normally: do not create another box, and do not
pass `--in` to a browser command.

`h5i box probe` says what this host can enforce and `h5i box capabilities <name>
--json` what a box actually received. Never infer the tier. h5i fails closed
instead of silently weakening a requested policy.

An export is a proposal holding `patch.diff`, `report.md` and `receipt.json`.
Review the report, denied egress, redactions, browser evidence and patch before
applying it ([export.md](export.md)). Sharing admits traffic into agent-written
code, so run `h5i box share` only when the user asks, and say that `--tunnel`
lets Cloudflare terminate TLS ([share.md](share.md)). Read [policy.md](policy.md)
before changing profiles, filesystem access, egress, or credentials.

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
