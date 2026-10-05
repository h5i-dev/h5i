# Tutorial 1: a calculator

In this tutorial you build a small web application, a calculator in which
every user has one memory register, and then prove in Lean 4 that it computes
what it should. When you finish, `POST /rpc` with
`{"cmd":"apply","op":"mul","arg":7}` multiplies your memory by 7 and stores the
result in PostgreSQL. Lean will also have checked, for every user, every state
and every input, that the result is the mathematical one or the correct refusal,
that a command only writes the caller's own memory, that a `get` after a
successful command returns its result, and that the code never panics.

The tutorial uses these files:

| File | Contents |
|---|---|
| `kernel/src/lib.rs` | the application logic, which is what the proofs are about |
| `server/src/lib.rs` | the PostgreSQL store and the JSON API |
| `server/src/main.rs` | the axum server |
| `proofs/Spec.lean` | a description of what the application should do |
| `proofs/Proofs.lean` | the proofs that the code does it |
| `proofs/generated/` | Lean translated from `kernel/`, which you never edit |

## Running the application

You need Docker and Rust. Start PostgreSQL, create a token for two users in
the same organization, and start the server:

```
cd examples  # its own Cargo workspace; run from the repository root
docker run -d -p 127.0.0.1:55432:5432 -e POSTGRES_USER=h5i -e POSTGRES_PASSWORD=h5i postgres:17
export DATABASE_URL=postgres://h5i:h5i@127.0.0.1:55432/h5i H5I_APP_SECRET=dev-secret
ALICE=$(H5I_APP_ISSUE=1:1 cargo run -q -p calculator-server)   # org 1, user 1
BOB=$(H5I_APP_ISSUE=1:2 cargo run -q -p calculator-server)     # org 1, user 2
cargo run -p calculator-server &                           # serves 127.0.0.1:8080
```

Then send a few commands. Bob's memory stays at 0 because every user has their
own register, and the invalid operations come back as errors:

```
rpc() { curl -s -H "Authorization: Bearer $1" -H 'content-type: application/json' -d "$2" localhost:8080/rpc; echo; }
rpc $ALICE '{"cmd":"set","value":6}'                # {"value":6}
rpc $ALICE '{"cmd":"apply","op":"mul","arg":7}'     # {"value":42}
rpc $BOB   '{"cmd":"get"}'                          # {"value":0}
rpc $ALICE '{"cmd":"apply","op":"div","arg":0}'     # {"error":"division_by_zero"}
rpc $ALICE '{"cmd":"apply","op":"sub","arg":100}'   # {"error":"underflow"}
```

## The kernel

An h5i-app application has two parts: the kernel, which makes every decision, and
the shell, which moves data between the kernel, HTTP and PostgreSQL. The kernel
lives in `kernel/src/lib.rs`.

The state is a list of rows. You declare each row type once with `schema!`,
which defines the struct and, for the server, its table mapping:

```rust
h5i_app_schema::schema! {
    mapping calc_tables for calculator_kernel;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Memory in "memories" {
        key { user: u64 }
        value: u64,
    }
}

pub struct Snapshot { pub memories: Vec<Memory> }
```

The whole application is one function:

```rust
pub fn transition(actor: &Principal, snap: &Snapshot, cmd: &Command)
    -> Result<(Option<Memory>, Reply), Error>
```

It receives the caller, the rows of the caller's organization and the command,
and it returns the row to write together with the reply, or a refusal that
commits nothing. Since it cannot reach the database, the clock or the network,
its result depends only on its arguments, which is what makes it possible to
prove things about it.

The kernel is written in the part of Rust that Aeneas can translate to Lean.
In practice this means that loops are `while` loops over indices rather than
iterators, that errors are handled with `match` rather than `?`, and that text
is `Vec<u8>` rather than `String`. The arithmetic shows the resulting style:

```rust
Op::Mul => {
    if b != 0 && a > u64::MAX / b {
        Err(Error::Overflow)
    } else {
        Ok(a * b)
    }
}
```

