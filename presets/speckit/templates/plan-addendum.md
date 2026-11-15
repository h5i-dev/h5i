
## Verified Kernel Design (h5i)

<!--
  ACTION REQUIRED: Fill these sections from the spec's INV-*, AUTH-* and
  Trust Boundary entries. Every ID in the spec appears in the Proof Map
  exactly once, either with a theorem or marked "excluded" with a reason.
-->

### Kernel Boundary

**Kernel crate**: [e.g., `kernel/` (extracted by `h5i app extract`)]

**Entry point**: `transition(actor, snapshot, command)`

**Tables** (`schema!`):

| Table | Key | Columns | Tenant-scoped |
| --- | --- | --- | --- |
| [links] | [slug] | [owner, target] | [yes] |

**Commands**:

| Command | Accepted when | Writes | Refusal |
| --- | --- | --- | --- |
| [Retarget { slug, target }] | [caller owns slug] | [PutLink] | [NotOwner, writes nothing] |

**Shell responsibilities** (not in the kernel): [authentication, decoding,
status codes, storage engine]

### Proof Map

<!--
  Theorem: fully qualified Lean name as `h5i app check` reports it,
  e.g. `links_kernel.Theorems.retarget_owner`. List it in
  [check].theorems.
  Mutant: the `[[mutant]]` name in h5i-app.toml, prefixed with the ID in
  snake case (auth_01_...). It edits the kernel so the rule breaks in
  exactly the way the spec's "Broken when" line says.
  UI node: the guarantee node in h5i-app.ui.json, whose id is the
  requirement ID in lower case (auth-01).
-->

| ID | Theorem | Statement (hypotheses ⇒ conclusion) | Mutant | UI node |
| --- | --- | --- | --- | --- |
| INV-01 | [`kernel.Theorems.slug_unique`] | [Inv s ∧ accepted ⇒ Inv s'] | [`inv_01_skip_slug_check`] | `inv-01` |
| AUTH-01 | [`kernel.Theorems.retarget_owner`] | [accepted Retarget ⇒ caller = owner] | [`auth_01_drop_owner_check`] | `auth-01` |

### Mutants

```toml
# Add to h5i-app.toml. `old` must occur exactly once in the kernel source.
[[mutant]]
name = "auth_01_drop_owner_check"
old = "[the exact check, e.g. link.owner == actor.user]"
new = "[the weakened check, e.g. true]"
```

### Trust Boundary and Shell Tests

| Spec item | h5i-app.ui.json node | HTTP test |
| --- | --- | --- |
| [Bearer token identifies the caller] | [assumption `a-auth`] | [`.h5i-tests/tests/retarget-other-owner.yaml`] |

### Verification Gate

The feature is done when, for the current kernel:

1. `h5i app lint` reports no unexplained warnings.
2. `h5i app prove` passes with every Proof Map theorem in `[check].theorems`.
3. `h5i app mutate` reports every Proof Map mutant as caught.
4. `h5i test` passes every flow in the table above against staging.
5. The h5i trace command (`/speckit.h5i.trace`) reports no gaps.
