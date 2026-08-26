"""`h5i-agent` command dispatch: `web` and `wasmtime` subcommands."""

import sys

_USAGE = """usage: h5i-agent <command> [options]

commands:
  web         run the agent in your browser (no extra dependencies)
  wasmtime    run the agent under wasmtime in the terminal
              (install the runtime with:  pip install 'h5i-agent[wasmtime]')

Run `h5i-agent <command> --help` for a command's options.
"""


def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    if not argv or argv[0] in ("-h", "--help"):
        sys.stdout.write(_USAGE)
        return 0 if argv else 2

    cmd, rest = argv[0], argv[1:]
    if cmd == "web":
        from . import web
        return web.main(rest)
    if cmd == "wasmtime":
        try:
            from . import runtime
        except ImportError as e:
            sys.exit(
                f"the wasmtime runtime is not available ({e}).\n"
                f"install it with:  pip install 'h5i-agent[wasmtime]'"
            )
        return runtime.main(rest)

    sys.stderr.write(f"unknown command: {cmd}\n\n{_USAGE}")
    return 2


if __name__ == "__main__":
    sys.exit(main())
