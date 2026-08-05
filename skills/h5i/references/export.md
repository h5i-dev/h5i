# The output gate

```bash
h5i dev export <name> [--out <dir>] [--force]
```

Writes three files, after freezing the box with a mediated commit:

| File | What it is |
| --- | --- |
| `patch.diff` | the tree diff against the pinned base, path-validated |
| `report.md` | what the box was, what changed, every command that ran |
| `receipt.json` | the machine-readable records, with the enforced policy digest |

`--force` is required to replace a non-empty output directory: an export is
evidence, and silently replacing one is how evidence goes missing.

## Reading a receipt

Each record carries:

- `source` — which lane observed it. `host-env-run` is host-observed;
  `tee-shim` and `inbox-capture` are recorded inside the box. Keep the
  distinction in mind when you weigh the evidence.
- `exit_code`, `timed_out`, `wall_ms`, `cpu_ms`, `max_rss_kb`.
- `egress` — allowed/denied counts per host, from the proxy, not from the box.
- `redactions` — secret rules that fired, by rule id.
- `raw_oid` / `raw_size` / `raw_truncated` — the stored payload and whether it
  was capped.

Two things worth checking before applying a patch: a non-zero `egress.denied`
(the box tried to reach something the policy refused) and any `redactions` (a
credential passed through a command line or output).

An agent inside a box can add records but cannot rewrite ones already written.
It can also stop writing: a gap between host-observed process exits and
box-reported commands is itself a finding. We claim that much and no more.
