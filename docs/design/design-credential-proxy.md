# Design: the credential-injecting proxy

An agent box authenticates to its provider API without the long-lived token
entering the box. Code: `crates/h5i-sandbox/src/auth_proxy.rs`.

The egress allowlist proxy tunnels HTTPS with `CONNECT`, so it can gate the host
but cannot add an `Authorization` or `x-api-key` header. The auth proxy is a
second proxy that takes the box's request in cleartext on host loopback and
sends a new TLS request upstream with the real credential added host-side:

```
box --http--> 10.0.2.2:<authport> (host loopback)
              auth_proxy: verify dummy, strip it, inject real token
              --TLS--> https://api.anthropic.com/...
```

The box gets a base-URL override and a per-run dummy token:

| runtime | base-URL var | dummy var | upstream host |
|---|---|---|---|
| Claude | `ANTHROPIC_BASE_URL` | `ANTHROPIC_AUTH_TOKEN` | `api.anthropic.com` |
| Codex | `OPENAI_BASE_URL` | `OPENAI_API_KEY` | `api.openai.com` |

`NO_PROXY` gains `10.0.2.2` so the base URL is dialed directly, not through the
`CONNECT` proxy.

## Properties

- Token not in the box. `resolve_credential` reads it from h5i's host
  environment, with the runtime CLI's precedence. It is never a box env var,
  mount or argv. `Credential`'s `Debug` is redacted.
- No SSRF. Upstream is pinned to the runtime's one API host. The request's own
  `Host` is ignored; only its path is reused.
- DNS pinned once at spawn, like the egress proxy's `pin_dns`.
- Loopback and shared-secret gated. The listener binds `127.0.0.1` and injects
  only for requests carrying the per-run dummy token, which other host users do
  not have.
- No token or body logging. Upstream errors return a bare `502`.

Responses are relayed with connection-close framing (`reqwest` blocking +
rustls), which streams JSON and SSE as bytes arrive.

## When it engages

On the container and supervised backends (`auth_proxy::engage`), when all hold:

1. The box can reach the host proxy: container uses the egress proxy plan (slirp
   `allow_host_loopback` at `10.0.2.2`); supervised needs non-empty `net.egress`.
2. The profile is a known agent runtime (`agent-claude`, `agent-codex`, or bare
   `agent`).
3. A host credential resolves for that runtime.

Otherwise the box keeps its in-box login. `H5I_CREDENTIAL_PROXY=off` forces the
in-box path (for example, to use a subscription logged in inside the box). Once
a credential resolves, a proxy that fails to start refuses the run instead of
falling back.

## Supervised tier

The supervised tier normally runs `slirp4netns --disable-host-loopback`. When the
proxy engages (`supervisor::setup_egress` with `auth_port`):

- `--disable-host-loopback` is dropped so `10.0.2.2` reaches the host proxy;
- nftables becomes default-drop with one accept,
  `ip daddr 10.0.2.2 tcp dport <auth_port>`, so all other host-loopback ports and
  all direct API egress are dropped;
- `scrub_box_credentials` removes `.credentials.json` / `auth.json` from the
  env's own `~/.claude` / `~/.codex` copy, never the real HOME.

A token left in the box would still be unusable: the API host is dropped, there
is no other egress, and the proxy ignores the client's credential.

## Limits

- No audit record yet. The auth proxy should log runtime, upstream host,
  credential fingerprint and request count through the `secrets_broker` path.
- The `process` tier has no egress netns and stays on in-box login.
- After a scrub, running the same env with the proxy off finds no stored
  credential and prompts an in-box login, which repopulates the copy.
- Credentials are long-lived on the host. `SecretGrant::ttl` anticipates a
  minting source the proxy could refresh per window; not built.
