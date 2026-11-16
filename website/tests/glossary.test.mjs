// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

// The term data (`src/data/glossary.json`) must be self-consistent, its `learnMore` links must resolve, and the
// generated glossary page must match it. No hand-written page may use an `avoid` form.
// Generated pages are skipped (their text comes from Rust help this lint cannot change); the generator names
// them in its own source (`crates/ocx_schema/src/docs/`): the directory it owns and each page path.

import assert from 'node:assert/strict';
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { dirname, join, relative } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { matcher, validate } from '@ocx-sh/theme/glossary';
import { PAGE, render } from '../scripts/glossary.mjs';
import { maskProse, pagesUnder } from '../scripts/prose.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const DOCS = join(HERE, '..', 'src', 'content', 'docs');
const GENERATOR = join(HERE, '..', '..', 'crates', 'ocx_schema', 'src', 'docs');
const GLOSSARY = JSON.parse(readFileSync(join(HERE, '..', 'src', 'data', 'glossary.json'), 'utf8'));

// Reader floors, just under the real counts: a scan that silently loses pages or forms must fail.
const MIN_PAGES = 97;
const MIN_FORMS = 97;

// The one generated page `website/.gitignore` ignores (the SBOM table, built by `task sbom:generate:page`): it
// exists on a developer machine and not in CI, so the scan skips it to read the same pages in both.
const IGNORED_GENERATED = ['reference/dependencies.md'];

// Explicit exceptions: `path:form` -> reason. None today, the two `<Node name="entrypoints/">` labels in
// `explanation/storage.mdx` are JSX attributes and the prose masking already skips them.
const ALLOWED = new Map([]);

/** The generator-owned directory and page paths (relative to the docs root), read from the generator source. */
function generatedPages() {
  const sources = readdirSync(GENERATOR)
    .filter((n) => n.endsWith('.rs'))
    .map((n) => readFileSync(join(GENERATOR, n), 'utf8'))
    .join('\n');
  const dirs = [...sources.matchAll(/const (?:OWNED_DIR|COMMAND_DIR): &str = "([^"]+)";/g)].map((m) => m[1]);
  const files = [...sources.matchAll(/"(reference\/[a-z0-9\/-]+\.md)"/g)].map((m) => m[1]);
  return { dirs: [...new Set(dirs)], files: [...new Set(files)] };
}

/** The glossary's own avoid-form finder, the one the theme plugin shares with consumer lints. */
const findAvoid = matcher(GLOSSARY, { match: 'avoid' });
const TERM = new Map(GLOSSARY.map((row) => [row.id, row.term]));

/** Banned forms in one page's source: `{ line, form, use }`, line numbers counted over the whole masked text. */
function bannedForms(source, rel = '') {
  const prose = maskProse(source);
  return findAvoid(prose)
    .filter((hit) => !ALLOWED.has(`${rel}:${hit.text}`))
    .map((hit) => ({
      line: prose.slice(0, hit.start).split('\n').length,
      form: hit.text,
      use: TERM.get(hit.id),
    }));
}

/** Scan every hand-written page. Returns the hit list and what was read. */
function scan(docsDir = DOCS, pages = hand_written(docsDir)) {
  const forms = new Set(GLOSSARY.flatMap((row) => row.avoid ?? []));
  const hits = [];
  for (const file of pages) {
    const rel = relative(docsDir, file).split('\\').join('/');
    for (const hit of bannedForms(readFileSync(file, 'utf8'), rel)) hits.push({ rel, ...hit });
  }
  return { hits, pages: pages.length, forms: forms.size };
}

/** The pages a person writes: not generator output, not the changelog, not the git-ignored SBOM page. */
function hand_written(docsDir) {
  const { dirs, files } = generatedPages();
  return pagesUnder(docsDir).filter((file) => {
    const rel = relative(docsDir, file).split('\\').join('/');
    if (rel === 'changelog.md' || IGNORED_GENERATED.includes(rel)) return false;
    if (files.includes(rel)) return false;
    return !dirs.some((d) => rel.startsWith(`${d}/`));
  });
}

