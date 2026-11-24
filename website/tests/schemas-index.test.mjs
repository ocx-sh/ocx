// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

// `schemas/index.html` links every published schema file; the deploy refuses a tree without a root index.

import assert from 'node:assert/strict';
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, relative, sep } from 'node:path';
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
// The published set, as the `schemas` genrule's `outs` in crates/ocx_schema/BUILD.bazel lists it: a
// stale version on disk must not stand in for a current one.
const PUBLISHED = [
  'config/v1.json',
  'errors/v1.json',
  'execution-record/v1.json',
  'metadata/v1.json',
  'patch/v1.json',
  'project-lock/v4.json',
  'project/v1.json',
  'reports/v1.json',
];
const REQUIRE_DIST = process.env.WEBSITE_REQUIRE_DIST === '1';

// Run by `task website:check` on the Taskfile build and inside the `//website:site` action on the
// Bazel one, so both routes are held to this one file set.
test('the built schemas tree is the schema files plus the index that links them', {
  skip: !REQUIRE_DIST && !existsSync(SCHEMAS) && 'no built site at website/dist (run `task website:build`)',
}, () => {
  const paths = schemaPaths(SCHEMAS);
  assert.deepEqual(paths, PUBLISHED);
  const files = readdirSync(SCHEMAS, { recursive: true, withFileTypes: true })
    .filter((e) => e.isFile())
    .map((e) => relative(SCHEMAS, join(e.parentPath, e.name)).split(sep).join('/'))
    .sort();
  assert.deepEqual(files, [...paths, 'index.html'].sort());
  const html = readFileSync(join(SCHEMAS, 'index.html'), 'utf8');
  for (const path of paths) assert.ok(html.includes(`href="${path}"`), `${path} not linked`);
});
