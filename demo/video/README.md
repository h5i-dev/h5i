# h5i demo film

The h5i product video (~1:14), built as a deterministic HTML timeline and
rendered to mp4.

## The film — `index.html`

Short and punchy, for landing pages and social. Structure: pain first
(agent editing production code, Git only shows the diff), product line by 0:09,
then the sandbox as the star — three BLOCKED cards (fs / net / refs) — followed
by "The agent still finishes the task. Only the reviewed diff is merged.",
"Every diff gets a receipt." with a checked Prompt/Commands/Tests/Denials/Diff
list, ten seconds of scale (Claude + Codex in parallel), and the closing line
"Git tracks the diff. h5i tracks the run." CTA is GitHub + h5i.dev (no
`curl | sh` — the hook frames piping scripts to a shell as the threat).

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

```bash
node render.mjs                          # -> out/h5i-demo.mp4
node render.mjs --fps 60 --crf 16        # smoother / higher quality
node render.mjs --stills 40,70           # PNG frames for eyeballing a moment
```

## Editing the film

All content lives in `index.html`:

- Scene scripts (`evHookM`, `evEnvM`, `evRecM`, …) are arrays of
  `{at, cmd}` / `{at, out:[html lines]}` events, times in seconds local to the
  scene.
- Scene boundaries and eyebrow labels are in the `SCENES` table; the total
  runtime is `TOTAL`.
- Evidence-rail chips are in `RAIL` (absolute seconds).

Because rendering is deterministic, re-rendering after an edit reproduces
every unchanged frame exactly.
