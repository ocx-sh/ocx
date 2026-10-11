// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

import { readFileSync, realpathSync } from 'node:fs';
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';

const CODE_INCLUDE = /^<<<\s+@\/(\S+?)(?:\{(\S+)\})?$/;
const MD_INCLUDE = /^<!--@include:\s*(\S+?)(?:\{(\d+),\})?\s*-->$/;

// An include reads only inside `root`: an absolute path or a `..` run must not pull any file into a public page.
function read(path, root) {
  let real;
  try {
    real = realpathSync(path);
  } catch (cause) {
    throw new Error(`include: cannot read ${path}`, { cause });
  }
  const rel = relative(root, real);
  if (rel === '..' || rel.startsWith(`..${sep}`) || isAbsolute(rel)) {
    throw new Error(`include: ${path} is outside the repository`);
  }
  return readFileSync(real, 'utf8');
}

/**
 * Remark plugin for the two include forms the pages use.
 *
 * - A paragraph that is only `<<< @/<path>{lang}` becomes a `code` node with `lang` and the file's text, one
 *   trailing newline removed. `@` is `srcDir`.
 * - An `html` node that is only `<!--@include: <path>{N,}-->` is replaced by the file's lines N to the end
 *   (1-based), parsed as markdown. `<path>` is relative to the including file; `{N,}` is optional.
 *
 * @param {{ srcDir: string }} options `srcDir` is the absolute directory `@` names; includes must stay inside its
 * grandparent, the repository root (`website/src` -> the repository).
 * @returns {import('unified').Plugin<[], import('mdast').Root>}
 */
export function remarkIncludes({ srcDir }) {
  const processor = this;
  const root = realpathSync(resolve(srcDir, '..', '..'));
  // A replacement list for the node, or null to keep it and descend.
  const expand = (node, dir) => {
    if (node.type === 'paragraph' && node.children.length === 1 && node.children[0].type === 'text') {
      const code = CODE_INCLUDE.exec(node.children[0].value.trim());
      if (code) {
        const value = read(resolve(srcDir, code[1]), root).replace(/\n$/, '');
        return [{ type: 'code', lang: code[2] ?? null, meta: null, value }];
      }
    }
    if (node.type === 'html') {
      const md = MD_INCLUDE.exec(node.value.trim());
      if (md) {
        const path = join(dir, md[1]);
        const text = read(path, root).split('\n').slice(md[2] ? Number(md[2]) - 1 : 0).join('\n');
        const included = processor.parse(text);
        walk(included, dirname(path));
        return included.children;
      }
    }
    return null;
  };
  const walk = (parent, dir) => {
    if (!parent.children) return;
    parent.children = parent.children.flatMap((child) => {
      const replacement = expand(child, dir);
      if (replacement) return replacement;
      walk(child, dir);
      return [child];
    });
  };
  return (tree, file) => walk(tree, dirname(file.path));
}
