# h5i Manual

Command reference for h5i, ordered the way an engagement runs. New here? Read
[What h5i is](#what-h5i-is) and [The engagement](#the-engagement) first: they
give the mental model before the per-command reference.

`h5i <command> --help` is always authoritative for flags. This manual explains
what the commands are *for*.

---

## What h5i is

> Build. Verify. Hack.

*h5i* (pronounced *high-five*) unifies web development, formal verification
with Lean 4, and red-teaming around a composable Rust framework.
Build an application with h5i-app, keep its critical logic in a Rust kernel,
and use [`h5i app`](#h5i-app) to extract and prove properties of that logic in
Lean 4. Then test the running app for bugs the proofs did not cover. The
red-teaming tools also work with applications built on any stack. Most of this
manual covers those tools because they have the most commands.

On the red-teaming side, a session is Chromium, driven through
[agent-browser](https://github.com/vercel-labs/agent-browser) and pointed at a
capture proxy h5i runs for that session. Every request is policy-checked and
written down before the bytes move, and the request is refused when the record
cannot be written. An agent drives a session by name, reads the page as an
accessibility outline with `@ref` handles, and works on the traffic that page
produced: inspect it, change one field, send it again, compare what came back.
The page and its traffic are one session, because every byte the browser sends
goes through the proxy that keeps the session's record.

No other tool has both ends. Playwright and Puppeteer drive a browser and cannot
say what it reached. Burp owns the traffic and not the browser, so a person has
to wire the two together and keep them in step. h5i starts the proxy, launches
the browser pointed at it, and writes the decision before the bytes move, so a
request that is not in the log did not go through the session.

One Rust binary for h5i's half. The browser half is agent-browser (0.25 or
newer) and the Chromium it installs (`npm install -g agent-browser &&
agent-browser install`); `h5i browser open` refuses, and leaves nothing behind,
when it is missing or too old.

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
  included. Owner-only, kept for every session, in no export unless named.

The two are never merged. Every row also says which lane saw it. A session's
lane is `proxy-observed`: h5i's loopback proxy stored the HTTP exchange the
session's Chromium sent through it. Records written by an older h5i, which had
its own engine, can also say `engine-claimed` (the engine's account of itself)
or `host-observed` (something outside the engine seeing it too); they are still
read, and a lane is never upgraded.

### What it is not

- **Not a scanner.** No crawl-and-flag mode, no payload generation, no severity
  score.
- **Not a sandbox by default.** A session's Chromium runs as an ordinary host
  process, and `h5i browser status` says so. Containment is a box you ask for.
- **Not a browser.** The browser is Chromium. h5i owns what goes through its
  proxy: the policy, the record, and the messages.
- **Not a content filter.** h5i does not classify what a page says. It bounds
  what a persuaded agent can reach.
- **Not a defence against a targeted kernel exploit.** See [Limits](#limits).

---

## The engagement

Sections are named after the commands; the order is the workflow. The join
between phases is the message id.

| Phase | What you are doing | Section |
|---|---|---|
| Scope | Say what is in bounds, and make the proxy enforce it | [Scope and identity](#scope-and-identity) |
| Recon | Find what the target exposes, and record how you know | [`h5i recon`](#h5i-recon) |
| Drive | Open a page, act on it, capture what it fetched | [`h5i browser`](#h5i-browser) |
| Test | Change one thing, send it again, compare the answers | [`h5i websec`](#h5i-websec) |
| Evidence | Write the claim down against the ids that support it | [Findings](#findings) |
| Regression | Commit the flow so the bug cannot come back | [`h5i test`](#h5i-test) |

```bash
h5i browser open https://target.example --project acme
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
for a box when the code under test is untrusted or agent-written, or when you
want the browser itself confined and its network decided outside it: a box's
`browser` profile runs Chromium under the box's tier and egress. See
[Containment](#containment-optional).

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
| [`h5i test`](#h5i-test) | Replay portable attack flows and check them with an `expect` verdict or your own oracle. A plugin. |
| [`h5i project`](#h5i-project) | The durable engagement: notes, findings, evidence, checklists and reports. |
| [`h5i box`](#boxes) | Create, run, inspect and export boxes. Optional containment. |
| [`h5i ui`](#the-console) | The console: every session and box on this machine, read-only. |
| [`h5i skill`](#h5i-skill) | Write or print the agent skill this binary carries. |
| [`h5i app`](#h5i-app) | Build verified applications with h5i-app: extract a kernel to Lean, gate its proofs, mutate it. |
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
| Identity | Who the session says it is | the account signed in to its browser |

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

The name is also the durable project [`h5i project`](#h5i-project) keeps: the
findings, evidence and reports that outlive any one session under it.

### What a refusal looks like

Out of scope is a recorded decision, not a tooling error:

```
$ h5i browser navigate https://blog.acme.com/ --session acme1
$ h5i browser requests --denied-only --session acme1
—          DENIED GET https://blog.acme.com/ — `blog.acme.com` is refused by the deny rule `blog.acme.com`
```

The request gets an `allowed: false` row in `requests.jsonl`, a response record
describing the refusal, and no bytes between them. The browser is answered
`403` with the same reason, so the page shows why it did not load. Change scope only with
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
else is a concept an agent has to learn: not the proxy, not the port it listens
on, not the Chromium process, and not, in the ordinary case, the session itself.

```bash
h5i browser open https://example.com
h5i browser snapshot            # the page as a model should read it
h5i browser click @e3
h5i browser screenshot          # a PNG of the page, into the session's artifacts
h5i browser reload
h5i browser requests            # what it asked for, and what was refused
h5i browser close
```

### What a session is

Two processes under one record. h5i's half is a capture proxy
(`h5i __capture-proxy`) with the session's control channel beside it. The
browser half is Chromium, run by agent-browser and pointed at that proxy:

```bash
npm install -g agent-browser && agent-browser install   # once per machine
```

`open` checks that agent-browser runs before it creates anything. A missing one
stops there, prints the install command, and leaves no session behind. The
proxy writes a per-session CA (`0600`) and uses it to decrypt HTTPS. agent-browser
is launched with `--ignore-https-errors`, so Chromium accepts the proxy's
certificates for that session; the CA file is still written for anyone who
installs it instead. `status` repeats that line, and `session.json` records it
as `proxy.chromium_tls`.

Every request the browser sends goes through the proxy, which checks it against
the session's policy and writes it down *before* it is forwarded, and refuses it
when the record cannot be written. Every message is kept, both directions,
headers and bodies, in the session's owner-only store. A request that is not in
`h5i browser requests` did not go through the session.

```
requests : proxy-observed (captured at h5i's proxy; the external browser itself is not confined)
```

The lane is `proxy-observed`, and the line says what it does not cover:
agent-browser and Chromium run as ordinary host processes. For a browser that is
itself confined, use a box (see [Containment](#containment-optional)).

The server's `Set-Cookie` answers also land in the session's own jar. A replay
of a request the browser sent carries that request's own `Cookie`; a request
carried in from another session (`resend --as`) has its credentials dropped,
and the jar is where this session's come from.

Where Chromium's own sandbox cannot start, which is common without root, set
`AGENT_BROWSER_ARGS=--no-sandbox` in the environment h5i runs in. It turns that
sandbox off, so use it only for a target you trust.

### Page verbs and workbench verbs

The page verbs go to the session's Chromium through agent-browser, with the
launch flags the session started with on every call, so a relaunched browser is
still captured. Each command is handed over on stdin, never in argv, so a value
shaped like a flag stays a value and a credential is not in the process table:

| verb | what it does |
|---|---|
| `snapshot [-i] [--delta] [--url U]` | the accessibility outline, with `@ref` handles |
| `navigate <url>` | go there, `http(s)` or `about:blank` only (`proxy-open` is the same verb) |
| `reload` | load the current page again |
| `click <@ref\|css>` | click it |
| `type <target> <text>` | clear the field and fill it |
| `press <key>` | `Enter`, `Tab`, `Control+a` |
| `select <target> <value>` | choose an option by its value |
| `set-checked <target> <true\|false>` | a state, not a toggle |
| `scroll <px>` | negative scrolls up |
| `wait-for --selector S` / `--text T` | until it is on the page |
| `wait-for-script <expr>` | until a page expression is true |
| `eval <js>` | run JavaScript in the page and print the result |
| `console [--clear]` | what the page logged |
| `screenshot [--out P] [--full] [--url U]` | a PNG |

Each is recorded in the session's action log, and its output is kept under the
session's `artifacts/`, so a DOM or console observation is evidence the session
holds rather than something the agent saw once. `screenshot` writes into
`artifacts/` under a name *h5i chooses*; `--out` names a file instead.

The workbench verbs go to the proxy's control channel and act on its network
layer: `requests`, `resend`, `sequence`, `socket`, `grpc` and `rpc --stdio`.
See [`h5i websec`](#h5i-websec) for what they are for. `env` lists, by name,
the credentials `type` can resolve from the environment h5i runs in.

Driving agent-browser yourself works too, and is where capture can silently
stop: its daemon restarts when idle, and a later bare `agent-browser open`
starts a browser that is not proxied. `h5i browser proxy` prints the line and
the `env` to export so every launch from that shell is proxied. `status` shows
when the proxy last let a target request through and warns after five quiet
minutes.

### `proxy`: a session without opening a page

```bash
h5i browser proxy https://app.example --session chrome
# prints: agent-browser --proxy ... --ignore-https-errors --args ... open ...
#   and:  export AGENT_BROWSER_PROXY=... AGENT_BROWSER_IGNORE_HTTPS_ERRORS=1 AGENT_BROWSER_ARGS=...
agent-browser dashboard start          # a human signs in
h5i browser proxy https://api.example --no-browser --session api
```

`proxy` starts the session and prints how to point a browser at it.
`--no-browser` skips the agent-browser check: the proxy and its control channel,
for a session driven only by `resend`, `sequence` and the plugins (`h5i test`
starts its actors this way), or one whose browser you point at it yourself.

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
is what gets written down.

### `open` navigates a session that is already there

Opening a URL in a browser that is already up means *go there*. So `open`
navigates the session it finds, and `--new` is how you say you meant a second
one. The flags that only make sense at creation (`--allow`, `--project`,
`--no-loopback`, `--expires-in`, `--dom-instrument`) are *refused* rather than
ignored when a session is reused: a session's policy is fixed when its proxy
starts, so accepting a grant and doing nothing with it would be a grant the
caller believes it made.

#### The page grants itself

A session reaches the URL it was opened on and nothing else remote. `--allow` is
for origins beyond it: an API the page calls, a CDN it pulls from. Loopback is
reachable by default because it is the dev server; `--no-loopback` takes that
back. What the browser asks for beyond that is refused at the proxy with a
`403`, and the refusal is a row in the log.

#### Logging in

The best login is a human's: `agent-browser dashboard start`, then sign in in
the session's browser. For an agent login, name the credential rather than
typing it:

```bash
export H5I_SECRET_ACME_PASS=…
h5i browser env                                        # names only, never values
h5i browser type '#password' '$H5I_SECRET_ACME_PASS'
```

The placeholder is resolved from the environment h5i runs in on the way to the
browser. The reply, the artifact and the action log carry the placeholder; a
placeholder whose variable is not set is refused rather than typed as written.

### The control lock

Two parties can drive one page: the agent, and a human.

- The agent holds control by default. A session exists to let an agent work; it
  should not have to ask.
- A human takes control with `h5i browser take <session>`. The agent's mutating
  verbs are refused with a typed message rather than fighting for the pointer;
  read-only verbs keep working, because watching never collides.
- Handing control back with `release` invalidates what the agent knew. The page
  moved, so every `@ref` from its last snapshot may point somewhere else. It
  must re-snapshot before its next page action, and acting first is refused
  rather than mis-clicked. Workbench verbs (`resend`, `socket`, `grpc`) carry no
  `@ref` and are not held up by it.

On this machine the lock is advisory: it pauses `h5i browser` and nothing else.

### Sessions end, and endings are recorded

Closing a session stops its proxy and its Chromium and writes the ending into
its record instead of deleting it, so "how did this end" stays answerable and
the id can never be reused.

| state | what happened |
| --- | --- |
| `live` | started, and the proxy answered the last time anyone looked |
| `closed` | ended by `h5i browser close`; the record is complete |
| `died` | the proxy stopped without being asked; the record has a gap and says so |
| `expired` | outlived `--expires-in` |
| `evicted` | the box holding it was removed |

A verb sent to a session that is not live is refused with exit code 69
(`EX_UNAVAILABLE`), never silently restarted:

```console
$ h5i browser requests
browser session `br_7k2xqa` was closed: closed by the user. It will not be
restarted automatically. Start a new one with `h5i browser open <url>`.
$ echo $?
69
```

The distinct code matters: an agent whose retry cannot tell "the session is
gone" from "the click did not work" starts a second browser and loses both the
page and the record of how it lost it.

Records written by an older h5i, whose sessions ran its own engine, are still
read by `status`, `audit` and the plugins. A verb that needs that engine running
refuses on them and says so.

### Everything a session returns is untrusted

The page composed the title, the link text, the error message and the URL;
Chromium and the proxy only carried them. So every answer h5i relays is
scrubbed before it reaches a terminal or a model: escape sequences never
survive, other control characters are removed, and strings, arrays and nesting
are capped with the truncation stated in the value rather than performed
quietly.

Escape sequences matter most. `ESC` in a relayed string is a page rewriting the
terminal it is printed into: moving the cursor over the line above, hiding what
it just did, repainting a prompt. Nothing a browser has to say needs `ESC`.

Files a session produces are named by the host, never by the page, and land
under the session's own `artifacts/` directory.

### Where sessions live

`$H5I_BROWSER_HOME`, else the box's own `/tmp` when h5i is running inside one,
else `$XDG_STATE_HOME/h5i/browser`, else `~/.local/state/h5i/browser`.

Deliberately *not* under a git repository: every other noun in h5i stores its
state under the enclosing repo because every other noun is about a repo, and a
browser is not. `h5i browser open` in an empty directory is the ordinary case.

The default session is per registry, so two agents sharing a `$HOME` share it.
Give each its own with `$H5I_BROWSER_HOME`, or give each session a `--session
<name>`.

| variable | what it names |
| --- | --- |
| `H5I_BROWSER_HOME` | the session registry's directory |
| `H5I_BROWSER_SESSION` | which session a verb acts on, when `--session` is not given |
| `AGENT_BROWSER_ARGS` | extra Chromium launch flags, merged into every launch (`--no-sandbox`, say) |

### `audit`: the whole session, in one timeline

`requests` is the network layer on its own, and it is the verb to reach for in a
loop. `h5i browser audit` is the one to read afterwards: what the agent asked
for, what the proxy decided about every request, who was driving, and how the
session ended, merged and ordered.

```console
$ h5i browser audit
  sources  : actions read · requests read · control read
  note     session rows are ordered by the clock of the process that wrote them

  host    session opened  (http://localhost:3000/ — on this machine, unconfined…)
  proxy   verb   navigate http://localhost:3000/
  proxy   #0 GET http://localhost:3000/
  proxy   #0 200  153 bytes
  proxy   verb   snapshot
  host    control -> human  (taken by a human)
  host    control -> agent  (handed back; the agent must re-snapshot)
  proxy   verb   snapshot
  proxy   #1 DENIED GET https://tracker.example/px  (origin is not in the allowlist)
  proxy   verb   resend
  host    session closed  (closed by the user)
```

Three things this does that neither log does alone:

- **The two lanes stay apart.** Action and request rows are the session's own
  account, written by its proxy and its verbs; handovers and lifecycle are
  h5i's, written from outside. Every row says which.
- **It orders across sources.** "Was a human at the controls when that request
  was sent" is a question about two logs at once.
- **It says what it could not read.** `sources` reports each log as `read`,
  `empty` or *`unavailable`*. An empty timeline over a log h5i cannot see looks
  exactly like a session that did nothing, and those are different findings.

Rows carry `caused_by` where the source recorded the link. Nothing infers a link
from timing: a request that merely happened near a verb is not one that verb
caused.

`--json` gives the whole thing, including the session record.

## h5i websec

The HTTP workbench: read what a session sent, change a part of it, send it
again, and compare the answers. Design: `docs/design/design-websec.md`.

```bash
h5i browser open https://target.example
h5i websec requests                              # captured messages
h5i websec show req_42                           # one message, body bounded
h5i websec show req_42 --raw                     # one message, exactly
h5i websec replay req_42 --set query.id=456      # edit and resend
h5i websec replay req_42 --set query.id=456 --body   # print the response body
h5i websec replay req_42 --set-each query.id=./ids.txt --body   # sweep, each body
h5i websec diff res_42 res_43                    # compare two answers
h5i websec match res_43 --status 200 --contains ok
h5i websec experiment ./plan.json                # many sends, folded to clusters
h5i websec finding create --title … --evidence req_42
h5i websec import-nuclei template.yaml           # print an h5i test; sends nothing
```

Every verb prints text by default: the HTTP message itself under one line of
ids, status, size and time, with the body cut at 4 KiB and control characters
made inert. A replay reads like this:

```text
res_43 ← req_43 · 200 · 31 bytes · 4 ms
sent  GET https://target.example/api/invoice?id=456
set   query.id=456 (was 42)

HTTP/1.1 200
content-type: application/json

{"id":456,"owner":"bob"}
```

`--repeat`, `--race` and `--set-each` print one row per send instead. `--json`
on any verb gives the JSON envelope, for a caller that parses; the exit codes
are the same either way.

`--body` prints the decoded, untruncated response body to stdout (the `curl`
view); `--raw` prints the status line, headers, and body (the `curl -i` view,
for when the answer is in a header). Both also work with `--set-each`/`--repeat`,
printing each send's response. The message store holds bodies and credentials
in full, which is why it is owner-only and never included in an export unless
it is named.

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
only when all three agree. A step with no answer is a request the session did
not make, and the usual reason is the session's allowance of 500 requests h5i
sends itself: add `"reset_budget": true`, or split the plan. A walk cut short
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

`import-nuclei` does not send the template. It prints an [`h5i test`](#h5i-test)
file; [Importing a Nuclei template](#importing-a-nuclei-template) says what that
keeps and what it refuses.

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

A session finding lives beside that session and goes when the session does. To
keep it, promote it into a project (below), which copies both the finding and
the messages it cites, with credentials removed.

---

## h5i project

A browser session is disposable. What you concluded from it should not be. A
project is the durable side of an engagement: notes, findings, the evidence
they rest on, the checklists you were asked to cover, and the reports issued
from them. It lives under `~/.local/share/h5i/projects`, owner-only, and nothing
under `h5i browser rm` touches it.

```bash
h5i project init acme --title "ACME web" --target https://acme.test
h5i browser open https://acme.test --project acme
# ... find something, write a session finding, then keep it:
h5i project finding promote --all -p acme --session <name>
```

Promoting copies each cited request and response into the project as evidence,
with `Authorization`, `Cookie`, secret-looking query parameters and JSON or form
fields replaced by `[removed]`. The copy is the display version; the raw capture
is never copied. `h5i browser rm` refuses to remove a session whose findings are
not saved to a project yet, and names the promote command, so work is not lost
by a cleanup.

A finding keeps severity, the assessor's confidence, and the fix status as three
separate fields: "high but unconfirmed" and "low but confirmed" are different
facts, and so are "fix claimed" and "fix verified".

```bash
h5i project finding create -p acme --title "Users list readable by any account" \
    --severity high --severity-reason "any authenticated user reads all users" \
    --summary "A normal login can read the full user list." \
    --remediation "Add a role check on /admin/users." --evidence E-1
h5i project note add -p acme "admin panel is at /admin, owner confirmed"
```

### Checklists

A checklist is any Markdown list you import: an internal standard, a client's
list, last round's notes. Its items become tracked, and a `--required` list has
to end each item with an outcome (`recorded`, `blocked`, `not-applicable`) and a
reason. Coverage is reported as counts, never a score: "18 of 24 items have an
outcome; 4 open; 2 blocked" says what was looked at, not that the rest is safe.

```bash
h5i project checklist import -p acme checks.md --required
h5i project checklist mark -p acme C1 --status recorded --note "role check present" --link F-2
h5i project checklist coverage -p acme
```

### The report

The report is a free Markdown document you write. h5i fills the counts, ids and
evidence through directives it resolves against the project, so the prose cannot
drift from the data:

| directive | renders |
|---|---|
| `{{finding F-3}}` | the finding in full: severity, status, impact, remediation, evidence |
| `{{findings}}` | a table of every finding |
| `{{evidence E-2}}` | the redacted request and response |
| `{{coverage}}` | coverage counts for every checklist |
| `{{term IDOR}}` | mark a term for the glossary and a definition panel |
| `{{glossary}}` | define every term the report used |

```bash
h5i project report new -p acme          # a starting template, not a form
h5i project report set -p acme report.md   # or write the draft yourself
h5i project report check -p acme        # dangling ids and missing outcomes are errors
h5i project report issue -p acme        # freeze it as a versioned snapshot
h5i project report export -p acme --version 1 --format pdf
```

`check` is deterministic: an unknown directive, an id that names nothing, and a
required item with no outcome are errors; a finding with no evidence or no
remediation is advice. `issue` freezes the draft with a snapshot of everything
it referenced, so a later change to a finding never rewrites a report already
handed over. PDF export uses a local Chromium when one is found (`$H5I_CHROME`
to name it); otherwise export HTML and print from a browser.

The same report renders in `h5i ui` under the project's Report tab, where a
marked term is a button that opens its plain-language definition, with a link to
OWASP, MDN or Wikipedia to read more.

Every term has a plain-language definition; `h5i project glossary <term>` prints
one on the command line, and a project's own `glossary.toml` adds or overrides
terms.

---

## h5i test

A finding that is fixed and never tested again comes back. `h5i test` replays a
flow you wrote and checks that a property you stated still holds.

```bash
h5i plugin install test
h5i test --target https://staging.example --openapi openapi.json
```

It does not decide what a secure response means. A test needs a verdict, and
there are two. A step may carry `expect`, data over that step's response: one
of `status`, `body`, `header`, `headers`, `response` or `dsl`, combined with
`all`, `any` and `not`. A text value is a literal substring, or a regular
expression written `regex:`. A `header` clause may add `contains` or `regex`.
When the file names no oracle, exit 0 means every step `expect` held, 1 means
one did not, and anything else means the test could not decide. That third
answer is never silently a pass.

`expect` reads the responses in the flow. It does not call out, and a clause it
cannot express does not belong in it. Name an oracle for that property: `jq`,
`grep`, `diff`, or the application's own test client. If the file names an
oracle, the oracle decides, and a step `expect` does not replace it. A file
with neither is refused, because nothing would decide.

```yaml
flow:
  - send: probe
    expect:
      all:
        - status: 200
        - body: "regex:role.*admin"
```

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
after the verdict even when it fails. A passing verdict with a failed cleanup
is an error, not a pass.

### Importing a Nuclei template

`h5i websec import-nuclei template.yaml` reads one Nuclei template and prints
an `h5i.test/v1` document. It sends nothing. Matchers become `expect`, and the
command never writes an oracle. Save the document under `.h5i-tests/tests` and
run it with `h5i test`, on the target and under the policy you would use for a
test you wrote.

A template that cannot be said as data is refused, and the refusal is the
result. A `code`, `javascript`, `headless` or `flow` block runs code, so the
command will not keep its HTTP requests and drop the rest. An `unsafe` raw
request, an out-of-band matcher such as `interactsh`, a payload list stored in
a file, and a path that is not relative to `{{BaseURL}}` are refused the same
way: the printed file would not mean what the template means. An inline payload
list becomes a sweep on that step, which passes if `expect` held for one
combination.

### What the oracle is given

The oracle runs with its working directory set to the test file's directory.
This is the path taken when the file names an oracle.

| Variable | What it holds |
|---|---|
| `H5I_TEST_RESULT` | The structured JSON evidence bundle. |
| `H5I_TEST_ARTIFACTS` | Its owner-only artifact directory. |
| `H5I_TEST_INPUTS` | The saved response names the test declared, comma separated. |

### Coverage, and what it counts

A send may declare the OpenAPI operation and mutation class it exercises. It
counts only when the flow completed and the verdict decided: every `expect`
held or one did not, or the oracle exited 0 or 1. Setup and cleanup sends
without `covers` never count. An error is not a covered operation.

With `--openapi`, h5i reports that coverage, report-only unless `--min-coverage`
is given. Without an OpenAPI denominator it reports no percentage and refuses a
minimum rather than inventing one.

Every run writes `result.json`, `junit.xml` and per-response artifacts, into an
owner-only directory because response bodies are evidence. Credential request
headers are redacted from exported request JSON; response bodies stay exact and
are never uploaded.

---

## Containment (optional)

Nothing above this line needs a box. Requiring one up front would fail
hello-world on CI, under AppArmor, on macOS and in a container, for nothing the
record does not already give.

A session's Chromium runs on this machine as an ordinary process, and its
status line says so:

```
placed : on this machine, unconfined — agent-browser and Chromium run as ordinary host processes
```

The proxy decides what the browser may reach and writes every decision down,
but the browser itself is not confined. Reach for a box when:

- The code under test is untrusted, or an agent wrote it.
- A target's response might be, and a parser bug should land somewhere
  disposable.
- You want the browser confined and its network decided outside it. A box's
  `browser` profile runs Chromium through agent-browser under the box's tier and
  egress allowlist (see [Choosing the engine](#choosing-the-engine)).

The box is a separate surface, not the browser's implementation detail. An agent
inside one opens a session the ordinary way, and it runs beside the agent, in
the same box.

### Boxes

A box is a disposable environment holding a checkout, a toolchain, a dev server
and, with the `browser` profile, a Chromium. Nothing of your machine is inside
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
                      [--engine <chromium|lightpanda>]
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
| `--engine <e>` | Browser engine for the `browser` profile: `chromium` (default) or `lightpanda`. Pinned in the digest; never falls back. See [Choosing the engine](#choosing-the-engine). |

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
| `report.md` | What ran, what the browser saw, who was at the controls, and the agent's own proposal. |
| `receipt.json` | Every observed execution, with the policy digest that was enforced. |

It refuses rather than overwrites a non-empty directory (`--force` to replace).
Secret redaction and size caps apply throughout.

Read `report.md` before applying. In order: denied egress attempts, every
command with its lane and exit code, what the browser saw (console errors,
uncaught exceptions, failed requests, observed by h5i rather than reported by
the agent), viewer sessions
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
| `i` | Hand the keyboard to the page |
| `Esc` | Return it |
| `D` | The console pane: what the page logged and what it threw |
| `?` | The key list |
| `q` | Leave |

The labelling keys (`f`, `F`, `yf`, `gi`) and `H` `L` are bound only when the
stream advertises hints and history; a Chromium screencast advertises neither,
so they stay unbound rather than doing nothing.

A viewer attaches read-only. It does not weaken the box policy, publish the
browser port, or become part of the agent's session.

### Choosing the engine

`--engine` selects the browser engine for a box's `browser` profile and records
that choice in the policy digest. Both are driven by agent-browser.

| engine | use |
|---|---|
| `chromium` | Default; broadest web compatibility. |
| `lightpanda` | Third-party lightweight headless engine; still runs script. |

Chromium runs with an isolated profile and the box's policy. Its requests are
seen at the box's boundary and by h5i's mediator on the agent-browser socket,
not by a session's capture proxy: inspect the box-level network evidence. An
older profile that pins `--engine h5i` is refused with that name, because h5i's
own engine has been removed.

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
colour alone. Its sources are the mediator's record of the agent's browser
actions and the evidence in each run's receipt; its header names which it is
reading rather than leaving an empty screen.

`--deny-only` keeps only what was refused. `--json` emits the same event
envelope the console reads, one object per line.

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
its findings, a flight recorder of one row per receipt across five lanes
(files, egress, exit, limits, page), the policy that was actually
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
| `unclassified` | The record says live and the session's control file is gone. |

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
## h5i app

[h5i-app](https://github.com/h5i-dev/h5i/tree/main/crates/h5i-app) applications
keep their logic in a kernel crate that Charon and Aeneas translate to Lean, so
its properties are proven rather than tested. `h5i app` runs that loop. A
project is the kernel crate, a Lake project of proofs (`proofs/`) and an
`h5i-app.toml` that says what to extract, which theorems must exist, and which
bugs the proofs must reject.

```bash
h5i app new counter            # a kernel, proofs that already go through, h5i-app.toml
cd counter
h5i app doctor                 # Charon, Aeneas and Lean at the versions this h5i pins
h5i app prove                  # extract to proofs/generated/, lake build, then the gate
h5i app lint                   # routes take an Actor; no client-set identity in Command
h5i app mutate                 # each [[mutant]] must break a proof
h5i app mutate --auto --list   # mutants generated from the kernel's syntax
```

For CI, `h5i app extract --check` changes nothing and fails if
`proofs/generated/` is not what the kernel extracts to, and `--all` runs a verb
on every project under a directory.

`check` (and `prove`) fail on `sorry` or `native_decide` in hand-written Lean,
on any `axiom` declaration, and on any theorem of a hand-written module that
depends on an axiom other than `propext`, `Classical.choice` and `Quot.sound`.

`mutate` runs each mutant in a copy of the project: it edits the kernel,
re-extracts it and rebuilds the proofs. A mutant is caught when the proofs stop
building. One that survives is a behaviour the spec does not pin down, such as
a theorem that holds because the kernel refuses everything. `--auto` adds
mutants generated from the kernel's syntax: comparison boundaries, `==` and
`!=`, `&&` and `||`, dropped `!`, and `if` conditions forced to `true` or
`false`.

`[authz] guarded` opts a kernel into framework-proven authorization
([`docs/app/AUTHZ.md`](app/AUTHZ.md)). Each listed function must be defined as
`h5i_app_std::authz::guard` or `guard_viewed` of the app's `Transition`,
`Policy`, `Disclose` (and `View`) impls. `extract` then writes
`proofs/generated/Authz.lean`, which proves `<kernel>.<fn>.authorized` by
instantiating the framework's theorem, with no hand-written Lean,
`<fn>.kernel_apply_eq` (the policy is checked against the kernel's own
`apply`) and `Committed.pg_stored` (the kernel's `sql_writes` and `decode`
store and load what that `apply` computes). If `sql_write` is not a plain
list of `schema!` table writes, `extract` fails unless
`[authz] storage = "<theorem>"` names a hand-written storage proof. `check`
builds `Authz` itself and gates these (and `<fn>.read_only_sound` when the
server overrides `Kernel::read_only`). `lint` parses the server with `syn`
and fails unless every `impl Kernel` outside tests runs a listed function as
`transition` and the kernel's `apply` as `apply`, and every `impl Store`
writes and loads with the generated `schema_store`/`schema_load` and keeps
the default `load_for`. An app without `[authz]` is unaffected.

`h5i app new` requires the h5i-app Lean library from git at this binary's
release tag; `--h5i-path <checkout>` uses a local h5i checkout instead.

```toml
# h5i-app.toml
[extract]
crate = "kernel"
start-from = ["transition"]   # Charon start points, inside the crate
include = ["h5i_app_sql"]     # crates extracted with the kernel
schema = true                 # also start from what schema! generates, and write its lean "..." file
std-specs = true              # copy h5i-app-std's specs next to the extraction

[authz]
guarded = ["guarded_transition"]   # kernel fns defined as h5i_app_std::authz::guard(_viewed)
# storage = "my_kernel.Storage.db_inv"   # only if the storage proof cannot be generated

[check]
theorems = ["my_kernel.inv_preserved"]   # must exist and pass the gate

[mutate]
targets = ["Theorems"]        # Lake targets a mutant must break (default: all)

[[mutant]]
name = "off_by_one"
old = "if s.count >= s.limit {"
new = "if s.count > s.limit {"
```

---

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
session's, checked by its policy and written into its receipts.

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
$ h5i browser requests --denied-only --session acme1
—          DENIED GET https://blog.acme.com/ — `blog.acme.com` is refused by the deny rule `blog.acme.com`
```

That is an `allowed: false` row in `requests.jsonl` with no bytes behind it. A
`jq` pass over the log afterwards could also tell you a request was out of
scope, but by then it had been sent.

Where the file lives matters three ways. `~/.config/h5i` is in every profile's
`fs.deny`, so a box cannot widen the scope confining it. The capture proxy never
reads it: `h5i browser` resolves it and passes the rules as arguments. And it works outside a repository,
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

### What the browser saw

A run that drove the browser also carries what the page said back: console
errors, uncaught exceptions, and requests that failed. h5i collects these
itself, right after the command, in the same box under the same policy, so the
timing is not the agent's to choose. Only what is new since the last drain is
recorded.

A browser command with no browser to ask is recorded as `unavailable`, not as a
clean page. "Nothing was looked at" is a different claim from "nothing was
wrong", and a reviewer has to be able to tell them apart.

### What we do not claim

An agent can stage *extra* records, or stop writing. Both are visible: a gap
between host-observed exits and box-reported commands is itself a finding. We do
not claim more than that.

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
- A browser session records what its Chromium sends through the session's
  loopback proxy. agent-browser and Chromium run as ordinary host processes,
  and they are launched with `--ignore-https-errors`, so Chromium accepts every
  certificate error for that session. A missing `agent-browser` refuses before
  a session exists.
- A request the browser sends without the proxy (a Chromium relaunched by hand
  without the proxy settings) is not in the session's record. `status` warns
  after five minutes with nothing captured.

## Files

| Path | What it is |
|---|---|
| `.h5i/env.toml` | This checkout's box policy: profiles, services, container image. Local, not shared: it carries machine paths and resource caps, and `.h5i/` is gitignored. |
| `.git/.h5i/env/<agent>/<slug>/` | One box: its manifest, resolved policy, receipts, workspace. |
| `.git/.h5i/cache/<eco>/<key>/` | Warm dependency caches. |
| `.git/.h5i/env/<agent>/<slug>/spool/` | The box's one writable window: staged posts and capture records. |
| `~/.config/h5i/` | Host-side egress allowlist. Outside every box-granted path. |
| `~/.config/h5i/projects/<name>.toml` | One project's engagement scope, resolved by `browser open --project`. Outside every box-granted path, for the reason under [Scope](#scope-the-engagement-kind). |

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
| `sessions/<id>/actions.jsonl` | The verbs the agent asked for, page verbs and workbench verbs both. |
| `sessions/<id>/messages/` | The capture store: headers and bodies both directions. `0700`, and evidence rather than account. |
| `sessions/<id>/findings/findings.jsonl` | What `h5i websec finding` wrote. |
| `sessions/<id>/recon/ledger.jsonl` | The endpoint ledger. |
| `sessions/<id>/recon/jobs/` | One record per recon run that spent requests. |
| `sessions/<id>/artifacts/` | Files the session produced: screenshots, and the output of every page verb. |
| `sessions/<id>/dom-report.jsonl` | What the DOM instrument reported, for a session opened with `--dom-instrument`. |
| `sessions/<id>/control`, `control.jsonl` | Where the session's control channel listens, and the handover journal. |
| `sessions/<id>/proxy` | The session's loopback proxy URL. |
| `sessions/<id>/proxy-ca.pem`, `proxy-ca-key.pem` | That session's interception CA and its private key, both `0600`. |
| `sessions/<id>/proxy.log` | The proxy process's stdout and stderr. |
| `default` | The id every verb acts on when nobody says which. |

Two rules for anything reading these. The capture store holds `Authorization`
and session cookies in full, so a script that copies out of `messages/` is
copying credentials. `proxy-ca-key.pem` is the same kind of material: it signs
the certificates the capture proxy presents. A session's name can be reused
once that session has ended, so group by `project` or `id`, never by name.

---

## Environment variables

All optional; h5i ships with working defaults.

### Set by you

| Variable | Purpose |
|---|---|
| `H5I_AGENT` | Which runtime a box is scoped to (`claude`, `codex`). Decides the env's branch namespace and the `agent` profile's credentials and egress. The namespace takes 1–64 ASCII letters, digits, hyphens, or underscores after trimming; unset is `human` silently, anything else warns on stderr and namespaces the box under `human`. |
| `H5I_DEFAULT_ISOLATION` | Pin this clone's default tier when `--isolation` is not given. `--isolation auto` re-probes past it. |
| `H5I_SECRET_<NAME>` | Default source for a secret grant `<NAME>`. Injected for one run, redacted from evidence, audited by fingerprint. For a browser session, the value `h5i browser type` puts in place of `$H5I_SECRET_<NAME>`. |
| `H5I_BROWSER_HOME`, `H5I_BROWSER_SESSION` | Where browser sessions live, and which one a verb acts on. See [Where sessions live](#where-sessions-live). |
| `AGENT_BROWSER_ARGS` | Extra launch flags for a browser session's Chromium, such as `--no-sandbox` where its sandbox cannot start. |
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

### Builds

| Variable | Purpose |
|---|---|
| `H5I_SKIP_WEB_BUILD` | Skip the console bundle and leave a stub, for a Rust-only build with no Node on the machine. |

---

## See also

- `h5i <command> --help`: the authoritative flag reference
- `man h5i`: the terse CLI reference
- [`skills/h5i/`](../skills/h5i/): the agent-facing skill (`h5i skill show`)
- [`docs/ROADMAP.md`](ROADMAP.md): what is built and what is not
- [`docs/design/`](design/): the design behind each part
  (`design-browser.md`, `design-policy.md`)
- [`SECURITY.md`](../SECURITY.md): reporting a vulnerability
