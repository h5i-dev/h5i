# Tutorial 4: a private inbox

In this tutorial you build private messaging for an organization and prove
that it keeps messages confidential. Users send messages to each other, read
their inbox and their sent box, mark messages read, delete messages from their
own view and block senders whose messages they no longer want.

The [second tutorial](../board/TUTORIAL.md) proved what a command may change:
every write is allowed by a policy. Confidentiality is about what a command
may reveal, and a policy on writes cannot express it, because a command that
writes nothing can still leak through its reply or its error. This tutorial
introduces a `view`, the part of the state a user may learn, and a theorem
about two runs of the kernel: if two states look the same to a user, every
command by that user has the same outcome in both.

| File | Contents |
|---|---|
| `kernel/src/lib.rs` | the application logic |
| `server/src/lib.rs`, `server/src/main.rs` | the PostgreSQL store, the JSON API and the axum server |
| `proofs/Spec.lean` | what a user may see and touch, the meaning of writes, the invariants |
| `proofs/Commands.lean` | one lemma per command describing what it writes and replies |
| `proofs/Theorems.lean` | confinement, invariants and freshness, about one run |
| `proofs/Noninterference.lean` | the theorem about two runs |
| `proofs/Apply.lean` | the proof that committing a write set does what the specification says |
| `proofs/Scenarios.lean` | small concrete runs that show the theorems are not vacuous |

## Running the application

With PostgreSQL running as in the first tutorial, create tokens for three
users and start the server:

```
cd examples  # its own Cargo workspace; run from the repository root
export DATABASE_URL=postgres://h5i:h5i@127.0.0.1:55432/h5i H5I_APP_SECRET=dev-secret
ALICE=$(H5I_APP_ISSUE=1:1 cargo run -q -p inbox-server)
BOB=$(H5I_APP_ISSUE=1:2 cargo run -q -p inbox-server)
CAROL=$(H5I_APP_ISSUE=1:3 cargo run -q -p inbox-server)
cargo run -p inbox-server &

rpc() { curl -s -H "Authorization: Bearer $1" -H 'content-type: application/json' -d "$2" localhost:8080/rpc; echo; }
```

Alice sends Bob two messages. Each message is numbered within the
conversation from its sender to its recipient, so the reply is the number:

```
rpc $ALICE '{"cmd":"send","to":2,"text":"lunch at noon?"}'   # {"seq":0}
rpc $ALICE '{"cmd":"send","to":2,"text":"or one"}'           # {"seq":1}
rpc $BOB   '{"cmd":"inbox"}'
# [{"from":1,"to":2,"seq":0,"text":"lunch at noon?","read":false},
#  {"from":1,"to":2,"seq":1,"text":"or one","read":false}]
rpc $CAROL '{"cmd":"inbox"}'                                   # []
```

Carol cannot touch the message, and she cannot even learn that it exists: a
message between other people gets the same answer as one that was never sent.

```
rpc $CAROL '{"cmd":"delete","from":1,"to":2,"seq":0}'   # {"error":"not_found"}
rpc $CAROL '{"cmd":"delete","from":1,"to":2,"seq":7}'   # {"error":"not_found"}
```

Bob reads the first message and deletes it. It disappears from his inbox but
stays in Alice's sent box, where she can see that he read it:

```
rpc $BOB   '{"cmd":"mark_read","from":1,"seq":0}'       # {"ok":true}
rpc $BOB   '{"cmd":"delete","from":1,"to":2,"seq":0}'   # {"ok":true}
rpc $BOB   '{"cmd":"inbox"}'
# [{"from":1,"to":2,"seq":1,"text":"or one","read":false}]
rpc $ALICE '{"cmd":"sent"}'
# [{"from":1,"to":2,"seq":0,"text":"lunch at noon?","read":true},
#  {"from":1,"to":2,"seq":1,"text":"or one","read":false}]
```

Finally Bob blocks Alice. Her messages to him are refused until he unblocks
her, while her messages to Carol go through:

```
rpc $BOB   '{"cmd":"block","user":1}'                   # {"ok":true}
rpc $ALICE '{"cmd":"send","to":2,"text":"hello?"}'      # {"error":"blocked"}
rpc $ALICE '{"cmd":"send","to":3,"text":"hello?"}'      # {"seq":0}
rpc $BOB   '{"cmd":"unblock","user":1}'                 # {"ok":true}
rpc $ALICE '{"cmd":"send","to":2,"text":"hello?"}'      # {"seq":2}
```

## The kernel

The state has two tables. A message is keyed by its sender, its recipient and
its number in that conversation. Each side hides a message from its own view
with a flag, so deleting never removes the other side's copy:

