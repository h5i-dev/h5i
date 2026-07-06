# h5i env Model Pinning — design

Status: proposed (2026-07-06). Not implemented. Captures the enforcement
analysis behind pinning a fixed model (e.g. `claude-haiku-4-5`) inside an `h5i
env` box so the in-box agent cannot switch it.

## Problem

An operator may want an env locked to one model — cost control (force Haiku for
cheap fan-out), reproducibility, or policy. Today `h5i env` has **no** model
control: the profile can allowlist env-var *keys* (`env.pass`) but cannot *set* a
value, the inject-value path (`injected_env`) is wired only to the secrets
broker, and the managed-settings injection carries only the wrap-bash hook. So
nothing stops an in-box agent from choosing any model its credentials allow.

## What "enforceable" means per runtime

The pin is only as strong as the weakest place a model can be selected. For
**Claude Code** the authoritative levers (verified against the docs) are:

- Managed settings are the **highest** precedence and cannot be overridden.
- `availableModels: ["<id>"]` + `enforceAvailableModels: true` + `model: "<id>"`
  makes it a hard restriction:
  - `/model` in-session → rejected ("restricted by your organization's settings")
  - `--model` flag / `ANTHROPIC_MODEL` → replaced at startup, session begins on
    the allowed model
  - the `/model` picker hides excluded models
- `ANTHROPIC_MODEL` alone is **weak** (replaced at startup, and an in-box
  subprocess can re-set it), so env-var injection is at best belt-and-suspenders.

The subprocess-escape concern ("a fresh session re-evaluates settings") is
**neutralized when the enforcement is a fixed-path root-owned file**: every
Claude session started in the box — subprocess or not — reads the same
`/etc/claude-code/managed-settings.json`, so re-evaluation still hits the
enforced allowlist. The in-box agent can't write that root-owned path.

Non-Claude-Code paths (a raw `curl` to the API, another tool) are outside Claude
settings entirely and remain bounded only by `net.egress` + `tools`.

Codex has no managed-settings equivalent (managed-settings injection is skipped
for the Codex profile); a Codex pin would go through `.codex/config.toml` +
config-lock and needs its own enforcement-semantics verification. Out of scope
for this memo.

## Tier analysis (the mount question)

The hard Claude pin needs a root-owned, read-only file at the fixed managed path
`/etc/claude-code/managed-settings.json`.

- **container** — airtight, zero host setup. Podman gives the box a private
  overlay rootfs, so the nested target is auto-created on the overlay and bound
  read-only at the identical path (`container::prepare_managed_settings`,
  mounted `,ro`). This is where h5i already injects the wrap-bash managed file.

- **process / supervised (kernel tiers)** — the box shares the host's **real
  `/etc`**. `MS_BIND` requires the target to **already exist**, and a rootless
  h5i cannot `mkdir /etc/claude-code` (real-root-owned `/etc`; uid-0 *inside* the
  userns maps back to the unprivileged real uid on the host fs → `EPERM`). So the
  clean container trick is unavailable, and h5i degrades to a **soft** pin:
  config-lock the user-scope `settings.json` (which *does* exist) read-only +
  inject `ANTHROPIC_MODEL` — user-scope precedence, not the unbreakable
  `enforceAvailableModels` guarantee.

Note the capability itself is present on supervised: config-lock already forces
`CLONE_NEWNS` there (supervised is otherwise pidns=false), and the kernel-tier
`pre_exec` already `MS_BIND`s over an `/etc` file (`/etc/hosts` for egress DNS
pinning, `sandbox.rs`) and bind+remount-RO's the config-lock paths. The **only**
missing ingredient on supervised is a pre-existing mountpoint to bind over.

## Roadmap — Option 1: host-provisioned managed mountpoint (supervised hard pin)

Make supervised as airtight as container by having the **host** supply the one
thing a rootless h5i can't create: the mountpoint.

1. **One-time host provisioning (root, out of band):** create an empty file
   `/etc/claude-code/managed-settings.json` (mode `0644`, root-owned). This is
   the whole privileged step; h5i never needs root at runtime.
2. **Per-env managed file:** on `env create`/`shell`/`run` under the supervised
   (or process) tier, h5i writes its computed managed-settings JSON — carrying
   the wrap-bash hook **and** the model keys (`model`, `availableModels`,
   `enforceAvailableModels`) — to a per-env file outside `$WORK`.
3. **Bind read-only in the box's mount ns:** in `pre_exec`, `MS_BIND` the per-env
   file over the pre-provisioned `/etc/claude-code/managed-settings.json`, then
   `MS_BIND|MS_REMOUNT|MS_RDONLY` — mechanically identical to the existing
   `/etc/hosts` bind and the config-lock RO remount. Contained by the userns, so
   the host file is untouched; unremovable in-box (`mount`/`umount2` are
   seccomp-denied).
4. **Detection + graceful fallback:** if the mountpoint is absent, h5i does **not**
   fail — it falls back to today's soft pin and surfaces a one-line note
   ("supervised model pin is soft: run `sudo install -m0644 /dev/null
   /etc/claude-code/managed-settings.json` once for a hard pin"). No silent
   downgrade.

Why this is acceptable despite the host dependency: it is a **single idempotent
root action per host**, not per-env or per-run; it mirrors how real fleets
deploy Claude Code managed settings anyway; and it fails open to the existing
soft pin, so nothing regresses on an unprovisioned host.

## Rejected — Option 2: overlay the whole `/etc`

Mount an overlay at `/etc` (lowerdir = real `/etc`, upperdir = tmpfs carrying the
managed file) inside the userns+mountns so the file appears with no host change.
Doable, but **fragile**: a wrong overlay shadows `passwd`/`resolv.conf`/CA certs
the box needs, and it enlarges the in-box mount surface for little gain over
Option 1. Not pursued.

## Policy surface (shared by all tiers)

- New profile field, e.g. `[profile.X] model = "claude-haiku-4-5"` (or
  per-runtime `[profile.X.model] claude = … codex = …`). `env.set`-style value
  injection could generalize this, but a typed `model` field keeps validation
  and digesting clean.
- Extend `hooks::managed_settings_wrap_bash_json()` to also emit the model keys
  when a pin is set (container + provisioned-supervised).
- Inject `ANTHROPIC_MODEL` via `injected_env` on all tiers as a secondary signal.
- **Fold the pin into `policy_digest`** so it is tamper-evident and travels in
  the env manifest (unlike the managed-settings file and `box_git`, which are
  deliberately not serialized/digested — those are box-plumbing, whereas the
  model pin is a policy *claim* a reviewer must be able to verify).

## Enforcement summary

| Tier | Claude model pin | Host setup |
|------|------------------|------------|
| container | hard (`enforceAvailableModels` via RO managed mount) | none |
| supervised | hard **iff** mountpoint provisioned (Option 1); else soft | one-time root file |
| process | same as supervised | one-time root file |
| workspace | none (no confinement) | — |

Non-Claude egress (raw API calls, other tools) is out of scope for the model pin
and remains governed by `net.egress` + `tools`.
