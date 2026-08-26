#!/usr/bin/env python3
"""Run h5i-agent.wasm on this machine with wasmtime: no browser, no Node.

The module is a reactor with zero imports, so any wasm runtime can embed it and
call its exports. This is a terminal REPL over that module, driven by the same
loop web/index.html uses: instantiate the wasm, take a task, stream the model
output, run tools against an in-memory filesystem, and keep the conversation.
The agent's logic runs inside the wasm; this host only performs effects and
marshals JSON.

    pip install wasmtime
    crates/h5i-wasm-harness/scripts/build-wasm.sh       # writes ../build/h5i-agent.wasm

    python3 hosts/wasmtime_host.py                      # interactive, offline scripted model
    python3 hosts/wasmtime_host.py --model-url URL      # interactive, a live endpoint (streams)
    python3 hosts/wasmtime_host.py --demo               # non-interactive self-check (asserts)
"""

import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.request
from importlib.metadata import PackageNotFoundError, version

import wasmtime

try:
    WASMTIME_VERSION = version("wasmtime")
except PackageNotFoundError:
    WASMTIME_VERSION = "?"

HERE = os.path.dirname(os.path.abspath(__file__))
WASM = os.path.normpath(os.path.join(HERE, "..", "build", "h5i-agent.wasm"))

# ANSI colors, only when stdout is a terminal.
_TTY = sys.stdout.isatty()
def _c(code): return (lambda s: f"\033[{code}m{s}\033[0m") if _TTY else (lambda s: s)
BLUE, GREEN, DIM, RED, BOLD = _c("34"), _c("32"), _c("2"), _c("31"), _c("1")


class Agent:
    """Thin embedding of the module over the seven-export ABI (see ../src/wasm.rs)."""

    def __init__(self, path):
        engine = wasmtime.Engine()
        self.store = wasmtime.Store(engine)
        module = wasmtime.Module.from_file(engine, path)
        inst = wasmtime.Instance(self.store, module, [])  # no imports to supply
        ex = inst.exports(self.store)
        self.mem = ex["memory"]
        self._alloc = ex["alloc"]
        self._init = ex["agent_init"]
        self._step = ex["agent_step"]
        self._resume = ex["agent_resume"]
        self._dump = ex["agent_dump"]

    def _write_input(self, s):
        data = s.encode("utf-8")
        ptr = self._alloc(self.store, len(data))
        self.mem.write(self.store, data, ptr)
        return ptr, len(data)

    def _read_packed(self, packed):
        packed &= (1 << 64) - 1
        ptr, length = packed >> 32, packed & 0xFFFFFFFF
        return bytes(self.mem.read(self.store, ptr, ptr + length)).decode("utf-8")

    def _call(self, fn, payload):
        ptr, length = self._write_input(payload)
        return json.loads(self._read_packed(fn(self.store, ptr, length)))

    def init(self, params):
        return self._call(self._init, json.dumps(params))

    def step(self, event):
        return self._call(self._step, json.dumps(event))

    def resume(self, task):
        return self._call(self._resume, json.dumps({"task": task}))

    def dump(self):
        return json.loads(self._read_packed(self._dump(self.store)))


def memory_tools(initial=None):
    files = dict(initial or {})

    def run(name, args):
        path = args.get("path", "")
        if name == "write_file":
            content = args.get("content", "")
            files[path] = content
            return True, f"wrote {len(content)} bytes to {path}"
        if name == "read_file":
            return (True, files[path]) if path in files else (False, f"no such file: {path}")
        if name == "list_dir":
            return True, "\n".join(sorted(files))
        return False, f"no executor for {name}"

    return run, files


def scripted_model(envelopes):
    state = {"i": 0}

    def call(_request):
        if state["i"] >= len(envelopes):
            return {"model_failed": {"status": 400, "body": "mock script exhausted"}}
        env = envelopes[state["i"]]
        state["i"] += 1
        return {"model_reply": {"body": json.dumps(env)}}

    return call


def assistant(content, tool_calls=()):
    message = {"role": "assistant", "content": content}
    if tool_calls:
        message["tool_calls"] = [
            {"id": tid, "type": "function", "function": {"name": name, "arguments": args}}
            for (tid, name, args) in tool_calls
        ]
    return {"choices": [{"message": message}]}


