# Example team personas

A **persona** is an optional markdown file that gives one `h5i team` agent a
standing working style — think of it as a small "Dockerfile for behavior". It is
injected near the top of that agent's launch prompt, ahead of the task.

```bash
h5i team add-env myteam env/claude/auth-fix --persona examples/personas/architect.md
```

These files are **examples**, not a fixed menu. Roles are not enforced: by
default every team member is an independent peer that implements the task and
peer-reviews the others. Copy any of these, edit freely, or write your own —
whatever shapes the agent the way you want.

- [`architect.md`](architect.md) — design/structure first, minimal surface.
- [`implementer.md`](implementer.md) — complete, tested, idiomatic change.
- [`reviewer.md`](reviewer.md) — correctness/risk first, verify claims.

Notes:
- Omit `--persona` for a plain peer (no standing style).
- `--as <name>` is optional too; omit it and h5i assigns a ref-safe name.
- Inspect what an agent will see with `h5i team persona <name> --team <team>`.
