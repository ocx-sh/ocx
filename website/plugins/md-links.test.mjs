// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { remarkMdLinks } from './md-links.mjs';

const ROOT = '/site/content/docs';
const OPTIONS = { root: ROOT, base: '/docs/' };

async function run(source, page = 'guide/quickstart.md') {
  const { unified } = await import('unified');
  const { default: remarkParse } = await import('remark-parse');
  const { VFile } = await import('vfile');
  const processor = unified().use(remarkParse).use(remarkMdLinks, OPTIONS);
  const file = new VFile({ value: source, path: `${ROOT}/${page}` });
  return processor.run(processor.parse(file), file);
}

const urls = (tree) => {
  const found = [];
  const walk = (node) => {
    if (node.type === 'link' || node.type === 'definition') found.push(node.url);
    node.children?.forEach(walk);
  };
  walk(tree);
  return found;
};

test('a relative .md link with a fragment becomes base + route + trailing slash + fragment', async () => {
  assert.deepEqual(urls(await run('[a](./install.md#cli)\n')), ['/docs/guide/install/#cli']);
});

test('a parent-relative link and an index page route to their directory', async () => {
  const tree = await run('[a](../reference/index.md) [b](../reference/environment.md)\n');
  assert.deepEqual(urls(tree), ['/docs/reference/', '/docs/reference/environment/']);
});

test('a reference-style definition is rewritten', async () => {
  assert.deepEqual(urls(await run('[a][ref]\n\n[ref]: ./install.md\n')), ['/docs/guide/install/']);
});

test('external, fragment-only and tree-leaving links are untouched', async () => {
  const tree = await run('[a](https://example.com/x.md) [b](#local) [c](../../../CHANGELOG.md)\n');
  assert.deepEqual(urls(tree), ['https://example.com/x.md', '#local', '../../../CHANGELOG.md']);
});

test('text inside a code fence and inline code is untouched', async () => {
  const tree = await run('[a](./x.md#a)\n\n```md\n[b](./x.md#a)\n```\n\n`[c](./x.md#a)`\n');
  assert.deepEqual(urls(tree), ['/docs/guide/x/#a']);
  const fence = tree.children.find((node) => node.type === 'code');
  assert.equal(fence.value, '[b](./x.md#a)');
});
