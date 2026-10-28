# Design: the in-terminal browser

The viewer is the half of a box browser that a human looks at and drives. Two
viewers share one keymap: the terminal viewer (`h5i box view`, code in
`crates/h5i-core/src/termview`) and the loopback viewer served to a browser
(`h5i box view --web`, `crates/h5i-core/src/viewer.html`). The keys are listed
in [`MANUAL.md`](../MANUAL.md#h5i-box-view). Host browser sessions have no
viewer; `h5i browser view` went with h5i's own engine.

Several keys are bound only when the stream advertises the matching feature.
A Chromium screencast advertises none of them, so with today's engines the
hint and history keys stay unbound. The viewer side is built and tested; the
engine side that served it was removed.

## V1. Keys, not a pointer

A terminal pointer is a poor instrument:

- Mouse reporting (`?1003h`, SGR) gives cells, not pixels. `?1016` gives
  pixels where supported, and a terminal without it keeps reporting cells with
  no way to tell.
- The viewer hides the cursor, so the only aiming feedback is the page's hover
  state a frame later.
- The legacy key encoding has no key-up, so a held key, and therefore a drag,
  cannot be expressed.

Fixing that would take OS-level input outside the terminal, which conflicts
with every escape sequence being generated host-side. So the viewer is driven
by naming a target and pressing a key. `i` hands the page the keyboard and
pointer for what keys cannot express (V3).

## V2. Hints are the snapshot's refs

A hint label is a label stuck to a `snapshot` ref, with geometry attached by
the engine. The overlay therefore cannot offer a target the verb layer would
refuse, and pressing a label dispatches `click @eN`, so a human's action leaves
the same receipt shape as an agent's: a role and an accessible name, not a
coordinate.

Labels are minted by the engine, not by each viewer, so two viewers on one
page agree. Matching what has been typed is per-viewer state. Labels are
prefix-free, so a viewer acts the moment a label is complete (`vim::narrow`).
`F` and `gi` narrow the overlay to fields.

## V3. Capability gating, and the one key that fails open

Which keys work is read from the `features` list the stream sends with
`status`, never inferred from the engine's name (`vim::Features`,
`vim::resolve`).

- Movement is not gated: scrolling goes out as wheel and arrow events every
  engine understands.
- Hints, history, reload and typing are gated. An unavailable key says so
  rather than doing nothing.
- `i` fails open, because handing the page the keyboard predates feature
  lists and is the only way to drive a canvas or a drag on an engine that
  sends none:

| what the stream sent | `i` |
|---|---|
| no feature list | bound |
| a list including `pointer` | bound |
| a list without `pointer` | refused, pointing at `f` |

## V4. Typing is real key events

The viewer sends key events, not whole field values, so the page sees
`keydown` before the edit and `input` after, the caret moves, and `Backspace`
and `Tab` behave as typed. The rules the receiving engine must keep:

- An unmapped key still delivers its DOM events, so a page's own shortcuts
  work while a field has focus.
- Modified keys are commands (`Ctrl-S` does not type `s`). Shift is the
  exception; the viewer reports `text` because only it knows the keyboard
  layout.
- Focusing a field does not clear it.

## V5. Latency, by not doing work

A keystroke cannot be answered locally: nothing appears until the page is laid
out again and a frame encoded. Three rules keep that cheap:

1. Batch the keys. At most one input message is on the wire; keys struck
   while it is away ride out together in the next. Batched, never coalesced: a
   keystroke is a delta, and dropping one loses a character. A lost batch is
   abandoned rather than retried, since a repeat could type the word twice.
   (Measured against the removed engine: 25 characters went from 869 ms
   behind to 88 ms.)
2. Render only for a viewer that can take a frame under ack pacing, instead of
   rendering one to throw away.
3. Send only the pixels that changed (`termview/image.rs`, `damage`). The
   viewer diffs the new frame against what is on screen, rounds the changed box
   out to cell boundaries, and places that rectangle. One typed word fell from
   1016 KB to 28 to 85 KB.

The patching rules:

- Cell alignment only grows the box; rounding inward would leave stale pixels
  at the edge.
- A patch is placed over what is beneath it and is never deleted on its own. A
  whole frame releases all patches, and one is sent every 24 patches to bound
  terminal memory. A scattered change is sent whole (`Damage::worth_patching`).
- Pasting the patch over the old frame must reproduce the new frame exactly.
  A test pins this at the origin, at the far edge where the cell grid divides
  neither dimension, and across full-width bands.
- A frame identical to the one on screen is not sent.

## V6. The control lock

A human takes control rather than asking for it: entering a mode that acts on
the page takes the lock in either viewer. HINT holds the lock even though it
sends the page nothing, because the labels describe the page as it is now and
an agent navigating under the overlay would invalidate them.
`Mode::holds_control` and `Mode::types_into_the_page` are separate questions
(`termview/status.rs`).

The terminal viewer holds the lock file directly. The loopback viewer's input
passes a gate in the forward (`view.rs`) that drops input unless the human
holds the lock, so `control` is the one message the forward answers itself and
never passes upstream: the lock is a host fact, and the other end of that
socket is inside the box. It grants nothing beyond what the box's viewer token
already allows. The console's frame relay (`browser_frames.rs`) sends no input
at all, and a test checks its source for that.

## V7. Keeping the two viewers identical

`termview::vim::BINDINGS` is compared against the `BINDINGS` table parsed out
of `viewer.html`, and a change to one without the other fails the build. The
documented table is what is compared; a second test ties it to the resolver.

Two differences are intended:

- The web viewer has a real pointer and visible cursor, so the mouse stays live
  in VIEW without a mode. What it can reach is still up to the engine.
- The terminal viewer holds the lock file directly; the web viewer goes through
  the forward (V6).

## V8. What this is not

- No pixel-accurate pointer in the terminal (V1).
- No damage-tracked rendering upstream: only transmission is damage-based (V5).
- No hints, history or reload against a stream that does not advertise them,
  which today is every stream.
