# h5i demo film: Proven, not patched

A 48-second 3D film, rendered in Blender (EEVEE). A hooded engineer
extracts the board app's Rust kernel into Lean 4 and proves its authorization
rule. Then an attacker tries to delete someone else's post and every attempt is
refused. The figures wear hoods with their faces in shadow, so neither
character has a visible face.

| Time | Beat |
| --- | --- |
| 0:00–0:05 | Night. The engineer types the delete permission check in `kernel/src/lib.rs`. |
| 0:05–0:14 | Over the shoulder: `h5i app extract` → Lean 4, `h5i app prove` → `✓ authorized`. |
| 0:14–0:21 | A red room. The attacker forges a session and an `X-Role` header, then fuzzes the route. Every response is 403. |
| 0:21–0:31 | Network space. Requests hit a shield around the h5i kernel and shatter. The `authorized` theorem circles it. |
| 0:31–0:36 | The attacker's screen: 4096 of 4096 requests refused, 0 bypasses. The attacker leans back. |
| 0:36–0:41 | The engineer deploys: "Proven in Lean 4. Shipped as Rust." |
| 0:41–0:48 | End card: Proven, not patched. `cargo add h5i-app`. |

## What is real and what is illustrated

The `delete` function, the `authorized` theorem, the `allowed` definition, and
the theorem names in the proof output are from `examples/app/board`. The
`authorized` theorem states that every write the kernel makes for a
successful command is `allowed`. For `DelPost`, `allowed` means the caller is
the post's author or a moderator. The film claims only this rule.

Everything else is scripted: the terminal output, the attacker, the fuzzer,
its 4096 requests, and the deploy. The proof covers the kernel's writes. It
does not cover HTTP routes that bypass the kernel. The page under the video
says so.

`.h5i-tests/tests/board-auth.yaml` is an example regression test for the board
app. The guides link to it, but the film does not use it.

## Building

`film/build.sh` runs the whole pipeline. All of it is deterministic.

| Step | File | Output |
| --- | --- | --- |
| Timing | `film/timeline.py` | Shot times and captions, shared by every step |
| Screen textures | `film/screens.py` | `out/{laptop,hacker,hackside}/NNNN.png`, one per frame (Pillow) |
| 3D frames | `film/scene.py` | `out/frames/NNNN.png`. It builds three sets and drives everything from `update(t)` |
| Soundtrack | `film/audio.py` | `out/soundtrack.wav`. It synthesizes the drone score, typing, impacts and chimes, so nothing is licensed |
| Finish | `film/compose.py` | `out/h5i-demo.mp4`. It adds bloom, grain, captions, the end card and the audio |

```bash
PY=/path/to/python film/build.sh --publish   # ~1 h on one GPU; copies MP4 + poster into assets/
blender -b --factory-startup -P film/scene.py -- out --stills 3,13,27 --pct 50   # quick 3D stills
python film/compose.py out --still 13,44                                       # finished stills
```

You need Blender 5.x and a Python with `pillow numpy scipy fonttools brotli
imageio-ffmpeg`. The symbol fallback font is DejaVu Sans Mono; set
`H5I_FILM_SYMBOLS` if it is somewhere else. `out/` is git-ignored.

The page is fingerprinted by `docs/build-content.py`. If you change it, update
its `PAGE_HISTORY["demo/"]` entry.
