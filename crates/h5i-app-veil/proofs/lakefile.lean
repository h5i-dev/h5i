import Lake
open Lake DSL

-- Optional Veil backend for h5i-app temporal verification.
--
-- Kept as an independent Lake package on Veil's Lean toolchain (`v4.35.0`)
-- so `H5iAppLib` (`v4.31.0`, pinned by Aeneas), the root `lakefile.lean`, and
-- normal h5i builds/CI never require Veil or pay its dependency cost.
require veil from git
  "https://github.com/verse-lab/veil" @ "v4.35.0"

package h5i_app_veil

@[default_target] lean_lib H5iAppVeil where
  roots := #[`H5iAppVeil]
