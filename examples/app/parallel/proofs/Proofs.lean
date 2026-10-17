import ParallelKernel
import Schema
open Aeneas Aeneas.Std Result
namespace parallel_kernel.Proofs

/-- Disabled push rules can never notify, regardless of event contents. -/
theorem disabled_denies (event pattern display : Slice U8)
    (members minimum maximum power required : U64) (requireDisplay : Bool) :
    push_rule event pattern display members minimum maximum power required false requireDisplay = ok 0#u32 := by
  simp [push_rule]

/-- Insufficient sender permission denies before inspecting any event text. -/
theorem insufficient_power_denies (event pattern display : Slice U8)
    (members minimum maximum power required : U64) (requireDisplay : Bool)
    (h : power < required) :
    push_rule event pattern display members minimum maximum power required true requireDisplay = ok 0#u32 := by
  simp [push_rule, h]

/-- A malformed term count fails closed, even if tags permit the query. -/
theorem too_many_terms_denies (text terms : Slice U8) (count : Usize)
    (any : Bool) (negated : U32) (tags required : U64) (h : count > 8#usize) :
    bookmark_search text terms count any negated tags required = ok false := by
  unfold bookmark_search
  simp only [lift]
  split <;> simp_all

/-- Requiring tag 0 rejects a bookmark with no tags, before any text scan. -/
theorem zero_tags_denies (text terms : Slice U8) (count : Usize)
    (any : Bool) (negated : U32) :
    bookmark_search text terms count any negated 0#u64 1#u64 = ok false := by
  simp [bookmark_search, lift]

end parallel_kernel.Proofs
