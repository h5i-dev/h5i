# h5i preset for Spec Kit

A [Spec Kit](https://github.com/github/spec-kit) preset for h5i-app projects.
It extends the Spec Kit templates and commands so that each invariant and
authorization rule in a spec is mapped to a Lean theorem, a mutant and a node
in `h5i-app.ui.json`, and checked with the existing `h5i app` commands. h5i
itself does not depend on Spec Kit.

## Install

```bash
specify init my-app --integration claude
cd my-app
h5i skill speckit                              # writes ./h5i-speckit-preset
specify preset add --dev ./h5i-speckit-preset
```

The h5i binary carries this directory, so the preset matches the installed
`h5i app`. Its version is set to the h5i version on export. Requires Spec Kit
0.9.0 or later. `--dev` copies the files; after upgrading h5i, export again,
then `specify preset remove h5i` and `add` again. From a checkout of this
repository, `specify preset add --dev presets/speckit` also works.

## What it changes

| Spec Kit artifact | Strategy | Adds |
| --- | --- | --- |
| `constitution-template` | append | Principles K1–K6: decisions in the kernel, a theorem per rule, the proof gate, a mutant per proof, the trust boundary, what "verified" means |
| `spec-template` | append | `INV-NN` and `AUTH-NN` rules, each with a "Broken when" counterexample, and a Trust Boundary list |
| `plan-template` | append | Kernel tables and commands, the Proof Map (ID → theorem, mutant, UI node), `[[mutant]]` entries, shell tests, the verification gate |
| `tasks-template` | append | A verification phase: one proof task and one mutant task per ID |
| `speckit.plan` | append | Fill the Proof Map from the spec's IDs |
| `speckit.implement` | append | Run `h5i app prove` and `h5i app mutate` before reporting done |
| `speckit.h5i.trace` | new | Check each ID against `h5i-app.toml`, `h5i-app.ui.json` and the latest `.h5i/app/runs` records |

## Naming

The plan's Proof Map ties the files together by name:

| Spec | `h5i-app.toml` | `h5i-app.ui.json` |
| --- | --- | --- |
| `AUTH-01` | theorem in `[check].theorems`, `[[mutant]] name = "auth_01_..."` | guarantee node `auth-01`, `proved by` edge to a theorem node with that `symbol` |

## Workflow

1. `/speckit.constitution`, then `/speckit.specify`: write the rules with IDs.
2. `/speckit.plan`: design the kernel and the Proof Map.
3. `/speckit.tasks`, then `/speckit.implement`: write the kernel, proofs,
   mutants and `h5i-app.ui.json`, then run `h5i app prove` and
   `h5i app mutate`.
4. `/speckit.h5i.trace`: one row per ID, from the run records.

Agents that use skills invoke these as `/speckit-plan`, `/speckit-h5i-trace`
and so on.

## Limits

The trace command is run by the agent. It checks that the theorem passed the
gate and that the mutant broke the proofs. It does not check that the theorem
states the rule correctly, so review each theorem's statement and hypotheses.