test('the generator source names the generated pages', () => {
  const { dirs, files } = generatedPages();
  assert.deepEqual(dirs, ['reference/command-line']);
  assert.equal(files.length, 4, `expected four generated single pages, got ${files}`);
  for (const f of files) assert.ok(existsSync(join(DOCS, f)), `${f} is named by the generator but absent`);
});

test('no hand-written page uses a form the glossary retired', () => {
  const { hits, pages, forms } = scan();
  assert.ok(pages >= MIN_PAGES, `scan read ${pages} pages, floor is ${MIN_PAGES}`);
  assert.ok(forms >= MIN_FORMS, `scan read ${forms} avoid forms, floor is ${MIN_FORMS}`);
  const report = hits.map((h) => `${h.rel}:${h.line}: "${h.form}" is retired, use "${h.use}"`);
  assert.equal(hits.length, 0, `\n${report.join('\n')}`);
});

test('the scan masks code, links, tags and comments but finds prose', () => {
  const masked = maskProse(
    [
      '---',
      'title: blob store',
      '---',
      'import Foo from "blob store";',
      'A `blob store` and [x](blob-store) and <Node name="blob store" /> {/* blob store */}',
      '```',
      'blob store',
      '```',
      '[ref]: blob store',
      '## Heading \\{#blob-store\\}',
      'plain blob store here',
    ].join('\n'),
  ).split('\n');
  assert.equal(masked.length, 11);
  assert.ok(!masked[9].includes('blob-store'), 'a heading anchor is not prose');
  assert.deepEqual(
    masked.map((l) => l.includes('blob store')),
    [false, false, false, false, false, false, false, false, false, false, true],
  );
});

test('a banned form wrapped across a line, or split by markup, is judged as the matcher judges it', () => {
  assert.deepEqual(bannedForms('first operator\ntier wrapped').map((h) => [h.line, h.use]), [[1, 'config tier']]);
  assert.deepEqual(bannedForms('a\n\nfirst\n  operator   tier').map((h) => h.line), [4]);
  assert.deepEqual(bannedForms('the floating `:user` tag'), [], 'a code span between words is not a phrase');
});

test('fences close on the opener character and length only', () => {
  const retired = 'operator tier';
  // A ~~~ fence holding a ``` line: the line neither closes it nor opens another; text after it is prose.
  assert.equal(bannedForms(`~~~\n\`\`\`\n${retired}\n~~~\n${retired}\n`).map((h) => h.line).join(), '5');
  // A four-backtick fence holding a three-backtick line.
  assert.equal(bannedForms(`\`\`\`\`\n\`\`\`\n${retired}\n\`\`\`\`\n${retired}\n`).map((h) => h.line).join(), '5');
  assert.equal(bannedForms(`\`\`\`\n${retired}\n\`\`\`\n${retired}\n`).map((h) => h.line).join(), '4');
  assert.equal(bannedForms(`~~~\n${retired}\n~~~~\n${retired}\n`).length, 1, 'a longer closer closes');
});

test('a lone `<` never masks prose up to a `>` in a later paragraph', () => {
  assert.deepEqual(bannedForms('x <y is small.\n\nThe operator tier says so, and a > b.').map((h) => h.line), [3]);
  assert.deepEqual(bannedForms('<Node\n  name="operator tier"\n/> then operator tier').map((h) => h.line), [3]);
});

