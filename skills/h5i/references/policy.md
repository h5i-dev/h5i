# Policy: what a box is allowed to do

Policy comes from a **profile**, resolved at creation and pinned by digest. The
digest in `h5i dev status` is what was actually enforced, not what was asked
for.

## Profiles

Two built-ins need no file:

- `default` — fail-closed build/test confinement. No home access, no network.
- `agent` — agent-in-box, and **runtime-scoped**: `agent-claude` grants only
  `~/.claude*` state and Anthropic egress, `agent-codex` only `~/.codex` and
  OpenAI. A Claude box cannot read Codex's credentials or reach its API.

Repo profiles live in `.h5i/env.toml` as `[profile.<name>]`. A profile of the
same name as a built-in overlays it. An omitted `net.egress` inherits the
built-in's allowlist; an explicit `egress = []` opts out.

Keys: `fs.read` / `fs.write` / `fs.deny`, `net.mode` (`deny` | `host`),
`net.egress` (domain allowlist, container tier only), `resources.{mem,procs,
wall,fsize,cpu}`, `env.pass`, `tools`, `container.image`, `[shell] rcfile`.

## Credentials

Host credentials do not enter a box. Model API calls are authenticated by a
host-side proxy that injects the key outside the boundary. Per-box HOME state
is a **copy**, seeded once from the real HOME and never written back, so
concurrent boxes cannot corrupt each other's session files.

Explicit secrets go through the broker and are recorded by id and fingerprint,
never by value:

```bash
h5i dev run <name> --secret DEPLOY_KEY -- ./deploy.sh
```

## Egress

Only the `container` tier has a real allowlist: a host-side DNS-pinned
HTTP/HTTPS CONNECT proxy, fail-closed with `403`. Exact hosts, `.wildcard`, and
`:port` forms. It blocks proxy-respecting tooling; airtight L3/L4 is a stronger
tier that does not exist yet, and the docs say so rather than implying more.

```bash
h5i dev allow api.example.com      # persistent user allowlist entry
h5i dev allow api.example.com --remove
```

Denied hosts become searchable `egress-denied` findings on the receipt.
