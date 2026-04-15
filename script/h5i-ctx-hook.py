#!/usr/bin/env python3
"""
Claude Code PostToolUse hook — auto-emit h5i context trace entries.

Receives a JSON object on stdin with the shape:
  {
    "hook_event_name": "PostToolUse",
    "tool_name": "Edit" | "Write" | "Read" | ...,
    "tool_input": { "file_path": "...", ... },
    ...
  }

Emits:
  ACT    after Edit / Write
  OBSERVE after Read

Silently exits on any error so it never blocks Claude Code.
"""
import json
import os
import subprocess
import sys

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def relative(path: str) -> str:
    """Return path relative to repo root, or the basename as fallback."""
    try:
        return os.path.relpath(path, REPO_ROOT)
    except ValueError:
        return os.path.basename(path)


def trace(kind: str, message: str) -> None:
    subprocess.run(
        ["h5i", "context", "trace", "--kind", kind, message],
        cwd=REPO_ROOT,
        capture_output=True,
        timeout=5,
    )


def main() -> None:
    raw = sys.stdin.read()
    if not raw.strip():
        return

    try:
        data = json.loads(raw)
    except json.JSONDecodeError:
        return

    tool = data.get("tool_name", "")
    inp = data.get("tool_input", {})

    if tool in ("Edit", "Write"):
        file_path = inp.get("file_path", "")
        if file_path:
            verb = "edited" if tool == "Edit" else "wrote"
            trace("ACT", f"{verb} {relative(file_path)}")

    elif tool == "Read":
        file_path = inp.get("file_path", "")
        if file_path:
            trace("OBSERVE", f"read {relative(file_path)}")


if __name__ == "__main__":
    try:
        main()
    except Exception:
        # Never block Claude Code on hook failure
        sys.exit(0)
