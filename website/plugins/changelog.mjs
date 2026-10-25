// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

// The changelog page renders the newest releases only; older ones live on the GitHub releases page. One page
// holding every release is thousands of DOM nodes, far past the Lighthouse DOM budget.

/**
 * `text` (the generated CHANGELOG.md) cut to its header and the first `count` `## [x.y.z]` sections. Every link
 * definition is kept, since the kept headings are references. Returned whole when there are no more than `count`.
 *
 * @param {string} text
 * @param {number} count
 */
export function recentReleases(text, count) {
  const lines = text.split('\n');
  const headings = lines.flatMap((line, i) => (line.startsWith('## [') ? [i] : []));
  if (headings.length <= count) return text;
  const definitions = lines.filter((line) => /^\[[^\]]+\]: /.test(line));
  return [...lines.slice(0, headings[count]), ...definitions, ''].join('\n');
}
