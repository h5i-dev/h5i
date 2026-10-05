import Commands
/-!
# Theorems about the ledger

`writes_of` sums up `Commands.lean`: a successful command does one of five
things. The money theorems split on those five, using two facts about list
sums: an upsert changes the sum by new row minus replaced row, and two
different rows add up to at most the sum.
-/
open Aeneas Aeneas.Std Result ledger_kernel ledger_kernel.Spec ledger_kernel.Commands H5iAppLib

namespace ledger_kernel.Theorems

/-- What a successful command writes, in five cases. -/
theorem writes_of (a : Principal) (s : Snapshot) (c : Command) ws r
    (h : transition a s c = .ok (.Ok (ws, r))) :
    let st := Snapshot.toSt s
    -- List: nothing.
    (c = .List ∧ ws.val = []) ∨
    -- Open: an empty account with the next id, and the counter moved past it.
    (c = .Open ∧ ∃ n : U64, n.val = st.next + 1 ∧
      ws.val = [.PutAccount ⟨s.ledger.next_id, a.user, 0#u64⟩, .SetLedger { s.ledger with next_id := n }]) ∨
    -- Deposit: the caller's account and total deposits grow by `amt`.
    (∃ id amt q b d, c = .Deposit id amt ∧ findAcc st id.val = some q ∧ q.owner = a.user ∧
      b.val = q.balance.val + amt.val ∧ d.val = st.deposited + amt.val ∧
      ws.val = [.PutAccount { q with id, balance := b }, .SetLedger { s.ledger with deposited := d }]) ∨
    -- Withdraw: the caller's account shrinks and total withdrawals grow by `amt`.
    (∃ id amt q b w, c = .Withdraw id amt ∧ findAcc st id.val = some q ∧ q.owner = a.user ∧
      amt.val ≤ q.balance.val ∧ b.val = q.balance.val - amt.val ∧ w.val = st.withdrawn + amt.val ∧
      ws.val = [.PutAccount { q with id, balance := b }, .SetLedger { s.ledger with withdrawn := w }]) ∨
    -- Transfer: `amt` moves from the caller's account to a different one.
    (∃ src dst amt qa qb x y, c = .Transfer src dst amt ∧ src ≠ dst ∧
      findAcc st src.val = some qa ∧ qa.owner = a.user ∧ amt.val ≤ qa.balance.val ∧
      findAcc st dst.val = some qb ∧ x.val = qa.balance.val - amt.val ∧ y.val = qb.balance.val + amt.val ∧
      ws.val = [.PutAccount { qa with id := src, balance := x }, .PutAccount { qb with id := dst, balance := y }]) := by
  intro st
  cases c with
  | Open =>
    obtain ⟨n, hn, hws⟩ := post_of_ok (open_spec a.user s) h ws r rfl
    exact .inr (.inl ⟨rfl, n, hn, hws⟩)
  | Deposit id amt =>
    obtain ⟨q, hq, ho, b, d, hb, hd, hws⟩ := post_of_ok (deposit_spec a.user s id amt) h ws r rfl
    exact .inr (.inr (.inl ⟨id, amt, q, b, d, rfl, hq, ho, hb, hd, hws⟩))
  | Withdraw id amt =>
    obtain ⟨q, hq, ho, hamt, b, w, hb, hw, hws⟩ := post_of_ok (withdraw_spec a.user s id amt) h ws r rfl
    exact .inr (.inr (.inr (.inl ⟨id, amt, q, b, w, rfl, hq, ho, hamt, hb, hw, hws⟩)))
  | Transfer src dst amt =>
    obtain ⟨hne, qa, qb, ha, ho, hamt, hb, x, y, hx, hy, hws⟩ :=
      post_of_ok (transfer_spec a.user s src dst amt) h ws r rfl
    exact .inr (.inr (.inr (.inr ⟨src, dst, amt, qa, qb, x, y, rfl, hne, ha, ho, hamt, hb, hx, hy, hws⟩)))
  | List =>
    simp [transition, vec_clone_eq Account.Insts.CoreCloneClone s.accounts (fun _ => rfl)] at h
    left
    exact ⟨rfl, by rw [← h.1]; rfl⟩

/-- No command makes the kernel fail: no panic, overflow or bad index. -/
theorem transition_total (a : Principal) (s : Snapshot) (c : Command) : ∃ r, transition a s c = ok r := by
  cases c <;> simp only [transition]
  · exact ok_of (open_spec _ _)
  · exact ok_of (deposit_spec _ _ _ _)
  · exact ok_of (withdraw_spec _ _ _ _)
  · exact ok_of (transfer_spec _ _ _ _ _)
  · exact ⟨.Ok (alloc.vec.Vec.new Write, .Accounts s.accounts),
      by simp [vec_clone_eq Account.Insts.CoreCloneClone s.accounts (fun _ => rfl)]⟩

/-! ## Accounts after a write -/

theorem findAcc_put (s : St) (a : Account) (n : Nat) :
    findAcc (applyWrite s (.PutAccount a)) n = if a.id.val = n then some a else findAcc s n := by
  simp only [findAcc, applyWrite]
  induction s.accounts with
  | nil => simp [upsert]
  | cons y ys ih =>
    by_cases h : y.id = a.id
    · by_cases ha : a.id.val = n <;> simp [upsert, h, ha]
    · simp only [upsert, h, if_false, List.find?_cons, ih]
      by_cases hy : y.id.val = n <;> by_cases ha : a.id.val = n <;> simp_all

/-- Writing account `a` changes the total by `a`'s balance minus the balance
of the account it replaces. -/
theorem total_put (s : St) (a : Account) :
    total (applyWrite s (.PutAccount a)) + ((findAcc s a.id.val).map (·.balance.val)).getD 0 =
      total s + a.balance.val := by
  have := sum_upsert (·.id) (fun b : Account => b.balance.val) a s.accounts
  simp only [findAcc, total, applyWrite]
  simpa using this

theorem findAcc_mem {s : St} {n : Nat} {q : Account} (h : findAcc s n = some q) :
    q ∈ s.accounts ∧ q.id.val = n :=
  ⟨List.mem_of_find?_eq_some h, by simpa using List.find?_some h⟩

/-! ## Bounds from the invariant -/

/-- One balance fits in what was deposited and not withdrawn. -/
theorem balance_le {s : St} (hi : Inv s) {n : Nat} {q : Account} (hq : findAcc s n = some q) :
    q.balance.val + s.withdrawn ≤ s.deposited := by
  have := le_sum (fun b : Account => b.balance.val) (findAcc_mem hq).1
  have := hi.conserved
  simp only [total] at *
  omega

/-- Two different accounts together hold less than 2^64. -/
theorem two_balances_fit {s : St} (hi : Inv s) {n m : Nat} {qa qb : Account}
    (ha : findAcc s n = some qa) (hb : findAcc s m = some qb) (hne : n ≠ m) :
    qa.balance.val + qb.balance.val < 2 ^ 64 := by
  obtain ⟨ma, ia⟩ := findAcc_mem ha
  obtain ⟨mb, ib⟩ := findAcc_mem hb
  have := add_le_sum (fun b : Account => b.balance.val) ma mb (by rintro rfl; exact hne (ia ▸ ib))
  have := hi.conserved
  have := hi.fits
  simp only [total] at *
  omega

theorem total_set (s : St) (l : Ledger) : total (applyWrite s (.SetLedger l)) = total s := rfl

theorem findAcc_fresh {s : St} (hi : Inv s) : findAcc s s.next = none :=
  List.find?_eq_none.2 fun b hb => by have := hi.fresh b hb; simp; omega

/-! ## Conservation -/

/-- What a successful command does to the total: a deposit adds its amount,
a withdrawal removes it, and every other command keeps it. -/
theorem total_after (a : Principal) (s : Snapshot) (c : Command) ws r
    (hinv : Inv (Snapshot.toSt s)) (h : transition a s c = .ok (.Ok (ws, r))) :
    match c with
    | .Deposit _ amt => total (applyAll (Snapshot.toSt s) ws.val) = total (Snapshot.toSt s) + amt.val
    | .Withdraw _ amt => total (applyAll (Snapshot.toSt s) ws.val) + amt.val = total (Snapshot.toSt s)
    | _ => total (applyAll (Snapshot.toSt s) ws.val) = total (Snapshot.toSt s) := by
  rcases writes_of a s c ws r h with
    ⟨hc, hws⟩ | ⟨hc, n, hn, hws⟩ | ⟨id, amt, q, b, d, hc, hq, ho, hb, hd, hws⟩ |
    ⟨id, amt, q, b, w, hc, hq, ho, hamt, hb, hw, hws⟩ |
    ⟨src, dst, amt, qa, qb, x, y, hc, hne, ha, ho, hamt, hb, hx, hy, hws⟩ <;>
    subst hc <;> simp only [hws, applyAll, List.foldl_cons, List.foldl_nil, total_set]
  · -- Open: the new account's id is fresh, so it replaces nothing.
    have := total_put (Snapshot.toSt s) ⟨s.ledger.next_id, a.user, 0#u64⟩
    have hf : findAcc (Snapshot.toSt s) s.ledger.next_id.val = none := findAcc_fresh hinv
    simp [hf] at this; omega
  · have := total_put (Snapshot.toSt s) { q with id, balance := b }
    simp only [hq, Option.map_some, Option.getD_some] at this
    omega
  · have := total_put (Snapshot.toSt s) { q with id, balance := b }
    simp only [hq, Option.map_some, Option.getD_some] at this
    omega
  · -- Transfer: the first write replaces the source, the second the destination.
    have h1 := total_put (Snapshot.toSt s) { qa with id := src, balance := x }
    have h2 := total_put (applyWrite (Snapshot.toSt s) (.PutAccount { qa with id := src, balance := x }))
      { qb with id := dst, balance := y }
    have hne' : src.val ≠ dst.val := fun e => hne ((u64_val_eq _ _).1 e)
    simp only [findAcc_put, hne', if_false, ha, hb, Option.map_some, Option.getD_some] at h1 h2
    omega

/-! ## The invariant -/

theorem fresh_upsert {l : List Account} {N : Nat} (hl : ∀ b ∈ l, b.id.val < N) {a : Account} (ha : a.id.val < N) :
    ∀ b ∈ upsert (·.id) a l, b.id.val < N := by
  intro b hb
  rcases mem_upsert_of hb with rfl | hb
  · exact ha
  · exact hl b hb

/-- Successful commands keep the invariant. -/
theorem inv_preserved (a : Principal) (s : Snapshot) (c : Command) ws r
    (hinv : Inv (Snapshot.toSt s)) (h : transition a s c = .ok (.Ok (ws, r))) :
    Inv (applyAll (Snapshot.toSt s) ws.val) := by
  have ht := total_after a s c ws r hinv h
  obtain ⟨hf, hc, hfit⟩ := hinv
  rcases writes_of a s c ws r h with
    ⟨hc', hws⟩ | ⟨hc', n, hn, hws⟩ | ⟨id, amt, q, b, d, hc', hq, ho, hb, hd, hws⟩ |
    ⟨id, amt, q, b, w, hc', hq, ho, hamt, hb, hw, hws⟩ |
    ⟨src, dst, amt, qa, qb, x, y, hc', hne, ha, ho, hamt, hb, hx, hy, hws⟩ <;>
    subst hc' <;> simp only [hws, applyAll, List.foldl_cons, List.foldl_nil] at ht ⊢
  · exact ⟨hf, hc, hfit⟩
  all_goals refine ⟨?_, ?_, ?_⟩ <;> simp only [Snapshot.toSt, total, applyWrite] at *
  · -- Open: the new id is the old counter, below the new one.
    exact fresh_upsert (fun b hb => by have := hf b hb; omega) (by simp; omega)
  · omega
  · exact hfit
  · -- Deposit: the account exists, so its id is below the counter.
    have := hf q (findAcc_mem hq).1
    exact fresh_upsert hf (by simp [(findAcc_mem hq).2] at this ⊢; omega)
  · omega
  · scalar_tac
  · have := hf q (findAcc_mem hq).1
    exact fresh_upsert hf (by simp [(findAcc_mem hq).2] at this ⊢; omega)
  · omega
  · exact hfit
  · -- Transfer: both accounts exist.
    have h1 := hf qa (findAcc_mem ha).1
    have h2 := hf qb (findAcc_mem hb).1
    exact fresh_upsert (fresh_upsert hf (by simp [(findAcc_mem ha).2] at h1 ⊢; omega))
      (by simp [(findAcc_mem hb).2] at h2 ⊢; omega)
  · omega
  · exact hfit

theorem init_inv : Inv init := by
  constructor <;> simp [init, total]

/-- The invariant holds in every state the ledger can reach. -/
theorem reachable_inv {s : St} (h : Reachable s) : Inv s := by
  induction h with
  | init => exact init_inv
  | step _ ht ih => exact inv_preserved _ _ _ _ _ ih ht

/-- Conservation: the accounts hold exactly what was deposited and not
withdrawn. -/
theorem conservation {s : St} (h : Reachable s) : total s + s.withdrawn = s.deposited :=
  (reachable_inv h).conserved

/-- The money in all accounts fits in a u64. -/
theorem total_fits {s : St} (h : Reachable s) : total s < 2 ^ 64 := by
  have := (reachable_inv h).conserved
  have := (reachable_inv h).fits
  omega

/-! ## The overflow checks never fire

In a reachable state, withdrawals and transfers never refuse with `Overflow`:
the invariant bounds every balance by the total, which fits in a u64. -/

/-- The owner of an account can withdraw any amount up to its balance. -/
theorem withdraw_succeeds (a : Principal) (s : Snapshot) (id amt : U64) (q : Account)
    (hr : Reachable (Snapshot.toSt s)) (hq : findAcc (Snapshot.toSt s) id.val = some q)
    (ho : q.owner = a.user) (hamt : amt.val ≤ q.balance.val) :
    ∃ ws r, transition a s (.Withdraw id amt) = ok (.Ok (ws, r)) := by
  have hi := reachable_inv hr
  have := balance_le hi hq
  have := hi.fits
  obtain ⟨x, hx, ws, r, rfl⟩ := (WP.spec_equiv_exists _ _).1
    (withdraw_ok a.user s id amt q hq ho hamt (by simp only [Snapshot.toSt] at *; omega))
  exact ⟨ws, r, hx⟩

/-- The owner of an account can transfer any amount up to its balance to
any other account. -/
theorem transfer_succeeds (a : Principal) (s : Snapshot) (src dst amt : U64) (qa qb : Account)
    (hr : Reachable (Snapshot.toSt s)) (hne : src ≠ dst)
    (ha : findAcc (Snapshot.toSt s) src.val = some qa) (ho : qa.owner = a.user) (hamt : amt.val ≤ qa.balance.val)
    (hb : findAcc (Snapshot.toSt s) dst.val = some qb) :
    ∃ ws r, transition a s (.Transfer src dst amt) = ok (.Ok (ws, r)) := by
  have hfit := two_balances_fit (reachable_inv hr) ha hb (fun e => hne ((u64_val_eq _ _).1 e))
  obtain ⟨x, hx, ws, r, rfl⟩ := (WP.spec_equiv_exists _ _).1
    (transfer_ok a.user s src dst amt qa qb hne ha ho hamt hb hfit)
  exact ⟨ws, r, hx⟩

/-- A deposit succeeds unless total deposits would pass 2^64 - 1; the check
on the account's own balance never fires. -/
theorem deposit_succeeds (a : Principal) (s : Snapshot) (id amt : U64) (q : Account)
    (hr : Reachable (Snapshot.toSt s)) (hq : findAcc (Snapshot.toSt s) id.val = some q)
    (ho : q.owner = a.user) (hd : s.ledger.deposited.val + amt.val < 2 ^ 64) :
    ∃ ws r, transition a s (.Deposit id amt) = ok (.Ok (ws, r)) := by
  have := balance_le (reachable_inv hr) hq
  obtain ⟨x, hx, ws, r, rfl⟩ := (WP.spec_equiv_exists _ _).1
    (deposit_ok a.user s id amt q hq ho hd (by simp only [Snapshot.toSt] at *; omega))
  exact ⟨ws, r, hx⟩

/-! ## Only the owner moves money out -/

theorem findAcc_set (s : St) (l : Ledger) (n : Nat) : findAcc (applyWrite s (.SetLedger l)) n = findAcc s n := rfl

/-- If a command lowers the balance of an account, the caller owns it. -/
theorem debit_by_owner (a : Principal) (s : Snapshot) (c : Command) ws r
    (hinv : Inv (Snapshot.toSt s)) (h : transition a s c = .ok (.Ok (ws, r)))
    {n : Nat} {q p : Account} (hq : findAcc (Snapshot.toSt s) n = some q)
    (hp : findAcc (applyAll (Snapshot.toSt s) ws.val) n = some p)
    (hlt : p.balance.val < q.balance.val) : q.owner = a.user := by
  rcases writes_of a s c ws r h with
    ⟨hc', hws⟩ | ⟨hc', m, hm, hws⟩ | ⟨id, amt, q', b, d, hc', hq', ho, hb, hd, hws⟩ |
    ⟨id, amt, q', b, w, hc', hq', ho, hamt, hb, hw, hws⟩ |
    ⟨src, dst, amt, qa, qb, x, y, hc', hne, ha, ho, hamt, hb, hx, hy, hws⟩ <;>
    simp only [hws, applyAll, List.foldl_cons, List.foldl_nil, findAcc_set, findAcc_put] at hp
  · -- List changes nothing.
    rw [hq] at hp; cases hp; omega
  · -- Open: the new account's id is fresh, so it is not `n`.
    split at hp
    · have hf : findAcc (Snapshot.toSt s) s.ledger.next_id.val = none := findAcc_fresh hinv
      simp_all
    · rw [hq] at hp; cases hp; omega
  · -- Deposit only raises a balance.
    split at hp
    · subst n; rw [hq'] at hq; cases hq; cases hp; simp at hlt; omega
    · rw [hq] at hp; cases hp; omega
  · -- Withdraw: the caller owns the account.
    split at hp
    · subst n; rw [hq'] at hq; cases hq; exact ho
    · rw [hq] at hp; cases hp; omega
  · -- Transfer: the destination gains, the source belongs to the caller.
    split at hp
    · subst n; rw [hb] at hq; cases hq; cases hp; simp at hlt; omega
    · split at hp
      · subst n; rw [ha] at hq; cases hq; exact ho
      · rw [hq] at hp; cases hp; omega

/-- In every reachable state, only an account's owner can lower its balance. -/
theorem only_owner_debits (a : Principal) (s : Snapshot) (c : Command) ws r
    (hr : Reachable (Snapshot.toSt s)) (h : transition a s c = .ok (.Ok (ws, r)))
    {n : Nat} {q p : Account} (hq : findAcc (Snapshot.toSt s) n = some q)
    (hp : findAcc (applyAll (Snapshot.toSt s) ws.val) n = some p)
    (hlt : p.balance.val < q.balance.val) : q.owner = a.user :=
  debit_by_owner a s c ws r (reachable_inv hr) h hq hp hlt

end ledger_kernel.Theorems