```rust
pub struct Message in "messages" {
    key { sender: u64, recipient: u64, seq: u64 }
    text: Text,
    read: bool,
    sender_deleted: bool,
    recipient_deleted: bool,
}

pub struct Block in "blocks" {
    key { owner: u64, sender: u64 }
}
```

The bulletin board numbered posts with a counter shared by the whole board.
That would be a leak here: the id of Alice's message would tell Bob how many
messages the organization had sent before it. Numbering each conversation
separately avoids this, because the number only counts messages that both
Alice and Bob already know about. `send` takes one more than the largest
number in the conversation so far:

```rust
fn send(user: u64, s: &Snapshot, to: u64, text: &Text) -> Outcome {
    if !text_ok(text) {
        return Err(Error::BadText);
    }
    // The sender learns that `to` blocked them; `view` in Spec.lean says so.
    if is_blocked(&s.blocks, to, user) {
        return Err(Error::Blocked);
    }
    let seq = match last_seq(&s.messages, user, to) {
        None => 0,
        Some(k) => {
            if k == u64::MAX {
                return Err(Error::Overflow);
            }
            k + 1
        }
    };
    // ...
    Ok((one(Write::PutMessage(m)), Reply::Sent(seq)))
}
```

`delete` shows the other habit that confidentiality asks for. It refuses a
message that is not the caller's before looking it up, with the same error it
gives for a missing message:

```rust
fn delete(user: u64, s: &Snapshot, from: u64, to: u64, seq: u64) -> Outcome {
    // Someone else's message is reported exactly like a missing one.
    if from != user && to != user {
        return Err(Error::NotFound);
    }
    match find_message(&s.messages, from, to, seq) {
        None => Err(Error::NotFound),
        Some(m) => {
            let sender_deleted = if from == user { true } else { m.sender_deleted };
            let recipient_deleted = if to == user { true } else { m.recipient_deleted };
            // ...
        }
    }
}
```

The other commands are short. `inbox` and `sent` filter the messages,
`mark_read` looks up a message addressed to the caller, and `block` and
`unblock` write the caller's own block row. You can run the kernel's tests
with `cargo test -p inbox-kernel`.

## The server

The server follows the second tutorial. The store loads both tables and turns
each write into an upsert or a delete, and `server/tests/postgres.rs` runs
random command sequences through PostgreSQL and through the in-memory
reference engine and checks that they agree. Note that the engine still loads
the whole organization for every request, including messages between other
people. Nothing in the shell filters them; keeping them private is the
kernel's job, and the proofs are about the kernel.

## Writing the specification

Extract the kernel with `scripts/app/extract-inbox.sh` and open
`proofs/Spec.lean`. The state is two lists, and `involves u m` says that user
`u` sent or received message `m`. The central definition is the view, the
part of the state that user `u` may learn:

```lean
def view (s : St) (u : Nat) : List Message × List Block :=
  (s.msgs.filter (involves u), s.blocks.filter (fun b => b.sender.val = u))
```

A user may learn the messages they sent or received, whole, and who has
blocked them. They may not learn anything about messages between other
people, nor about other people's blocks.

The main theorem compares two runs. It takes two states that agree on `u`'s
view and differ in any other way, and says that every command by `u` has the
same outcome in both:

```lean
theorem noninterference (a : Principal) (s₁ s₂ : Snapshot) (c : Command)
    (hv : view (Snapshot.toSt s₁) a.user.val = view (Snapshot.toSt s₂) a.user.val) :
    transition a s₁ c = transition a s₂ c
```

The conclusion is an equation between whole results. Two equal results have
the same writes, the same reply and the same error, down to the error code, so
the theorem rules out leaks that a property of a single run cannot see. A
theorem saying "every message in a reply involves the caller" is true of a
kernel that answers `forbidden` for Alice's message to Bob and `not_found`
for a missing one, and yet that kernel tells Carol which conversations exist.

### What the view has to contain

The block part of the view was not there at first. With a view of only the
messages,

```lean
def view (s : St) (u : Nat) : List Message :=
  s.msgs.filter (involves u)
```

every command goes through except `send`, where Lean stops with two programs
that differ in one call (the goal is shortened):

```
error: Noninterference.lean:70:13: unsolved goals
case Send
a : Principal
s₁ s₂ : Snapshot
hv : view (Snapshot.toSt s₁) ↑a.user = view (Snapshot.toSt s₂) ↑a.user
d : U64
t : alloc.vec.Vec U8
⊢ (do
      let b ← text_ok t
      if b = true then do
          let b1 ← is_blocked s₁.blocks d a.user
          if b1 = true then ok (core.result.Result.Err Error.Blocked)
          ...
    do
    let b ← text_ok t
    if b = true then do
        let b1 ← is_blocked s₂.blocks d a.user
        if b1 = true then ok (core.result.Result.Err Error.Blocked)
        ...
```

