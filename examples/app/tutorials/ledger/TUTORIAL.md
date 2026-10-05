# Tutorial 3: a ledger

In this tutorial you build a small ledger and prove that it never creates or
loses money. The users of an organization open accounts, deposit into and
withdraw from their own accounts, and transfer from their own accounts to any
account. Balances are `u64` and never go negative.

The [second tutorial](../board/TUTORIAL.md) proved facts about single rows,
such as unique post ids. This one proves arithmetic facts about a whole table:
the sum of all balances equals what was deposited minus what was withdrawn, no
command overflows or underflows, and only the owner of an account can lower
its balance. Along the way you will see how to reason about sums of lists, and
how an invariant that bounds a quantity turns an overflow check in the code
into one that provably never fires.

| File | Contents |
|---|---|
| `kernel/src/lib.rs` | the application logic |
| `server/src/lib.rs`, `server/src/main.rs` | the PostgreSQL store, the JSON API and the axum server |
| `proofs/Spec.lean` | the meaning of writes, the total and the invariant |
| `proofs/Commands.lean` | per command, what it writes and when it succeeds |
| `proofs/Theorems.lean` | conservation, the bounds and the owner theorem |
| `proofs/Apply.lean` | the proof that committing a write set does what the specification says |
| `proofs/Scenarios.lean` | the session below, checked on the extracted code |

## Running the application

With PostgreSQL running as in the first tutorial, create tokens for two users
and start the server:

```
cd examples  # its own Cargo workspace; run from the repository root
export DATABASE_URL=postgres://h5i:h5i@127.0.0.1:55432/h5i H5I_APP_SECRET=dev-secret
ALICE=$(H5I_APP_ISSUE=1:1 cargo run -q -p ledger-server)
BOB=$(H5I_APP_ISSUE=1:2 cargo run -q -p ledger-server)
cargo run -p ledger-server &

rpc() { curl -s -H "Authorization: Bearer $1" -H 'content-type: application/json' -d "$2" localhost:8080/rpc; echo; }
```

Alice and Bob each open an account, and Alice deposits 100 into hers. Bob can
neither withdraw from Alice's account nor transfer out of it:

```
rpc $ALICE '{"cmd":"open"}'                                  # {"id":0}
rpc $BOB   '{"cmd":"open"}'                                  # {"id":1}
rpc $ALICE '{"cmd":"deposit","account":0,"amount":100}'      # {"balance":100}
rpc $BOB   '{"cmd":"withdraw","account":0,"amount":50}'      # {"error":"forbidden"}
rpc $BOB   '{"cmd":"transfer","from":0,"to":1,"amount":50}'  # {"error":"forbidden"}
```

Alice sends 30 to Bob. A transfer to the same account is refused, and so is a
withdrawal larger than the balance:

```
rpc $ALICE '{"cmd":"transfer","from":0,"to":1,"amount":30}'  # {"balance":70}
rpc $ALICE '{"cmd":"transfer","from":0,"to":0,"amount":10}'  # {"error":"same_account"}
rpc $BOB   '{"cmd":"withdraw","account":1,"amount":31}'      # {"error":"insufficient_funds"}
rpc $BOB   '{"cmd":"withdraw","account":1,"amount":20}'      # {"balance":10}
```

Finally, a deposit that would take the total deposits past `u64::MAX` is
refused, and the listing shows that the accounts hold 80, which is the 100
deposited minus the 20 withdrawn:

```
rpc $ALICE '{"cmd":"deposit","account":0,"amount":18446744073709551615}'  # {"error":"overflow"}
rpc $BOB   '{"cmd":"list"}'
# [{"id":0,"owner":1,"balance":70},{"id":1,"owner":2,"balance":10}]
```

## The kernel

The state has two tables: the accounts, and a single `ledger` row that holds
the next account id and the totals deposited and withdrawn.

```rust
pub struct Account in "accounts" {
    key { id: u64 }
    owner: u64,
    balance: u64,
}

pub struct Ledger in "ledger" {
    key {}
    next_id: u64,
    deposited: u64,
    withdrawn: u64,
}
```

There are only two kinds of write, `PutAccount` and `SetLedger`, and each
command that changes money makes two of them. A deposit writes the account
with its new balance and the ledger row with its new total. Every addition is
checked before it happens, so the kernel refuses with `Overflow` where Rust
would otherwise panic:

