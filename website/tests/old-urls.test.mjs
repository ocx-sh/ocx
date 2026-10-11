// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

// Every URL the old site served still resolves. `old-urls.txt` lists one old page URL or `page#anchor`
// per line. A URL resolves when its page is built, or a stub stands there whose `ocx-fragment-map` JSON
// (`{"<old fragment>": "<new url>#<id>"}`) names the new home of the fragment; the fragment must then exist as an
// `id` on the page the map names.

import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const FIXTURE = join(HERE, 'fixtures', 'old-urls');
const DIST = join(FIXTURE, 'dist');

/** The non-empty lines of an `old-urls.txt`. */
function readOldUrls(file) {
  return readFileSync(file, 'utf8')
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean);
}

/** The built page for a site URL: its `index.html` text, or `null` when no page stands there. */
function readPage(dist, url) {
  const file = join(dist, url, 'index.html');
  return existsSync(file) ? readFileSync(file, 'utf8') : null;
}

const hasId = (html, id) => html.includes(`id="${id}"`);

/** The page's `ocx-fragment-map` JSON, or `null` when it has none. */
function fragmentMap(html) {
  const match = /<script id="ocx-fragment-map"[^>]*>([\s\S]*?)<\/script>/.exec(html);
  return match ? JSON.parse(match[1]) : null;
}

/**
 * Problems for `urls` over the built site at `dist`, in input order, one `"<url>: <reason>"` each. Reasons:
 * `no page`, `no fragment map entry`, `fragment missing on <page url>`.
 */
function checkOldUrls(dist, urls) {
  const problems = [];
  for (const url of urls) {
    const [page, fragment] = url.split('#');
    const html = readPage(dist, page);
    if (html === null) {
      problems.push(`${url}: no page`);
      continue;
    }
    if (fragment === undefined) continue;
    const map = fragmentMap(html);
    if (map && Object.hasOwn(map, fragment)) {
      const [targetPage, targetFragment] = map[fragment].split('#');
      const target = readPage(dist, targetPage);
      if (target === null) problems.push(`${url}: no page`);
      else if (targetFragment !== undefined && !hasId(target, targetFragment)) {
        problems.push(`${url}: fragment missing on ${targetPage}`);
      }
    } else if (hasId(html, fragment)) {
      continue;
    } else {
      problems.push(map ? `${url}: no fragment map entry` : `${url}: fragment missing on ${page}`);
    }
  }
  return problems;
}

test('the url list is read', () => {
  assert.equal(readOldUrls(join(FIXTURE, 'old-urls.txt')).length, 4);
});

test('every old url resolves over the fixture dist', () => {
  assert.deepEqual(checkOldUrls(DIST, readOldUrls(join(FIXTURE, 'old-urls.txt'))), []);
});

test('a missing page, an unmapped fragment and a missing id are each named', () => {
  assert.deepEqual(checkOldUrls(DIST, readOldUrls(join(FIXTURE, 'old-urls-broken.txt'))), [
    '/docs/missing/: no page',
    '/docs/installation/#nope: no fragment map entry',
    '/docs/getting-started/#gone: fragment missing on /docs/getting-started/',
  ]);
});

const SITE_DIST = join(HERE, '..', 'dist');
// The gate sets it after building: a missing dist then fails instead of skipping.
const REQUIRE_DIST = process.env.WEBSITE_REQUIRE_DIST === '1';

test('every old url of the old site resolves over the built site', {
  skip: !REQUIRE_DIST && !existsSync(join(SITE_DIST, 'docs')) && 'no built site at website/dist (run `task website:build`)',
}, () => {
  assert.ok(existsSync(join(SITE_DIST, 'docs')), 'WEBSITE_REQUIRE_DIST=1 but there is no built site at website/dist');
  const urls = readOldUrls(join(HERE, '..', 'old-urls.txt'));
  assert.ok(urls.length > 1000, `old-urls.txt lists ${urls.length} urls`);
  assert.deepEqual(checkOldUrls(SITE_DIST, urls), []);
});
