// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

// The page inventory (docs_ia.md section 5) names every page of the docs exactly once and assigns it to
// one cluster. Over the built site the inventory's pages and the pages under dist/docs are the same set.

import assert from 'node:assert/strict';
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { dirname, join, relative, sep } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const IA = join(HERE, '..', '..', '.claude', 'artifacts', 'docs_ia.md');
const DIST_DOCS = join(HERE, '..', 'dist', 'docs');
const CLUSTERS = ['first-steps', 'everyday', 'publishing', 'explanation', 'integration', 'misc', 'generated'];
// Output that is not a content page: assets, the search index and the not-found page.
const NOT_PAGES = new Set(['_astro', 'pagefind', '404']);

/** The `{ old, new, cluster }` rows of the section 5 table. */
function inventoryRows(markdown) {
  const section = markdown.slice(markdown.indexOf('## 5. Page inventory'), markdown.indexOf('## 6. Merge'));
  return section
    .split('\n')
    .map((line) => line.split('|').slice(1, -1).map((cell) => cell.trim()))
    .filter((cells) => cells.length === 6 && /\.mdx?$/.test(cells[1]))
    .map(([old, page, , , cluster]) => ({ old, new: page, cluster }));
}

/** The site URL of a page path under the content root. */
function pageUrl(path) {
  const dir = path.replace(/\.mdx?$/, '').replace(/(^|\/)index$/, '');
  return `/docs/${dir ? `${dir}/` : ''}`;
}

// The gate sets it after building: a missing dist then fails instead of skipping.
const REQUIRE_DIST = process.env.WEBSITE_REQUIRE_DIST === '1';

/** Page URLs of every `index.html` under `dist/docs`, redirect stubs and non-content output left out. */
function builtPages(root) {
  const pages = [];
  const walk = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      if (NOT_PAGES.has(entry.name) && dir === root) continue;
      if (entry.isDirectory()) walk(join(dir, entry.name));
      else if (entry.name === 'index.html') {
        const html = readFileSync(join(dir, entry.name), 'utf8');
        if (html.includes('data-to="')) continue;
        const path = relative(root, dir).split(sep).join('/');
        pages.push(`/docs/${path ? `${path}/` : ''}`);
      }
    }
  };
  walk(root);
  return pages;
}

test('the inventory lists each page once, in a known cluster', () => {
  const rows = inventoryRows(readFileSync(IA, 'utf8'));
  assert.ok(rows.length >= 100, `the inventory has ${rows.length} rows`);
  const urls = rows.map((row) => pageUrl(row.new));
  assert.deepEqual(urls.filter((url, i) => urls.indexOf(url) !== i), []);
  assert.deepEqual(rows.filter((row) => !CLUSTERS.includes(row.cluster)), []);
});

test('the inventory pages are the built pages', {
  skip: !REQUIRE_DIST && !existsSync(DIST_DOCS) && 'no built site at website/dist (run `task website:build`)',
}, () => {
  assert.ok(existsSync(DIST_DOCS), 'WEBSITE_REQUIRE_DIST=1 but there is no built site at website/dist');
  const planned = inventoryRows(readFileSync(IA, 'utf8')).map((row) => pageUrl(row.new));
  const built = builtPages(DIST_DOCS);
  assert.deepEqual(built.filter((url, i) => built.indexOf(url) !== i), []);
  assert.deepEqual(planned.filter((url) => !built.includes(url)).sort(), []);
  assert.deepEqual(built.filter((url) => !planned.includes(url)).sort(), []);
});
