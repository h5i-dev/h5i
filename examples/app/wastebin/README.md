# Wastebin

[Wastebin](https://github.com/matze/wastebin) is a pastebin built on axum. This
example ports its paste rules (at commit b27a2ab, 2026-09-26) to an h5i-app
kernel, serves them from PostgreSQL, and proves properties of the extracted
code in Lean, including the fix for
[issue #190](https://github.com/matze/wastebin/issues/190), where link
previews burned pastes.

## The model

Anyone may create a paste, optionally with an expiry in seconds, burn after
reading, and a password. The paste belongs to the first uid in the caller's
signed `uid` cookie, and a caller without one gets a new uid, as with
Wastebin's form route. A paste is found by a random slug. If the slug the shell
drew is taken, the kernel refuses and the server draws again, up to ten times
as Wastebin does.

Reads follow `Database::get`. Reading an expired paste deletes it and answers
404. A paste with a password is not shown, and not deleted, until the request
carries the right one. A burn-after-reading paste is deleted by the read that
shows it. The paste page (`/{id}`) and the other read routes (`/raw/{id}`,
`/dl/{id}`, and `/md/{id}` in Wastebin) are separate commands, `View` and
`Fetch`, because only the page asks for confirmation. Deleting follows
`delete_for`: it succeeds only for a uid that owns the paste, and a missing
paste gets the same refusal. `Purge` deletes every expired paste, like
`wastebin-ctl purge`, and the server runs it with `wastebin-server purge`.

The kernel has no clock and no random numbers. The server's `Shell` draws a
random slug for every request and puts it into the `Principal`, next to the
uids from the cookie, and the engine stamps each attempt with the database's
time (`Kernel::stamp`), so a client chooses neither. The engine runs with
`monotonic`, so a paste that has expired never comes back because a later
request carries an earlier time.

Some parts are left out. Wastebin encrypts a password-protected paste with
ChaCha20-Poly1305 under a key derived with Argon2; here the shell stores a
keyed HMAC fingerprint of the password, the kernel compares fingerprints,
and the text is stored in plain. Highlighting, Markdown rendering, titles,
QR codes, themes, the render cache, the HTML forms (the server speaks JSON),
the `?owner=` handoff links and `wastebin-ctl list` are not modeled, and the
file extension lives only in the URL. Wastebin uses one random number as both
the row id and the URL; the kernel keeps a counter id next to the random slug,
so an id is never reused.

## Issue #190

Chat apps fetch a link to build a preview. Before commit 632ddf2 (2026-04-24),
fetching `/{id}` revealed a burn-after-reading paste and deleted it, so the
recipient found it gone. The fix answers a plain request with a "reveal and
burn" page, and only a request with `confirm_burn=1` shows the paste.

The kernel has two variants. `transition` is Wastebin today, and
`transition_pre190` differs only in the paste page, which reads without
asking. `Spec.PreviewSafe` states the property: in a reachable state, a
request for the paste page without the confirmation field never removes a
paste that has not expired and never shows a burn-after-reading paste.
Lean proves it for `transition` and proves it false for `transition_pre190`
on a state that one `Create` reaches.

The fix covers the page only. `/raw/{id}` and `/dl/{id}` still burn on a
plain GET (`raw_link_burns`), so a preview of such a link burns the paste;
Wastebin hands out the page link, which is safe.

A related race (GHSA-gv98-3446-rvjx, fixed in 80295da) let concurrent reads
all see a burn-after-reading paste before the first delete landed. Here the
read and the delete commit in one SERIALIZABLE transaction, and `burn_once`
shows that no state after the burning read holds the paste.

## Theorems

Unless noted otherwise, the theorems hold for both variants and for every
principal, state and command. `Spec.lean` holds the definitions they use.

| Theorem | Statement |
|---|---|
| `transition_total`, `pre190_total` | The kernel never fails. |
| `authorized` | Every write of a successful command is allowed by `Spec.allowed`: a new paste gets the next id, the request's unused slug and the caller's uid, and a paste is deleted only by its owner, after it expired, or when it burns. |
| `delete_needs_owner` | `Delete` succeeds only if one of the caller's uids owns the paste. |
| `removed_only_if` | A paste disappears only if the caller owns it, it has expired, or it is a burn-after-reading paste being read. |
| `inv_preserved`, `reachable_inv` | In every reachable state, ids and slugs are unique and every id is below the counter. |
| `shown_of` | A read shows only the paste whose slug the request names (its text, expiry and burn flag, and whether the caller may delete it), only if it has not expired and the password matched, and a burn-after-reading paste is deleted in the same commit. |
| `never_expired` | An expired paste is never shown. |
| `burn_once`, `burn_never_again` | After a read shows a burn-after-reading paste, no state reachable afterwards holds a paste with its id, so every later read shows another paste. |
| `preview_fixed` | `PreviewSafe transition`. |
| `preview_broken` | `¬ PreviewSafe transition_pre190`: a bot's plain request shows and deletes the paste (`pre190_bot_burns`) in a reachable state (`s0_reachable`). |
| `apply_spec` | The kernel's `apply` computes `Spec.applyAll`, as long as the paste vector cannot overflow. |

`Scenarios.lean` runs today's kernel on small states, so the theorems do not
hold because the kernel refuses everything: the bot gets the confirmation
page, a confirmed read shows and burns the paste and a second read gets
`NotFound`, the owner deletes a paste while a stranger and a caller without a
cookie are refused, a paste is served until its expiry and is gone after it,
purge removes it, and the right password opens a locked paste while a missing
or wrong one is refused.

The proofs trust the engine's clock to report the time, and the shell to draw
slugs at random, to check the cookie's signature and to fingerprint
passwords. All theorems
depend only on Lean's standard axioms.

## Running it

```
cd examples  # its own Cargo workspace; run from the repository root
WASTEBIN_SIGNING_KEY=... DATABASE_URL=postgres://... cargo run -p wastebin-server
curl -c jar -d '{"text":"hi","burn_after_reading":true}' -H 'content-type: application/json' localhost:8088/
curl localhost:8088/<id>                  # {"confirm_burn":true}
curl 'localhost:8088/<id>?confirm_burn=1' # the paste, once
curl -b jar -X DELETE localhost:8088/<id>
```

`tests/postgres.rs` checks the PostgreSQL store against the kernel on random
commands and runs the preview scenario through the HTTP routes.

## Building the proofs

```
scripts/app/extract-wastebin.sh        # regenerates proofs/generated/WastebinKernel.lean
cd examples/app/wastebin/proofs && lake build
```
