# Design: the browser

A browser session is Chromium, driven by agent-browser, with every request it
sends passing through a per-session h5i capture proxy. h5i owns the proxy, the
network layer behind it (`crates/h5i-browser`) and the session record
(`crates/h5i-core/src/browser_session.rs`). It does not own the page engine.
The commands are in [`MANUAL.md`](../MANUAL.md#h5i-browser); this file covers
how the parts fit and what each one guarantees.

h5i's own page engine (Blitz, Stylo, Boa) was removed on 2026-10-19; its
design is in git history.

## Architecture

```
h5i browser <verb>
   |                         page verbs (stdin, never argv)
   +-----------------------> agent-browser --> Chromium
   |                                              |
   |                                              | every request
   |                                              v
   |  workbench verbs        h5i __capture-proxy (hudsucker, per-session CA)
   +-----------------------> control channel      |
                               |                  | LocalBroker: policy, receipt,
                               +--- shares -------+ jar, capture store
```

A session is two processes under one record:

- h5i's half: `h5i __capture-proxy` (`src/cli/capture_proxy.rs`), an
  intercepting proxy bound to loopback, with the session's control channel
  beside it. Both write through one `LocalBroker` (`net.rs`).
- The browser half: Chromium, run by agent-browser with `--proxy` pointed at
  that proxy and `--ignore-https-errors`, so it accepts the certificates the
  proxy mints from the session's CA.

Page verbs (`snapshot`, `click`, `type`, `screenshot`, ...) go to Chromium
through agent-browser, with the session's launch flags on every call so a
relaunched browser is still captured. Each command is passed on stdin, so a
value shaped like a flag stays a value and a credential is not in the process
table.

Workbench verbs go to the control channel (`control.rs`), one JSON request and
one JSON reply per line. The verbs are an exhaustive table in `verbs.rs`:
`status`, `requests`, `resend`, `env`, `socket`, `grpc`. A failure carries a
`code` (`unknown-verb`, `bad-request`, `refused`, `internal`), prose naming the
recovery, and `retryable`.

Proxy traffic and workbench replays share one broker, so their sequence numbers
cannot collide and both land in one timeline.

### What the crate holds

| module | role |
|---|---|
| `policy.rs` | origin allowlist, deny rules, loopback exemption, address checks |
| `receipt.rs`, `h5i-wire` | the request log row and the `Sink` it is written to |
| `capture.rs` | owner-only message store: headers and bodies, both directions |
| `cookies.rs` | the session jar |
| `secrets.rs` | `H5I_SECRET_*` resolution and redaction |
| `budget.rs` | per-navigation spending ceilings |
| `net.rs`, `broker.rs` | `LocalBroker` and the operations other code may call on it |
| `edits.rs`, `multipart.rs` | structured request edits for `resend` |
| `ws.rs`, `wsclient.rs`, `sse.rs`, `rawsock.rs`, `grpc.rs` | the socket, stream and gRPC verbs |
| `dom_inject.rs` | the opt-in DOM instrument the proxy injects |
| `control.rs`, `verbs.rs` | the control channel |

## Invariants

No record, no request. For every request Chromium sends, the proxy asks the
broker to record it first (`observe_proxy_request`). The policy verdict is
written into that row. If the row cannot be appended, the proxy answers `507`
("h5i refused an unrecorded request") and nothing is forwarded. A request that
is not in `h5i browser requests` did not go through the session. The `Sink`
trait returns a `Result` for this reason: a sink that swallows errors would
turn fail-closed into fail-open.

Denied requests are answered at the proxy with `403` and the reason, and the
denial is a row in the log.

The message store is separate from receipts. Receipts are safe to export; the
store holds bodies, `Authorization` headers and session cookies. Store writes
are best-effort and their failures are counted, so capture never blocks a
fetch. A missing or truncated body keeps an explicit state instead of appearing
empty. The proxy records the wire bytes before it changes anything (for example
injecting the DOM instrument).

The control channel and the proxy act on the same network layer; the control
channel holds no page.

## Policy

`Policy::check` (`policy.rs`) decides a URL:

- Deny rules (`--deny`, `--deny-path`) run first, before the loopback
  exemption, so nothing reaches past them.
- Schemes: `http`, `https`, `ws`, `wss` are checked by the same rules; `data:`
  is allowed because it reaches no network; anything else is refused by name.
- Origins: the session's target origin plus `--allow` entries. `*.host`
  wildcards carry the same scheme and port constraints as an exact origin.
- Loopback is allowed by default because it is the dev server.
  `--no-loopback` removes it. Loopback is decided by parsing the host as an
  address, so `127.evil.com` is not loopback and `[::1]` is.

A session's policy is fixed when its proxy starts. `open` on an existing
session refuses creation-only flags (`--allow`, `--no-loopback`, ...) rather
than ignoring them.

Two limits of the proxy path:

- The proxy checks the URL. It connects upstream through hudsucker's own
  connector, so the address-level check (`check_address`, internal ranges in
  `is_internal_address`) and DNS pinning apply only to connections the broker
  makes itself (`resend`, `socket`, `grpc`).
- The budget in `budget.rs` (request count, wire and decoded bytes, network
  time, `max_load_time`) is charged by broker-originated requests. Proxy
  traffic is recorded, not budgeted.

## Requests the broker makes

`resend`, `sequence`, `socket` and `grpc` put bytes on the wire from h5i
itself, under the session's policy:

- Redirects are followed by hand, so every hop is a policy decision of its own.
- A `301`, `302` or `303` turns a `POST` into a bodyless `GET`, so a password
  is not replayed to wherever the server points.
- The host's resolved addresses are checked and pinned; a caller that connects
  itself (a WebSocket) asks for the approved addresses.
- A remote WebSocket is refused when the session routes through an egress
  proxy, since a raw socket would bypass it.
- Edits (`method=`, `header.X=`, `json.a.b=`, ...) fail rather than coerce. A
  target the request lacks is refused unless the edit is marked `+` or the run
  passes `--create`.
- A request carried in from another session (`resend --as`) has its
  credentials dropped; this session's come from its jar.

Each budget ceiling is charged to a `Spender`. `max_load_time` is checked only
for `Spender::Page`, so a replay the agent drives is charged what it spent,
not how long ago the session last navigated.

## Cookies

The jar learns from the `Set-Cookie` headers the proxy sees and from cookies
Chromium sends that the jar does not hold (stored host-only). It exists so
broker-originated requests carry this session's credentials. Narrowings
enforced at store time:

- `Domain` widening is refused onto a public suffix, onto a domain the setter
  is not within on a label boundary, or from an IP host.
- `__Secure-` requires `Secure`; `__Host-` also requires `Path=/` and no
  `Domain`.
- `SameSite=None` requires `Secure`.
- `HttpOnly` is honoured: a script-side reader does not get those cookies.

Callers outside the broker do not get the jar. They get three operations
(`document_cookie`, `store_cookie`, `keep_only_origin`), each enforcing
`HttpOnly` and origin scoping (`net.rs`, the `jar()` doc comment).

## Credentials

The agent names a credential and never reads it:

```bash
export H5I_SECRET_ACME_PASS=...
h5i browser env                                  # names only
h5i browser type '#password' '$H5I_SECRET_ACME_PASS'
```

- Only `H5I_SECRET_*` is reachable. The rest of `H5I_*` is configuration, and a
  prefix allowlist fails closed where a denylist would not.
- The reply, the artifact and the action log carry the placeholder. A
  placeholder whose variable is unset is refused, not typed literally.
- Anything written back out passes `Secrets::redact`, which replaces the
  longest value first so a secret that contains another is not half-revealed.

A human login goes through `agent-browser dashboard start`, which keeps the
typed password in that browser.

## The session surface

A session has an opaque id (`br_7k2xqa`) that is in the record, in `--json` and
in receipts, and is never reused. People type names (`--session auth`), which
can be reused once their session has ended. A verb resolves its session from
`--session`, then `$H5I_BROWSER_SESSION`, then the default.

| concept | agent-facing |
|---|---|
| session | yes, usually implicitly: `open` sets the default |
| session name | yes, for running several at once |
| session id | no; durable reference in `--json` and receipts |
| box | yes, as a placement, never part of a session's definition |
| proxy port, Chromium process, CDP | no |

Two rules keep the next verb from moving under an agent:

- No "if only one session is live, use it". That silently redirects the next
  verb the moment a second session exists.
- The default outlives the session it names, so the next bare verb can say "the
  session you were on was closed" rather than "no session is open".

Sessions end by having their ending written into the record, not by deletion.
The on-disk layout under `$XDG_STATE_HOME/h5i/browser` is a contract; see
[`MANUAL.md`](../MANUAL.md#a-browser-sessions-directory).

The control lock (`crates/h5i-core/src/control.rs`) lets a human take a session
from the agent; after `release`, the agent must re-snapshot before its next page
action. The viewers' side of the lock is in
[`design-interminal-browser.md`](design-interminal-browser.md) V6.

## Security boundary

- The lane is `proxy-observed`. agent-browser and Chromium run as ordinary host
  processes; h5i observes and gates their network traffic and does not confine
  them. A confined browser needs a box.
- The proxy decrypts HTTPS with a per-session CA. The CA and its key are written
  `0600`; the key signs every certificate the proxy presents, so it is
  credential material.
- The message store and `dom-report.jsonl` hold target content and credentials
  and are owner-only.
- The DOM instrument's report beacon is consumed by the proxy before policy and
  before recording, and its body is capped at 1 MiB.
- Capture stops if someone drives agent-browser outside h5i: its daemon
  restarts when idle and a bare `agent-browser open` starts an unproxied
  browser. `h5i browser proxy` prints the flags and `env` to keep a shell
  proxied, and `status` warns after five minutes with no target request.
- Where Chromium's sandbox cannot start, `AGENT_BROWSER_ARGS=--no-sandbox`
  turns it off; use it only for a trusted target.
- The control lock is advisory on the host: it pauses `h5i browser` and nothing
  else.

## Limits

- No page engine of h5i's own. Page semantics, script and rendering are
  Chromium's.
- The proxy path has no address-level rebinding check and no spending budget
  (see Policy).
- The proxy does not see traffic from a browser that is not pointed at it.
