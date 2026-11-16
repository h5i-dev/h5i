"""Render the in-world screen textures as PNG sequences, one per screen.

    python screens.py OUT_DIR

writes OUT_DIR/{laptop,hacker,hackside}/NNNN.png, one image per film frame.
scene.py maps each sequence onto a screen's emission shader. Frames whose
content is unchanged are hard links to the previous file.

All terminal output is scripted for the film. The Rust and Lean shown are
taken from examples/app/board.
"""

import os
import re
import sys

from PIL import Image, ImageDraw, ImageFont

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from timeline import FPS, NFRAMES  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
FONTS = os.environ.get("H5I_FILM_FONTS", os.path.join(HERE, "..", "out", "fonts"))


def font(name, size):
    return ImageFont.truetype(os.path.join(FONTS, name + ".ttf"), size)


MONO = font("SpaceMono-400", 25)
MONO_B = font("SpaceMono-700", 25)
MONO_S = font("SpaceMono-400", 21)
MONO_L = font("SpaceMono-700", 44)
SANS = font("SpaceGrotesk-400", 22)

BG = (11, 11, 16)
PANEL = (17, 17, 24)
BAR = (24, 24, 33)
TEXT = (230, 230, 236)
DIM = (130, 130, 142)
FAINT = (80, 80, 92)
ACCENT = (255, 122, 89)
OK = (52, 211, 153)
DANGER = (255, 98, 88)
REF = (167, 139, 250)
KW = (255, 160, 120)
TY = (125, 196, 255)
STR = (186, 230, 160)

# --------------------------------------------------------------------------
# source text (from examples/app/board)

RUST = """\
fn delete(user: u64, s: &Snapshot, id: u64) -> Outcome {
    match find_post(&s.posts, id) {
        None => Err(Error::NotFound),
        Some(p) => {
            if p.author == user || is_moderator(&s.moderators, user) {
                Ok((one(Write::DelPost(id)), Reply::Done))
            } else {
                Err(Error::Forbidden)
            }
        }
    }
}"""

LEAN = """\
theorem authorized (a : Principal) (s : Snapshot) (c : Command) ws r
    (hinv : Inv (Snapshot.toSt s))
    (h : transition a s c = .ok (.Ok (ws, r))) :
    ∀ w ∈ ws.val, allowed (Snapshot.toSt s) a.user.val w := by
  rcases writes_of a s c ws r h with ...

def allowed (s : St) (u : Nat) : Write → Prop
  | .DelPost id =>
    ∃ q, findPost s id.val = some q ∧ (q.author.val = u ∨ isMod s u)"""

RUST_KW = r"\b(fn|match|if|else|let|mut|pub|return|struct|enum|impl)\b"
LEAN_KW = r"\b(theorem|def|by|with|match|fun|let|if|then|else|rcases)\b"
TYPES = r"\b([A-Z][A-Za-z0-9_]*)\b"


def highlight(line, kw):
    """Split a line into (text, colour) runs with a tiny regex highlighter."""
    spans = [(m.start(), m.end(), KW) for m in re.finditer(kw, line)]
    spans += [(m.start(), m.end(), TY) for m in re.finditer(TYPES, line)]
    spans += [(m.start(), m.end(), REF) for m in re.finditer(r"[∀∃∈∧∨→←⟨⟩]", line)]
    spans += [(m.start(), m.end(), STR) for m in re.finditer(r'"[^"]*"', line)]
    spans.sort()
    out, i = [], 0
    for a, b, c in spans:
        if a < i:
            continue
        if a > i:
            out.append((line[i:a], TEXT))
        out.append((line[a:b], c))
        i = b
    if i < len(line):
        out.append((line[i:], TEXT))
    return out


FALLBACK = os.environ.get("H5I_FILM_SYMBOLS", "/usr/share/fonts/dejavu-sans-mono-fonts/DejaVuSansMono.ttf")
_fallback = {}
_cmaps = {}


def has_glyph(f, ch):
    key = f.path
    if key not in _cmaps:
        from fontTools.ttLib import TTFont
        _cmaps[key] = set(TTFont(key).getBestCmap())
    return ord(ch) in _cmaps[key]


def sym(f):
    """DejaVu Sans Mono at the same size, for symbols Space Mono lacks."""
    if f.size not in _fallback:
        _fallback[f.size] = ImageFont.truetype(FALLBACK, f.size)
    return _fallback[f.size]


