# CLAUDE.md

## Never build this workspace in debug mode

A dev-profile build of this workspace is very large. It is prohibited.
Every cargo invocation here passes `--release`:

```
cargo build --release --workspace
cargo test --release --workspace
cargo clippy --release --workspace --all-targets -- -D warnings
```

The binary is `./target/release/h5i`. Scripts that need it default to that
path. Do not point them back at `./target/debug/h5i`.

This is enforced, not just requested:

1. `target/debug` is a regular file, not a directory, so cargo stops with
   "failed to create directory ... File exists" before it compiles anything.
   Managed by `scripts/no-debug-guard.sh` (on, off, status).
2. A PreToolUse hook, `.claude/hooks/deny-debug-build.py`, refuses dev-profile
   cargo commands, target-directory redirection, and attempts to remove the
   guard.

Do not disable the guard, do not redirect the cargo target directory, and do
not work around a "File exists" failure on `target/debug`. If a task looks
like it needs a dev build, stop and say so. Lifting the guard is a
maintainer call.

CI is exempt. `.github/workflows/test.yaml` runs on throwaway runners and
builds dev on purpose. Do not add `--release` there.
