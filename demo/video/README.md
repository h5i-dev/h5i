# h5i demo film

A ~2:40 product demo video of h5i, built as a deterministic HTML timeline and
rendered to mp4. It follows the README's 60-second flow:

1. **Hook** — two terminals: the `git log` you see, and meanwhile an unboxed
   agent piping attacker-malware.com scripts to `sh`, uploading
   `~/.aws/credentials`, clobbering another agent's worktree, deleting tests.
   `git log` remembers *what* changed — not *why*, and not any of that.
2. **Capture → recall** — Claude works on the left (prompts, compressed tool
   output, `h5i capture commit`); on the right a human replays it all with
   `h5i recall log` / `h5i recall context show`, then `h5i share push`.
3. **The sandboxed worktree** — `h5i env create/shell`, an agent with
   permissions off, three policy denials (fs / net / refs) landing live on the
   diagram, then `diff → propose → apply`.
4. **The ensemble** — `h5i team auto-create/dispatch`, sealed claude + codex
   envs, peer review, neutral verify, explainable verdict, one merged result.
5. **Outro** — the evidence rail: everything in `refs/h5i/*`, install command.

The signature element is the **evidence rail** along the bottom: every moment
in the film appends a chip (intent, capture, deny:fs, verdict, …), so the video
itself accumulates the audit trail it is describing.

## Files

- `index.html` — the film. Every frame is a pure function of time `t`, so it
  both plays live and can be seeked deterministically. Open it in a browser to
  watch (space = play/pause, drag the scrub bar). No network needed — fonts and
  logo are local under `assets/`.
- `render.mjs` — renders the film to `out/h5i-demo.mp4` by driving
  `window.SEEK(t)` in headless Chromium and piping frames to ffmpeg.
- `assets/` — Space Grotesk / Space Mono (latin subsets) and the h5i logo.

## Render

Needs `ffmpeg` on PATH and any Playwright install (a local `node_modules`, a
global one, or an `~/.npm/_npx` cache) with its Chromium downloaded.

```bash
node render.mjs                    # -> out/h5i-demo.mp4 (1080p30, H.264)
node render.mjs --fps 60 --crf 16  # smoother / higher quality
node render.mjs --stills 40,70     # PNG frames for eyeballing a moment
```

## Editing the film

All content lives in `index.html`:

- Scene scripts (`evAgent`, `evEnv`, `evOrch`, …) are arrays of
  `{at, cmd}` / `{at, out:[html lines]}` events, times in seconds local to the
  scene.
- Scene boundaries and eyebrow labels are in the `SCENES` table; the total
  runtime is `TOTAL`.
- Evidence-rail chips are in `RAIL` (absolute seconds).

Because rendering is deterministic, re-rendering after an edit reproduces
every unchanged frame exactly.
