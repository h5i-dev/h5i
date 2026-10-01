# Tutorial 2: a bulletin board

In this tutorial you build a bulletin board for an organization and prove
that it enforces its permission rules and keeps its data consistent. Everyone
in the organization can read and publish posts. Only the author of a post may
edit it, and the author or a moderator may delete it. Moderators appoint and
remove moderators; while there is none, a user may appoint themselves, and the
last moderator cannot be removed.

The [first tutorial](../calculator/TUTORIAL.md) introduced the kernel, the
server and extraction, and proved that a function computes the right values.
This one proves two new kinds of property. A permission theorem says that
every write a command makes is allowed by a policy, judged against the state
before the command. An invariant says that some facts, such as unique post
ids, hold in every state the board can reach, however many commands run.

| File | Contents |
|---|---|
| `kernel/src/lib.rs` | the application logic |
| `server/src/lib.rs`, `server/src/main.rs` | the PostgreSQL store, the JSON API and the axum server |
| `proofs/Spec.lean` | the policy, the meaning of writes and the invariants |
| `proofs/Commands.lean` | one lemma per command describing what it writes |
| `proofs/Theorems.lean` | the permission and invariant theorems |
| `proofs/Apply.lean` | the proof that committing a write set does what the specification says |
| `proofs/Storage.lean` | the proof that SQL writes and loads hold that same state |
| `proofs/Scenario.lean` | a concrete run that meets the theorems' hypotheses |

## Running the application

With PostgreSQL running as in the first tutorial, create tokens for two users
and start the server:

```
cd examples  # its own Cargo workspace; run from the repository root
export DATABASE_URL=postgres://h5i:h5i@127.0.0.1:55432/h5i H5I_APP_SECRET=dev-secret
ALICE=$(H5I_APP_ISSUE=1:1 cargo run -q -p board-server)
BOB=$(H5I_APP_ISSUE=1:2 cargo run -q -p board-server)
cargo run -p board-server &

rpc() { curl -s -H "Authorization: Bearer $1" -H 'content-type: application/json' -d "$2" localhost:8080/rpc; echo; }
```

Alice publishes a post, which Bob can neither edit nor delete:

```
rpc $ALICE '{"cmd":"publish","text":"hello from alice"}'   # {"id":0}
rpc $BOB   '{"cmd":"edit","id":0,"text":"hacked"}'         # {"error":"forbidden"}
rpc $BOB   '{"cmd":"delete","id":0}'                       # {"error":"forbidden"}
```

Since the board has no moderator yet, Bob can appoint himself. After that,
Alice can no longer do the same, Bob cannot remove himself because he is the
last moderator, and as a moderator he can delete Alice's post:

```
rpc $BOB   '{"cmd":"promote","user":2}'                    # {"ok":true}
rpc $ALICE '{"cmd":"promote","user":1}'                    # {"error":"forbidden"}
rpc $BOB   '{"cmd":"demote","user":2}'                     # {"error":"last_moderator"}
rpc $BOB   '{"cmd":"delete","id":0}'                       # {"ok":true}
```

## The kernel

The state has three tables, declared once with `schema!`: posts, moderators
and a counter that hands out post ids.

```rust
pub struct Post in "posts" {
    key { id: u64 }
    author: u64,
    text: Text,
}

pub struct Moderator in "moderators" {
    key { user: u64 }
}
```

A command now returns a list of writes, such as `Write::PutPost(post)` or
`Write::DelModerator(user)`, because publishing touches two tables: it stores
the post and advances the counter. Each command is its own function, and
`transition` only dispatches:

```rust
pub fn transition(actor: &Principal, s: &Snapshot, cmd: &Command) -> Outcome {
    match cmd {
        Command::Publish { text } => publish(actor.user, s, text),
        Command::Edit { id, text } => edit(actor.user, s, *id, text),
        Command::Delete { id } => delete(actor.user, s, *id),
        Command::List => Ok((Vec::new(), Reply::Posts(s.posts.clone()))),
        Command::Promote { user } => promote(actor.user, s, *user),
        Command::Demote { user } => demote(actor.user, s, *user),
    }
}
```

