import Spec
/-! Each extracted helper computes its list counterpart. Loops use `H5iAppLib`. -/
open Aeneas Aeneas.Std Result atuin_kernel atuin_kernel.Spec H5iAppLib

namespace atuin_kernel.Lemmas

/-- Alphanumeric or hyphen. -/
def byteOK (n : Nat) : Bool :=
  decide ((97 ≤ n ∧ n ≤ 122) ∨ (65 ≤ n ∧ n ≤ 90) ∨ (48 ≤ n ∧ n ≤ 57) ∨ n = 45)

@[step]
theorem byte_ok_spec (c : U8) : byte_ok c ⦃ b => b = byteOK c.val ⦄ := by
  unfold byte_ok byteOK
  step*
  all_goals (simp only [u8_eq_iff, UScalar.ofNatCore_val_eq] at *; simp_all; try omega)

@[step]
theorem username_ok_spec (nm : alloc.vec.Vec U8) :
    username_ok nm ⦃ b => b = !(nm.val.any (fun c => !byteOK c.val)) ⦄ := by
  unfold username_ok username_ok_loop
  apply WP.spec_mono (loop_search nm.val (fun c => !byteOK c.val) id (fun _ _ => false) true _ ?_
    0#usize (by simp))
  · intro r hr; simp only [id] at hr; rw [search_bool _ _ _ _ _ hr]; split <;> simp_all
  · intro j hj; unfold username_ok_loop.body; h5i_step

@[step]
theorem username_taken_spec (us : alloc.vec.Vec User) (nm : alloc.vec.Vec U8) :
    username_taken us nm ⦃ b => b = us.val.any (fun u => decide (u.username.val = nm.val)) ⦄ := by
  unfold username_taken username_taken_loop
  apply WP.spec_mono (loop_search us.val (fun u => decide (u.username.val = nm.val)) id (fun _ _ => true) false _ ?_
    0#usize (by simp))
  · intro r hr; simp only [id] at hr; rw [search_bool _ _ _ _ _ hr]; split <;> simp_all
  · intro j hj; unfold username_taken_loop.body; h5i_step [alloc.vec.Vec.eq_iff]

@[step]
theorem user_exists_spec (us : alloc.vec.Vec User) (k : U64) :
    user_exists us k ⦃ b => b = us.val.any (fun u => decide (u.id = k)) ⦄ := by
  unfold user_exists user_exists_loop
  apply WP.spec_mono (loop_search us.val (fun u => decide (u.id = k)) id (fun _ _ => true) false _ ?_
    0#usize (by simp))
  · intro r hr; simp only [id] at hr; rw [search_bool _ _ _ _ _ hr]; split <;> simp_all
  · intro j hj; unfold user_exists_loop.body; h5i_step

theorem user_clone (u : User) : User.Insts.CoreCloneClone.clone u = ok u := by
  simp [User.Insts.CoreCloneClone.clone, u8vec_clone, lift]

theorem record_clone (r : Record) : Record.Insts.CoreCloneClone.clone r = ok r := by
  simp [Record.Insts.CoreCloneClone.clone, u8vec_clone, lift]

@[step]
theorem user_clone_spec (u : User) : User.Insts.CoreCloneClone.clone u ⦃ u' => u' = u ⦄ := by
  rw [user_clone]; simp

@[step]
theorem record_clone_spec (r : Record) : Record.Insts.CoreCloneClone.clone r ⦃ r' => r' = r ⦄ := by
  rw [record_clone]; simp

@[step]
theorem find_user_spec (us : alloc.vec.Vec User) (k : U64) :
    find_user us k ⦃ r => r = us.val.find? (fun u => decide (u.id = k)) ⦄ := by
  unfold find_user find_user_loop
  apply WP.spec_mono (loop_search us.val (fun u => decide (u.id = k)) id (fun _ u => some u) none _ ?_
    0#usize (by simp))
  · intro r hr; simp only [id] at hr
    rw [hr, show (fun (_ : Nat) (u : User) => some u) = (fun _ x => some (id x)) from rfl, searchFrom_find]
    simp
  · intro j hj; unfold find_user_loop.body; h5i_step

/-- The size cap check; 0 means no cap. -/
def fitsB (max : Nat) (r : NewRecord) : Bool := decide (max = 0 ∨ r.data.length ≤ max)

@[step]
theorem fits_spec (max : U64) (r : NewRecord) : fits max r ⦃ b => b = fitsB max.val r ⦄ := by
  unfold fits fitsB
  have := r.data.len_ineq; have := usize_max_le
  step*
  all_goals (simp_all; try scalar_tac)

@[step]
theorem all_fit_spec (max : U64) (rs : alloc.vec.Vec NewRecord) :
    all_fit max rs ⦃ b => b = !(rs.val.any (fun r => !fitsB max.val r)) ⦄ := by
  unfold all_fit all_fit_loop
  apply WP.spec_mono (loop_search rs.val (fun r => !fitsB max.val r) id (fun _ _ => false) true _ ?_
    0#usize (by simp))
  · intro r hr; simp only [id] at hr; rw [search_bool _ _ _ _ _ hr]; split <;> simp_all
  · intro j hj; unfold all_fit_loop.body; h5i_step

/-- `Status`: (host, tag, idx) of each of the user's records. -/
def statusL (rs : List Record) (u : U64) : List (U64 × U64 × U64) :=
  (rs.filter (fun r => decide (r.user = u))).map (fun r => (r.host, r.tag, r.idx))

@[step]
theorem status_of_spec (rs : alloc.vec.Vec Record) (u : U64) :
    status_of rs u ⦃ v => v.val = statusL rs.val u ⦄ := by
  unfold status_of status_of_loop
  apply WP.spec_mono (loop_fold rs.val (fun v : alloc.vec.Vec (U64 × U64 × U64) => v.val)
    (fun acc r => if decide (r.user = u) then acc ++ [(r.host, r.tag, r.idx)] else acc)
    (fun v k => v.length ≤ k) (fun x => status_of_loop.body rs u x.1 x.2) ?_ _ 0#usize (by simp) (by simp))
  · intro r hr; rw [hr, foldl_filter_map]; simp [statusL]
  · intro o j hj ho; have := rs.len_ineq; unfold status_of_loop.body; h5i_step

/-- `AddRecords`: one put per new record, owned by the caller. -/
def toWritesL (u : U64) (rs : List NewRecord) : List Write :=
  rs.map (fun r => .PutRecord { user := u, host := r.host, tag := r.tag, idx := r.idx, data := r.data })

@[step]
theorem to_writes_spec (u : U64) (rs : alloc.vec.Vec NewRecord) :
    to_writes u rs ⦃ v => v.val = toWritesL u rs.val ⦄ := by
  unfold to_writes to_writes_loop
  apply WP.spec_mono (loop_fold rs.val (fun v : alloc.vec.Vec Write => v.val)
    (fun acc r => acc ++ [.PutRecord { user := u, host := r.host, tag := r.tag, idx := r.idx, data := r.data }])
    (fun v k => v.length ≤ k) (fun x => to_writes_loop.body u rs x.1 x.2) ?_ _ 0#usize (by simp) (by simp))
  · intro r hr; rw [hr, foldl_map]; simp [toWritesL]
  · intro o j hj ho; have := rs.len_ineq; unfold to_writes_loop.body; h5i_step

/-- `NextRecords`: the caller's records of one series from `start`, at most `count`. -/
def nextL (rs : List Record) (u h t start count : U64) : List Record :=
  rs.foldl (fun acc r =>
    if decide (r.user = u ∧ r.host = h ∧ r.tag = t ∧ start.val ≤ r.idx.val) && decide (acc.length < count.val)
    then acc ++ [r] else acc) []

@[step]
theorem next_records_spec (rs : alloc.vec.Vec Record) (u h t start count : U64) :
    next_records rs u h t start count ⦃ v => v.val = nextL rs.val u h t start count ⦄ := by
  unfold next_records next_records_loop
  apply WP.spec_mono (loop_fold rs.val (fun v : alloc.vec.Vec Record => v.val)
    (fun acc r =>
      if decide (r.user = u ∧ r.host = h ∧ r.tag = t ∧ start.val ≤ r.idx.val) && decide (acc.length < count.val)
      then acc ++ [r] else acc)
    (fun v k => v.length ≤ k) (fun x => next_records_loop.body rs u h t start count x.1 x.2) ?_ _ 0#usize
    (by simp) (by simp))
  · intro r hr; rw [hr]; rfl
  · intro o j hj ho; have := rs.len_ineq; have := usize_max_le
    unfold next_records_loop.body; h5i_step

@[step]
theorem signed_in_spec (s : Snapshot) (a : Principal) :
    signed_in s a ⦃ r => r.map (·.val) = signedIn (Snapshot.toSt s) a ⦄ := by
  unfold signed_in
  cases a <;> step* <;> simp_all [signedIn, Snapshot.toSt]

end atuin_kernel.Lemmas
