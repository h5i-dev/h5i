#!/usr/bin/env node
// Render an h5i demo film (a deterministic HTML timeline) to an mp4.
//
//   node render.mjs                          # index.html -> out/h5i-demo.mp4
//   node render.mjs --stills 5,30            # PNG stills at given seconds -> out/still-*.png
//   node render.mjs --fps 30 --crf 18
//
// Needs: ffmpeg on PATH, and playwright (any install — a local node_modules,
// a global one, or an ~/.npm/_npx cache) with its chromium headless shell.

import { createRequire } from 'node:module';
import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, readdirSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);

function resolvePlaywright() {
  for (const name of ['playwright', 'playwright-core']) {
    try { return require(name); } catch {}
  }
  const npx = path.join(homedir(), '.npm', '_npx');
  if (existsSync(npx)) {
    const hits = readdirSync(npx)
      .map(d => path.join(npx, d, 'node_modules', 'playwright'))
      .filter(p => existsSync(p))
      .map(p => ({ p, v: require(path.join(p, 'package.json')).version }))
      .sort((a, b) => b.v.localeCompare(a.v, undefined, { numeric: true }));
    if (hits.length) return createRequire(path.join(hits[0].p, 'x.js'))('playwright');
  }
  throw new Error('playwright not found — run: npm i playwright-core');
}

function findHeadlessShell() {
  const root = path.join(homedir(), '.cache', 'ms-playwright');
  if (!existsSync(root)) return null;
  const dirs = readdirSync(root).filter(d => d.startsWith('chromium'))
    .sort((a, b) => (b.match(/\d+$/)?.[0] ?? 0) - (a.match(/\d+$/)?.[0] ?? 0));
  for (const d of dirs) {
    for (const bin of ['headless_shell', 'chrome']) {
      const p = path.join(root, d, 'chrome-linux', bin);
      if (existsSync(p)) return p;
    }
  }
  return null;
}

const args = process.argv.slice(2);
const opt = (name, dflt) => {
  const i = args.indexOf('--' + name);
  return i >= 0 ? args[i + 1] : dflt;
};
const FPS = Number(opt('fps', 30));
const CRF = Number(opt('crf', 18));
const stills = opt('stills', null);
const PAGE = opt('page', 'index.html');
const BASE = path.basename(PAGE, '.html');

const outDir = path.join(here, 'out');
mkdirSync(outDir, { recursive: true });

const { chromium } = resolvePlaywright();
const executablePath = findHeadlessShell();
const browser = await chromium.launch({
  executablePath: executablePath ?? undefined,
  args: ['--force-device-scale-factor=1', '--hide-scrollbars', '--font-render-hinting=none'],
});
const page = await browser.newPage({ viewport: { width: 1920, height: 1080 } });
await page.goto('file://' + path.join(here, PAGE) + '?render=1');
await page.evaluate(() => document.fonts.ready);
const TOTAL = await page.evaluate(() => window.TOTAL);

async function frame(ms, type, quality) {
  await page.evaluate(t => window.SEEK(t), ms);
  return page.screenshot({ type, ...(quality ? { quality } : {}) });
}

if (stills) {
  for (const s of stills.split(',')) {
    const buf = await frame(Number(s) * 1000, 'png');
    const f = path.join(outDir, `still-${BASE}-${Number(s).toFixed(1)}s.png`);
    writeFileSync(f, buf);
    console.log('wrote', f);
  }
  await browser.close();
  process.exit(0);
}

const nFrames = Math.ceil((TOTAL / 1000) * FPS);
const outFile = path.join(outDir, BASE === 'index' ? 'h5i-demo.mp4' : `h5i-${BASE}.mp4`);
console.log(`rendering ${nFrames} frames @ ${FPS}fps (${(TOTAL / 1000).toFixed(0)}s) -> ${outFile}`);

const ff = spawn('ffmpeg', [
  '-y', '-f', 'image2pipe', '-framerate', String(FPS), '-i', '-',
  '-c:v', 'libx264', '-preset', 'slow', '-crf', String(CRF),
  '-pix_fmt', 'yuv420p', '-movflags', '+faststart', outFile,
], { stdio: ['pipe', 'inherit', 'inherit'] });
const ffDone = new Promise((res, rej) =>
  ff.on('close', c => (c === 0 ? res() : rej(new Error('ffmpeg exit ' + c)))));

const t0 = Date.now();
for (let i = 0; i < nFrames; i++) {
  const buf = await frame((i / FPS) * 1000, 'jpeg', 95);
  if (!ff.stdin.write(buf)) await new Promise(r => ff.stdin.once('drain', r));
  if (i % 150 === 0) {
    const rate = (i + 1) / ((Date.now() - t0) / 1000);
    process.stdout.write(`\r${i}/${nFrames} frames  (${rate.toFixed(1)} fps, eta ${Math.round((nFrames - i) / rate)}s)   `);
  }
}
ff.stdin.end();
await ffDone;
await browser.close();
console.log(`\ndone -> ${outFile}`);
