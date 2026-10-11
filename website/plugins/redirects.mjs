// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

// Old docs URLs keep resolving. `redirects.d/*.json` maps an old page URL to its new home:
// `{ "<old url>": { "to": "<new url>", "fragments": { "<old fragment>": "<new url>#<id>" } } }`.
// A page with `to` no longer exists, so a stub stands at its old URL. A page without `to` still builds and only
// gains the fragment map. Both forward `location.hash` through the `ocx-fragment-map` JSON.

import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

/**
 * Merges every `*.json` in `dir`. The same `to`, or the same (page, fragment) pair, in two files is an error;
 * one page spread over several files with disjoint fragments is allowed.
 *
 * @returns {Record<string, { to?: string, fragments: Record<string, string> }>}
 */
export function loadRedirects(dir) {
  const pages = {};
  const owner = new Map();
  for (const file of readdirSync(dir).filter((name) => name.endsWith('.json')).sort()) {
    const doc = JSON.parse(readFileSync(join(dir, file), 'utf8'));
    for (const [page, { to, fragments = {} }] of Object.entries(doc)) {
      const entry = (pages[page] ??= { fragments: {} });
      const claim = (key, value, slot) => {
        if (owner.has(key)) throw new Error(`redirects: ${key} is set in ${owner.get(key)} and ${file}`);
        owner.set(key, file);
        slot(value);
      };
      if (to !== undefined) claim(`${page} to`, to, (value) => (entry.to = value));
      for (const [fragment, target] of Object.entries(fragments)) {
        claim(`${page}#${fragment}`, target, (value) => (entry.fragments[fragment] = value));
      }
    }
  }
  return pages;
}

/** One hop: a page's fragment map wins, else its `to` keeps the fragment; `url` itself when nothing moves it. */
function hop(pages, url) {
  const hash = url.indexOf('#');
  const page = hash < 0 ? url : url.slice(0, hash);
  const fragment = hash < 0 ? undefined : url.slice(hash + 1);
  const entry = pages[page];
  if (!entry) return url;
  if (fragment !== undefined && Object.hasOwn(entry.fragments, fragment)) return entry.fragments[fragment];
  if (!entry.to) return url;
  return fragment === undefined ? entry.to : `${entry.to}#${fragment}`;
}

/**
 * `url` followed through the page moves and the fragment maps until nothing moves it further. A (page, fragment)
 * that is itself a key in another map is followed too; a chain that returns to a URL it has been through throws.
 */
export function resolveTarget(pages, url) {
  const path = [url];
  for (let next = hop(pages, url); next !== path.at(-1); next = hop(pages, next)) {
    if (path.includes(next)) throw new Error(`redirects: cycle ${[...path, next].join(' -> ')}`);
    path.push(next);
  }
  return path.at(-1);
}

const escapeAttr = (text) => text.replaceAll('&', '&amp;').replaceAll('"', '&quot;').replaceAll('<', '&lt;');

// Reads the map, then replaces the location: the mapped target for the hash, else (stubs) the page's `to`.
export const FORWARD =
  "(function(){var s=document.getElementById('ocx-fragment-map'),m=JSON.parse(s.textContent)," +
  "h=(function(x){try{return decodeURIComponent(x)}catch(e){return x}})(location.hash.slice(1)),to=s.getAttribute('data-to')," +
  "t=Object.prototype.hasOwnProperty.call(m,h)?m[h]:to?to+location.hash:'';if(t)location.replace(t)})();";

function mapScript(map, to) {
  const json = JSON.stringify(map).replaceAll('<', '\\u003c');
  const data = to ? ` data-to="${escapeAttr(to)}"` : '';
  return `<script id="ocx-fragment-map" type="application/json"${data}>${json}</script><script>${FORWARD}</script>`;
}

function stub(to, map) {
  const href = escapeAttr(to);
  return (
    '<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Moved</title>' +
    `<meta name="robots" content="noindex"><link rel="canonical" href="${href}">` +
    `<noscript><meta http-equiv="refresh" content="0; url=${href}"></noscript>${mapScript(map, to)}` +
    `</head><body><p>This page moved to <a href="${href}">${href}</a>.</p></body></html>`
  );
}

/**
 * Astro integration: reads `dir` at config time (a collision fails the build) and writes the stubs and fragment
 * maps into the output once the build is done.
 *
 * @param {{ dir: string, base?: string }} options `dir` holds the `*.json` files; `base` is the site path of the output.
 */
export function ocxRedirects({ dir, base = '/docs/' }) {
  let pages = {};
  const file = (url) => {
    if (!url.startsWith(base)) throw new Error(`redirects: ${url} is outside ${base}`);
    return join(url.slice(base.length), 'index.html');
  };
  return {
    name: 'ocx-redirects',
    hooks: {
      'astro:config:setup': () => {
        pages = loadRedirects(dir);
      },
      'astro:build:done': async ({ dir: out }) => {
        const root = fileURLToPath(out);
        const resolved = {};
        const missing = [];
        for (const [url, entry] of Object.entries(pages)) {
          const targets = { ...entry.fragments };
          for (const [fragment, target] of Object.entries(targets)) targets[fragment] = resolveTarget(pages, target);
          const final = entry.to ? { to: resolveTarget(pages, entry.to), ...targets } : targets;
          for (const [what, target] of Object.entries(final)) {
            const page = target.split('#')[0];
            if (!existsSync(join(root, file(page)))) missing.push(`${url} ${what === 'to' ? 'to' : `#${what}`} -> ${target}`);
          }
          resolved[url] = { to: final.to, map: targets };
        }
        if (missing.length > 0) {
          throw new Error(`redirects: target page not built:\n  ${missing.join('\n  ')}`);
        }
        for (const [url, { to, map }] of Object.entries(resolved)) {
          const path = join(root, file(url));
          const built = existsSync(path);
          if (to) {
            if (built) throw new Error(`redirects: ${url} moves to ${to} but is still built`);
            await mkdir(dirname(path), { recursive: true });
            await writeFile(path, stub(to, map));
          } else {
            if (!built) throw new Error(`redirects: ${url} has fragments but is not built and has no "to"`);
            if (Object.keys(map).length === 0) continue;
            const html = await readFile(path, 'utf8');
            await writeFile(path, html.replace('</head>', () => `${mapScript(map)}</head>`));
          }
        }
      },
    },
  };
}
