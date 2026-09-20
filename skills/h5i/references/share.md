# Sharing a box's dev server

`h5i box share` lets one other person, on their own machine, try the web app in
a box while it runs. It is the only path that carries traffic into a box. Run it
when the user asks, never as a step you add to a plan yourself.

## Ask before you share

Sharing exposes agent-written code to another human, and under `--tunnel` to a
third party's network. The risk is somebody else's, so the decision is theirs.
Say so when a share would help, such as feedback on a prototype or a colleague
clicking through a flow, and let them choose. To check your own work use
`agent-browser` or `h5i box view`.

## The two modes

```bash
h5i box share <name> --port 3000           # peer to peer; they run `h5i join`
h5i box share <name> --port 3000 --tunnel  # a plain URL; any browser, no h5i
```

Peer to peer is the default and end-to-end encrypted. `--tunnel` shells out to
`cloudflared` to reach someone with no h5i installed, and Cloudflare terminates
TLS, so it can read the traffic. Pick it only when the other side cannot install
anything, and say that trade out loud first.

Both print an invite. Hand it over exactly as printed; it is printed once and
cannot be reproduced.

## Managing one

```bash
h5i box share status <name>          # the endpoint and every grant
h5i box share ls                     # this clone's share records
h5i box share ls --json              # the same, with `name` and `live` per row
h5i box share grant <name> --label sam   # a second ticket (--tunnel shares only)
h5i box share revoke <name> <grant>  # cut off one peer
h5i box share stop <name>            # end it
h5i box share stop <name> --force    # delete the record, whatever it says
```

`grant` takes `--expire`, so a second peer can get a shorter ticket than the
first. It works on `--tunnel` shares only: a P2P ticket needs the running
endpoint's addressing, which lives in the serving process, so `grant` refuses
there instead of handing out a ticket that reaches nothing.

To add a second peer to a P2P share, stop it and start a new one, then re-issue
tickets to everybody, including the person already connected, whose ticket the
restart invalidates. A box carries one share at a time, so a second start
alongside the first is refused.

The share runs in the foreground until Ctrl-C, so start it where the human can
see it, not in a background job they cannot find later. It carries at most 64
connections into the box at once; past that a visitor gets a `503` asking them
to reload, and the count lands in the receipt.

It also ends on its own, writing its receipt on the way out: when the last
ticket expires, when the box stops having a running session, or when
`cloudflared` exits under `--tunnel`. So cleanup is usually automatic, and a
share started an hour ago may already be gone. Check `share status` before
pointing anyone at it.

Ctrl-C during that ending skips the waiting, so connections still mid-copy lose
their closing byte counts, and it still writes the receipt. Only a second Ctrl-C
exits without one.

## What it needs, and what it refuses

- Linux or macOS, for two different reasons. On Linux the box has a network
  namespace this machine can enter, and the only route in goes there. macOS has
  no namespaces, so a box binds the host's loopback; h5i asks Darwin which
  process holds the listening socket, shares it only if that process belongs to
  the box (its session and descendants), and otherwise refuses and names the
  holder. It re-checks on every connection. macOS boxes at the `container` or
  `microvm` tier run in a VM where no host process holds the port, so they are
  refused.
- The box has to be running (a live `h5i box shell` or `h5i box run`) and have a
  network of its own with a loopback in it. The profile decides that second
  half, not the tier: a profile that denies egress gets an empty namespace with
  no `lo` up at every tier, so `default`-profile boxes cannot be shared and
  `share` says so. Use an agent profile (`agent`, `agent-claude`, `agent-codex`),
  whose egress allowlist brings a loopback with it. Otherwise "the box's port
  3000" is the host's port 3000, and `share` refuses instead of publishing
  whatever is listening. The error says which of the two is missing.
- Something should be listening on the port, whatever the dev server inside the
  box binds. (`h5i box ports` lists *declared services*, not everything
  listening, so it will usually not answer this question.) Sharing a port with
  nothing behind it warns instead of failing, because a dev server that is about
  to start is a reasonable thing to share.
- Tickets expire: one hour by default, 24 hours at most.

While a share is running, the box is held: `h5i box rm`, `abort`, `apply` and
`rebase` all refuse it and say which share to stop first, and `gc` leaves it
alone. Stop the share and they work again.

## What lands in the receipt

A share writes its own lane into the box's receipt: who connected, over what
path (direct, relayed, or tunnel), for how long, how much moved, who was refused
and why, and who was turned away before a ticket was weighed at all. It is
written when the share ends, so an export taken during a share does not carry it
yet.

Name the share session when you summarise a box's history. It records that the
box was opened to someone.
