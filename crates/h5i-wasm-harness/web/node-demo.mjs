// Verify the wasm module runs through the JS host loop, under Node — the same
// WebAssembly API a browser uses, so this proves index.html's core will work.
// Runs a scripted session (write a file, read it back, finish) against an
// in-memory filesystem and asserts the outcome.
//
//   node crates/h5i-wasm-harness/web/node-demo.mjs
//
// (Build the module first: crates/h5i-wasm-harness/scripts/build-wasm.sh)

import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';

import { Agent, runTask, memoryTools, scriptedModel, assistant } from './host.mjs';

const wasmPath = fileURLToPath(new URL('../build/h5i-agent.wasm', import.meta.url));
const bytes = await readFile(wasmPath);
const agent = await Agent.fromBytes(bytes);

const tools = memoryTools();

const model = scriptedModel([
  assistant('', [['c1', 'write_file', JSON.stringify({ path: 'hello.txt', content: 'hi' })]]),
  assistant('', [['c2', 'read_file', JSON.stringify({ path: 'hello.txt' })]]),
  assistant('done: the file says hi'),
]);

const trace = [];
const done = await runTask(
  agent,
  { task: 'create hello.txt with hi, then read it back',
    tools: ['read_file', 'write_file', 'list_dir'],
    workspace_note: 'browser in-memory VFS', max_steps: 10 },
  { model, runTool: tools.run, onEffect: (e) => trace.push(Object.keys(e)[0]) },
);

console.log('effects:', trace.join(' → '));
console.log('status :', done.status);
console.log('result :', done.result);
console.log('file   :', JSON.stringify(tools.files.get('hello.txt')));

assert.equal(done.status, 'success', 'run should succeed');
assert.equal(tools.files.get('hello.txt'), 'hi', 'file written through the boundary');
const dump = agent.dump();
assert.equal(dump.messages.filter((m) => m.role === 'tool').length, 2, 'two tool results');

// Multi-turn via agent_resume on the same instance.
const done2 = await runTask(
  agent,
  { task: 'list the files' },
  { model: scriptedModel([assistant('there is one file: hello.txt')]), runTool: tools.run, fresh: false },
);
assert.equal(done2.status, 'success', 'resume should succeed');
assert.ok(agent.dump().messages.length > dump.messages.length, 'resume kept the history');

console.log('\nOK — the wasm module runs end-to-end through the JS host loop (incl. resume).');
