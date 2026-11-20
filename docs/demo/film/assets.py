"""CC0 assets from Poly Haven (https://polyhaven.com) for the two rooms.

    python assets.py OUT_DIR          # download what is missing, check every SHA-256
    python assets.py OUT_DIR --pin    # re-resolve the URLs from the Poly Haven API
                                      # and rewrite assets.lock.json (maintainers only)

Files land in OUT_DIR/polyhaven/<asset>/..., keeping the relative paths a
glTF expects for its .bin and textures. Every URL and checksum is pinned in
assets.lock.json, so a build never pulls a file it has not seen before.
Everything here is CC0; docs/demo/README.md credits it.
"""

import hashlib
import json
import os
import sys
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
LOCK = os.path.join(HERE, "assets.lock.json")

# textures: (asset, resolution, maps). maps are Poly Haven map names.
TEXTURES = [
    ("wood_table_001", "2k", ["diff", "nor_gl", "rough"]),          # the engineer's desk
    ("laminate_floor_02", "2k", ["diff", "nor_gl", "rough"]),       # the engineer's floor
    ("painted_plaster_wall", "2k", ["diff", "nor_gl", "rough"]),    # the engineer's walls
    ("concrete_floor_worn_001", "2k", ["diff", "nor_gl", "rough"]),  # the attacker's floor
    ("concrete_wall_006", "2k", ["diff", "nor_gl", "rough"]),       # the attacker's wall
    ("poly_wool_herringbone", "1k", ["diff", "nor_gl", "rough"]),   # chair upholstery
    ("cotton_jersey", "1k", ["nor_gl", "rough"]),                   # the engineer's hoodie
    ("knitted_fleece", "1k", ["nor_gl", "rough"]),                  # the attacker's hoodie
    ("denim_fabric_04", "1k", ["nor_gl"]),                          # trousers
]
# models: glTF at 1k
MODELS = [
    "desk_lamp_arm_01",
    "book_encyclopedia_set_01",
    "steel_frame_shelves_03",
    "potted_plant_02",
    "office_notepads",
    "stationery_supplies",
    "cardboard_box_01",
]


def root(out):
    return os.path.join(out, "polyhaven")


def texture_path(out, asset, m):
    res = next(r for a, r, _ in TEXTURES if a == asset)
    return os.path.join(root(out), asset, f"{asset}_{m}_{res}.jpg")


def model_path(out, asset):
    return os.path.join(root(out), asset, f"{asset}_1k.gltf")


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def get(url, dst):
    req = urllib.request.Request(url, headers={"User-Agent": "h5i-film-build"})
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    with urllib.request.urlopen(req, timeout=120) as r, open(dst + ".part", "wb") as f:
        while chunk := r.read(1 << 20):
            f.write(chunk)
    os.replace(dst + ".part", dst)


def api(asset):
    req = urllib.request.Request(f"https://api.polyhaven.com/files/{asset}", headers={"User-Agent": "h5i-film-build"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.load(r)


def pin(out):
    """Resolve every file from the API, download it and record its SHA-256."""
    entries = []
    for asset, res, maps in TEXTURES:
        files = api(asset)
        for m in maps:
            key = {"nor_gl": "nor_gl", "diff": "Diffuse", "rough": "Rough"}[m]
            url = files[key][res]["jpg"]["url"]
            entries.append({"path": os.path.relpath(texture_path(out, asset, m), root(out)), "url": url})
    for asset in MODELS:
        g = api(asset)["gltf"]["1k"]["gltf"]
        entries.append({"path": f"{asset}/{asset}_1k.gltf", "url": g["url"]})
        for rel, inc in sorted(g.get("include", {}).items()):
            entries.append({"path": f"{asset}/{rel}", "url": inc["url"]})
    for e in entries:
        dst = os.path.join(root(out), e["path"])
        if not os.path.exists(dst):
            get(e["url"], dst)
        e["sha256"] = sha256(dst)
    with open(LOCK, "w") as f:
        json.dump(entries, f, indent=1)
        f.write("\n")
    print(f"pinned {len(entries)} files in {LOCK}")


def fetch(out):
    with open(LOCK) as f:
        entries = json.load(f)
    total = 0
    for e in entries:
        dst = os.path.join(root(out), e["path"])
        if not os.path.exists(dst) or sha256(dst) != e["sha256"]:
            print("get", e["url"], flush=True)
            get(e["url"], dst)
            if sha256(dst) != e["sha256"]:
                os.remove(dst)
                sys.exit(f"checksum mismatch: {e['url']}")
        total += os.path.getsize(dst)
    print(f"{len(entries)} Poly Haven files OK ({total / 1e6:.0f} MB)")


if __name__ == "__main__":
    out = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else os.path.join(HERE, "..", "out"))
    (pin if "--pin" in sys.argv else fetch)(out)
