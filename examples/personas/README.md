# Example agent personas

A **persona** is an optional markdown file that gives the agent in an `h5i box`
a standing working style: a small "Dockerfile for behavior". You declare it per
profile in `.h5i/env.toml`, and at `h5i box create` the listed files are
concatenated, in declared order, into a single `PERSONA.md` at the worktree
root.

```toml
# .h5i/env.toml
[profile.architect]
isolation = "process"
persona = ["examples/personas/architect.md"]   # one or more, concatenated in order
```

```bash
h5i box create auth-fix --profile architect     # PERSONA.md baked here
```

These files are **examples**, not a fixed menu. Nothing enforces a role: a
persona only shapes how one agent works. Copy any of these, edit freely, or
write your own.

- [`architect.md`](architect.md): design/structure first, minimal surface.
- [`implementer.md`](implementer.md): complete, tested, idiomatic change.
- [`reviewer.md`](reviewer.md): correctness/risk first, verify claims.

Notes:

- **h5i bakes the file; it does not wire it in.** Nothing writes `@PERSONA.md`
  into a `CLAUDE.md` or an instruction into `AGENTS.md` for you. Point your
  agent at `PERSONA.md` yourself, or reference it from a file the runtime
  already loads.
- Sources are read **from inside the worktree**, so they must be committed at
  the base revision. Paths are relative to `$WORK` and may not contain `..`; a
  missing source fails `box create` rather than launching an agent with a
  silently empty persona.
- Omit `persona` from the profile for an agent with no standing style.
- List several files to compose a style, e.g. a role brief plus a house-rules
  file.
- The baked persona's sha256 is pinned in the box manifest (`persona_digest`).
- `h5i box create` overwrites `PERSONA.md`, and adds it to the worktree's git
  exclude file so it never shows in `box diff` or reaches a commit. Inspect it
  directly at the worktree root.
