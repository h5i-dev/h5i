# h5i demo film: is the web app your AI built really secure?

The h5i product video (0:50), built as a deterministic HTML timeline and
rendered to mp4. It tells the same story as the front page and the pitch deck:
**find bugs, prove correctness, two sides of one coin, one workspace.** It is
linked from the site footer as "Demo video".

## The claim the film makes

AI agents now write and ship web apps. Securing one has two sides: finding
the bugs nobody anticipated, and proving the rules the logic must keep. The
film shows h5i doing both. An agent red-teams the running app and finds a
real access-control flaw, the network policy stops it when it drifts out of
scope, and Lean 4 checks the proofs of application logic written on h5i-app.

There is exactly one bug, one denial, and one theorem. Each proves its half
without turning the story into a tour.

## The five scenes

**1. The question (0:00 to 0:09).** "Is the web app your AI built really
secure?" Then the two sides: find bugs, prove correctness.

**2. Find bugs (0:09 to 0:22).** The agent opens one authorized target with
capture and an origin allowlist, clicks into Alice's orders, replays the
captured request with Bob's ID, and compares the responses. Alice can read
Bob's order, and the evidence is attached.

**3. Within bounds (0:22 to 0:31).** Untrusted page content directs the agent
to `paste.example`. The configured allowlist refuses the destination, and
`h5i browser audit` shows the denied attempt in the same session record.

**4. Prove correctness (0:31 to 0:43).** The `authorized` theorem from the
bulletin-board tutorial, quoted from its proof file, then `lake build` and the
three properties Lean checked. The closing line says what a proof covers and
what red-teaming is still for.

**5. The close (0:43 to 0:50).** "Find bugs. Prove correctness. With every
change the AI ships." Then the product position, URLs, and license line.

## What is deliberately not in it

Snapshots, recon, the console, human takeover, isolation tiers, credential
brokering, export, the Rust side of the extraction, and the trust boundary
are all real, but none is the point of this first look. Performance remains
supporting proof on the website rather than the opening premise here.

There are also no diagrams. Everything the film asserts, it asserts with the
outline an agent actually gets and the terminal a person actually types into.

## The rules the frames follow

- One subject per scene. Never two panels competing.
- Colour is load-bearing and narrow: **red** is refused, **green** is allowed
  or proven, **orange** is h5i and nothing else. `@ref` handles, message ids,
  and Lean keywords get violet, which is none of the three.
- The terminal appears only when it is proving something.
- No persistent chip rail. Each command appears only where it advances the
  story.

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
node render.mjs --stills 6,20,41 --scale 1 # fast PNG frames for eyeballing a layout
```

## Editing the film

All content lives in `index.html`:

- Scene scripts (`evFind`, `evDeny`, `evProve`) are arrays of
  `{at, cmd}` / `{at, out:[html lines]}` events, times in seconds local to the
  scene. `out` lines are raw HTML; `cmd` is escaped and typed out.
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
