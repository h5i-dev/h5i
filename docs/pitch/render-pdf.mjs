#!/usr/bin/env node
// Render the pitch deck (index.html) to its committed PDF twin.
//
//   node render-pdf.mjs                      # -> h5i-pitch-deck.pdf
//   node render-pdf.mjs --out other.pdf
//
// Uses the deck's own @media print block (one 1280x720 page per slide), so
// the output matches the in-browser "Export PDF" button exactly. Re-run this
// after editing index.html and commit the refreshed PDF alongside it.
//
// Needs: playwright (any install — a local node_modules, a global one, or an
// ~/.npm/_npx cache) with its chromium headless shell.

import { createRequire } from 'node:module';
import { existsSync, readdirSync } from 'node:fs';
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
const OUT = path.resolve(here, opt('out', 'h5i-pitch-deck.pdf'));
const SRC = path.join(here, 'index.html');

const { chromium } = resolvePlaywright();
const executablePath = findHeadlessShell() ?? undefined;

const browser = await chromium.launch({ executablePath });
const page = await browser.newPage({ viewport: { width: 1280, height: 720 } });
await page.goto('file://' + SRC, { waitUntil: 'networkidle' });
await page.emulateMedia({ media: 'print' });
await page.pdf({ path: OUT, preferCSSPageSize: true, printBackground: true });
await browser.close();
console.log('wrote ' + OUT);
