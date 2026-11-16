"""Finish the 3D frames into the film: bloom, vignette, captions, end card.

    python compose.py OUT_DIR [--still SECONDS] [--crf 20]

Reads OUT_DIR/frames/NNNN.png (from scene.py) and writes OUT_DIR/h5i-demo.mp4
with the ffmpeg binary bundled by imageio-ffmpeg (or `ffmpeg` on PATH).
"""

import os
import shutil
import subprocess
import sys

import numpy as np
from PIL import Image, ImageDraw, ImageFilter, ImageFont

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from timeline import CAPTIONS, FPS, H, NFRAMES, SHOTS, W  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 and not sys.argv[1].startswith("--") else os.path.join(HERE, "..", "out"))
FONTS = os.environ.get("H5I_FILM_FONTS", os.path.join(OUT, "fonts"))
ASSETS = os.path.join(HERE, "..", "assets")


def opt(name, default=None):
    return sys.argv[sys.argv.index(name) + 1] if name in sys.argv else default


def font(name, size):
    return ImageFont.truetype(os.path.join(FONTS, name + ".ttf"), size)


CAP = font("SpaceGrotesk-400", 50)
TITLE = font("SpaceGrotesk-400", 84)
SUB = font("SpaceGrotesk-400", 40)
MONO = font("SpaceMono-400", 38)
SMALL = font("SpaceGrotesk-400", 30)
ACCENT = (255, 122, 89)


def smooth(x):
    x = max(0.0, min(1.0, x))
    return x * x * (3 - 2 * x)


def ramp(t, a, b):
    return smooth((t - a) / (b - a))


END0 = SHOTS[-1][1]
H, W = H, W
yy, xx = np.mgrid[0:H, 0:W]
VIGNETTE = 1 - 0.42 * np.clip((((xx - W / 2) / (W * 0.62)) ** 2 + ((yy - H / 2) / (H * 0.7)) ** 2), 0, 1) ** 1.4
VIGNETTE = VIGNETTE[..., None].astype(np.float32)
RNG = np.random.default_rng(3)


