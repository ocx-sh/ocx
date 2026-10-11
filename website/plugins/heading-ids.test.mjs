// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { remarkHeadingIds } from './heading-ids.mjs';

// The parser packages arrive with the site's package.json; import them when a test runs.
async function headings(source, { mdx = false } = {}) {
  const { unified } = await import('unified');
  const { default: remarkParse } = await import('remark-parse');
  const { VFile } = await import('vfile');
  const processor = unified().use(remarkParse);
  if (mdx) processor.use((await import('remark-mdx')).default);
  processor.use(remarkHeadingIds);
  const file = new VFile({ value: source, path: '/docs/page.md' });
  const tree = await processor.run(processor.parse(file), file);
  return tree.children.filter((node) => node.type === 'heading');
}

const text = (node) => node.children.map((child) => child.value ?? '').join('');

test('markdown {#id} becomes the heading id and leaves the text', async () => {
  const [heading] = await headings('## Install the CLI {#install-cli}\n');
  assert.equal(heading.data?.hProperties?.id, 'install-cli');
  assert.equal(text(heading), 'Install the CLI');
});

test('MDX \\{#id\\} becomes the heading id and leaves the text', async () => {
  const [heading] = await headings('## Install the CLI \\{#install-cli\\}\n', { mdx: true });
  assert.equal(heading.data?.hProperties?.id, 'install-cli');
  assert.equal(text(heading), 'Install the CLI');
});

test('a suffix after inline code drops the whole trailing text node', async () => {
  const [heading] = await headings('### `OCX_HOME` {#ocx-home}\n');
  assert.equal(heading.data?.hProperties?.id, 'ocx-home');
  assert.deepEqual(
    heading.children.map((child) => child.type),
    ['inlineCode'],
  );
});

test('a heading without a suffix is untouched', async () => {
  const [heading] = await headings('## Plain heading\n');
  assert.equal(heading.data, undefined);
  assert.equal(text(heading), 'Plain heading');
});

test('a brace group that is not a trailing id is untouched', async () => {
  const [heading] = await headings('## Set {#not-last} twice\n');
  assert.equal(heading.data, undefined);
  assert.equal(text(heading), 'Set {#not-last} twice');
});
