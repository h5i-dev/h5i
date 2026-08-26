// The wasm host loop, environment-agnostic: the same code drives the module in
// a browser and under Node, because both expose the identical `WebAssembly`
// API. Nothing here is browser-specific — the page (index.html) only supplies a
// model callback, a tool executor, and the DOM.
//
// The ABI (see ../src/wasm.rs): the host calls `alloc(n)`, writes UTF-8 JSON at
// the returned pointer, then calls an export with `(ptr, len)`. Every export
// returns a packed `u64 = (ptr << 32) | len` pointing at guest-owned JSON valid
// only until the next call — so we decode it immediately. Memory can grow (the
// module's bump allocator calls `memory.grow`), which detaches the old
// ArrayBuffer, so we always re-view `memory.buffer` right before touching it.

const enc = new TextEncoder();
const dec = new TextDecoder();

export class Agent {
  constructor(instance) {
    this.ex = instance.exports;
  }

  /** Instantiate from bytes (browser: `fetch(...).arrayBuffer()`; Node: `fs`). */
  static async fromBytes(bytes) {
    const { instance } = await WebAssembly.instantiate(bytes, {});
    return new Agent(instance);
  }

  #writeInput(str) {
    const bytes = enc.encode(str);
    const ptr = this.ex.alloc(bytes.length);
    new Uint8Array(this.ex.memory.buffer, ptr, bytes.length).set(bytes);
    return [ptr, bytes.length];
  }

  #readPacked(packed) {
    // Exports return i64 as a BigInt (JS-BigInt-to-Wasm integration).
    const ptr = Number(packed >> 32n);
    const len = Number(packed & 0xffffffffn);
    return dec.decode(new Uint8Array(this.ex.memory.buffer, ptr, len));
  }

  #call(exportName, str) {
    const [ptr, len] = this.#writeInput(str);
    const packed = this.ex[exportName](ptr, len);
    return JSON.parse(this.#readPacked(packed)); // read before the next call
  }

  init(params) {
    return this.#call('agent_init', JSON.stringify(params));
  }

  step(event) {
    return this.#call('agent_step', JSON.stringify(event));
  }

  resume(task) {
    return this.#call('agent_resume', JSON.stringify({ task }));
  }

  dump() {
    return JSON.parse(this.#readPacked(this.ex.agent_dump()));
  }
}

/**
 * Drive one task to completion.
 * @param agent   an {@link Agent}
 * @param params  { task, tools:[...], workspace_note, max_steps? }
 * @param model   async (requestBody:string) => Event  (an OpenAI request in;
 *                {model_reply:{body}} | {model_failed:{status,body}} out)
 * @param runTool async (name, args) => { ok:boolean, output:string }
 * @param onEffect optional (effect) => void, for live UI
 * @param fresh   if false, continue an existing conversation via resume()
 * @returns { status, result }
 */
export async function runTask(agent, params, { model, runTool, onEffect, fresh = true } = {}) {
  let effect = fresh ? agent.init(params) : agent.resume(params.task);
  for (;;) {
    onEffect?.(effect);
    if (effect.done) return effect.done;
    if (effect.fatal) return { status: 'fatal', result: effect.fatal.message };
    if (effect.call_model) {
      const event = await model(effect.call_model.request);
      effect = agent.step(event);
    } else if (effect.run_tool) {
      const { call_id, name, args } = effect.run_tool;
      const { ok, output } = await runTool(name, args);
      effect = agent.step({ tool_finished: { call_id, ok, output } });
    } else {
      return { status: 'protocol_error', result: 'unknown effect: ' + JSON.stringify(effect) };
    }
  }
}

// ---- an in-memory filesystem tool executor, shared by the demos ----

export function memoryTools(initial = {}) {
  const files = new Map(Object.entries(initial));
  const run = (name, args) => {
    const path = args.path ?? '';
    switch (name) {
      case 'write_file':
        files.set(path, args.content ?? '');
        return { ok: true, output: `wrote ${(args.content ?? '').length} bytes to ${path}` };
      case 'read_file':
        return files.has(path)
          ? { ok: true, output: files.get(path) }
          : { ok: false, output: `no such file: ${path}` };
      case 'list_dir':
        return { ok: true, output: [...files.keys()].sort().join('\n') };
      default:
        return { ok: false, output: `no executor for ${name}` };
    }
  };
  return { run, files };
}