```rust
} else if l.deposited > u64::MAX - amount {
    // Total deposits must fit in a u64.
    Err(Error::Overflow)
} else if a.balance > u64::MAX - amount {
    Err(Error::Overflow)
} else {
    let a2 = Account { id, owner: a.owner, balance: a.balance + amount };
    let l2 = Ledger { next_id: l.next_id, deposited: l.deposited + amount, withdrawn: l.withdrawn };
    Ok((two(Write::PutAccount(a2), Write::SetLedger(l2)), Reply::Balance(a2.balance)))
}
```

The second check looks necessary, since the balance is a separate number, but
it can never fail: the balance is part of the total deposits, and the first
check has just ensured that the total plus `amount` fits. The code cannot know
this, because it sees one state and nothing about how the state came about.
The proofs can, and we will come back to it.

A transfer reads both accounts and writes both:

```rust
fn transfer(user: u64, s: &Snapshot, src: u64, dst: u64, amount: u64) -> Outcome {
    if src == dst {
        return Err(Error::SameAccount);
    }
    match find_account(&s.accounts, src) {
        None => Err(Error::NotFound),
        Some(a) => {
            if a.owner != user {
                Err(Error::Forbidden)
            } else if amount > a.balance {
                Err(Error::Insufficient)
            } else {
                match find_account(&s.accounts, dst) {
                    None => Err(Error::NotFound),
                    Some(b) => {
                        if b.balance > u64::MAX - amount {
                            Err(Error::Overflow)
                        } else {
                            let a2 = Account { id: src, owner: a.owner, balance: a.balance - amount };
                            let b2 = Account { id: dst, owner: b.owner, balance: b.balance + amount };
                            Ok((two(Write::PutAccount(a2), Write::PutAccount(b2)), Reply::Balance(a2.balance)))
                        }
                    }
                }
            }
        }
    }
}
```

The check `src == dst` matters more than it seems. Both balances are read
from the state before the command, so with `src == dst` the second write would
overwrite the first and the account would gain `amount` instead of keeping its
balance.

## The server

The server is the same as in the second tutorial, with `PutAccount` and
`SetLedger` each turned into an upsert. The JSON calls the accounts of a
transfer `from` and `to`, and the kernel `src` and `dst`. `server/tests/postgres.rs` runs
random commands, including amounts close to `u64::MAX`, through PostgreSQL
and the in-memory reference engine, checks that both agree, and checks
conservation on the final state.

## Writing the specification

Extract the kernel with `h5i app extract examples/app/tutorials/ledger` and open
`proofs/Spec.lean`. The state is the ledger row's three numbers and the list
of accounts, and the new definition is the total:

```lean
structure St where
  next : Nat
  deposited : Nat
  withdrawn : Nat
  accounts : List Account

/-- The money in all accounts together. -/
def total (s : St) : Nat :=
  (s.accounts.map (fun a => a.balance.val)).sum
```

`total` is a `Nat`, so it cannot overflow, and it has no loop. It is the sum
of a list, which Lean's libraries know a great deal about. The invariant then
says what should hold in every state:

```lean
structure Inv (s : St) : Prop where
  fresh : ∀ a ∈ s.accounts, a.id.val < s.next
  -- No money is created or lost.
  conserved : total s + s.withdrawn = s.deposited
  -- Total deposits fit in a u64, so every sum of balances does too.
  fits : s.deposited < 2 ^ 64
```

`conserved` is written as an addition, rather than as
`total s = s.deposited - s.withdrawn`, because subtraction on `Nat` stops at
0, and the addition says in addition that nothing was withdrawn that had not
been deposited. `fits` looks redundant, since `deposited` comes from a `u64`,
but `St` holds a plain `Nat`, and the bound is what the proofs about overflow
need. `Reachable` is defined exactly as in the second tutorial.

## Describing each command

`Commands.lean` gives each command two lemmas. The first is the familiar one:
what a successful run writes and which facts made it succeed. For a transfer,
the accounts differ, the caller owns the source, and the amount is covered:

```lean
theorem transfer_spec (u : U64) (s : Snapshot) (src dst amt : U64) :
    transfer u s src dst amt ⦃ r => ∀ ws rep, r = .Ok (ws, rep) → src ≠ dst ∧
      ∃ qa qb, findAcc (Snapshot.toSt s) src.val = some qa ∧ qa.owner = u ∧ amt.val ≤ qa.balance.val ∧
        findAcc (Snapshot.toSt s) dst.val = some qb ∧
        ∃ x y : U64, x.val = qa.balance.val - amt.val ∧ y.val = qb.balance.val + amt.val ∧
          ws.val = [.PutAccount { qa with id := src, balance := x }, .PutAccount { qb with id := dst, balance := y }] ⦄
```

