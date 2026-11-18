"""Synthesize the film's soundtrack: a drone score and sound effects.

    python audio.py OUT_DIR

Reads OUT_DIR/packets.json (scene.py --dump-packets) for impact times and
writes OUT_DIR/soundtrack.wav (48 kHz stereo). Everything is generated
here, so there is nothing to license.
"""

import json
import os
import sys
import wave

import numpy as np
from scipy import signal

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from timeline import TOTAL  # noqa: E402

OUT = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(__file__), "..", "out"))
SR = 48000
N = int(TOTAL * SR)
t = np.arange(N) / SR
rng = np.random.default_rng(11)
L = np.zeros(N)
R = np.zeros(N)


def env(points):
    """Piecewise-linear envelope over the whole film: [(time, value), ...]."""
    ts, vs = zip(*points)
    return np.interp(t, ts, vs)


def add(x, at, gain=1.0, pan=0.0):
    i = int(at * SR)
    if i >= N:
        return
    x = x[: N - i] * gain
    L[i:i + len(x)] += x * np.sqrt(0.5 * (1 - pan))
    R[i:i + len(x)] += x * np.sqrt(0.5 * (1 + pan))


def bandpass(x, lo, hi, order=2):
    sos = signal.butter(order, [lo, hi], btype="band", fs=SR, output="sos")
    return signal.sosfilt(sos, x)


def lowpass(x, f, order=2):
    return signal.sosfilt(signal.butter(order, f, btype="low", fs=SR, output="sos"), x)


def hz(note):
    return 440.0 * 2 ** ((note - 69) / 12)


# ------------------------------------------------------------------ score

def pad(notes, start, end, gain, bright=600, attack=1.5, release=1.5, detune=0.004):
    """A soft detuned-saw pad, low-passed, between start and end seconds."""
    i0, i1 = int(start * SR), int(min(end + release, TOTAL) * SR)
    tt = t[i0:i1] - start
    x = np.zeros_like(tt)
    for n in notes:
        f = hz(n)
        for d in (-detune, 0, detune):
            ph = rng.uniform(0, 1)
            x += signal.sawtooth(2 * np.pi * f * (1 + d) * tt + ph * 6.28) / 3
    x = lowpass(x, bright, 2) / len(notes)
    e = np.minimum(1, tt / attack) * np.clip((end + release - start - tt) / release, 0, 1)
    out = x * e * gain
    L[i0:i1] += out
    R[i0:i1] += lowpass(np.roll(out, 240), bright * 1.2)


# A minor: calm night, tension in the attacker's room, resolve to A major.
pad([45, 52, 57, 60], 0.0, 17.0, 0.22, bright=500, attack=2.5)          # Am
pad([45, 52, 57, 59], 12.0, 17.0, 0.10, bright=900, attack=2.0)          # lift as the proof lands
pad([46, 53, 58, 61], 17.0, 24.0, 0.20, bright=420, attack=0.4, release=0.6)  # Bbm: unease
pad([33, 34], 17.0, 34.0, 0.16, bright=160, attack=1.0, release=0.8)    # low minor-second rumble
pad([45, 52, 57, 60, 64], 24.0, 34.0, 0.18, bright=1200, attack=0.3, release=0.6)
pad([46, 53, 58, 61], 34.0, 39.0, 0.16, bright=380, attack=0.3, release=0.8)
pad([45, 52, 57, 61, 64], 39.0, 51.0, 0.24, bright=900, attack=1.2, release=2.5)  # A major
pad([33, 45], 46.0, 51.0, 0.18, bright=200, attack=0.6, release=2.0)

# pulse: a muted sub kick under the network sequence, accelerating
beat = 24.0
while beat < 33.8:
    k = np.exp(-np.arange(int(0.35 * SR)) / SR * 14)
    f = 55 * (1 + 2.2 * np.exp(-np.arange(len(k)) / SR * 40))
    add(np.sin(2 * np.pi * np.cumsum(f) / SR) * k, beat, 0.38)
    beat += 0.5 if beat < 28.5 else 0.25

# room tone
air = lowpass(rng.normal(0, 1, N), 900) * 0.012
L += air
R += lowpass(rng.normal(0, 1, N), 900) * 0.012

# ------------------------------------------------------------------ effects


def click(gain, bright=4000):
    n = int(0.03 * SR)
    x = rng.normal(0, 1, n) * np.exp(-np.arange(n) / SR * 260)
    return bandpass(x, bright * 0.5, min(bright * 2, 20000)) * gain


def typing(a, b, rate, gain, bright, pan):
    tt = a
    while tt < b:
        add(click(gain * rng.uniform(0.6, 1.0), bright * rng.uniform(0.8, 1.2)), tt, 1.0, pan)
        tt += rng.exponential(1 / rate) + 0.03


for a, b in [(3.6, 7.6), (9.0, 9.7), (12.7, 13.3), (39.8, 40.3)]:
    typing(a, b, 9, 0.5, 3500, -0.2)
for a, b in [(17.5, 19.5), (19.9, 21.9), (22.1, 23.9)]:
    typing(a, b, 13, 0.7, 2600, 0.2)


def bell(f, dur, gain):
    n = int(dur * SR)
    tt = np.arange(n) / SR
    x = sum(np.sin(2 * np.pi * f * m * tt) * np.exp(-tt * d) * a
            for m, d, a in [(1, 3, 1), (2.01, 5, 0.4), (3.0, 8, 0.2), (4.2, 12, 0.1)])
    return x * gain * np.minimum(1, tt / 0.004)


def blip(f, gain, dur=0.08):
    n = int(dur * SR)
    tt = np.arange(n) / SR
    return np.sign(np.sin(2 * np.pi * f * tt)) * np.exp(-tt * 40) * gain * 0.4