// ---- a scripted mock model (offline, no network / CORS) ----
// Replays canned chat-completions envelopes in order, like the CLI's --script.

export function scriptedModel(envelopes) {
  let i = 0;
  return async (_request) => {
    if (i >= envelopes.length) {
      return { model_failed: { status: 400, body: 'mock script exhausted' } };
    }
    return { model_reply: { body: JSON.stringify(envelopes[i++]) } };
  };
}

// ---- a real model over fetch (OpenAI-compatible endpoint) ----
// Note: the browser enforces CORS; the endpoint must send permissive CORS
// headers (a local llama.cpp/Ollama, or a proxy that adds them).

export function fetchModel(url, { apiKey } = {}) {
  return async (request) => {
    try {
      const headers = { 'Content-Type': 'application/json' };
      if (apiKey) headers['Authorization'] = `Bearer ${apiKey}`;
      const resp = await fetch(url, { method: 'POST', headers, body: request });
      const body = await resp.text();
      return resp.ok
        ? { model_reply: { body } }
        : { model_failed: { status: resp.status, body } };
    } catch (e) {
      return { model_failed: { status: 0, body: String(e) } };
    }
  };
}

// A real model over fetch that STREAMS: it asks the endpoint for SSE, calls
// `onToken` for each content delta (so a terminal can render live), and
// reassembles the one envelope the module expects. Mirrors the CLI's stream.rs.
export function streamingFetchModel(url, { apiKey, onToken } = {}) {
  return async (request) => {
    const headers = { 'Content-Type': 'application/json' };
    if (apiKey) headers['Authorization'] = `Bearer ${apiKey}`;
    let body = request;
    try {
      const o = JSON.parse(request);
      o.stream = true;
      body = JSON.stringify(o);
    } catch { /* send as-is */ }

    let resp;
    try {
      resp = await fetch(url, { method: 'POST', headers, body });
    } catch (e) {
      return { model_failed: { status: 0, body: String(e) } };
    }
    if (!resp.ok || !resp.body) {
      return { model_failed: { status: resp.status, body: await resp.text().catch(() => '') } };
    }

    const reader = resp.body.getReader();
    let buf = '';
    let content = '';
    const tools = [];
    let done = false;
    while (!done) {
      const { value, done: rd } = await reader.read();
      if (rd) break;
      buf += dec.decode(value, { stream: true });
      let i;
      while ((i = buf.indexOf('\n\n')) >= 0) {
        const event = buf.slice(0, i);
        buf = buf.slice(i + 2);
        for (const raw of event.split('\n')) {
          const line = raw.replace(/\r$/, '');
          if (!line.startsWith('data:')) continue;
          const data = line.slice(5).trim();
          if (data === '[DONE]') { done = true; break; }
          let j;
          try { j = JSON.parse(data); } catch { continue; }
          const delta = j.choices && j.choices[0] && j.choices[0].delta;
          if (!delta) continue;
          if (delta.content) { content += delta.content; onToken?.(delta.content); }
          for (const tc of delta.tool_calls || []) {
            const idx = tc.index ?? 0;
            let slot = tools.find((t) => t.index === idx);
            if (!slot) { slot = { index: idx, id: '', name: '', args: '' }; tools.push(slot); }
            if (tc.id) slot.id = tc.id;
            if (tc.function?.name) slot.name += tc.function.name;
            if (tc.function?.arguments) slot.args += tc.function.arguments;
          }
        }
      }
    }
    const message = { role: 'assistant', content };
    if (tools.length) {
      message.tool_calls = tools.map((t) => ({
        id: t.id, type: 'function', function: { name: t.name, arguments: t.args },
      }));
    }
    return { model_reply: { body: JSON.stringify({ choices: [{ message }] }) } };
  };
}

/** Build the assistant envelope helpers used to script the mock. */
export function assistant(content, toolCalls = []) {
  const message = { role: 'assistant', content };
  if (toolCalls.length) {
    message.tool_calls = toolCalls.map(([id, name, args]) => ({
      id,
      type: 'function',
      function: { name, arguments: args },
    }));
  }
  return { choices: [{ message }] };
}
