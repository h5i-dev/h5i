import Lake
open Lake DSL

-- Pinned to the Aeneas commit that generated generated/NotesKernel.lean.
require aeneas from git
  "https://github.com/AeneasVerif/aeneas" @ "b86120db3183b0107eb5f2637b11c424cd06ef1c" / "backends/lean"

require h5i_app_lib from "../../../../crates/h5i-app-core/proofs"

package notes_proofs

-- Generated Lean: the extracted Rust, the schema with the policy's spec, the
-- copied specs and the authorization theorem (`h5i app extract`). Do not
-- edit.
@[default_target] lean_lib Generated where
  srcDir := "generated"
  roots := #[`NotesKernel, `Schema, `StdSpecs, `AuthzSpecs, `Authz]

-- The one hand-written proof: `tenant-of-token`, named in h5i-app.toml.
@[default_target] lean_lib Proofs where
  roots := #[`Tenant]
