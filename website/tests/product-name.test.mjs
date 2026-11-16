// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

// In prose the product is "OCX"; lowercase `ocx` is only the command, in code font (docs-style.md § Terms).
// Every page is read, the generated reference pages and `title`/`description` values included.

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join, relative } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { maskProse, pagesUnder } from '../scripts/prose.mjs';

const DOCS = join(dirname(fileURLToPath(import.meta.url)), '..', 'src', 'content', 'docs');
// Reader floors, just under the real counts: a scan that loses pages or the product name must fail.
const MIN_PAGES = 97;
const MIN_PRODUCT = 500;
// The changelog quotes release notes; the SBOM page is git-ignored (present locally, absent in CI).
const SKIPPED = ['changelog.md', 'reference/dependencies.md'];

// A bare word: not part of a file name, path, package name, domain or identifier.
const BARE = /(?<![\w./~@-])ocx(?![\w/-]|\.\w)/g;

// A command page is titled by its command (`ocx config`), and a title cannot carry code font:
// those exact titles count as the command wherever a page quotes them.
const COMMAND_TITLES = pagesUnder(join(DOCS, 'reference', 'command-line'))
  .map((f) => /^title: "(ocx [^"]+)"$/m.exec(readFileSync(f, 'utf8'))?.[1])
  .filter(Boolean);

/** `{ line, text }` for each bare lowercase `ocx` in the prose of one source. */
function lowercaseUses(source, commandTitles = COMMAND_TITLES) {
  let prose = maskProse(source, { keepAttrs: true });
  for (const title of commandTitles) prose = prose.split(title).join(title.replace(/ocx/g, 'OCX'));
  return [...prose.matchAll(BARE)].map((m) => ({
    line: prose.slice(0, m.index).split('\n').length,
    text: prose.split('\n')[prose.slice(0, m.index).split('\n').length - 1].trim(),
  }));
}

test('no page writes the product as lowercase ocx in prose', () => {
  const pages = pagesUnder(DOCS).filter((f) => !SKIPPED.includes(relative(DOCS, f).split('\\').join('/')));
  let product = 0;
  const report = [];
  for (const file of pages) {
    const source = readFileSync(file, 'utf8');
    product += (maskProse(source, { keepAttrs: true }).match(/\bOCX\b/g) ?? []).length;
    for (const hit of lowercaseUses(source)) report.push(`${relative(DOCS, file)}:${hit.line}: ${hit.text}`);
  }
  assert.ok(pages.length >= MIN_PAGES, `read ${pages.length} pages, floor is ${MIN_PAGES}`);
  assert.ok(product >= MIN_PRODUCT, `saw ${product} "OCX", floor is ${MIN_PRODUCT}`);
  assert.equal(report.length, 0, `\nwrite "OCX" for the product, \`ocx\` for the command:\n${report.join('\n')}`);
});

test('the scan flags prose and keeps code, paths, names and links out', () => {
  const clean = [
    '---\ntitle: Install OCX\n---',
    'Run `ocx exec`, edit ocx.toml or ~/.ocx, see ghcr.io/ocx-sh/ocx and ocx-sh, setup-ocx, ocx.sh.',
    '[x](https://ocx.sh/ocx) <Node name="ocx/" />',
    '```\nocx install\n```',
  ].join('\n');
  assert.deepEqual(lowercaseUses(clean), []);
  assert.deepEqual(lowercaseUses('---\ntitle: Install ocx\n---\n').map((h) => h.line), [2]);
  assert.deepEqual(lowercaseUses('a\n\nocx resolves it').map((h) => h.line), [3]);
  assert.deepEqual(lowercaseUses('<ShellSession title="Run ocx" />').map((h) => h.line), [1]);
  assert.ok(COMMAND_TITLES.length >= 6, `found ${COMMAND_TITLES.length} command page titles`);
  assert.deepEqual(lowercaseUses('---\ntitle: "ocx config"\n---\n', ['ocx config']), []);
  assert.deepEqual(lowercaseUses('---\ntitle: "ocx config"\n---\n', []).map((h) => h.line), [2]);
});
