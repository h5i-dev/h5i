import Spec
/-!
# Theorems

Both authorization properties, over every actor, state and command:
every document written is one the actor may write, and every document a
reply shows is one the actor may read.
-/
open Aeneas Aeneas.Std Result H5iAppLib roles_kernel

namespace Roles

/-- `may` over the kernel's role list is `May` over held roles. -/
theorem covers_iff (s : Snapshot) (u : U64) (rs : alloc.vec.Vec U64) (hrs : ∀ r, r ∈ rs.val ↔ Holds s u r)
    (t : List (U64 × alloc.vec.Vec U8)) (p : List U8) : covers t rs.val p ↔ May s t u p := by
  simp only [covers, May, hrs]

/-- A write is a document at a path the actor may write, and the path is trimmed. -/
theorem transition_writes (a : Principal) (s : Snapshot) (c : Command) :
    transition a s c ⦃ OnOk (fun ws _ => ∀ w ∈ ws.val, May s s.writers.val a.user w.path.val) ⦄ := by
  unfold transition
  h5i_steps
  all_goals simp only [OnOk]
  -- The errors and reads write nothing.
  all_goals try simp
  -- Put: the one write is at the trimmed path `may` checked.
  have hrs := o_post rs (by assumption)
  simp_all [covers_iff s a.user rs hrs]

theorem writes_authorized :
    WritesAuthorized transition id (·.val) (fun s a (w : Doc) => May s s.writers.val a.user w.path.val) :=
  writesAuthorized_of_spec transition_writes

/-- A reply shows only stored documents the actor may read. -/
theorem transition_reads (a : Principal) (s : Snapshot) (c : Command) :
    transition a s c ⦃ OnOk (fun _ r => ∀ d ∈ readsOf r, d ∈ s.docs.val ∧ May s s.readers.val a.user d.path.val) ⦄ := by
  unfold transition
  h5i_steps
  all_goals simp only [OnOk, readsOf]
  -- The errors write and show nothing.
  all_goals try (simp; done)
  all_goals have hrs := o_post rs (by assumption)
  -- Get: the found document, checked by `may`.
  · have hd : d ∈ s.docs.val := List.mem_of_find?_eq_some (p := fun d => decide (d.id = id)) (by simp_all)
    simp_all [covers_iff s a.user rs hrs]
  -- List: the documents `may` keeps.
  · simp_all [covers_iff s a.user rs hrs]

theorem reads_authorized :
    ReadsAuthorized transition id readsOf
      (fun s a (d : Doc) => d ∈ s.docs.val ∧ May s s.readers.val a.user d.path.val) :=
  readsAuthorized_of_spec transition_reads

end Roles
