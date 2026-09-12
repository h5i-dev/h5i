# h5i demo film — AI red teaming

The h5i product video (0:59), built as a deterministic HTML timeline and
rendered to mp4. It tells the same story as the front page and pitch deck:
**the red-teaming browser for AI agents**. It is linked from the site footer as
"Demo video".

## The claim the film makes

Automated red teaming usually makes an agent coordinate a browser, discovery
tools, a proxy, a request editor, and a separate sandbox. h5i joins those steps
into one policy-controlled, evidence-linked session. The film follows that
session from authorized target to verified flaw, then shows what happens when
the agent drifts outside its allowed scope.

There is exactly one denial in the film. It proves that scope is enforced and
recorded without turning the story into a parade of hypothetical attacks.

## The six scenes

**1. The workflow (0:00 to 0:08).** Open one authorized target with capture and
an origin allowlist. Browse, recon, and test appear as parts of one workflow,
with one session, one policy, and one evidence trail.

**2. Browse (0:08 to 0:18).** The agent reads the application as a compact
outline, follows an invoice link, and sees the captured request and response.
The browser action and HTTP messages share the same authenticated session.

**3. Recon (0:18 to 0:29).** The agent extracts endpoint candidates, performs a
bounded crawl, calibrates away soft 404s, and lists confirmed endpoints. Each
row names the captured message that supports it; recon says what exists, not
whether it is vulnerable.

**4. Test (0:29 to 0:44).** From the browser's own traffic, the agent inspects
Alice's invoice request, changes the ID to Bob's, replays it, compares the
responses, and verifies an access-control flaw. Browser state never has to be
handed to a separate proxy or repeater.

**5. Enforce and audit (0:44 to 0:53).** Untrusted page content directs the
agent to `paste.example`. The configured allowlist refuses the destination,
and `h5i browser audit` shows the denied attempt in the same session record.

**6. The close (0:53 to 0:59).** "Let agents test like professional hackers.
Keep every action contained and auditable." Then the product position, URLs,
and license line.

## What is deliberately not in it

The console, human takeover, individual isolation tiers, credential brokering,
and export are all real, but none is the point of this first look. The film
shows the configured network limit at work without turning into a tour of the
sandbox platform. Performance remains supporting proof on the website and in
the pitch deck rather than the opening premise here.

There are also no diagrams. Everything the film asserts, it asserts with the
outline an agent actually gets and the terminal a person actually types into.

## The rules the frames follow

- One subject per scene. Never two panels competing.
- Colour is load-bearing and narrow: **red** is refused, **green** is allowed,
  **orange** is h5i and nothing else. `@ref` handles get violet, which is none
  of the three.
- The terminal appears only when it is proving something.
- No persistent chip rail. Each command appears only where it advances the one
  session's story.

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

- Scene scripts (`evOpen`, `evBrowse`, `evRecon`, `evWebsec`, `evDeny`) are
  arrays of `{at, cmd}` / `{at, out:[html lines]}` events, times in seconds
  local to the scene. `out`
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
