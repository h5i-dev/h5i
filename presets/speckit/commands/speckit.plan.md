## h5i: Verified Kernel Design

This project uses the h5i preset. After the steps above, fill the plan's
"Verified Kernel Design" section:

1. Collect every `INV-*` and `AUTH-*` ID from the spec. If the spec has none
   but the feature changes state or access rules, stop and ask the user to
   add them (or run the specify step again) before planning.
2. Put every rule the spec states into the kernel's `transition`. List the
   tables, commands, acceptance conditions and refusals. Keep authentication,
   decoding and storage in the shell.
3. Fill the Proof Map: one row per ID with a fully qualified theorem name, a
   statement with its hypotheses, a mutant name prefixed with the ID, and the
   UI node id (the ID in lower case). Name every hypothesis the theorem
   needs; each must be established by the request path, not added to make the
   proof easy.
4. Derive each mutant from the spec's "Broken when" line: the smallest kernel
   edit that makes the rule false. It must still compile and extract: `h5i
   app mutate` reports an edit that breaks the build as invalid, not caught.
5. Fill the Trust Boundary table from the spec. Give each shell-enforced
   security rule an `h5i test` flow.
6. Treat the constitution's K1–K6 principles as Constitution Check gates.

If `h5i` is installed, run `h5i app doctor` and record any missing tool in
Technical Context. For the kernel, proof and mutant conventions, read the h5i
skill's `references/app.md` (`h5i skill install`, or `skills/h5i/` in the h5i
repository).
