# Design: the in-terminal browser

The engine renders a page; this is the half a human looks at and drives. Two
viewers share one keymap: the terminal viewer (`h5i browser view`,
`h5i box view --term`) and the loopback viewer served to a browser. Sections V1
to V8.

## In one screen

- The keyboard is the instrument, not the pointer. A terminal reports cells, not
  pixels, and hides the cursor, so aiming is guesswork (V1).
- Hint labels are stuck to [`snapshot`](design-browser.md) refs, so the overlay
  can only offer what the verb layer would accept, and a human's click leaves the
  same receipt an agent's would (V2).
- Which keys are bound is read from what the engine advertises, never inferred
  from its name. One key, `i`, fails open (V3).
- Typing sends real key events, so the caret moves and the page hears `keydown`
  (V4).
- Latency is managed by not doing work: batch the keys, render only for a viewer
  that can take a frame, send only the pixels that changed (V5).
- Reaching for the controls takes the control lock, in both viewers (V6).
- The two keymaps are kept identical by a test that parses one out of the other
  (V7).

Part of the h5i design set. The user-facing keys and commands are in
[`MANUAL.md`](../../MANUAL.md); the engine underneath is
[`design-browser.md`](design-browser.md); the roadmap is
[`ROADMAP.md`](../../ROADMAP.md).

---

## V1. Why keys and not a pointer

A pointer in a terminal is a poor instrument, and not because the terminal is
primitive:

- **Cells, not pixels.** Mouse reporting is `?1003h` with SGR coordinates, so a
  click lands at the corner of the cell it was in. `?1016` would give pixels, but
  a terminal that does not support it keeps reporting cells with no way to tell
  the difference.
- **No cursor to aim with.** The viewer hides it, so the only feedback that the
  pointer is anywhere is the page's own hover state, arriving a frame later.
- **No key releases.** The legacy encoding has no key-up, so a press and a
  release are synthesized together. A held key, and therefore a drag, is not
  expressible.

Closing that gap needs pixel mouse reporting, the progressive keyboard protocol,
a locally composited cursor and input prediction. Terminal emulators that have
done it reach outside the terminal for OS-level input events, which for a viewer
whose claim is that every escape sequence is generated host-side is not an option.

Naming a target and pressing a key needs none of it.

The pointer is still there under `i`, for what a keyboard cannot express. See V3
for when it is offered.

## V2. Hints are the snapshot's refs

The hard part of a hint overlay is deciding what is actionable. Every
browser-side implementation answers it with a heuristic DOM walk of its own.

Here it is already answered. `snapshot` mints a ref only for something a caller
could act on, and the verb layer honours exactly those, so a hint is a label
stuck to a ref. Three consequences:

1. **The overlay cannot lie.** It cannot offer a target the engine would then
   refuse, because the overlay is not the one deciding.
2. **The trail is the same shape.** Pressing a label dispatches `click @e7`, so
   the receipt names a role and an accessible name. A pixel click would record a
   coordinate, which tells a reviewer nothing. A human and an agent driving the
   same page leave comparable records.
3. **Geometry is the only thing added.** `Page::hint_targets` walks the same refs
   and attaches rects.

Two details worth stating, because the obvious implementation gets both wrong:

- **Inline elements have no box.** A non-replaced inline element, which is the
  ordinary shape of a link inside a paragraph, has zero taffy size: its text is
  laid out by parley into the containing block. Reading only box layout produced
  an overlay that could label a form and not one link in an article. The fallback
  unions the element's inline runs, as `getClientRects` does.
- **Labels are minted by the engine.** Two viewers numbering one page
  independently is two answers to a question with one right answer, and the first
  disagreement is a human pressing `sd` and activating something else. Matching
  what has been typed is per-viewer state and stays with each viewer. Both halves
  rest on the labels being prefix-free, which is what lets a viewer act the
  moment a label is complete rather than waiting to see if another key follows.

`F` and `gi` narrow the overlay to fields. Labelling a link under "type into
something" offers a choice whose only possible answer is a refusal.

## V3. Capability gating, and the one key that fails open

The terminal viewer watches boxes running an engine that is not ours. Which keys
mean anything is therefore a property of the session, read from a `features` list
the engine sends with `status`, never inferred from its name.

Movement is deliberately outside the gate. Scrolling goes out as wheel and arrow
events every engine understands, so the keys a reader uses most work everywhere.
Only hints, history, reload and typing are gated, and an unavailable key says so
rather than doing nothing: a key that does nothing and explains nothing gets
pressed harder.

`i` is the exception, and the asymmetry is deliberate. Every other gated key is a
capability introduced with this keymap, so an engine that has not mentioned one
does not have it. Handing the page the keyboard predates all of this, and on a
box running an engine that will never send a feature list it is the only way to
drive a canvas or a drag. So for `pointer` the question is "did you say no", not
"did you say yes":

| what the engine sent | `i` |
| --- | --- |
| nothing at all | bound, as it always was |
| a list including `pointer` | bound |
| a list without `pointer` | refused, pointing at `f` |

h5i's own engine does not advertise `pointer`, and should not: its viewer lane
drops a pointer press and a move, and acts only on a release over a link. A mode
that appears to work and does almost nothing is worse than one that is absent.

## V4. Typing is real key events

`type` sets a field's whole value, fires `input` and `change`, and leaves the
caret at the end. That suits an agent, which knows what the field should say and
has no caret. It is the wrong shape for a person, and the gap showed up as "we do
not support text input": there was no `keydown` on the page at all, so no caret
moved, `Backspace` in the middle of a word did nothing, `Tab` did not reach the
next field, and a page listening for typing never heard any.

