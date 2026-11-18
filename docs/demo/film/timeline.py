"""Shared timeline for the h5i demo film.

Every other script (screens.py, scene.py, compose.py) reads its timing from
here, so a beat moves in one place. Times are in seconds; frames are 1-based
like Blender's.
"""

FPS = 24
W, H = 1920, 1080

# (name, start, end) in seconds. Shots are hard cuts unless compose.py fades.
SHOTS = [
    ("opening", 0.0, 8.0),          # the engineer's room: a robot vacuum, a smart speaker,
                                    # a server rack, each serving HTTP; then the engineer
    ("laptop_code", 8.0, 17.0),     # over the shoulder: Rust -> Lean -> proven
    ("hacker", 17.0, 24.0),         # red room: the attack is typed
    ("network", 24.0, 34.0),        # requests hit the proof shield and shatter
    ("hacker_fail", 34.0, 39.0),    # the attacker's screen fills with 403s
    ("engineer_calm", 39.0, 46.0),  # engineer ships the proven kernel; the camera pulls back to
                                    # the opening's devices, and their tags turn from red to green
    ("endcard", 46.0, 51.0),        # title card
]
TOTAL = SHOTS[-1][2]
NFRAMES = round(TOTAL * FPS)

# Lower-third captions: (start, end, text). compose.py fades them in and out.
CAPTIONS = [
    (0.8, 4.9, "Web apps run everywhere, from servers to the devices around you."),
    (5.3, 7.8, "Every one of them is a target."),
    (8.6, 12.4, "h5i extracts your Rust kernel into Lean 4…"),
    (12.6, 16.7, "…and proves what it must do, such as no panics or unauthorized writes."),
    (17.6, 23.6, "Someone will try to break it."),
    (24.6, 29.0, "Every request runs through the proven kernel."),
    (29.2, 33.7, "For example, only the author or a moderator can delete a post."),
    (34.6, 38.7, "A proof mathematically guarantees the correctness of the implementation."),
    (39.6, 42.3, "Proven in Lean 4. Shipped as Rust."),
    (42.7, 45.9, "From servers to robots, wherever web apps run."),
]


def frame(t):
    """Seconds -> 1-based frame number."""
    return int(round(t * FPS)) + 1


def shot(name):
    for n, a, b in SHOTS:
        if n == name:
            return a, b
    raise KeyError(name)
