"""h5i-agent: run the WebAssembly coding-agent harness from the command line.

Two hosts drive the same `h5i-agent.wasm` module:

    h5i-agent web        # in your browser
    h5i-agent wasmtime   # under wasmtime, in the terminal (needs the `wasmtime` extra)

The module and the browser page are bundled in the wheel, so an installed
`h5i-agent` needs no repo checkout and no Rust toolchain.
"""

__version__ = "0.1.0"
