// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

// `schemas/index.html` links every published schema file; the deploy refuses a tree without a root index.

import assert from 'node:assert/strict';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { schemaPaths, schemasIndex } from '../scripts/schemas-index.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));

test('the index links each schema file, with its title', () => {
  const dir = mkdtempSync(join(tmpdir(), 'schemas-'));
  for (const [path, body] of [['a/v1.json', { title: 'A <1>' }], ['b/v2.json', {}]]) {
    mkdirSync(join(dir, dirname(path)), { recursive: true });
    writeFileSync(join(dir, path), JSON.stringify(body));
  }
  const html = schemasIndex(dir);
  assert.match(html, /<a href="a\/v1.json">a\/v1.json<\/a> &mdash; A &#60;1&#62;/);
  assert.match(html, /<a href="b\/v2.json">b\/v2.json<\/a><\/li>/);
});

const SCHEMAS = join(HERE, '..', 'dist', 'schemas');
const REQUIRE_DIST = process.env.WEBSITE_REQUIRE_DIST === '1';

test('the built index links every built schema', {
  skip: !REQUIRE_DIST && !existsSync(SCHEMAS) && 'no built site at website/dist (run `task website:build`)',
}, () => {
  const paths = schemaPaths(SCHEMAS);
  assert.ok(paths.length >= 8, `${paths.length} schemas built`);
  const html = readFileSync(join(SCHEMAS, 'index.html'), 'utf8');
  for (const path of paths) assert.ok(html.includes(`href="${path}"`), `${path} not linked`);
});
