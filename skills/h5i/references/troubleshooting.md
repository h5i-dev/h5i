# Troubleshooting

## `h5i box probe`

Reports what this host can enforce: Landlock ABI, user namespaces, seccomp, and
rootless Podman. Present bits do not mean confinement works, so h5i runs a
functional self-test before it lets a box claim a tier. `create` can still fail
on a host whose bits look fine, with AppArmor-restricted user namespaces on CI
the usual cause.

## Common failures

`cannot enforce the built-in 'agent' profile`: the host has no supervised or
container tier, so API egress cannot be scoped. The box was created with
`default` instead, and coding agents will not run in it. Install rootless
Podman, or accept a box that can only build and test.

A command works on the host and fails in the box: read the error, which names
the path or host. Usually a path outside `$WORK`, a host not in `net.egress`, or
a program not in the profile's `tools` allowlist.

`git` fails inside the box: the in-box git surface is narrow on purpose, holding
the worktree's own admin dir, objects and ref namespace. `refs/h5i/env` meta,
hooks and the policy directory are sealed, because a box that could rewrite its
own manifest could widen its own policy.

A box is "busy": another read-write session holds its lock, and `h5i box status`
shows which. Teardown refuses while a read-only observer is attached.

## Interactive sessions

`h5i box shell` keeps the controlling tty, so job control and TUIs work, and has
no wall-clock kill. It uses a generated plain rcfile instead of your `~/.bashrc`
or `~/.zshrc`, which under confinement routinely call tools the sandbox blocks.
Pin your own with `[shell] rcfile = "<path-relative-to-$WORK>"` in the profile
(bash sources it directly, zsh from the generated rc).

Under zsh the session gets its own `$ZDOTDIR` and `$HISTFILE` inside the box,
and history persists per box under `<env>/shell/history/`. Your real
`~/.zsh_history` is outside every grant, so a session pointed at it prints
`locking failed for ~/.zsh_history: operation not permitted` at startup and
after every command.

Project config directories (`$WORK/.claude`, `$WORK/.codex`) are mounted
read-only during an interactive session, and the user settings files are pinned
read-only as single files, so an agent cannot switch off its own observation.
