#!/usr/bin/env bash
# Build the h5i demo film end to end:
#   fonts -> screen textures -> Blender (EEVEE) frames -> soundtrack -> MP4.
#
#   film/build.sh            # full build, ~1 hour on one GPU
#   film/build.sh --publish  # also copy the MP4 and poster into assets/
#
# Needs: blender (5.x) on PATH and a Python with pillow, numpy, scipy,
# fonttools, brotli and imageio-ffmpeg (set PY to choose it).
set -euo pipefail
cd "$(dirname "$0")"
OUT=../out
PY=${PY:-python3}
mkdir -p "$OUT/fonts"

for f in SpaceGrotesk-400 SpaceMono-400 SpaceMono-700; do
  [ -f "$OUT/fonts/$f.ttf" ] || "$PY" -c "
from fontTools.ttLib import TTFont
t = TTFont('../assets/$f.woff2'); t.flavor = None; t.save('$OUT/fonts/$f.ttf')"
done

"$PY" screens.py "$OUT"
blender -b --factory-startup -P scene.py -- "$OUT" --dump-packets "$OUT/packets.json"
# two processes keep the GPU busy while the other one syncs the scene
half=$("$PY" -c "from timeline import NFRAMES; print(NFRAMES // 2)")
total=$("$PY" -c "from timeline import NFRAMES; print(NFRAMES)")
blender -b --factory-startup -P scene.py -- "$OUT" --from 1 --to "$half" > "$OUT/render-a.log" 2>&1 &
blender -b --factory-startup -P scene.py -- "$OUT" --from $((half + 1)) --to "$total" > "$OUT/render-b.log" 2>&1 &
wait
"$PY" audio.py "$OUT"
"$PY" compose.py "$OUT"

if [ "${1:-}" = "--publish" ]; then
  cp "$OUT/h5i-demo.mp4" ../assets/h5i-demo.mp4
  "$PY" -c "
from PIL import Image
Image.open('$OUT/poster.png').convert('RGB').save('../assets/poster.jpg', quality=88)"
fi
