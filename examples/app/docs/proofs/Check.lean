import Frame
/-!
# The invariant checker

`check_inv` returns `true` exactly when `Inv` holds. Migrations run it on every
tenant before committing, so a migration that breaks an invariant is rolled
back.
-/
open Aeneas Aeneas.Std Result docs_kernel docs_kernel.Spec H5iAppLib

namespace docs_kernel.Check

/-! ## Searches -/

@[step]
theorem project_exists_spec (ps : alloc.vec.Vec Project) (k : U64) :
    project_exists ps k ⦃ b => b = ps.val.any (fun q => decide (q.id = k)) ⦄ := by
  unfold project_exists project_exists_loop
  apply WP.spec_mono (loop_search ps.val (fun q => decide (q.id = k)) id (fun _ _ => true) false _ ?_
    0#usize (by simp))
  · intro r hr; simp only [id] at hr
    rw [hr, searchFrom_const, UScalar.ofNatCore_val_eq, List.drop_zero]
    split <;> simp_all
  · intro j hj; unfold project_exists_loop.body; h5i_step

/-! ## Loops over one table: no element is a counterexample -/

@[step]
theorem projects_owned_spec (s : Snapshot) :
    projects_owned s ⦃ b => b = !(s.projects.val.any (fun p => decide (owners s.members.val p.id.val = 0))) ⦄ := by
  unfold projects_owned projects_owned_loop
  apply WP.spec_mono (loop_search s.projects.val (fun p => decide (owners s.members.val p.id.val = 0))
    id (fun _ _ => false) true _ ?_ 0#usize (by simp))
  · intro r hr; simp only [id] at hr
    rw [hr, searchFrom_const, UScalar.ofNatCore_val_eq, List.drop_zero]
    split <;> simp_all
  · intro j hj; unfold projects_owned_loop.body; h5i_step

@[step]
theorem members_in_projects_spec (s : Snapshot) :
    members_in_projects s ⦃ b => b = !(s.members.val.any (fun m => !(s.projects.val.any (fun q => decide (q.id = m.project))))) ⦄ := by
  unfold members_in_projects members_in_projects_loop
  apply WP.spec_mono (loop_search s.members.val (fun m => !(s.projects.val.any (fun q => decide (q.id = m.project))))
    id (fun _ _ => false) true _ ?_ 0#usize (by simp))
  · intro r hr; simp only [id] at hr
    rw [hr, searchFrom_const, UScalar.ofNatCore_val_eq, List.drop_zero]
    split <;> simp_all
  · intro j hj; unfold members_in_projects_loop.body; h5i_step
    all_goals (obtain ⟨x, hx, he⟩ := b_post; exact ⟨by scalar_tac, x, hx, by rw [he]⟩)

@[step]
theorem docs_in_projects_spec (s : Snapshot) :
    docs_in_projects s ⦃ b => b = !(s.documents.val.any (fun d => !(s.projects.val.any (fun q => decide (q.id = d.project))))) ⦄ := by
  unfold docs_in_projects docs_in_projects_loop
  apply WP.spec_mono (loop_search s.documents.val (fun d => !(s.projects.val.any (fun q => decide (q.id = d.project))))
    id (fun _ _ => false) true _ ?_ 0#usize (by simp))
  · intro r hr; simp only [id] at hr
    rw [hr, searchFrom_const, UScalar.ofNatCore_val_eq, List.drop_zero]
    split <;> simp_all
  · intro j hj; unfold docs_in_projects_loop.body; h5i_step
    all_goals (obtain ⟨x, hx, he⟩ := b_post; exact ⟨by scalar_tac, x, hx, by rw [he]⟩)

@[step]
theorem hooks_in_projects_spec (s : Snapshot) :
    hooks_in_projects s ⦃ b => b = !(s.webhooks.val.any (fun w => !(s.projects.val.any (fun q => decide (q.id = w.project))))) ⦄ := by
  unfold hooks_in_projects hooks_in_projects_loop
  apply WP.spec_mono (loop_search s.webhooks.val (fun w => !(s.projects.val.any (fun q => decide (q.id = w.project))))
    id (fun _ _ => false) true _ ?_ 0#usize (by simp))
  · intro r hr; simp only [id] at hr
    rw [hr, searchFrom_const, UScalar.ofNatCore_val_eq, List.drop_zero]
    split <;> simp_all
  · intro j hj; unfold hooks_in_projects_loop.body; h5i_step
    all_goals (obtain ⟨x, hx, he⟩ := b_post; exact ⟨by scalar_tac, x, hx, by rw [he]⟩)

@[step]
theorem projects_fresh_spec (s : Snapshot) :
    projects_fresh s ⦃ b => b = !(s.projects.val.any (fun p => decide (s.counter.next_id.val ≤ p.id.val))) ⦄ := by
  unfold projects_fresh projects_fresh_loop
  apply WP.spec_mono (loop_search s.projects.val (fun p => decide (s.counter.next_id.val ≤ p.id.val))
    id (fun _ _ => false) true _ ?_ 0#usize (by simp))
  · intro r hr; simp only [id] at hr
    rw [hr, searchFrom_const, UScalar.ofNatCore_val_eq, List.drop_zero]
    split <;> simp_all
  · intro j hj; unfold projects_fresh_loop.body; h5i_step

@[step]
theorem docs_fresh_spec (s : Snapshot) :
    docs_fresh s ⦃ b => b = !(s.documents.val.any (fun d => decide (s.counter.next_id.val ≤ d.id.val))) ⦄ := by
  unfold docs_fresh docs_fresh_loop
  apply WP.spec_mono (loop_search s.documents.val (fun d => decide (s.counter.next_id.val ≤ d.id.val))
    id (fun _ _ => false) true _ ?_ 0#usize (by simp))
  · intro r hr; simp only [id] at hr
    rw [hr, searchFrom_const, UScalar.ofNatCore_val_eq, List.drop_zero]
    split <;> simp_all
  · intro j hj; unfold docs_fresh_loop.body; h5i_step

/-- Four-eyes rule and "approver exactly when approved or published", for one document. -/
def docWF (d : Document) : Bool :=
  match d.approver with
  | some a => decide ((d.status = .Approved ∨ d.status = .Published) ∧ a ≠ d.author)
  | none => !decide (d.status = .Approved ∨ d.status = .Published)

@[step]
theorem doc_well_formed_spec (d : Document) : doc_well_formed d ⦃ b => b = docWF d ⦄ := by
  unfold doc_well_formed docWF
  rcases d with ⟨_, _, _, _, _, st, ap, _⟩
  cases st <;> cases ap <;> simp [WP.spec_ok, u64_bne, u64_val_eq]

@[step]
theorem docs_well_formed_spec (v : alloc.vec.Vec Document) :
    docs_well_formed v ⦃ b => b = !(v.val.any (fun d => !docWF d)) ⦄ := by
  unfold docs_well_formed docs_well_formed_loop
  apply WP.spec_mono (loop_search v.val (fun d => !docWF d)
    id (fun _ _ => false) true _ ?_ 0#usize (by simp))
  · intro r hr; simp only [id] at hr
    rw [hr, searchFrom_const, UScalar.ofNatCore_val_eq, List.drop_zero]
    split <;> simp_all
  · intro j hj; unfold docs_well_formed_loop.body; h5i_step

/-! ## Unique keys: no element has a later duplicate -/

@[step]
theorem project_dup_after_spec (v : alloc.vec.Vec Project) (i : Usize) (hi : i.val < v.length) :
    project_dup_after v i ⦃ b => b = (v.val.drop (i.val + 1)).any (fun q => decide (q.id = v.val[i.val].id)) ⦄ := by
  unfold project_dup_after
  step as ⟨j, hj⟩
  unfold project_dup_after_loop
  apply WP.spec_mono (loop_search v.val (fun q => decide (q.id = v.val[i.val].id)) id (fun _ _ => true) false _ ?_
    j (by scalar_tac))
  · intro r hr; simp only [id] at hr; rw [hr, searchFrom_const, hj]; split <;> simp_all
  · intro k hk; unfold project_dup_after_loop.body; h5i_step

/-- Index predicate for the uniqueness loops: `v[i]` has a later duplicate. -/
def dupAt {α} (key : α → U64 × U64) (l : List α) (i : Nat) : Bool :=
  if h : i < l.length then (l.drop (i + 1)).any (fun y => decide (key y = key l[i])) else false

@[step]
theorem project_ids_unique_spec (v : alloc.vec.Vec Project) :
    project_ids_unique v ⦃ b => b = !((List.range v.length).any (dupAt (fun q => (q.id, q.id)) v.val)) ⦄ := by
  unfold project_ids_unique project_ids_unique_loop
  apply WP.spec_mono (loop_search (List.range v.length) (dupAt (fun q => (q.id, q.id)) v.val)
    id (fun _ _ => false) true _ ?_ 0#usize (by simp))
  · intro r hr; simp only [id] at hr
    rw [hr, searchFrom_const, UScalar.ofNatCore_val_eq, List.drop_zero]
    split <;> simp_all
  · intro k hk; unfold project_ids_unique_loop.body; h5i_step
    all_goals (
      have hk' : k.val < v.val.length := by scalar_tac
      refine ⟨hk', ?_⟩
      simp only [dupAt, dif_pos hk', Prod.mk.injEq, and_self]
      simp_all)

@[step]
theorem member_dup_after_spec (v : alloc.vec.Vec Member) (i : Usize) (hi : i.val < v.length) :
    member_dup_after v i ⦃ b => b = (v.val.drop (i.val + 1)).any (fun q => decide (q.project = v.val[i.val].project ∧ q.user = v.val[i.val].user)) ⦄ := by
  unfold member_dup_after
  step as ⟨j, hj⟩
  unfold member_dup_after_loop
  apply WP.spec_mono (loop_search v.val (fun q => decide (q.project = v.val[i.val].project ∧ q.user = v.val[i.val].user)) id (fun _ _ => true) false _ ?_
    j (by scalar_tac))
  · intro r hr; simp only [id] at hr; rw [hr, searchFrom_const, hj]; split <;> simp_all
  · intro k hk; unfold member_dup_after_loop.body; h5i_step

@[step]
theorem member_keys_unique_spec (v : alloc.vec.Vec Member) :
    member_keys_unique v ⦃ b => b = !((List.range v.length).any (dupAt (fun m => (m.project, m.user)) v.val)) ⦄ := by
  unfold member_keys_unique member_keys_unique_loop
  apply WP.spec_mono (loop_search (List.range v.length) (dupAt (fun m => (m.project, m.user)) v.val)
    id (fun _ _ => false) true _ ?_ 0#usize (by simp))
  · intro r hr; simp only [id] at hr
    rw [hr, searchFrom_const, UScalar.ofNatCore_val_eq, List.drop_zero]
    split <;> simp_all
  · intro k hk; unfold member_keys_unique_loop.body; h5i_step
    all_goals (
      have hk' : k.val < v.val.length := by scalar_tac
      refine ⟨hk', ?_⟩
      simp only [dupAt, dif_pos hk', Prod.mk.injEq, and_self]
      simp_all)

@[step]
theorem doc_dup_after_spec (v : alloc.vec.Vec Document) (i : Usize) (hi : i.val < v.length) :
    doc_dup_after v i ⦃ b => b = (v.val.drop (i.val + 1)).any (fun q => decide (q.id = v.val[i.val].id)) ⦄ := by
  unfold doc_dup_after
  step as ⟨j, hj⟩
  unfold doc_dup_after_loop
  apply WP.spec_mono (loop_search v.val (fun q => decide (q.id = v.val[i.val].id)) id (fun _ _ => true) false _ ?_
    j (by scalar_tac))
  · intro r hr; simp only [id] at hr; rw [hr, searchFrom_const, hj]; split <;> simp_all
  · intro k hk; unfold doc_dup_after_loop.body; h5i_step

@[step]
theorem doc_ids_unique_spec (v : alloc.vec.Vec Document) :
    doc_ids_unique v ⦃ b => b = !((List.range v.length).any (dupAt (fun d => (d.id, d.id)) v.val)) ⦄ := by
  unfold doc_ids_unique doc_ids_unique_loop
  apply WP.spec_mono (loop_search (List.range v.length) (dupAt (fun d => (d.id, d.id)) v.val)
    id (fun _ _ => false) true _ ?_ 0#usize (by simp))
  · intro r hr; simp only [id] at hr
    rw [hr, searchFrom_const, UScalar.ofNatCore_val_eq, List.drop_zero]
    split <;> simp_all
  · intro k hk; unfold doc_ids_unique_loop.body; h5i_step
    all_goals (
      have hk' : k.val < v.val.length := by scalar_tac
      refine ⟨hk', ?_⟩
      simp only [dupAt, dif_pos hk', Prod.mk.injEq, and_self]
      simp_all)

@[step]
theorem hook_dup_after_spec (v : alloc.vec.Vec Webhook) (i : Usize) (hi : i.val < v.length) :
    hook_dup_after v i ⦃ b => b = (v.val.drop (i.val + 1)).any (fun q => decide (q.project = v.val[i.val].project)) ⦄ := by
  unfold hook_dup_after
  step as ⟨j, hj⟩
  unfold hook_dup_after_loop
  apply WP.spec_mono (loop_search v.val (fun q => decide (q.project = v.val[i.val].project)) id (fun _ _ => true) false _ ?_
    j (by scalar_tac))
  · intro r hr; simp only [id] at hr; rw [hr, searchFrom_const, hj]; split <;> simp_all
  · intro k hk; unfold hook_dup_after_loop.body; h5i_step

@[step]
theorem hook_projects_unique_spec (v : alloc.vec.Vec Webhook) :
    hook_projects_unique v ⦃ b => b = !((List.range v.length).any (dupAt (fun w => (w.project, w.project)) v.val)) ⦄ := by
  unfold hook_projects_unique hook_projects_unique_loop
  apply WP.spec_mono (loop_search (List.range v.length) (dupAt (fun w => (w.project, w.project)) v.val)
    id (fun _ _ => false) true _ ?_ 0#usize (by simp))
  · intro r hr; simp only [id] at hr
    rw [hr, searchFrom_const, UScalar.ofNatCore_val_eq, List.drop_zero]
    split <;> simp_all
  · intro k hk; unfold hook_projects_unique_loop.body; h5i_step
    all_goals (
      have hk' : k.val < v.val.length := by scalar_tac
      refine ⟨hk', ?_⟩
      simp only [dupAt, dif_pos hk', Prod.mk.injEq, and_self]
      simp_all)

/-! ## From the checks to `Inv` -/

theorem not_any_iff {α} (l : List α) (P : α → Bool) : (!(l.any P)) = true ↔ ∀ x ∈ l, P x = false := by
  simp

/-- The uniqueness loops decide `Nodup` of the keys. -/
theorem unique_iff {α} (key : α → U64 × U64) (l : List α) :
    (!((List.range l.length).any (dupAt key l))) = true ↔ (l.map key).Nodup := by
  rw [nodup_map_iff_no_later, not_any_iff]
  constructor
  · intro h i hi
    have := h i (List.mem_range.2 hi)
    simpa [dupAt, hi] using this
  · intro h i hmem
    have hi := List.mem_range.1 hmem
    simpa [dupAt, hi] using h i hi

/-- Keys `(k, k)` are unique iff keys `k` are. -/
theorem nodup_diag {α} (k : α → U64) (l : List α) :
    (l.map (fun x => (k x, k x))).Nodup ↔ (l.map k).Nodup := by
  have : l.map (fun x => (k x, k x)) = (l.map k).map (fun y => (y, y)) := by simp
  rw [this]
  constructor
  · exact List.Nodup.of_map _
  · exact fun h => h.map (fun a b hab => (Prod.mk.inj hab).1)

theorem docWF_iff (d : Document) :
    docWF d = true ↔
      ((d.status = .Approved ∨ d.status = .Published → ∃ a, d.approver = some a ∧ a ≠ d.author) ∧
       (d.approver.isSome ↔ (d.status = .Approved ∨ d.status = .Published))) := by
  unfold docWF
  cases h : d.approver <;> simp <;> tauto

/-- `check_inv` computes a Boolean conjunction of the checks. -/
def checkB (s : Snapshot) : Bool :=
  !(s.projects.val.any (fun p => decide (owners s.members.val p.id.val = 0))) &&
  !(s.members.val.any (fun m => !(s.projects.val.any (fun q => decide (q.id = m.project))))) &&
  !(s.documents.val.any (fun d => !(s.projects.val.any (fun q => decide (q.id = d.project))))) &&
  !((List.range s.projects.length).any (dupAt (fun q => (q.id, q.id)) s.projects.val)) &&
  !((List.range s.members.length).any (dupAt (fun m => (m.project, m.user)) s.members.val)) &&
  !((List.range s.documents.length).any (dupAt (fun d => (d.id, d.id)) s.documents.val)) &&
  !(s.projects.val.any (fun p => decide (s.counter.next_id.val ≤ p.id.val))) &&
  !(s.documents.val.any (fun d => decide (s.counter.next_id.val ≤ d.id.val))) &&
  !(s.documents.val.any (fun d => !docWF d)) &&
  !(s.webhooks.val.any (fun w => !(s.projects.val.any (fun q => decide (q.id = w.project))))) &&
  !((List.range s.webhooks.length).any (dupAt (fun w => (w.project, w.project)) s.webhooks.val))

theorem check_inv_eq (s : Snapshot) : check_inv s ⦃ b => b = checkB s ⦄ := by
  unfold check_inv
  step*
  all_goals (subst_vars; simp only [checkB, Bool.and_false, Bool.false_and, Bool.and_true, Bool.true_and, *])

theorem checkB_iff (s : Snapshot) : checkB s = true ↔ Inv (Snapshot.toSt s) := by
  simp only [checkB, Bool.and_eq_true]
  rw [show (List.range s.projects.length) = List.range s.projects.val.length from rfl,
    show (List.range s.members.length) = List.range s.members.val.length from rfl,
    show (List.range s.documents.length) = List.range s.documents.val.length from rfl,
    show (List.range s.webhooks.length) = List.range s.webhooks.val.length from rfl]
  simp only [unique_iff, nodup_diag]
  simp only [not_any_iff]
  constructor
  · rintro ⟨⟨⟨⟨⟨⟨⟨⟨⟨⟨h1, h2⟩, h3⟩, h4⟩, h5⟩, h6⟩, h7⟩, h8⟩, h9⟩, h10⟩, h11⟩
    refine ⟨?_, ?_, ?_, h4, h5, h6, ?_, ?_, ?_, ?_, ?_, h11⟩ <;> simp only [Snapshot.toSt]
    · intro p hp; have := h1 p hp; simp at this; omega
    · intro m hm; simpa using h2 m hm
    · intro d hd; simpa using h3 d hd
    · intro p hp; have := h7 p hp; simp at this; omega
    · intro d hd; have := h8 d hd; simp at this; omega
    · intro d hd; exact ((docWF_iff d).1 (by simpa using h9 d hd)).1
    · intro d hd; exact ((docWF_iff d).1 (by simpa using h9 d hd)).2
    · intro w hw; simpa using h10 w hw
  · intro hi
    refine ⟨⟨⟨⟨⟨⟨⟨⟨⟨⟨?_, ?_⟩, ?_⟩, hi.proj_keys⟩, hi.member_keys⟩, hi.doc_keys⟩, ?_⟩, ?_⟩, ?_⟩, ?_⟩,
      hi.hook_keys⟩
    · intro p hp; have := hi.owned p hp; simp only [Snapshot.toSt] at this; simp; omega
    · intro m hm; have := hi.member_proj m hm; simp only [Snapshot.toSt] at this; simpa using this
    · intro d hd; have := hi.doc_proj d hd; simp only [Snapshot.toSt] at this; simpa using this
    · intro p hp; have := hi.proj_fresh p hp; simp only [Snapshot.toSt] at this; simp; omega
    · intro d hd; have := hi.doc_fresh d hd; simp only [Snapshot.toSt] at this; simp; omega
    · intro d hd; simpa using (docWF_iff d).2 ⟨hi.four_eyes d hd, hi.approver_iff d hd⟩
    · intro w hw; have := hi.hook_proj w hw; simp only [Snapshot.toSt] at this; simpa using this

/-- The checker is exact: it returns `true` iff the invariants hold. -/
theorem check_inv_spec (s : Snapshot) : check_inv s ⦃ b => (b = true ↔ Inv (Snapshot.toSt s)) ⦄ := by
  apply WP.spec_mono (check_inv_eq s)
  intro b hb; rw [hb]; exact checkB_iff s

end docs_kernel.Check
