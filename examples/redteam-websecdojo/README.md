# Sandboxed red-teaming: a websecdojo example profile

This is a worked example of driving an AI agent as a **red-teamer inside an h5i
box**: a disposable, confined worktree whose network is scoped to the target and
whose every request is recorded. The agent runs with permissions bypassed, which
is safe precisely because the box is the boundary, not the agent's own judgement.

The `redteam` profile in [`env.toml`](./env.toml) was used to solve two
[websecdojo](https://websecdojo.com/) challenges end to end, each by a separate
`claude` running inside its own supervised box.

## Run it

Copy the `[profile.redteam]` block from `env.toml` into your repo's
`.h5i/env.toml`, then:

```bash
h5i box create rt --profile redteam --isolation supervised
h5i box shell rt
```

Inside the box, start the agent and give it the task:

```bash
claude          # bypass-permissions works; h5i websec/recon are already installed
```

The agent drives the target with h5i's own tools:

```bash
h5i browser open https://websecdojo.com/vault/ --capture
h5i websec requests
# --body prints the decoded response body straight to stdout, like curl:
h5i websec replay req_0 --set method=HEAD --set header.user-agent=Agent33 --body
```

`--set` targets are `method=`, `path=`, `query.KEY=`, `header.NAME=`,
`cookie.NAME=`, `json.PATH=`. Drop `--body` to get the JSON envelope (ids,
applied edits, status) for scripting.

Two agents, one target each, works too: create `vault` and `pebble` boxes and
open one in each `tmux` window.

## Why this profile "just works"

The profile grants `~/.claude*` HOME state, which h5i reads as an agent-capable
profile. As a result the box automatically:

- seeds a copy of your `claude` session in (host credentials never enter the
  box; the copy is read-once and never written back),
- sets `IS_SANDBOX=1` so `claude --dangerously-skip-permissions` runs as the
  box's uid 0 instead of refusing,
- installs h5i's own plugins (`websec`, `recon`) into the box, so the agent can
  capture and replay HTTP without a manual `plugin install`.

Egress is enforced at L3/L4 by the `supervised` tier (nftables pinned to
resolved IPs): the agent can reach `websecdojo.com`, the Anthropic API, and the
listed CVE references, and nothing else. An off-list host fails to resolve.

## Scope and safety

- Test only targets you are authorized to test. Swap the `net.egress` hosts for
  your own scope; the profile denies everything not listed.
- The box's changes stay in the box until you review and `h5i box export` them.
- Credentials (`~/.ssh`, `~/.aws`, `~/.config/gh`) are denied outright.
