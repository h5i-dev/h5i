#!/usr/bin/env bash
# Build the h5i demo film end to end:
#   fonts -> screen textures -> Blender (EEVEE) frames -> soundtrack -> MP4.
#
#   film/build.sh            # full build, ~1 hour on one GPU
#   film/build.sh --publish  # also copy the poster into assets/ and the MP4 into media/demo/
#
# The site embeds the film from YouTube (https://youtu.be/I7m39qNO0G8).
# --publish also copies the MP4 to media/demo/, outside docs/ so Pages does
# not serve it; upload that file to YouTube to change the film.
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

# Blender Studio's Human Base Meshes (CC0), the characters' bodies
HBM="$OUT/human_base_meshes_bundle.blend"
if [ ! -f "$HBM" ]; then
  url=${H5I_FILM_HUMAN_URL:-https://mirror.freedif.org/blender/demo/asset-bundles/human-base-meshes/human-base-meshes-bundle-v1.4.1.zip}
  curl -fL -o "$OUT/hbm.zip" "$url"
  echo "811f43accbb31a88266d932f8f5563b2d13586fca0ba2693aad1f5fe582b3515  $OUT/hbm.zip" | sha256sum -c -
  "$PY" -c "
import zipfile; z = zipfile.ZipFile('$OUT/hbm.zip')
open('$HBM', 'wb').write(z.read('human-base-meshes-bundle-v1.4.1/human_base_meshes_bundle.blend'))"
fi

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
  cp "$OUT/h5i-demo.mp4" ../../../media/demo/h5i-demo.mp4
  "$PY" -c "
from PIL import Image
Image.open('$OUT/poster.png').convert('RGB').save('../assets/poster.jpg', quality=88)"
fi
