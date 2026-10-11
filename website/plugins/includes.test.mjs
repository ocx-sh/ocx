// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, relative } from 'node:path';
import { test } from 'node:test';

import { remarkIncludes } from './includes.mjs';

function site() {
  // <repo>/website/src: the include root is two levels up, as in the real site.
  const srcDir = join(mkdtempSync(join(tmpdir(), 'includes-')), 'website', 'src');
  mkdirSync(join(srcDir, '_scripts'), { recursive: true });
  mkdirSync(join(srcDir, 'docs'));
  writeFileSync(join(srcDir, '_scripts', 'demo.sh'), '#!/bin/sh\necho demo\n');
  writeFileSync(join(srcDir, 'snippet.md'), 'dropped one\ndropped two\n# Kept heading\n\nkept body\n');
  return srcDir;
}

async function run(srcDir, source) {
  const { unified } = await import('unified');
  const { default: remarkParse } = await import('remark-parse');
  const { VFile } = await import('vfile');
  const processor = unified().use(remarkParse).use(remarkIncludes, { srcDir });
  const file = new VFile({ value: source, path: join(srcDir, 'docs', 'page.md') });
  return processor.run(processor.parse(file), file);
}

test('<<< @/path{lang} becomes a fenced block of the file', async () => {
  const tree = await run(site(), '<<< @/_scripts/demo.sh{sh}\n');
  assert.equal(tree.children.length, 1);
  assert.deepEqual(
    { type: tree.children[0].type, lang: tree.children[0].lang, value: tree.children[0].value },
    { type: 'code', lang: 'sh', value: '#!/bin/sh\necho demo' },
  );
});

test('<!--@include: path{3,}--> splices lines 3 to the end as markdown', async () => {
  const tree = await run(site(), '<!--@include: ../snippet.md{3,}-->\n');
  assert.deepEqual(
    tree.children.map((node) => node.type),
    ['heading', 'paragraph'],
  );
  assert.equal(tree.children[0].children[0].value, 'Kept heading');
  assert.equal(tree.children[1].children[0].value, 'kept body');
});

test('an include without a range takes the whole file', async () => {
  const tree = await run(site(), '<!--@include: ../snippet.md-->\n');
  assert.equal(tree.children[0].type, 'paragraph');
  assert.match(tree.children[0].children[0].value, /^dropped one/);
});

test('a missing file is an error that names the path', async () => {
  await assert.rejects(run(site(), '<<< @/_scripts/absent.sh{sh}\n'), /absent\.sh/);
});

function secret() {
  const path = join(mkdtempSync(join(tmpdir(), 'outside-')), 'secret.txt');
  writeFileSync(path, 'private\n');
  return path;
}

test('an absolute code-include path outside the repository is rejected', async () => {
  const srcDir = site();
  await assert.rejects(run(srcDir, `<<< @/${secret()}{txt}\n`), /outside the repository/);
});

test('a .. escape out of the repository is rejected', async () => {
  const srcDir = site();
  const path = secret();
  const escape = relative(srcDir, path);
  await assert.rejects(run(srcDir, `<<< @/${escape}{txt}\n`), /outside the repository/);
  // An `<!--@include:-->` path resolves against the including page's directory.
  const pageEscape = relative(join(srcDir, 'docs'), path);
  await assert.rejects(run(srcDir, `<!--@include: ${pageEscape}-->\n`), /outside the repository/);
});

test('an include elsewhere inside the repository still works', async () => {
  const srcDir = site();
  writeFileSync(join(srcDir, '..', '..', 'CHANGELOG.md'), 'top\n');
  const tree = await run(srcDir, '<<< @/../../CHANGELOG.md{md}\n');
  assert.equal(tree.children[0].value, 'top');
});
