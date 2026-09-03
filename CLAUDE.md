# CLAUDE.md

## This workspace's debug build is very large

Large enough that it has filled a disk. The dependency graph carries a browser
engine and a patched JavaScript engine, so `target/debug` runs to tens of
gigabytes where `target/release` is a fraction of it.

That is a fact about the workspace and it applies to anyone who builds it. What
follows from it depends on the machine, so it is not a repository rule: on a
roomy box a dev build is ordinary, and CI builds one on purpose
(`.github/workflows/test.yaml` runs on throwaway runners, so do not add
`--release` there).

If your machine cannot spare the space, this repository ships two opt-in
guards that make a release-only checkout stick:

1. `scripts/no-debug-guard.sh on` writes a regular file at `target/debug`, so
   cargo stops with "failed to create directory ... File exists" before it
   compiles anything. `off` and `status` do what they say. The file lives under
   `target/`, which is ignored, so it never leaves your checkout.
2. `scripts/deny-debug-build.py` is a Claude Code PreToolUse hook that refuses
   dev-profile cargo commands, target-directory redirection, and attempts to
   remove the guard. It is a script, not a setting: nothing wires it up until
   you add it to your own `.claude/settings.local.json`, which is gitignored.

   ```json
   { "hooks": { "PreToolUse": [ { "matcher": "Bash", "hooks": [
     { "type": "command",
       "command": "python3 \"$CLAUDE_PROJECT_DIR/scripts/deny-debug-build.py\"" } ] } ] } }
   ```

With both on, every cargo invocation takes `--release`, the binary is
`./target/release/h5i`, and scripts that need it default to that path. Do not
work around a "File exists" failure on `target/debug`: turn the guard off
deliberately, or build release.
