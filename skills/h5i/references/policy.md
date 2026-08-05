# Policy: what a box is allowed to do

Policy comes from a **profile**, resolved at creation and pinned by digest. The
digest in `h5i box status` is what was actually enforced, not what was asked
for.

## Profiles

Three built-ins need no file:

- `default` — fail-closed build/test confinement. No home access, no network.
- `agent` — agent-in-box, and **runtime-scoped**: `agent-claude` grants only
  `~/.claude*` state and Anthropic egress, `agent-codex` only `~/.codex` and
  OpenAI. A Claude box cannot read Codex's credentials or reach its API.
- `browser` — the agent profile plus a headless Chrome and the `agent-browser`
  binary, in the **same box**, so the dev server the agent just started is on
  `localhost:3000` for the browser too. Egress is exactly the agent profile's,
  never wider. Create refuses on a host without the tooling rather than handing
  back a box whose first `agent-browser open` fails.

Repo profiles live in `.h5i/env.toml` as `[profile.<name>]`. A profile of the
same name as a built-in overlays it. An omitted `net.egress` inherits the
built-in's allowlist; an explicit `egress = []` opts out.

Keys: `fs.read` / `fs.write` / `fs.deny`, `net.mode` (`deny` | `host`),
`net.egress` (host allowlist; enforced differently per tier, see below),
`resources.{mem,procs,wall,fsize,cpu}`, `env.pass`, `tools`, `container.image`,
`[shell] rcfile`.

## Credentials

Host credentials do not enter a box. Model API calls are authenticated by a
host-side proxy that injects the key outside the boundary. Per-box HOME state
is a **copy**, seeded once from the real HOME and never written back, so
concurrent boxes cannot corrupt each other's session files.

Explicit secrets go through the broker and are recorded by id and fingerprint,
never by value:

```bash
h5i box run <name> --secret DEPLOY_KEY -- ./deploy.sh
```

## Egress

Two tiers enforce `net.egress`, and they are not equivalent:

- **`supervised`** puts the box in a private network namespace and enforces the
  allowlist with **nftables rules pinned to resolved IPs**, with DNS pinned by
  a hosts file and a seccomp-notify gate on `socket()`. This is L3/L4: a
  program that ignores proxy settings still cannot reach an off-list address.
- **`container`** routes through a host-side DNS-pinned HTTP/HTTPS CONNECT
  proxy, fail-closed with `403`. This is L7: it binds proxy-respecting tooling
  and buys portability, not airtight scoping.

Both accept exact hosts, `.wildcard`, and `:port` forms. `process` has no
egress enforcement, so a non-empty `net.egress` is refused there rather than
silently ignored.

```bash
h5i box allow api.example.com      # persistent user allowlist entry
h5i box allow api.example.com --remove
```

Denied hosts become searchable `egress-denied` findings on the receipt.

## Authenticated egress

A box never holds a credential. When it must talk to an authenticated service,
the profile says so and h5i runs a host-side proxy that adds the header on the
way out:

```toml
[[profile.review.auth]]
host = "api.github.com"
credential_env = "GITHUB_TOKEN"   # read on the host, never inside the box
base_url_var = "GH_HOST"          # what the client is pointed at
```

Two limits worth knowing before you declare one. It binds clients you can point
at another origin, so a plain `curl https://api.github.com` still goes nowhere.
And a grant whose `credential_env` is unset on the host is an error at launch:
h5i refuses rather than sending the box out unauthenticated.

What the box may *do* with that credential is authorization, and belongs in a
fine-grained token scoped to the repository and operations you meant.
