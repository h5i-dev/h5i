# Design: flows and verdicts, one replay core with two owners

Status: proposed, 2026-09-24. This design does not add a third engine. It names
the line between the two multi-step engines h5i already ships, `websec sequence`
and `h5i test`, so they stop drifting apart, and it opens one seam between them:
a declarative verdict that is data, sits below the bash oracle, and is the only
form a shared or imported recipe is allowed to carry.

```text
one flow language  ->  three verdict sources  ->  two owners
  (send + edit +        none  | matcher |          agent (live)
   extract, shared)      (data)  oracle (code)      repository (CI)
```

## F1. What is being claimed

h5i has two ways to run an ordered, stateful HTTP flow, and they arrived from
opposite ends:

- `websec sequence` (design-websec.md, W11) runs steps in a live session, binds
  a value out of one response into the next, and stops on a failed binding. The
  verdict is nobody's job here: the agent reads the result and writes a
  `finding` if it concludes something. It is interactive, session-local, and its
  artifacts are the session's own message store.
- `h5i test` (design-test.md) replays a repository-owned flow and hands the
  evidence bundle to an external oracle, which returns pass, fail or error by
  exit code. It deliberately has no assertion language, because the repository
  owns the property being tested and h5i owns only execution and evidence.

Two questions kept coming back. Should there be a stateful assertion DSL on top
of `sequence`. And should a shared or imported attack recipe (the Nuclei case)
carry bash to decide its own verdict. This design answers both by refusing to
answer them as one. The flow language is one thing and should be shared. The
verdict has three sources with different trust models, and which source a flow
may use is the whole decision.

## F2. The distinction that matters: who owns the verdict

The two engines are not redundant and neither reduces to the other. The dividing
line is not "interactive versus CI". It is who is allowed to say pass or fail,
and what that makes the artifact.

| | `websec sequence` | `h5i test` |
|---|---|---|
| verdict owner | the agent, after the fact | the repository, via oracle |
| verdict form | a `finding` the agent writes | oracle exit code |
| artifact is | data (the flow), read live | code (the oracle) plus data |
| trust model | runs in this session only | runs the repo's own program |
| shareable | yes, it is data | no, it carries executable code |
| home | live engagement, exploration | committed CI regression |

Read the last two rows together. A `sequence` file is inert data: a list of
sends, edits and extractors. Handing it to someone else is handing them a
description of requests, which the receiver's own session, policy and receipts
still gate. An `h5i test` is that same data plus an oracle command, and the
oracle is an arbitrary program the runner executes. The oracle is exactly the
kind of thing an h5i box exists to contain. So a test is safe to run because you
wrote its oracle and committed it next to your code. It is not safe to download.

That asymmetry is the design. It is why the two engines coexist rather than
merge, and it is why the verdict, not the flow, is where the trust boundary
lives.

## F3. What they already share, and where they have drifted

Both engines describe a step the same way in spirit: send a named request as an
actor, apply the websec edit language (`set`, `unset` over `query.`, `json.`,
`header.`), save the response under a stable name, and extract bindings for
later steps. `h5i test` says so in as many words: a flow step "may apply the
existing websec edit language". This is the shared core, and it is real, not
aspirational.

The extractors are already aligned in code, which is worth stating because the
prose disagrees. `design-websec.md` W11 advertises `jsonpath:`, `regex:`, `css:`,
`header:` and `cookie:`, but that list was aspirational: the shipped
implementations match each other exactly.

- `sequence` (`src/cli/websec.rs`, `fn extract_one`): `regex:`, `json:`,
  `header:`, `status`.
- `test` (`crates/h5i-test/src/main.rs`, `fn extract`): `regex:`, `json:`,
  `header:`, `status`.

So the only work here is to keep the doc honest and to keep the two functions
from drifting later, not to reconcile them now. F7's importer relies on this: a
recipe written once must extract the same way in both engines, and today it does.

