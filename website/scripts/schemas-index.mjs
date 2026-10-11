// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

// Writes `<dir>/index.html`, a plain list linking every `<kind>/<version>.json` under `<dir>`: the
// deploy action refuses a tree with no root index, and `https://ocx.sh/schemas/` would otherwise 404.
// Built from the files on disk, so it cannot drift from what is published.
//
//   node scripts/schemas-index.mjs dist/schemas

import { readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const escape = (s) => s.replace(/[&<>"]/g, (c) => `&#${c.charCodeAt(0)};`);

/** `kind/version.json` paths under `dir`, sorted. */
export function schemaPaths(dir) {
  return readdirSync(dir, { withFileTypes: true })
    .filter((e) => e.isDirectory())
    .flatMap((e) => readdirSync(join(dir, e.name)).filter((f) => f.endsWith('.json')).map((f) => `${e.name}/${f}`))
    .sort();
}

/** The index page for the schemas under `dir`. */
export function schemasIndex(dir) {
  const items = schemaPaths(dir).map((path) => {
    const { title } = JSON.parse(readFileSync(join(dir, path), 'utf8'));
    const note = typeof title === 'string' ? ` &mdash; ${escape(title)}` : '';
    return `    <li><a href="${escape(path)}">${escape(path)}</a>${note}</li>`;
  });
  return `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>OCX JSON Schemas</title>
</head>
<body>
  <h1>OCX JSON Schemas</h1>
  <p>The JSON Schemas OCX publishes, one file per kind and version. See the <a href="/docs/">documentation</a>.</p>
  <ul>
${items.join('\n')}
  </ul>
</body>
</html>
`;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const dir = process.argv[2];
  if (!dir) throw new Error('usage: schemas-index.mjs <schemas dir>');
  writeFileSync(join(dir, 'index.html'), schemasIndex(dir));
}
