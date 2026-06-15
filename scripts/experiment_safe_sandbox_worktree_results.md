# Safe Sandbox-Style Worktree Experiment

Run:

```bash
H5I_BIN=target/debug/h5i WORKDIR=/tmp/h5i-safe-worktree-smoke \
  ./scripts/experiment_safe_sandbox_worktree.sh
```

What it tests:

- Plain `git worktree`: an agent running in the worktree writes directly to the
  parent checkout by absolute path.
- `h5i env`: the same write is run through an env created with
  `--isolation process --audit all`.

Observed on this host:

```text
PASS: plain worktree command modified parent checkout: /tmp/h5i-safe-worktree-smoke/repo/protected.txt
PASS: h5i env preserved parent checkout: /tmp/h5i-safe-worktree-smoke/repo/protected.txt
PASS: h5i env denied the jailbreak attempt (exit 2)
```

The denied `h5i env` command reported:

```text
sh: 1: cannot create /tmp/h5i-safe-worktree-smoke/repo/protected.txt: Permission denied
◈  evidence f56fade2e94fc216 (env env/experiment/safe, policy d332df40ec92)
```

Interpretation:

`git worktree` is a convenient checkout mechanism, not a sandbox boundary.
`h5i env` keeps the worktree ergonomics but adds a pinned policy, kernel
confinement, and command evidence, so it can be described as a safe
sandbox-style worktree when the requested isolation tier is available.
