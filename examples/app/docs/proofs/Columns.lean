import DocsKernel
import H5iAppLib
/-!
# The app's own column types

`Schema.lean` (generated) encodes `Role` and `Status` columns with these. The
numbering must match the server's `enum_field!`.
-/
open Aeneas Aeneas.Std Result docs_kernel

namespace docs_kernel

def Role.col : Role → h5i_app_sql.Val
  | .Viewer => .Int 0#i64
  | .Editor => .Int 1#i64
  | .Owner => .Int 2#i64

def Status.col : Status → h5i_app_sql.Val
  | .Draft => .Int 0#i64
  | .InReview => .Int 1#i64
  | .Approved => .Int 2#i64
  | .Published => .Int 3#i64

theorem Role.col_to_val (r : Role) : Role.Insts.H5i_app_sqlColumn.to_val r = ok r.col := by
  cases r <;> rfl
theorem Status.col_to_val (r : Status) : Status.Insts.H5i_app_sqlColumn.to_val r = ok r.col := by
  cases r <;> rfl

theorem Role.col_from_val (r : Role) : Role.Insts.H5i_app_sqlColumn.from_val r.col = ok (some r) := by
  cases r <;> simp [Role.Insts.H5i_app_sqlColumn.from_val, Role.col]
theorem Status.col_from_val (r : Status) : Status.Insts.H5i_app_sqlColumn.from_val r.col = ok (some r) := by
  cases r <;> simp [Status.Insts.H5i_app_sqlColumn.from_val, Status.col]

@[simp] theorem Role.col_inj (a b : Role) : a.col = b.col ↔ a = b := by
  cases a <;> cases b <;> simp [Role.col]
@[simp] theorem Status.col_inj (a b : Status) : a.col = b.col ↔ a = b := by
  cases a <;> cases b <;> simp [Status.col]

end docs_kernel