You can run the kernel's unit test with `cargo test -p calculator-kernel`.

## The server

`server/src/lib.rs` connects the kernel to the framework through four small
trait implementations, none of which makes a decision:

| Implementation | Purpose |
|---|---|
| `impl Kernel for Calc` | points the framework at `transition` and `apply` |
| `impl Store for CalcStore` | loads a tenant's rows and writes the row the kernel returned |
| `impl Api for CalcStore` | turns JSON into a `Command` and a `Reply` into JSON |
| `impl ReplyCodec for CalcStore` | lets clients retry safely with an `Idempotency-Key` header |

The engine runs each request in one SERIALIZABLE transaction: it loads the
caller's tenant, calls `transition`, writes the result and commits, retrying
if another transaction conflicts. Handlers never receive a database
connection, so every change goes through the kernel. `main.rs` is an ordinary
axum server that merges the ready-made `/rpc` route.

`server/tests/postgres.rs` runs random commands through PostgreSQL and through
the in-memory reference engine, which simply calls `apply`, and checks that
both give the same replies and the same final state.

## Translating the kernel to Lean

Put Charon and Aeneas on your `PATH` (`h5i app doctor` checks their
versions) and extract the kernel. From the repository root:

```
h5i app extract examples/app/calculator
```

What to extract is in `h5i-app.toml`, next to `kernel/` and `proofs/`: the
crate, the functions Charon starts from, and the crates extracted with it.

Charon compiles the kernel to an intermediate representation, and Aeneas
writes `proofs/generated/CalculatorKernel.lean`. The `Mul` case above becomes:

```lean
  | Op.Mul =>
    if b != 0#u64
    then
      let i ← core.num.U64.MAX / b
      if a > i
      then ok (core.result.Result.Err Error.Overflow)
      else let i1 ← a * b
           ok (core.result.Result.Ok i1)
    else let i ← a * b
         ok (core.result.Result.Ok i)
```

Every operation that can panic in Rust, such as an overflowing `a * b` or an
out-of-range index, returns a `Result` in the translation, so proving that the
result is `ok` also proves that the Rust code does not panic. You never edit
this file, and CI re-runs the script and fails if its output changes.

## Writing the specification

`proofs/Spec.lean` is the file a reviewer reads. It describes the arithmetic
on natural numbers and mentions neither `U64` nor loops:

```lean
def eval : Op → Nat → Nat → Except Error Nat
  | .Add, a, b => if a + b < 2 ^ 64 then .ok (a + b) else .error .Overflow
  | .Sub, a, b => if b ≤ a then .ok (a - b) else .error .Underflow
  | .Mul, a, b => if a * b < 2 ^ 64 then .ok (a * b) else .error .Overflow
  | .Div, a, b => if b = 0 then .error .DivByZero else .ok (a / b)

def memOf (ms : List Memory) (u : Nat) : Nat :=
  ((ms.find? (fun m => m.user.val = u)).map (fun m => m.value.val)).getD 0
```

Keep the specification short and obvious. The proofs make the code agree with
it, so a mistake in the specification is one that nothing else will catch.

## Writing the proofs

Build the proofs with `lake build` in `proofs/`; the first build downloads
Mathlib. `Proofs.lean` starts with single functions and ends with the whole
application.

The first theorem, `compute_spec`, states that the Rust arithmetic agrees with
`eval`:

```lean
theorem compute_spec (op : Op) (a b : U64) :
    compute op a b ⦃ r => match r with
      | .Ok v => eval op a.val b.val = .ok v.val
      | .Err e => eval op a.val b.val = .error e ⦄
```

The notation `f x ⦃ r => P r ⦄` means that running `f x` succeeds and that its
result satisfies `P`. The proof splits on the operation and runs `step*`,
which steps through the generated code. What remains are facts about the
overflow checks, which `scalar_tac`, a solver for linear arithmetic, proves.
The one nonlinear fact, that `a > MAX / b` holds exactly when `a * b ≥ 2^64`,
is the lemma `mul_check`.

