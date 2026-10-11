// Lighthouse budgets and page discovery for `pnpm run lighthouse` (scripts/lighthouse.mjs).
// Values are the `content` class of ocx-sh/website tests/budgets.mjs and never go above it;
// the docs are plain pages, so there is one class.
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { gzipSync } from 'node:zlib';

const KB = 1024;

/** Served under this path on ocx.sh (astro.config.mjs `base`). */
export const BASE = '/docs/';

/** Bytes, gzip of the response body, except `domElements`. */
export const BUDGET = { preJsGz: 11 * KB, totalBytes: 147 * KB, domElements: 800 };

/** Gzipped HTML must fit Lighthouse's first simulated round trip (10 TCP packets). */
export const HTML_GZ_MAX = 14_200;

export const CATEGORIES = ['performance', 'accessibility', 'best-practices', 'seo'];

/** lhci assertions: 100 in every category plus the budgets, judged on the median run. */
export function assertions(budget = BUDGET) {
  const opt = (o) => ['error', { aggregationMethod: 'median-run', ...o }];
  return {
    ...Object.fromEntries(CATEGORIES.map((c) => [`categories:${c}`, opt({ minScore: 1 })])),
    'resource-summary:script:size': opt({ maxNumericValue: budget.preJsGz }),
    'total-byte-weight': opt({ maxNumericValue: budget.totalBytes }),
    'dom-size': opt({ maxNumericValue: budget.domElements }),
  };
}

/** Every built HTML page as a sorted route (`x/index.html` -> `/x/`, `404.html` as itself). */
export function pages(distDir) {
  if (!existsSync(distDir)) throw new Error(`missing dist: ${distDir}`);
  return readdirSync(distDir, { recursive: true, encoding: 'utf8' })
    .map((f) => f.split('\\').join('/'))
    .filter((f) => f.endsWith('.html'))
    .map((f) => (f === 'index.html' ? '/' : `/${f.replace(/index\.html$/, '')}`))
    .sort();
}

/** Route -> served path under BASE. */
export const pathOf = (route) => BASE + route.slice(1);

/** Pages whose gzipped HTML is over HTML_GZ_MAX, as `route: bytes` strings. */
export function oversizedHtml(distDir, routes = pages(distDir)) {
  return routes.flatMap((r) => {
    const file = r.endsWith('/') ? `${r}index.html` : r;
    const n = gzipSync(readFileSync(distDir + file)).length;
    return n > HTML_GZ_MAX ? [`${r}: ${n} B gzip > ${HTML_GZ_MAX}`] : [];
  });
}
