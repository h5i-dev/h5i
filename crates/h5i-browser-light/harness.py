#!/usr/bin/env python3
"""How every harness in this crate reaches the engine, in one place.

ROADMAP §B19.5. Three scripts used to each hold their own answer to "where is
the engine and how is it invoked", and when the engine stopped being its own
binary — it became a library behind `h5i __engine` — `wpt/run.py` was updated
and `corpus/run.py` and `corpus/compare.py` were not. Both had been pointed at
`target/{debug,release}/h5i-browser-light` ever since, a path that does not
exist, so neither had run. The instrument this repository credits with finding
most of the engine's real work had been dead for weeks and nothing said so.

A shared module rather than three fixed copies, because the failure was not
that the paths were wrong. It was that there were three of them.

The second half of that finding is not a path at all, and it is why
`instrument_argv` exists rather than a bare string. See `ENGINE_GRANT`.
"""

import os
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent

# The engine, as the shipping binary reaches it.
#
# `h5i __engine` is hidden, not private: it is documented as the way to reach
# the engine on its own, and it is exactly what `h5i browser` execs. A harness
# that used `h5i browser open` instead would be measuring the session registry,
# the control lock and the placement logic as well as the engine, which is a
# different subject.
ENGINE_SUBCOMMAND = "__engine"

# What an instrument has to be granted, and why it is a flag rather than a list.
#
# The engine denies every remote origin unless one is granted, which is right
# for an agent and wrong for a corpus: the third-party subresources are most of
# what there is to *see*, and a run that refuses them is measuring its own
# allowlist. `corpus/run.py` used to build a per-URL wildcard list plus six
# hard-coded CDNs, which is the same decision made quietly and less completely
# — a page pulling from a seventh CDN looked like a page that failed.
#
# `--allow-any-remote` is that decision made out loud. It widens the *name*
# check only: a public name resolving into private space is still refused, a
# page from the web still may not reach loopback, and a box's own egress
# enforcement is untouched. See `policy.rs`.
ENGINE_GRANT = "--allow-any-remote"


def engine_binary(explicit=None):
    """Find the `h5i` binary, or explain what to build.

    Release first: every harness here measures something — latency, memory,
    crash rate — and a debug build answers a different question about all
    three. A debug binary is accepted with a warning rather than refused,
    because a correctness sweep on one is still worth running.
    """
    if explicit:
        path = Path(explicit)
        if not path.exists():
            sys.exit(f"no engine at {path}")
        return str(path)

    env = os.environ.get("H5I_BIN")
    if env:
        if not Path(env).exists():
            sys.exit(f"H5I_BIN points at {env}, which does not exist")
        return env

    release = REPO / "target" / "release" / "h5i"
    if release.exists():
        return str(release)

    debug = REPO / "target" / "debug" / "h5i"
    if debug.exists():
        print(
            "warning: using the debug build; latency and memory numbers from it "
            "mean nothing. Build with `cargo build --release -p h5i`.",
            file=sys.stderr,
        )
        return str(debug)

    found = shutil.which("h5i")
    if found:
        return found

    sys.exit(
        "no h5i binary found. Build one:\n"
        "    cargo build --release -p h5i\n"
        "or name it with --binary / $H5I_BIN."
    )


def instrument_argv(binary, verb, *args, grant=True):
    """One engine invocation, with the instrument grant attached.

    `grant=False` is for a harness that is deliberately measuring what the
    default policy refuses, which is a real thing to want and must not be the
    accident it used to be.
    """
    argv = [binary, ENGINE_SUBCOMMAND, verb, *args]
    if grant:
        argv.append(ENGINE_GRANT)
    return argv


def check_engine(binary):
    """Fail now, with a readable message, rather than once per URL.

    A harness that discovers a broken engine on its 900th page has wasted an
    hour and reports a sweep of failures that are all one fact.
    """
    try:
        done = subprocess.run(
            [binary, ENGINE_SUBCOMMAND, "capabilities"],
            capture_output=True,
            text=True,
            timeout=60,
        )
    except FileNotFoundError:
        sys.exit(f"{binary} is not executable")
    except subprocess.TimeoutExpired:
        sys.exit(f"{binary} __engine capabilities did not answer within 60s")
    if done.returncode != 0:
        sys.exit(
            f"{binary} __engine capabilities failed ({done.returncode}).\n"
            f"{(done.stderr or '').strip()[:500]}\n\n"
            "A build without the `browser` feature has no `__engine`."
        )
    return done.stdout
