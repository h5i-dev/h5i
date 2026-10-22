# h5i demo: Build → Verify → Test the running app → Regression

The 80-second film is a deterministic HTML timeline, rendered to MP4. It keeps
the opening question and the shell, console, editor, and browser windows. The
agent follows one board application through four stages.

| Time | Beat |
| --- | --- |
| 0:00–0:07 | Opening question and the Build and Verify Your Web Apps title card. |
| 0:07–0:19 | Build: a large editor shows the board's Rust delete permission check. |
| 0:19–0:28 | Extract: Charon and Aeneas visibly generate the Lean 4 author and moderator branches from Rust, then the full generated definition appears. |
| 0:28–0:42 | Verify: read the `authorized` theorem, its `writes_of` case split, and the scripted Lean check result. |
| 0:42–0:52 | Explore: the theorem dependency graph follows the real Lean lemmas, then states the kernel write proof boundary. |
| 0:52–1:09 | Test the running board end to end with Development Infrastructure. Its red-teaming tool reveals the HTTP read flaw. |
| 1:09–1:16 | Regression: fix the adapter's visibility check and replay a YAML test; Alice gets 200 and Bob's private post returns 403. |
| 1:16–1:20 | Close: Build and Verify Your Web Apps. with h5i. The Verifiable Web Framework for Rust. |

The Rust kernel and Lean theorem names shown are drawn from
`examples/app/board`. Its `authorized` theorem concerns successful **kernel
writes**. The dependency graph shows only dependencies present in the board's
Lean proof. `h5i app check examples/app/board` records `delete_spec → writes_of`,
`writes_of → authorized`, and `other_mod → authorized` in its declaration
catalog; the film renders that small, fixed subset. The private-post
`GET /api/posts/{id}` route, its IDOR, and the adapter fix are an
**illustrative scenario** for this film; that route is not
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
  to ffmpeg. `assets/` holds the fonts, logo, and demo cover image.

The page plays in a browser. Space pauses or resumes; the scrub bar seeks.
Rendering needs ffmpeg, Playwright, and Chromium:

```bash
node render.mjs --scale 1 --out-height 1080 --fps 24 --crf 20 --preset veryfast
node render.mjs --stills 14,20,22,25,34,45,50,64,73 --scale 1
```

The page is fingerprinted by `docs/build-content.py`; changes require an
updated `PAGE_HISTORY["demo/"]` entry.