/** The first character in a term or definition outside code spans that the generator does not escape, if any. */
const unsafeInProse = (text) => /[{}<|]/.exec(text.replace(/`[^`]*`/g, ''))?.[0];

test('data: terms and definitions hold no character the generated page cannot carry', () => {
  for (const row of GLOSSARY) {
    for (const field of ['term', 'definition']) {
      assert.equal(unsafeInProse(row[field] ?? ''), undefined, `${row.id}.${field} has an unescaped { } < or | outside backticks`);
    }
  }
  assert.equal(unsafeInProse('a `{x}` b'), undefined);
  assert.equal(unsafeInProse('a {x} b'), '{', 'a planted row is caught');
  assert.equal(unsafeInProse('a | b'), '|');
});

test('data: ids are unique and no variant or avoid form is claimed twice', () => {
  const ids = new Set();
  const variants = new Map();
  for (const row of GLOSSARY) {
    assert.ok(!ids.has(row.id), `duplicate id ${row.id}`);
    ids.add(row.id);
    for (const v of row.variants ?? []) {
      assert.ok(!variants.has(v), `variant "${v}" is listed by both ${variants.get(v)} and ${row.id}`);
      variants.set(v, row.id);
    }
  }
  for (const row of GLOSSARY) {
    for (const a of row.avoid ?? []) {
      assert.ok(!variants.has(a), `avoid form "${a}" of ${row.id} is a variant of ${variants.get(a)}`);
    }
  }
});

/** The route of every source page under the docs root (`explanation/storage/`; the landing page is `''`). */
function routes() {
  return pagesUnder(DOCS).map((file) => {
    const route = relative(DOCS, file).split('\\').join('/').replace(/\.mdx?$/, '').replace(/(^|\/)index$/, '');
    return route ? `${route}/` : '';
  });
}

/** The `excludePages` prefixes no page route starts with: each one is a silent no-op. */
function deadPrefixes(rows, pageRoutes) {
  return rows.flatMap((row) =>
    (row.excludePages ?? []).filter((prefix) => !pageRoutes.some((route) => route.startsWith(prefix))).map((p) => `${row.id}: ${p}`),
  );
}

test('data: the theme validator finds no problem', () => {
  assert.deepEqual(validate(GLOSSARY), []);
});

test('data: every excludePages prefix is a route prefix that matches a page', () => {
  const pageRoutes = routes();
  assert.ok(pageRoutes.length >= MIN_PAGES, `route scan read ${pageRoutes.length} pages, floor is ${MIN_PAGES}`);
  const rows = GLOSSARY.filter((row) => row.excludePages);
  assert.ok(rows.length > 0, 'no row carries excludePages: the check below would read nothing');
  for (const row of rows) {
    for (const prefix of row.excludePages) {
      assert.match(prefix, /^[a-z0-9][\w./-]*\/$/, `${row.id}: "${prefix}" must be a route prefix without a leading slash, ending in "/"`);
    }
  }
  assert.deepEqual(deadPrefixes(GLOSSARY, pageRoutes), []);
  const planted = [{ id: 'planted', excludePages: ['no/such/page/', 'explanation/signing/'] }];
  assert.deepEqual(deadPrefixes(planted, pageRoutes), ['planted: no/such/page/'], 'a prefix matching no page is caught');
});

test('data: every internal learnMore reaches an existing page and anchor', () => {
  for (const row of GLOSSARY) {
    const link = row.learnMore;
    if (!link || /^[a-z][a-z0-9+.-]*:/i.test(link)) continue;
    const [path, anchor] = link.split('#');
    const base = join(DOCS, path.replace(/\/+$/, ''));
    const source = [`${base}.md`, `${base}.mdx`, join(base, 'index.md'), join(base, 'index.mdx')].find(existsSync);
    assert.ok(source, `${row.id}: learnMore "${link}" has no source page`);
    if (anchor) {
      const text = readFileSync(source, 'utf8');
      assert.ok(
        text.includes(`{#${anchor}}`) || text.includes(`\\{#${anchor}\\}`),
        `${row.id}: ${relative(DOCS, source)} has no {#${anchor}}`,
      );
    }
  }
});

test('the glossary page is what the data generates', () => {
  assert.ok(existsSync(PAGE), 'reference/glossary.md is missing; run `task website:glossary:generate`');
  assert.equal(
    readFileSync(PAGE, 'utf8'),
    render(),
    'reference/glossary.md is stale; run `task website:glossary:generate`',
  );
});
