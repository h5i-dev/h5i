-- Written by `h5i app extract` from `[authz] guarded` in h5i-app.toml. Do not edit.
import AuthzSpecs
/-!
# Framework-proven authorization

Each `h5i_authz g` reads the policy's instances off `g`'s extracted body and
proves `g.authorized` by instantiating `h5i_app_std.Specs.authz`: every
committed command makes only writes the policy permits and discloses only rows
it lets the caller see. For `guard_viewed` it also proves noninterference of
read-only commands and `H5iAppLib.ReadOnly.Sound` (`g.read_only_sound`).
-/
open Aeneas Aeneas.Std Result H5iAppLib forum_kernel

h5i_derive_all

h5i_authz forum_kernel.guarded_transition
