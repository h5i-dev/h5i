# The browser in the box

A `browser` box is an agent box with a real headless Chrome and the
`agent-browser` daemon in it. The agent, its builds and tests, the dev server on
`localhost:3000`, Chrome, and the daemon are all inside **one** box, so the app
under test is reachable at loopback with no port publishing and no second
container.

```bash
h5i box --profile browser --name ui
h5i box shell ui
```

## Driving it

h5i does not reimplement clicking. `agent-browser` is the automation, and its
own `--help` is the full verb table. The shape that matters for an agent:

```bash
agent-browser open http://localhost:3000
agent-browser snapshot                  # accessibility tree with @refs
agent-browser click @e2
agent-browser fill @e3 "test@example.com"
agent-browser screenshot shot.png
agent-browser console                   # what the page logged
agent-browser errors                    # uncaught exceptions
agent-browser network requests          # what it fetched, and what failed
```

Read the **snapshot**, not the HTML. It is an accessibility tree with `@e2`-style
handles, which is both far cheaper in tokens and far more stable than selectors.

Handles come from a snapshot and go stale when the page moves. If a click lands
somewhere unexpected, re-snapshot rather than retrying the same handle.

## What the box does to the browser, and why

- **Fresh profile, created in the box.** No host cookie jar, no host extension,
  no host history. Nothing you are logged into on the host is logged in here.
- **Chrome's egress is the box's egress.** At `supervised` that is an nftables
  allowlist pinned to resolved IPs, which needs no cooperation from Chrome.
  Loopback is always open, because the dev server is the whole point.
  `--allowed-domains` is set from the same policy as a second, in-process layer.
- **Chrome's own sandbox is off.** h5i's seccomp policy denies the namespace
  syscalls it needs, at every tier, so Chrome runs `--no-sandbox`. The box is the
  boundary, not Chrome. This is a real reduction in defence in depth and it is
  stated rather than hidden.
- **AI chat is refused.** `agent-browser chat` and the dashboard's AI panel send
  page content to an external gateway, which inside a box is an exfiltration path
  with a friendly name. The gateway credential is never injected, and its absence
  is the whole mechanism.
- **Downloads land in the box.** They resolve under the workspace and go through
  the export gate like any other file.

## The control lock

Two clients can drive one browser — you, and a human at the viewer. Nothing
upstream arbitrates between them, so h5i does.

```bash
h5i browser status <box>     # who holds control, and whether your @refs are stale
h5i browser take <box>       # a human takes control
h5i browser release <box>    # hands it back
```

You hold control by default. A human **takes** it rather than asking, and when
they do:

- Your mutating verbs are refused with a typed message, not left to fight for
  the pointer. **Wait — do not retry in a loop.**
- Read-only verbs (`snapshot`, `screenshot`, `console`) keep working. Watching
  never collides.
- When control comes back, your handles are stale because the page moved under
  you. Run `agent-browser snapshot` before acting. Acting first is refused rather
  than mis-clicked.

## The viewer

A human can watch the box's browser, and take over inside it:

```bash
h5i box view <box>           # serves the viewer on loopback, prints the URL
h5i browser url <box>        # the URL again, without starting a forward
```

The box has to be running (a live `h5i box shell` or `h5i box run` session), and
its browser has to be streaming — inside the box, `agent-browser stream enable`.

What the forward is, since it is a security boundary rather than a convenience:
the box's stream port is never published. It stays in the box's private network
namespace, and h5i enters that namespace to reach it. Every connection carries a
per-box token minted at creation and never written anywhere the box can read,
cross-origin handshakes are refused, and input reaches the page only while the
human holds the control lock.

## What lands in the receipt

Every run that drove the browser carries the page's own answer: console errors,
uncaught exceptions, and failed requests, collected by h5i right after the
command rather than reported by you. Each record carries only what is new since
the last one.

```bash
h5i box inspect <box> --capture <id>    # includes a `browser :` line
```

This is why "I clicked Submit and it worked" is not worth writing in a report:
the export already carries what the page actually did, under **What the browser
saw**, and a reviewer reads it next to your account. If the page threw an
exception while you were verifying a fix, say so — it is already in the bundle.

Viewer sessions are recorded too, in their own lane, including whether a human
took the controls during one.

## When the browser will not start

`agent-browser doctor`, **run inside the box**, is the tool for this. It reports
what Chrome it found, whether chat is disabled, and where its socket lives.

The daemon detaches and sends its own stderr to `/dev/null`, so a failure
normally surfaces as "exited during startup with no error output". Set
`AGENT_BROWSER_DEBUG=1` and it writes to `$AGENT_BROWSER_SOCKET_DIR/<session>.log`
instead — that log is the only place the real error appears.
