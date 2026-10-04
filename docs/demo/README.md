# h5i teaser: is the web app your AI built really secure?

The h5i product film (1:04), built as a deterministic HTML timeline and
rendered to mp4. It is cut like a teaser, not a walkthrough, and tells the
same story as the front page and the pitch deck: **find vulnerabilities, prove
correctness, two sides of one coin, one workspace.** It is linked from the
site footer as "Demo video".

## The claim the film makes

AI agents now write and ship web apps. Securing one has two sides: finding
the bugs nobody anticipated, and proving the rules the logic must keep. The
film shows h5i doing both, in one workspace with two faces. The agent
red-teams the running app and finds a real access-control flaw, the network
policy stops it when it drifts out of scope, then the workspace turns over and
Lean 4 proves the application logic correct.

There is exactly one bug, one denial, and one theorem. Each proves its half
without turning the story into a tour.

## How it is cut

The opening is a teaser; the rest is a demo.

**Title cards (0:00 to 0:13).** Black, uppercase, letters tracking in, then
a hard cut. "Your AI built it. In an afternoon." "Is it really secure?" Then
"Security has two sides, one workspace: find vulnerabilities, prove
correctness."

**The desk (0:13 to 0:56).** Three windows, all real screens: the agent's
shell on the left, headed by the coding agent that is driving it, the
`h5i ui` console on the right, and, overlapping the console's lower right,
the app under test in the browser the agent drives, which in the second half
becomes the agent's editor: the Rust kernel, its Lean extraction, the proof. The agent types in the shell; what it does shows up
in the console and in the app; a pointer clicks through the console's views
and tabs. Nothing shakes and nothing flashes; the camera cuts to a gentle
push toward the console where it helps and leaves the shell in frame. The
narration line is optional: the finding's title, the refused run, and the
theorems turning proven carry the story on their own.

| Time | Beat |
|---|---|
| 0:13 | The agent opens the authorized target and clicks into Alice's orders; the app window follows, the History fills. |
| 0:19 | The agent replays the captured request with Bob's ID; the row lands in History, and Bob's order appears in Alice's page. The diff names the vulnerability. |
| 0:24 | Click: Findings. The finding card, with the three messages it rests on. |
| 0:28 | Untrusted page content asks for a leak; the agent tries to open paste.example and the app window shows it refused by policy. |
| 0:31 | Click: Sandboxes. The refused run in the box's receipts, with the policy that refused it. |
| 0:34 | Click: Apps. The board kernel's theorems, unconfirmed, with no receipt yet. The editor window opens: the agent writes the kernel in Rust, on h5i-app. |
| 0:38 | `cargo app-extract`: Charon and Aeneas turn the Rust into `BoardKernel.lean`, and the editor shows it. |
| 0:41 | The agent states the rule and writes the proof in `Theorems.lean`. |
| 0:46 | `cargo app-verify`: extraction, lake build, the axiom gate and the mutants pass, and the rows turn proven. |
| 0:52 | Click: Evidence. The receipt: what was checked, over what was prepared, never a score. |

**The close (0:56 to 1:04).** "Find vulnerabilities." "Prove correctness."
The mark. The tagline. The URLs.

## What is deliberately not in it

Snapshots, recon, the console, human takeover, isolation tiers, credential
brokering, export, and the trust boundary are all real, but none is the point
of this first look. Performance remains supporting proof on the website rather
than the opening premise here. There is no soundtrack; the renderer writes
video only.

## The rules the frames follow

- The shell and the console are always on screen after the cards; the app's
  window joins them while the agent is browsing. Scenes advance by a command
  in the shell or a click on a tab, never by a cut to something else.
- Every command has a visible consequence in the console or the app, so the
  film is followable with the narration line off.
- Colour is load-bearing and narrow: **red** is refused or wrong, **green** is
  allowed or proven, **orange** is h5i and nothing else. `@ref` handles,
  message ids, and Lean keywords get violet, which is none of the three.
- One line of narration at a time, as a lower third under the workspace.
- The cinema stays in the cards. After the cut into the workspace there is no
  shake, no flash, no drift: a product demo, with a zoom where it helps.

## Files

- `index.html`: the film. Every frame, camera and grain included, is a pure
  function of time `t`, so it both plays live and can be seeked
  deterministically. Open it in a browser to watch (space = play/pause, drag
  the scrub bar). Fonts and the logo are served from `assets/`.
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
node render.mjs --stills 9,25,32.5,46 --scale 1 # fast PNG frames for eyeballing a layout
```

## Editing the film

All content lives in `index.html`:

- The `T` table holds the absolute times of every beat; everything else is
  expressed relative to it, so moving a beat moves what belongs to it.
- `shots` is the camera: one static framing per beat, hard cuts between.
  `clicks` is the pointer: which tab it moves to and when it presses.
- `term` is the agent's shell: `{at, cmd}` / `{at, out:[html lines]}` events.
  `out` lines are raw HTML; `cmd` is escaped and typed out. The app pane's
  state changes are in `draw()` next to the command that causes each one.
- `drawConsole()` is the console's and the app window's state as a function of
  time: which view and tab are open, which rows, chips and pages are visible.
  The console markup sits between the `ws:start` and `ws:end` markers; the
  pitch deck carries its own static copies of an earlier, one-window version.
- `caps` is the lower third, one entry per beat. The title cards are the
  `.card` elements, driven by `card()`.
- A pane whose content outgrows its box clips silently. After adding lines,
  render a still at the fullest frame and look, in the close-up that frames it.

Because rendering is deterministic, re-rendering after an edit reproduces
every unchanged frame exactly.

The page is fingerprinted by `docs/build-content.py`, so an edit here needs its
`PAGE_HISTORY["demo/"]` entry updated or the docs build fails.
