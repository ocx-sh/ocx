// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

import { visit } from 'unist-util-visit';

/**
 * Remark plugin: a heading whose last text ends in ` {#id}` (markdown) or the escaped ` \{#id\}` (MDX, where the
 * parser already decoded the escapes) gets `data.hProperties.id = id`, and the suffix leaves the heading text.
 * A last text node that is only the suffix is dropped. Headings without a suffix are left untouched.
 *
 * @returns {import('unified').Plugin<[], import('mdast').Root>}
 */
export function remarkHeadingIds() {
  return (tree) => {
    visit(tree, 'heading', (heading) => {
      const last = heading.children.at(-1);
      const match = last?.type === 'text' ? /\s*\{#([^\s{}]+)\}\s*$/.exec(last.value) : null;
      if (!match) return;
      const id = match[1];
      last.value = last.value.slice(0, match.index);
      if (last.value === '') heading.children.pop();
      heading.data = { ...heading.data, id, hProperties: { ...heading.data?.hProperties, id } };
    });
  };
}
