# Port targets

Axum apps with real authorization or business rules, to port and verify.
Surveyed 2026-09-25; axum confirmed from each `Cargo.toml`.

| App | Size | Authorization | Past bug | Fit |
|---|---|---|---|---|
| [Kellnr](https://github.com/kellnr/kellnr) (crate registry) | ~12k LOC, 16 routes | admin, read-only users, crate owners/users/groups, restricted downloads | [PR #1243](https://github.com/kellnr/kellnr/pull/1243): read-only sessions could change owners and ACLs | easy |
| [Atuin](https://github.com/atuinsh/atuin) sync server | ~2.6k LOC, 14 routes | per-user records, open-registration flag | [#3297](https://github.com/atuinsh/atuin/issues/3297) (open): account delete without password | easy |
| [Wastebin](https://github.com/matze/wastebin) | ~6.3k LOC | owner-only delete, expiry, burn after reading | [#190](https://github.com/matze/wastebin/issues/190): link previews burn pastes | easy |
| [RealWorld/Conduit](https://github.com/launchbadge/realworld-axum-sqlx) | ~2.1k LOC, 12 routes | author-only edit/delete; check hidden in a SQL CTE | [#16](https://github.com/launchbadge/realworld-axum-sqlx/issues/16): wrong `favorited` flag | easy (axum 0.3, needs update) |
| [crates.io](https://github.com/rust-lang/crates.io) | ~12k LOC controllers, 66 endpoints | owner rights None/Publish/Full, team owners, token scopes, delete rules | [PR #14760](https://github.com/rust-lang/crates.io/pull/14760): locked accounts passed OAuth | medium (team membership from GitHub) |
| [Svix](https://github.com/svix/svix-webhooks) | ~6.6k LOC | org / application access levels | none found | medium |
| [Windmill](https://github.com/windmill-labs/windmill) | ~56k LOC API, 312 routes | workspaces, groups, folders, path-scoped tokens, Postgres RLS | GHSA-2ppx-66jv-wpw5 (token scope bypass), GHSA-x2wf-f962-7frq (cross-workspace resume) | hard; one slice medium |

Not suitable: Lemmy (actix-web), the axum examples (no authorization),
Rauthy and Kanidm (identity-protocol logic).

Ported: the first five. Kellnr and Atuin are small; crates.io adds
credibility; Conduit is the plain-app effort baseline; Wastebin has
time-dependent rules. Svix and Windmill are not ported.

## Results

The ports now live in the h5i-web-app repository.

Effort is kernel lines, spec lines, proof lines.

- Kellnr (2026-09-25), around PR #1243.
  Fixed code: a read-only non-admin commits nothing on any login path; ACL
  and yank writes need an admin or owner; last-owner and restricted-download
  rules. Old code: the theorem fails; a session login adds an owner and a token login grants a
  group. Effort: 329, 60, 466, plus 61 lines of counterexamples.
- Atuin (2026-09-26), at 5b10eb0. User
  isolation, replies hold only the caller's records, account deletion leaves
  no session or record, unique users and names, record size cap. Issue
  #3297: the fixed kernel requires the password; current Atuin does not.
  Effort: 470, 65, 660 (about 1.4 per kernel line).
- Wastebin (2026-09-27), at b27a2ab,
  PostgreSQL server; the shell supplies time and slugs. The kernel never
  fails; only the owner deletes a live ordinary paste; unique ids and slugs;
  a read shows only the requested, unexpired paste; burn-after-reading shows
  once. Issue #190: preview-safe after 632ddf2, reachable counterexample
  before. `/raw` still burns on GET. Effort: 275, 51, 700 (about 2.5).
- Conduit (2026-09-27), realworld-axum-sqlx
  at f1b2565, every route, PostgreSQL server with the RealWorld JSON API.
  Only authors edit or delete their articles and comments; follow and
  favorite writes touch only the caller's rows; unique users, emails, slugs,
  follow and favorite pairs; nothing points at a deleted article. Every reply equals a spec
  function of state and caller, and matches a later read; the feed holds
  only followed authors. Issue #16 (open): upstream's `favorited` query and
  `?favorited=` filter break the reply theorem on a two-article state.
  Effort: 888 plus 170 for `apply` (upstream handlers: 1077), 127, 1611,
  plus 213 lines of scenarios and counterexamples (about 1.5 per upstream
  line).
- crates.io (2026-09-27), at 067b45e,
  PostgreSQL server. Ownership, publishing, yanking, invitations, token
  scopes, deletion. Every write fits a policy over Full and Publish rights;
  tokens stay within endpoint and crate scopes; every crate keeps a user
  owner; versions go only with their crate; deletion rules (age, owners,
  downloads, reverse dependencies); the kernel never fails. PR #14760: after
  the fix a locked user commits nothing; before it a locked sign-in works.
  Team membership, clock and download counts are inputs. Effort: 1026, 142,
  1516 (about 1.5), `lake build` 1:48.