def run_task(agent, params, model, run_tool, fresh=True, on_effect=None):
    effect = agent.init(params) if fresh else agent.resume(params["task"])
    while True:
        if on_effect:
            on_effect(effect)
        if "done" in effect:
            return effect["done"]
        if "fatal" in effect:
            return {"status": "fatal", "result": effect["fatal"]["message"]}
        if "call_model" in effect:
            effect = agent.step(model(effect["call_model"]["request"]))
        elif "run_tool" in effect:
            rt = effect["run_tool"]
            ok, output = run_tool(rt["name"], rt["args"])
            effect = agent.step({"tool_finished": {"call_id": rt["call_id"], "ok": ok, "output": output}})
        else:
            return {"status": "protocol_error", "result": f"unknown effect: {effect}"}


# ---- terminal REPL ----

TOOL_NAMES = ["read_file", "write_file", "list_dir"]


def typewriter(text, delay=0.008):
    for ch in text:
        sys.stdout.write(ch)
        sys.stdout.flush()
        if delay:
            time.sleep(delay)


def mock_model():
    """Offline scripted demo: a fixed write->read->done script, typed out."""
    base = scripted_model([
        assistant("", [("c1", "write_file", json.dumps({"path": "hello.txt", "content": "hi"}))]),
        assistant("", [("c2", "read_file", json.dumps({"path": "hello.txt"}))]),
        assistant('Done. hello.txt contains "hi".'),
    ])

    def call(request):
        ev = base(request)
        if "model_reply" in ev:
            content = json.loads(ev["model_reply"]["body"])["choices"][0]["message"].get("content") or ""
            typewriter(content)
            if content:
                print()  # end the model's line
        return ev

    return call


def streaming_model(url, api_key=None):
    """A live OpenAI-compatible endpoint over urllib; renders content live."""

    def call(request):
        body = json.loads(request)
        body["stream"] = True
        headers = {"Content-Type": "application/json"}
        if api_key:
            headers["Authorization"] = f"Bearer {api_key}"
        req = urllib.request.Request(url, data=json.dumps(body).encode(), headers=headers, method="POST")
        try:
            resp = urllib.request.urlopen(req, timeout=180)
        except urllib.error.HTTPError as e:
            return {"model_failed": {"status": e.code, "body": e.read().decode("utf-8", "replace")}}
        except Exception as e:  # noqa: BLE001
            return {"model_failed": {"status": 0, "body": str(e)}}

        content, tools = "", []
        for raw in resp:  # the response iterates by line
            line = raw.decode("utf-8", "replace").rstrip("\r\n")
            if not line.startswith("data:"):
                continue
            data = line[5:].strip()
            if data == "[DONE]":
                break
            try:
                j = json.loads(data)
            except json.JSONDecodeError:
                continue
            delta = (j.get("choices") or [{}])[0].get("delta") or {}
            piece = delta.get("content")
            if piece:
                content += piece
                sys.stdout.write(piece)
                sys.stdout.flush()
            for tc in delta.get("tool_calls") or []:
                idx = tc.get("index", 0)
                slot = next((t for t in tools if t["index"] == idx), None)
                if slot is None:
                    slot = {"index": idx, "id": "", "name": "", "args": ""}
                    tools.append(slot)
                if tc.get("id"):
                    slot["id"] = tc["id"]
                fn = tc.get("function") or {}
                slot["name"] += fn.get("name") or ""
                slot["args"] += fn.get("arguments") or ""
        if content:
            sys.stdout.write("\n")  # end the streamed line
            sys.stdout.flush()
        message = {"role": "assistant", "content": content}
        if tools:
            message["tool_calls"] = [
                {"id": t["id"], "type": "function", "function": {"name": t["name"], "arguments": t["args"]}}
                for t in tools
            ]
        return {"model_reply": {"body": json.dumps({"choices": [{"message": message}]})}}

    return call


