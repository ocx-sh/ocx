// Offline check of the budgets and page discovery: `node --test scripts/`.
import assert from 'node:assert/strict';
import { existsSync, mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { pathToFileURL } from 'node:url';
import { ocxRedirects } from '../plugins/redirects.mjs';
import { BUDGET, CATEGORIES, assertions, oversizedHtml, pages, pathOf } from '../lighthouse.budgets.mjs';

// ocx-sh/website tests/budgets.mjs `content` class: ceilings, never exceeded here.
const WEBSITE = { preJsGz: 11 * 1024, totalBytes: 147 * 1024, domElements: 900 };

test('budgets stay at or under the website ceilings', () => {
  for (const k of Object.keys(WEBSITE)) assert.ok(BUDGET[k] <= WEBSITE[k], k);
});

test('assertions: 100 per category, budgets, median run', () => {
  const a = assertions();
  for (const c of CATEGORIES) assert.equal(a[`categories:${c}`][1].minScore, 1);
  assert.equal(a['dom-size'][1].maxNumericValue, BUDGET.domElements);
  assert.ok(Object.values(a).every(([lvl, o]) => lvl === 'error' && o.aggregationMethod === 'median-run'));
});

test('pages: routes from dist HTML, sorted, base-prefixed', () => {
  const d = mkdtempSync(join(tmpdir(), 'lh-'));
  mkdirSync(join(d, 'guides/x'), { recursive: true });
  for (const f of ['index.html', '404.html', 'guides/x/index.html']) writeFileSync(join(d, f), '<html></html>');
  writeFileSync(join(d, 'a.js'), '');
  const routes = pages(d);
  assert.deepEqual(routes, ['/', '/404.html', '/guides/x/']);
  assert.equal(pathOf('/guides/x/'), '/docs/guides/x/');
  assert.deepEqual(oversizedHtml(d + '/', routes), []);
  assert.throws(() => pages(join(d, 'nope')), /missing dist/);
});


// Lighthouse follows a stub to its destination, so the stub URL would only re-audit another page.
test('pages: redirect stubs are skipped, a page that only gained a fragment map stays', async () => {
  const d = mkdtempSync(join(tmpdir(), 'lh-'));
  mkdirSync(join(d, 'guides/x'), { recursive: true });
  writeFileSync(join(d, 'index.html'), '<html></html>');
  writeFileSync(join(d, 'guides/x/index.html'), '<html><head></head></html>');
  const redirects = mkdtempSync(join(tmpdir(), 'lh-redirects-'));
  writeFileSync(
    join(redirects, 't.json'),
    JSON.stringify({
      '/docs/old/': { to: '/docs/guides/x/' },
      '/docs/guides/x/': { fragments: { a: '/docs/guides/x/#b' } },
    }),
  );
  const plugin = ocxRedirects({ dir: redirects });
  plugin.hooks['astro:config:setup']();
  await plugin.hooks['astro:build:done']({ dir: pathToFileURL(`${d}/`) });
  assert.ok(existsSync(join(d, 'old/index.html')), 'the plugin wrote the stub');
  assert.deepEqual(pages(d), ['/', '/guides/x/']);
});
