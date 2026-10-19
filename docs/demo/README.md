# h5i demo: Build → Verify → Hack → Regression

The 60-second film is a deterministic HTML timeline, rendered to MP4. It keeps
the opening question and the shell, console, editor, and browser windows. The
agent follows one board application through four stages.

| Time | Beat |
| --- | --- |
| 0:00–0:07 | Opening question and the Build / Verify / Hack title card. |
| 0:07–0:13 | Build: the agent writes the board's Rust kernel. |
| 0:13–0:29 | Verify: extract to Lean 4, show `authorized`, run proof and the configured mutation, then reveal the kernel proof boundary. |
| 0:29–0:49 | Hack: open the same board in a browser, replay Alice's request with Bob's post ID, and record the adapter finding. |
| 0:49–0:56 | Regression: fix the adapter's visibility check and replay a YAML test; Alice gets 200 and Bob's private post returns 403. |
| 0:56–1:00 | Close: Build. Verify. Hack. Repeat. |

The Rust kernel and Lean theorem names shown are drawn from
`examples/app/board`. Its `authorized` theorem concerns successful **kernel
writes**. The private-post `GET /api/posts/{id}` route, its IDOR, and the
adapter fix are an **illustrative scenario** for this film; that route is not
present in the checked-in board server, and no live finding or regression run
is claimed. The UI labels the scenario. This distinction matters: a proof of
kernel writes cannot establish the behavior of a separate HTTP read route.

The regression flow in `docs/demo/.h5i-tests/tests/board-auth.yaml` is an example for
the illustrated route. It requires a server implementing that route and
`BOARD_ALICE_TOKEN` for an Alice session to run. The film's terminal and
receipt outputs are scripted.

## Files and rendering

- `index.html`: all frames, text, camera positions, and state transitions.
  `T` holds absolute beat times; `term` holds scripted shell events; `caps`
  holds lower-third text; `drawConsole()` controls UI state.
- `render.mjs`: drives `window.SEEK(t)` in headless Chromium and pipes frames
  to ffmpeg. `assets/` holds the fonts and logo.

The page plays in a browser. Space pauses or resumes; the scrub bar seeks.
Rendering needs ffmpeg, Playwright, and Chromium:

```bash
node render.mjs --scale 1 --out-height 1080 --fps 24 --crf 20 --preset veryfast
node render.mjs --stills 10,26,37,43,52 --scale 1
```

The page is fingerprinted by `docs/build-content.py`; changes require an
updated `PAGE_HISTORY["demo/"]` entry.
