// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

import { posix, relative, sep } from 'node:path';

import { visit } from 'unist-util-visit';

const PAGE_LINK = /^([^#?]+)\.mdx?([#?].*)?$/;
const EXTERNAL = /^[a-z][a-z0-9+.-]*:|^\/\//i;

/** The route a `.md` link target names, keeping its `#fragment`/`?query`; the target itself when it is not one. */
function route(target, page, base) {
  const match = PAGE_LINK.exec(target);
  if (!match || EXTERNAL.test(target)) return target;
  const path = match[1].startsWith('/')
    ? posix.normalize(match[1].slice(1))
    : posix.join(posix.dirname(page), match[1]);
  if (path.startsWith('..')) return target;
  const dir = path.replace(/(^|\/)index$/, '');
  return `${base}${dir ? `${dir}/` : ''}${match[2] ?? ''}`;
}

/**
 * Remark plugin: rewrites `link` and `definition` URLs that name a `.md`/`.mdx` page inside the docs tree to the
 * page's route, `base` + directory + trailing slash, keeping the `#fragment`. `index` pages route to their
 * directory. External URLs, bare `#fragment` links and targets leaving the tree are left alone. Code and inline
 * code are never touched.
 *
 * @param {{ root: string, base: string }} options `root` is the absolute docs content root; `base` the site path
 *   of that root, for example `/docs/`. The page's own path is `file.path` relative to `root`.
 * @returns {import('unified').Plugin<[], import('mdast').Root>}
 */
export function remarkMdLinks({ root, base }) {
  const prefix = base.endsWith('/') ? base : `${base}/`;
  return (tree, file) => {
    const page = relative(root, file.path).split(sep).join('/');
    if (page.startsWith('..')) return;
    visit(tree, ['link', 'definition'], (node) => {
      node.url = route(node.url, page, prefix);
    });
  };
}
