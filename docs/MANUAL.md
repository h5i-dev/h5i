# h5i Manual

Command reference for h5i, ordered the way an engagement runs. New here? Read
[What h5i is](#what-h5i-is) and [The engagement](#the-engagement) first: they
give the mental model before the per-command reference.

`h5i <command> --help` is always authoritative for flags. This manual explains
what the commands are *for*.

---

## What h5i is

> Give an AI agent a browser it can drive and you can audit. Every request is
> policy-checked and written down before the bytes move, and the fetch is
> refused when the record cannot be written.

*h5i* (pronounced *high-five*) is a red-teaming browser for AI agents. An agent
drives a session by name, reads the page as an outline with `@ref` handles, and
works on the traffic that page produced: inspect it, change one field, send it
again, compare what came back. The page and its traffic are one session, because
the engine *is* the HTTP client.

No other tool has both ends. Playwright and Puppeteer drive a browser and cannot
say what it reached. Burp owns the traffic and not the browser, so it needs
interception, a proxy setting and usually a CA certificate, and it cannot say
*why* a request happened. h5i writes the decision before the bytes move, so a
request that is not in the log did not happen.

One Rust binary, engine included. No proxy, no certificate, no server, no
daemon, no SaaS.

### The division of labour

h5i does the deterministic work; the agent does the judging.

| h5i owns | the agent owns |
|---|---|
| exact capture, stable message ids | which request matters |
| session, cookie and identity state | which parameter to bend |
| structured edit, resend, diff, timing | what a difference means |
| scope, rate, budget, and the refusal | what to try next |
| the record, and refusing to act without one | whether any of it is a vulnerability |

h5i finds nothing and flags nothing. No payloads, no wordlists, no verdicts. A
finding is what the agent writes with `h5i websec finding create`; h5i stores it
and checks that the ids it cites exist.

### Two kinds of record

- **The account.** What was asked for, decided, and returned, in a shape that is
  safe to paste into a bug report: `requests.jsonl`, `actions.jsonl`, the
  receipts, the endpoint ledger.
- **The evidence.** The messages themselves, `Authorization` and cookies
  included. Owner-only, opt-in with `--capture`, in no export unless named.

The two are never merged. Every row also says which lane saw it:
`engine-claimed` is the engine's account of itself, `host-observed` is something
outside the engine seeing it too. A lane is never upgraded.

### What it is not

- **Not a scanner.** No crawl-and-flag mode, no payload generation, no severity
  score.
- **Not a sandbox by default.** A session with no `--in` runs like any other
  headless browser, and `h5i browser status` says so. Containment is a placement
  you ask for.
- **Not a complete browser.** Of twenty single-page applications measured,
  eighteen read usefully and one not at all. Tabs, extensions, Service Workers,
  WebRTC and iframes are absent by decision, and a page needing an API the
  engine lacks gets that API *named* in the snapshot.
- **Not a content filter.** h5i does not classify what a page says. It bounds
  what a persuaded agent can reach.
- **Not a defence against a targeted kernel exploit.** See [Limits](#limits).

---

## The engagement

Sections are named after the commands; the order is the workflow. The join
between phases is the message id.

| Phase | What you are doing | Section |
|---|---|---|
| Scope | Say what is in bounds, and make the engine enforce it | [Scope and identity](#scope-and-identity) |
| Recon | Find what the target exposes, and record how you know | [`h5i recon`](#h5i-recon) |
| Drive | Open a page, act on it, capture what it fetched | [`h5i browser`](#h5i-browser) |
| Test | Change one thing, send it again, compare the answers | [`h5i websec`](#h5i-websec) |
| Evidence | Write the claim down against the ids that support it | [Findings](#findings) |
| Regression | Commit the flow so the bug cannot come back | [`h5i test`](#h5i-test) |

```bash
h5i browser open https://target.example --project acme --capture --script
h5i recon extract                            # what the pages and bundles disclosed
h5i recon known                              # robots.txt, sitemap.xml, security.txt
h5i recon crawl --max-requests 200 --rate 4  # walk it under this session's login
h5i recon triage --calibrate                 # fold the noise, confirm what is real
h5i websec requests                          # the captured messages, by id
h5i websec replay req_42 --set query.id=456
h5i websec diff res_42 res_43
h5i websec finding create --title '...' --evidence req_42,res_43
```

Recon says what exists and websec tests it. Neither needs the other, so you can
enter at any phase.

### Where a box fits

A box is a disposable, confined environment, and nothing above needs one. Reach
for a box when the code under test is untrusted, when a target's response might
be, or when you want the network decision made outside the engine:

```bash
h5i browser open https://target.example --in mybox
```

Same session, same verbs, same record. What changes is who saw the network,
which is why a boxed session can earn the `host-observed` lane and a host
session cannot. See [Containment](#containment-optional).

---

## Install

```bash
curl -fsSL https://h5i.dev/install.sh | sh                      # prebuilt binary
curl -fsSL https://h5i.dev/install.sh | sh -s -- --websec --recon --test  # with the plugins
cargo install --path .                                          # from source
```

The plugins are not in the default install. `--websec` adds the HTTP workbench,
`--recon` the endpoint ledger and `--test` the regression runner; each is
fetched as its own archive and registered with `h5i plugin install`, so `h5i
plugin list` stays the whole truth about what is there.

`h5i.dev/install.sh` and `raw.githubusercontent.com/h5i-dev/h5i/main/install.sh`
are the same file, and CI fails if they ever stop being. Use the second one if
you would rather the install path not depend on the domain.

Then, so your agent knows how to use it:

```bash
h5i skill install           # writes the skill into ~/.claude/skills/h5i (or ~/.codex)
npx skills add h5i-dev/h5i  # same bytes, if you do not have the binary yet
```

---

## Command groups

| Group | What it is for |
|---|---|
| [`h5i browser`](#h5i-browser) | Browser sessions: open one, drive it, capture what it fetched. |
| [`h5i websec`](#h5i-websec) | Read, edit, resend and compare what a session sent. A plugin. |
| [`h5i recon`](#h5i-recon) | What a target exposes, and how h5i knows. A plugin. |
| [`h5i test`](#h5i-test) | Replay portable attack flows and check them with your own oracles. A plugin. |
| [`h5i box`](#boxes) | Create, run, inspect and export boxes. Optional containment. |
| [`h5i box share`](#h5i-box-share) | Open one box's dev server to one other person. The only inbound path. |
| [`h5i join`](#h5i-box-share) | Open a box someone else is sharing, from their ticket. |
| [`h5i runner`](#h5i-runner) | Pair a second Linux machine and run boxes there over SSH. |
| [`h5i ui`](#the-console) | The console: every session and box on this machine, read-only. |
| [`h5i skill`](#h5i-skill) | Write or print the agent skill this binary carries. |
| [`h5i plugin`](#h5i-plugin) | Install what is not in the default build: the workbench, the ledger, the tests. |
| `h5i completion` | Shell completions for bash, zsh, fish and friends. |

`h5i dev *` and `h5i env *` remain hidden aliases for `h5i box *` through one
release.

---

## Scope and identity

Before anything is sent, say what is in bounds. Scope is three separate things:

| Layer | What it answers | Where |
|---|---|---|
| Engagement scope | What the target's owner authorised | `--project`, and [Scope](#scope-the-engagement-kind) |
| Origin grant | What this session may reach at all | `--allow`, on `open` |
| Identity | Who the session says it is | `--identity`, and a cookie jar |

The first two are enforced before the wire, and a refusal is a row in the log
with no bytes behind it. Identity is not a permission: it is what the target
sees, and changing it is how a two-account test is run.

### `--project`: grouping sessions by engagement

A name addresses one session; a *project* names the engagement many sessions
belong to. Because a name is reused and a project is not, the project is what
survives the work:

```bash
h5i browser open https://api.acme.com --project acme --session auth --new
h5i browser list --all --json | jq 'group_by(.project)'
```

It is on the record, so every fold across an engagement (the union ledger, every
finding, every origin reached under any identity) is a `jq` pass over the
directories in [Files](#a-browser-sessions-directory) rather than something h5i
has to ship a verb for.

When `~/.config/h5i/projects/<name>.toml` exists, `--project` also resolves it
as the session's engagement scope. See [Scope](#scope-the-engagement-kind).

### Browser identities

A session uses one identity for HTTP headers, JavaScript, screen geometry, locale,
and time zone. The identity is fixed when the session opens and recorded in its
audit data.

```bash
h5i browser identity list
h5i browser identity check firefox-143-linux --script
h5i browser open https://example.com --script --identity privacy
```

| mode | behavior |
|---|---|
| `native` | Truthfully identifies h5i. This is the default. |
| `privacy` | Uses stable h5i values so installations reveal fewer local differences. |
| `compatible` | Coherently presents another supported browser identity. |

Use `identity show <name>` to print an identity as TOML, or pass a TOML file to
`--identity`. Contradictory identities and identities requiring unsupported
features are refused rather than partially applied. Currently,
`firefox-143-linux` is supported; Chrome identities require client hints and
WebGL capabilities this engine does not provide.

Identity consistency is not anonymity. TLS and HTTP/2 fingerprints, installed
fonts, network location, and input timing remain outside this feature.
### What a refusal looks like

Out of scope is a recorded decision, not a tooling error:

```
$ h5i browser navigate https://blog.acme.com/ --session acme1
Error: denied by policy: `blog.acme.com` is refused by the deny rule
`blog.acme.com`.
```

The request gets an `allowed: false` row in `requests.jsonl`, a response record
describing the refusal, and no bytes between them. Change scope only with
authorisation, and never from inside a box.

---

## h5i recon

Discovery, kept apart from testing. Recon records what a target exposes and how
it knows; calling a difference a vulnerability stays the agent's claim. Design:
`docs/design/design-recon.md`.

```bash
h5i recon extract                             # read what the session already fetched
h5i recon known                               # robots.txt, sitemap.xml, security.txt
h5i recon crawl --max-requests 200 --rate 4   # walk it under this session's login
h5i recon paths --wordlist ./words.txt        # ask for what was never disclosed
h5i recon triage --calibrate                  # fold the noise, confirm what is real
h5i recon endpoints --state confirmed --json  # the inventory, with evidence
```

Every endpoint carries a state, and the states are the point:

| State | What it means |
|---|---|
| `candidate` | Something disclosed it. No request was ever sent. |
| `observed` | A request answered, and the row names the message. |
| `confirmed` | The answer differs from what that directory says about a path that is not there. |
| `refused` | Policy declined it. Kept, because it is a fact about the scope. |
| `gone` | Confirmed once, and now answering like a missing path. |

Confirmation happens only in `triage --calibrate`, which learns what a missing
path looks like in each directory. Against an application that answers `200`
for everything, nothing is confirmed without it.

h5i ships no wordlist and generates no payloads: `paths --wordlist` takes a list
you bring, `--reuse-words` uses the words the session has already seen, and
`recon import --format urls|katana|subfinder|httpx|openapi` reads a file another
tool produced as candidates that stay candidates until an h5i request answers.
`recon export` writes the inventory as JSONL; `recon merge --from <session>`
folds another session's ledger in, keeping each identity's observations apart.

Runs that spend requests are jobs: `h5i recon jobs list`, `jobs show`, and
`jobs resume`, which re-runs the same parameters and skips what the ledger has
already answered. The ledger is written as a run goes, so a run that is killed
keeps what it found.

---

## h5i browser

A *session* is the whole agent-facing surface. `h5i browser open` makes one,
every verb that follows acts on it, and `h5i browser close` ends it. Nothing
else is a concept an agent has to learn: not the process that renders the page,
not the port it listens on, not whether it is running inside a box, and not, in
the ordinary case, the session itself.

```bash
h5i browser open https://example.com
h5i browser snapshot            # the page as a model should read it
h5i browser click @e3
h5i browser screenshot          # a PNG of the page, into the session's artifacts
h5i browser reload              # re-fetch where the session actually is
h5i browser requests            # what it asked for, and what was refused
h5i browser close
```

`screenshot` writes into the session's own artifacts directory under a name *h5i
chooses*; the engine picks only the bytes. `--out` names a file instead. Like
every other verb that reads the page, it is refused while `login` is on: a
password is pixels before it is anything else.

### Which session a verb acts on

Every session has an opaque id (`br_7k2xqa`), and it is in the record, in
`--json` and in the receipts, because a durable reference has to be something no
rename can break. It is not what you type. So a verb resolves its session in three steps, most explicit first:

1. `--session <name>` (`-s`), a name someone chose, or an id pasted from
   `--json`
2. `$H5I_BROWSER_SESSION`
3. the default: the session `open` last made

Running several at once is what names are for:

```bash
h5i browser open https://example.com/login --session auth   --new
h5i browser open https://example.com/      --session public --new
h5i browser snapshot --session auth
h5i browser list                       # the default is the row marked `*`
```

A name is comfortable to type precisely because it is not an identity: it can be
reused once the session it named has ended. The id cannot, which is why the id
is what gets written down, and why `--restore` takes one.

### `open` navigates a session that is already there

Opening a URL in a browser that is already up means *go there*. So `open`
navigates the session it finds, and `--new` is how you say you meant a second
one. The flags that only make sense at creation (`--allow`, `--project`, `--in`,
`--script`, `--no-loopback`, `--permissive-cors`, `--expires-in`, `--restore`,
`--cookie-jar`, `--capture`) are *refused*
rather than ignored when a session is reused: a session's policy is fixed when its engine starts, so
accepting a grant and doing nothing with it would be a grant the caller believes
it made.

### Defaults

With no flags a session runs on this machine in your ordinary process space,
like any other headless browser. There is no sandbox and h5i does not claim one.

What it adds is the record. The engine is the HTTP client, so every request is
checked against the session's policy and written down *before* the bytes move,
and the fetch is refused when the record cannot be written. A request that is
not in `h5i browser requests` did not happen. That holds whether or not there is
a box:

```
requests : engine-claimed (fail-closed, and the engine's own account of what it fetched)
```

#### The page grants itself

A session reaches the URL it was opened on and nothing else remote. `--allow` is
for origins beyond it: an API the page calls, a CDN it pulls from. Loopback is
reachable by default because it is the dev server; `--no-loopback` takes that
back.

#### Cross-site credentials

By default a page here may not send this session's credentials to another origin
on a request whose answer nobody can read: `mode: "no-cors"` with
`credentials: "include"` is refused, because an opaque response cannot be
checked.

That is right for containing an agent and wrong for testing a target, since the
refused shape is the classic POST-based CSRF. With it in force h5i cannot act as
the *victim*, so a negative result means "h5i declined", not "the target is
safe". `--permissive-cors` lifts it for one session:

```bash
h5i browser open https://attacker.example --script --permissive-cors
```

It is in that session's policy digest and named on the `open` banner and in
`h5i browser status`, so no finding gathered under it can be mistaken for one
gathered without it. A cross-origin `cors` read still needs the server's
permission, and `mode: "same-origin"` still refuses to cross.

It puts no credential where there was not one. The jar holds only what the
loaded page could itself send: a `Domain=` cookie keeps its scope, everything
else is dropped on navigation, so a cross-host attack page has nothing of the
target's to send. Two ports on one host are two origins and one jar.

### `read`: one page, no session

```bash
h5i browser read https://example.com
h5i browser read url1 url2 url3 --json
```

```
confined : process (files and environment; the origin allowlist is the engine's)
```

For the shape a crawl has: fetch, read, move on. No cookies carried between
verbs, no `@ref` to click, nothing resident afterwards. The targets grant
themselves and only themselves, so a page pulling a script from a third-party
CDN, or redirecting to another host, is refused and says so in the log.

`--allow ORIGIN`, repeatable, is for when that refusal is the problem rather
than the point: a page written in a library served from a CDN, read without the
grant, is the page the library never ran on. Inside a box it can only narrow,
because the box's egress list is enforced outside the engine.

Several targets share one browser, one connection pool, one cookie jar and one
font set, and a page that fails does not stop the ones after it. `--json`
returns the page, its request log, and what was holding the engine together.

#### `--in <box>`: an allowlist a tier enforces

An allowlist that is not simply "what I asked for" belongs in a file. Write it
in `.h5i/env.toml`:

```toml
[profile.docs]
isolation = "supervised"

[profile.docs.net]
mode   = "host"
egress = ["docs.rs", "*.rust-lang.org"]
```

```bash
h5i browser read https://docs.rs/serde --in docs --json
```

```
confined : box docs, policy 6bca3b30c268
```

The tier resolves the pinned policy, enforces egress at a network namespace
boundary outside the engine, and writes a receipt. The digest on that line is
the policy actually enforced, which an allowlist assembled from command-line
arguments could never hand back.

A read can have this and a session cannot. A session is resident by design, and
the supervised tier cannot hold a resident process yet: its seccomp-notify gate
is served by a thread inside the `h5i` process that started the run, so when
that command exits every filtered syscall blocks. A read runs to completion
inside that command.

Aim a read at `localhost` and use no box: under a tier with its own network
namespace the loopback is the sandbox's, not your dev server's.

### Reading and acting, beyond `snapshot` and `click`

```bash
h5i browser structured                          # what the page says about itself
h5i browser transcript                          # what its media says, from `<track>`
h5i browser markdown --url https://example.com  # go there and read, in one trip
h5i browser find  --role button --name 'Sign in'
h5i browser click --role button --name 'Sign in'
h5i browser set-checked @e4 true
h5i browser select @e5 'Express shipping'
h5i browser press  @e1 Enter
h5i browser script --save flow.json
```

`structured` is the cheapest read: JSON-LD, OpenGraph, `<meta>`, `<link rel>`, a
few hundred bytes where a snapshot is a few hundred lines. A page with no
metadata answers `empty`, which is a fact about the page rather than a failed
read. Every read verb takes `--url`, which goes there first and then reads: one
round trip where `navigate` and then the read would be two, and the reply still
names the URL it ended up on so a redirect is not silent.

`transcript` reads the hole the other verbs leave. A `snapshot` names a
`<video>` and `markdown` skips it, so a page whose substance is a forty-minute
talk reads as a title and a play button. Most players ship a `<track>`, and a
caption file is prose with timestamps: the shape a model reads well, and the one
audio is not.

```console
$ h5i browser transcript --url https://example.com/talk
url: https://example.com/talk
media: 1 element(s), 1 with timed text, 412 cue(s) read
--- BEGIN UNTRUSTED PAGE CONTENT ---
…
--- END UNTRUSTED PAGE CONTENT ---
```

### When the page acts on its own

With `--script`, a page can move the ground under a verb, and both ways it can
are reported rather than left to be inferred.

**A form the page submits itself.** `form.submit()` from a handler, or a form
that submits on load, produces a real request. It is not sent from inside the
page, because this engine drives navigation through its own verbs so that an
agent and a receipt both see it. It goes out at the verb boundary instead,
through the broker and into the request log like any other. The reply carries
`page_submitted` with where it went, and the session has landed on the answer by
then, so take a new `snapshot`: every `@ref` you hold describes the page that is
gone.

**`load` and `error` on subresources.** An `<img>`, `<script>`, `<link>` or
`<iframe>` that did or did not arrive fires at the element that asked for it, so
`<img src=x onerror=…>` and `<svg onload=…>` behave the way they do in a
browser. An element whose only interactivity is a handler attribute, such as
`<div onclick=…>`, reads as role `clickable` and takes a `@ref`, which is how
you fire one.

### Video transcripts

`snapshot` and `markdown` describe a media element but do not decode its
audio. Use `transcript` to read caption tracks declared by the page:

```bash
h5i browser transcript --url https://example.com/talk
h5i browser transcript --url https://example.com/talk --lang en
```

Caption files are fetched through the browser broker, so normal origin policy
and receipts apply. h5i reads at most one language track plus a chapters track.
Media without captions is reported explicitly.

Some sites expose captions only through player APIs. The optional yt-dlp helper
handles those sites:

```bash
h5i browser transcript --via yt-dlp   --url https://www.youtube.com/watch?v=…
```

This helper is never an implicit fallback. It opens its own connections, so its
traffic cannot appear in the engine request log; instead, the exact helper
command and its host-observed result appear in the audit. It runs in the
session's placement, receives no browser credentials, ignores user yt-dlp
configuration, and has a two-minute default budget. Set
`H5I_HELPER_BUDGET_SECS` to override that budget.

Use an exact language tag such as `--lang en`, or an intentional pattern such
as `--lang 'ja.*'`. Automatic captions are labeled as such. The `ytdlp`
build feature, enabled by default, controls whether this helper path exists.
### h5i browser view

Watch the page and take the controls, without a box.

```bash
h5i browser view                      # draw it in this terminal
h5i browser view --web                # serve it to your browser instead
h5i browser view --session auth       # when more than one is open
```

`h5i box view` reaches the same viewer for a session in a box. The difference
that shows on screen is what the status line can claim: a boxed session's egress
is enforced outside the engine, while a host session's rests on the engine's own
word, so it reads `engine-claimed` rather than naming a box. Watching changes
neither; `--in` is what makes the claim checkable.

The keys are the ones under [h5i box view](#h5i-box-view).

### The control lock

Two clients can drive one page: the agent, and a human at the live view.

- The agent holds control by default. A session exists to let an agent work; it
  should not have to ask.
- A human takes control, never asks for it: `h5i browser take <session>`, or by
  reaching for the controls at either live view, which takes it for you. The
  agent's mutating verbs are refused with a typed message rather than fighting
  for the pointer; read-only verbs keep working, because watching never
  collides.
- Handing control back invalidates what the agent knew. The page moved, so every
  `@ref` from its last snapshot may point somewhere else. It must re-snapshot
  before acting, and acting first is refused rather than mis-clicked.

`take` says which kind of pause it just created, because the two are genuinely
different:

- In a box: enforced. Every verb is carried in from the host, and none of them
  is now.
- On this machine: advisory. It pauses `h5i browser` and nothing else. An agent
  that drives the engine binary directly is not stopped by it.

### Sessions end, and endings are recorded

Closing a session writes the ending into its record instead of deleting it, so
"how did this end" stays answerable and the id can never be reused.

| state | what happened |
| --- | --- |
| `live` | started, and the engine answered the last time anyone looked |
| `closed` | ended by `h5i browser close`; the record is complete |
| `died` | the engine stopped without being asked; the record has a gap and says so |
| `expired` | outlived `--expires-in` |
| `evicted` | the box holding it was removed |

A verb sent to a session that is not live is refused with exit code 69
(`EX_UNAVAILABLE`), never silently restarted:

```console
$ h5i browser snapshot
browser session `br_7k2xqa` was closed: closed by the user. It will not be
restarted automatically. Start a new one with `h5i browser open <url>`, or
carry this one's storage forward with `h5i browser open <url> --restore br_7k2xqa`.
$ echo $?
69
```

The distinct code matters: an agent whose retry cannot tell "the session is
gone" from "the click did not work" starts a second browser and loses both the
page and the record of how it lost it.

`--restore` is an inheritance, not a resurrection. It produces a new id, writes
`restored_from` into the new record, and carries the *cookie jar* and nothing
else. The jar is mirrored into the session directory whenever it changes, so a
login a human performed once at the live view survives:

```bash
h5i browser open https://example.com/login --session auth
h5i browser login --session auth        # the human types the password
h5i browser login --session auth --off
h5i browser close --session auth

h5i browser open https://example.com/app --restore br_7k2xqa   # still signed in
```

No verb returns a cookie value: the file is handed to the next engine, never to
a model. A session that left no jar is refused by name rather than silently
seeding nothing.

`--cookie-jar <path>` seeds the same jar from a file, which is how a login this
engine cannot perform gets in: a human signs in with their own browser and
pastes the cookie into `{"version": 1, "cookies": [...]}`. Both flags write
before the engine starts. A row no server could have set, such as a `__Host-`
name without the flags that name means, is refused and counted on stderr, so the
`restored N cookie(s)` line is the check that the login carried.

### Everything a session returns is untrusted

The page composed the title, the link text, the error message and the URL; the
engine only carried them. So every answer h5i relays is scrubbed before it
reaches a terminal or a model: escape sequences never survive, other control
characters are removed, and strings, arrays and nesting are capped with the
truncation stated in the value rather than performed quietly.

Escape sequences matter most. `ESC` in a relayed string is a page rewriting the
terminal it is printed into: moving the cursor over the line above, hiding what
it just did, repainting a prompt. Nothing a browser has to say needs `ESC`.

Files a session produces are named by the host, never by the session, and land
under the session's own `artifacts/` directory.

### Where sessions live

`$H5I_BROWSER_HOME`, else the box's own `/tmp` when h5i is running inside one,
else `$XDG_STATE_HOME/h5i/browser`, else `~/.local/state/h5i/browser`.

Deliberately *not* under a git repository: every other noun in h5i stores its
state under the enclosing repo because every other noun is about a repo, and a
browser is not. `h5i browser open` in an empty directory is the ordinary case.

The box case is not a preference. Inside a box `$HOME` is the host's path over a
sealed overlay and `~/.local/state` is not writable, so a session there would
fail to start; the box's `/tmp` is private to it and lives exactly as long as
its sessions can.

The default session is per registry, so two agents sharing a `$HOME` share it.
Give each its own with `$H5I_BROWSER_HOME`, or give each session a `--session
<name>`.

| variable | what it names |
| --- | --- |
| `H5I_BROWSER_HOME` | the session registry's directory |
| `H5I_BROWSER_SESSION` | which session a verb acts on, when `--session` is not given |
| `H5I_BROWSER_ENGINE` | the engine binary on this machine |
| `H5I_BROWSER_ENGINE_IN_BOX` | the engine command inside a box, when the box's `PATH` is not where it is |

The last two are separate on purpose. Mixing them points one side at a path the
other cannot see.

### Choosing the engine

`--engine` selects the browser engine and records that choice in the policy
digest.

| engine | use |
|---|---|
| `chromium` | Default; broadest web compatibility. |
| `lightpanda` | Third-party lightweight headless engine. |
| `h5i` | Built-in, auditable engine optimized for agent reading and actions. |

The h5i engine is smaller than Chromium but slower on script-heavy pages. It
supports JavaScript, redirects, cookies, policy-checked subrequests, page
outlines, screenshots, forms, and common agent actions. It is intentionally not
a complete browser: unsupported APIs are named in snapshots and console errors
rather than silently approximated.

Use `--script` only when the page needs JavaScript. For maximum compatibility,
choose Chromium. Chromium runs with an isolated profile and the box's policy,
but its internal requests are not the built-in engine's broker receipts; inspect
the box-level network evidence instead.

### `audit`: the whole session, in one timeline

`requests` is the network layer on its own, and it is the verb to reach for in a
loop. `h5i browser audit` is the one to read afterwards: what the agent asked
for, what the engine decided about every fetch, who was driving, and how the
session ended, merged and ordered.

```console
$ h5i browser audit
  sources  : actions read · requests read · control read
  note     engine rows are ordered by the engine's own clock, which h5i cannot verify

  host    session opened  (http://localhost:3000/ — on this machine, no containment…)
  engine  #0 GET http://localhost:3000/
  engine  #0 200  153 bytes
  engine  verb   snapshot
  host    control -> human  (taken by a human)
  host    control -> agent  (handed back; the agent must re-snapshot)
  engine  verb   snapshot
  engine  #1 DENIED GET https://tracker.example/px  (origin is not in the allowlist)
  engine  verb ! click @e1 — denied by policy
  host    session closed  (closed by the user)
```

Three things this does that neither log does alone:

- **The two lanes stay apart.** Action and request rows are the engine's account
  of itself; handovers and lifecycle are h5i's, written from outside. Every row
  says which.
- **It orders across sources.** "Was a human at the controls when that form was
  submitted" is a question about two logs at once. The engine stamps its own
  rows, h5i stamps its own, and the output says the engine's clock is the
  engine's claim.
- **It says what it could not read.** `sources` reports each log as `read`,
  `empty` or *`unavailable`*. An empty timeline over a log h5i cannot see looks
  exactly like a session that did nothing, and those are different findings.

Rows carry `caused_by` where the source recorded the link. Nothing infers a link
from timing: a request that merely happened near a verb is not one that verb
caused.

`--json` gives the whole thing, including the session record. It is the same
structure `h5i box export` writes for each session placed in a box.

## h5i websec

The HTTP workbench: read what a session sent, change a part of it, send it
again, and compare the answers. Design: `docs/design/design-websec.md`.

```bash
h5i browser open https://target.example --capture
h5i websec requests                              # captured messages
h5i websec show req_42 --raw                     # one message, exactly
h5i websec replay req_42 --set query.id=456      # edit and resend
h5i websec diff res_42 res_43                    # compare two answers
h5i websec match res_43 --status 200 --contains ok
h5i websec experiment ./plan.json                # many sends, folded to clusters
h5i websec finding create --title … --evidence req_42
```

Capture is opt-in (`--capture`) because the message store holds bodies and
credentials in full. It is never included in an export unless it is named.

### Experiments

One request sent many ways, with the answers folded into clusters. The plan
names what varies; the values are yours, and nothing here generates one.

```json
{"request": "req_42",
 "positions": [
   {"name": "user", "target": "query.user", "values_file": "users.txt"},
   {"name": "role", "target": "json.role", "values": ["user", "admin"]}],
 "strategy": "product",
 "baseline": "res_42",
 "extract": {"error": "regex:SQL error: (\\w+)"},
 "rate": 4}
```

`product` sends every combination and `zip` takes the nth value of each
position together. Responses group by status, type, redirect target, size and
*what the body says*, so two answers of the same shape and length stay apart;
five hundred sends come back as a handful of rows, each naming every message it
folded. `"as": "other-session"` sends under another identity, and the results
are read from that session's store.

The report counts `planned`, `answered` and `read` separately, and `ok` is true
only when all three agree. A step with no answer is a request the engine did
not make, and the usual reason is the page's allowance of 500 requests per
navigation: add `"reset_budget": true`, or split the plan. A walk cut short
reads exactly like a walk that found nothing, so it says so instead.

The ceiling is 1000 sends per experiment, and `--rate` is a ceiling on what the
target sees, between one an hour and as fast as the wire allows. Both are also
on `h5i browser resend`, as `--walk` and `--rate`.

### Findings

What the agent concluded, and the evidence it stands on.

```bash
h5i websec finding create --title "cross-tenant invoice read" \
    --state "verified once" --evidence req_42,res_43 --repro ./exploit.json
h5i websec finding update finding_1 --note "only on the JSON endpoint"
h5i websec finding list
```

The log is append-only, so what was believed at turn 30 is still readable at
turn 300. The title, state and repro replace; notes and evidence accumulate.
`--state` is free text: h5i does not read it, so h5i does not restrict it.

h5i asserts one thing here, that every message id cited is a message this
session holds. Whether the claim is true is the agent's to say. Findings live
beside the message store, owner-only, and are never in an export.

`h5i browser rpc --stdio` is the same verbs over one process: one JSON object
per line in, one per line out, ids matched. A loop that sends hundreds of
requests pays process startup once instead of every time.

---

### What is safe to paste, and what is not

The capture store holds `Authorization` headers and session cookies in full. It
is the one artifact h5i keeps that is *not* safe to paste: owner-only, never in
an export unless named, never rendered by the console. The request log is the
part you can paste. See [Receipts](#receipts) for which lane observed what, and
[Files](#a-browser-sessions-directory) for what is on disk.

---

## h5i test

A finding that is fixed and never tested again comes back. `h5i test` replays a
flow you wrote and asks *your* oracle whether the property still holds.

```bash
h5i plugin install test
h5i test --target https://staging.example --openapi openapi.json
```

It does not decide what a secure response means. There is no assertion
language: `jq`, `grep`, `diff` or your application's own test client may be the
oracle. Exit 0 means the property held, 1 means it did not, and anything else
means the test could not decide. That third answer is never silently a pass.

### Where tests live

Tests are strict YAML or JSON with `version: h5i.test/v1`, under
`.h5i-tests/tests`, committed with `.h5i-tests/oracles`. That directory sits
outside `.h5i/`, which is local state and gitignored.

A request template carries a method, a target-relative path, headers and a body.
`${name}` uses a value an earlier step extracted; `${env.NAME}` reads a CI
variable. Unknown and missing fields are errors, not ignored configuration.

Actors name isolated sessions and cookie jars, which is what makes a
two-identity authorisation test portable. A step sends a template as an actor,
may apply the websec edit language, saves its response under a stable name, and
may extract a regex, JSON field, header or status for later steps. Cleanup runs
after the oracle even when it fails.

### What the oracle is given

The oracle runs with its working directory set to the test file's directory:

| Variable | What it holds |
|---|---|
| `H5I_TEST_RESULT` | The structured JSON evidence bundle. |
| `H5I_TEST_ARTIFACTS` | Its owner-only artifact directory. |
| `H5I_TEST_INPUTS` | The saved response names the test declared, comma separated. |

### Coverage, and what it counts

A send may declare the OpenAPI operation and mutation class it exercises. It
counts only when the flow completed *and* the oracle returned 0 or 1; setup and
cleanup sends without `covers` never count.

With `--openapi`, h5i reports oracle-checked operation coverage, report-only
unless `--min-coverage` is given. Without an OpenAPI denominator it reports no
percentage and refuses a minimum rather than inventing one.

Every run writes `result.json`, `junit.xml` and per-response artifacts, into an
owner-only directory because response bodies are evidence. Credential request
headers are redacted from exported request JSON; response bodies stay exact for
the oracle and are never uploaded.

---

## Containment (optional)

Nothing above this line needs a box. Requiring one up front would fail
hello-world on CI, under AppArmor, on macOS and in a container, for nothing the
record does not already give.

Reach for a box when:

- The code under test is untrusted, or an agent wrote it.
- A target's response might be, and a parser bug should land somewhere
  disposable.
- You want the network decision made *outside* the engine. Only this one changes
  what the record is worth: a boxed session can earn the `host-observed` lane.

`h5i box run -- h5i browser open` is ordinary composition and `--in` is sugar
over the same placement. The box is a separate surface, not the browser's
implementation detail.

### The default sandbox

A local session runs in a process-tier sandbox that confines files, environment,
syscalls, and resources. It does not enforce the browser origin allowlist at the
network boundary; the engine enforces and records that policy itself.

```
placed : on this machine, in a process-tier sandbox
         (files and environment; not its network)
```

The browser broker owns policy, credentials, cookies, budgets, and receipts. A
separate renderer parses and executes page content. The renderer receives only
the responses the broker has authorized, and a renderer crash ends the session.

### `--in <box>`: the same session, inside a box

```bash
h5i browser open http://localhost:3000 --in mybox
```

This places the resident browser in the named box and records the box policy
digest. On Linux, resident sessions currently require a tier that can keep the
engine alive; use `browser read --in` when you need a one-shot read behind the
supervised tier's network allowlist. A microVM can provide both residence and a
network boundary.

Inside a network namespace, `localhost` means the box, not the host. Ensure the
engine binary is installed inside the box or configure
`H5I_BROWSER_ENGINE_IN_BOX`.
### Opening a session from inside a box

An agent already in a box cannot use `--in`: it means "put this session in a box
I am outside of", which is what lets it promise an enforced takeover and a lane
the engine did not claim for itself. From inside neither is true, so it is
refused rather than quietly doing something weaker.

Open it without the flag. It runs beside the agent, in the same box:

```
placed   : this machine, which is box env/human/web (its policy is not readable from in here)
requests : engine-claimed (fail-closed, and the engine's own account of what it fetched)
```

The box is *named*, because a session there is not uncontained. Nothing is
claimed about what the box enforces, because the policy is host-side and sealed:
from in there h5i cannot read its own boundary.

The control channel inside a box is a Unix socket, not a loopback port: a box's
netns may have no usable loopback at all, and every `h5i box run` gets a fresh
one.

### Boxes

A box is a disposable environment holding a checkout, a toolchain, a dev server
and, on request, the browser session itself. Nothing of your machine is inside
it, egress is an allowlist enforced at its boundary, and the only way out is the
[output gate](#h5i-box-export).

### Making a box

```bash
h5i box .                       # snapshot this repository at HEAD
h5i box --pr 1234               # a pull request (number, #number, or URL)
h5i box https://github.com/o/r  # clone an external repository
h5i box --new                   # an empty box; the agent builds from nothing
```

`h5i box [SOURCE]` is shorthand for [`h5i box create`](#h5i-box-create), and
takes the same flags. A pull request is `--pr`, not a positional: a bare number
is ambiguous with everything else a source could be, and `h5i box create`
already spelled it as a flag.

Where the code comes from decides the shape of the box:

- *This repository* → a real git worktree on its own branch, sharing the object
  store, so `h5i box apply` can land it back locally.
- A URL, a PR, or `--new` → a *detached* box. It gets a repository of its own
  inside its directory, this repository is neither read nor written after
  creation, and the inherited `origin` remote is dropped so the box cannot reach
  a network handle nobody granted it. `apply` and `rebase` refuse and point at
  `export`. This is the shape external code should always arrive in.

### h5i box create

```
h5i box create <NAME> [--from <rev>] [--pr <n>] [--clone <url>] [--new]
                      [--profile <p>] [--isolation <tier>] [--image <img>]
                      [--engine <chromium|lightpanda|h5i>]
```

The base revision is frozen at creation and pinned immutably. The policy is
resolved, digested and stored *before* any state is created on disk, so an
unsatisfiable request fails closed rather than leaving half a box behind.

| Flag | Meaning |
|---|---|
| `--from <rev>` | Base revision (default `HEAD`). |
| `--pr <n\|url>` | Fetch `refs/pull/<n>/head` and pin it as the base. Needs only `git`. |
| `--clone <url>` | Copy an external repository in. Detached. |
| `--new` | Empty box (a fresh repository with one empty commit). Detached. |
| `--profile <p>` | See [Profiles](#profiles). |
| `--isolation <tier>` | See [Isolation tiers](#isolation-tiers). |
| `--image <img>` | Base image for `isolation=container` and `isolation=microvm`. Pre-pulled; runs never pull. |
| `--engine <e>` | Browser engine for the `browser` profile: `chromium` (default), `lightpanda`, or `h5i`. Pinned in the digest; never falls back. See [Choosing the engine](#choosing-the-engine). |

A profile can also refuse individual browser actions, enforced by h5i on the
daemon's control socket rather than advised:

```toml
[profile.browser.browser]
deny = ["evaluate", "state"]   # a bare family name covers state_save/state_load
```

`evaluate` is arbitrary code in the page; `state_*` and `credentials_*` reach
the browser's stored secrets. A denied verb never reaches the browser, and the
refusal lands in the receipt's `browser-proxy` lane. This is enforcement against
an agent using the documented path, not containment against one that goes
looking: the daemon runs inside the box, and a box has no internal privilege
boundary.

### Working in a box

```bash
h5i box ls                            # every box on this clone
h5i box status <name>                 # policy actually enforced, evidence, base drift
h5i box run <name> -- cargo test      # one command; the exit code passes through
h5i box shell <name>                  # interactive confined session
h5i box diff <name>                   # what changed against the pinned base
h5i box log <name>                    # the box's event log
```

`h5i box shell` is the agent-in-box: stdio is inherited, so every command the
session spawns is contained by the box rather than by the agent choosing to wrap
each call.

### h5i box detect

Runtime detection: what an eBPF collector in the kernel saw inside a box.
Read-only, and available on every build, because the verbs are how you find out
why the collector is *not* working, so gating them behind it would hide the
answer from the hosts that need it.

```bash
h5i box detect probe                  # can this machine watch a box, and if not, why
h5i box detect rules                  # the whole signature catalogue
h5i box detect rules --filter secret  # one family, or one rule id
h5i box detect show <name>            # what fired in this box, worst first
h5i box detect show <name> --min alert
```

Turn it on per profile with `[profile.<name>.detect] enabled = true`; see
[Runtime detection](#runtime-detection) for the section and what it costs.

### Services and ports

```bash
h5i box service start <name> <service>   # a declared long-lived process
h5i box service status <name>
h5i box service logs <name> <service>
h5i box ports <name>                     # the per-box dynamic port map
```

Services are declared in `.h5i/env.toml`:

```toml
[service.web]
command = "npm run dev"
port = 3000
```

Supported at the `workspace` and `process` tiers in v1. At `supervised` and
`container` the network namespace belongs to a single session, so run the dev
server inside the same `h5i box shell` as everything else.

### h5i box export

The output gate. A box has no write access outside itself; this is the only way
out, and it is deliberately a human step.

```bash
h5i box export <name> --out ./review
git apply --3way ./review/patch.diff
```

| File | What it is |
|---|---|
| `patch.diff` | The tree diff against the pinned base, path-validated: no symlink escapes, no nested `.git`, no agent-introduced gitlinks. |
| `report.md` | What ran, what the browser saw, what the kernel saw, who was at the controls, and the agent's own proposal. |
| `receipt.json` | Every observed execution, with the policy digest that was enforced. |
| `receipts/<id>.raw` | Each ingress session: who connected, over what path, for how long, how much moved, what was refused. Present when the box was shared. |

It refuses rather than overwrites a non-empty directory (`--force` to replace).
Secret redaction and size caps apply throughout.

Read `report.md` before applying. In order: denied egress attempts, every
command with its lane and exit code, what the browser saw (console errors,
uncaught exceptions, failed requests, observed by h5i rather than reported by
the agent), what the kernel saw when runtime detection was on, viewer sessions
including whether a human took the controls, and the agent's proposal.

`h5i box apply <name>` lands a proposed box onto its parent branch in this
repository instead. It refuses for a detached box.

### h5i box cache

Cold dependency install is the difference between a 20-second box and a
four-minute one.

```bash
h5i box cache ls              # caches for this project, and whether they are stale
h5i box cache mounts          # exactly what a box would get
h5i box cache refresh <eco>   # populate one, in a dedicated box with no agent in it
h5i box cache rm <eco>
```

What makes it safe rather than merely fast:

- One cache per project and ecosystem, keyed by a digest of that ecosystem's
  lockfiles. A cache whose key no longer matches is listed stale and never
  handed to a box.
- Mounted *read-only* into an agent box. Every package manager falls back to
  fetching what it cannot find, so this costs no correctness.
- Written only by `h5i box cache refresh`, which runs the install alone with
  egress narrowed to the registry hosts and no agent inside. It needs a
  project-declared profile for that, and refuses with the profile written out
  ready to paste rather than creating a box whose fetch could not work.

No mutable surface is ever shared between an agent box and anything else.

### h5i box view

```bash
h5i box view mybox
h5i box view mybox --web
```

The viewer combines the rendered page with action, network, console, and policy
events. Each row keeps its observation source and evidence grade separate; h5i
does not infer causal links that are absent from the event stream.

| Key | Does |
| --- | --- |
| `j` `k` | Scroll a line |
| `d` `u` | Scroll half a page |
| `space` `b` | Scroll a page |
| `gg` `G` | Top, bottom |
| `f` | Label everything on screen, then follow the one you type |
| `F` | Label the fields, then type into the one you choose |
| `yf` | Label everything, then copy that link |
| `gi` | Type into the first field on the page |
| `yy` | Copy this page's URL |
| `H` `L` | Back, forward |
| `r` | Reload |
| `i` | Hand the keyboard to the page, where an engine can use it |
| `Esc` | Return it |
| `D` | The console pane: what the page logged and what it threw |
| `?` | The key list |
| `q` | Leave |

A viewer attaches read-only. It does not weaken the box policy, publish the
browser port, or become part of the agent's session.
### Where the engine lives

The engine is part of the `h5i` binary, run as a separate process by execing
itself. Its own CLI is reachable but hidden, and bypasses session names,
placement, the control lock and the audit:

```bash
h5i __engine open https://docs.rs/ --allow docs.rs   # one-shot render, then exit
h5i __engine doctor                                  # what fonts it found
```

### Inspecting what happened

```bash
h5i box probe                       # what this host can enforce at all
h5i box capabilities <name> --json  # what this box actually got
h5i box doctor <name>               # can it still enforce its claim? are its refs intact?
h5i box secrets <name>              # declared grants, dry-run resolution, never values
h5i box inspect <name> --capture <id>
h5i box compare <a> <b>             # boxes side by side
h5i box watch <name>                # policy decisions, one line each, as they happen
h5i box watch <name> --deny-only    # only what was refused
```

`h5i box watch` is the tail of the receipt, not a viewer: no viewport, no panes,
no control lock. Pipe it, grep it, leave it in a second pane. Every row names
the lane that observed it and the grade of that evidence, in words, never by
colour alone:

```
09:14:02  box  fail-closed  request   allow  GET https://docs.rs/blitz/  #41 subresource
09:14:02  box  fail-closed  response  200    #41 12.0 KB, 84ms
09:14:03  box  fail-closed  request   DENY   GET https://telemetry.example.com/collect  #43
09:14:03  box  fail-closed  policy           telemetry.example.com: not in net.egress   (<- #43)
```

`--deny-only` keeps a refusal's *pair*: the request row carries the method and
URL, the verdict row the reason. `--json` emits the same event envelope the
console reads, one object per line.

Only h5i's own engine writes a live request log, and an image-backed tier keeps
it out of the host's reach. `watch` says so in its header rather than leaving an
empty screen.

### Lifecycle

```bash
h5i box rebase <name>       # re-pin onto the parent branch's current tip
h5i box abort <name>        # stop; manifest and workspace preserved for forensics
h5i box rm <name> [--force] # remove entirely
h5i box gc                  # reclaim applied/aborted workspaces
```

### h5i box allow

```bash
h5i box allow                 # list the current entries
h5i box allow api.example.com
```

A persistent, user-level egress allowlist merged into every container-tier box
whose profile *already* sets `net.egress`. A deny-all profile is never widened.
Stored under `~/.config/h5i/`, outside every box-granted path, and it refuses to
run inside a box.

---

## h5i box share

Share one port from a running box without publishing that port directly.

```bash
h5i box share <name> [--port 3000] [--expire 60m] [--label alex]
h5i box share <name> --direct-only
h5i box share <name> --tunnel
h5i box share status <name>
h5i box share grant <name> --label sam --expire 30m
h5i box share revoke <name> <grant>
h5i box share stop <name>
```

The recipient runs `h5i join -` and supplies the ticket on stdin. Passing the
ticket as an argument also works, but exposes it to shell history and the process
list. Treat a ticket like a password: possession is authorization, forwarding it
admits another person, and h5i stores only its hash.

Peer-to-peer mode is end-to-end encrypted and may use a relay that sees endpoint
addresses, timing, and volume but not content. `--direct-only` refuses relay
fallback. `--tunnel` creates a normal browser link through Cloudflare; Cloudflare
terminates TLS and can read that traffic. Tunnel mode requires `cloudflared`.

A share needs a live box session. On Linux the box must have a usable network
namespace; h5i refuses configurations where it cannot distinguish the box's port
from the host's. On macOS h5i verifies that the listening process belongs to the
box and repeats that ownership check for every connection.

The share credential moves from the first URL into an HttpOnly cookie, and h5i
removes it before forwarding the request to the app. Proxy and visitor identity
headers are also removed. The app still receives ordinary browser headers, its
own cookies, and its query string. WebSocket upgrades are supported.

Each request is authorized independently and normally uses one connection into
the box. A share permits at most 64 concurrent box connections. Malformed,
unauthorized, expired, revoked, overloaded, and unreachable attempts are counted
separately in the receipt.

### Joining safely

The shared page is agent-written code running in your browser. Use a private
window when practical, especially when the joiner must bind `127.0.0.1`, because
browser cookies are scoped by host rather than port.

h5i normally chooses a private address from `127.0.0.0/8`. macOS may require
`--shared-jar`; WSL browser access may require `--bind 127.0.0.1`. Both choices
share browser storage with other services on that host and therefore require
explicit consent. h5i never binds the join proxy outside loopback.

h5i blocks service-worker registration and cross-site requests carrying the
share credential. It does not otherwise sandbox the page: downloads, granted
permissions, browser storage, and page scripts have the same powers as on any
link you open.

### Grants, revocation, and receipts

Use one labeled grant per person. `share revoke` drops that person's live
connections; `share stop` ends every grant and writes the final receipt.
Additional grants are currently available only for tunnel shares. The maximum
share lifetime is 24 hours and the default is one hour.

The receipt records transport, duration, grants, peers, connection and byte
counts, refusals, route failures, incomplete responses, clock anomalies, and
whether shutdown produced partial totals. Tunnel receipts explicitly state that
the connection was not end-to-end encrypted.
## h5i runner

A *runner* is a second Linux machine you own that h5i reaches over SSH: a spare
laptop, a lab box, a VM, a small server. Boxes run there; the repository, the
policy, the credentials and the patch gate stay here.

This is *placement*, a second axis beside the isolation tier a box already
declares. It does not change what a box is allowed to do. What it changes is
which machine an escape would reach.

```bash
h5i runner pair pi5 h5i@pi.local      # pair, pinning the machine's host key
h5i runner probe pi5                  # what can it actually do, right now
h5i runner list                       # what this account has paired
h5i runner unpair pi5                 # forget it here
```

### What pairing does

1. Reads and pins the machine's SSH *host key*. That key is the runner's
   identity: `runner_id` is its SHA-256, and a box records the id, never the
   name, so renaming a runner cannot move a box onto other hardware.
2. Generates a keypair for this runner alone, owner-only, under
   `~/.config/h5i/runners/<name>/`.
3. Installs one line in the runner's `authorized_keys`:

   ```
   restrict,command="/usr/local/bin/h5i runner serve-stdio" ssh-ed25519 AAAA…
   ```

   `restrict` is the security argument in one word: that key cannot open a
   shell, forward a port, forward your agent, or allocate a terminal.
4. Connects over the new key and probes, so pairing either works end to end or
   leaves nothing behind.

Nothing listens on the runner: no daemon, no port, no token, no TLS. The worker
is a process per request, started by sshd and gone when the request ends.

Pairing trusts the host key it sees first, like your first `ssh` to a new host.
To close that window, read the real fingerprint on the machine and pass it:

```bash
ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub   # on the runner
h5i runner pair pi5 h5i@pi.local --fingerprint SHA256:…
```

`--print-only` prints the `authorized_keys` line instead of installing it.

### What a runner advertises

A runner needs Linux, sshd and `h5i`, not a container runtime. Everything past
those three is *advertised* by `h5i runner probe`:

```
$ h5i runner probe pi5
✔ `pi5` — h5i 0.3.4 on linux aarch64, protocol 1

  isolation     process, supervised
  container     no
  memory        7.6 GiB
  workspace     41.2 GiB free
  boxes persist yes
  own egress    yes
  kvm           no
  runner id     3f9a1c04b7e2
```

A box asking for something a runner does not advertise is refused with the
missing capability named, never quietly given something weaker. The isolation
list is what that kernel demonstrably ran a moment ago, not which features are
present.

Two entries change what you can do next:

- boxes persist: no. Box state does not survive a reboot (read-only OS, tmpfs
  workspace). Anything not exported is gone.
- own egress: no. The runner has no default route, so a box on it cannot pull
  images or install packages.

### Putting a box on one

```bash
h5i box create fix-auth --runner pi5
```

The base commit, the branch and the resolved policy digest are all made here.
What crosses is the source as a git bundle; what comes back is the digest the
runner actually enforced, and the box is refused if the two differ.

```
$ h5i box ls
env/human/fix-auth   created   isolation=container  base=fa31b1f97547 captures=0 on=pi5
```

The manifest records the runner's host-key hash, not its name, so renaming a
runner cannot move a box onto other hardware. `h5i box rm` checks that identity
before removing anything there, and clears this side first: an unreachable
runner leaves the box for its lease to reap.

### Working in one

```bash
h5i box run fix-auth -- cargo test      # runs on the runner
h5i box propose fix-auth                # bring the work home
h5i box diff fix-auth                   # review it here
h5i box apply fix-auth                  # land it
h5i box export fix-auth                 # or take the patch and receipts
```

`box run` executes under the policy pinned at create, and the receipt comes home
with the exit code, timings and the runner's egress summary. Its lane is
*`runner-observed`*: h5i saw it from outside the box, so the box could not have
forged it, but this machine did not watch it either. It counts as neither
host-observed nor box-claimed.

`box propose` is the careful part. The runner commits what the box has and sends
a bundle of only the new work. h5i unpacks it into a throwaway repository with
its own object database and inspects it there: size and count ceilings, path
traversal, nested git repositories, submodule pointers the base did not have.
Only a passing tree crosses into your repository, and h5i writes the commit
itself, so the runner's history and authorship never enter yours.

If something is refused, nothing lands:

```
$ h5i box propose fix-auth
Error: mediated commit refused (fail-closed) — 1 path violation(s):
  - a submodule pointer the base did not have, at vendor/thing
```

After a successful propose, `diff`, `apply` and `export` behave exactly as for a
local box.

### Not on a runner yet

`box shell` (needs a pty), streamed output (`box run` returns when the command
finishes), agents (h5i will not send model credentials to another machine), and
`clone:` / `--new` sources. See `docs/design/design-runner.md` R1 to R13.

### Unpairing

`h5i runner unpair <name>` removes the record, the key and the pin from this
machine. It does not touch the runner: the `authorized_keys` line stays until
you delete it, and the command says so, with the comment to search for.

---

## The console

```bash
h5i ui
h5i ui --port 0
h5i ui --open
```

The read-only web console is one screen over everything h5i is doing on this
machine. Four sections: an overview of what wants a person, the browser
sessions, the boxes, and what this host can enforce. Every route it calls is a
GET, and every next step it suggests is a command it copies to the clipboard.

**Sessions** are where the workbench and recon do their work. Boxes are the
repository's; sessions are the machine's, because `h5i browser open` needs no
repository. The column lists them loudest first and searches the whole registry
by name, id or target. Selecting one opens six tabs:

| tab | shows |
|---|---|
| History | every fetch: the agent verb that spent it, method, host, path, status, size, time, initiator. A filter bar (`host:api status:4xx -path:/static verb:click refused`), sortable columns, and an inspector that draws why the fetch exists and the command that reads its bytes |
| Sitemap | what the session reached as a tree of origins and paths, counts folded upward, refusals kept apart |
| Actions | `h5i browser audit` as a timeline: each verb, whether it succeeded, and the receipts it spent |
| Findings | what the agent concluded with `h5i websec finding`, each with the message ids it rests on |
| Recon | the endpoint ledger by state, and the runs that spent requests |
| About | the record: placement, confinement, engine, identity, policy digest, capture |

The console never renders a stored message. Headers, cookies and bodies stay
on disk, owner-only, and every row prints the `h5i websec show` that reads it.

**Boxes** show each box's tier, status and one signal, and for a selected box
its findings, a flight recorder of one row per receipt across six lanes
(files, egress, exit, limits, page, kernel), the policy that was actually
enforced, and the diff against the pinned base. A browser box has a second
tab with the live in-box browser terminal.

### Reclaiming space

A session's record, logs and ledger are small; its capture store is not. Two
verbs, named the way boxes name the same two acts:

```bash
h5i browser rm <session>...          # erase sessions entirely (--force for a live one)
h5i browser rm --ended               # every session that has ended, no names needed
h5i browser rm --older-than 30 --dry-run
h5i browser gc                       # reclaim stored messages older than a week
h5i browser gc --older-than 0 --dry-run
```

`rm` takes the whole directory: record, logs, jar and store. `gc` takes only
the stored messages of *ended* sessions, and leaves a `capture-reclaimed.json`
where they were, so "the bytes were kept and later reclaimed" stays a different
fact from "the bytes were never kept". What each session did and what recon
found remain readable afterwards.

`close` ends a session and keeps its record, so a console listing hundreds of
sessions after `close --all` is the registry, not a close that failed: one
record per `open`, and a benchmark adds one per target. `rm` is the verb that
shortens that list. Without names it selects by shape instead: `--ended` for
every session that has ended, `--all` for every record (a live one still needs
`--force`), `--older-than DAYS` for the ones that ended before a cutoff, which
on its own means `--ended`. `--dry-run` says what would go and removes nothing.

### Attention

Every session gets one of five states, and each carries the evidence that
produced it rather than a score:

| State | What it means |
|---|---|
| `waiting on you` | A human holds the control lock, or a run stopped for a reason a person has to answer. |
| `finished, unread` | The session ended, or its last run finished, and this browser has not looked. |
| `working` | Live, and something happened in the last minute. |
| `idle` | Live and quiet. |
| `unclassified` | The record says live and the engine's control file is gone. |

The bar at the top counts them and filters the list, so a person running many
sessions is told where to look rather than reading every row. `finished,
unread` clears when you open that session, and the memory of what you have read
is this browser's alone: the console never writes it back, which is what keeps
a passive view passive.

Boxes keep their own words in the same bar (`refused egress`, `with failures`),
because a refusal is the boundary working and folding it into the five states
would be a lie for the sake of a tidier row.

### What it does not show

The console never renders a stored message. Bodies, cookies and `Authorization`
headers live in the capture store, which is owner-only on disk and is the one
artifact h5i keeps that holds credentials in full. Each request row prints the
command that reads it instead: `h5i websec show req_42 --session <name>`.

The console binds loopback and uses a random bearer token in the URL. Keep that
URL private: any local process or page that obtains it can read the console.
Untrusted box and page strings are rendered as text, never HTML. The console can
stop a browser or take its control lock, but it cannot edit the box or widen its
policy.
## h5i skill

`skills/h5i/` is embedded in the binary at build time, so the skill cannot
document flags the installed binary does not have.

```bash
h5i skill install [--target <dir>]   # write it out
h5i skill show [<page>]              # print SKILL.md or one reference page
h5i skill path                       # where an install would write
```

This is also how the *in-box* agent gets the skill: nothing is baked into an
image, and nothing is copied from host to box.

---

## h5i plugin

A plugin is a separate executable h5i runs by name. It is not in the default
build, and installing one is a deliberate act.

```bash
h5i plugin install websec --from ./h5i-websec   # from a release archive or a build
h5i plugin install recon --from ./h5i-recon
h5i plugin list                                 # what is installed, and what exists
h5i plugin remove recon
```

Only names h5i knows can be installed, and they live in h5i's own state
directory rather than on `$PATH`, so `h5i plugin list` is the whole truth about
what `h5i <name>` can become. A plugin holds no privilege of its own: it reaches
a session through the same verbs a person types, so its requests are the
engine's, checked by the engine's policy and written into the engine's receipts.

A build without a plugin still knows the name. `h5i recon` on a plain install
says what the capability is and how to get it rather than "unknown command".

---

## Policy

A box's policy is resolved at creation, serialized to `policy.resolved.toml`,
and *digested*. Every receipt records the digest that was actually in force, so
"what was enforced" is never a matter of trust.

### Profiles

Built-ins need no file:

| Profile | What it grants |
|---|---|
| `default` | Fail-closed build/test confinement: system paths read-only, `$WORK` read-write, no network. |
| `agent` | The agent-in-box surface, scoped to `$H5I_AGENT`'s runtime. |
| `agent-claude` / `agent-codex` | Pin one runtime: only that agent's HOME state and API egress. |
| `browser` | The agent profile plus headless Chrome and the `agent-browser` daemon. |

Runtime scoping is not cosmetic: a Claude box must not get Codex's credentials
or egress to OpenAI, because a prompt-injected agent could otherwise read the
*other* runtime's token and use it against an allowlisted host.

The built-in read set carries the paths `/etc/resolv.conf` is a symlink *to*
(`/mnt/wsl/resolv.conf` on WSL, the systemd-resolved and resolvconf locations
under `/run`). `/etc` alone is not enough: Landlock follows the link to a path
the box was never granted, and the failure does not look like a denied file.
`getaddrinfo` answers "Temporary failure in name resolution" and a
`net.mode = "host"` box reads as a machine with no network. The entries are the
same on every host whether the files exist or not, so one profile does not get a
different digest per machine. A custom profile that sets `fs.read` replaces that
list, so add the line `readlink -f /etc/resolv.conf` names on your host.

Custom profiles live in `.h5i/env.toml`:

```toml
[profile.review]
isolation = "supervised"

[profile.review.fs]
read  = ["/usr", "/etc"]
write = ["$WORK"]

[profile.review.net]
mode     = "deny"
egress   = ["api.github.com"]
unix     = false          # AF_UNIX sockets; see below
loopback = [3000]         # macOS only; see below

[profile.review.resources]
mem   = "4G"
procs = 256
wall  = "30m"
```

### Runtime detection

Optional eBPF detection reports what a run actually did from kernel syscall
tracepoints. It observes; it does not block. Landlock, seccomp, namespaces, and
the egress proxy remain the enforcement mechanisms.

```toml
[profile.review.detect]
enabled = true
require = false
buffer_kb = 256
rules = ["*"]
```

`require = true` refuses a run when observation cannot attach. Detection needs
a build with `--features bpf`, Linux 5.8 or newer, and `CAP_BPF` plus
`CAP_PERFMON`. Use `h5i box detect probe`, `detect rules`, and
`detect show <box>` to inspect availability and results.

Coverage is full for workspace, process, and supervised runs; partial for
containers whose workload leaves h5i's process tree; and unavailable inside a
microVM's guest kernel. Every receipt states its coverage.

### Isolation tiers

| Tier | Boundary | Network |
|---|---|---|
| `workspace` | Separate worktree only | none |
| `process` | Landlock, seccomp, namespaces, and resource limits | deny or host |
| `supervised` | Process tier plus private network and socket supervision | L3/L4 allowlist |
| `container` | Rootless Podman | proxy-based L7 allowlist |
| `microvm` | Hardware-isolated guest via microsandbox | guest L3/L4 allowlist |

`auto` chooses the strongest available tier. An explicitly requested tier
fails rather than silently downgrading.

The microVM tier requires `msb` 0.6 or newer, host virtualization, and a
pre-pulled OCI image. It protects the host kernel boundary but currently provides
no per-request egress tally or authenticated-egress grants. `h5i box probe`
reports missing requirements.
### AF_UNIX sockets

`[profile.X.net] unix = true` lets the box create `AF_UNIX` sockets. Off by
default, because `SCM_RIGHTS` passes file descriptors, which is authority
smuggling.

What the grant does *not* open, which is why it can exist at all: abstract
sockets are scoped by the box's private netns; filesystem-bound ones are scoped
by Landlock; and `/tmp`, where `.X11-unix`, `tmux-*` and an ssh-agent live, is a
per-box scratch at the kernel tiers. What is left is a host socket sitting
inside a granted path, so the grant is opt-in per profile and pinned in the
digest.

The `browser` profile sets it, because the `agent-browser` daemon's control
socket is a filesystem-bound `AF_UNIX` listener.

### Credentials

- *Model API*: the key stays on the host. A reverse proxy injects it into
  outbound requests from the box, scoped per runtime, so a Claude box cannot
  reach the OpenAI credential.
- Any other service: the same mechanism, declared as policy:

        [[profile.review.auth]]
        host           = "api.github.com"
        credential_env = "GITHUB_TOKEN"   # read on the host, never in the box
        base_url_var   = "GH_HOST"        # what the client reads
        token_var      = "GH_TOKEN"       # where the box gets its per-run dummy

    `token_var` is required. The proxy gates every request on a per-run token, so
    the box has to be handed it in whatever variable its client already sends as
    a credential. The real credential stays on the host; the box only ever holds
    the dummy.

    The limit is real, so know it before you declare a grant: it binds clients
    you can point at another origin, so a plain `curl https://api.github.com`
    still goes nowhere. A TLS-terminating forward proxy would lift that, at the
    cost of a CA the box trusts, and it is deliberately not built.

    Restricting *what* the box may do with a credential is authorization, and it
    belongs where it is already solved: a fine-grained token scoped to one
    repository and the operations you meant.

- Per-box HOME state is a copy of the host agent's config, seeded once and never
  written back, with credential-shaped entries stripped at any depth
  (`credentials*`, `.netrc`, ssh keys, `*.pem`/`*.key`/`*.p12`), keeping only
  the runtime's own token, which it cannot function without.

### Secrets

Declared per profile, brokered host-side, injected for the life of one run:

```toml
[profile.review]
secrets = ["DEPLOY_KEY"]

[profile.review.secret.DEPLOY_KEY]
source = "env:H5I_SECRET_DEPLOY_KEY"   # the default for a bare name
inject = "env"                          # `file` is workspace-tier only in v1
```

The value never appears in the policy, the digest, or any receipt. `h5i box
secrets <name>` dry-runs the resolution and reports a fingerprint, never a
value. A grant that cannot be resolved fails the run closed rather than starting
a box that will fail confusingly later.

Two limits on what a source may be, because a profile lives in the repository:

- `source = "command:…"` runs host-side code outside the sandbox, as you. It
  needs the profile's `allow_command_extractors = true` *and*
  `H5I_ALLOW_COMMAND_EXTRACTORS=1` in your environment. The profile flag pins
  the decision in the digest; it cannot also be the authority for it.
- `source = "file:…"` is a host-side read handed to the box, so it may not point
  inside the profile's `fs.deny`. A policy that puts `~/.ssh` out of the box's
  reach cannot read `~/.ssh/id_ed25519` on its behalf.

An `[[auth]]` grant is the one place h5i attaches a credential you hold to a
request it originates. The destination must be a bare hostname, and every run
prints which variable is being attached and where it goes.

`ttl` is advisory and is shown as `ttl=<value>(advisory)`: h5i resolves a grant
once and never expires it.

### Scope: the engagement kind

A profile says what this machine will permit. A **scope** says what the target's
owner authorised. Enforced together, digested apart.

One TOML file per project, at `~/.config/h5i/projects/<name>.toml`:

```toml
allow      = ["*.acme.com", "api-staging.acme.io"]
deny       = ["blog.acme.com"]
deny_paths = ["/admin/*", "/account/*/delete"]
```

`h5i browser open --project acme` resolves it. `allow` joins the origin grant.
`deny` and `deny_paths` are checked before the wire, ahead of the loopback
exemption and the instrument mode, and nothing can grant past them. An unknown
key is an error, not ignored configuration.

```
$ h5i browser navigate https://blog.acme.com/ --session acme1
Error: denied by policy: `blog.acme.com` is refused by the deny rule
`blog.acme.com`.
```

That is an `allowed: false` row in `requests.jsonl` with no bytes behind it. A
`jq` pass over the log afterwards could also tell you a request was out of
scope, but by then it had been sent.

Where the file lives matters three ways. `~/.config/h5i` is in every profile's
`fs.deny`, so a box cannot widen the scope confining it. The engine never reads
it: the host resolves it and passes the rules as arguments, which is the only
arrangement that works for a boxed session. And it works outside a repository,
because an engagement is not a checkout.

The digest goes on the session record as `scope_digest`, beside `policy_digest`.
One of them moving tells a reviewer which document changed. A project with no
scope file is allowed; `--project` is then only a label.

---

## Receipts

One append-only JSONL log per box, plus the raw payload of each record. A record
is generated from observation, never from the agent's account of itself.

Two properties the design depends on:

- Append only, and sealed. The box's write window under its own directory is
  exactly `<box>/spool`. The receipt log and the stored payloads are siblings of
  that spool, outside every grant. The box stages a record; the host ingests it.
  There is no path from inside to a record the host has already written.
- Redacted at the boundary. Secrets are scrubbed from the command and from the
  payload *before* either is written, and the scrub is recorded by rule id,
  never by value.

Every record carries the *lane* that observed it, so the two kinds of evidence
never blur:

| Lane | Who observed it |
|---|---|
| `host-env-run` | h5i, host-side. Exits and resource usage come from the supervisor's `wait4`; egress from the allowlist proxy's own log. |
| `viewer` | h5i's own viewer forward. The box supplies none of it. |
| `tee-shim` | The box's shell shim. Box-claimed. |
| `inbox-capture` | Staged by the box. Box-claimed. |

A record can also carry a `runtime` block, which is a *second observer of the
same command* rather than a lane of its own. See below.

### What the browser saw

A run that drove the browser also carries what the page said back: console
errors, uncaught exceptions, and requests that failed. h5i collects these
itself, right after the command, in the same box under the same policy, so the
timing is not the agent's to choose. Only what is new since the last drain is
recorded.

A browser command with no browser to ask is recorded as `unavailable`, not as a
clean page. "Nothing was looked at" is a different claim from "nothing was
wrong", and a reviewer has to be able to tell them apart.

### What the kernel saw

A run under a profile with `[detect] enabled = true` carries a `runtime` block:
which scope selected the events, how completely it covered the tier, how many
events were seen and how many were lost, and every signature that fired with a
few examples of what tripped it.

The block is written even when the collector could not attach, carrying the
reason. That is deliberate and it is the point of the whole lane: a missing
block and a quiet box would otherwise look identical. Read it this way:

- No `runtime` block at all: the profile did not ask to be watched.
- A block with `unavailable`: it asked, and the probe could not attach. The
  reason is in the block; `h5i box detect probe` explains it in full.
- A block with `coverage: none` or `partial`: some or all of the run happened
  where this scope cannot reach it, with the reason attached.
- A block with detections and `events_lost` above zero: the list is a lower
  bound, because events were dropped before anything examined them.
- A block with no detections, `coverage: full` and `events_lost: 0`: nothing the
  catalogue models happened. That is not the same as nothing happening: `h5i box
  detect rules` is a finite list, and behaviour no rule models produces no line.

Every path and command line in the block is a string the box passed to a
syscall, captured on the way *in*. It is not the kernel's resolution of that
string, and the probe sees the attempt rather than the outcome. A `connect` the
network namespace refused looks exactly like one that succeeded.

### What we do not claim

An agent can stage *extra* records, or stop writing. Both are visible: a gap
between host-observed exits and box-reported commands is itself a finding. We do
not claim more than that.

The kernel lane narrows what "stop writing" buys, and does not close it: the
collector cannot be defeated by a box declining to cooperate, but it only runs
where the profile asked and the host could attach, and it only reports what a
signature models.

---

## Limits

h5i states these limits explicitly because its claims are security-sensitive.

- Containment cannot stop an agent from sending source code to an allowed model
  API. Use a trusted model endpoint or deny model egress when that matters.
- Workspace, process, supervised, and container tiers share the host kernel.
  They are not defenses against a targeted kernel exploit. The microVM tier adds
  a hypervisor boundary.
- Container egress is proxy-based and therefore applies only to software that
  honors the proxy. Supervised and microVM tiers enforce network rules lower in
  the stack.
- Interactive workspace, process, and supervised shells share a terminal with
  the operator. Depending on the OS and kernel, a process may inject input,
  continue reading an inherited terminal, or leave terminal settings changed.
  `h5i box probe` reports input-injection exposure. Container and microVM
  terminals are separate.
- Chrome's own Linux sandbox is disabled inside a box because h5i's seccomp
  policy blocks the namespace operations it needs. The box boundary remains,
  but one browser defense layer is absent.
- On macOS, a box shares the host's loopback. Declared service ports are
  reachable locally, and manually started services must list allowed loopback
  ports. Linux boxes use a private network namespace.
- macOS Seatbelt has no seccomp equivalent, cgroup memory ceiling, or per-box
  process-count ceiling. Status and probe output mark those gaps. Use a container
  or microVM when hard resource ceilings are required.
- Chromium placement on macOS relies on the tier's egress boundary rather than
  agent-browser's in-process domain list. Chrome may restart when its proxy route
  changes, losing its temporary browser profile.
- Browser viewing covers the rendered tab, not browser chrome, native dialogs,
  or the desktop.
- Chrome consumes substantial memory and CPU. Browser support is opt-in per box.
- Chromium driving depends on the pinned external `agent-browser` tool.
## Files

| Path | What it is |
|---|---|
| `.h5i/env.toml` | This checkout's box policy: profiles, services, container image. Local, not shared: it carries machine paths and resource caps, and `.h5i/` is gitignored. |
| `.git/.h5i/env/<agent>/<slug>/` | One box: its manifest, resolved policy, receipts, workspace. |
| `.git/.h5i/cache/<eco>/<key>/` | Warm dependency caches. |
| `.git/.h5i/env/<agent>/<slug>/spool/` | The box's one writable window: staged posts and capture records. |
| `~/.config/h5i/` | Host-side egress allowlist. Outside every box-granted path. |
| `~/.config/h5i/projects/<name>.toml` | One project's engagement scope, resolved by `browser open --project`. Outside every box-granted path, for the reason under [Scope](#scope-the-engagement-kind). |
| `~/.config/h5i/runners/<name>/` | One paired runner: its record, its dedicated key, its pinned host key. Owner-only, and outside every box-granted path for the same reason the allowlist is. |

### A browser session's directory

Sessions live under `$XDG_STATE_HOME/h5i/browser` (`~/.local/state/h5i/browser`
by default, `0700`), one directory per session id, and this layout is a
contract: everything h5i knows about a session is a file here, in a shape `jq`
can read, so the folds h5i does not ship are scripts rather than feature
requests.

| Path | What it is |
|---|---|
| `sessions/<id>/session.json` | The record: what `h5i browser status --json` prints. |
| `sessions/<id>/requests.jsonl` | The request log, one JSON object per line, written before the wire. |
| `sessions/<id>/actions.jsonl` | The verbs the agent asked for. Joined to the log by sequence. |
| `sessions/<id>/helpers.jsonl` | Outside programs h5i ran on the session's behalf, when any did. |
| `sessions/<id>/cookies.json` | The cookie jar. Credential material, `0600`, the one file `--restore` copies. |
| `sessions/<id>/messages/` | The capture store: headers and bodies both directions, `--capture` only. `0700`, and evidence rather than account. |
| `sessions/<id>/findings/findings.jsonl` | What `h5i websec finding` wrote. |
| `sessions/<id>/recon/ledger.jsonl` | The endpoint ledger. |
| `sessions/<id>/recon/jobs/` | One record per recon run that spent requests. |
| `sessions/<id>/artifacts/` | Files the session produced. |
| `sessions/<id>/control`, `control.jsonl` | Where the engine listens, and the handover journal. |
| `default` | The id every verb acts on when nobody says which. |

Two rules for anything reading these. The capture store holds `Authorization`
and session cookies in full, so a script that copies out of `messages/` is
copying credentials. And a session's name can be reused once that session has
ended, so group by `project` or `id`, never by name.

---

## Environment variables

All optional; h5i ships with working defaults.

### Set by you

| Variable | Purpose |
|---|---|
| `H5I_AGENT` | Which runtime a box is scoped to (`claude`, `codex`). Decides the env's branch namespace and the `agent` profile's credentials and egress. The namespace takes 1–64 ASCII letters, digits, hyphens, or underscores after trimming; unset is `human` silently, anything else warns on stderr and namespaces the box under `human`. |
| `H5I_DEFAULT_ISOLATION` | Pin this clone's default tier when `--isolation` is not given. `--isolation auto` re-probes past it. |
| `H5I_SECRET_<NAME>` | Default source for a secret grant `<NAME>`. Injected for one run, redacted from evidence, audited by fingerprint. |
| `H5I_SKILL_DIR` | Where `h5i skill install` writes. |
| `H5I_CREDENTIAL_PROXY` | Turn the credential proxy off (`0`) for a box that must reach the model API directly. |
| `H5I_LOG` | `tracing_subscriber` filter for h5i's own diagnostics, e.g. `h5i_core=debug`. Goes to stderr. `RUST_LOG` is honoured as a fallback. |
| `H5I_NO_PROBE_CACHE` | Re-probe host capabilities instead of reusing the cached answer. |

### Set by h5i, inside a box

Read these to detect that you are in one; do not set them yourself.

| Variable | Meaning |
|---|---|
| `H5I_ENV_ID` | The box's id. Its presence is how the skill decides you are inside. |
| `H5I_ENV_POLICY_DIGEST` | The digest of the policy actually enforced. |
| `H5I_ENV_CAPTURE_SPOOL` | The box's only write window: staged receipt records. |
| `H5I_ENV_BASE_TREE`, `H5I_ENV_AUDIT_CAPTURE` | Box plumbing. |

### Tests

| Variable | Purpose |
|---|---|
| `H5I_TEST_CONTAINER` | Opt in to the real-container integration tests (pulls an image, makes a live call). |
| `H5I_TEST_NET` | Opt in to the supervised egress allowlist end-to-end test (needs outbound network). |
| `H5I_RUNNER_STATE_DIR` | Where a runner worker keeps box state. For driving a worker against a scratch directory; a real runner uses its default. |
| `H5I_BPF_LIVE` | Opt in to the live eBPF attach suite. It loads programs into the running kernel, so it needs `CAP_BPF` and does not run by accident; without it the suite skips and prints why. |

### Builds

| Variable | Purpose |
|---|---|
| `H5I_BPF_REQUIRE` | Fail the build if the eBPF probe cannot be compiled, instead of shipping a binary whose detector reports `unavailable` forever. Set it in CI and for releases. |
| `CLANG` | Which `clang` compiles the eBPF probe. Otherwise `clang`, then `clang-20` down to `clang-14`, each tested against the BPF target before it is trusted. |
| `H5I_SKIP_WEB_BUILD` | Skip the console bundle and leave a stub, for a Rust-only build with no Node on the machine. |

---

## See also

- `h5i <command> --help`: the authoritative flag reference
- `man h5i`: the terse CLI reference
- [`skills/h5i/`](../skills/h5i/): the agent-facing skill (`h5i skill show`)
- [`docs/ROADMAP.md`](ROADMAP.md): what is built and what is not
- [`docs/design/`](design/): the design behind each part
  (`design-browser.md`, `design-policy.md`, `design-runner.md`,
  `design-detect.md`)
- [`SECURITY.md`](../SECURITY.md): reporting a vulnerability