The second lemma goes the other way: it says when the command succeeds. For a
transfer, the business conditions are enough, together with one bound on the
numbers, namely that the two balances together fit in a `u64`:

```lean
theorem transfer_ok (u : U64) (s : Snapshot) (src dst amt : U64) (qa qb : Account) (hne : src ≠ dst)
    (ha : findAcc (Snapshot.toSt s) src.val = some qa) (ho : qa.owner = u) (hamt : amt.val ≤ qa.balance.val)
    (hb : findAcc (Snapshot.toSt s) dst.val = some qb) (hfit : qa.balance.val + qb.balance.val < 2 ^ 64) :
    transfer u s src dst amt ⦃ r => ∃ ws rep, r = .Ok (ws, rep) ⦄
```

The proof runs `step*`, which leaves one goal per refusal. In each of them
the hypotheses contradict the branch that was taken. For the overflow branch,
the context contains `i = U64.rMax - amt` and `b.balance > i`, which together
with `hfit` and `hamt` are linear facts about natural numbers, so `omega`
finds the contradiction. `scalar_tac`, which the first tutorial used, is
`omega` with a preprocessing step that adds the bounds of every `U64` in the
context; it is the tool to reach for when a goal mentions machine integers,
and `omega` is enough once they have been turned into `Nat`s.

## Sums over lists

Each write replaces one row of the accounts table, so the proofs about the
total need to know how replacing a row changes a sum. `H5iAppLib.upsert`
replaces the first row with the same key, or appends one. The library lemma
`sum_upsert` states the effect on any sum, with no assumption about the list:

```lean
theorem sum_upsert {α κ} [DecidableEq κ] (key : α → κ) (f : α → Nat) (x : α) (l : List α) :
    ((upsert key x l).map f).sum + ((l.find? (fun y => key y = key x)).map f).getD 0 =
      (l.map f).sum + f x
```

In words, the new sum plus the value of the row that was replaced (or 0 if
there was none) equals the old sum plus the value of the new row. Again the
statement avoids subtraction. The proof is an induction on the list with one
case split, closed by `simp` and `omega`. For the accounts table it becomes
`total_put`:

```lean
theorem total_put (s : St) (a : Account) :
    total (applyWrite s (.PutAccount a)) + ((findAcc s a.id.val).map (·.balance.val)).getD 0 =
      total s + a.balance.val
```

The second fact, also in `H5iAppLib`, bounds single rows by the sum. `le_sum` says that one row is
at most the sum, and `add_le_sum` says that two different rows together are:

```lean
theorem add_le_sum {α} (f : α → Nat) {l : List α} {a b : α} (ha : a ∈ l) (hb : b ∈ l) (hne : a ≠ b) :
    f a + f b ≤ (l.map f).sum
```

## Proving conservation

`writes_of` combines the command lemmas, as in the second tutorial, and
`total_after` uses it to state what each command does to the total:

```lean
theorem total_after (a : Principal) (s : Snapshot) (c : Command) ws r
    (hinv : Inv (Snapshot.toSt s)) (h : transition a s c = .ok (.Ok (ws, r))) :
    match c with
    | .Deposit _ amt => total (applyAll (Snapshot.toSt s) ws.val) = total (Snapshot.toSt s) + amt.val
    | .Withdraw _ amt => total (applyAll (Snapshot.toSt s) ws.val) + amt.val = total (Snapshot.toSt s)
    | _ => total (applyAll (Snapshot.toSt s) ws.val) = total (Snapshot.toSt s)
```

A deposit adds exactly its amount, a withdrawal removes exactly its amount,
and every other command, transfers included, leaves the total unchanged. Each
case applies `total_put` once per written account and hands the resulting
equations to `omega`. The transfer case is the interesting one:

```lean
have h1 := total_put (Snapshot.toSt s) { qa with id := src, balance := x }
have h2 := total_put (applyWrite (Snapshot.toSt s) (.PutAccount { qa with id := src, balance := x }))
  { qb with id := dst, balance := y }
have hne' : src.val ≠ dst.val := fun e => hne ((u64_val_eq _ _).1 e)
simp only [findAcc_put, hne', if_false, ha, hb, Option.map_some, Option.getD_some] at h1 h2
omega
```

