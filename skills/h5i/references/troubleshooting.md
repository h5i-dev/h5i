# Troubleshooting

## `h5i box probe`

Reports what this host can enforce: Landlock ABI, user namespaces, seccomp, and
rootless Podman. Bits being present is not the same as confinement working —
h5i also runs a functional self-test before it lets a box claim a tier, so
`create` can fail on a host whose bits look fine (AppArmor-restricted user
namespaces on CI is the classic case).

## Common failures

**"cannot enforce the built-in 'agent' profile"** — the host has no supervised
or container tier, so API egress cannot be scoped. The box was created with
`default` instead and coding agents will not run in it. Install rootless Podman
or accept a build/test-only box.

**A command works on the host and fails in the box** — read the error, it names
the path or host. Usually one of: a path outside `$WORK`, a host not in
`net.egress`, or a program not in the profile's `tools` allowlist.

**`git` fails inside the box** — the in-box git surface is deliberately narrow
(the worktree's own admin dir, objects, its own ref namespace). Reading
`refs/h5i/env` meta, hooks and the policy directory is sealed: a box that could
rewrite its own manifest could widen its own policy.

**A box is "busy"** — another read-write session holds its lock. `h5i box
status` shows which. Teardown refuses while a read-only observer is attached.

## Interactive sessions

`h5i box shell` keeps the controlling tty (job control and TUIs work) and has no
wall-clock kill. It uses a generated plain rcfile rather than your `~/.bashrc`,
which under confinement routinely calls tools the sandbox blocks. Pin your own
with `[shell] rcfile = "<path-relative-to-$WORK>"` in the profile.

Project config directories (`$WORK/.claude`, `$WORK/.codex`) are mounted
read-only during an interactive session, and the user settings files are pinned
read-only as single files. This is deliberate: an agent must not be able to
switch off its own observation.
