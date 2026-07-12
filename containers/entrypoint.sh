#!/bin/sh
# h5i agent-in-box entrypoint (shared by the agent-claude / agent-codex images).
#
# h5i's container backend runs a read-only rootfs and forwards the HOST's
# $HOME path into the box — a path that does not exist (or is not writable)
# in this image. Claude / Codex must be able to write their state (~/.claude,
# ~/.claude.json, ~/.codex), so repoint HOME at the box's private /tmp tmpfs.
# This state is deliberately ephemeral: credentials come from h5i's host-side
# credential-injecting proxy (base-URL override + per-run dummy token), never
# from a login persisted inside the box.
HOME=/tmp/agent-home
export HOME
mkdir -p "$HOME"

# Container-standard bin dirs first: the forwarded host PATH is ordered for
# the host's filesystem, not this image's.
PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:$PATH"
export PATH

# In-box git identity, so `git commit` on the env branch works out of the box.
# Fills the gap only (the fresh tmpfs HOME has no ~/.gitconfig); the mediated
# propose/apply path re-validates everything host-side regardless.
if ! git config user.email >/dev/null 2>&1; then
    git config --global user.name "${USER:-h5i-agent} (h5i box)" 2>/dev/null || true
    git config --global user.email "${USER:-h5i-agent}@h5i.invalid" 2>/dev/null || true
fi

# `h5i env shell` / `env run` always pass an argv; a bare `podman run` of this
# image (no args) still deserves a usable shell.
[ "$#" -gt 0 ] || set -- /bin/bash
exec "$@"
