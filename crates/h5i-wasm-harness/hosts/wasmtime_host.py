#!/usr/bin/env python3
"""Run h5i-agent.wasm on this machine with wasmtime: no browser, no Node.

The module is a reactor with zero imports, so any wasm runtime can embed it and
call its exports. This mirrors web/node-demo.mjs against a purpose-built wasm
engine (wasmtime, from the Bytecode Alliance): instantiate the module, drive the
agent loop with a scripted mock model and an in-memory filesystem, decode the
packed-u64 returns, and assert the outcome. The agent's logic runs inside the
wasm; this host only performs effects and marshals JSON.

    pip install wasmtime
    crates/h5i-wasm-harness/scripts/build-wasm.sh          # writes ../build/h5i-agent.wasm
    python3 crates/h5i-wasm-harness/hosts/wasmtime_host.py
"""

import json
import os
import sys
from importlib.metadata import PackageNotFoundError, version

import wasmtime

try:
    WASMTIME_VERSION = version("wasmtime")
except PackageNotFoundError:
    WASMTIME_VERSION = "?"

HERE = os.path.dirname(os.path.abspath(__file__))
WASM = os.path.normpath(os.path.join(HERE, "..", "build", "h5i-agent.wasm"))


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
        # Exports return i64; the module packs (ptr << 32) | len. Mask to u64
        # in case the runtime hands back a signed value.
        packed &= (1 << 64) - 1
        ptr, length = packed >> 32, packed & 0xFFFFFFFF
        # Re-read memory each call: the bump allocator may have grown it.
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


def main():
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
         "tools": ["read_file", "write_file", "list_dir"],
         "workspace_note": "wasmtime in-memory VFS", "max_steps": 10},
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

    # Multi-turn on the same instance via agent_resume.
    done2 = run_task(
        agent, {"task": "list the files"},
        scripted_model([assistant("there is one file: hello.txt")]),
        run_tool, fresh=False,
    )
    assert done2["status"] == "success", "resume should succeed"
    assert len(agent.dump()["messages"]) > len(dump["messages"]), "resume kept the history"

    print("\nOK — h5i-agent.wasm ran under wasmtime on this machine (incl. resume).")


if __name__ == "__main__":
    main()
