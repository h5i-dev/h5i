"""Shared timeline for the h5i demo film.

Every other script (screens.py, scene.py, compose.py) reads its timing from
here, so a beat moves in one place. Times are in seconds; frames are 1-based
like Blender's.
"""

FPS = 24
W, H = 1920, 1080

# (name, start, end) in seconds. Shots are hard cuts unless compose.py fades.
SHOTS = [
    ("engineer_wide", 0.0, 5.0),    # dark room, hooded engineer at a laptop
    ("laptop_code", 5.0, 14.0),     # over the shoulder: Rust -> Lean -> proven
    ("hacker", 14.0, 21.0),         # red room: the attack is typed
    ("network", 21.0, 31.0),        # requests hit the proof shield and shatter
    ("hacker_fail", 31.0, 36.0),    # the attacker's screen fills with 403s
    ("engineer_calm", 36.0, 42.0),  # engineer ships the proven kernel
    ("endcard", 42.0, 48.0),        # title card
]
TOTAL = SHOTS[-1][2]
NFRAMES = round(TOTAL * FPS)

# Lower-third captions: (start, end, text). compose.py fades them in and out.
CAPTIONS = [
    (0.8, 4.6, "Every web app is a target."),
    (5.6, 9.4, "h5i extracts your Rust kernel into Lean 4…"),
    (9.6, 13.7, "…and proves what it must do, such as no panics or unauthorized writes."),
    (14.6, 20.6, "Someone will try to break it."),
    (21.6, 26.0, "Every request runs through the proven kernel."),
    (26.2, 30.7, "For example, only the author or a moderator can delete a post."),
    (31.6, 35.7, "A proof mathematically guarantees the correctness of the implementation."),
    (36.6, 41.6, "Proven in Lean 4. Shipped as Rust, to servers and microcontrollers."),
]


def frame(t):
    """Seconds -> 1-based frame number."""
    return int(round(t * FPS)) + 1


def shot(name):
    for n, a, b in SHOTS:
        if n == name:
            return a, b
    raise KeyError(name)
