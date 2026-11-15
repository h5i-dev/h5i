
## Invariants and Authorization *(mandatory for h5i-app features)*

<!--
  ACTION REQUIRED: State each rule the kernel must never break.
  Use stable IDs. Later steps map each ID to a Lean theorem, a mutant and
  an h5i-app.ui.json node, so do not renumber IDs once a plan exists.

  - INV-NN: a property every reachable state satisfies, or every accepted
    command preserves.
  - AUTH-NN: who may perform which command on which rows, and what a refused
    command changes (normally: nothing).

  Write each rule so a reviewer can tell when it is broken. The "Broken when"
  line is a concrete counterexample: the state and command that would violate
  the rule. It becomes the mutant in the plan.

  Keep identity out of these rules: how the caller is authenticated belongs
  under Trust Boundary below, not in an AUTH rule.
-->

### State Invariants

- **INV-01**: [Rule, e.g., "No two links share a slug within a tenant"]
  - **Broken when**: [e.g., "Create accepts a slug that already exists"]
- **INV-02**: [Rule]
  - **Broken when**: [Counterexample]

### Authorization Rules

- **AUTH-01**: [Rule, e.g., "Only a link's owner can retarget it; a refused retarget writes nothing"]
  - **Broken when**: [e.g., "Retarget by a non-owner changes the target"]
- **AUTH-02**: [Rule]
  - **Broken when**: [Counterexample]

### Trust Boundary

<!--
  What these rules assume but the kernel cannot check. Each item becomes an
  assumption or boundary node in h5i-app.ui.json. Items that are security
  rules enforced by the HTTP shell get an `h5i test` flow instead.
-->

- [e.g., "The bearer token identifies the caller; token handling is shell code"]
- [e.g., "The clock is supplied by the shell and not checked"]
