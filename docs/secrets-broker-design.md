# h5i Secrets Broker — design

Status: implemented (phase 1). Supersedes the fail-closed stub in
`sandbox::validate_profile` that refused any non-empty `secrets`.

## Problem

Today `secrets` fails closed: a profile that lists any secret is refused, so an
env can be given **no** credential safely. That blocks every real workflow — a
private `git clone`, a branch push, an authenticated API call. An agent that
needs a token today must leak a full one through `env.pass` (inherited into the
child's environment, visible in `/proc/<pid>/environ`, and easy to echo into a
capture). The broker replaces that with **capability-scoped, audited, redacted,
fail-closed** credential delivery.

## Principles (Codex-reviewed)

1. **Capability-scoped.** A grant is an explicit, named request in the profile.
   Nothing is ambient; nothing is inherited wholesale.
2. **Audit-only-by-default.** Every delivery records the grant **id**, source,
   injection method, TTL, and a value **fingerprint** (sha256 prefix) — never
   the value. The record lands in the env event log (`refs/h5i/env`).
3. **Prefer file/fd over env.** Default injection writes the secret to a `0600`
   file **outside** `$WORK` (so it is never committed) and points the child at
   it via `<NAME>_FILE`. Env-var injection is opt-in.
4. **Redacted from evidence.** The resolved value is scrubbed from the capture
   (exact-match), in addition to h5i's existing pattern-based secret redaction,
   so a token echoed to stdout cannot land in `refs/h5i/objects`.
5. **Fail closed.** If a declared grant cannot be resolved or delivered, the run
   **refuses** — never runs with the credential silently absent.
6. **Ephemeral.** The injected file lives only for the run; it is unlinked when
   the run ends (a Drop guard, even on error/panic). TTL bounds validity for
   sources that mint/derive a credential (recorded; enforced where the source
   supports it).

## Schema (`.h5i/env.toml`)

Simple form — names with defaults:

```toml
[profile.default]
secrets = ["GITHUB_TOKEN"]
```

Rich form — per-grant config (each table also implicitly grants the name):

```toml
[profile.default.secret.GITHUB_TOKEN]
source = "env:GH_PAT"     # env:VAR | file:/abs/path   (default: env:H5I_SECRET_<NAME>)
inject = "file"           # file | env                  (default: file)
ttl    = "1h"             # recorded; advisory for static host sources
```

`source`/`inject`/`ttl` are part of the **resolved policy** (and therefore the
policy digest) — they are config, not values, so a tampered source is detected.
Values never touch the policy, the manifest, or any git ref.

## Resolution sources (host-side, never committed)

- `env:VAR` — host environment variable `VAR`.
- `file:/abs/path` — contents of a host file (trailing newline trimmed).
- default (no `source`) — host env `H5I_SECRET_<NAME>`.

A source that resolves to empty/missing is a **fail-closed** error.

## Injection

- `inject = "file"` (default): write value to
  `<env-dir>/secrets/<NAME>` (mode `0600`, outside `$WORK`), set child env
  `<NAME>_FILE=<path>`. The file is unlinked when the run ends.
- `inject = "env"`: set child env `<NAME>=<value>` directly (subject to the same
  no-wholesale-inherit rule — it is added to the child only, never the host).

Injected env vars are applied **after** the `env.pass` allowlist, so a secret is
never confused with a passed-through host var.

## Provenance event

One `secret` event per delivered grant:

```
grant=GITHUB_TOKEN source=env:GH_PAT inject=file ttl=1h fp=sha256:ab12… 
```

The fingerprint lets a reviewer confirm "the same token was used across these
runs" without ever seeing it.

## Integration points

- `sandbox::validate_profile` — now accepts `secrets`/`secret` grants (validates
  names + source/inject syntax) instead of refusing them.
- `sandbox::Profile` — gains `secret_grants: Vec<SecretGrant>` (config only),
  merged from the simple `secrets` list and the rich `[secret.<name>]` tables.
- `secrets_broker::resolve` — host-side; returns injections + tempfile guard +
  values-to-redact + audit records. Pure resolution logic is unit-tested.
- `env::run` — resolves grants before the run, threads injections into
  `sandbox::run`, scrubs the values from the raw output before capture, emits a
  `secret` event per grant, and drops the tempfile guard after.
- `sandbox::run` / each tier — accepts an `injected_env: &[(String,String)]`
  applied to the child.

## Threat model / limits (honest)

- The broker reduces credential exposure to **the run's child process tree** and
  scrubs evidence; it does **not** stop a process that *uses* the credential
  legitimately from sending it somewhere — that is the egress layer's job
  (container/supervisor tier).
- `inject = "env"` is visible in the child's `/proc/self/environ`; `file` is
  preferred for that reason.
- TTL is enforced only where the source mints a time-bounded credential; for a
  static `env:`/`file:` source it is recorded, not enforced.