# terminal: extract done, proof ticks, then the proven chime
add(blip(1320, 0.18), 11.0)
for i, dt in enumerate([1.0, 1.3, 1.6, 1.9, 2.2, 2.5]):
    add(blip(1500 + 60 * i, 0.12), 13.3 + dt)
for i, n in enumerate([69, 73, 76, 81]):
    add(bell(hz(n + 12), 2.5, 0.22), 15.6 + i * 0.09, 1.0, -0.3 + 0.2 * i)
# deploy confirmation
for i, n in enumerate([76, 81]):
    add(bell(hz(n + 12), 2.0, 0.16), 41.15 + i * 0.1)
# the closing: the red tags come back with the opening's pings, then the green
# wave goes out (WAVE0 in scene.py) and each device chimes as it turns green.
# The times are when the wave reaches each one: laptop, speaker, vacuum, rack.
for i, at in enumerate((42.75, 42.85, 42.95, 43.05)):
    add(bell(hz(86 + 3 * i), 0.8, 0.05), at, 1.0, (-0.6, -0.5, 0.0, 0.2)[i])
for i, (at, pan) in enumerate([(43.76, 0.2), (44.07, -0.1), (44.65, -0.5), (45.25, -0.7)]):
    add(bell(hz([76, 81, 85, 88][i] + 12), 2.4, 0.15), at, 1.0, pan)
# attacker: refusal buzzes
for at in (17.9 + 1.94, 20.3 + 2.2):
    add(blip(220, 0.35, 0.18), at, 1.0, 0.3)


def whoosh(dur, gain, f0=300, f1=3000):
    n = int(dur * SR)
    x = rng.normal(0, 1, n)
    out = np.zeros(n)
    seg = 1024
    for i in range(0, n, seg):
        k = i / n
        f = f0 * (f1 / f0) ** k
        out[i:i + seg] = bandpass(x[i:i + seg + 2048], f * 0.7, min(f * 1.4, 20000))[:len(out[i:i + seg])]
    e = np.sin(np.pi * np.arange(n) / n) ** 2
    return out * e * gain


def impact(gain):
    n = int(1.4 * SR)
    tt = np.arange(n) / SR
    ting = sum(np.sin(2 * np.pi * f * tt) * np.exp(-tt * d) for f, d in [(1870, 6), (2410, 7), (3170, 9), (4630, 12)]) / 4
    thump = np.sin(2 * np.pi * 70 * tt * (1 + np.exp(-tt * 30))) * np.exp(-tt * 9)
    crack = bandpass(rng.normal(0, 1, n), 1500, 9000) * np.exp(-tt * 45)
    return (ting * 0.5 + thump * 0.9 + crack * 0.5) * gain


# opening: the robot vacuum's motor while it is in view, a soft ping as each
# device's route tag appears (times match TAG_AT in scene.py), and a low hit
# when the tags turn red with "Every one of them is a target."
n = int(5.0 * SR)
tt = np.arange(n) / SR
motor = bandpass(rng.normal(0, 1, n), 180, 900) * 0.5 + np.sin(2 * np.pi * 118 * tt) * 0.15
add(motor * np.clip(tt / 0.8, 0, 1) * np.clip((5.0 - tt) / 1.4, 0, 1), 0.0, 0.10, -0.4)
for i, at in enumerate((0.9, 1.9, 3.0, 3.8)):
    add(bell(hz(86 + 3 * i), 0.8, 0.07), at, 1.0, (-0.5, -0.6, 0.0, 0.2)[i])
add(impact(0.22), 5.25, 1.0, 0.0)

add(whoosh(0.7, 0.25, 2000, 200), 23.75)  # cut into the network space
add(whoosh(2.2, 0.16, 250, 1800), 43.5, 1.0, -0.2)  # the green wave spreads
with open(os.path.join(OUT, "packets.json")) as fh:
    packets = json.load(fh)
for i, p in enumerate(packets):
    pan = rng.uniform(-0.5, 0.2)
    if p["hero"]:
        add(whoosh(p["t1"] - p["t0"], 0.18), p["t0"], 1.0, -0.6)
        add(impact(0.75), p["t1"], 1.0, -0.1)
    else:
        add(impact(0.22), p["t1"], 1.0, pan)

# the attacker's screen floods; a low stinger on the summary
for i in range(40):
    add(blip(200 + (i % 3) * 20, 0.05, 0.05), 34.2 + (i / 40) ** 0.6 * 2.3, 1.0, 0.3)
add(impact(0.6), 36.6)
# end card: a deep hit and a bright shimmer
n = int(4 * SR)
tt = np.arange(n) / SR
add(np.sin(2 * np.pi * 41 * tt) * np.exp(-tt * 1.2) * 0.5, 47.3)
for i, nn in enumerate([69, 76, 81, 85, 88]):
    add(bell(hz(nn + 12), 3.5, 0.12), 47.3 + i * 0.06, 1.0, -0.4 + 0.2 * i)

# ------------------------------------------------------------------ master

# fade the very start and end
fade = env([(0, 0), (0.6, 1), (TOTAL - 2.2, 1), (TOTAL, 0)])
mix = np.stack([L, R], 1) * fade[:, None]
# gentle glue compression: soft clip after normalising to -1 dBFS peaks
mix /= np.max(np.abs(mix)) + 1e-9
mix = np.tanh(mix * 2.4) / np.tanh(2.4) * 0.89
path = os.path.join(OUT, "soundtrack.wav")
with wave.open(path, "wb") as w:
    w.setnchannels(2)
    w.setsampwidth(2)
    w.setframerate(SR)
    w.writeframes((mix * 32767).astype(np.int16).tobytes())
print("wrote", path)