def draw_text(d, x, y, text, f, col):
    """Draw text with per-glyph fallback, keeping the monospace grid."""
    adv = d.textlength("M", font=f)
    for ch in text:
        g = f if has_glyph(f, ch) or ch == " " else sym(f)
        if g is f:
            d.text((x, y), ch, font=f, fill=col)
        else:
            d.text((x, y + f.size * 0.08), ch, font=g, fill=col)
        x += adv
    return x


def draw_runs(d, x, y, runs, f=MONO):
    for text, col in runs:
        x = draw_text(d, x, y, text, f, col)
    return x


def clamp(v, a=0.0, b=1.0):
    return max(a, min(b, v))


def typed(text, t, t0, cps=26):
    """The prefix of `text` typed by time t, starting at t0."""
    n = int(clamp((t - t0) * cps, 0, len(text)))
    return text[:n], n >= len(text)


def caret(d, x, y, t, col=TEXT, h=28):
    if int(t * 2.2) % 2 == 0:
        d.rectangle([x + 2, y + 3, x + 14, y + h], fill=col)


def window_bar(d, w, title, tabs=(), active=0, col=BAR):
    d.rectangle([0, 0, w, 46], fill=col)
    for i, c in enumerate([(255, 95, 87), (254, 188, 46), (40, 200, 64)]):
        d.ellipse([18 + i * 26, 15, 34 + i * 26, 31], fill=c)
    x = 110
    for i, tab in enumerate(tabs):
        tw = d.textlength(tab, font=SANS) + 36
        if i == active:
            d.rectangle([x, 6, x + tw, 46], fill=PANEL)
            d.rectangle([x, 6, x + tw, 8], fill=ACCENT)
        d.text((x + 18, 12), tab, font=SANS, fill=TEXT if i == active else DIM)
        x += tw + 4
    if title:
        d.text((w - d.textlength(title, font=SANS) - 22, 12), title, font=SANS, fill=DIM)


# --------------------------------------------------------------------------
# laptop: editor (Rust above, generated Lean below) + terminal on the right

LW, LH = 1600, 1000

# (time typed, command, [(delay after typed, runs...)])
LAPTOP_TERM = [
    (6.0, "h5i app extract", [
        (0.4, [("  charon  ", DIM), ("kernel/src/lib.rs", TEXT), ("  →  LLBC", DIM)]),
        (0.8, [("  aeneas  ", DIM), ("LLBC  →  ", DIM), ("BoardKernel.lean", TEXT)]),
        (1.2, [("  ✓ ", OK), ("78 definitions extracted", TEXT)]),
    ]),
    (9.7, "h5i app prove", [
        (0.5, [("  lean 4  ", DIM), ("checking proofs/Theorems.lean", TEXT)]),
        (1.0, [("  ✓ ", OK), ("transition_total", TEXT)]),
        (1.3, [("  ✓ ", OK), ("panic_free", TEXT)]),
        (1.6, [("  ✓ ", OK), ("writes_of", TEXT)]),
        (1.9, [("  ✓ ", OK), ("authorized", ACCENT), ("   ∀ w ∈ ws, allowed s u w", REF)]),
        (2.2, [("  ✓ ", OK), ("author_kept", TEXT)]),
        (2.5, [("  ✓ ", OK), ("reachable_inv", TEXT)]),
        (3.0, [("  PROVEN", OK), ("  17 theorems · 0 sorry", DIM)]),
    ]),
    (37.0, "fly deploy", [
        (0.5, [("  building ", DIM), ("board (release)", TEXT)]),
        (1.0, [("  kernel  ", DIM), ("same Rust that Lean proved", TEXT)]),
        (1.5, [("  ✓ ", OK), ("live at ", TEXT), ("board.example", ACCENT)]),
]),
]


def term_lines(script, t, cps=22):
    """Terminal lines visible at time t, plus whether the caret is on a prompt."""
    lines = []
    for t0, cmd, outs in script:
        if t < t0:
            break
        txt, done = typed(cmd, t, t0, cps)
        lines.append(("cmd", txt, done))
        if not done:
            return lines, True
        t_done = t0 + len(cmd) / cps
        for dt, runs in outs:
            if t >= t_done + dt:
                lines.append(("out", runs, True))
    return lines, False


