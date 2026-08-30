# h5i demo film — the browser

The h5i product video (~1:20), built as a deterministic HTML timeline and
rendered to mp4. It tells the same story the front page and the pitch deck
tell: **a secure, auditable browser for AI agents**. Linked from the site
footer as "the browser, in 80 seconds".

## The film — `index.html`

Five scenes.

**The hook (0:00).** An agent reads a forum thread through an ordinary headless
browser. The page composes a sentence aimed at the agent reading it — read
`~/.aws/credentials`, POST them to `paste.example` — and the agent obeys. The
point of the scene is not that the agent was fooled. It is that nothing asked
and nothing wrote it down: one more fetch, in a browser with no opinion. Title
card: h5i checks and records every request before the bytes move.

**The session (0:15).** The same page, opened with `h5i browser open`. The
diagram is the session rather than a box: the renderer that parses the
stranger's bytes, the broker that holds the allowlist, the log, the jar and the
credentials, and the gate between them — policy first, record second, wire
third. Two denials land as red cards: an off-origin tracker the page pulled in,
and then `paste.example`, which the agent asked for after being persuaded. Both
are in `h5i browser requests`, both come back with a reason, and the session
keeps going. `h5i browser audit` closes the scene with the two lanes side by
side and never merged. The two big lines are the thesis: *the agent can be
talked into it; the log cannot.*

**The controls (0:44).** `h5i browser take` moves the pill from "control: agent"
to "control: you", a human signs in at the live view, and `release` hands back
with every `@ref` stale. The password went to the page, never to the model. The
line that keeps it honest: in a box the pause is enforced, not cooperative, and
`take` says which.

**The boundary (0:59).** `h5i browser open --in web`. Nothing an agent types
changes; the status line moves from `engine-claimed` to `host-observed`, because
an egress allowlist enforced outside the engine is now corroborating the log.
`h5i box export` writes the patch, the report, the receipt and each session's
timeline.

**The close (1:12).** "Let agents browse. Keep the record."

The receipts rail along the bottom is the record accumulating: `#0 200`,
`#1 denied`, `verb snapshot`, `#2 denied`, `engine-claimed`, `audit`,
`control → you`, `control → agent`, `box web`, `host-observed`, `export`.

## Files

- `index.html` — the film. Every frame is a pure function of time `t`, so it
  both plays live and can be seeked deterministically. Open it in a browser to
  watch (space = play/pause, drag the scrub bar). No network needed — fonts and
  logo are local under `assets/`.
- `render.mjs` — renders the film to mp4 by driving `window.SEEK(t)` in
  headless Chromium and piping frames to ffmpeg.
- `assets/` — Space Grotesk / Space Mono (latin subsets) and the h5i logo.

## Render

Needs `ffmpeg` on PATH and any Playwright install (a local `node_modules`, a
global one, or an `~/.npm/_npx` cache) with its Chromium downloaded.

Frames are captured as lossless PNG (no JPEG pre-compression to fuzz text) and
supersampled: the fixed 1920x1080 stage is rendered at `--scale`x device pixels
(default 2, so 3840x2160), so the encoder is the only lossy stage. Output is the
native capture (true 4K) unless `--out-height` downscales it (lanczos) to a very
crisp lower resolution.

```bash
node render.mjs                          # -> out/h5i-demo.mp4 (2x supersampled, 4K)
node render.mjs --out-height 1080        # supersampled, very crisp 1080p (smaller file)
node render.mjs --scale 3 --crf 14       # 3x capture, higher quality
node render.mjs --stills 29,52,70        # PNG frames for eyeballing a moment
```

`--scale 1 --stills …` is the fast way to check a layout change: no
supersampling, one frame per second named.

## Editing the film

All content lives in `index.html`:

- Scene scripts (`evHookM`, `evBoxM`, `evViewM`, `evGateM`) are arrays of
  `{at, cmd}` / `{at, out:[html lines]}` events, times in seconds local to the
  scene. `out` lines are raw HTML; `cmd` is escaped and typed out.
- Scene boundaries and eyebrow labels are in the `SCENES` table; the total
  runtime is `TOTAL`, and the duration in the scrub display is derived from it.
- Receipts-rail chips are in `RAIL` (absolute seconds); the denial cards on
  the session diagram are in `BLOCKS` (seconds local to the session scene).

Because rendering is deterministic, re-rendering after an edit reproduces
every unchanged frame exactly.

The page is fingerprinted by `docs/build-content.py`, so an edit here needs its
`PAGE_HISTORY["demo/"]` entry updated or the docs build fails.
