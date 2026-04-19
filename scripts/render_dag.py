"""Render h5i context dag as a dark-theme PNG using pattern-based coloring."""
import subprocess, re, os
from PIL import Image, ImageDraw, ImageFont

# ── Run h5i and capture plain text ────────────────────────────────────────────
env = os.environ.copy()
env["NO_COLOR"] = "1"   # force plain output
result = subprocess.run(
    ["/home/koukyosyumei/Dev/h5i/target/debug/h5i", "context", "dag"],
    cwd="/tmp/h5i-demo",
    capture_output=True, text=True, env=env,
)
lines = result.stdout.rstrip().splitlines()

print(f"Captured {len(lines)} lines")
for l in lines[:5]:
    print(repr(l))

# ── Colour palette (Catppuccin Mocha) ─────────────────────────────────────────
BG      = "#1e1e2e"
FG      = "#cdd6f4"
DIM     = "#6c7086"
BLUE    = "#89b4fa"
YELLOW  = "#f9e2af"
GREEN   = "#a6e3a1"
MAGENTA = "#cba6f7"
CYAN    = "#89dceb"
RED     = "#f38ba8"
WHITE   = "#cdd6f4"

# ── Font loading ──────────────────────────────────────────────────────────────
FONT_PATHS = [
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
]
FONT_SIZE = 32
font = None
for fp in FONT_PATHS:
    try:
        font = ImageFont.truetype(fp, FONT_SIZE)
        break
    except Exception:
        pass
if font is None:
    font = ImageFont.load_default()

# Measure monospace cell
dummy = Image.new("RGB", (200, 40))
dd = ImageDraw.Draw(dummy)
bb = dd.textbbox((0, 0), "M", font=font)
CW = bb[2] - bb[0]
CH = bb[3] - bb[1] + 5

PAD_X  = 70
PAD_Y  = 48
HEADER = 68

# ── Assign a colour to each character position on a line ──────────────────────
def colorize(line: str) -> list[tuple[str, str]]:
    """Return list of (char_or_segment, colour) for the line."""
    stripped = line.rstrip()

    # ── Header / separator lines ──────────────────────────────────────────
    if stripped.startswith("──") or stripped.startswith("  ──"):
        return [(stripped, DIM)]

    # ── Goal line ─────────────────────────────────────────────────────────
    if "Goal:" in stripped:
        pre, _, rest = stripped.partition("Goal:")
        return [(pre, DIM), ("Goal:", DIM), (rest, CYAN)]

    # ── Summary footer (◈  2 OBSERVE · 3 THINK …) ─────────────────────────
    if "◈" in stripped and ("OBSERVE" in stripped or "THINK" in stripped):
        parts: list[tuple[str, str]] = []
        tokens = re.split(r'(\d+\s+\w+)', stripped)
        for tok in tokens:
            m = re.match(r'(\d+)\s+(OBSERVE|THINK|ACT|NOTE|MERGE)', tok)
            if m:
                col = {"OBSERVE": BLUE, "THINK": YELLOW, "ACT": GREEN,
                       "NOTE": DIM, "MERGE": MAGENTA}[m.group(2)]
                parts.append((m.group(1), col))
                parts.append((" " + m.group(2), col))
            else:
                parts.append((tok, DIM))
        return parts

    # ── Merge parent lines  ╠  ├─  └─ ───────────────────────────────────
    if stripped.startswith("  ╠"):
        # the hex id is 8 hex chars; the scope name follows
        m = re.search(r'([0-9a-f]{8})(.*)', stripped)
        if m:
            pre  = stripped[:m.start()]
            nid  = m.group(1)
            rest = m.group(2)
            return [(pre, MAGENTA), (nid, DIM), (rest, MAGENTA)]
        return [(stripped, MAGENTA)]

    # ── Node header lines  ●/◆/■/⊕/○  <id>  <KIND>  <time> ──────────────
    node_m = re.match(
        r'^(\s*)(●|◆|■|⊕|○|·)\s+([0-9a-f]{8})\s+(OBSERVE|THINK|ACT|MERGE|NOTE)\s+(\d{2}:\d{2}:\d{2})',
        stripped,
    )
    if node_m:
        sym   = node_m.group(2)
        nid   = node_m.group(3)
        kind  = node_m.group(4)
        ts    = node_m.group(5)
        col   = {"●": BLUE, "◆": YELLOW, "■": GREEN, "⊕": MAGENTA, "○": DIM}.get(sym, FG)
        pre   = node_m.group(1)
        after = stripped[node_m.end():]
        return [
            (pre,         FG),
            (sym + "  ",  col),
            (nid + "  ",  DIM),
            (kind,        col),
            (" " * (8 - len(kind)), FG),
            (ts,          DIM),
            (after,       DIM),
        ]

    # ── Content / connector lines  │  content ─────────────────────────────
    conn_m = re.match(r'^(\s*)(│|╠|╚)(.*)', stripped)
    if conn_m:
        connector = conn_m.group(2)
        rest      = conn_m.group(3)
        col       = MAGENTA if connector in ("╠", "╚") else DIM
        return [(conn_m.group(1), FG), (connector, col), (rest, DIM)]

    # ── Empty / other ─────────────────────────────────────────────────────
    return [(stripped, DIM)]


# ── Compute canvas size ────────────────────────────────────────────────────────
# Measure actual pixel width of each line using the font, not char count,
# to handle Unicode symbols (●◆■⊕╠) whose rendered width differs from len().
def line_px_width(draw, line, font):
    if not line.strip():
        return 0
    bb = draw.textbbox((0, 0), line, font=font)
    return bb[2] - bb[0]

_measure_img  = Image.new("RGB", (1, 1))
_measure_draw = ImageDraw.Draw(_measure_img)
max_line_px = max((line_px_width(_measure_draw, l, font) for l in lines), default=CW * 60)
W = PAD_X * 2 + max_line_px + PAD_X * 2   # symmetric padding
H = HEADER + PAD_Y + len(lines) * CH + PAD_Y + 20

img  = Image.new("RGB", (W, H), BG)
draw = ImageDraw.Draw(img)

# ── Title bar ─────────────────────────────────────────────────────────────────
draw.rectangle([0, 0, W, HEADER], fill="#313244")
for xoff, col in [(32, "#f38ba8"), (66, "#f9e2af"), (100, "#a6e3a1")]:
    r = 13
    draw.ellipse([xoff - r, HEADER // 2 - r, xoff + r, HEADER // 2 + r], fill=col)
title = "h5i context dag"
try:
    tf = ImageFont.truetype(FONT_PATHS[0], 26)
except Exception:
    tf = font
draw.text((W // 2, HEADER // 2), title, fill="#cdd6f4", font=tf, anchor="mm")

# ── Render each line ──────────────────────────────────────────────────────────
y = HEADER + PAD_Y
for raw_line in lines:
    segments = colorize(raw_line)
    x = PAD_X
    for text, colour in segments:
        if not text:
            continue
        draw.text((x, y), text, fill=colour, font=font)
        bb = draw.textbbox((x, y), text, font=font)
        x = bb[2]
    y += CH

# ── Footer ────────────────────────────────────────────────────────────────────
footer = "github.com/Koukyosyumei/h5i   ·   cargo install h5i-core"
try:
    ff = ImageFont.truetype(FONT_PATHS[0], 22)
except Exception:
    ff = font
draw.text((W // 2, H - 14), footer, fill=DIM, font=ff, anchor="mm")

out = "/home/koukyosyumei/Dev/h5i/assets/screenshot_h5i_dag.png"
img.save(out, "PNG", optimize=True)
print(f"Saved {W}×{H}px → {out}")