`memory_of` searches a vector, and its proof uses `H5iAppLib.loop_search`, which
turns a statement about one step of a search loop into a statement about the
whole loop. As a result the proof needs no loop invariant of its own:

```lean
theorem memory_of_spec (ms : alloc.vec.Vec Memory) (u : U64) :
    memory_of ms u ⦃ v => v.val = memOf ms.val u.val ⦄
```

With these two lemmas, the theorems about `transition` are short:

| Theorem | Statement |
|---|---|
| `transition_total` | no input makes the kernel fail |
| `apply_correct` | `apply` returns `eval`'s result or its refusal, and writes the result to the caller's memory |
| `writes_own_memory` | a command writes only the caller's row |
| `get_after` | after any successful command, a `get` by the same user returns its result |
| `others_unchanged` | after any successful command, every other user's memory is unchanged |

`get_after` and `others_unchanged` also go through `apply`, the function that
defines what committing a write means. `schema!` generates its row operation,
SQL writes and decoder. `proofs/Storage.lean` instantiates the shared
`H5iAppLib.Store` theorem: after any sequence of commits, the database holds the
rows of the computed state and loading them decodes that state, up to row
order. The PostgreSQL statement and `SELECT` semantics remain the trusted
boundary.

`get_after` assumes that the command succeeded and that its write was
committed. `set_then_get` checks that these hypotheses can hold: on an empty
snapshot, a user sets 5 and then reads 5 back. `sub_refused` shows a
refusal: subtracting 1 from an empty memory returns `Underflow`.

Finally, you can check that the proofs assume nothing beyond Lean's three
standard axioms:

```lean
#print axioms calculator_kernel.Proofs.get_after
-- depends on axioms: [propext, Classical.choice, Quot.sound]
```

## Introducing a bug

To see what a failing proof looks like, change `Sub` so that it also refuses
equal numbers:

```rust
if a <= b {            // was: a < b
    Err(Error::Underflow)
```

Then re-extract, build and gate the proofs:

```
h5i app prove examples/app/calculator
```

Lean stops at the `Sub` case and shows the goal it could not prove:

```
error: Proofs.lean:29:11: unsolved goals
case h1
a b : U64
h✝ : a ≤ b
⊢ (if ↑b ≤ ↑a then Except.ok (↑a - ↑b) else Except.error Error.Underflow) = Except.error Error.Underflow
```

The code refused because `a ≤ b`, but when `b ≤ a` holds as well, that is when
`a = b`, the specification says the answer is `a - b = 0` and not a refusal.
A test suite finds this bug only if someone thinks of trying `5 - 5`, whereas
the proof covers every input. Undo the change before you continue.

## Limits of the proofs

The theorems are about the Rust code as translated by Aeneas, and they cover
the arithmetic, the refusals, the isolation between users
(`others_unchanged`), the `get` round trip and the absence of panics. The framework enforces the rest of the request path
by construction: every request runs through `transition` in a SERIALIZABLE
transaction, tenants are kept apart, and a retry with an idempotency key runs a
command at most once.

A few things remain trusted in this tutorial. The JSON decoder must turn a
request into the command the client meant, although, since every theorem holds
for all commands, a wrong decoding can only produce a command the client could
have sent directly. The store must write what `apply` says, which the test
checks here. Finally, the tools (Charon, Aeneas, Lean and rustc) and PostgreSQL
are trusted. [`docs/app/TRUST.md`](../../../docs/app/TRUST.md) lists the trusted parts
of the framework in full.

## Exercises

1. Add `Op::Rem` for the remainder, and extend `compute`, `eval` and the proof
   of `compute_spec`; `step*` does most of the work.
2. Add a `Clear` command that resets the caller's memory to 0, and extend
   `get_after` to cover it.
3. Delete the proof of `others_unchanged` and write it again. It combines
   `writes_own_memory` and `apply_spec` with a small lemma about `memOf` after
   an `upsert` for a different user.

The [next tutorial](../board/TUTORIAL.md), a bulletin board, adds permissions and invariants.
