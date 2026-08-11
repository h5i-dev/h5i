# Sharing a box's dev server

`h5i box share` lets one other person, on their own machine, try the web app in
a box while it is still running. It is the only path in h5i that lets traffic
*into* a box, so treat it as something you do when asked, and never as a step
you add to a plan on your own.

## Ask before you share

Sharing exposes agent-written code to another human, and in one mode to a third
party's network. That is a decision about somebody else's risk, so it belongs to
the person you are working for. If sharing would help — they want feedback on a
prototype, or a colleague should click through a flow — say so and let them
decide. Do not start a share to check your own work: you have `agent-browser`
and `h5i box view` for that.

## The two modes

```bash
h5i box share <name> --port 3000           # peer to peer; they run `h5i join`
h5i box share <name> --port 3000 --tunnel  # a plain URL; any browser, no h5i
```

Peer to peer is the default and is end-to-end encrypted. `--tunnel` shells out
to `cloudflared` and reaches someone with no h5i installed, at a real cost:
**Cloudflare terminates TLS and can read the traffic.** Never pick `--tunnel`
silently — it is the right answer only when the other side cannot install
anything, and the person you are working for should know that is the trade.

Both print an invite. Hand it over exactly as printed; it is printed once and
cannot be reproduced.

## Managing one

```bash
h5i box share status <name>          # the endpoint and every grant
h5i box share ls                     # what is shared on this clone
h5i box share grant <name> --label sam   # a second ticket (--tunnel shares only)
h5i box share revoke <name> <grant>  # cut off one peer
h5i box share stop <name>            # end it
```

`grant` works on a `--tunnel` share only. A peer-to-peer ticket needs the
running endpoint's addressing, which lives in the serving process, so adding a
second peer to a P2P share means starting a second share. The command says so
rather than handing out a ticket that reaches nothing.

The share runs in the foreground until Ctrl-C, so start it where the human can
see it rather than in a background job they cannot find later. A share carries
at most 64 connections into the box at once; past that a visitor gets a `503`
asking them to reload, and the count lands in the receipt.

It also ends on its own, and in each case it writes its receipt on the way out:
when the last ticket expires, when the box stops having a running session, and —
on `--tunnel` — if `cloudflared` exits. So a share is never something you have
to remember to clean up, but it is also not something you can start and assume
is still up an hour later.

## What it needs, and what it refuses

- The box must be **running** (a live `h5i box shell` or `h5i box run`) and have
  a network of its own — `supervised` or `container`, or `process` with a
  profile that denies egress. Otherwise "the box's port 3000" is the host's port
  3000, and `share` refuses rather than publishing whatever is listening. The
  error says which of the two is missing.
- Something should be listening on the port — whatever the dev server inside
  the box binds. (`h5i box ports` lists *declared services*, not everything
  listening, so it will usually not answer this question.) Sharing a port with
  nothing behind it warns rather than fails, because a dev server that is about
  to start is a reasonable thing to share.
- Tickets expire — one hour by default, 24 hours at most.

## What lands in the receipt

A share writes its own lane into the box's receipt: who connected, over what
path (direct, relayed, or tunnel), for how long, how much moved, and who was
turned away. It is written when the share ends, so an export taken during a
share will not have it yet.

If you are summarising a box's history, a share session is worth mentioning by
name. A box that was opened to someone is not the same artifact as one that was
not.
