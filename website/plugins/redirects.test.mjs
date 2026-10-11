// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { runInNewContext } from 'node:vm';

import { FORWARD, resolveTarget } from './redirects.mjs';

const pages = {
  '/docs/a/': { to: '/docs/b/', fragments: { x: '/docs/b/#y' } },
  '/docs/b/': { fragments: { y: '/docs/c/#z' } },
  '/docs/c/': { fragments: {} },
  '/docs/loop/': { fragments: { p: '/docs/loop/#q', q: '/docs/loop/#p' } },
};

test('a page move keeps the fragment', () => {
  assert.equal(resolveTarget(pages, '/docs/a/'), '/docs/b/');
  assert.equal(resolveTarget(pages, '/docs/a/#other'), '/docs/b/#other');
});

test('a fragment key in another map is followed', () => {
  assert.equal(resolveTarget(pages, '/docs/a/#x'), '/docs/c/#z');
  assert.equal(resolveTarget(pages, '/docs/b/#y'), '/docs/c/#z');
});

test('an unmoved target is returned unchanged', () => {
  assert.equal(resolveTarget(pages, '/docs/c/#z'), '/docs/c/#z');
  assert.equal(resolveTarget(pages, '/docs/elsewhere/#z'), '/docs/elsewhere/#z');
});

test('a cycle throws', () => {
  assert.throws(() => resolveTarget(pages, '/docs/loop/#p'), /redirects: cycle/);
});

test('the stub script forwards a malformed fragment instead of throwing', () => {
  const replaced = [];
  const script = { textContent: '{}', getAttribute: () => '/docs/b/' };
  runInNewContext(FORWARD, {
    document: { getElementById: () => script },
    location: { hash: '#%E0%A4%A', replace: (url) => replaced.push(url) },
  });
  assert.deepEqual(replaced, ['/docs/b/#%E0%A4%A']);
});
