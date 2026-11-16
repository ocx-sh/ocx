// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

// A command a reader types sits in a `<ShellSession>` frame: a shell-language fence or a doc-script include
// outside one fails. File content stays a plain fence and names its file with `title="…"`.
// Rule text: `.claude/rules/docs-style.md` § Terminal Commands.
// Skipped: generated reference pages and the changelog (their text is not written here), and the git-ignored
// `reference/dependencies.md` (absent in CI).

import assert from 'node:assert/strict';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { dirname, join, relative } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const DOCS = join(HERE, '..', 'src', 'content', 'docs');
const GENERATOR = join(HERE, '..', '..', 'crates', 'ocx_schema', 'src', 'docs');

// Reader floors, just under the real counts: a scan that silently loses pages or sessions must fail.
const MIN_PAGES = 97;
const MIN_SESSIONS = 230;

const SHELLS = new Set(['sh', 'shell', 'bash', 'zsh', 'console', 'powershell', 'pwsh', 'fish', 'nu', 'nushell', 'elvish', 'bat', 'cmd']);
const IGNORED_GENERATED = ['changelog.md', 'reference/dependencies.md'];

/** Every `.md`/`.mdx` file under `dir`, as a sorted list of absolute paths. */
function pagesUnder(dir) {
  const out = [];
  for (const name of readdirSync(dir).sort()) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) out.push(...pagesUnder(path));
    else if (/\.mdx?$/.test(name)) out.push(path);
  }
  return out;
}

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

/** The pages a person writes. */
function handWritten(docsDir) {
  const { dirs, files } = generatedPages();
  return pagesUnder(docsDir).filter((file) => {
    const rel = relative(docsDir, file).split('\\').join('/');
    if (IGNORED_GENERATED.includes(rel) || files.includes(rel)) return false;
    return !dirs.some((d) => rel.startsWith(`${d}/`));
  });
}

/**
 * Command blocks outside a session in one page source: `{ line, what }`, plus the number of sessions opened.
 * Fences close on their own marker and length only (CommonMark), so a fence inside a fence is not counted.
 */
function findLoose(source) {
  const loose = [];
  let sessions = 0;
  let depth = 0;
  let fence = null;
  source.split('\n').forEach((text, i) => {
    const m = /^\s*(`{3,}|~{3,})(.*)$/.exec(text);
    if (fence) {
      if (m && m[1][0] === fence.ch && m[1].length >= fence.len && m[2].trim() === '') fence = null;
      return;
    }
    if (m) {
      fence = { ch: m[1][0], len: m[1].length };
      const info = m[2].trim();
      const lang = info.split(/[\s{]/)[0].toLowerCase();
      if (SHELLS.has(lang) && !/\btitle=/.test(info) && depth === 0) loose.push({ line: i + 1, what: `${lang} fence` });
      return;
    }
    if (/^\s*<ShellSession(\s|>|$)/.test(text)) {
      sessions += 1;
      depth += 1;
    } else if (/^\s*<\/ShellSession>/.test(text)) depth = Math.max(0, depth - 1);
    else if (/^\s*\\?<\\?<\\?<\s*@\/_scripts\//.test(text) && depth === 0) loose.push({ line: i + 1, what: 'doc-script include' });
  });
  return { loose, sessions };
}

function scan(docsDir = DOCS) {
  const pages = handWritten(docsDir);
  const hits = [];
  let sessions = 0;
  for (const file of pages) {
    const rel = relative(docsDir, file).split('\\').join('/');
    const found = findLoose(readFileSync(file, 'utf8'));
    sessions += found.sessions;
    for (const h of found.loose) hits.push({ rel, ...h });
  }
  return { hits, pages: pages.length, sessions };
}

test('every command block of a hand-written page sits in a ShellSession', () => {
  const { hits, pages, sessions } = scan();
  assert.ok(pages >= MIN_PAGES, `scan read ${pages} pages, floor is ${MIN_PAGES}`);
  assert.ok(sessions >= MIN_SESSIONS, `scan saw ${sessions} sessions, floor is ${MIN_SESSIONS}`);
  const report = hits.map((h) => `${h.rel}:${h.line}: ${h.what} outside a <ShellSession>; wrap it, or add title="<file name>" if it is file content`);
  assert.equal(hits.length, 0, `\n${report.join('\n')}`);
});

test('a shell fence outside a session fails, one inside, titled or in another language does not', () => {
  const fenced = (info, body = 'ocx pull') => `\`\`\`${info}\n${body}\n\`\`\`\n`;
  assert.deepEqual(findLoose(fenced('sh')).loose, [{ line: 1, what: 'sh fence' }]);
  assert.equal(findLoose(fenced('powershell')).loose.length, 1);
  assert.equal(findLoose(`<ShellSession>\n\n${fenced('sh')}\n</ShellSession>\n`).loose.length, 0);
  assert.equal(findLoose(`<ShellSession>\n\n${fenced('sh')}\n</ShellSession>\n${fenced('bash')}`).loose.length, 1, 'closed session');
  assert.equal(findLoose(fenced('sh title="build.sh"')).loose.length, 0, 'file content');
  assert.equal(findLoose(fenced('text')).loose.length, 0);
  assert.equal(findLoose(fenced('sh {2}')).loose.length, 1, 'meta is not a title');
});

test('a doc-script include outside a session fails', () => {
  const include = '\\<\\<\\< @/_scripts/first-package/exec.sh\\{sh\\}\n';
  assert.deepEqual(findLoose(include).loose, [{ line: 1, what: 'doc-script include' }]);
  assert.equal(findLoose(`<ShellSession>\n\n${include}\n</ShellSession>\n`).loose.length, 0);
});

test('fences close on their own marker and length only', () => {
  // A ~~~ fence holding a ``` line is one fence; the sh fence after it is judged on its own.
  assert.equal(findLoose('~~~md\n```sh\nx\n```\n~~~\n').loose.length, 0);
  assert.equal(findLoose('````md\n```sh\nx\n```\n````\n```sh\ny\n```\n').loose.map((h) => h.line).join(), '6');
});

test('sessions are counted, indented or not', () => {
  assert.equal(findLoose('<ShellSession title="a">\n\n</ShellSession>\n   <ShellSession>\n\n   </ShellSession>\n').sessions, 2);
});
