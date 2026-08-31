# h5i Manual

Command reference for h5i. New here? Read [What h5i is](#what-h5i-is) and [The
loop](#the-loop) first: they give the mental model before the per-command
reference.

`h5i <command> --help` is always authoritative for flags. This manual explains
what the commands are *for*.

---

## What h5i is

> Give an AI agent a browser it can drive and you can audit. Every request is
> policy-checked and written down before the bytes move, and the fetch is
> refused when the record cannot be written.

*h5i* (pronounced *high-five*) is a secure, auditable browser for AI agents. An
agent drives a session by name and reads an outline with `@ref` handles; you
read the log the engine wrote as it went. Because the engine *is* the HTTP
client, that log is a decision record rather than an observation made beside the
network: a request that is not in it did not happen.

Auditable by default. Containable on demand. A session runs on your machine like
any other headless browser, and h5i says so rather than letting the word
"browser" imply a boundary. One flag places the same session inside a sandbox,
which changes nothing an agent types and changes who saw the network.

It is one Rust binary, engine included. No server, no daemon, no SaaS.

### Four parts

Everything h5i does maps to one of these:

1. A browser session. One page state, one cookie jar, one request log, one
   policy, under one name. Every request is checked and recorded before the
   wire; a fetch that cannot be recorded is refused. Page text comes back fenced
   as data, stripped of anything that could repaint a terminal.
2. An audit. The verbs the agent asked for, the decision the engine made about
   every fetch, the moments a human took the controls, and how the session
   ended, in one ordered timeline. Every row says whether it is the engine
   describing itself or something h5i saw from outside, and the two are never
   merged.
3. A box, when you want one. A disposable environment holding the code, the
   agent, the toolchain, the dev server and, on request, the browser session
   itself. Nothing of your machine inside it; egress is an allowlist enforced at
   its boundary.
4. An output gate. At the end you export a patch, a report, an execution receipt
   and every session's timeline, after inspection. The agent has no direct write
   path to the host.

The value is not any one of these. It is that what the agent read, what it
reached, who was driving and what came back all sit in one record that both the
agent and the human can operate on.

### What it is not

- Not a sandbox by default. A session with no `--in` is not contained, and `h5i
  browser status` says so on every line. Containment is a placement, and h5i
  never claims one it does not have.
- Not a complete browser. Of twenty single-page applications measured, eighteen
  read usefully and one not at all. Canvas, WebSockets, Workers and IndexedDB
  are absent, and a page needing an API the engine lacks gets that API *named*
  in the snapshot rather than a blank space.
- Not a content filter. h5i does not classify what a page says. It bounds what a
  persuaded agent can reach.
- Not a provenance system. h5i used to record who wrote what, with git notes,
  blame overlays and a multi-agent orchestra. That is gone. What survives is
  containment and the receipt of what actually ran.
- Not a defence against a targeted kernel exploit. See [Limits](#limits).

---

## The loop

Reading and acting on a page, which needs nothing else:

```bash
h5i browser open https://example.com       # the page grants itself; `--allow` adds more
h5i browser snapshot                       # the outline, with @ref handles
h5i browser click @e3
h5i browser requests                       # what it reached, and what was refused
h5i browser audit                          # the whole session, afterwards
h5i browser close
```

Building something and then browsing it, which is what a box is for:

```bash
h5i box .                                  # a box from this repository
h5i box shell mybox                        # work in it (this is where an agent runs)
# inside: edit, build, start the dev server
h5i browser open http://localhost:3000 --in mybox
h5i box export mybox --out ./review        # patch + report + receipt + timelines
git apply --3way ./review/patch.diff       # apply it where you want
```

The full loop the browser makes possible:

```
agent edits code -> starts dev server -> opens a session against it
  -> reads the outline -> clicks and fills -> reads the request log and the
  console -> fixes the code -> human takes the controls, hands them back
  -> export patch, report, receipt, session timeline
```

---

## Install

```bash
curl -fsSL https://h5i.dev/install.sh | sh     # prebuilt binary
cargo install --path .                         # from source
```

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
| [`h5i browser`](#h5i-browser) | Browser sessions: open one, drive it, close it. Auditable by default, containable with `--in`. |
| [`h5i box`](#h5i-box) | Create, run, inspect and export boxes. The sandbox a session, an agent and a dev server can be placed in. |
| [`h5i box share`](#h5i-box-share) | Open one box's dev server to one other person. The only inbound path. |
| [`h5i ui`](#h5i-ui) | The box console: the whole fleet, as one read-only screen. |
| [`h5i runner`](#h5i-runner) | Pair a second Linux machine and run boxes there over SSH. |
| [`h5i skill`](#h5i-skill) | Write or print the agent skill this binary carries. |
| [`h5i join`](#h5i-box-share) | Open a box someone else is sharing, from their ticket. |
| `h5i completion` | Shell completions for bash, zsh, fish and friends. |

`h5i dev *` and `h5i env *` both remain as hidden aliases for `h5i box *`
through one release. The noun the product uses everywhere else is *box*, so the
command is too.

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

### The id is internal

Every session has an opaque id (`br_7k2xqa`), and it is in the record, in
`--json` and in the receipts, because a durable reference has to be something no
rename can break. It is not what you type. A CLI that demands an opaque string
on every verb is copying a remote-browser HTTP API, where the id exists because
the client and the browser share nothing else. Here they share a filesystem.

So a verb resolves its session in three steps, most explicit first:

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

There is deliberately *no* "if only one session is live, use it" rule. It reads
as helpful and is the same hazard as a moving default: an agent that opened one
session, had it end, and opened another under a different name would find its
next verb quietly landing somewhere it never asked for.

### `open` navigates a session that is already there

Opening a URL in a browser that is already up means *go there*. So `open`
navigates the session it finds, and `--new` is how you say you meant a second
one. The flags that only make sense at creation (`--allow`, `--in`, `--script`,
`--no-loopback`, `--expires-in`, `--restore`) are *refused* rather than ignored
when a session is reused: a session's policy is fixed when its engine starts, so
accepting a grant and doing nothing with it would be a grant the caller believes
it made.

### What is true by default

Started with no flags, a session runs on this machine in your ordinary process
space, like any other headless browser. There is no sandbox, and h5i does not
claim one.

What it does that another headless browser does not is record. The engine is the
HTTP client, so every request is checked against the session's policy and
written down *before* the bytes move, and the fetch is refused when the record
cannot be written. A request that is not in `h5i browser requests` did not
happen. That is a property of the engine, not of a container, so it holds
whether or not there is a box.

The honest name for that is auditability, and the CLI says so on every status
line:

```
requests : engine-claimed (fail-closed, and the engine's own account of what it fetched)
```

#### The page grants itself

A session reaches the URL it was opened on, and nothing else remote. Naming a
URL and then naming its origin again is ceremony that teaches nothing, so `open`
grants the page it was given exactly as `read` grants its targets. `--allow` is
for the origins beyond it: an API the page calls, a CDN it pulls from. Loopback
is reachable by default because it is the dev server, and `--no-loopback` takes
that back.

The grant is the page and not "and whatever this page pulls in". An off-origin
subresource is still refused, and still says so in the request log, which is the
part a wider default would have given away.

### Browser identities

A page can ask this engine who it is in four places: the `User-Agent` and
`Accept-Language` it sent, the `navigator` object it exposes, the `screen`
geometry it reports, and the offset `Date` computes local time from. Those four
answers used to come from four different files, and nothing kept them agreeing.
Disagreement is the only thing a fingerprinting script needs: it does not have
to know what h5i is to notice that the browser claiming Windows on the wire
reports `MacIntel` in script.

So a session has one identity, resolved when it opens, frozen for its life,
digested into its record, and read by every layer. Nothing randomises per page
load, which is itself a tell. A browser whose values change between two loads of
the same site is announcing itself, not blending in.

```
h5i browser identity list
h5i browser identity check firefox-143-linux --script
h5i browser open https://example.com --script --identity privacy
```

Three modes, because "stealth" would be one word for two strategies that pull in
opposite directions:

| mode | what it does |
|---|---|
| `native` | Answers as h5i, truthfully. The default, and byte-for-byte what h5i sent before identities existed. |
| `privacy` | Still h5i, with the two values that vary between *installations* pinned: the patch version in the agent string, and the host's time zone. One install stops being distinguishable from another. |
| `compatible` | Claims a different browser, coherently, and only as far as this engine can actually back the claim. |

`h5i browser identity show <name>` prints one as the TOML you would edit, and
`--identity <path>` takes a file. A file that contradicts itself is refused with
the field named: a Windows agent string over a `MacIntel` platform, a mobile
identity with no touch points, a viewport larger than the screen it claims, a
Firefox carrying Chrome's product token.

#### Refused, never half applied

An identity declares what it needs the engine to have, and one that needs
something h5i does not have is *refused*:

```
$ h5i browser identity check chrome-151-windows --script
✗ refused
    needs ua-client-hints: this engine sends no Sec-CH-UA and exposes no navigator.userAgentData
    needs webgl2: this engine has no WebGL
    Not applied in part: an agent string claiming chrome in front
    of an engine missing these is louder than no claim at all.
```

That refusal is the design rather than a gap in it. Chrome has sent `Sec-CH-UA`
on every request since it shortened its agent string, and exposes
`navigator.userAgentData` to match; a Chrome agent string in front of a browser
with neither is *more* detectable than an honest one, because the absence is the
signal. The identity ships anyway, and is usable unchanged the day a backend can
answer for it.

`firefox-143-linux` is the compatible identity this engine can back, and the
reason is the same fact from the other side: Firefox sends no client hints at
all, so its identity surface is the agent string, `navigator`, `screen` and the
locale, which is exactly what an identity covers.

#### What it does not cover

`identity check` prints this list as plainly as the first one, because the claim
is coherence and not invisibility:

- the TLS ClientHello, its extension order and its GREASE
- the HTTP/2 SETTINGS and pseudo-header order
- canvas and WebGL readback, which this engine cannot produce
- `AudioContext` fingerprints, which this engine has none of
- the installed font set, which is the host's
- pointer and keyboard event timing

A determined check still sees this engine for what it is. What an identity buys
is that the answers it *does* give agree with each other.

#### Optional, at both switches

Identities are a build feature (`identity`, on by default) as well as a flag,
and the two promise different things.

`--identity native` is the default and costs nothing at run time: the wire
values are settled once when the session is built, so a request parses the same
`Accept-Language` it always parsed, and with `--script` off (which is also the
default) there is no script realm to read `navigator` from at all.

Without the *feature*, the code is not in the binary. There is no identity
module, no `Screen` interface, no `--identity` to type and no `h5i browser
identity` to run; the engine sends the one agent string it has always sent. That
is a claim a hardened deployment can make about the file it installed rather
than about how someone invokes it, which is the same reason `ytdlp` is a feature
and not just a flag.

```
cargo build --release --no-default-features \
  --features "browser,ytdlp,web,share,runner"
```

One thing is deliberately not validated: the exit address. "A Japanese address
with a New York time zone" is a real incoherence and not one an identity can
see, because where a request leaves from is the egress proxy's business.
Language is not checked against time zone either. `navigator.languages` is a
preference, not a location, and `en-US` with `Asia/Tokyo` describes every
English speaker in Japan.

### `read`: one page, no session

```bash
h5i browser read https://example.com
h5i browser read url1 url2 url3 --json
```

```
confined : process (files and environment; the origin allowlist is the engine's)
```

For the shape a crawl has: fetch, read, move on. No cookies carried between
verbs, no `@ref` to click, nothing resident afterwards, and `h5i browser list`
shows nothing when it is done.

There is no `--allow` here, and the omission is the design. The engine is
fail-closed, so something has to name the origins; making you name a URL and
then name its origin again is ceremony that teaches nothing. So the targets
grant themselves, and only themselves. A page that pulls a script from a
third-party CDN, or redirects to another host, is still refused and still says
so in the log, which is the part a wider default would have given away.

Several targets share one browser (one connection pool, one cookie jar and one
font set across the batch) and a page that fails does not stop the ones after
it. `--json` returns the page, its request log, and what was holding the engine
together, which is what a crawl wants and what no other headless browser can
hand over completely.

#### `--in <box>`: an allowlist a tier enforces

An allowlist that is not simply "what I asked for" belongs in a file, not in
arguments. Write it in `.h5i/env.toml`:

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

The read runs inside that box, through the same `box run` you would type
yourself: the tier resolves the pinned policy, enforces egress at a network
namespace boundary outside the engine, and writes a receipt. The digest on the
line is the policy that was actually enforced, which is the thing an allowlist
assembled from command-line arguments could never hand back.

A read can have this and a session cannot, and the reason is the difference
between the two rather than a preference. A session is resident by design
(`snapshot` then `click @e3` needs the page to still be there), and the
supervised tier cannot hold a resident process yet: its seccomp-notify gate is
served by a thread inside the `h5i` process that started the run, so when that
command exits the gate has no server and every filtered syscall blocks. A read
runs to completion inside that command, which is the shape that tier already
has.

Aim a read at `localhost` and use no box: under a tier with its own network
namespace the loopback is the sandbox's, not the one your dev server is on.

### The default sandbox

A session on this machine runs in a process-tier sandbox: the same Landlock
filesystem scoping, seccomp filter and rlimits `isolation = process` applies,
built from a profile rather than resolved from a repository. There is no box, no
worktree and no manifest; a session is not one of those.

```
placed   : on this machine, in a process-tier sandbox (its files and its environment; not its network)
```

The reason a browser gets this by default is that a browser is the thing h5i
runs that most reliably parses bytes a stranger wrote. A bug in Blitz, Stylo, an
image decoder or Boa would otherwise be running as whoever started the session.

What it contains: the filesystem the engine can *write* (its own session
directory, and the two `/dev` sinks: nothing else, and nothing under `$HOME`),
the environment it can read (cleared, then only what was granted), how much it
can allocate, and the privilege-escalation and kernel surface seccomp denies.

Reading is a weaker boundary than writing here, and the difference is worth
stating rather than folding into "the system to read". The engine reads what any
confined h5i profile reads: `/usr`, `/lib`, `/bin`, `/sbin`, `/etc`, `/opt`,
`/proc`, and `/tmp`, which is where another agent's scratch tends to be. `$HOME`
is granted nothing, and `~/.ssh`, `~/.aws`, `~/.config/gh` and `~/.config/h5i`
are denied outright, so the files worth taking are not in reach; the ones in
`/tmp` are. Narrowing the list is not the fix, since `/etc` and `/proc` stay
either way, so this is left for the broker/renderer split, where the half that
parses the page gets a profile written for a process that only parses.

What it does not contain, and does not claim to:

- The network. A browser needs it, so the engine keeps the host's reachability,
  loopback included. The policy that decides *which* origins is the engine's own
  and a compromised engine is past it.
- Starting a program. `execve` is not denied. What makes that survivable is that
  Landlock's domain is inherited across `execve` and cannot be relaxed: a shell
  a compromised engine starts reads and writes exactly what the engine could.

It does not upgrade the request lane. A sandboxed session is still
`engine-claimed`, because a process-tier sandbox corroborates no part of the
log. Containing the network, and earning `host-observed`, needs a boundary
outside the engine: `--in`, below.

Two consequences worth knowing before they surprise you:

- Secrets are not inherited. Unconfined, the engine reads the whole environment,
  so a compromised one reads every `H5I_SECRET_*` on the machine. Confined it
  reads none unless named:

        H5I_SECRET_ACME_PASS=... h5i browser open https://acme.test --secret ACME_PASS
        h5i browser type @e2 '$H5I_SECRET_ACME_PASS'

    `--secret ACME_PASS` and `--secret H5I_SECRET_ACME_PASS` name the same
    credential. The value is resolved from the environment the command is
    started in, once, and delivered to that session alone. It lands in the
    session's broker process, not in the renderer that parses the page.

    Fail-closed: a credential that cannot be resolved refuses the session,
    rather than starting one that will decline the first `type` that needs it.
    The same in both shapes, because a host that cannot confine still has to
    answer `--secret` the same way.

    Not for `--in`. A session placed in a box gets its credentials from that
    box's policy, declared in `.h5i/env.toml` under the profile it was created
    with. `--secret` with `--in` is refused rather than ignored.

- Personal fonts are visible, and nothing else under `$HOME` is. `~/.fonts` and
  `~/.local/share/fonts` are granted read-only, because a font someone installed
  is not the page's font: the ones a page supplies arrive over `@font-face`, go
  through the broker and are parsed either way, so this adds no new parser
  input. The grant is exactly those directories: a file elsewhere under `$HOME`
  is refused.

    Two mechanics behind it. The environment is cleared inside the sandbox, so
    the engine's own `$HOME`-based discovery would find nothing; h5i computes
    the list, grants it, and passes it back as `--font-dir`, one list for both.
    And if a personal directory cannot be granted (a symlink that resolves
    somewhere the policy denies is the case that exists) it is dropped and the
    sandbox is kept, with a line saying so. A font path must never be the reason
    a session runs unconfined.

`--no-sandbox` runs the engine unconfined. Some hosts cannot confine at all (no
Landlock, an AppArmor profile, a CI container, macOS, Windows), and there the
session runs unconfined and says which, on the placement line and in the record.
A sandbox nobody can see is indistinguishable from one that was never applied.

### Two processes: the broker and the renderer

A session is two processes, and neither is a command you type. The one h5i
starts is the broker: it holds the allowlist, the HTTP client, the receipt sink,
the cookie jar, the per-page budget and the credentials. It starts the renderer,
which parses the HTML, runs the cascade, decodes the images and fonts, runs the
page's script and draws the frame. The renderer holds none of the things in that
first list. It asks; the broker decides and records.

```
$ ps -o pid,ppid,args
  55509  36242  h5i __engine serve https://example.com ...
  55511  55509  h5i __engine --brokered serve https://example.com ...
```

The reason is that a browser's parsers are where a stranger's bytes are read. A
bug in Blitz, Stylo, an image decoder or Boa used to be a bug in the process
that also held the policy, the jar and every `H5I_SECRET_*` in the environment.
Now it is a bug in a process whose environment has none of them, and whose only
route to the network is to ask something else, which writes a receipt first.

What this buys, exactly:

- Credentials named with `--secret` are in the broker's environment and not in
  the renderer's. A page-bound `type` resolves through the broker, so an intact
  renderer receives the value for the field it was told to fill and no other. A
  compromised one can ask the broker to resolve any credential this session was
  granted, which is the narrowing the split actually buys: it cannot reach one
  the session was not granted, and unconfined that would have been every
  `H5I_SECRET_*` on the machine. Narrowing it further needs the control channel
  to move to the broker, which is not built.
- The receipts path, the egress proxy and the allowlist are likewise absent from
  the renderer.
- The cookie jar is not in the renderer's memory. Script sees the non-`HttpOnly`
  subset because it asks for it, which is the same answer it always got, from a
  place a parser bug cannot reach past.
- When the renderer dies the broker knows, with a status. When the broker dies
  the renderer stops, because a renderer that cannot receipt is not a browser.

What it does not buy yet. The renderer is in the broker's network namespace, so
it can still open a socket of its own. The split moved what a compromised parser
can reach *in memory*. The request log becomes evidence against a compromised
parser only when the renderer's own profile denies the network, which is
`docs/roadmap-history.md` §B18.7 step 3 and is not built.

`H5I_BROWSER_NO_SPLIT=1` runs the engine as one process, the way it ran before.
It is there for comparing the two shapes, and for a host where spawning is the
problem. A spawn that fails falls back to one process on its own, and says so.

### `--in <box>`: the same session, inside a box

```bash
h5i box --profile browser --engine h5i --name web
h5i browser open https://example.com --in web
h5i browser snapshot  # identical verb, identical answer
```

`--in` changes nothing you type. It changes who saw the network, when the box's
tier is one that applies its egress allowlist at its own boundary. That boundary
is outside the thing being described, so the session's lane is upgraded:

```
requests : host-observed (also seen at the box's boundary, outside the engine)
```

Being in a box is not by itself enough to earn that line. A box whose policy
lets the engine reach the whole host network corroborates nothing, and such a
session stays `engine-claimed`. What earns `host-observed` is an egress
allowlist or a `deny` net mode: enforcement outside the engine.

Can `--allow` get around the box's `net.egress`? No, and the reason is at
creation rather than in the browser. A profile that declares an egress allowlist
cannot be created at a tier that cannot enforce one:

```
$ h5i box create webtest --profile browser --isolation process
Error: profile 'browser' sets a net.egress domain allowlist, but isolation
'process' cannot enforce it (process-v1 supports net.mode deny|host only) —
use a supervisor/container backend or drop net.egress (fail-closed)
```

So a box whose list reaches the engine is a box with a boundary underneath it.
h5i does hand that list to the engine, and `--allow` and the start URL add to
it, but what they add is what the engine will *ask* for. What leaves the box is
still decided outside it:

```
$ h5i browser read https://example.com --in webbox   # webbox allows en.wikipedia.org
  confined : box webbox, policy 2bb35dc2fa6c...
https://example.com: could not open https://example.com/: denied by policy:
`example.com` could not be resolved: Temporary failure in name resolution
```

The engine granted the target it was handed, exactly as `read` always does, and
the box's pinned DNS refused it anyway.

A box that declares no egress list has nothing to widen: its net mode is host or
deny, nothing outside the engine is deciding about hosts, and a session there
keeps saying `engine-claimed`. That line, not the tier's name, is what says
whether anything corroborated the request log.

Two mechanics are worth knowing, because they explain the shape of the feature:

- The engine runs as a service, not as a run. `h5i box run` holds the box's
  exclusive writer lock for the life of the command, so a resident engine
  started that way would lock every later verb out of its own box. It is started
  the way `h5i box service start` starts things, which takes the service lock
  instead.
- Verbs are carried in, over a socket. Each `h5i box run` gets a fresh network
  namespace, so a port bound by the resident session is unreachable from the
  next run. The connect fails with `ENETUNREACH`, which reads exactly like a
  session that is not running. The control channel inside a box is therefore a
  Unix socket in the box's own `/tmp`, because the box's filesystem is one
  filesystem across every run in it.

Carrying the verb in has a second consequence, and it is the useful one: the
control lock is checked on the host, outside the box. For a boxed session that
makes a human takeover a boundary rather than a request, which the arrangement
with the agent inside the box structurally cannot be.

`--in` needs a tier that can hold a resident process: `workspace`, `process` or
`microvm`. The `browser` profile's egress allowlist needs `supervised` or
`container`, and those two cannot hold a service yet, so on Linux today the tier
that does both is `microvm`. `h5i browser open --in` says which of these applies
to your box before it starts anything, rather than timing out.

### Opening a session from inside a box

An agent already in a box does not need `--in`, and cannot use it: `--in` means
"put this session in a box I am outside of", which is what lets it promise an
enforced takeover and a lane the engine did not claim for itself. From inside,
neither is true, so it is refused with the reason rather than silently doing
something weaker.

Open it without the flag. It runs beside the agent, in the same box, and the
record says exactly that:

```
placed   : this machine, which is box env/human/web (its policy is not readable from in here)
requests : engine-claimed (fail-closed, and the engine's own account of what it fetched)
```

Two things are deliberate in those lines. The box is *named*, because a session
there is not uncontained and saying "no containment beyond the engine" would
understate what is true: the same class of error as overstating it, in the
direction that happens to be safe. And nothing is claimed about what the box
enforces, because the policy is host-side and sealed: from in there, h5i cannot
read its own boundary.

The control channel inside a box is a Unix socket rather than a loopback port.
Not a preference either: a box's netns may have no usable loopback at all
(`net.mode = deny` leaves nothing to dial), and every `h5i box run` gets a fresh
one, so a port bound in one is unreachable from the next.

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

## video: difference engines, briefly
source: https://example.com/talk.mp4

track: captions [en] "English" (default) — receipt #7

[00:01] Ada: Difference engines, briefly.
[12:40] And that is the whole of it.
--- END UNTRUSTED PAGE CONTENT ---
```

Nothing here decodes audio, and `capabilities.video` stays `false`. The tracks
are fetched through the same broker as any image: policy-checked, receipted, and
attributed to the page that declared them, so a `<track>` pointing at loopback
from a page on the open web is refused exactly as an `<img>` there would be. The
receipt number beside each track is the row in `h5i browser requests` that paid
for the text.

Media with *no* track is reported as media with no track, not as silence: that
is the fact that says the words exist only in the audio, and it is the one a
caller routes on.

At most two tracks are read per media element: one of the *words*, and the
*outline* of them. A well-localised player declares thirty languages and they
are the same words thirty times, so `--lang en` names which one; a `chapters`
track is different information rather than a translation, so it is read
alongside. `metadata` tracks are machine payload the page reads with script, and
`descriptions` is for a screen reader; neither is a transcript.

#### When the captions are not in the markup: `--via yt-dlp`

Some sites do not put their captions in a `<track>` at all. YouTube is the one
that matters most: its transcript lives behind the player's own JSON API, and
reaching it takes a program that knows how that API works. `--via yt-dlp` is
that lane, for about 1,700 sites:

```bash
h5i browser transcript --via yt-dlp --url https://www.youtube.com/watch?v=…
```

It is a different lane, not a better one, and the difference is the point.
yt-dlp opens its own sockets from a process the engine never sees. Nothing it
fetches is in `h5i browser requests`, and nothing can be. The engine did not
decide about those requests and did not see them, so a log that listed them
would be an observation dressed as a decision record. The reply says so in an
`evidence` line, and the run itself lands in `h5i browser audit` as a
host-observed row carrying the exact argv h5i executed:

```
  sources  : actions read · requests read · control empty · helpers read

  host    session opened  (…)
  engine  #0 GET https://www.youtube.com/watch?v=…
  host    helper yt-dlp --ignore-config --skip-download --no-playlist … -- https://…  412 cue(s) in en
```

Four things are deliberate:

- Never implicit, never a fallback. An engine read that found no captions stays
  a read that found none. A silent fallback would move a session's traffic
  outside the engine's log without anyone asking, which is the one thing that
  log promises cannot happen.
- It runs where the session runs, and with no session it runs here. A boxed
  session runs yt-dlp *inside its box*, and a boxed session whose box has no
  yt-dlp is refused rather than served from the host: running it outside would
  move the session's network to a boundary its caller did not choose. Whether
  that box has an egress boundary at all is a separate question, and the reply
  answers it rather than assuming: a tier that confines files and environment
  and not network is reported as exactly that. Note the standing Linux trade-off
  here, the same one `open --in` reports: the tiers that enforce egress cannot
  hold a resident session, so today a boxed session is on a tier with no network
  boundary unless it is `microvm`.
- It gets no credential. `--secret` grants are resolved by the broker on the way
  into a page, and this lane has no page and no broker. The child's environment
  is built from a short allowlist that does not include the `H5I_SECRET_`
  namespace.
- `--ignore-config` is passed, so a `~/.config/yt-dlp/config` cannot add flags
  h5i did not choose. The argv in the audit is what actually ran.

One tag, one invocation, and both the author's captions and the machine
transcript for it, the second because a video whose only transcript is
machine-generated is exactly what this lane is for. Which languages a video has
is listed either way, from the metadata, so nothing is downloaded to find that
out.

`--lang` is an *exact* tag, because yt-dlp matches it with a regex `fullmatch`
and widening is not free: YouTube answers `en.*` with `en` plus `en-de` and
`en-en`, the author's captions and two machine translations of them, which is
three downloads and a rate limit where one download was wanted. The value is
still a regex, so `--lang 'ja.*'` is how you reach a machine translation, which
YouTube tags by its pair (`ja-en`) rather than by the language alone. A run that
matches nothing names the tags the video does have and points at that flag.

The exit code follows what arrived, not what yt-dlp returned. It exits non-zero
if any part of a run failed, so a run that wrote the transcript and then hit a
rate limit on a second language is a complete answer reported as a failure. h5i
reports it as an answer, and puts the partial failure in the note beside it,
because that is what explains a missing title.

No session is needed when `--url` names the media. There is no page here to
render, so the only thing a session was ever contributing to this lane is a
placement, and a run with none happens on this machine. It says exactly that in
its `evidence` line, it opens no session and leaves none behind, and it is still
written down: `h5i browser audit --no-session` lists the runs that belong to no
session, with the same argv and the same host-observed lane as a row inside a
session's timeline. They are kept out of that timeline on purpose, because a run
that was not part of a session must not appear inside one. To put the run behind
a boundary, open a session with `--in <box>` and it runs in there.

A host session's helper is contained by whatever started h5i, and the reply says
that too rather than implying a boundary that is not there. Automatic captions
are labelled as automatic, because a machine transcription mishears names and a
reader about to quote one should know first.

One helper run is given two minutes, after which h5i stops it. The stop is a
result and not an error: whatever the run had already written is still read and
returned, with the stop named in the note, because a run killed at the budget
has usually written some of what it was asked for. `H5I_HELPER_BUDGET_SECS`
overrides the two minutes, clamped to between one second and thirty minutes, for
a link slow enough to need it.

The lane is behind the `ytdlp` cargo feature, on by default. Build with
`--no-default-features` (plus the features you want) for a binary with no code
path that can exec a helper at all.

A *locator* names an element by what it is called rather than by where it sat.
`--role button --name 'Sign in'` survives a re-render that moves everything; a
`@ref` from an older reading does not, and is refused rather than resolved
against whatever now occupies that position. `--selector <css>` is the third way
in. `find` reports what a locator matches without acting on it.

`set-checked`, `select` and `press` take either a `@ref` and the value, or a
locator and the value. With a locator there is no ref, because the locator is
the handle. Prefer `set-checked` to clicking a checkbox: a click *toggles*, so
where it lands depends on what the page was serving, and setting a state is
idempotent. That is the difference between a session that replays to the same
place and one that does not.

`script --save` writes what the session did as steps of verified CSS selectors,
so the script outlives the reading it was recorded from. `h5i __engine replay`
sends each step back through the control channel an agent would use, so the
policy, the receipts and the action log see a replay exactly as they see a live
session.

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

- The two lanes stay apart. The action and request rows are the engine's own
  account of itself; the handovers and the lifecycle are h5i's, written from
  outside. Every row says which. Merging them into one confident-looking column
  is the exact confusion the lane split exists to prevent.
- It orders across sources. "Was a human at the controls when that form was
  submitted" is a question about two logs at once, and a current-holder field
  cannot answer it. The engine stamps its own rows and h5i stamps its own; the
  engine's clock is the engine's claim, and the output says so.
- It says what it could not read. `sources` reports each log as `read`, `empty`
  or *`unavailable`*. An empty timeline over a log h5i cannot see looks exactly
  like a session that did nothing, and those are different findings.

Rows carry `caused_by` where the source recorded the link, so a fetch can be
traced to the verb the page was under when it went out. Nothing here infers a
link from timing: a request that merely happened near a verb is not a request
that verb caused.

`--json` gives the whole thing, including the session record. It is the same
structure `h5i box export` writes for each session placed in a box.

### Sessions end, and endings are recorded

A session directory outlives the session. Closing one writes the ending into its
record instead of deleting it, which is what makes "how did this end" answerable
afterwards, and what makes an id impossible to reuse.

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

The distinct code is the point. An agent whose retry cannot tell "the session is
gone" from "the click did not work" is an agent that silently starts a second
browser and loses both the page it was reasoning about and the record of how it
lost it.

`--restore` is an inheritance, not a resurrection: it produces a *new id*, and
writes `restored_from` into the new record.

What it carries is the *cookie jar*, and only that. A session mirrors its jar
into its own directory while it runs: owner-only, written whenever the jar
changes rather than at exit, because a session is stopped with a signal and a
shutdown hook would never run. So a login a human performed once at the live
view survives into the next session:

```bash
h5i browser open https://example.com/login --session auth
h5i browser login --session auth        # the human types the password
h5i browser login --session auth --off
h5i browser close --session auth

h5i browser open https://example.com/app --restore br_7k2xqa   # still signed in
```

No verb returns a cookie value, and this adds none: the file is handed to the
next engine, never to a model. A session that left no jar (one that ran in a box
whose `/tmp` this machine cannot read, or one from before this existed) is
refused by name rather than silently seeding nothing.

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

### The control lock

Two clients can drive one page: the agent, and a human at the live view.

- The agent holds control by default. A session exists to let an agent work; it
  should not have to ask.
- A human takes control, never asks for it. `h5i browser take <session>`. The
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

`--engine` picks what runs pages in the box, and the choice is pinned in the
digest: a run never quietly falls back to a different one.

| engine | what it is |
| --- | --- |
| `chromium` (default) | A real browser. Everything works; it costs what a browser costs. |
| `lightpanda` | A third-party headless engine. |
| `h5i` | h5i's own engine, part of this binary. Described below. |

#### The h5i engine

An engine built for the case an agent is actually in: read a page, act on it,
read what changed. It runs in *one process* and holds a page in roughly a
seventh of the memory Chromium needs (76 MiB against 563 MiB, median over the
same pages), from a binary a ninth the size. It is about 30% *slower*, because
it has an interpreter where Chromium has a JIT, and that trade is the whole
point of choosing it deliberately rather than by default.

It runs page JavaScript, follows redirects and `<meta refresh>`, keeps a cookie
jar scoped to one origin, and puts every request through the same broker as the
rest of h5i, so a page's own fetches are policy-checked and receipted like
anything else. What it reads is an outline with `@ref` handles, not pixels.

Two properties to check before you rely on it:

- It says what it could not do. A page needing an API this engine lacks gets
  that API *named* in the snapshot rather than a blank space, and console errors
  carry the script and line they came from. "The page is empty" and "I could not
  read the page" are different answers, and it gives different answers.
- It is not a complete browser, and does not pretend to be. Of twenty
  single-page applications measured, eighteen read usefully and one not at all.
  Canvas, WebSockets, Workers and IndexedDB are absent. For a page it cannot
  read, the answer is `--engine chromium`.

Driven directly it has its own CLI (`h5i __engine --help`), which is what `h5i
browser` sits in front of. See [The engine, underneath](#the-engine-underneath).

Fonts are found by walking the system font directories at startup, not linked in
at build time, so `h5i __engine doctor` reports what it found and `--font-file`
names one directly. The scan keeps a budget of two dozen files, and what it
spends them on is a preference order: the regular text faces first, then an
emoji face, then weight and slant variants. Emoji sit ahead of the variants
deliberately: a slant can be synthesised and an emoji face is the only cover for
a range no other font on the system has.

Colour emoji render as colour, both the outline kind (COLR) and the embedded
bitmap kind that `NotoColorEmoji` uses. A `--font-file` naming a bitmap-only
face is registered but ordered behind every face that can draw an outline,
because such a font also claims the digits, `#`, `*` and the space for its
keycap sequences: in front, it wins those characters, draws none of them, and
reports each as a full emoji square, so the page loses every number and every
word space at once. That reads as a broken layout engine rather than a font
problem, which is why the ordering is a rule and not a preference.

### Driving Chromium

`h5i browser` drives h5i's own engine. When the box is pinned to `chromium`, the
driver is `agent-browser`, run *inside* the box, and its `--help` is the verb
table. h5i does not wrap it: forty automation verbs behind a second CLI would
buy nothing but drift.

```bash
agent-browser open http://localhost:3000
agent-browser snapshot                  # accessibility tree with @refs
agent-browser click @e2
agent-browser fill @e3 "test@example.com"
agent-browser screenshot shot.png
agent-browser stream enable             # so `h5i box view` has something to show
```

The trade is worth stating plainly. Chromium reads pages this engine cannot, and
gives up the two properties `h5i browser` is built on: its request lane is
best-effort rather than fail-closed, and its control channel is inside the box,
where the control lock is a request rather than a boundary.

What the `browser` profile does to Chrome, and why:

- Fresh profile, created in the box. No host cookie jar, extension or history.
  Nothing you are logged into on the host is logged in there.
- Chrome's egress is the box's egress, enforced by the tier. Loopback stays
  open, because the dev server under test is the whole point.
  `--allowed-domains` is derived from the same policy as a second layer.
- AI chat is refused. `agent-browser chat` sends page content to an external
  gateway, which inside a box is an exfiltration path with a friendly name. The
  gateway credential is never injected, and its absence is the mechanism.
- On macOS it is granted the host's per-user temp directory
  (`/var/folders/<xx>/<yy>/T`), read and write. This is the widest thing any
  profile asks for, so understand it before you use it.

    Chrome puts its ProcessSingleton lock socket there, and it finds that
    directory through `confstr(_CS_DARWIN_USER_TEMP_DIR)` rather than `TMPDIR`.
    The per-env `/tmp` redirect cannot move it, and without the grant Chrome
    will not start.

    The cost is that the directory is shared: a browser box can read what other
    host processes leave there and can plant files they will pick up. That is
    exactly the cross-agent rendezvous point the `/tmp` redirect exists to
    remove, reintroduced for this one profile on this one platform. Other
    profiles, and every profile on Linux, are unaffected.

    Two consequences of that grant being a machine-specific absolute path. The
    pinned policy digest differs between two Macs for the same profile. That is
    harmless, because `policy.resolved.toml` is verified against the digest
    stored beside it, and both are written together at create time; nothing
    re-resolves the profile and compares. But a `browser` env created on one
    machine and pulled to another carries a grant for a directory that does not
    exist there, so Chrome will fail to start until the env is recreated.

---

## h5i box

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

The output gate. A box has no write access to anything outside itself; this is
the only way out, and it is deliberately a human step.

```bash
h5i box export <name> --out ./review
```

Produces:

| File | What it is |
|---|---|
| `patch.diff` | The tree diff against the pinned base, path-validated: no symlink escapes, no nested `.git`, no agent-introduced gitlinks. |
| `report.md` | What ran, what the browser saw, what the kernel saw, who was at the controls, and the agent's own proposal. |
| `receipt.json` | Every observed execution, with the policy digest that was enforced. |
| `receipts/<id>.raw` | The full account of each ingress session: who connected, over what path, for how long, how much moved, what was refused. Present when the box was shared. |

It refuses rather than overwrites an existing non-empty directory (`--force` to
replace). Secret redaction and size caps apply to all of it.

Read `report.md` before applying. It surfaces, in this order:

- denied egress attempts: the box tried to reach hosts the policy refused
- *what ran*: every command, its lane, its exit code
- what the browser saw: console errors, uncaught exceptions and failed requests,
  observed by h5i rather than reported by the agent
- what the kernel saw: signatures that fired against the syscalls a box actually
  made, when runtime detection was on for the run
- viewer sessions: including whether a human took the controls
- the agent's proposal

Then apply it where you want:

```bash
git apply --3way ./review/patch.diff
```

`h5i box apply <name>` still lands a proposed box onto its parent branch in this
repository, for the local case where that is what you want. It refuses for a
detached box.

### h5i box cache

Cold dependency install is the difference between a 20-second box and a
four-minute one, so warm caches are in scope.

```bash
h5i box cache ls              # caches for this project, and whether they are stale
h5i box cache mounts          # exactly what a box would get
h5i box cache refresh <eco>   # populate one, in a dedicated box with no agent in it
h5i box cache rm <eco>
```

Rules that make this safe rather than merely fast:

- One cache per project and ecosystem, keyed by a digest of that ecosystem's
  lockfiles. A cache whose key no longer matches is listed as stale and never
  handed to a box: packages resolved for a different dependency set are a
  silent, hard-to-explain wrong answer.
- Mounted *read-only* into an agent box. That costs nothing in correctness:
  every package manager falls back to fetching what it cannot find.
- Written *only* by `h5i box cache refresh`, which runs the install step alone,
  with egress narrowed to the registry hosts and no agent inside. `refresh`
  needs a project-declared profile whose egress is the registry hosts and
  nothing else, and it refuses with that profile written out ready to paste
  rather than creating a box whose fetch could not have worked.

No mutable surface is ever shared between an agent box and anything else.

### h5i box view

Watch a box's browser from the host, and take over when you want to.

```bash
h5i box view <name> [--port 7331]     # serve it to your browser
h5i box view <name> --term            # draw it in this terminal
```

The box has to be running (a live `h5i box shell` or `h5i box run`), and its
browser has to be streaming (`agent-browser stream enable`, inside the box).

This is a security boundary, not a convenience, so here is what it does. The
box's stream port is *never published*: it stays inside the box's private
network namespace, and h5i enters that namespace by pid, connects from inside,
and hands the socket back out. Then:

- the forward binds loopback only, on a port h5i chose
- every connection carries a *per-box token*, minted at creation and never
  written anywhere the box can read
- cross-origin WebSocket handshakes are refused, so a page you have open in
  another tab cannot reach a running box
- frames flow out always; input flows in only for the control-lock holder

#### In the terminal

`--term` draws the page in the terminal you are already in, next to the agent.
It also works over SSH, which the browser viewer does not without forwarding a
port. It needs a terminal that speaks the Kitty graphics protocol: kitty,
Ghostty, WezTerm and Konsole all do. When yours does not, h5i says so and points
you at the browser viewer rather than filling the screen with base64.

This path binds nothing at all. There is no port, no token, and no page: the
viewer runs inside the command you just typed, enters the box the same way the
forward does, and holds the socket itself. Nothing else on the host can connect
to it, so there is nothing to authenticate.

The other direction matters as much. A terminal's output is not text: an escape
sequence can rewrite your clipboard, retitle the window, or ask the graphics
protocol to read a file. So the box never writes to your terminal. It supplies
compressed pixels inside a WebSocket message, and every escape sequence you
receive is generated by h5i.

Row one is h5i's, and a page cannot draw on it or be clicked through it. It
carries the box, the mode, who holds the control lock, the page's origin, the
egress policy, and a count of console errors. The origin is the field that never
gets shortened at the expense of the truth: a long URL loses its path, and a
host too long for the row is cut from the left, because shortening
`bank.example.evil.test` from the right is the trick itself.

Two modes:

| Key | Does |
| --- | --- |
| `i` | Take the control lock and start driving the page |
| `Ctrl-]` | Hand control back and return to watching |
| `q` | Leave |

Watching is read-only and leaves the mouse to your terminal, so selection and
scrollback still work. Driving takes the lock, because in a terminal viewer
there is no second window to run `h5i browser take` in. Leaving hands it back.
Both are recorded in the receipt, under the same lane the browser viewer uses.

Two limits. A terminal reports key presses and not releases, so h5i sends a
press and a release together: typing works exactly, and holding a key down does
not. Clicks are placed at the resolution of a terminal cell, which is fine for a
form and coarse for a dense canvas.

### The engine, underneath

The engine is part of the `h5i` binary. `h5i browser` runs it as a separate
process and speaks a protocol to it, the way it always has; what changed is that
it execs itself to get there instead of a second file.

It used to ship as `h5i-browser-light` beside `h5i`, and two files bought three
problems. The default install left `h5i browser open` with nothing to render a
page. Two halves of one protocol could drift apart with no handshake between
them. And a box could *read* the engine without being allowed to `exec` it,
because Landlock makes `~/.cargo/bin` readable and not executable, so `command
-v` found it and `exec` refused it.

The engine's own CLI is still reachable, hidden, for the cases that want it
directly:

```bash
h5i __engine --help
h5i __engine open https://docs.rs/ --allow docs.rs   # one-shot render, then exit
h5i __engine doctor                                  # what fonts it found
h5i __engine skill install                           # the engine's own skill
```

Take it deliberately. `h5i browser` is the surface that knows about session
names, placement, the control lock and the audit; `__engine` is what sits under
it, and reaching past the front door means giving all of that up. It is hidden
from `--help` for that reason and documented here for the same one.

What the engine gives you with no box is a browser whose whole network activity
is in a log you can read. What a box adds is that the agent cannot go around it.

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

`h5i box watch` is the tail of the receipt rather than a viewer: no viewport, no
panes, no control lock, and nothing it prints can take the controls. It is meant
to be piped, grepped, and left running in a second pane while an agent works.

Every row names the lane that observed it and the grade of that evidence, as
words:

```
09:14:02  box  fail-closed  request   allow  GET https://docs.rs/blitz/  #41 subresource
09:14:02  box  fail-closed  response  200    #41 12.0 KB, 84ms
09:14:03  box  fail-closed  request   DENY   GET https://telemetry.example.com/collect  #43
09:14:03  box  fail-closed  policy           telemetry.example.com: not in net.egress   (<- #43)
```

Terse is not licence to drop the qualifier. A row that did not say whether the
box or the host observed it would assert more than h5i knows, so the lane and
the grade are on every line and colour never carries them alone.

`--deny-only` keeps a refusal's *pair*: the request row carries the method and
the URL, the verdict row carries the reason, and dropping either leaves half an
answer. `--json` emits the same event envelope the console reads, one object per
line, so the three readers of that stream agree on the wire shape.

Only h5i's own browser engine writes a live request log, and an image-backed
tier keeps it out of the host's reach. `watch` says so in its header rather than
leaving an empty screen to be interpreted.

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

Let one other person try the web app in a box, from their own machine, while it
is still running inside the boundary. This is the one path in h5i that lets
something *in*, so read this section before you use it.

```bash
h5i box share <name> [--port 3000] [--expire 60m] [--label alex]
h5i box share <name> --direct-only    # fail rather than relay a single byte
h5i box share <name> --tunnel         # any browser, no h5i on their side

h5i box share status <name>
h5i box share ls
h5i box share grant <name> [--label sam] [--expire 30m]   # a second ticket (--tunnel only)
h5i box share revoke <name> <grant>
h5i box share stop <name>
h5i box share stop <name> --force            # delete the record, whatever it says
```

The other side runs:

```bash
h5i join h5i1_eyJ2IjoxLCJib3hf…
h5i join -                            # the same, with the ticket on stdin
h5i join - --shared-jar               # on a machine with only 127.0.0.1 to offer
h5i join - --bind 127.0.0.1           # WSL: the only loopback Windows can reach
```

The share needs a live session of the box to dial into, not a live dev server.
Starting one before the server is up warns and carries on. Visitors get a `502`
saying the server is not up rather than that their link is bad, and the share
starts working the moment the port binds. That is measured, and the receipt
counts those attempts on their own `unreached` line.

The session is the constraint. A second `h5i box run` is refused while the first
holds the box, so the server has to come up in the session that is already
there.

What the joining side is trusting. A ticket names a machine to dial and h5i will
dial it, so a ticket is worth the same care as any link. h5i refuses one that
names your *own* loopback or link-local addresses, but a ticket from a stranger
is still a stranger's.

The page then arrives on `http://127.<x>.<y>.<z>:<port>`, a loopback address
picked at random for this join. A cookie jar is scoped by *host* and ignores the
port, so on `127.0.0.1` that jar is shared with every local service you run, in
both directions: the token this proxy sets would reach them, and every cookie
they have set would be forwarded to the box. An address of its own gives this
share a jar nothing else has written to.

On Linux none of this arises: the whole of `127.0.0.0/8` is routed to `lo`, and
every join gets an address of its own. macOS configures only `127.0.0.1` on
`lo0`, so there the bind falls back to the shared jar, and the two directions
get different answers, because only one of them can be answered.

Your cookies going *into* somebody else's box is narrowed, though not closed. On
a shared jar, only cookies the box itself set are ever sent back to it, learned
from its own `Set-Cookie` headers, so a `session=` belonging to your own local
app is never forwarded by this proxy. The cost of that filter is that a cookie
the app wrote from JavaScript never reaches the box either. Nothing in a
`Set-Cookie` says so, and `h5i join` tells you when it applies.

What the filter cannot reach is the page itself. It is served on `127.0.0.1`,
cookies ignore the port, and script on that page can therefore read any
non-HttpOnly cookie in the jar and send it to the box under its own steam. The
filter governs what h5i hands over, not what the page does. A private window
with nothing else open in it is the answer to that half, and it is why `h5i
join` refuses this jar unless you ask for it.

The token going *out* to your other local services has no fix that does not need
a cookie host of h5i's own, so it is not fixed. It is yours to decide, and `h5i
join` refuses that jar unless you pass `--shared-jar`. If you would rather not,
`sudo ifconfig lo0 alias 127.0.0.2` gives h5i an address to take instead, which
lasts until you reboot. Keep that one for h5i: an address you also run your own
services on is a jar shared with them, and h5i cannot tell why it is there.

WSL is the case where the private address works against you. Windows forwards
only `127.0.0.1` into the VM, so the address a join picks binds fine, the P2P
path comes up, and the URL is still dead in every Windows browser: nothing on
the Linux side fails. `h5i join` prints a warning when it sees this. The answer
is `--bind 127.0.0.1`, which binds that address exactly and counts as shared-jar
consent on its own, with the same cookie filter and the same caveats as
`--shared-jar`. Any other loopback address given to `--bind` keeps a jar of its
own. An address outside `127.0.0.0/8` is refused outright: the proxy is a door
for one browser on this machine, never a way to republish the box to the
network.

Two things the joining proxy refuses outright. A service worker registration,
because one would keep control of that address after the share ended. And a
request that fetch metadata says came from another page, including one from
another `h5i join` on the next port. That is a *different origin* but the *same
site*, so nothing in the browser holds the credential back between them, and one
share's page could otherwise drive another's box with the credential attached.

Nothing else the page does is sandboxed by h5i, and some of it outlives the
share: cookies, cached responses, `localStorage`, any permission you grant, and
anything it persuades the browser to download. All of that belongs to whatever
you next run on that address and port, which is why `h5i join` binds an
ephemeral one unless you ask for a fixed one. A private window is the simple way
to keep none of it.

The box's port is never published. h5i enters the box's network namespace by
pid, the same way `h5i box view` does, dials `127.0.0.1:<port>` from inside, and
passes the socket back out. No TCP listener is bound on an external address and
no port is forwarded, so the box gains no reachability it did not have.
Peer-to-peer mode does bind a UDP socket that anyone may send to, because hole
punching requires it, and everything that arrives on it has to present a ticket
before it becomes anything. The claim is about the box's port, not about there
being no socket.

That entry happens *once*, at startup. A small helper process lives in the box's
namespaces for the life of the share and answers "connect me", pinned to the one
port you named. Nothing on the wire, from a peer or from the shared page, can
move where it connects.

A share needs the box's port to be distinguishable from the host's, and the two
platforms establish that differently.

On *Linux* it rests on the box having a network namespace this machine can
enter: only then is "the box's port 3000" a distinct thing from this machine's
port 3000. Without one, sharing would publish whatever happened to be listening
on the host, so `h5i box share` refuses rather than guessing.

A box has a network of its own when it is running and at the `supervised` or
`container` tier, or at `process` with a profile that denies egress. Having one
is not enough, though. The second condition is about the *profile*: a profile
that denies egress gets an empty namespace with no loopback brought up in it, at
*every* tier, so nothing inside can reach even itself. `h5i box share` refuses
such a box rather than minting a ticket that can never move a byte. What works
is a profile with an egress allowlist, so `agent`, `agent-claude` or
`agent-codex`, because the uplink those get brings a loopback with it. Sharing
does not work at `workspace`, or at `process` with a profile that grants egress,
because both share the host's network. `scripts/share_matrix.sh` checks this
combination by combination.

On *macOS* there are no namespaces and a box binds the host's loopback, so the
two ports really are the same port, and h5i asks Darwin who holds it. None of
the tier and profile advice above applies there. What matters is that the box is
running and its dev server is the listener.

h5i shares the port only when the listening socket belongs to a process of that
box (the session and its descendants), and refuses when it belongs to anything
else, naming what holds it. The refusal is not hypothetical: a stray `serve.py`
left on port 3000 is enough, and h5i will not publish it just because it
answers. Two processes holding the same address (`SO_REUSEPORT`) is refused too,
since the kernel, not h5i, would decide which one a visitor reached. That check
runs again on *every* connection, so a box whose dev server exits cannot have
its share inherited by the next process to claim the port. macOS boxes at the
`container` or `microvm` tier run inside a VM, where no host process holds the
port at all; those are refused, and say so.

`scripts/share_macos.sh` checks the four outcomes on that platform: a stranger's
port refused, an empty port warned about, a box shadowed by a more specific
listener refused, and a visitor reaching the box rather than the stranger.

### The ticket is the whole access model

A ticket names one box, one port, one grant and an expiry, and carries a 256-bit
secret. Holding it is the authorization; there is no account on either side. h5i
keeps only the secret's SHA-256, so a ticket is printed once and cannot be
reprinted. Mint another with `share grant`.

Because holding it is the whole authorization, where the text ends up matters.
`/proc/<pid>/cmdline` is world-readable on an ordinary Linux box and `h5i join`
runs for the length of the session, so `h5i join h5i1_…` leaves a working invite
in the process table for every other user on that machine to read, and in shell
history besides. `h5i join -` takes the ticket from stdin instead:

```
pbpaste | h5i join -          # or: h5i join - < ticket.txt
```

A ticket is a capability, not a seat. Nothing marks one as used or binds it to a
person, so forwarding the text admits everyone it reaches, under the one grant.
What one ticket per person buys is that `share revoke <name> <grant>` cuts off
exactly the people you gave *that* ticket to, rather than everybody.

The receipt is where a forwarded ticket becomes visible. A grant used by more
than one peer gets a line of its own saying so, because two endpoint ids against
one grant id is otherwise something a reader has to spot for themselves in a
list that can run to 256 entries.

`share grant` mints a second ticket, but only for a `--tunnel` share today: a
peer-to-peer ticket needs the running endpoint's addressing, and only the
serving process has it, so `grant` refuses on a P2P share. Adding a second peer
to a P2P share means stopping it and starting a fresh one, which invalidates the
first person's ticket too, so mint new tickets for everybody, including whoever
was already connected. Starting a second share alongside the first is refused,
since a box carries one share at a time, so that is not a way round this.

Authorization is re-read from disk on every connection, so a revoke from another
terminal takes effect on the next one. Connections already open are dropped
within about a second, by a watchdog that asks about *that peer's own grant*, so
revoking one person cuts their live connections while everyone else's keep
working. `share stop` revokes everything; the serving process then writes its
receipt and exits on its own, which is why it is not a `kill`.

`--force` is a different verb wearing the same name: it deletes the record and
asks nothing to stop. A process that really was serving notices within about a
second and exits, and in that second visitors are told the share has ended,
which is what happened. Take the message it prints literally. If it says the
record was written straight back, a process is still serving the box and access
is *not* cut off.

The longest a share may last is 24 hours, and the default is one.

### The two transports

Peer to peer (the default). QUIC between the two h5i processes, end-to-end
encrypted, hole-punched to a direct path when the networks allow it. When they
do not, a relay carries it: the relay moves sealed packets and sees both
addresses, the timing and the volume, never the content.

`--direct-only` refuses that fallback. A peer that cannot get a direct path is
turned away before any application byte crosses, and the share stays up for
anyone who can, which is the useful behaviour when one peer is behind a hostile
NAT and another is not. It keeps checking afterwards, because a hole-punched
path can die and the transport will slide onto a relay.

Be precise about that second half. Two things enforce it. A watchdog closes the
connection within a second of seeing a relay path, and, because a second of
traffic is not nothing, a check runs immediately before every write, so no byte
h5i has not already handed to QUIC is handed to it after the path changed. What
remains is what the transport had already accepted and may retransmit on the new
path, which nothing above QUIC can recall. Setup is a guarantee; staying direct
is a very short leash. A flag that merely preferred a direct path would be worse
than none, because it would let you believe nothing was in the middle when
something was.

Quick tunnel (`--tunnel`). A browser cannot speak QUIC to an endpoint id, so
peer to peer needs h5i on both ends. When the person you want clicking the
prototype is a designer or a customer, `--tunnel` shells out to `cloudflared`
and hands back a plain link. The costs, plainly:

- Cloudflare terminates TLS, so this path is not end to end. Cloudflare can read
  the traffic. That fact is written into the box's receipt, not only here.
- `cloudflared` is a binary we neither ship nor pin. If it is missing, h5i says
  so and names the alternative.
- Cloudflare quick tunnels are not a production service: they cap concurrency
  and do not carry server-sent events.

What does not change is everything under the transport. The link carries a
token, the token is checked against the same grant table on every connection,
revocation still works mid-session, and the credential is stripped before
anything reaches the box. The capability degrades from "hold the secret" to
"hold the link"; it does not degrade to nothing.

### What the shared app can and cannot see

The token travels in the URL on the first request only. h5i answers that request
with a redirect that moves it into an `HttpOnly` cookie and sends the browser to
the same page without it. The token therefore stays out of the address bar, out
of `Referer` on every outbound link, and out of the app's own logs. On the way
to the box, both the cookie and the query parameter are removed. The app being
shared never sees the credential that admitted its visitor.

The app's own cookies are passed through untouched, and so is its query string
with two exceptions: a parameter literally named `h5i` is taken as the share
token and removed, and empty pairs are dropped (`?a=1&&b=2` arrives as
`?a=1&b=2`). WebSockets pass through as well: hot reload works, because a share
of a dev server that never reloaded would not be a share of a dev server.

A share carries at most 64 connections into the box at once. Past that, a
visitor gets a `503` telling them to reload rather than a `401` telling them
their link is bad, and the count of refusals lands in the receipt. The ceiling
exists because a share is a door on the open internet in tunnel mode and an
endpoint anyone may dial in P2P mode. Without it, one peer, or one page opening
sockets in a loop, becomes unbounded connections into the box.

One connection carries exactly one request, and that is an authorization control
rather than a performance choice. A connection is checked when its first request
arrives, which is only the same as checking every request if it cannot carry a
second. By default it can. `cloudflared` keeps a pool of connections to the
origin and reuses them for whatever request comes next, from whatever visitor;
browsers pool per origin the same way, which puts the identical problem on the
joiner's proxy.

So h5i reads the request's head, then exactly as many body bytes as it declared,
and then stops forwarding anything else on that connection. `Connection: close`
goes to the box as well, but that is a courtesy: the box runs agent-written code
and may decline, so the control cannot depend on it.

On the way back, the response's own `Connection` header is replaced with
`close`. Otherwise a keep-alive answer would tell the visitor's browser to reuse
a connection that will never answer again. The response is then framed by its
`Content-Length`, so the connection ends when the response does.

A response that says two contradictory things about its length has both of them
taken off, leaving the visitor with one framing rather than two: the chunk
stream if a `Transfer-Encoding` was the other half of the contradiction, the
connection closing if it was a second length. A response the box starts and
never finishes becomes a `502`, because relaying an unfinished head verbatim
would let the box choose when to be sanitised.

Two consequences follow. A chunked request body is parsed rather than just
copied, because forwarding one request means knowing where it ends and a chunk
stream only says so in its own framing. It is still forwarded verbatim, chunk
headers and all, so the box sees the request it would have seen anyway. And an
upgrade is the exception to the one-request rule: it does become a two-way pipe,
but only after the box has actually answered `101`, and only when the request
asked for it properly with both an `Upgrade` header and a `Connection: upgrade`.
A request that merely attached an `Upgrade:` header gets no exception.

The cost is that every request is its own connection. For a dev server that
serves each module separately, a first page load is a few hundred of them, each
with its own dial into the box. It works and it is not free.

What the box learns about whoever visits. On a quick tunnel, Cloudflare adds the
visitor's public IP (`CF-Connecting-IP`, `X-Forwarded-For`), their country, and
a handful of `CF-*` request headers. h5i drops all of them at the gate: the code
being demonstrated is agent-written and the person who clicked the link is a
third party who agreed to look at a page. What does reach the box is what any
web server sees from a browser (`User-Agent`, `Accept`, the app's own cookies),
plus `Host` and `X-Forwarded-Proto`, which stay because dev servers build
absolute URLs out of them. So the box can tell it is behind a proxy; it cannot
tell who is on the other end.

Where that stripping happens differs by transport. On a tunnel the request
arrives at the sharer's own front, which is what rewrites it. Peer to peer, the
sharer is a raw pipe by design, since the stream carried its own ticket and
everything on it comes from the peer that ticket admitted; the rewriting
happened a moment earlier, in the joiner's gate on their machine.

Measured both ways: a visitor who forges `X-Forwarded-For` and
`CF-Connecting-IP` at their own joiner proxy has both dropped before the bytes
cross, while an ordinary custom header of theirs is passed through untouched.
What this does not defend against is a peer running modified software, who can
put anything on that stream. The only person they can identify that way is
themselves.

### What the person joining is taking on

The app is agent-written code, and joining runs it in their browser. That is the
point of sharing a port rather than a picture of one, and it is also the
exposure, the same one as clicking any link a colleague sends.

One asymmetry runs the other way from what you would guess. In peer-to-peer mode
the app is served from the joiner's own loopback, and browsers exempt loopback
origins from their private-network protections, so a hostile page has an easier
reach at that machine's local services than the same page on a public origin
would. Tunnel mode, ironically, keeps those protections, because the origin is
public.

The joiner's local proxy is gated for the same reason the viewer forward is: a
port on loopback is reachable by every process on that machine and every page in
that browser. Its URL carries a token minted on the joining side, which is not
the ticket secret. Nothing that authorizes the share is ever handed to a
browser.

While a share is open, `h5i ui` marks the box *shared now*, with the port, the
transport and how many tickets can still admit somebody. That indicator is live;
the receipt below is what lands when the share ends.

`h5i box rm`, `abort`, `apply` and `rebase` all refuse a box that is being
shared, and name the share and the command that ends it. `rebase` in particular
force-checks-out the worktree, which would change the files under the dev server
a visitor is looking at. `gc` skips such a box and reclaims the others. `rm
--force` removes it anyway, and once the removal is actually going ahead it says
so, at which point the share notices within a few seconds and ends itself.

A ticket is worth the same care as a password in one more respect: it is an
argument to `h5i join`, so any other user on the joining machine can read it out
of `ps` for the life of the grant.

### What lands in the receipt

Every other receipt lane observes what left a box. This one records what came
in, and it is host observed in the strongest sense available: h5i owns both ends
of the bridge, the box supplies none of it and cannot suppress it.

```
share session, 612s (p2p transport)
opened   2026-08-10T10:00:00+00:00
closed   2026-08-10T10:10:12+00:00
shared   port 3000 inside the box, never published on the host
endpoint kbcd7fq2m4xn8s6r3v9w1y5z7a2b4c6d8e0f2g4h6j8k0l2m4n
peers    1
  kbcd7fq2m4x… via direct — grant a1b2c3d4 (alex), 300s, 12 connections, 900 B in / 4.9 KiB out
refused  3 attempt(s): of the 3 recorded, 1 presented no invite, 1 an unknown ticket, 0 expired, 1 revoked
turned   2 connection(s) away before any ticket was weighed: 2 no direct path was available
```

Lines appear only when they have something to say. `turned` covers what is not a
credential question at all: `--direct-only` with no direct path, a peer that
connected and never presented a ticket, and requests the gate refuses to parse.
`capacity` and `flooded` say the share was hammered rather than probed, and they
are deliberately separate from `refused` because the two mean opposite things.
`route` says h5i could not reach the box. That is a different fact from
`unreached`, which means nothing was listening on the port, and blaming the
second for the first sends somebody to check a dev server that is running fine.

`clock` appears when this machine's two clocks disagree, and it says which kind
of disagreement it is. The session length is measured on a clock nothing can
move, so it is right either way; the timestamps beside it and each peer's held
time are wall-clock readings.

A *jump* is a discontinuity: an NTP correction, a VM resumed from a snapshot,
somebody running `date -s`. Timestamps after one can be minutes or hours out, or
out of order, and a jump backwards cannot extend a live ticket. That was
measured by putting an hour back onto every grant before the check existed.

*Drift* is the two clocks running at different rates, which is ordinary and is
not a fault: the machine this was developed on runs its wall clock about five
per cent slow, all day. That gets the weaker sentence, because treating it as a
jump would mean shortening every ticket on such a host by the same five per
cent: a one-hour ticket dying at fifty-seven minutes.

A receipt can also open with `partial`, which means it was written before every
connection had finished: the byte counts below it are short and a peer may read
as still connected. That happens when the quiesce times out, and when a Ctrl-C
during the shutdown skips the wait.

Read the numbers for what they are. "Connections" counts connections *into the
box*, not requests to the share: a visitor who followed the invite link and read
nothing is a peer with zero.

A share also gets a line for each thing it left out:

- attempts refused
- connections turned away because the box was already carrying its limit
- connections refused at the front door before a credential was asked for
- peers past the 256 the receipt lists individually
- authorized peers who found nothing listening
- responses the box left unfinished

A cap that stops counting silently makes a busy share read as a quiet one, and a
truncated download reads to the visitor as the app being broken rather than as a
share that gave up.

The two refusal-for-load lines are separate on purpose: the front-door one costs
an anonymous flooder a TCP connect and can be driven into the millions, and a
reader who saw that number under the box's ceiling would draw the wrong
conclusion from it.

A box that was opened to someone and an identical box that was not are different
artifacts, and an export should not be silent about which one it came from. A
tunnel session carries the "not end-to-end encrypted" note in the same block.

---

## h5i ui

```bash
h5i ui                  # http://127.0.0.1:8765/?token=…
h5i ui --port 0         # let the OS pick the port
h5i ui --open           # hand the URL to this desktop's browser too
```

One surface: the fleet, the same boxes the commands above report on, drawn as
one screen.

Left is every box with its tier, status and one signal. Right is the box you
picked: the policy that was actually enforced, the services it declares, its
diffstat against the pinned base, and a flight recorder with one row per receipt
across five lanes (FS, NET, PROC, RES, PAGE). Click a row for the rendered
receipt, the same text `h5i box inspect` prints.

The browser tab draws the page beside what it cost. For a box running h5i's own
engine, the rendered page sits directly beside the request log that produced it,
so "what did looking at this page cost, and what was refused while I looked" is
one glance rather than two panes and a correlation done by eye. That picture is
only honest because this engine *is* the HTTP client: the list is the decision
record written before the bytes moved, not an observation made from beside the
network.

And it draws the fence. Everything the page supplied (its URLs, its console
output, the subjects of policy verdicts, the rendered frame) is wrapped in the
same `--- BEGIN/END UNTRUSTED PAGE CONTENT ---` boundary the engine prints for a
model, with the same note. The engine fences that text before it reaches
something deciding what to do next; without this the console showed it to a
person with no boundary at all, which left the human reader with less framing
than the model got.

It cannot drive anything. Every route is a `GET`. `shell`, `run`, `export`,
`propose`, `apply` and `rm` stay in the CLI, where a human types them, so there
is no mutating surface to guard and no way to turn the console into a remote
control for someone's boxes.

What guards it. The server binds `127.0.0.1` and nothing else. The URL carries a
token minted for this session and kept in memory (never written to disk, so no
box can read it), which the page trades for a `SameSite=Strict` cookie on first
load. Requests from another origin are refused outright.

What the colours mean. Red is the only one that makes a claim about the
boundary: the egress allowlist *refused* a destination, host-observed by the
proxy. Amber is something to look at (a run exited non-zero, the wall-clock
limit killed one, or the in-box browser reported errors) and says nothing about
containment. Grey means the evidence is weak: either the tier is `workspace` and
nothing was confined, or every receipt came from the in-box shim and so is the
box's own account. Each run row is labelled `host-observed` or `box-claimed` for
the same reason. Nothing on the screen is a score.

The console is a default-on cargo feature. `cargo build --no-default-features`
drops it, along with axum, tokio and the build script's need for Node, and the
`h5i ui` command with it.

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

1. Reads the machine's SSH *host key* and pins it. That key is the runner's
   identity: `runner_id` is its SHA-256, and a box records the id, never the
   name. Renaming a runner, or pointing the name at other hardware, therefore
   cannot move a box onto a machine it was not built for.
2. Generates a keypair used for this runner and nothing else, owner-only, under
   `~/.config/h5i/runners/<name>/`.
3. Installs one line in the runner's `authorized_keys`:

   ```
   restrict,command="/usr/local/bin/h5i runner serve-stdio" ssh-ed25519 AAAA…
   ```

   `restrict` is the whole security argument in one word: with it that key
   cannot open a shell, forward a port, forward your agent, or allocate a
   terminal. It can run that one command and nothing else.
4. Connects over the new key and probes, so that pairing either works end to end
   or leaves nothing behind.

Nothing listens on the runner. There is no daemon, no port, no token and no TLS:
the worker is a process per request, started by sshd and gone when the request
ends.

Pairing trusts the host key it sees the first time, exactly like your first
`ssh` to a new host. To close that window, read the real fingerprint on the
machine itself and pass it:

```bash
# on the runner
ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub
# here
h5i runner pair pi5 h5i@pi.local --fingerprint SHA256:…
```

`--print-only` prints the `authorized_keys` line instead of installing it, for a
machine where keys are added another way.

### Capabilities are advertised, never assumed

A runner needs Linux, sshd and `h5i`. It does not need a container runtime.
Everything past those three is *advertised* by `h5i runner probe`:

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

A box asking for something a runner does not advertise is refused, with the
missing capability named. It is never quietly given something weaker: the same
rule `--isolation` already follows here.

The isolation list is what the runner's kernel demonstrably ran a moment ago,
not which kernel features are present. The two are not the same thing, and only
the first is worth advertising.

Two entries change what you can do next, so they are called out rather than left
as a `no` in a table:

- boxes persist: no. Box state does not survive a reboot (a read-only OS, a
  tmpfs workspace). A reboot is an expired lease: anything not exported is gone.
- own egress: no. The runner has no default route, so a box on it cannot pull
  images or install packages. Egress brokered through this machine is a later
  milestone.

### Putting a box on one

```bash
h5i box create fix-auth --runner pi5
```

The base commit is pinned here, the branch is created here, and the policy is
resolved and digested here. What crosses is the source, as a git bundle, and
what comes back is the digest of the policy the runner actually enforced. The
box is refused if that does not match what was sent.

```
$ h5i box ls
env/human/fix-auth   created   isolation=container  base=fa31b1f97547 captures=0 on=pi5
```

The manifest records the runner's *host-key hash*, not its name. Renaming a
runner, or pointing a name at different hardware, therefore cannot move a box
onto a machine it was not built for: `h5i box rm` checks the identity before it
removes anything there.

`h5i box rm` clears both sides. It removes this side first: `rm` refuses a box
that is still live, and clearing the runner before that check would destroy the
box there while telling you the removal had failed. If the runner is unreachable
when its turn comes, the box is left there and its lease reaps it.

### Working in one

```bash
h5i box run fix-auth -- cargo test      # runs on the runner
h5i box propose fix-auth                # bring the work home
h5i box diff fix-auth                   # review it here
h5i box apply fix-auth                  # land it
h5i box export fix-auth                 # or take the patch and receipts
```

`box run` executes on the runner under the policy pinned at create, and the
receipt comes home with the exit code, the timings and the runner's own egress
summary. It is filed under a lane of its own, *`runner-observed`*: h5i saw it
from outside the box, so the box could not have forged it, but *this* machine
did not watch it either. It is not counted as host-observed and not counted as
box-claimed, because it is neither.

`box propose` is where the work returns, and it is the careful part. The runner
commits what the box has and sends a bundle of just the new work. That bundle is
unpacked into a throwaway repository with its own object database (not a branch,
not a ref namespace, a separate repository) and inspected there: size and count
ceilings, path traversal, nested git repositories, submodule pointers the base
did not have. Only a tree that passes crosses into your repository, and h5i
writes the commit itself. The runner's history and authorship never enter your
history at all.

If something is refused, nothing lands:

```
$ h5i box propose fix-auth
Error: mediated commit refused (fail-closed) — 1 path violation(s):
  - a submodule pointer the base did not have, at vendor/thing
```

After a successful propose, `diff`, `apply` and `export` behave exactly as they
do for a local box. There is nothing special about applying work that came from
a runner, which is the point.

### What is not built yet

- `box shell` on a runner. Interactive means a pty, which means bidirectional
  streaming and resize; that is the next piece of work.
- Streaming output. `box run` returns everything when the command finishes, so a
  long build is silent until it ends. The exit code, timings and evidence are
  all correct; you just do not see the log as it happens.
- Agents on a runner. An agent profile needs model credentials, and h5i will not
  send those to another machine. A credential channel that keeps them here is a
  later milestone, and until then a runner box runs builds, tests and commands
  rather than Claude or Codex.
- `clone:` and `--new` sources. Those build their repository inside the box;
  sending one across belongs with a later milestone.

The design, including what is deliberately deferred and why, is ROADMAP.md
sections R1 to R13.

### Unpairing

`h5i runner unpair <name>` removes the record, the key and the pin from this
machine. It does not touch the runner: the `authorized_keys` line stays until
you delete it, and the command says so, with the comment to search for.

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

A note on the one grant nobody would think to write. The built-in read set
carries the handful of paths `/etc/resolv.conf` is a symlink *to*
(`/mnt/wsl/resolv.conf` on WSL, the systemd-resolved and resolvconf locations
under `/run`), one file each. `/etc` alone is not enough, because Landlock
follows the link to a path the box was never granted, and what that costs does
not look like a denied file: `getaddrinfo` answers "Temporary failure in name
resolution" and a `net.mode = "host"` box reads as a machine with no network.
The entries are the same on every host whether the files exist or not, because a
grant resolved from the local `/etc` would give one profile a different policy
digest on every machine. A custom profile that sets `fs.read` replaces that
list, so a box of your own with `mode = "host"` needs the line for your host,
which `readlink -f /etc/resolv.conf` names.

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

Everything above this line is about what a box is *allowed* to do. `[detect]` is
about what it actually did, reported from a place the box cannot reach: an eBPF
collector attached to syscall tracepoints for the length of the run.

```toml
[profile.review.detect]
enabled   = true      # attach the probe for runs under this profile
require   = false     # refuse to run when the probe cannot attach
buffer_kb = 256       # ring buffer size
rules     = ["*"]     # rule ids, family names, or "*"
```

It is *observation only*. Nothing in this lane can deny a syscall; denial is
Landlock, seccomp, the network namespace and the egress proxy, and it stays
there. What the collector adds is a second opinion on the same run from an
observer that is neither at the boundary of the box nor inside it: the kernel
reports `execve`, `connect` and `openat` whether or not anything in the box
wanted them reported.

`enabled` is `false` by default because the collector needs `CAP_BPF`, which an
ordinary install does not have. `h5i box detect probe` says whether this machine
can watch a box and prints the one command that would change that; h5i never
runs it for you.

`require = true` is the fail-closed switch: if the probe cannot attach, the run
is refused rather than performed unwatched. That is the setting for "I am
running somebody else's dependency tree". It is off by default because a
mandatory detector on a laptop kernel is a tool that does not start.

Coverage differs by tier, and every receipt says which it got:

| Tier | Coverage | Why |
|---|---|---|
| `workspace`, `process`, `supervised` | full | the payload is a descendant of h5i, so the kernel-maintained process tree holds it and everything it spawns |
| `container` | partial | the container runtime double-forks, so the workload leaves h5i's process tree; what stays visible is the runtime's own activity on the host |
| `microvm` | none | the workload runs against a guest kernel, which a host probe cannot observe at all |

`h5i box detect rules` prints the whole signature catalogue with what each rule
is for. `h5i box detect show <box>` prints what fired, worst first.

This is opt-in at three separate layers, and all three have to say yes:

| Layer | Switch | Default |
|---|---|---|
| build | `cargo install --path . --features bpf` | off: a stock build and the released binaries carry no probe |
| host | `CAP_BPF` and `CAP_PERFMON` on the h5i binary | not granted |
| policy | `[profile.X.detect] enabled = true` | false |

`h5i box detect probe` reports all three and prints the command for whichever
one is missing.

Other requirements: Linux 5.8 or newer (`BPF_MAP_TYPE_RINGBUF`), and a `clang`
that can target BPF at build time. No BTF, no `vmlinux.h`, and no kernel
headers: the probe reads no kernel structure, only syscall tracepoint arguments,
which are stable ABI.

Reading a receipt needs none of it. The evidence types ship in every build, so a
colleague with a stock h5i can read an export from a machine that had the
collector.

`wall` is enforced everywhere. `mem` and `procs` are not enforced at the
`process` and `supervised` tiers on macOS: Darwin has no cgroups, does not
enforce `RLIMIT_AS` against the mmap'd heap every modern runtime uses, and
scopes `RLIMIT_NPROC` to the whole user rather than to one box, so h5i declines
to apply a limit it cannot hold. `h5i box status` marks such a value with `*`
and says so underneath, rather than listing it as enforced. Use `isolation =
"container"` or `isolation = "microvm"` where you need a real ceiling: both cap
memory and process count in the runtime itself.

### Isolation tiers

| Tier | What it is | Network scoping |
|---|---|---|
| `workspace` | No confinement; just a separate worktree. | none |
| `process` | Landlock + seccomp + namespaces, with a supervisor and a private pid namespace. | deny or host |
| `supervised` | Everything `process` has, including the private pid namespace, plus a private netns with an nftables egress allowlist pinned to resolved IPs, DNS pinned by a hosts file, and a seccomp-notify gate on `socket()`. | L3/L4 |
| `container` | Rootless Podman: a portable image, with a CONNECT-proxy egress allowlist. | L7 |
| `microvm` | A hardware-isolated guest with its own kernel, booted by [microsandbox](https://microsandbox.dev) (`msb`) from the same OCI images. Egress rules are evaluated by the VM's network stack. | L3/L4 |

`auto` (the default) picks the strongest tier the host can actually run. An
explicit tier *fails closed* if the host cannot satisfy it: h5i never silently
downgrades.

Worth being clear about, because two drafts of the design got it backwards: the
container tier buys *portability*, not tighter network control. Its allowlist is
a proxy, so it binds proxy-respecting tooling only. `supervised` enforces at
L3/L4 and does not have that hole.

#### The microvm tier

`microvm` is the one tier where the boundary is a virtual machine rather than a
policy applied to a host process. A kernel exploit inside the box meets the
hypervisor, not the host kernel it just subverted. Its `net.egress` allowlist
becomes default-deny plus one address rule per allowed destination, so a raw
socket to an unlisted IP is dropped rather than merely un-proxied.

Requirements, all three, or the tier refuses:

- microsandbox's `msb` on `PATH`, version 0.6 or newer.
- Host virtualization: `/dev/kvm` openable on Linux, Apple Silicon on macOS. A
  stock WSL2 kernel and most cloud CI runners have neither.
- A base image, from `--image`, the profile's `container.image`, or the
  repo-level `[container] image`, the same images the container tier runs,
  pre-pulled with `msb pull`.

`h5i box probe` reports which of the three is missing, since "install a package"
and "enable nested virtualization" are different problems.

Two things it does *not* do yet, stated rather than left to be discovered:

- No per-request egress tally. The container tier's proxy sees every CONNECT and
  records allow/deny counts in the capture manifest. A netstack filter drops
  packets without reporting them, so a microvm receipt carries no egress
  summary. Stronger enforcement, thinner evidence.
- No authenticated-egress grants. `[[profile.X.auth]]` hands the box a base URL
  pointing at a credential proxy on the *host's* loopback, which a microVM guest
  cannot dial. A profile that declares grants is refused at this tier rather
  than handed an origin that resolves to nothing.

In-box observation works as it does under `container`: the read-only
managed-settings mount carrying the `wrap-bash` hook, and the capture spool at
`/.h5i/spool`. The container tier's tee shim has no analogue here (it depends on
self-mounting the image, which a VM has nothing to do).

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

Being explicit about these is a feature, since the claim is a security claim.

- The agent can send your source to the model API. Containment stops the agent
  from touching the host. It does not stop it from putting private code in a
  prompt. That is a separate control (a self-hosted model, or no model egress at
  all) and we will not imply otherwise.
- Shared kernel at four of the five tiers. `workspace`, `process`, `supervised`
  and `container` all share the host kernel. Good against a runaway agent and
  against careless dependency code. Not a claim against a targeted kernel
  exploit. `isolation=microvm` is the tier where that is not true: the boundary
  is a hypervisor, so an exploit inside the box meets it rather than the kernel
  it just subverted. It buys that for thinner evidence, since a netstack filter
  drops packets without reporting them and a microvm receipt therefore carries
  no egress summary. It needs `msb`, host virtualization and a base image, and
  refuses rather than downgrades when any of the three is missing. See [The
  microvm tier](#the-microvm-tier).
- The container tier's egress scoping is L7. Its allowlist is a proxy, so it
  binds proxy-respecting tooling only.
- An interactive session at a kernel tier shares your terminal. `box shell`
  hands the box the terminal you launched it from, because that is what makes
  job control and every TUI work. A box shell is a nested shell, not a
  connection to somewhere else. A terminal is a two-way device, and the box gets
  both directions of it, so the residual is a list rather than a single door:
    - Typing at your shell. `TIOCSTI` pushes characters into the terminal's
      *input* queue, which your shell reads as if you had typed them, after the
      session ends. Whether that is closed is not h5i's to assert, and the two
      platforms answer from different places.

      On macOS the Seatbelt profile subtracts the ioctl, so it holds at
      `process` and `supervised`. It does *not* hold at `isolation=workspace`,
      which applies no profile by design, nor on a host whose Seatbelt is
      unusable.

      On Linux it is your kernel's setting, the same at every tier, since
      h5i does no ioctl filtering of its own there. Kernel 6.2 made TIOCSTI
      disableable via `CONFIG_LEGACY_TIOCSTI` and `dev.tty.legacy_tiocsti`, but
      upstream defaults that *open*. Many distros ship it closed, and a kernel
      older than 6.2 cannot close it at all.

      So h5i measures instead of claiming. `h5i box probe` prints one of:

              tty-injection= blocked at every tier
              tty-injection= blocked at the kernel tiers, possible at isolation=workspace
              tty-injection= possible at every tier

      and, when anything is open, how to close it.

    - Reading what you type next. The session's read grant on the terminal
      is not revoked when the shell exits, so a box process that outlives the
      session (a stray background job) can read the terminal it still holds
      open. This predates the tty ioctl grant; it is a property of sharing the
      device.
    - Leaving the terminal in a state. A box can set raw mode, turn echo
      off, change the line discipline, or take the terminal exclusive so other
      programs cannot open it. Recoverable (`stty sane`, or a new terminal), not
      an escape, but it is yours to recover.

    What is *not* reachable, checked rather than assumed: `TIOCCONS`
    (redirecting console output to the box's terminal) is refused by Darwin for
    a non-root process with or without a sandbox. Giving the box its own pty and
    proxying it is the fix that ends the whole list, and it is not built. The
    container and microVM tiers do not share a terminal at all.

- Chrome runs with its own sandbox off. On Linux, h5i's seccomp deny-list blocks
  the namespace syscalls Chrome's sandbox needs, at every tier. h5i's box is the
  boundary; Chrome's is not available inside it. That is one layer fewer than a
  browser on the host has.
- A dev server the box runs is reachable only if its port is declared. On macOS
  the box shares the *host's* loopback, so h5i denies outbound to it wholesale.
  Otherwise a box could reach a database or a dev server belonging to the host.
  A box that runs its own dev server and wants to point its own browser at it
  names the port: `[profile.X.net] loopback = [3000]`. Exactly that port is
  granted; everything else on loopback stays denied, and an undeclared port
  fails with `net::ERR_ACCESS_DENIED`. The port a declared `[service]` is
  running on is granted automatically while that service is alive, so this is
  only needed for a server started by hand. On Linux the box has its own network
  namespace and none of this applies.
- On macOS the browser has no in-process domain check. agent-browser cannot
  start Chrome from inside a Seatbelt sandbox. The failure reproduces under a
  fully permissive `sandbox-exec` profile and disappears without the sandbox, so
  it is not something a grant fixes. h5i therefore launches Chrome itself and
  attaches agent-browser to it with `--cdp`, which upstream refuses to combine
  with `--allowed-domains`. So that flag is not set for a macOS browser box: the
  tier's own egress enforcement is unchanged and still the boundary, but
  agent-browser's second, in-process domain list is gone. A page on a
  non-allowlisted host fails inside Chrome with `net::ERR_ACCESS_DENIED`.
- A browser box's Chrome is restarted when its route out changes. Chrome
  outlives the run that started it, and it takes its proxy address once, at
  launch, so a browser started before the box's current route (an upgrade, or a
  run whose proxy port moved) cannot reach the network through it. The box
  cannot restart it itself: a browser from a previous run is in a previous
  sandbox instance, which Seatbelt's same-sandbox signal grant does not reach.
  It is detected in the box and stopped host-side at the start of the next run,
  so the fix costs one extra run and says so rather than failing with a proxy
  error that reads like a page problem. The relaunch starts from a clean profile
  directory, so anything the old browser held (cookies, logins) is gone.
- Two kernel mechanisms, not one. Linux confines with Landlock, seccomp and
  namespaces. macOS confines with Seatbelt, which is default deny across
  filesystem, network, mach and sysctl in one policy, and which (unlike
  Landlock) can subtract a child from a granted parent, so the agent config lock
  is one rule there instead of a bind mount. That does not make `fs.deny`
  stronger on macOS: a denied path inside a granted parent is refused as a
  policy on every platform, so what is left is already outside every grant.

    Two things are genuinely absent on macOS. There is no syscall filter,
    because Darwin has no seccomp equivalent. And there is no memory or
    process-count cap, because Darwin has no cgroups, does not enforce
    `RLIMIT_AS` against an mmap'd heap, and scopes `RLIMIT_NPROC` to the whole
    user rather than to one box, so applying it would cap your machine and not
    the box. `h5i box probe` names the mechanism and the gaps.

    Rootless Podman runs on Linux and WSL2 natively, and on macOS through a
    `podman machine` VM.

- A macOS box shares the host's loopback. A Linux box gets its own network
  namespace, so its loopback is private. macOS has no namespaces, so a box binds
  the host's loopback (deliberately: it is the only way a dev server in a box is
  reachable). h5i closes the outbound half of this, denying the box every
  outbound loopback destination except its own egress proxy, but the box's own
  listening ports are reachable by any local process. `h5i box share` works on
  macOS by identifying which process holds the port rather than by owning the
  route to it. So it can promise that what it publishes is the box's server. It
  cannot make the port private to the box, and does not claim to.
- Cost. A Chrome sidecar is real RAM and CPU, even headless. Headless boxes stay
  first class, and the browser is opt-in per box.
- The viewport is not a desktop. CDP screencast shows the page. Native dialogs,
  browser chrome and anything outside the tab are invisible.
- A dependency on the critical path. `agent-browser` is someone else's release
  cadence. Pinned, CLI-boundary, forkable, but not ours.

---

## Files

| Path | What it is |
|---|---|
| `.h5i/env.toml` | Checked-in policy: profiles, services, container image. |
| `.git/.h5i/env/<agent>/<slug>/` | One box: its manifest, resolved policy, receipts, workspace. |
| `.git/.h5i/cache/<eco>/<key>/` | Warm dependency caches. |
| `.git/.h5i/env/<agent>/<slug>/spool/` | The box's one writable window: staged posts and capture records. |
| `~/.config/h5i/` | Host-side egress allowlist. Outside every box-granted path. |
| `~/.config/h5i/runners/<name>/` | One paired runner: its record, its dedicated key, its pinned host key. Owner-only, and outside every box-granted path for the same reason the allowlist is. |

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
- [`skills/h5i/`](skills/h5i/): the agent-facing skill (`h5i skill show`)
- [`ROADMAP.md`](ROADMAP.md): what is built and what is not
- [`SECURITY.md`](SECURITY.md): reporting a vulnerability
