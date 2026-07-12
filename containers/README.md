# h5i agent-in-box container images

Ready-made images for `h5i env` **container mode** (`isolation=container`,
rootless Podman). The container tier never mounts your host HOME, so the agent
CLI has to live *in the image* — these Containerfiles bake it in, together with
git and an in-box `h5i` built from this checkout:

| File | Profile | Ships |
| --- | --- | --- |
| `Containerfile.agent-claude` | `agent-claude` | Claude Code (`@anthropic-ai/claude-code`), git, h5i |
| `Containerfile.agent-codex` | `agent-codex` | Codex CLI (`@openai/codex`), git, h5i |

## Quick start (Claude)

```bash
# 1. Build (context = repo root; compiles h5i from source, so the first build takes a while)
podman build -f containers/Containerfile.agent-claude -t h5i-agent-claude .

# 2. Make it the repo's default container image
printf '[container]\nimage = "localhost/h5i-agent-claude:latest"\n' >> .h5i/env.toml

# 3. Create + enter the box (the host credential is brokered by h5i's auth
#    proxy: the box gets a base-URL override + per-run dummy token; the real
#    token never enters the container)
h5i env create mytask --profile agent-claude --isolation container
CLAUDE_CODE_OAUTH_TOKEN=$(claude setup-token) h5i env shell mytask
```

For Codex, swap the Containerfile/tag and export `OPENAI_API_KEY` instead.
`--image localhost/h5i-agent-claude:latest` at `env create` works instead of
the `[container]` default (and alone makes the container tier the auto-pick).

## What the box looks like at run time

- **Read-only rootfs**; the only writable surfaces are `/work` (the env
  worktree), a private 256 MB `/tmp` tmpfs, and the env's own git plumbing.
- **HOME is repointed to `/tmp/agent-home`** by `entrypoint.sh` (h5i forwards
  the host's `$HOME` *path*, which doesn't exist in the image). Agent state is
  therefore **ephemeral per run** — by design: credentials come from the
  host-side auth proxy, not a persisted in-box login. (Codex prints a benign
  warning that it won't create optional PATH-alias helpers under `/tmp`; it
  proceeds normally.)
- **Egress** is the profile's API allowlist (Anthropic hosts for
  `agent-claude`, OpenAI hosts for `agent-codex`) via a DNS-pinned CONNECT
  proxy. Anything else — `npm install`, `cargo fetch`, `pip` — is blocked
  unless you add it: `h5i env allow registry.npmjs.org` (host-side, merged
  into egress-declaring profiles only).
- **Observation**: the tee-shim records shell commands for any image; the
  baked-in `h5i` additionally powers the unremovable managed-settings
  `wrap-bash` hook (Claude images) and in-box `h5i capture run`.

## Extending with your project's toolchain

The images ship the agent, not your build tools. Add what your repo needs on
top, e.g.:

```dockerfile
FROM localhost/h5i-agent-claude:latest
USER root
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential pkg-config && rm -rf /var/lib/apt/lists/*
```

Keep the `ENTRYPOINT` (or re-declare it) — it is what makes HOME writable.

Images must be **pre-pulled/pre-built**: h5i runs containers with
`--pull=never`, so a missing image fails closed rather than pulling silently.
