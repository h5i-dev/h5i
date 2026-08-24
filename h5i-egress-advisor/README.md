# h5i-egress-advisor

Read an [h5i](https://github.com/h5i-dev/h5i) box's receipts, find the
destinations its egress allowlist refused, and turn each one into a question a
human can answer:

> `registry.npmjs.org:443` was refused 11 times across two runs. Do you want to
> allow it?

A refusal already shows up as a count in `h5i box inspect` and a red row in
`h5i ui`. Deciding what to do about one means reading the receipt, working out
which host it was, and writing the `h5i box allow` line yourself. That last step
is mechanical, and it is the one people skip — so the allowlist drifts out of
date and the box stays broken in a way nobody quite gets round to fixing.

This tool does the mechanical step and stops there. **It prints; it never
applies.** Widening a boundary is a decision, and the decision is yours.

## Install

```bash
cargo install --git https://github.com/AniketR10/h5i-egress-advisor
# or, from a clone
cargo install --path .
```

No runtime dependency on h5i itself — it reads the files h5i already wrote.

## Use

```bash
h5i box export mybox --out ./review
h5i-egress-advisor ./review/receipt.json     # an export bundle
h5i-egress-advisor --box mybox               # or the live box's receipt.jsonl
h5i-egress-advisor --box mybox --json        # the same report, for a script
h5i-egress-advisor --box mybox --toml        # a block for .h5i/env.toml
```

The positional argument takes any of the three shapes you might have: an export
bundle (`receipt.json`), a live append-only log (`receipt.jsonl`), or the
directory holding either. `--box` finds the log itself, in the store h5i keeps
under the repository's Git common directory (`.git/.h5i/env/<agent>/<slug>`);
the name is a slug (`mybox`) or the full `<agent>/<slug>`.

### What it prints

```text
receipt: ./review/receipt.json
box env/claude/fix-auth · profile review · container tier

14 refused across 2 of 2 run(s) with egress verdicts, at 4 destination(s):

registry.npmjs.org:443             11 refused   in 2 runs (a3f1c2, 9b04de)
    while running: npm install
    a package registry or source host — the ordinary reason a build reaches out
    h5i box allow registry.npmjs.org

203.0.113.7:8443                    1 refused   in 1 run (9b04de)
    while running: npm test
    no suggestion: a bare address names nothing you can review — find out what it is first

cache.internal.example.com:6379     1 refused   in 1 run (9b04de)
    while running: npm test
    not a host this tool recognises, and :6379 is not HTTP — check the command that wanted it
    h5i box allow cache.internal.example.com:6379

telemetry.example.net:443           1 refused   in 1 run (a3f1c2)
    while running: npm install
    no suggestion: this looks like a beacon, not a dependency (the 'telemetry' label)

Nothing above has been applied: these are lines to read, decide on, and paste.
`h5i box allow` takes effect at the next run, and only on a profile that already
sets `net.egress` — it never widens a deny-all one.
```

Each destination carries the three things the decision actually needs: how often
it was refused, which runs did it (the ids are the prefixes `h5i box inspect
--capture <id>` resolves), and the command that was running at the time.

### `--toml`, for the boxes `h5i box allow` cannot reach

`h5i box allow` is a host-side list merged in by the proxy tiers — `container`
and `microvm` — and only into a profile that already scopes `net.egress`. For a
`supervised` or `process` box the allowlist lives in the policy, so the answer
is a profile edit rather than a shorter command. The tool reads the receipt's
isolation claim and says which situation you are in; `--toml` then writes the
block:

```toml
# h5i-egress-advisor — candidates from ./review/receipt.json
# 14 refusal(s) at 4 destination(s), across 2 of 2 run(s) with egress verdicts.
#
# Every line here widens a boundary. Read it, delete what you do not need, and keep
# what is left narrow. This block sets `egress` for the profile rather than adding to
# it: merge it with what the profile already lists. A box's policy is resolved when the
# box is created, so create a new box after editing.

[profile.review.net]
egress = [
  "registry.npmjs.org",  # 11 refused in 2 runs (a3f1c2, 9b04de)
  "cache.internal.example.com:6379",  # 1 refused in 1 run (9b04de)
]

# Refused, and deliberately not suggested:
#   203.0.113.7:8443 — 1 refused: a bare address names nothing you can review — find out what it is first
#   telemetry.example.net:443 — 1 refused: this looks like a beacon, not a dependency (the 'telemetry' label)
```

The profile name comes from the receipt (or `--profile`); a receipt that names
none gets a `PROFILE` placeholder rather than a guess. Destinations that earned
no suggestion are listed underneath as comments, so nothing the tool saw
disappears just because it declined to recommend it.

### `--json`

```json
{
  "tool": "h5i-egress-advisor",
  "schema": 1,
  "box": { "env_id": "env/claude/fix-auth", "profile": "review",
           "isolation_claim": "container", "allow_reach": "proxy" },
  "totals": { "destinations": 4, "denied": 14, "runs_with_egress": 2, "runs_refused": 2 },
  "destinations": [
    { "host": "registry.npmjs.org", "port": 443, "denied": 11, "allowed": 0,
      "runs": ["a3f1c2", "9b04de"], "example_cmd": "npm install",
      "suggestion": { "kind": "allow", "rule": "registry.npmjs.org",
                      "command": "h5i box allow registry.npmjs.org",
                      "why": "a package registry or source host — …" } }
  ],
  "warnings": []
}
```

Both suggestion shapes carry every key: a declined destination has
`"kind": "no-suggestion"` with `rule` and `command` set to `null`, so a consumer
never has to tell a null apart from a missing key.

### Options and exit status

```text
--box <NAME>       read a box's log; <NAME> is a slug or <agent>/<slug>
--root <DIR>       the h5i store to look in (default: this repository's .git/.h5i)
--json             machine-readable report on stdout
--toml             a [profile.X.net] block for .h5i/env.toml
--profile <NAME>   the profile name to write in --toml output
--min <N>          only report destinations refused at least N times
--no-color         never colorize (NO_COLOR is honoured too)
```

| Code | Meaning |
|---|---|
| `0` | read the receipt, nothing was refused |
| `1` | read the receipt, refusals reported |
| `2` | could not read a receipt |

So `h5i-egress-advisor --box mybox >/dev/null || echo "check the allowlist"`
works in a hook, and a broken path is distinguishable from a clean box.

## How it decides

Every refused destination gets one of two answers, and the tool says which and
why:

- **An allowlist line.** The default. Known registries and source forges
  (`registry.npmjs.org`, `pypi.org`, `crates.io`, `github.com`, image
  registries, OS package mirrors …) are labelled as the ordinary reason a build
  reaches out. A host it does not recognise still gets a line — "I have not
  heard of this" is not evidence of anything, and you are the one who knows what
  you ran — but it is labelled as unrecognised, next to the command that wanted
  it. Ports outside 80/443 stay in the rule (`host:6379`), because a non-web
  port is exactly where you want the entry to be narrow.
- **No line, and a reason.** Known telemetry endpoints and hosts with a
  `telemetry`/`analytics`/`beacon`/`tracking` label are beacons, not
  dependencies; a bare IP address names nothing you can review. The tool refuses
  to write the line rather than putting a warning next to one.

It also flags a destination that was *reached* in one run and refused in
another — usually a second name for the same service, or an entry missing its
port — and it never presents a clamped host list as the whole picture: when h5i
hit its per-record cap, that is printed as a note.

## What it will not do

- Run `h5i`, or anything else. It reads files and writes to stdout.
- Write to the store it read from. Receipts are evidence; this tool opens them
  read-only and creates nothing.
- Decide for you. A beacon that really is your own metrics service is a line you
  can write yourself, knowing why.

Two limits worth knowing. The tool can see the *isolation tier* in a receipt but
not whether the profile already scopes `net.egress`, so it states the condition
rather than promising `h5i box allow` will take effect. And a receipt is a record
of what was refused, never of what a box needed: a destination it never tried is
not in here.

## Development

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

The tests run against receipts shaped like h5i's own, in `tests/fixtures` — an
export bundle and a live log with a torn tail line — plus a store built in the
layout h5i uses, because "find my box" is the half of this that has nothing to
do with parsing.

The receipt model in `src/receipt.rs` is deliberately a *subset* of h5i's, with
every field optional: a receipt written by a newer h5i than this was built
against still parses, and an unknown field is carried past rather than fatal.

## License

MIT — see [LICENSE](LICENSE).
