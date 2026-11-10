import Lake
open Lake DSL

-- Pinned to the Aeneas commit that generated generated/H5iAppH1.lean.
require aeneas from git
  "https://github.com/AeneasVerif/aeneas" @ "b86120db3183b0107eb5f2637b11c424cd06ef1c" / "backends/lean"

require h5i_app_lib from "../../h5i-app-core/proofs"

package h1_proofs

-- Generated Lean: the extracted Rust (`h5i app extract`). Do not edit.
lean_lib Generated where
  srcDir := "generated"
  roots := #[`H5iAppH1]

-- Hand-written specs and proofs.
@[default_target] lean_lib Proofs where
  roots := #[`H1Spec, `H1Proofs, `H1Props, `H1Smuggle, `H1Body, `H1Read, `H1Prefix, `H1Recv, `H1Valid, `H1Sound, `H1Chunked]
