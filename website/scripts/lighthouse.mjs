#!/usr/bin/env node
// `pnpm run lighthouse [--page /reference/environment/ ...]`: serve dist/docs under /docs/ with a
// small gzip static server (what the CDN does), run lhci (mobile preset) on each page and assert 100 in all four
// categories plus lighthouse.budgets.mjs. Needs `task website:build` first. No --page = every built
// page. A failing page gets 3 runs and is judged on the median run. Chrome: CHROME_PATH, then CHROME_BIN
// (the GitHub runner images set it), then the Playwright one.
// Ported from ocx-sh/rules_ocx site/scripts/lighthouse.mjs: sequential, no lanes, no cache.
import { spawn } from 'node:child_process';
import { mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { extname, join, normalize, sep } from 'node:path';
import { gzipSync } from 'node:zlib';
import { fileURLToPath } from 'node:url';
import { BASE, CATEGORIES, assertions, pages, pathOf } from '../lighthouse.budgets.mjs';

const ROOT = fileURLToPath(new URL('..', import.meta.url));
const DIST = `${ROOT}dist/docs`;
const OUT = `${ROOT}.lighthouseci`;
const PORT = process.env.LH_PORT || '4322';
const argv = process.argv.slice(2);
const wanted = argv.flatMap((a, i) => (argv[i - 1] === '--page' ? [a] : []));

const NO_WSL = { NODE_OPTIONS: `${process.env.NODE_OPTIONS ?? ''} --require ${ROOT}scripts/no-wsl.cjs` };
// Async, so the static server below keeps answering while lhci runs.
const run = (cmd, args, env = {}) =>
  new Promise((done) => {
    const child = spawn(cmd, args, { cwd: ROOT, stdio: 'inherit', env: { ...process.env, ...env } });
    child.on('exit', (code) => done(code === 0));
    child.on('error', () => done(false));
  });

async function chromePath() {
  const fromEnv = process.env.CHROME_PATH || process.env.CHROME_BIN;
  if (fromEnv) return fromEnv;
  return (await import('playwright-core')).chromium.executablePath();
}

async function waitFor(url) {
  for (let i = 0; i < 60; i++) {
    if (await fetch(url).then((r) => r.ok, () => false)) return;
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error(`preview did not come up at ${url}`);
}

/** One line of category scores and the budgeted numbers from the last collected run. */
function summary() {
  const file = readdirSync(OUT).filter((f) => /^lhr-.*\.json$/.test(f)).sort().pop();
  const lhr = JSON.parse(readFileSync(`${OUT}/${file}`, 'utf8'));
  const scores = CATEGORIES.map((c) => Math.round(lhr.categories[c].score * 100)).join('/');
  const dom = lhr.audits['dom-size'].numericValue;
  const bytes = Math.round(lhr.audits['total-byte-weight'].numericValue / 1024);
  return `${scores} dom=${dom} bytes=${bytes}KiB`;
}

/** lhci collect + assert for one URL; true when every assertion passes. */
async function audit(url, runs, chrome) {
  rmSync(OUT, { recursive: true, force: true });
  mkdirSync(OUT, { recursive: true });
  const rc = `${OUT}/rc.json`;
  writeFileSync(rc, JSON.stringify({
    ci: {
      collect: { url: [url], numberOfRuns: runs, chromePath: chrome,
        settings: { chromeFlags: '--headless=new --no-sandbox', disableFullPageScreenshot: true } },
      assert: { assertions: assertions() },
    },
  }));
  const lhci = (cmd) => run('pnpm', ['exec', 'lhci', cmd, `--config=${rc}`], NO_WSL);
  const ok = (await lhci('collect')) && (await lhci('assert'));
  console.log(`${ok ? 'PASS' : 'FAIL'} ${url} ${summary()}`);
  return ok;
}

const routes = wanted.length ? wanted : pages(DIST);
const chrome = await chromePath();
const bad = [];
console.log(`${routes.length} pages under ${BASE}`);
const TYPES = { '.html': 'text/html', '.css': 'text/css', '.js': 'text/javascript', '.json': 'application/json',
  '.svg': 'image/svg+xml', '.woff2': 'font/woff2', '.xml': 'application/xml' };
const server = createServer((req, res) => {
  const send = (f) => {
    const body = readFileSync(f);
    const type = TYPES[extname(f)] ?? 'application/octet-stream';
    const text = /^(text|application\/(json|xml))|svg/.test(type);
    res.writeHead(200, { 'content-type': type, ...(text ? { 'content-encoding': 'gzip' } : {}) });
    res.end(text ? gzipSync(body) : body);
  };
  let path;
  try {
    path = decodeURIComponent(new URL(req.url, 'http://x').pathname);
  } catch {
    res.writeHead(400).end();
    return;
  }
  const file = normalize(join(DIST, path.slice(BASE.length - 1)));
  try {
    if (!path.startsWith(BASE) || !(file === DIST || file.startsWith(DIST + sep))) throw new Error('outside');
    send(path.endsWith('/') ? join(file, 'index.html') : file);
  } catch {
    res.writeHead(404).end();
  }
}).listen(Number(PORT), '127.0.0.1');
try {
  await waitFor(`http://localhost:${PORT}${BASE}`);
  for (const r of routes) {
    const url = `http://localhost:${PORT}${pathOf(r)}`;
    if (!(await audit(url, 1, chrome)) && !(await audit(url, 3, chrome))) bad.push(`${r}: Lighthouse failed`);
  }
} finally {
  server.close();
  server.closeAllConnections();
}
if (bad.length) { console.error(`\nFAILED:\n${bad.join('\n')}`); process.exit(1); }
console.log('Lighthouse: all pages green');
