// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { withFontPreload } from './preload-font.mjs';

test('the preload link lands before </head>', () => {
  const out = withFontPreload('<html><head><title>x</title></head><body></body></html>', '/docs/_astro/f.abc.woff2');
  assert.match(out, /<title>x<\/title><link rel="preload" href="\/docs\/_astro\/f\.abc\.woff2" as="font" type="font\/woff2" crossorigin><\/head>/);
});

test('a page without </head> is an error', () => {
  assert.throws(() => withFontPreload('<p>x</p>', '/a.woff2'), /no <\/head>/);
});