Keeping the commands separate pays off in the proofs, which follow the same
structure. The rules themselves are ordinary conditions, as in `delete`:

```rust
fn delete(user: u64, s: &Snapshot, id: u64) -> Outcome {
    match find_post(&s.posts, id) {
        None => Err(Error::NotFound),
        Some(p) => {
            if p.author == user || is_moderator(&s.moderators, user) {
                Ok((one(Write::DelPost(id)), Reply::Done))
            } else {
                Err(Error::Forbidden)
            }
        }
    }
}
```

`apply` gives each write its meaning: `PutPost` replaces the post with the same
id or appends it, `DelPost` removes it, and so on.

## The server

The server follows the first tutorial. The store loads the three tables and
turns each write into an upsert or a delete, and `server/tests/postgres.rs`
checks against the in-memory reference engine that PostgreSQL ends up in the
same state as `apply` for random command sequences.

## Writing the specification

Extract the kernel with `scripts/app/extract-board.sh`, then open
`proofs/Spec.lean`. It describes the state as lists:

```lean
structure St where
  next : Nat
  posts : List Post
  mods : List Moderator
```

The policy says, for each kind of write, when user `u` may make it in state
`s`:

```lean
def allowed (s : St) (u : Nat) : Write → Prop
  | .PutPost p =>
    -- A new post by `u`, or an edit of `u`'s own post; the author never changes.
    p.author.val = u ∧ textOk p.text.val ∧
      (p.id.val = s.next ∨ ∃ q, findPost s p.id.val = some q ∧ q.author.val = u)
  | .DelPost id =>
    -- The author or a moderator deletes an existing post.
    ∃ q, findPost s id.val = some q ∧ (q.author.val = u ∨ isMod s u)
  | .PutModerator m =>
    -- A moderator appoints anyone; with no moderators, a user appoints themselves.
    isMod s u ∨ (s.mods = [] ∧ m.user.val = u)
  | .DelModerator t =>
    -- A moderator removes a moderator, and another one remains.
    isMod s u ∧ ∃ m ∈ s.mods, m.user ≠ t
  | .SetCounter c => c.next_id.val = s.next + 1
```

Note that the policy is about writes, not commands. This is deliberate: it
does not matter how a command decides, only what it changes, so a command
added later is checked against the same policy, and the policy needs to change
only when the application gains a new kind of write.

`applyWrite` gives the meaning of a write on lists, and `Inv` lists the facts
that must hold in every state:

```lean
structure Inv (s : St) : Prop where
  post_keys : (s.posts.map (·.id)).Nodup
  fresh : ∀ p ∈ s.posts, p.id.val < s.next
  texts : ∀ p ∈ s.posts, textOk p.text.val
  mod_keys : (s.mods.map (·.user)).Nodup
```

Finally, `Reachable` defines the states the board can reach: the empty board,
and anything a successful command produces from a reachable state.

## Describing each command

`Commands.lean` first gives the helpers specifications in terms of lists. For
example, `find_post` returns the first post with the given id:

```lean
theorem find_post_spec (ps : alloc.vec.Vec Post) (id : U64) :
    find_post ps id ⦃ o => o = ps.val.find? (fun p => p.id.val = id.val) ⦄
```

It then states, for each command, exactly what a successful run writes and
which facts about the state made it succeed. For `delete`, the post exists
and the caller is its author or a moderator:

```lean
theorem delete_spec (u : U64) (s : Snapshot) (id : U64) :
    delete u s id ⦃ r => ∀ ws rep, r = .Ok (ws, rep) →
      (∃ q, findPost (Snapshot.toSt s) id.val = some q ∧ (q.author = u ∨ isMod (Snapshot.toSt s) u.val)) ∧
        ws.val = [.DelPost id] ⦄
```

These proofs run `step*` through the generated code and then point out the
facts that `step*` collected along the way. They are the only proofs in this
tutorial that look at the code.

## Proving the policy