def laptop(t):
    im = Image.new("RGB", (LW, LH), BG)
    d = ImageDraw.Draw(im)
    window_bar(d, LW, "board · h5i-app", ("lib.rs", "Theorems.lean"), 0 if t < 7.5 else 1)

    # editor pane
    ex, ey, ew = 0, 46, 985
    d.rectangle([ex, ey, ex + ew, LH], fill=PANEL)
    d.text((24, ey + 16), "kernel/src/lib.rs", font=MONO_S, fill=DIM)
    rust = RUST.split("\n")
    # During the opening shot the permission check is still being typed.
    target = 4
    y = ey + 56
    for i, line in enumerate(rust):
        d.text((24, y), f"{148 + i:>3}", font=MONO_S, fill=FAINT)
        shown = line
        if i == target and t < 4.6:
            shown, _ = typed(line, t, 0.6, 14)
        elif i > target and t < 4.6:
            shown = ""
        x = draw_runs(d, 84, y, highlight(shown, RUST_KW), MONO_S)
        if i == target and t < 4.6:
            caret(d, x, y, t, ACCENT, 24)
        y += 31

    # generated Lean appears after extract
    ly = y + 22
    d.line([0, ly - 10, ew, ly - 10], fill=BAR, width=2)
    if t >= 7.4:
        a = clamp((t - 7.4) / 0.6)
        d.text((24, ly + 4), "proofs/Theorems.lean", font=MONO_S, fill=DIM)
        lean = LEAN.split("\n")
        n = int(clamp((t - 7.4) * 9, 0, len(lean)))
        y2 = ly + 44
        for i, line in enumerate(lean[:n]):
            runs = highlight(line, LEAN_KW)
            if a < 1:
                runs = [(s, tuple(int(c * a + b * (1 - a)) for c, b in zip(col, PANEL))) for s, col in runs]
            draw_runs(d, 24, y2, runs, MONO_S)
            y2 += 31
        # highlight the authorized theorem once it is proven
        if t >= 12.3:
            k = clamp((t - 12.3) / 0.5)
            col = tuple(int(OK[j] * k + PANEL[j] * (1 - k)) for j in range(3))
            d.rectangle([12, ly + 40, ew - 12, ly + 40 + 31 * 4 + 6], outline=col, width=3)

    # terminal
    tx = ew
    d.rectangle([tx, 46, LW, LH], fill=(8, 8, 12))
    draw_text(d, tx + 24, 62, "zsh — board", MONO_S, DIM)
    # The terminal is cleared before the last command.
    lines, typing = term_lines(LAPTOP_TERM if t < 36.5 else LAPTOP_TERM[2:], t)
    y = 110
    wrap = 30
    for kind, body, done in lines[-wrap:]:
        if kind == "cmd":
            x = draw_runs(d, tx + 24, y, [("$ ", ACCENT), (body, TEXT)], MONO_S)
            if not done:
                caret(d, x, y, t, TEXT, 24)
        else:
            draw_runs(d, tx + 24, y, body, MONO_S)
        y += 34
    if not typing:
        x = draw_runs(d, tx + 24, y, [("$ ", ACCENT)], MONO_S)
        caret(d, x, y, t, TEXT, 24)

    # big proven badge
    if 12.6 <= t:
        k = clamp((t - 12.6) / 0.4)
        bx, by = tx + 40, LH - 190
        col = tuple(int(OK[j] * k + 8 * (1 - k)) for j in range(3))
        d.rounded_rectangle([bx, by, LW - 40, by + 140], radius=18, outline=col, width=4)
        draw_text(d, bx + 30, by + 22, "✓ authorized", MONO_L, col)
        draw_text(d, bx + 32, by + 88, "proven in Lean 4 · kernel writes", MONO_S,
                  tuple(int(DIM[j] * k + 8 * (1 - k)) for j in range(3)))
    return im, (round(t * FPS) if 0.6 <= t <= 14 or 36.5 <= t <= 41 else int(t > 14))


# --------------------------------------------------------------------------
# hacker main screen: the attack

HW, HH = 1600, 900
TARGET = "https://board.example"

HACK_TERM = [
    (14.5, f"curl -X DELETE {TARGET}/api/posts/42 -b session=$BOB", [
        (0.4, [("HTTP/1.1 ", DIM), ("403 Forbidden", DANGER)]),
    ]),
    (16.9, f"curl -X DELETE {TARGET}/api/posts/42 -H 'X-Role: moderator'", [
        (0.4, [("HTTP/1.1 ", DIM), ("403 Forbidden", DANGER)]),
    ]),
    (19.1, "./fuzz --route /api/posts/{id} --mutate actor,role,id -n 4096", [
        (0.3, [("[*] ", ACCENT), ("4096 mutations queued", TEXT)]),
    ]),
]