def bloom(img):
    a = np.asarray(img, dtype=np.float32) / 255.0
    bright = np.clip(a - 0.55, 0, 1) * 1.6
    small = Image.fromarray((bright * 255).astype(np.uint8)).resize((W // 4, H // 4), Image.BILINEAR)
    acc = np.zeros_like(a)
    for r, w in ((3, 0.55), (10, 0.45), (26, 0.35)):
        b = small.filter(ImageFilter.GaussianBlur(r)).resize((W, H), Image.BILINEAR)
        acc += np.asarray(b, dtype=np.float32) / 255.0 * w
    out = 1 - (1 - a) * (1 - np.clip(acc, 0, 1))  # screen blend
    out *= VIGNETTE
    out += RNG.normal(0, 0.005, (H // 2, W // 2, 1)).repeat(2, 0).repeat(2, 1).astype(np.float32)
    return Image.fromarray((np.clip(out, 0, 1) * 255).astype(np.uint8))


def caption(img, t):
    for a, b, text in CAPTIONS:
        if a - 0.01 <= t <= b:
            k = ramp(t, a, a + 0.35) * (1 - ramp(t, b - 0.35, b))
            if k <= 0:
                continue
            layer = Image.new("RGBA", (W, H), (0, 0, 0, 0))
            d = ImageDraw.Draw(layer)
            f = CAP
            while d.textlength(text, font=f) > W - 260 and f.size > 34:
                f = font("SpaceGrotesk-400", f.size - 2)  # long captions shrink to fit
            tw = d.textlength(text, font=f)
            x, y = (W - tw) / 2, H - 150 + (1 - k) * 10
            pad = 26
            d.rounded_rectangle([x - pad, y - 16, x + tw + pad, y + 70], radius=12, fill=(0, 0, 0, int(150 * k)))
            d.rectangle([x - pad, y - 16, x - pad + 5, y + 70], fill=(*ACCENT, int(255 * k)))
            d.text((x, y + (CAP.size - f.size) * 0.6), text, font=f, fill=(240, 240, 244, int(255 * k)))
            img = Image.alpha_composite(img.convert("RGBA"), layer).convert("RGB")
    return img


LOGO = Image.open(os.path.join(ASSETS, "logo.png")).convert("RGB").resize((150, 150), Image.LANCZOS)


def endcard(img, t):
    k = ramp(t, END0 + 0.4, END0 + 1.6)
    if k <= 0:
        return img
    dark = img.filter(ImageFilter.GaussianBlur(12 * k))
    dark = Image.blend(img, dark, k)
    a = np.asarray(dark, dtype=np.float32) * (1 - 0.72 * k)
    base = Image.fromarray(a.astype(np.uint8)).convert("RGBA")
    layer = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    d = ImageDraw.Draw(layer)

    def center(text, y, f, col, t0):
        kk = ramp(t, t0, t0 + 0.6)
        tw = d.textlength(text, font=f)
        d.text(((W - tw) / 2, y + (1 - kk) * 14), text, font=f, fill=(*col, int(255 * kk)))
        return kk

    kl = ramp(t, END0 + 1.0, END0 + 1.6)
    if kl > 0:
        m = Image.new("L", LOGO.size, 0)
        ImageDraw.Draw(m).rounded_rectangle([0, 0, *LOGO.size], radius=28, fill=int(255 * kl))
        layer.paste(LOGO, ((W - LOGO.width) // 2, 200), m)
    center("Build and Verify Your Web Apps.", 400, TITLE, (240, 240, 244), END0 + 1.3)
    center("h5i — The Verifiable Web Framework for Rust", 530, SUB, (190, 190, 200), END0 + 1.8)
    kc = ramp(t, END0 + 2.4, END0 + 3.0)
    if kc > 0:
        cmd = "cargo add h5i-app"
        tw = d.textlength("$ " + cmd, font=MONO)
        x, y = (W - tw) / 2, 650
        d.rounded_rectangle([x - 34, y - 22, x + tw + 34, y + 66], radius=14,
                            fill=(16, 16, 22, int(235 * kc)), outline=(*ACCENT, int(255 * kc)), width=2)
        d.text((x, y), "$ ", font=MONO, fill=(*ACCENT, int(255 * kc)))
        d.text((x + d.textlength("$ ", font=MONO), y), cmd, font=MONO, fill=(240, 240, 244, int(255 * kc)))
    center("h5i.dev  ·  github.com/h5i-dev/h5i", 790, SMALL, (150, 150, 160), END0 + 3.0)
    return Image.alpha_composite(base, layer).convert("RGB")


def frame(f):
    t = (f - 1) / FPS
    img = Image.open(os.path.join(OUT, "frames", f"{f:04d}.png")).convert("RGB")
    if img.size != (W, H):
        img = img.resize((W, H), Image.LANCZOS)
    img = bloom(img)
    # fade in from black, and dip briefly at the cut into the network space
    fade = ramp(t, 0.0, 0.8)
    for _, a, _ in SHOTS[1:]:
        fade *= 0.55 + 0.45 * min(1.0, abs(t - a) / 0.08)
    if fade < 1:
        img = Image.fromarray((np.asarray(img, dtype=np.float32) * fade).astype(np.uint8))
    img = caption(img, t)
    img = endcard(img, t)
    return img


def ffmpeg():
    try:
        import imageio_ffmpeg
        return imageio_ffmpeg.get_ffmpeg_exe()
    except ImportError:
        return shutil.which("ffmpeg")


def main():
    if opt("--still"):
        ts = [float(s) for s in opt("--still").split(",")]
        for s in ts:
            p = os.path.join(OUT, "stills", f"final-{s:05.1f}.png")
            frame(int(round(s * FPS)) + 1).save(p)
            print("wrote", p)
        return
    out = os.path.join(OUT, "h5i-demo.mp4")
    crf = opt("--crf", "23")
    wav = os.path.join(OUT, "soundtrack.wav")
    audio_in = ["-i", wav] if os.path.exists(wav) else []
    audio_out = ["-c:a", "aac", "-b:a", "160k", "-shortest"] if audio_in else []
    p = subprocess.Popen([ffmpeg(), "-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgb24",
                          "-s", f"{W}x{H}", "-r", str(FPS), "-i", "-", *audio_in, *audio_out,
                          "-c:v", "libx264", "-preset", "slow", "-crf", crf, "-pix_fmt", "yuv420p",
                          "-movflags", "+faststart", out], stdin=subprocess.PIPE)
    for f in range(1, NFRAMES + 1):
        p.stdin.write(frame(f).tobytes())
        if f % 48 == 0:
            print(f"{f}/{NFRAMES}", flush=True)
    p.stdin.close()
    p.wait()
    print("wrote", out)
    poster = os.path.join(OUT, "poster.png")
    frame(int(round(13.0 * FPS)) + 1).save(poster)


if __name__ == "__main__":
    main()