The rule going forward: the step language (send, edit, extract, save, actor,
`no_follow`) is one grammar, and neither engine gets a private extractor the
other cannot honor. Where they differ is above the step, at the verdict, which is
F4. The `css:` and `cookie:` extractors W11 imagined are unbuilt in both; when one
is added it is added to both, or it is not added.

## F4. Three verdict sources

A flow produces evidence. Turning evidence into pass or fail can happen in three
places, and the choice is not a convenience, it is the trust model of the whole
artifact.

1. **No verdict (the agent decides).** The flow runs, bindings resolve, the
   agent reads the responses and writes a `finding` if it concludes something.
   This is `sequence` today. Right for live exploration and for the logic and
   authorization holes that resist being written down (a negative coupon, a
   swapped order id): those stay the agent's call, made in a finding it signs.

2. **Declarative matcher (data decides).** A small, closed matcher language runs
   against a step's response and yields a boolean: status equals, word present,
   regex hits, and the boolean combinators over them. This is the Nuclei shape.
   It is data, so a flow that carries only this kind of verdict stays shareable
   and sandbox-safe: running it executes no code the recipe author chose, only
   requests the receiver's policy still gates. This source does not exist in h5i
   yet. F6 adds it, deliberately bounded.

3. **External oracle (code decides).** An arbitrary program reads the evidence
   bundle and returns an exit code. This is `h5i test` today. It is unboundedly
   expressive (`jq`, a diff, the application's own test client) and therefore
   unshareable: it is the repository's own code, committed beside the flow, run
   because its owner trusts it.

The three are a ladder of expressiveness bought with trust. Source 2 is the new
rung, and it exists precisely because sources 1 and 3 leave a gap: 1 needs a
human or agent in the loop, 3 needs you to own and trust a program, and neither
can be the verdict of a recipe a stranger publishes. Source 2 can, because it
decides nothing the receiver has not already agreed a matcher may decide.

## F5. Why not just put bash everywhere

The tempting simplification is: `h5i test` already avoids an assertion DSL by
shelling out to bash, so let every flow, including shared and imported ones,
carry a bash oracle and be done. This design rejects that, for one reason that is
central to what h5i sells.

h5i's guarantee is containment with evidence: a box that fails closed, an audit
that shows what ran. A shared recipe that carries bash is executable code that
crosses the trust boundary at download time. Making every recipe an oracle turns
the recipe library into a channel for arbitrary code, which is the exact threat
the box exists to contain. The product would be shipping the payload it promises
to fence.

So bash-as-oracle is kept, unchanged, for the case it was built for: a
repository testing its own code with a program it wrote and committed. It is not
extended to the shared or imported layer. That layer gets source 2, which is
data. This is not a limitation to apologize for. "Recipes are data; only your own
repo's oracles are code; the box enforces the line" is a sentence only h5i can
say, and it is the dual-use answer the pitch needs.

## F6. The declarative matcher, bounded on purpose

Source 2 is the one new language, and the failure mode is growing it into a
general one nobody can read or sandbox. It is bounded by construction.

A matcher is attached to a step and evaluates against that step's response:

```json
{
  "send": "req_probe",
  "expect": {
    "all": [
      {"status": 200},
      {"body": "regex:SQL syntax error near '(\\w+)'"},
      {"header": "content-type", "contains": "text/html"}
    ]
  }
}
```

The whole grammar is: leaf matchers (`status` equals, `body` word or `regex`,
`header` name plus `contains` or `regex`) and the combinators `all`, `any`,
`not`. That is the Nuclei matcher surface and nothing past it. There are no
variables the matcher writes, no control flow, no arithmetic, no calls out. A
step with an `expect` that fails is a failed step, and a flow's verdict is that
every `expect` held (source 2's pass) unless a step has none, in which case that
step contributes no verdict and the flow falls back to source 1 or 3.

The hard line, and the rule to hold in review: anything a matcher cannot say does
not go into the matcher. It goes to an oracle (if the flow is repository-owned)
or to the agent (if the flow is live). The moment `expect` grows a way to
compare two responses, or to compute, it has become an assertion language and
has left the shareable, sandbox-safe rung it was created to occupy. `h5i test`
already proves the escape hatch exists and works: complex verdicts have a home,
and it is not here.

Branching between steps (feature "if role is admin, do X") is the same trap seen
from the flow side. A bounded form is allowed: a step may be gated on a prior
step's `expect` (run this step only if that one matched), because that is still
data and still decidable without running anything the author chose. A general
conditional with expressions is not. When a flow needs real branching, it is an
exploration, and exploration is the agent's job expressed as a prompt, not the
recipe's job expressed as code. This keeps source 2 a description of a known
attack, which is what makes it a durable, replayable regression asset rather than
a script.

## F7. The Nuclei importer, and what it targets

Nuclei templates are the largest free corpus of source-2 verdicts in existence:
a request, a set of matchers, a boolean. They are also stateless by design, one
request and its matchers, which is the layer h5i is weakest at owning by hand and
strongest at absorbing.

The importer's target is an `h5i test` file, not a `websec sequence` file, and
the reason is structural: a Nuclei template describes a request from scratch
(a method and a `{{BaseURL}}`-relative path), which is what an `h5i test` request
template expresses and what `websec sequence` cannot, since a sequence step only
resends a message a session already captured. So `h5i websec import-nuclei
<template>` prints an `h5i.test/v1` file whose verdict is `expect`. Concretely:

- A template's `http` (or legacy `requests`) block becomes one request template
  and one flow step per entry. `{{BaseURL}}` and `{{RootURL}}` are stripped to a
  relative path; any other Nuclei variable in a path, header or body is refused,
  because h5i does not resolve it.
- A single `raw:` request is parsed into the same request template: leading
  `@directive` lines are skipped, the request line gives the method and path, the
  header block is carried (dropping `Host`, which the engine sets from the target,
  and `Content-Length`, which it recomputes), and the body after the blank line is
  carried whole. A `raw:` block with more than one request is refused (its
  per-response matcher semantics do not reduce to one verdict), as is an
  `unsafe: true` request (its exact malformed bytes are the point, and a request
  template normalises them).
- `matchers` map onto `expect`: `word` to a `body` substring, `status` to
  `status`, `regex` to `body: regex:`. `matchers-condition` maps to `all`/`any`
  (Nuclei's default is `or`), a matcher's own `condition` over its words or
  patterns likewise (default `or`), and `negative: true` to `not`. A `word` or
  `regex` matcher imports only on `part: body`, because Nuclei's other parts
  match a text block this grammar's name-and-value `header` leaf cannot stand in
  for.
- `regex` `extractors` map onto the shared `extract`, which is why the extractor
  prefixes must stay aligned (F3): an imported extractor names its target the
  same way a hand-written one does.
- Multi-request templates become a multi-step flow, which is the case Nuclei
  expresses awkwardly and a flow expresses naturally.

What the importer never emits is an oracle. A construct that cannot be expressed
as `expect` (a `dsl` or `binary` matcher, a multi-request or `unsafe` raw block,
an unmodelled variable, a non-`regex` extractor) is refused with a reason, not
lowered into a bash script, because a skipped template is a gap and a smuggled
oracle is a shared executable.

Measured against the public `nuclei-templates` corpus (2026-09, 11,640 http
templates), the importer produces a clean data-only test for about 28% of them.
The ceiling is not a defect in the importer: the bulk of the rest is `dsl`
matchers, `flow` blocks and payload variables, which are code or unresolved
input by construction, not data. The design's point is exactly that this line
is where it is, so the number is a description of how much of Nuclei is already
data, not a coverage target to chase by weakening the boundary. Every `expect` it builds is verified against the shared
grammar (F6) before it is written, so an import never emits a verdict the engines
would reject. The output is data: committable into `.h5i-tests/` as an
oracle-less test, and shareable.

## F8. The regression seam, and how the two engines connect

The one place the engines touch is regression, and the wiring already half
exists. `finding create --repro` accepts a sequence or an experiment file: the
agent's live conclusion can already point at the flow that reproduces it. The
seam is to let that flow graduate into `h5i test` without a rewrite.

- A `sequence` flow is a valid `h5i test` `flow` (same step grammar, F3). What a
  test adds is a verdict field.
- If the flow carries source-2 `expect` matchers, it becomes a test with no
  oracle: a shareable regression that any repo can run and any registry can
  distribute, because its verdict is data. This is the common case for imported
  and for straightforward attacks.
- If the property needs source 3, the repository writes an oracle and commits it.
  This is the case that stays local and is never shared.

So the lifecycle is: explore with `sequence` (verdict source 1), the agent
concludes and files a `finding` with a `--repro` flow, and that flow graduates
into `.h5i-tests/` with either an `expect` (shareable) or an oracle (local). The
"world where every piece of software is attacked before it ships" is this loop
run in CI, and it is built from parts that already exist plus F6's matcher.

## F9. Sharing and the audit boundary

The registry the pitch wants (attack recipes people publish and pull) is exactly
and only the source-2 layer. A published recipe is a flow with `expect`
verdicts and no oracle. This is enforceable, not merely encouraged:

- The import path is data-only by construction. `import-nuclei` never emits an
  oracle, and it refuses a Nuclei template that carries an executable protocol
  (`code`, `javascript`, `headless`, `flow`) rather than importing only its
  http part, because reducing code to data silently would drop the very behavior
  the template is about. This is built.
- A pulled recipe runs like any other flow, under the session's policy, scope
  and receipts. It can describe requests; it cannot smuggle execution, because it
  contains none.
- `audit` already records what ran. A recipe that is data leaves a receipt of the
  requests it made and nothing else, which is the property that lets h5i say a
  downloaded recipe cannot reach outside the box.

The share half (a registry that refuses to publish or pull a flow carrying an
oracle) is designed and not built, because there is no recipe-publishing surface
yet. When one is added, the rule it enforces is the one above: an oracle is
repository-local by definition, and there is no such thing as a shared oracle.

This is the dual-use posture in one rule: recipes are descriptions, oracles are
your own code, and the box enforces that a description cannot become code. It is
the reason h5i can run a stranger's red-team recipe safely when a bash-carrying
alternative cannot.

## F10. What this changes, and what it does not

Does not change: `h5i test`'s oracle contract (design-test.md) is untouched.
Source 3 is exactly as it is, bash and all. `websec sequence`'s live behavior is
untouched. `finding` is untouched.

Changes, in order:

1. Keep the extractor prefixes honest (F3). They are already identical in code;
   this is a doc correction plus a shared implementation so they cannot drift.
2. Add the source-2 `expect` matcher to the shared step grammar (F6), evaluated
   by both engines. Bounded to the Nuclei matcher surface plus `all`/`any`/`not`
   and gated-step branching.
3. Add the Nuclei importer (`h5i websec import-nuclei`), which emits an
   oracle-less `h5i test` whose verdict is `expect` (F7), refusing what it cannot
   express rather than lowering it to an oracle.
4. Let an `expect`-carrying flow be an oracle-less `h5i test` (F8), and let
   `finding --repro` flows graduate into `.h5i-tests/`.
5. Keep the boundary data-only where a crossing exists today: the importer emits
   no oracle and refuses executable Nuclei protocols (F9). The registry half
   waits for a registry.

Deliberately not built: a general assertion language (that is the oracle's job,
and it already exists); response-to-response comparison inside `expect` (oracle);
general conditionals and loops in a flow (that is exploration, which is the
agent's job as a prompt); a shared oracle (a contradiction in terms). The point
of writing this down is that each of these will be asked for, and the answer to
each is a rung on F4's ladder that already has an owner.
