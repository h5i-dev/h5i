# h5i demo film: is the web app your AI built really secure?

The h5i product video (0:56), built as a deterministic HTML timeline and
rendered to mp4. It tells the same story as the front page and the pitch deck:
**find bugs, prove correctness, two sides of one coin, one workspace.** It is
linked from the site footer as "Demo video".

## The claim the film makes

AI agents now write and ship web apps. Securing one has two sides: finding
the bugs nobody anticipated, and proving the rules the logic must keep. The
film shows h5i doing both, in one workspace with two faces. The agent
red-teams the running app and finds a real access-control flaw, the network
policy stops it when it drifts out of scope, then the workspace turns over and
Lean 4 proves the application logic correct.

There is exactly one bug, one denial, and one theorem. Each proves its half
without turning the story into a tour.

## One continuous shot

The film is not a sequence of slides. After the question, one object is on
screen the whole time: the workspace. Its front face is the target app beside
the agent's shell; its back face is the Rust kernel beside its Lean proofs.
What the agent types on the right happens on the left, in the app itself:
the click changes the page, the replayed request renders Bob's order inside
Alice's session, the refused navigation drops a red wall across the page. The
coin flip is literal: the workspace rotates on its axis to reveal the other
side. One line of words under the workspace carries the narration.

| Time | Beat |
|---|---|
| 0:00 | The question. "Security has two sides." |
| 0:06 | A coin spins once: Find bugs / Prove correctness. It swells into the workspace. |
| 0:10 | Front face. The agent opens the authorized target, clicks into Alice's orders, replays the captured request with Bob's ID. Bob's order appears in Alice's session. The diff names the bug. |
| 0:24 | Untrusted page content tells the agent to leak the data. It tries; the allowlist refuses, the wall drops, the audit records it. |
| 0:31 | The workspace turns over. "Same workspace. Other side of the coin." |
| 0:34 | Back face. Aeneas extracts the Rust `delete` into Lean. The `authorized` theorem appears. `lake build` passes and three properties light up; the covered Rust lines get a green bar. |
| 0:49 | The workspace recedes. "Find bugs. Prove correctness." Then the product position, URLs, and license line. |

## What is deliberately not in it

Snapshots, recon, the console, human takeover, isolation tiers, credential
brokering, export, and the trust boundary are all real, but none is the point
of this first look. Performance remains supporting proof on the website rather
than the opening premise here.

## The rules the frames follow

- One object on screen. The workspace never cuts away; it changes.
- Every command has a visible consequence in the app or the proof pane, so
  nobody has to read the terminal to follow the story.
- Colour is load-bearing and narrow: **red** is refused or wrong, **green** is
  allowed or proven, **orange** is h5i and nothing else. `@ref` handles,
  message ids, and Lean keywords get violet, which is none of the three.
- One line of narration at a time, under the workspace, never over it.

## Files

- `index.html`: the film. Every frame is a pure function of time `t`, so it
  both plays live and can be seeked deterministically. Open it in a browser to
  watch (space = play/pause, drag the scrub bar). Fonts and the logo are
  served from `assets/`.
- `render.mjs`: renders the film to mp4 by driving `window.SEEK(t)` in
  headless Chromium and piping frames to ffmpeg.
- `assets/`: Space Grotesk / Space Mono (latin subsets) and the h5i logo.

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
node render.mjs --stills 21,33,46 --scale 1 # fast PNG frames for eyeballing a layout
```

## Editing the film

All content lives in `index.html`:

- The `T` table holds the absolute times of every beat; everything else is
  expressed relative to it, so moving a beat moves what belongs to it.
- `term` is the agent's shell: `{at, cmd}` / `{at, out:[html lines]}` events.
  `out` lines are raw HTML; `cmd` is escaped and typed out. The app pane's
  state changes are in `draw()` next to the command that causes each one.
- `rustLines`, `leanLines`, `thmLines` are the back face; `covered` is the set
  of Rust lines the theorem's green bar marks.
- `caps` is the narration line, one entry per beat.
- A pane whose content outgrows its box clips silently. After adding lines,
  render a still at the fullest frame and look.

Because rendering is deterministic, re-rendering after an edit reproduces
every unchanged frame exactly.

The page is fingerprinted by `docs/build-content.py`, so an edit here needs its
`PAGE_HISTORY["demo/"]` entry updated or the docs build fails.