def make_run_tool(vfs_run):
    def rt(name, args):
        ok, output = vfs_run(name, args)
        print(GREEN(f"\n⚙ {name} {json.dumps(args)}"))
        shown = output if len(output) <= 400 else output[:400] + "…"
        shown = shown if shown.strip() else "(no output)"
        print(DIM("  " + shown.replace("\n", "\n  ")))
        return ok, output

    return rt


def repl(model_url=None, api_key=None):
    if not os.path.exists(WASM):
        sys.exit(f"{WASM} not found — run scripts/build-wasm.sh first")

    live = bool(model_url)
    vfs_run, files = memory_tools()
    run_tool = make_run_tool(vfs_run)
    agent, first = None, True

    print(BOLD("h5i-agent") + DIM(f" — the loop runs under wasmtime {WASMTIME_VERSION} on this machine."))
    if live:
        print(DIM(f"live endpoint: {model_url}"))
    else:
        print(DIM("offline scripted demo (the typed task is illustrative). "))
    print(DIM("type a task and press enter; Ctrl-D or 'exit' to quit.\n"))

    while True:
        try:
            task = input(BLUE("» ")).strip()
        except (EOFError, KeyboardInterrupt):
            print()
            break
        if not task:
            continue
        if task in ("exit", "quit", ":q"):
            break

        if live:
            model = streaming_model(model_url, api_key)
            if agent is None:
                agent = Agent(WASM)
                first = True
            params = ({"task": task, "tools": TOOL_NAMES, "workspace_note": "wasmtime VFS",
                       "max_steps": 12} if first else {"task": task})
            fresh, first = first, False
        else:
            model = mock_model()
            agent = Agent(WASM)  # fresh session, fixed script
            params = {"task": task, "tools": TOOL_NAMES, "workspace_note": "wasmtime VFS", "max_steps": 12}
            fresh = True

        done = run_task(agent, params, model, run_tool, fresh=fresh)
        print()
        if done["status"] != "success":
            print(RED(f"[{done['status']}] {done['result']}"))


def demo():
    """Non-interactive self-check: run a scripted session and assert the outcome."""
    if not os.path.exists(WASM):
        sys.exit(f"{WASM} not found — run scripts/build-wasm.sh first")

    agent = Agent(WASM)
    run_tool, files = memory_tools()
    model = scripted_model([
        assistant("", [("c1", "write_file", json.dumps({"path": "hello.txt", "content": "hi"}))]),
        assistant("", [("c2", "read_file", json.dumps({"path": "hello.txt"}))]),
        assistant("done: the file says hi"),
    ])

    trace = []
    done = run_task(
        agent,
        {"task": "create hello.txt with hi, then read it back",
         "tools": TOOL_NAMES, "workspace_note": "wasmtime in-memory VFS", "max_steps": 10},
        model, run_tool, on_effect=lambda e: trace.append(next(iter(e))),
    )

    print("runtime:", "wasmtime", WASMTIME_VERSION)
    print("effects:", " -> ".join(trace))
    print("status :", done["status"])
    print("result :", done["result"])
    print("file   :", repr(files.get("hello.txt")))

    assert done["status"] == "success", "run should succeed"
    assert files.get("hello.txt") == "hi", "file written through the boundary"
    dump = agent.dump()
    assert sum(1 for m in dump["messages"] if m["role"] == "tool") == 2, "two tool results"

    done2 = run_task(
        agent, {"task": "list the files"},
        scripted_model([assistant("there is one file: hello.txt")]),
        run_tool, fresh=False,
    )
    assert done2["status"] == "success", "resume should succeed"
    assert len(agent.dump()["messages"]) > len(dump["messages"]), "resume kept the history"

    print("\nOK — h5i-agent.wasm ran under wasmtime on this machine (incl. resume).")


def main():
    ap = argparse.ArgumentParser(description="Run h5i-agent.wasm under wasmtime.")
    ap.add_argument("--model-url", help="a live OpenAI-compatible endpoint (interactive)")
    ap.add_argument("--api-key", help="bearer token for --model-url")
    ap.add_argument("--demo", action="store_true", help="non-interactive scripted self-check")
    args = ap.parse_args()
    if args.demo:
        demo()
    else:
        repl(args.model_url, args.api_key)


if __name__ == "__main__":
    main()
