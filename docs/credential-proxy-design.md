# Credential-injecting egress proxy ("option 2")

**Goal:** let an agent box (`h5i env`, `isolation=container`) authenticate to its
provider API **without the long-lived API token ever entering the box**, so a
prompt-injected or compromised agent has no reusable credential to read or
exfiltrate.

## Why the egress proxy alone can't do it

When a profile declares `net.egress`, the container backend
(`src/container.rs`) runs a host-side allowlist proxy. For HTTPS that proxy
speaks `CONNECT` — it tunnels an **end-to-end-encrypted** TLS stream. It can
gate *which host* the box reaches, but it can never read or rewrite the request,
so it cannot inject an `Authorization` / `x-api-key` header. The token therefore
had to live inside the box (a credential file or an env var), where the agent
can read `/proc/self/environ` and, absent an airtight L3/L4 egress boundary,
potentially exfiltrate it.

## Design

A **second**, credential-injecting proxy (`src/auth_proxy.rs`) terminates the
box→proxy hop in cleartext on host loopback and **re-originates** a fresh TLS
request upstream, injecting the real credential host-side:

```
box ──http──▶ 10.0.2.2:<authport> (host loopback)
              auth_proxy: verify dummy → strip it → inject REAL token
              └──TLS──▶ https://api.anthropic.com/…   (real token, host-side only)
```

The agent is pointed at the proxy with a base-URL override and a **dummy** token:

| runtime | base-URL var | dummy/auth var | upstream host |
|---|---|---|---|
| Claude | `ANTHROPIC_BASE_URL` | `ANTHROPIC_AUTH_TOKEN` | `api.anthropic.com` |
| Codex | `OPENAI_BASE_URL` | `OPENAI_API_KEY` | `api.openai.com` |

`NO_PROXY` is extended with `10.0.2.2` so the base URL is dialed directly rather
than re-wrapped through the egress `CONNECT` proxy.

### Security properties (all fail-closed)

- **Token never in the box.** The genuine credential is resolved from *h5i's
  own* host environment (`resolve_credential`, same precedence the runtime's CLI
  uses) and handed straight to the upstream request. It is never an env var,
  mount, or argv in the box; `Credential`'s `Debug` is redacted.
- **No SSRF.** The upstream host is pinned to the runtime's single API host. A
  request's own `Host`/authority is ignored — only its path is reused — so a
  prompt-injected box cannot aim the real token at an attacker host.
- **DNS-rebinding resistant.** The upstream host is resolved+pinned once at
  spawn (mirrors the egress proxy's `pin_dns`).
- **Loopback + shared-secret gated.** The listener binds `127.0.0.1`. Because
  loopback is reachable by *other* host processes too, the proxy injects the
  real credential only for a request presenting the **per-run dummy token** — an
  unguessable secret the box holds but other host users don't. (The box is
  *allowed* to use the proxy; other host users are not.)
- **Never logs the token or bodies.** Upstream errors are surfaced as a bare
  `502` so request/response detail can't echo out.

### Streaming

The live forwarder uses `reqwest` (blocking + rustls). Responses are relayed
under connection-close framing (upstream `Content-Length`/`Transfer-Encoding`
stripped), which streams both bounded JSON and unbounded SSE (`text/event-stream`)
back to the box as bytes arrive.

## When it engages

Automatically, on both the **container** and **supervised** backends, when
**all** hold (shared logic: `auth_proxy::engage`):

1. the box can reach the host proxy on this tier:
   - **container** — the net plan is the egress **proxy** plan (slirp
     `allow_host_loopback` at `10.0.2.2`);
   - **supervised** — `net.egress` is non-empty (the netns has a slirp uplink);
2. the profile is a known agent runtime (`agent-claude` / `agent-codex`, incl.
   the bare `agent` profile, which resolves to one);
3. a host-side credential is resolvable for that runtime.

If any fails, the box keeps its existing in-box-login path — we never *downgrade*
an active protection, and never break a working interactive-login flow that has
no host token to broker. Set `H5I_CREDENTIAL_PROXY=off` to force the in-box path
(e.g. to bill a subscription logged in inside the box rather than a
host-exported API key).

### Supervised tier specifics

The supervised (kernel) tier has no slirp `allow_host_loopback` by default — it
starts `slirp4netns` with `--disable-host-loopback` for an airtight L3/L4 netns.
When the auth proxy engages (`supervisor::setup_egress` with `auth_port`):

- `--disable-host-loopback` is **dropped** so the gateway `10.0.2.2` forwards to
  the host proxy on loopback;
- the nftables ruleset is rebuilt to a **proxy-only** allowlist: default-drop,
  with a single `ip daddr 10.0.2.2 tcp dport <auth_port> accept`. Every other
  host-loopback port **and all direct API egress** stay dropped. So the box's
  only reachable destination is the proxy — the boundary is the same nftables
  mechanism that enforces all supervised egress, now with one narrowly-scoped
  accept.
- the box's per-env `~/.claude` / `~/.codex` copy is **scrubbed** of its
  credential file (`.credentials.json` / `auth.json`) via `scrub_box_credentials`
  (only the env's own backing copy is touched — never the real HOME), so the
  token is *absent* from the box, not merely inert. Auth flows entirely through
  the proxy + the dummy env token.

Because egress is locked to the proxy alone, a token that somehow remained in the
box would still be inert: unusable directly (nftables drops the API host),
unexfiltratable (no other egress), and useless through the proxy (which injects
its own credential and ignores the client's).

## Follow-ups

- **Audit line.** The egress proxy records an allow/deny tally; the auth proxy
  should emit an evidence record (runtime, upstream host, credential
  *fingerprint* only, request count) so a reviewer can see the box authenticated
  via the broker. Integrate with `secrets_broker`'s fingerprint/audit path.
- **Process tier.** `supervised` is supported (above); the static `process` tier
  has no egress netns (domain allowlists are meaningless there), so it stays on
  the in-box-login path. No further work planned unless `process` gains egress.
- **Credential persistence edge (supervised).** `scrub_box_credentials` removes
  the credential from the persisted per-env HOME copy. If the same env is later
  run with the proxy disabled (opt-out or host token removed), the box has no
  stored credential and will prompt an in-box login (which repopulates the copy).
  This is the pre-option-2 behaviour, not a break — noted for awareness.
- **Short-lived minting.** `SecretGrant::ttl` anticipates a source that mints a
  scoped, expiring credential; the proxy could refresh it per window so even the
  host-side value is short-lived.
