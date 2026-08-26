"""Locate the bundled assets (the wasm module and the browser page).

An installed wheel carries them under `h5i_agent/_assets/`. Running straight from
a repo checkout (no wheel built), they are found in the crate's `build/` and
`web/` directories instead, so the same code works both ways.
"""

import os

_HERE = os.path.dirname(os.path.abspath(__file__))
_BUNDLED = os.path.join(_HERE, "_assets")
# The package lives at crates/h5i-wasm-harness/h5i_agent, so the crate root
# (with build/ and web/) is one level up.
_CRATE = os.path.normpath(os.path.join(_HERE, ".."))

# Asset name -> (bundled subpath, repo path). The browser page fetches the wasm
# at ../build/h5i-agent.wasm relative to /web/, so both hosts lay the files out
# under web/ and build/ when serving.
_ASSETS = {
    "h5i-agent.wasm": ("h5i-agent.wasm", os.path.join(_CRATE, "build", "h5i-agent.wasm")),
    "index.html": ("index.html", os.path.join(_CRATE, "web", "index.html")),
    "host.mjs": ("host.mjs", os.path.join(_CRATE, "web", "host.mjs")),
}


def path(name):
    """Absolute path to a bundled asset, or raise a clear error if absent."""
    bundled, repo = _ASSETS[name]
    for candidate in (os.path.join(_BUNDLED, bundled), repo):
        if os.path.exists(candidate):
            return candidate
    raise FileNotFoundError(
        f"{name} not found. In a source checkout, build the module first: "
        f"scripts/build-wasm.sh"
    )
