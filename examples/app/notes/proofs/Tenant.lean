import NotesKernel
import H5iAppLib.Obligation
/-!
# The tenant of a token

The one hand-written theorem of this example, to show a named proof of an
obligation (`[obligations] tenant-of-token` in `h5i-app.toml`): `h5i app check`
requires its type to be exactly `H5iAppLib.Obligation.TenantOfToken` of the
extracted `tenant_of` (written by `authz_kernel!`, the tenant the engine uses)
and `principal`. Without the name, `check`'s automation proves the same
statement, as it does for the forum.
-/
open Aeneas Aeneas.Std Result

namespace notes_kernel.Tenant

theorem tenant_of_token : H5iAppLib.Obligation.TenantOfToken tenant_of principal := by
  intro t u
  exact ⟨{ org := t, user := u }, by simp [principal], by simp [tenant_of]⟩

end notes_kernel.Tenant