The fix is split in two. `crates/h5i-browser/src/keys.rs` is the decision:
a table from a DOM key name to an edit, testable without a document. `Page::key_to_focused`
is the half that needs one.

Two rules run through the table:

- **An unmapped key is not swallowed.** It still delivers the DOM events, so a
  page's own shortcut keeps working while a field has focus.
- **Modified keys are commands.** `Ctrl-S` must not put an `s` in the field
  somebody was saving. Shift is the exception, since shifted characters arrive
  already shifted; that is also why `text` is reported by the viewer rather than
  derived by the engine, which cannot see the keyboard layout.

Events are dispatched *around* the edit, `keydown` before and `input` after,
which is the order an autocomplete or a framework-controlled input is written
against.

Focusing a field does not clear it. Emptying it would assume anyone who focuses a
field meant to replace it, which is wrong for everyone who came to correct a
character or append.

## V5. Latency, by not doing work

Measured on this machine, a keystroke costs about 13ms in the engine, of which
about 12ms is rasterization; applying the key itself is under 1ms, and JPEG
quality barely moves the total. The round trip a viewer sees is around 20ms
isolated, and the frame is 40 to 68KB.

Three things follow, in the order they were found.

**Batch the keys.** A keystroke cannot be answered locally: nothing appears until
the page has been laid out again and a frame encoded. Sent one at a time those
requests serialize, and typing 25 characters left the text 869ms behind the
fingers. At most one message is on the wire and every key struck while it is away
rides out together, which brought that to 88ms.

Batched, never coalesced. A keystroke is a *delta*, so dropping the ones in
between loses characters. What batching buys is that the relayout and render are
paid once per message however many keys it carries, which is the same saving
merging would have given without the loss. The earlier whole-value `insert` could
be coalesced precisely because it was a snapshot; that is also why a lost batch is
given up on rather than retried, since repeating one that may already have arrived
would type the word twice.

**Render only for a viewer that can take it.** Under ack pacing the engine held a
rendered frame for a viewer that still owed an ack, then threw it away when the
next replaced it. On a single-threaded page that is not merely waste: it is 13ms
the next keystroke spends waiting. The render is deferred to the moment somebody
can receive it, which also makes what they get more current than the held frame
would have been.

**Send only the pixels that changed.** This was the largest by far. Typing moves a
caret and a character or two, and the viewer was retransmitting the whole
viewport for it: about 40KB of deflated JPEG per key, a megabyte to type one
word, with the terminal decoding and blitting a full screen each time. The viewer
now diffs the new frame against what is on screen, rounds the changed box out to
cell boundaries and places just that rectangle. Measured over one typed word,
1016KB fell to 28 to 85KB.

Three rules keep that correct:

1. Cell alignment only ever grows the box. Rounding inwards would leave changed
   pixels along the edge showing the previous frame.
2. A patch is placed *over* what is beneath it and is never deleted individually,
   because what is underneath is the older picture it exists to correct. A whole
   frame releases them all, and one is sent every 24 patches to bound the
   terminal's memory.
3. Pasting a patch back over the old frame must reproduce the new frame exactly.
   A test pins this at the origin, against the far edge where the cell grid
   divides neither dimension, and across full-width bands. If it can fail, the
   viewer shows a page that was never rendered, which is worse than being slow.

A frame identical to the one on screen is not sent at all.

## V6. The control lock

The lock's own rule is that a human takes control rather than asking for it.
Reaching for the controls at either viewer takes it, so the detour through
`h5i browser take` in another window is gone.

HINT holds the lock, which is worth arguing for because an overlay is a read and
the human has not touched the page yet. Two things settle it. Asking for the
overlay is an intent to act, so making the human take the lock separately is a
step with no decision in it. And the labels describe the page *as it is now*: an
agent that navigates while the overlay is up leaves every label pointing into a
document that is gone, which the ref check would catch and refuse, having already
spent the human's keystroke.

`Mode::holds_control` and `Mode::types_into_the_page` are two questions, not one.
HINT holds the lock and sends the page nothing.

For the loopback viewer this needs one thing from the forward. Its messages pass
a gate that drops input unless the human holds the lock, so the viewer could not
take it. `control` is the single message the forward answers itself and never
passes upstream: the lock is a host fact kept in a file beside the box, and the
thing on the other end of that socket is inside the box. It grants nothing new,
since whoever reached that loopback port with the box's viewer token already has
the standing `h5i browser take` runs with.

## V7. Keeping the two viewers identical

The point of giving the loopback viewer the same keys is that there is one thing
to learn. A keymap maintained by hand in two languages drifts silently: nothing
fails, one viewer just quietly does something else.

So `termview::vim::BINDINGS` is compared against the JavaScript table parsed out
of `viewer.html`, and a change to either without the other fails the build. The
*documented* table is what is compared, since what a reader is promised is what
has to match; a second test ties that table to the resolver.

Two differences are real and stay:

- The browser has a real pointer with pixel coordinates and a visible cursor, so
  the mouse stays live in VIEW without a mode. What it can *reach* is still
  whatever the engine implements.
- The terminal viewer holds the lock file directly; the loopback viewer goes
  through the forward (V6).

## V8. What this is not

- **No pixel-parity pointer.** V1 says why. `i` remains for engines that can use
  it.
- **No damage-tracked rasterization.** The engine still renders the whole page
  for every frame; only the *transmission* is damage-based (V5). Cutting the 12ms
  render means damage inside blitz, which is a much larger piece of work.
- **No `Shift-Tab`.** Blitz offers forward focus only, and cycling all the way
  round a long form to fake backwards is a caret that appears to jump somewhere
  arbitrary.
- **No hints for `contenteditable`.** The snapshot mints no ref for it, so there
  is nothing to label.