Lean is saying that the outcome of `send` depends on whether the recipient
blocked the sender, which the view does not include. This is a real leak: the
`blocked` error tells Alice that Bob blocked her. The proof leaves you two
ways out. You can declare the leak by putting the blocks that name `u` in the
view, which makes the theorem say openly that a user may learn who blocked
them. Or you can remove it by making a refused send look like a successful
one, for example by storing the message already hidden from the recipient.

This tutorial declares it, because the rule is that a blocked sender's
messages are refused, and a refusal that looks like a delivery is a different
product (often called a shadow ban). The choice is now written down in the one
place a reviewer reads, and the theorem guarantees it is the only thing about
blocks that leaks: Alice learns whether Bob blocked her, not whom else Bob
blocked, and not whom Carol blocked.

Note that the view does not contain the blocks a user owns, because no
command of theirs reads them. A smaller view makes a stronger theorem, and if
you later add a command that lists your own blocks, the proof will ask for
them.

### What a user may touch

The second part of the specification says which rows a user's commands may
change. A write touches `u`'s rows if it writes a message that involves `u`,
or a block that `u` owns:

```lean
def touches (u : Nat) : Write → Prop
  | .PutMessage m => involves u m
  | .PutBlock b => b.owner.val = u
  | .DelBlock b => b.owner.val = u
```

and `others` is everything else, the messages that do not involve `u` and the
blocks that `u` does not own. Finally, `Inv` requires unique keys and valid
texts, and `Reachable` is defined as in the second tutorial.

## Describing each command

`Commands.lean` works as in the second tutorial, with one difference: the
helpers get exact specifications, stated as equations on lists, because the
two-run proof needs to know that a helper's result is a function of the list
it reads. For example, `find_message` returns the first message with the
given key:

```lean
theorem find_message_spec (ms : alloc.vec.Vec Message) (f t q : U64) :
    find_message ms f t q ⦃ o => o = ms.val.find? (keyIs f t q) ⦄
```

The command lemmas then say what a successful run writes. `send_spec` also
records that the new number is larger than every number already used in the
conversation, which `lastSeq_bound` proves about the loop in `last_seq`.

## Proving confinement

`Theorems.lean` starts with `writes_of`, which sums up the command lemmas in
six cases, and proves three confinement theorems from it. `reply_confined`
says that every message in a reply was sent or received by the caller:

```lean
theorem reply_confined (a : Principal) (s : Snapshot) (c : Command) ws ms
    (h : transition a s c = .ok (.Ok (ws, .Messages ms))) :
    ∀ m ∈ ms.val, involves a.user.val m
```

`writes_confined` says that every write touches only the caller's rows, and
`others_unchanged` turns that into a statement about states: after any
successful command, `others` is exactly what it was before. The last step
relies on the choice of key. An upsert replaces the row with the same key,
and since the key contains the sender and the recipient, the replaced row
involves the same users as the new one. With a plain numeric id, an upsert
could overwrite someone else's message, and this proof would need an
invariant to rule it out.

None of these theorems assumes an invariant, and neither does
noninterference. They hold for every state, not only the reachable ones.

## Proving noninterference

The proof in `Noninterference.lean` shows, for each helper, that it returns
the same value in both states. The idea is always the same: a helper only
looks at rows that match a condition, and when the condition implies
`involves u`, it may as well look at the filtered list, which is the view.
For lookups this is a list fact from `H5iAppLib`,

```lean
theorem find?_filter_of_imp (l : List α) (p q : α → Bool) (h : ∀ x ∈ l, p x = true → q x = true) :
    (l.filter q).find? p = l.find? p
```

and `H5iAppLib` has similar facts for `any`, `filter` and `foldl`. With them,
`find_message_same` states that a lookup by a key naming `u` gives the same
answer in both states, `is_blocked_same` does the same for the block that
names `u` as the sender, and so on. The theorem itself is then a case per
command that rewrites with these lemmas. `delete` needs one more step: when
the key does not name the caller, both runs stop at the first check with
`NotFound`, before any lookup.

Two scenario theorems in `Scenarios.lean` check that the statement has
content. `carol_cannot_tell` takes an empty inbox and one where Alice has
written to Bob, which are different states with the same view for Carol, and
concludes that no command of Carol's can distinguish them. `alice_sees_block`
shows that the block part of Alice's view is really used: with and without
Bob's block, her `send` gets different results. Other scenarios check that
messages are delivered, that Bob can delete them and that Carol cannot.

## Invariants and freshness

