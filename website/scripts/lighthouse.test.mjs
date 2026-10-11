// Offline check of the budgets and page discovery: `node --test scripts/`.
import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { BUDGET, CATEGORIES, assertions, oversizedHtml, pages, pathOf } from '../lighthouse.budgets.mjs';

// ocx-sh/website tests/budgets.mjs `content` class: ceilings, never exceeded here.
const WEBSITE = { preJsGz: 11 * 1024, totalBytes: 147 * 1024, domElements: 800 };

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

