import Lake
open Lake DSL

-- The repository-root Lake package, so Reservoir (reservoir.lean-lang.org),
-- which indexes only a root `lake-manifest.json`, lists h5i's proof libraries.
-- Each library is the proof project under `crates/<crate>/proofs`, built from
-- its own sources; those projects stay the ones CI and `h5i app check` build.
-- Same Aeneas pin as every proof project (see crates/h5i-app-cli/src/pins.rs).
require aeneas from git
  "https://github.com/AeneasVerif/aeneas" @ "b86120db3183b0107eb5f2637b11c424cd06ef1c" / "backends/lean"

package h5i

-- Reusable lemmas and tactics for Aeneas-extracted h5i-app kernels.
@[default_target] lean_lib H5iAppLib where
  srcDir := "crates/h5i-app-core/proofs"
  roots := #[`H5iAppLib]

-- The extracted Rust of each std crate (`h5i app extract`), and its proofs.
lean_lib H5iAppStdGenerated where
  srcDir := "crates/h5i-app-std/proofs/generated"
  roots := #[`H5iAppStd]

@[default_target] lean_lib H5iAppStdProofs where
  srcDir := "crates/h5i-app-std/proofs"
  roots := #[`StdSpecs, `AuthzSpecs, `Check]

lean_lib H5iAppJsonGenerated where
  srcDir := "crates/h5i-app-json/proofs/generated"
  roots := #[`H5iAppJson]

@[default_target] lean_lib H5iAppJsonProofs where
  srcDir := "crates/h5i-app-json/proofs"
  roots := #[`JsonSpec, `JsonProofs]

lean_lib H5iAppTokenGenerated where
  srcDir := "crates/h5i-app-token/proofs/generated"
  roots := #[`H5iAppToken]

@[default_target] lean_lib H5iAppTokenProofs where
  srcDir := "crates/h5i-app-token/proofs"
  roots := #[`TokenSpec, `TokenProofs, `TokenEncode]

lean_lib H5iAppH1Generated where
  srcDir := "crates/h5i-app-h1/proofs/generated"
  roots := #[`H5iAppH1]

@[default_target] lean_lib H5iAppH1Proofs where
  srcDir := "crates/h5i-app-h1/proofs"
  roots := #[`H1Spec, `H1Proofs, `H1Props, `H1Smuggle, `H1Body, `H1Read, `H1Prefix, `H1Recv, `H1Valid, `H1Sound, `H1Chunked, `H1Fields]

lean_lib H5iAppSqlGenerated where
  srcDir := "crates/h5i-app-sql/proofs/generated"
  roots := #[`H5iAppSql]

@[default_target] lean_lib H5iAppSqlProofs where
  srcDir := "crates/h5i-app-sql/proofs"
  roots := #[`Semantics]

lean_lib H5iAppPgsqlGenerated where
  srcDir := "crates/h5i-app-pgsql/proofs/generated"
  roots := #[`H5iAppPgsql]

@[default_target] lean_lib H5iAppPgsqlProofs where
  srcDir := "crates/h5i-app-pgsql/proofs"
  roots := #[`Abs, `Compile, `Render, `Sound]
