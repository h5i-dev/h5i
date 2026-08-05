# h5i demo film

The h5i product video (~1:18), built as a deterministic HTML timeline and
rendered to mp4. Embedded on the front page hero (`docs/index.html`) as an
`<iframe src="demo/?embed">`, which hides the scrub controls and loops.

## The film — `index.html`

The first buyer workflow, compressed: run a pull request you do not trust,
keep it off your machine. Five scenes. The hook (0:00): an AI-written PR is
checked out on a bare host, `npm install` fires a postinstall, and the keys
leave the laptop. The box (0:15): `h5i box --pr 214`, the agent runs with
permissions off inside the boundary, the same postinstall hits two denial
cards (fs, net), then the agent starts the dev server and drives agent-browser
through the signup flow. The viewport (0:44): `h5i box view` next to a mock
browser window, `h5i browser take` flips the control pill to "you" and a human
click lands, `release` hands back. The output gate (0:58): `h5i box export`
with the patch/report/screenshot/receipt checklist and the denied-egress line.
Close (1:10): "Run code you don't trust. Keep it off your machine." CTA is
GitHub + h5i.dev (no `curl | sh` in the close: the hook frames running
untrusted code as the threat).

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
node render.mjs --stills 40,70           # PNG frames for eyeballing a moment
```

## Editing the film

All content lives in `index.html`:

- Scene scripts (`evHookM`, `evBoxM`, `evViewM`, `evGateM`) are arrays of
  `{at, cmd}` / `{at, out:[html lines]}` events, times in seconds local to the
  scene.
- Scene boundaries and eyebrow labels are in the `SCENES` table; the total
  runtime is `TOTAL` (also update the hardcoded `/ 1:18` in the time display).
- Receipts-rail chips are in `RAIL` (absolute seconds); the denial cards on
  the box diagram are in `BLOCKS` (seconds local to the box scene).

Because rendering is deterministic, re-rendering after an edit reproduces
every unchanged frame exactly.
