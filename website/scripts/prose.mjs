// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

// Page walker and prose masker shared by the docs lint tests (`tests/*.test.mjs`).

import { readdirSync, statSync } from 'node:fs';
import { join } from 'node:path';

/** Every `.md`/`.mdx` file under `dir`, as a sorted list of absolute paths. */
export function pagesUnder(dir) {
  const out = [];
  for (const name of readdirSync(dir).sort()) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) out.push(...pagesUnder(path));
    else if (/\.mdx?$/.test(name)) out.push(path);
  }
  return out;
}

/**
 * Blank `text` except newlines, so offsets and line numbers survive. The filler is not whitespace: the matcher
 * reads any whitespace run as one space, so a space would join `floating`, a code span and `tag` into one form.
 */
const blank = (s) => s.replace(/[^\n]/g, '\uE000');

/** Prose of an MDX/MD source with everything that is not prose replaced by spaces; offsets and lines are preserved. */
export function maskProse(source, { keepAttrs = false } = {}) {
  let text = source;
  // Front matter.
  // `keepAttrs` leaves the `title`/`description` values and the text attributes of components readable.
  const keepKey = /^(?:title|description|label):/;
  text = text.replace(/^---\n[\s\S]*?\n---(?=\n|$)/, (fm) =>
    keepAttrs ? fm.split('\n').map((l) => (keepKey.test(l) ? l : blank(l))).join('\n') : blank(fm),
  );
  // Fenced code blocks (CommonMark): a fence closes on the opener's character, at least as long, nothing after it.
  const lines = text.split('\n');
  let fence = null;
  for (let i = 0; i < lines.length; i++) {
    const m = /^\s*(`{3,}|~{3,})/.exec(lines[i]);
    if (fence) {
      const closes = m && m[1][0] === fence.ch && m[1].length >= fence.len && /^\s*(`+|~+)\s*$/.test(lines[i]);
      if (closes) fence = null;
      lines[i] = blank(lines[i]);
    } else if (m) {
      fence = { ch: m[1][0], len: m[1].length };
      lines[i] = blank(lines[i]);
    } else if (/^\[[^\]]+\]:\s/.test(lines[i]) || /^(import|export)\s/.test(lines[i])) {
      lines[i] = blank(lines[i]);
    } else {
      lines[i] = lines[i]
        .replace(/`[^`]*`/g, blank)
        .replace(/\\?\{#[\w-]+\\?\}/g, blank)
        .replace(/\]\([^)]*\)/g, blank);
    }
  }
  text = lines.join('\n');
  // MDX and HTML comments, then tags with their attributes (attribute strings may hold `>`). A tag never spans
  // a blank line, so a stray `<` in prose does not swallow the paragraphs up to the next `>`.
  const nl = String.raw`\n(?![ \t]*\n)`;
  const tag = new RegExp(
    String.raw`<\/?[A-Za-z][\w.:-]*(?:"(?:[^"\n]|${nl})*"|'(?:[^'\n]|${nl})*'|\{(?:[^}\n]|${nl})*\}|${nl}|[^>"'{\n])*>`,
    'g',
  );
  text = text.replace(/\{\/\*[\s\S]*?\*\/\}/g, blank).replace(/<!--[\s\S]*?-->/g, blank).replace(/<code>[\s\S]*?<\/code>/g, blank);
  const keepText = /\b(?:title|alt|label|aria-label)=("[^"]*"|'[^']*')/g;
  return text.replace(tag, (t) => {
    if (!keepAttrs) return blank(t);
    const out = [...blank(t)];
    for (const m of t.matchAll(keepText)) {
      const at = m.index + m[0].length - m[1].length + 1;
      for (let i = 0; i < m[1].length - 2; i++) out[at + i] = t[at + i];
    }
    return out.join('');
  });
}

