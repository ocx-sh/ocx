// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

// The changelog carries a character outside the theme's preloaded Latin subset (the "ä" in a commit subject), so the
// browser fetches the "rest" font only after layout: one extra round trip that pushes LCP past the budget.
// The bundler content-hashes the font's file name, so the preload is written into the built page instead of
// the page's frontmatter, where a theme update that renames the file would go stale.

import { readdirSync } from 'node:fs';
import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const FONT = /^plex-sans-400-rest\.[\w-]+\.woff2$/;

/** Inserts the preload `<link>` for the built font `assets` names, before `</head>` of `html`. */
export function withFontPreload(html, href) {
  const link = `<link rel="preload" href="${href}" as="font" type="font/woff2" crossorigin>`;
  if (!html.includes('</head>')) throw new Error('preload-font: page has no </head>');
  return html.replace('</head>', () => `${link}</head>`);
}

/** Astro integration: preloads the theme's "rest" font on the pages in `pages` (paths under the output dir). */
export function ocxPreloadFont({ pages, base = '/docs/' }) {
  return {
    name: 'ocx-preload-font',
    hooks: {
      'astro:build:done': async ({ dir }) => {
        const root = fileURLToPath(dir);
        const fonts = readdirSync(join(root, '_astro')).filter((name) => FONT.test(name));
        if (fonts.length !== 1) throw new Error(`preload-font: expected one _astro/plex-sans-400-rest.*.woff2, found ${fonts.length}`);
        for (const page of pages) {
          const path = join(root, page, 'index.html');
          await writeFile(path, withFontPreload(await readFile(path, 'utf8'), `${base}_astro/${fonts[0]}`));
        }
      },
    },
  };
}
