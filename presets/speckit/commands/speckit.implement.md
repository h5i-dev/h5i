## h5i: Verification Gate

This project uses the h5i preset. Implementation is not complete until the
plan's Verification Gate passes. After the tasks above:

1. Run `h5i app lint`, then `h5i app prove`. Fix the kernel, the Lean
   specification or the proof until it passes. Never edit
   `proofs/generated/`, and never add `sorry`, `native_decide`, `axiom` or a
   hypothesis whose only purpose is to make a proof go through.
2. Run `h5i app mutate`. Every Proof Map mutant must be reported as caught
   (the proofs no longer build). A surviving mutant means the theorem does
   not state the rule; strengthen the statement, not the mutant.
3. If the plan lists shell tests and a staging target is available, run
   `h5i test .h5i-tests/tests/ --target <staging-url>`. Otherwise say they
   were not run.
4. Update `h5i-app.ui.json` so each requirement ID has a guarantee node (id =
   the ID in lower case) linked by a `proved by` edge to a theorem node whose
   `symbol` is the Proof Map theorem. Set `author` to say the explanation is
   agent-authored.
5. Run `__SPECKIT_COMMAND_H5I_TRACE__`.

In the completion report, list each requirement ID with its theorem, its
mutant verdict and any exclusion. Call a requirement verified only if its
theorem passed and its mutant was caught in this run. Include failures as
failures.