def fuzz_line(i):
    ids = (i * 7919) % 997
    mut = ["actor=bob", "role=moderator", "actor=0", "id=-1", "id=18446744073709551615",
           "actor=alice'--", "session=replayed", "id=42 actor=null", "role[]=admin"][i % 9]
    verb = ["DELETE", "PUT", "DELETE", "POST"][i % 4]
    return [(f"[{i:04d}] ", FAINT), (f"{verb:<6} /api/posts/{ids:<4} ", TEXT),
            (f"{mut:<26}", DIM), ("403", DANGER)]


def hacker(t):
    im = Image.new("RGB", (HW, HH), (10, 4, 6))
    d = ImageDraw.Draw(im)
    window_bar(d, HW, "kali — ssh", ("attack.sh",), 0, (30, 12, 14))
    y = 70
    if t < 31:
        lines, typing = term_lines(HACK_TERM, t, 30)
        for kind, body, done in lines:
            if kind == "cmd":
                x = draw_runs(d, 24, y, [("# ", DANGER), (body, TEXT)], MONO_S)
                if not done:
                    caret(d, x, y, t, DANGER, 24)
            else:
                draw_runs(d, 24, y, body, MONO_S)
            y += 36
        if not typing and t > 20:
            n = int((t - 20.3) * 30)
            for i in range(max(0, n - 12), max(0, n)):
                draw_runs(d, 24, y, fuzz_line(i), MONO_S)
                y += 32
        key = round(t * FPS)
    else:
        # the wall of refusals, then the summary
        n = min(4096, int((t - 31) ** 2 * 260 + (t - 31) * 120))
        rows = 21
        for i in range(max(0, n - rows), n):
            draw_runs(d, 24, y, fuzz_line(i), MONO_S)
            y += 34
        if t > 33.6:
            k = clamp((t - 33.6) / 0.35)
            d.rectangle([0, HH - 230, HW, HH], fill=(10, 4, 6))
            col = tuple(int(DANGER[j] * k + 10 * (1 - k)) for j in range(3))
            d.rectangle([40, HH - 210, HW - 40, HH - 40], outline=col, width=4)
            d.text((80, HH - 190), f"4096 / 4096 requests refused", font=MONO_L, fill=col)
            d.text((82, HH - 120), "0 bypasses found", font=MONO_L, fill=col)
        key = round(t * FPS)
    return im, key


def hackside(t):
    """Secondary monitor: a scrolling hex dump and an attempt counter."""
    im = Image.new("RGB", (HW, HH), (8, 3, 5))
    d = ImageDraw.Draw(im)
    window_bar(d, HW, "", ("traffic",), 0, (30, 12, 14))
    off = int(t * 9)
    y = 66
    for r in range(22):
        row = off + r
        hexs = " ".join(f"{(row * 31 + c * 17 + (row * c) % 13) % 256:02x}" for c in range(16))
        d.text((24, y), f"{row * 16:08x}  {hexs}", font=MONO_S,
               fill=(150, 50, 50) if (row % 7) else DANGER)
        y += 34
    attempts = 0 if t < 20.3 else min(4096, int((t - 20.3) * 30 + max(0, t - 31) ** 2 * 260))
    d.rectangle([HW - 520, HH - 170, HW - 30, HH - 30], fill=(20, 6, 8), outline=DANGER, width=3)
    d.text((HW - 495, HH - 155), "ATTEMPTS", font=MONO_S, fill=DIM)
    d.text((HW - 495, HH - 120), f"{attempts:>5}", font=MONO_L, fill=DANGER)
    d.text((HW - 270, HH - 155), "BYPASS", font=MONO_S, fill=DIM)
    d.text((HW - 270, HH - 120), "    0", font=MONO_L, fill=DANGER)
    return im, int(t * 9) * 10000 + attempts


SCREENS = {"laptop": laptop, "hacker": hacker, "hackside": hackside}


def main(out):
    for name, fn in SCREENS.items():
        d = os.path.join(out, name)
        os.makedirs(d, exist_ok=True)
        last_key, last_path = None, None
        for f in range(1, NFRAMES + 1):
            t = (f - 1) / FPS
            path = os.path.join(d, f"{f:04d}.png")
            im, key = fn(t)
            if os.path.exists(path):
                os.remove(path)  # never write through a hard link from a previous run
            if key == last_key and last_path:
                os.link(last_path, path)
            else:
                im.save(path, compress_level=1)
                last_key, last_path = key, path
        print("wrote", d)


if __name__ == "__main__":
    if len(sys.argv) > 2:  # stills: python screens.py OUT name t
        im, _ = SCREENS[sys.argv[2]](float(sys.argv[3]))
        im.save(os.path.join(sys.argv[1], f"{sys.argv[2]}-{sys.argv[3]}.png"))
    else:
        main(sys.argv[1])