`reachable_inv` proves the invariants for every reachable state, as in the
second tutorial. Two more theorems describe how messages change over time.
`send_fresh` says that the key of a new message is not used by any existing
message, so sending never overwrites. `msg_kept` says that no command changes
a message's text or clears its read or deleted flags; in particular, a
message deleted from someone's view never comes back. It is stated for
reachable states, because it needs unique keys to know that the message a
command looked up is the one being compared.

## Committing a write set

`schema!` generates the table and `apply` loops with their proofs.
`Apply.lean` proves that dispatch over writes computes `Spec.applyAll`, and
`Storage.lean` instantiates the shared theorem connecting it to PostgreSQL's
stored rows and the decoded load result.

## Introducing a bug

A common way to leak is a helpful error. Suppose someone finds `not_found`
confusing for a message that exists and changes `delete` to look the message
up first and answer `Forbidden` when it belongs to other people:

```diff
 pub enum Error {
     NotFound,
+    Forbidden,
     Blocked,
@@
 fn delete(user: u64, s: &Snapshot, from: u64, to: u64, seq: u64) -> Outcome {
-    // Someone else's message is reported exactly like a missing one.
-    if from != user && to != user {
-        return Err(Error::NotFound);
-    }
     match find_message(&s.messages, from, to, seq) {
         None => Err(Error::NotFound),
         Some(m) => {
+            if from != user && to != user {
+                return Err(Error::Forbidden);
+            }
```

After re-extracting, every lemma in `Commands.lean` and every theorem in
`Theorems.lean` still holds: the new code writes the same rows and returns no
message it should not. Only noninterference fails:

```
error: Noninterference.lean:91:4: unsolved goals
case neg
a : Principal
s₁ s₂ : Snapshot
hv : view (Snapshot.toSt s₁) ↑a.user = view (Snapshot.toSt s₂) ↑a.user
f t q : U64
h : ¬(f = a.user ∨ t = a.user)
hf : (f != a.user) = true
ht : (t != a.user) = true
⊢ (do
      let o ← find_message s₁.messages f t q
      match o with
        | none => ok (core.result.Result.Err Error.NotFound)
        | some m => ok (core.result.Result.Err Error.Forbidden)) =
    do
    let o ← find_message s₂.messages f t q
    match o with
      | none => ok (core.result.Result.Err Error.NotFound)
      | some m => ok (core.result.Result.Err Error.Forbidden)
```

The hypothesis `h` says that the key does not name the caller, and the goal
asks for two lookups in the two states to agree although the view says
nothing about them. With this change Carol can find out whether Alice and Bob
have been talking, and how many messages they exchanged, by trying numbers
until the error changes. The change looks like better error reporting, and a
test suite that checks each error on its own would pass it.

## Limits of the proofs

Noninterference is about the kernel's result, and it covers every channel
that goes through that result: replies, errors and writes. It does not cover
channels outside it. The kernel scans every message in the organization, so
the time a request takes depends on how many messages other people have
exchanged, and a patient user can measure it. Concurrent requests by other users can also
cause SERIALIZABLE retries that a user may notice as latency. The framework
keeps organizations apart, and the shell encodes errors as HTTP status codes;
both are trusted, as in the earlier tutorials.

The view itself is a choice, and a coarse one. It grants each party the whole
message row, so the theorem allows Alice to learn when Bob reads a message
(the sent box shows `read`, which is intended) and also whether he deleted it
(which the replies happen to include but the JSON does not print). A finer
view would list only the columns each side may see.

The numbers in keys leak exactly what the view says: the number of messages
in a conversation, which both participants already know. The second
tutorial's shared counter would not pass this proof unless the counter were
in the view, which is what the [document service](../../docs) does with its
id counter; its view then says that every user may learn how many projects
and documents the organization has created.

Finally, the theorem is about one command at a time. It shows that what `u`
learns from a command depends only on `u`'s view at that moment, but the view
itself changes when other people act: Bob's `mark_read` changes Alice's view,
by design. What a user can infer from watching their view change over many
steps is outside the theorem.

## Exercises

1. Make a refused send look like a success: store the message with
   `recipient_deleted` already set, reply `Sent`, and remove the blocks from
   `view`. Which part of `noninterference` fails now, and what would have to
   change in `view` and in `Reply::Messages` for a version of it to hold?
2. Add a `Blocks` command that lists the caller's own blocks. Run the proofs
   and extend `view` where Lean asks you to.
3. Add a `Purge` step that removes a message once both sides have deleted it.
   Does `send_fresh` still hold? Can a key `(from, to, seq)` ever name two
   different messages over time, and how would you state that?

The [next tutorial](../booking/TUTORIAL.md), meeting rooms, adds time as an input, invariants about intervals, and effects outside the database.
