# Example team personas

A **persona** is an optional markdown file that gives an `h5i env` agent a
standing working style — think of it as a small "Dockerfile for behavior". You
declare it **per profile** in `.h5i/env.toml`; at `h5i env create` the listed
files are concatenated into a git-ignored `PERSONA.md` at the worktree root,
which the agent loads automatically (`@PERSONA.md` in `CLAUDE.md` for Claude, a
read instruction in `AGENTS.md` for Codex).

```toml
# .h5i/env.toml
[profile.architect]
isolation = "process"
persona = ["examples/personas/architect.md"]   # one or more, concatenated in order
```

```bash
h5i env create auth-fix --profile architect     # PERSONA.md baked here
h5i team add-env myteam env/claude/auth-fix --runtime claude   # inherits it
```

These files are **examples**, not a fixed menu. Roles are not enforced: by
default every team member is an independent peer that implements the task and
peer-reviews the others. Copy any of these, edit freely, or write your own —
whatever shapes the agent the way you want.

- [`architect.md`](architect.md) — design/structure first, minimal surface.
- [`implementer.md`](implementer.md) — complete, tested, idiomatic change.
- [`reviewer.md`](reviewer.md) — correctness/risk first, verify claims.

Notes:
- Omit `persona` from the profile for a plain peer (no standing style).
- List several files to compose a style (e.g. a role brief + a house-rules file).
- The baked persona's sha256 is pinned in the env manifest (`persona_digest`).
- `h5i env create` overwrites `PERSONA.md`; it is git-ignored, so it never shows
  in the agent's diff. Inspect it directly at the worktree root.
