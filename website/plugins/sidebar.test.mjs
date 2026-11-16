// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

// The sidebar is explicit in `astro.config.mjs`, so a page nobody lists is unreachable from the navigation.
// Every content page appears in it unless it is named below. The config is read as text: importing it runs
// the changelog mirror and the Astro config, neither of which a test needs.

import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, relative } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const DOCS = join(ROOT, 'src', 'content', 'docs');

// Pages the sidebar leaves out on purpose: path under the content root -> reason. None today: the landing page,
// the changelog and the generated pages are all listed.
const ABSENT = new Map([]);

/** The page routes (`/guides/x/`) the sidebar config links. */
export function sidebarLinks(config) {
  return new Set([...config.matchAll(/\bpage\(\s*'[^']*'\s*,\s*'(\/[^']*)'\s*\)/g)].map((m) => m[1]));
}

/** The route of a content page path: `guides/x.mdx` -> `/guides/x/`, `index.mdx` -> `/`. */
export function pageRoute(rel) {
  const dir = rel.replace(/\.mdx?$/, '').replace(/(^|\/)index$/, '');
  return dir ? `/${dir}/` : '/';
}

function pagesUnder(dir) {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return pagesUnder(path);
    return /\.mdx?$/.test(entry.name) ? [path] : [];
  });
}

/** Content pages (relative paths) whose route the sidebar does not link and that are not named in `absent`. */
export function unreachable(config, docsDir, absent = ABSENT) {
  const linked = sidebarLinks(config);
  return pagesUnder(docsDir)
    .map((file) => relative(docsDir, file).split('\\').join('/'))
    .filter((rel) => !absent.has(rel) && !linked.has(pageRoute(rel)))
    .sort();
}

const CONFIG = readFileSync(join(ROOT, 'astro.config.mjs'), 'utf8');

test('every content page is in the sidebar unless it is named as absent', () => {
  assert.ok(sidebarLinks(CONFIG).size >= 100, `the config links ${sidebarLinks(CONFIG).size} pages`);
  assert.deepEqual(unreachable(CONFIG, DOCS), []);
});

test('an absent page is a real page, and a page the sidebar drops is reported', () => {
  const pages = new Set(pagesUnder(DOCS).map((f) => relative(DOCS, f).split('\\').join('/')));
  for (const rel of ABSENT.keys()) assert.ok(pages.has(rel), `${rel} is listed as absent but does not exist`);
  const dropped = CONFIG.replace(/\bpage\(\s*'[^']*'\s*,\s*'\/guides\/choose-versions\/'\s*\)/, "page('x', '/x/')");
  assert.notEqual(dropped, CONFIG, 'the mutation landed');
  assert.deepEqual(unreachable(dropped, DOCS), ['guides/choose-versions.mdx']);
});

test('a page planted in a temporary docs tree is reported, a listed one is not', () => {
  const dir = mkdtempSync(join(tmpdir(), 'sidebar-'));
  try {
    mkdirSync(join(dir, 'guides'));
    writeFileSync(join(dir, 'guides', 'planted.md'), '---\ntitle: x\n---\n');
    writeFileSync(join(dir, 'index.mdx'), '---\ntitle: x\n---\n');
    assert.deepEqual(unreachable(CONFIG, dir), ['guides/planted.md']);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