`Theorems.lean` begins with `writes_of`, which combines the command lemmas
into a single statement: a successful command writes nothing, or it publishes,
edits, deletes, promotes or demotes in exactly the way its lemma describes.
The permission theorem is then a case analysis over those six outcomes:

```lean
theorem authorized (a : Principal) (s : Snapshot) (c : Command) ws r
    (hinv : Inv (Snapshot.toSt s)) (h : transition a s c = .ok (.Ok (ws, r))) :
    ∀ w ∈ ws.val, allowed (Snapshot.toSt s) a.user.val w
```

Most cases follow directly from the command lemmas. Demotion needs one more
step, because the kernel checks that there are at least two moderators while
the policy requires a moderator other than the one being removed. The lemma
`other_mod` bridges the two, and it uses the invariant that moderator keys are
unique; this is why `authorized` assumes `Inv`. Once `reachable_inv` below is
proven, `authorized_reachable` drops that assumption for every reachable
state.

## Proving the invariants

Each kind of state change gets one lemma that shows it keeps `Inv`. For
example, `inv_put_post` says that writing a post with valid text and an id
below the counter keeps every post id unique and fresh. `inv_preserved`
combines them over the same six cases, and `reachable_inv` follows by
induction over `Reachable`:

```lean
theorem reachable_inv {s : St} (h : Reachable s) : Inv s := by
  induction h with
  | init => exact init_inv
  | step _ ht ih => exact inv_preserved _ _ _ _ _ ih ht
```

The same case analysis also gives two facts that relate the states before and
after a command. `author_kept` says that no command changes the author of an
existing post, and `moderator_kept` says that once the board has a moderator,
it always has one.

Theorems with hypotheses can hold for the wrong reason: if no state satisfied
them, they would say nothing. `Scenario.lean` rules this out with a concrete
run. Alice appoints herself and posts "hi", `s2_reachable` proves the state
she reaches is `Reachable`, and in it Bob cannot delete her post
(`bob_cannot_delete`) and she cannot demote herself, the last moderator
(`last_moderator_stays`).

## Committing a write set

The theorems describe the new state with `Spec.applyAll`, the meaning of a
write set on lists. `schema!` generates the kernel's `apply` and its table
operations, with their loop lemmas. `Apply.lean` proves that the dispatch over
`Write` computes `Spec.applyAll`. `Storage.lean` then instantiates
`H5iAppLib.Store`: the database reached by running the planned SQL writes holds
exactly those rows, and a load decodes the same state up to row order.

## Introducing a bug

A classic authorization mistake is to check the wrong user. Change `delete`
so that it asks whether the post's author is a moderator, rather than the
caller:

```rust
if p.author == user || is_moderator(&s.moderators, p.author) {
```

After re-extracting, the proof of `delete_spec` fails:

```
error: Commands.lean:87:28: Type mismatch: After simplification, term
  b_post.mp h✝¹
 has type
  ∃ m ∈ ↑s.moderators, m.user = p.author
but is expected to have type
  ∃ m ∈ ↑s.moderators, m.user = u
```

Lean shows that the code established that the author is a moderator, while
the specification requires the caller to be one. With this bug, any user can
delete the posts of a moderator, which a test suite catches only if it
happens to try that combination.

## Limits of the proofs

The theorems cover the kernel as translated by Aeneas: the permissions of
every write, the invariants of every reachable state, the fixed authorship of
posts, the continuity of moderation, and the absence of panics. As in the
first tutorial, the framework runs every request through `transition` in a
SERIALIZABLE transaction, while the JSON decoding, the store's agreement with
`apply` (which the test checks) and the tools remain trusted. The
[document service](../../docs) goes further and proves the store's agreement
with `apply` as well.

## Exercises

1. Let moderators edit any post, and update `allowed` and the proofs. Check
   that `author_kept` still holds, and explain why.
2. Add a `Pin { id }` command that only moderators may run, with a `pinned`
   column on `Post`.
3. Prove that a user who is neither an author nor a moderator can never cause
   a post to be deleted, stated directly in terms of `transition`.

The [next tutorial](../ledger/TUTORIAL.md), a ledger, proves arithmetic invariants over whole tables.
