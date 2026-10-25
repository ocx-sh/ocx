// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { recentReleases } from './changelog.mjs';

const text = [
  '# Changelog',
  '',
  '## [3.0.0] - 2026-03-01',
  '',
  '- three',
  '',
  '## [2.0.0] - 2026-02-01',
  '',
  '- two',
  '',
  '## [1.0.0] - 2026-01-01',
  '',
  '- one',
  '[3.0.0]: https://example.test/3',
  '[2.0.0]: https://example.test/2',
  '[1.0.0]: https://example.test/1',
  '',
].join('\n');

test('keeps the header and the newest releases, drops the older ones', () => {
  const out = recentReleases(text, 2);
  assert.match(out, /^# Changelog\n/);
  assert.match(out, /- three/);
  assert.match(out, /- two/);
  assert.doesNotMatch(out, /- one/);
  assert.doesNotMatch(out, /## \[1\.0\.0\]/);
});

test('keeps every link definition, so the kept headings still resolve', () => {
  const out = recentReleases(text, 1);
  for (const v of ['3.0.0', '2.0.0', '1.0.0']) assert.match(out, new RegExp(`^\\[${v.replaceAll('.', '\\.')}\\]: https`, 'm'));
});

test('a changelog with no more releases than the count is returned whole', () => {
  assert.equal(recentReleases(text, 3), text);
  assert.equal(recentReleases(text, 10), text);
});
