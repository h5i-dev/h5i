# h5i demo film: Build and Verify Your Web Apps

A 48-second 3D film, rendered in Blender (EEVEE). A hooded engineer
extracts the board app's Rust kernel into Lean 4 and proves its authorization
rule. Then an attacker tries to delete someone else's post and every attempt is
refused. The figures wear hoods with their faces in shadow, so neither
character has a visible face.

The bodies come from Blender Studio's
[Human Base Meshes](https://www.blender.org/download/demo-files/) (CC0).
`film/character.py` turns the mesh into a hoodie, trousers, gloves and shoes,
rigs it, and poses it every frame with two-bone IK. The engineer nods when
the proof passes and leans back after the deploy. The attacker leans in to
type and ends with their hands on their head.

| Time | Beat |
| --- | --- |
| 0:00–0:05 | Night. The engineer types the delete permission check in `kernel/src/lib.rs`. |
| 0:05–0:14 | Over the shoulder: `h5i app extract` → Lean 4, `h5i app prove` → `✓ authorized`. |
| 0:14–0:21 | A red room. The attacker forges a session and an `X-Role` header, then fuzzes the route. Every response is 403. |
| 0:21–0:31 | Network space. Requests hit a shield around the h5i kernel and shatter. The `authorized` theorem circles it. |
| 0:31–0:36 | The attacker's screen: 4096 of 4096 requests refused, 0 bypasses. The attacker puts their hands on their head. |
| 0:36–0:41 | The engineer deploys: "Proven in Lean 4. Shipped as Rust." |
| 0:41–0:48 | End card: Build and Verify Your Web Apps. `cargo add h5i-app`. |

The film is hosted on YouTube: <https://youtu.be/I7m39qNO0G8>. `index.html`
and the front page show `assets/poster.jpg` and load the
`youtube-nocookie.com` player only when someone clicks play. The MP4 itself is
kept at `media/demo/h5i-demo.mp4`, outside `docs/` so Pages does not serve
it. To change the film, rebuild it with `film/build.sh --publish` and upload
the new MP4 to YouTube.

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
| Characters | `film/character.py` | Dressed, rigged figures posed from joint targets (imported by `scene.py`) |
| 3D frames | `film/scene.py` | `out/frames/NNNN.png`. It builds three sets and drives everything from `update(t)` |
| Soundtrack | `film/audio.py` | `out/soundtrack.wav`. It synthesizes the drone score, typing, impacts and chimes, so nothing is licensed |
| Finish | `film/compose.py` | `out/h5i-demo.mp4`. It adds bloom, grain, captions, the end card and the audio |

```bash
PY=/path/to/python film/build.sh --publish   # ~1 h on one GPU; copies the poster into assets/ and the MP4 into media/demo/
blender -b --factory-startup -P film/scene.py -- out --stills 3,13,27 --pct 50   # quick 3D stills
python film/compose.py out --still 13,44                                       # finished stills
```

You need Blender 5.x and a Python with `pillow numpy scipy fonttools brotli
imageio-ffmpeg`. `build.sh` downloads the Human Base Meshes bundle and verifies its SHA-256 (set
`H5I_FILM_HUMAN` to use a local `.blend`). The symbol fallback font is DejaVu Sans Mono; set
`H5I_FILM_SYMBOLS` if it is somewhere else. `out/` is git-ignored.

The page is fingerprinted by `docs/build-content.py`. If you change it, update
its `PAGE_HISTORY["demo/"]` entry.
