# h5i demo film: Build and Verify Your Web Apps

A 52-second 3D film, rendered in Blender (EEVEE). It opens on the web apps
around us: a robot vacuum, a smart speaker and a server rack, each serving
HTTP. Then a hooded engineer
extracts the board app's Rust kernel into Lean 4 and proves its authorization
rule. Then an attacker tries to delete someone else's post and every attempt is
refused. It closes on the same devices, their tags now green with a proven
mark, so the film ends where it began. The figures wear hoods with their faces in shadow, so neither
character has a visible face.

The bodies come from Blender Studio's
[Human Base Meshes](https://www.blender.org/download/demo-files/) (CC0).
`film/character.py` turns the mesh into a hoodie, trousers, gloves and shoes,
rigs it, and poses it every frame with two-bone IK. The engineer nods when
the proof passes and leans back after the deploy. The attacker leans in to
type and ends with their hands on their head.

The hoodies have sculpted folds at the elbows, above the cuffs and over the
hem, ribbed cuffs and a hem band, a kangaroo pocket and drawstrings, with a
knit normal map. The rooms are dressed with CC0 textures and props from
[Poly Haven](https://polyhaven.com) (see [Credits](#credits)), lit by the
desk lamp, the screens and the city through the window. The city is blocks
of lit windows up to a kilometre away, so the lens turns them into bokeh.
The network space stays abstract on purpose.

| Time | Beat |
| --- | --- |
| 0:00–0:05 | Night, the engineer's room. The camera follows a robot vacuum across the floor, past a server rack, to a smart speaker and the laptop on the desk. Each device shows a tag with an HTTP route: "Web apps run everywhere, from servers to the devices around you." |
| 0:05–0:08 | The tags turn red: "Every one of them is a target." The engineer types the delete permission check in `kernel/src/lib.rs`. |
| 0:08–0:17 | Over the shoulder: `h5i app extract` → Lean 4, `h5i app prove` → `✓ authorized`. |
| 0:17–0:24 | A red room. The attacker forges a session and an `X-Role` header, then fuzzes the route. Every response is 403. |
| 0:24–0:34 | Network space. Requests hit a shield around the h5i kernel and shatter. The `authorized` theorem circles it. |
| 0:34–0:39 | The attacker's screen: 4096 of 4096 requests refused, 0 bypasses. The attacker puts their hands on their head. |
| 0:39–0:41 | Over the shoulder, the engineer deploys the board app: "Proven in Lean 4. Shipped as Rust." |
| 0:41–0:47 | The camera pulls back to the whole room. The opening's tags come back red, then a green ring spreads from the laptop and each device's tag turns green with a check mark: "From servers to robots, wherever web apps run." |
| 0:47–0:52 | End card: Build and Verify Your Web Apps. `cargo add h5i-app`. |

The film is hosted on YouTube: <https://youtu.be/eHOLi1kxWEo>. `index.html`
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
the post's author or a moderator. The film claims only this rule for the
board app.

The film's "robots" stand for embedded and edge devices. h5i's embedded example
is `examples/embedded/thermostat`, which also runs on the small XIAO RP2040
board on the desk (a background detail; its LED turns green with the rest).
Its `interlock` theorem says the heater is off at or above 35.0 °C in every
reachable state, and h5i also proves the HTTP/1.1 parser it is served through.

Everything else is scripted: the terminal output, the attacker, the fuzzer,
its 4096 requests and the deploy. The robot vacuum, the smart speaker and the
server rack, their routes, and the green proven marks they get at the end
illustrate where h5i is meant to be used. The film does not claim that these
products run h5i today. The proof covers the kernel's writes. It
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
| Assets | `film/assets.py` | `out/polyhaven/`: CC0 textures and models, every URL and SHA-256 pinned in `film/assets.lock.json` |
| Characters | `film/character.py` | Dressed, rigged figures posed from joint targets (imported by `scene.py`) |
| Realism | `film/realism.py` | Textures, props, the city, practical lights and motion blur for the rooms (imported by `scene.py`) |
| 3D frames | `film/scene.py` | `out/frames/NNNN.png`. It builds three sets and drives everything from `update(t)` |
| Soundtrack | `film/audio.py` | `out/soundtrack.wav`. It synthesizes the drone score, typing, impacts and chimes, so nothing is licensed |
| Finish | `film/compose.py` | `out/h5i-demo.mp4`. It adds lens effects (chromatic aberration, halation), bloom, grain, captions, the end card and the audio |

```bash
PY=/path/to/python film/build.sh --publish   # ~1 h on one GPU; copies the poster into assets/ and the MP4 into media/demo/
blender -b --factory-startup -P film/scene.py -- out --stills 3,16,30 --pct 50   # quick 3D stills
python film/compose.py out --still 16,47                                       # finished stills
```

You need Blender 5.x and a Python with `pillow numpy scipy fonttools brotli
imageio-ffmpeg`. `build.sh` downloads the Human Base Meshes bundle and verifies its SHA-256 (set
`H5I_FILM_HUMAN` to use a local `.blend`), then the Poly Haven files (about 56 MB), each checked
against `film/assets.lock.json`. The symbol fallback font is DejaVu Sans Mono; set
`H5I_FILM_SYMBOLS` if it is somewhere else. `out/` is git-ignored.

The page is fingerprinted by `docs/build-content.py`. If you change it, update
its `PAGE_HISTORY["demo/"]` entry.

## Credits

Everything the film downloads is CC0 (public domain).

- Bodies: [Human Base Meshes](https://www.blender.org/download/demo-files/) by Blender Studio.
- Textures from [Poly Haven](https://polyhaven.com):
  `wood_table_001` (desks), `laminate_floor_02` and `painted_plaster_wall` (the engineer's room),
  `concrete_floor_worn_001` and `concrete_wall_006` (the attacker's room),
  `poly_wool_herringbone` (chairs), `cotton_jersey` and `knitted_fleece` (hoodies),
  `denim_fabric_04` (trousers).
- Models from [Poly Haven](https://polyhaven.com):
  `desk_lamp_arm_01`, `book_encyclopedia_set_01`, `steel_frame_shelves_03`, `potted_plant_02`,
  `office_notepads`, `stationery_supplies`, `cardboard_box_01`.

The laptop, the devices, the XIAO board, the monitors and the city are built
in `scene.py` and `realism.py`.
