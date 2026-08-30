# h5i demo film — the browser

The h5i product video (0:58), built as a deterministic HTML timeline and
rendered to mp4. It tells the same story the front page and the pitch deck
tell: **a fast, auditable browser for AI agents**. Linked from the site footer
as "the browser, in under a minute".

## The claim the film makes

Browsers built for people are both *too heavy* and *too risky* for agents, and
h5i answers both. So the first half is the engine and the second half is the
record, and neither half is an afterthought: a film that opened on prompt
injection would read as a security wrapper around somebody else's browser.

One session runs through all three working scenes, and there is exactly one
denial in the whole film. Repeating a single refusal is stronger than parading
several attacks.

## The four scenes

**1. The engine (0:00 to 0:12).** *Run more browser sessions.* Four sessions
come up in about a quarter of a second each, then the three numbers: ~5×
faster reads, ~80% less peak memory, 300k+ web standards tests passed, with
"Benchmarked on simple websites." under them. Twelve seconds spent establishing
that this is a headless browser, not a security wrapper.

**2. The browser (0:12 to 0:28).** *The agent reads and controls the page
directly.* `open`, `snapshot`, `click @e2`, with a beat between each command and
its result. Three things and no more: it can read the page, it can act on it,
and the session is still there afterwards. No security yet.

**3. The record (0:28 to 0:50).** *A malicious page can mislead the agent. It
cannot change the browser policy.* The same session. A `snapshot --delta` brings
back new text the page's readers wrote, fenced as untrusted, telling the agent
to send credentials to `paste.example`. The agent tries it. The refusal lands as
a full-width red band, `h5i browser requests` shows the two rows that matter,
and then an ordinary `click` proves the session survived. Twenty-two seconds for
one event, because this is the event.

**4. The close (0:50 to 0:58).** Fast enough to run more sessions. Controlled
enough to trust them. Then "Let agents browse. Keep control.", the two URLs and
the licence line.

## What is deliberately not in it

Human takeover and the sandbox are both real and both worth showing, and both
belong in their own short films rather than this one. Carrying them here makes
it ambiguous again whether h5i is a browser or a sandbox platform. The same goes
for `engine-claimed` versus `host-observed`, the broker and renderer split, and
`h5i box export`: all true, none of them the point of a first look.

There are also no diagrams. Everything the film asserts, it asserts with the
outline an agent actually gets and the terminal a person actually types into.

## The rules the frames follow

- One subject per scene. Never two panels competing.
- Colour is load-bearing and narrow: **red** is refused, **green** is allowed,
  **orange** is h5i and nothing else. `@ref` handles get violet, which is none
  of the three.
- The terminal appears only when it is proving something.
- No persistent chip rail, and the request record appears in scene 3 alone.

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
node render.mjs --stills 27,49 --scale 1 # fast PNG frames for eyeballing a layout
```

## Editing the film

All content lives in `index.html`:

- Scene scripts (`evOpen`, `evBrowse`, `evDeny`) are arrays of `{at, cmd}` /
  `{at, out:[html lines]}` events, times in seconds local to the scene. `out`
  lines are raw HTML; `cmd` is escaped and typed out.
- Scene boundaries and eyebrow labels are in the `SCENES` table; the total
  runtime is `TOTAL`, and the duration in the scrub display is derived from it.
- A terminal whose content outgrows its box scrolls silently, which reads as a
  bug on video. After adding lines, check that `tbody.scrollHeight` still equals
  `clientHeight` at the scene's fullest frame, and size the box or step the type
  down until it does.

Because rendering is deterministic, re-rendering after an edit reproduces
every unchanged frame exactly.

The page is fingerprinted by `docs/build-content.py`, so an edit here needs its
`PAGE_HISTORY["demo/"]` entry updated or the docs build fails.