The second write happens in the state after the first, so `h2` mentions the
destination account as the first write left it. `findAcc_put` says that a
lookup after a write returns the written account if the ids match and the old
one otherwise, and since `src ≠ dst`, the old one is `qb`. What remains is
`t₁ + qa = t + (qa - amt)` and `t₂ + qb = t₁ + (qb + amt)` with `amt ≤ qa`,
from which `omega` concludes `t₂ = t`.

Opening an account uses the invariant: the new account has balance 0, but
the sum stays the same only if the write appends rather than replaces a row
with money in it. `fresh` says that no account has the id `next`, so nothing
is replaced.

`inv_preserved` combines `total_after` with the new ledger row to show that
`conserved` still holds, and shows `fits` from the fact that the new
`deposited` is again a `u64`. As before, `reachable_inv` follows by induction,
and conservation is one of its fields:

```lean
theorem conservation {s : St} (h : Reachable s) : total s + s.withdrawn = s.deposited :=
  (reachable_inv h).conserved

theorem total_fits {s : St} (h : Reachable s) : total s < 2 ^ 64
```

## Overflow checks that never fire

`total_fits` is the bound that makes the kernel's overflow checks for
withdrawals and transfers unreachable. Two different accounts are two
different rows of the list, so by `add_le_sum` their balances add up to at
most the total, which is below 2^64:

```lean
theorem two_balances_fit {s : St} (hi : Inv s) {n m : Nat} {qa qb : Account}
    (ha : findAcc s n = some qa) (hb : findAcc s m = some qb) (hne : n ≠ m) :
    qa.balance.val + qb.balance.val < 2 ^ 64
```

This is exactly the hypothesis `hfit` of `transfer_ok`, so in a reachable
state a transfer succeeds whenever the business conditions hold:

```lean
theorem transfer_succeeds (a : Principal) (s : Snapshot) (src dst amt : U64) (qa qb : Account)
    (hr : Reachable (Snapshot.toSt s)) (hne : src ≠ dst)
    (ha : findAcc (Snapshot.toSt s) src.val = some qa) (ho : qa.owner = a.user) (hamt : amt.val ≤ qa.balance.val)
    (hb : findAcc (Snapshot.toSt s) dst.val = some qb) :
    ∃ ws r, transition a s (.Transfer src dst amt) = ok (.Ok (ws, r))
```

In other words, the `Overflow` branch in `transfer` is dead code in every
state the ledger can reach. `withdraw_succeeds` does the same for the check on
`withdrawn + amount`, using `balance_le`, which bounds one balance plus the
total withdrawn by the total deposited. `deposit_succeeds` keeps one
condition, `deposited + amt < 2^64`, because that check can fire; the
scenario `deposit_overflow` shows it does. Its second check, on the balance,
never fires.

The code checks every operation locally, which makes it safe on any input, and `transition_total` still holds for every
snapshot. The invariant bounds a quantity, here the total, and the proof
shows which of those local checks can never fail. The invariant cannot be
dropped: on an arbitrary snapshot, two accounts may well hold `u64::MAX`
each.

## Only the owner moves money out

The last theorem compares each account before and after a command. If its
balance went down, the caller owns it:

```lean
theorem only_owner_debits (a : Principal) (s : Snapshot) (c : Command) ws r
    (hr : Reachable (Snapshot.toSt s)) (h : transition a s c = .ok (.Ok (ws, r)))
    {n : Nat} {q p : Account} (hq : findAcc (Snapshot.toSt s) n = some q)
    (hp : findAcc (applyAll (Snapshot.toSt s) ws.val) n = some p)
    (hlt : p.balance.val < q.balance.val) : q.owner = a.user
```

The proof uses `findAcc_put` to follow account `n` through the writes. A
deposit and the destination of a transfer only raise a balance, the source of
a transfer and a withdrawal belong to the caller, and every other account is
untouched.

Unlike the policy in the second tutorial, this theorem is not stated per
write. A per-write policy would accept both writes of a transfer to the same
account, the debit because the caller owns the account and the credit because
anyone may be paid; it takes the whole-table property, conservation, to
reject that combination.

## Scenarios

A theorem with a false hypothesis is true and says nothing, and a theorem
about a kernel that refuses everything is easy to prove. `Scenarios.lean`
guards against both by replaying the session above on the extracted code.
Each step is closed by `simp`, which evaluates the lookups, and `step*`,
which evaluates the arithmetic:

```lean
theorem open_alice : Runs s0 alice .Open s1 := by run
theorem open_bob : Runs s1 bob .Open s2 := by run
theorem deposit_alice : Runs s2 alice (.Deposit 0#u64 100#u64) s3 := by run
theorem transfer_alice : Runs s3 alice (.Transfer 0#u64 1#u64 30#u64) s4 := by run
```

From these, `reachable_s4` shows that the final state is reachable, so the
theorems apply to it, and `transfer_succeeds_s3` applies `transfer_succeeds`
to a concrete state, which shows that its hypotheses can hold. Alice's
transfer lowers her balance, so `only_owner_debits` covers something that
happens. The refusals are there too: `bob_cannot_withdraw`,
`bob_cannot_transfer`, `no_self_transfer`, `no_overdraft` and
`deposit_overflow`.

## Committing a write set

`schema!` generates the ledger's upsert and `apply` loops with their proofs.
`Apply.lean` proves that the dispatch over writes computes `Spec.applyAll`.
`Storage.lean` instantiates the shared theorem connecting those writes to
the rows PostgreSQL holds and the state its load decodes.

## Introducing a bug

Remove the check that the two accounts of a transfer differ:

```rust
if src == dst {
    return Err(Error::SameAccount);
}
```

Now a transfer from an account to itself writes the debited account and then
the credited one, both computed from the old balance, and the account gains
`amount`. After re-extracting, `lake build` stops in `transfer_spec`:

```
error: Commands.lean:83:9: Tactic `assumption` failed
...
ha : findAcc (Snapshot.toSt s) ↑src = some a
hb : findAcc (Snapshot.toSt s) ↑dst = some b
⊢ ¬src = dst
```

The lemma promises that the accounts differ, and nothing in the code
establishes it any more. It is tempting to delete the promise from
`transfer_spec` and `writes_of`. Do that, and the failure moves to the
transfer case of `total_after`:

```
error: Theorems.lean:150:4: omega could not prove the goal:
a possible counterexample may satisfy the constraints
  ...
where
 b := ↑(↑qa.balance - ↑amt)
 c := ↑(total (applyWrite (Snapshot.toSt s) (Write.PutAccount { id := src, owner := qa.owner, balance := x })))
 d := ↑(total (Snapshot.toSt s))
 e := ↑(total
    (applyWrite (applyWrite (Snapshot.toSt s) (Write.PutAccount { id := src, owner := qa.owner, balance := x }))
      (Write.PutAccount { id := dst, owner := qb.owner, balance := y })))
 f := ↑((Option.map (fun x => ↑x.balance)
        (if ↑src = ↑dst then some { id := src, owner := qa.owner, balance := x } else some qb)).getD
    0)
```

The term `f` is the balance the second write replaces, and Lean shows that it
depends on whether `src = dst`: if it does, the second write replaces the
account the first one just wrote, not `qb`, and `omega` finds totals `d` and
`e` that differ. A unit test catches this bug only if it happens to transfer
from an account to itself. Undo the change before you continue.

## Limits of the proofs

The theorems cover the kernel as translated by Aeneas: conservation in every
reachable state, the effect of each command on the total, the bounds that
rule out overflow for withdrawals and transfers, the owner rule for debits,
and the absence of panics. The one limit that remains is real: once the total
ever deposited reaches `u64::MAX`, deposits are refused, even if much of it
has been withdrawn. Keeping the running totals rather than just the current
supply is what makes conservation a statement about history, and a larger
counter type would move the limit.

As in the earlier tutorials, the framework runs every request through
`transition` in a SERIALIZABLE transaction, which matters here because two
concurrent transfers from the same account must not both see the old
balance. The JSON decoding, the store's agreement with `apply` (which the
test checks) and the tools remain trusted.

## Exercises

1. Add a `Close { account }` command that deletes an empty account owned by
   the caller, with a new `DelAccount` write. You will need a lemma like
   `sum_upsert` for `List.filter`.
2. Add an optional per-account limit on a single withdrawal, stored as a
   column on `Account`, and prove that no successful withdrawal exceeds it.
3. Prove that no command changes the owner of an existing account, and use it
   to state `only_owner_debits` across a whole sequence of commands.

The [next tutorial](../inbox/TUTORIAL.md), private messages, proves that users learn nothing about messages that are not theirs.
