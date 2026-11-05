# Notes: a policy declared in the schema

Notes in an organization, with members and admins. A note is visible to
everyone if it is public, otherwise to its owner and admins; the owner or an
admin edits and deletes it, and its owner never changes. Admins add and remove
members; the first member of an empty organization may add themselves.

The policy is not Rust the app writes. It is the `policy` block in the
kernel's `schema!` ([`kernel/src/lib.rs`](kernel/src/lib.rs)):

```rust
policy Notes {
    principal Principal.user;
    write Write;
    row Row;
    admin members.admin;
    table Member in members key user: u64 {
        put PutMember del DelMember row OfMember owner user
        write [admin, first] visible [owner, admin]
    }
    table Note in notes key id: u64 {
        put PutNote del DelNote row OfNote owner owner
        write [owner, admin] visible [public, owner, admin]
    }
}
```

From it `schema!` generates the `Policy` impl and the view in the Rust subset
Aeneas translates, and the Lean schema's `notes_kernel.PolicySpec`: the same
rules stated over lists, rendered from the block rather than from the
generated Rust, with theorems that the extracted `policy::permits`, `visible`
and `stored` compute them. The kernel runs behind the guard
(`h5i_app_std::authz_kernel!`), so the framework's
`guarded_transition.authorized` holds for this policy. There is no
hand-written Lean: `proofs/generated/` is all `h5i app extract`'s.

What the block means, what is generated and what stays trusted:
[`docs/design/design-app-authz.md`](../../../docs/design/design-app-authz.md),
"A policy from the schema".

```bash
cargo test -p notes-kernel          # from examples/app: includes the comparison with a hand-written policy
cargo app check examples/app/notes  # from the repository root: extraction, proofs, obligations
cargo app mutate examples/app/notes --security
```

There is no server: the kernel is the example. A server would run
`notes_kernel::App` on the store `notes_tables!(App, store NotesStore)`
generates, as [`../forum/server`](../forum/server) does.
