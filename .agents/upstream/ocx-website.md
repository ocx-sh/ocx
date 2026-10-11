# Upstream patch for ocx-sh/website

Theme gaps found while porting the ocx docs to Astro, plus two root-site changes. ocx ships local copies
(`website/plugins/*.mjs`, `website/src/components/Terminal.astro`, `terminal.mjs`, `terminal-player.css`);
this patch moves them into `@ocx-sh/theme`. The patch is generated against ocx-sh/website
`5afea49825806eb941adf7b70e94ae7dcff7b11b` (`main` at the time of writing) and passes `git apply --check`
on a clean checkout of that commit. Nothing in it has been built or tested: run `pnpm install` (the new
dependencies change `pnpm-lock.yaml`) and the full suite before opening the PR.

## What it contains

| Area | Change |
|---|---|
| `packages/theme/src/remark/` | `remarkHeadingIds` (trailing `{#id}` or MDX `\{#id\}` becomes the heading id), `remarkMdLinks` (`./x.md#a` becomes `<base>x/#a`, fence-aware), `remarkIncludes` (`<<< @/path{lang}` and `<!--@include: path{N,}-->`) |
| `packages/theme/src/starlight/index.mjs` | `remarkHeadingIds` always on; new option `ocxTheme({ docs: { root, srcDir? } })` adds the other two. `package.json` gains `unist-util-visit` and test-only `unified`, `remark-parse`, `remark-mdx`, `vfile` |
| `packages/theme/test/remark-*.test.mjs` | The same cases ocx runs, under vitest |
| `packages/theme/src/components/terminal.mjs` | The cast is fetched when an open terminal nears the viewport or a collapsed one first expands, not on mount. Unit and e2e tests updated to match |
| `site/` | `/team/` page (Kate's photo in `site/public/team/kate.jpg`, a binary hunk), the "Content-Addressed and Deduplicated" landing section, a `/team/` root claim in `nav.json` with the tests and the budget class that list root pages |

## Open points before it merges

- The Terminal change reverses `2210983da` ("pre-fetch terminal recordings", recorded in
  `.agents/research/ocx-components-port.md` as "do not regress"). It is a deliberate decision of the docs-next-level
  loop (decision D8): a docs page with dozens of casts must not fetch them all on load. The cost is click latency on
  the first expand.
- `remark-*.mjs` have JSDoc without parameter types. If `tsc --checkJs` (`prepack`) rejects them, add the `mdast`
  types; the behavior is covered by the tests.
- A `/team/` claim touches the Bunny match golden only if a golden row probes it. Run `pnpm test` and refresh
  `infra/bunny/__snapshots__/match-golden.txt` if a row changes.
- The team page shows Michael's initials, not a photo: the old page loaded a GitHub avatar, which a Lighthouse-100
  page should not fetch from a third party. Drop a local `site/public/team/michael.jpg` and add `src` to use one.

## Apply

Extract the patch from this file and apply it in a clean checkout of ocx-sh/website:

```sh
awk '/^~~~~~~~~ diff$/{on=1;next} /^~~~~~~~~$/{on=0} on' ocx-website.md > theme-gaps.patch
git apply --check theme-gaps.patch && git apply theme-gaps.patch
```

Then `pnpm install`, `pnpm test`, and a theme release that bumps `packages/theme/package.json`.

## Repin ocx after the theme release

ocx pins `@ocx-sh/theme` exactly (decision D1). After the release, in `website/package.json` of ocx change the one line

```diff
-    "@ocx-sh/theme": "0.1.1",
+    "@ocx-sh/theme": "<released version>",
```

and run the lockfile update. Then delete the local copies and let the theme provide them: remove
`website/plugins/*.mjs` and their tests, `website/src/components/Terminal.astro`, `terminal.mjs` and
`terminal-player.css`; in `astro.config.mjs` drop the three `remarkPlugins` entries and the `components.Terminal`
mapping, and pass `ocxTheme({ docs: { root: <content dir>, srcDir: <content dir> } })`; point MDX imports of
`Terminal` at `@ocx-sh/theme/components/Terminal.astro`.

## Patch

~~~~~~~~ diff
packages/theme/package.json                     |   9 +++-
 packages/theme/src/components/Terminal.astro    |   6 +--
 packages/theme/src/components/terminal.mjs      |  43 +++++++++++-----
 packages/theme/src/nav.json                     |   1 +
 packages/theme/src/remark/heading-ids.mjs       |  25 ++++++++++
 packages/theme/src/remark/includes.mjs          |  62 ++++++++++++++++++++++++
 packages/theme/src/remark/md-links.mjs          |  42 ++++++++++++++++
 packages/theme/src/starlight/index.mjs          |  20 +++++++-
 packages/theme/test/components-terminal.test.ts |  22 +++++++--
 packages/theme/test/registry.test.ts            |   2 +-
 packages/theme/test/remark-heading-ids.test.mjs |  55 +++++++++++++++++++++
 packages/theme/test/remark-includes.test.mjs    |  57 ++++++++++++++++++++++
 packages/theme/test/remark-md-links.test.mjs    |  54 +++++++++++++++++++++
 site/public/team/kate.jpg                       | Bin 0 -> 20806 bytes
 site/src/content/docs/index.mdx                 |  16 +++++-
 site/src/content/docs/team/index.mdx            |  23 +++++++++
 site/src/dist.test.ts                           |   7 +--
 site/src/landing.test.ts                        |   4 +-
 tests/budgets.mjs                               |   4 +-
 tests/e2e/components-terminal.spec.ts           |  41 +++++++++-------
 20 files changed, 445 insertions(+), 48 deletions(-)

diff --git a/packages/theme/package.json b/packages/theme/package.json
index 72f80f8..9994608 100644
--- a/packages/theme/package.json
+++ b/packages/theme/package.json
@@ -119,7 +119,8 @@
     "@zag-js/tooltip": "1.44.0",
     "@zag-js/tree-view": "1.44.0",
     "@zag-js/vanilla": "1.44.0",
-    "asciinema-player": "3.15.1"
+    "asciinema-player": "3.15.1",
+    "unist-util-visit": "^5.0.0"
   },
   "peerDependencies": {
     "@astrojs/starlight": ">=0.42.0 <0.43",
@@ -136,7 +137,11 @@
     "@iconify-json/simple-icons": "1.2.97",
     "@types/node": "^24.10.0",
     "ajv": "8.20.0",
+    "remark-mdx": "^3.1.0",
+    "remark-parse": "^11.0.0",
     "astro": "7.3.5",
-    "typescript": "^6.0.3"
+    "typescript": "^6.0.3",
+    "unified": "^11.0.5",
+    "vfile": "^6.0.3"
   }
 }
diff --git a/packages/theme/src/components/Terminal.astro b/packages/theme/src/components/Terminal.astro
index 9b42e40..3345510 100644
--- a/packages/theme/src/components/Terminal.astro
+++ b/packages/theme/src/components/Terminal.astro
@@ -1,8 +1,8 @@
 ---
 // Terminal (WP13.1): a recorded cast played with asciinema-player in terminal-window chrome.
-// The cast is fetched on page load; the player JS + CSS only when a player is created: an open
-// terminal on first hover/focus/touch/click (near the viewport when autoPlay), a collapsed one on
-// first expand.
+// The cast is fetched when the terminal nears the viewport (open) or first expands (collapsed); the
+// player JS + CSS only when a player is created: an open terminal on first hover/focus/touch/click
+// (near the viewport when autoPlay), a collapsed one on first expand.
 // A collapsed terminal is a Zag collapsible (C-142): chrome = trigger, player area = content.
 import { base } from 'astro:config/client';
 import { withBase } from './base-url.mjs';
diff --git a/packages/theme/src/components/terminal.mjs b/packages/theme/src/components/terminal.mjs
index 4992767..63d0635 100644
--- a/packages/theme/src/components/terminal.mjs
+++ b/packages/theme/src/components/terminal.mjs
@@ -237,8 +237,8 @@ function wireControls(root, player) {
 }
 
 /**
- * Wire one rendered `.ocx-terminal` root: prefetch its cast immediately, and create the player
- * lazily: an open terminal on its first hover, focus, touch or start click (as it nears the
+ * Wire one rendered `.ocx-terminal` root: fetch its cast when it is seen or opened, and create the
+ * player lazily: an open terminal on its first hover, focus, touch or start click (as it nears the
  * viewport only with `autoPlay`); a collapsed one (a Zag collapsible, C-142) on its first expand,
  * reported by `ocx:collapsible:change`.
  * @param {HTMLElement} root
@@ -250,13 +250,21 @@ export function mountTerminal(root, loadPlayer = loadDefault) {
   /** @type {unknown} */
   const parsed = JSON.parse(root.dataset['options'] ?? '{}');
   const opts = /** @type {PlayerOptions} */ (parsed);
-  // Prefetch on mount, collapsed or not, so expanding is instant (ocx 2210983da).
-  // Low priority: many casts on one page must not compete with the main content.
-  const data = fetch(root.dataset['src'] ?? '', { priority: 'low' }).then((r) => {
-    if (!r.ok) throw new Error(`HTTP ${r.status}`);
-    return r.text();
-  });
-  data.catch(() => {}); // surfaced in the player area once a player is wanted
+  // The cast is fetched when the terminal is seen (an open one near the viewport) or opened (a collapsed
+  // one), never on page load: a docs page with many casts must not pay for the ones nobody scrolls to.
+  // Low priority: casts must not compete with the main content.
+  /** @type {Promise<string> | undefined} */
+  let cast;
+  const fetchCast = () => {
+    if (!cast) {
+      cast = fetch(root.dataset['src'] ?? '', { priority: 'low' }).then((r) => {
+        if (!r.ok) throw new Error(`HTTP ${r.status}`);
+        return r.text();
+      });
+      cast.catch(() => {}); // surfaced in the player area once a player is wanted
+    }
+    return cast;
+  };
 
   // Our start overlay (SSR'd; asciinema's own is hidden in CSS): hidden once playback starts, and
   // a click starts it through the same create path as everything else.
@@ -266,12 +274,12 @@ export function mountTerminal(root, loadPlayer = loadDefault) {
   /** @param {boolean} autoPlay */
   const create = (autoPlay) =>
     // Wait for the web fonts too: the player measures its cell size from the mono font once, at create.
-    (player ??= Promise.all([loadPlayer(), data, document.fonts?.ready])
+    (player ??= Promise.all([loadPlayer(), fetchCast(), document.fonts?.ready])
       .then(([m, d]) => {
         const p = m.create({ data: d }, area, {
           ...opts,
           autoPlay,
-          preload: true, // the data is already here: show 00:00, not --:--, before play
+          preload: true, // the data is already here (create waits for it): show 00:00, not --:--, before play
           theme: 'ocx',
           terminalFontFamily: 'var(--ocx-font-mono)',
           controls: false, // our own bar (below) drives play/pause/seek/fullscreen through the player API
@@ -296,9 +304,20 @@ export function mountTerminal(root, loadPlayer = loadDefault) {
   });
 
   if (root.dataset['zagRoot'] !== 'collapsible' && !opts.autoPlay) {
-    // Nothing loads before interaction: the SSR box and start overlay already hold the layout.
+    // The player loads on interaction (the SSR box and start overlay already hold the layout); only the
+    // cast text is fetched ahead, once the terminal nears the viewport.
+    const seen = new IntersectionObserver(
+      (entries) => {
+        if (!entries.some((e) => e.isIntersecting)) return;
+        seen.disconnect();
+        void fetchCast();
+      },
+      { rootMargin: '200px' },
+    );
+    seen.observe(root);
     const arm = () => {
       for (const type of INTERACTION) root.removeEventListener(type, arm);
+      seen.disconnect();
       void create(false);
     };
     for (const type of INTERACTION) root.addEventListener(type, arm, { passive: true });
diff --git a/packages/theme/src/nav.json b/packages/theme/src/nav.json
index 432949d..2bc68c8 100644
--- a/packages/theme/src/nav.json
+++ b/packages/theme/src/nav.json
@@ -42,6 +42,7 @@
     { "path": "/integrations/", "repo": "ocx-sh/website", "search": false },
     { "path": "/apps/", "repo": "ocx-sh/website", "search": false },
     { "path": "/install/", "repo": "ocx-sh/website", "search": false },
+    { "path": "/team/", "repo": "ocx-sh/website", "search": false },
     { "path": "/docs/", "repo": "ocx-sh/ocx", "search": true },
     { "path": "/schemas/", "repo": "ocx-sh/ocx", "search": false },
     { "path": "/catalog/", "repo": "ocx-sh/index", "search": false },
diff --git a/packages/theme/src/remark/heading-ids.mjs b/packages/theme/src/remark/heading-ids.mjs
new file mode 100644
index 0000000..5ac5281
--- /dev/null
+++ b/packages/theme/src/remark/heading-ids.mjs
@@ -0,0 +1,25 @@
+// SPDX-License-Identifier: Apache-2.0
+// Copyright 2026 The OCX Authors
+
+import { visit } from 'unist-util-visit';
+
+/**
+ * Remark plugin: a heading whose last text ends in ` {#id}` (markdown) or the escaped ` \{#id\}` (MDX, where the
+ * parser already decoded the escapes) gets `data.hProperties.id = id`, and the suffix leaves the heading text.
+ * A last text node that is only the suffix is dropped. Headings without a suffix are left untouched.
+ *
+ * @returns {import('unified').Plugin<[], import('mdast').Root>}
+ */
+export function remarkHeadingIds() {
+  return (tree) => {
+    visit(tree, 'heading', (heading) => {
+      const last = heading.children.at(-1);
+      const match = last?.type === 'text' ? /\s*\{#([^\s{}]+)\}\s*$/.exec(last.value) : null;
+      if (!match) return;
+      const id = match[1];
+      last.value = last.value.slice(0, match.index);
+      if (last.value === '') heading.children.pop();
+      heading.data = { ...heading.data, id, hProperties: { ...heading.data?.hProperties, id } };
+    });
+  };
+}
diff --git a/packages/theme/src/remark/includes.mjs b/packages/theme/src/remark/includes.mjs
new file mode 100644
index 0000000..026191e
--- /dev/null
+++ b/packages/theme/src/remark/includes.mjs
@@ -0,0 +1,62 @@
+// SPDX-License-Identifier: Apache-2.0
+// Copyright 2026 The OCX Authors
+
+import { readFileSync } from 'node:fs';
+import { dirname, join, resolve } from 'node:path';
+
+const CODE_INCLUDE = /^<<<\s+@\/(\S+?)(?:\{(\S+)\})?$/;
+const MD_INCLUDE = /^<!--@include:\s*(\S+?)(?:\{(\d+),\})?\s*-->$/;
+
+function read(path) {
+  try {
+    return readFileSync(path, 'utf8');
+  } catch (cause) {
+    throw new Error(`include: cannot read ${path}`, { cause });
+  }
+}
+
+/**
+ * Remark plugin for the two include forms the pages use.
+ *
+ * - A paragraph that is only `<<< @/<path>{lang}` becomes a `code` node with `lang` and the file's text, one
+ *   trailing newline removed. `@` is `srcDir`.
+ * - An `html` node that is only `<!--@include: <path>{N,}-->` is replaced by the file's lines N to the end
+ *   (1-based), parsed as markdown. `<path>` is relative to the including file; `{N,}` is optional.
+ *
+ * @param {{ srcDir: string }} options `srcDir` is the absolute directory `@` names.
+ * @returns {import('unified').Plugin<[], import('mdast').Root>}
+ */
+export function remarkIncludes({ srcDir }) {
+  const processor = this;
+  // A replacement list for the node, or null to keep it and descend.
+  const expand = (node, dir) => {
+    if (node.type === 'paragraph' && node.children.length === 1 && node.children[0].type === 'text') {
+      const code = CODE_INCLUDE.exec(node.children[0].value.trim());
+      if (code) {
+        const value = read(resolve(srcDir, code[1])).replace(/\n$/, '');
+        return [{ type: 'code', lang: code[2] ?? null, meta: null, value }];
+      }
+    }
+    if (node.type === 'html') {
+      const md = MD_INCLUDE.exec(node.value.trim());
+      if (md) {
+        const path = join(dir, md[1]);
+        const text = read(path).split('\n').slice(md[2] ? Number(md[2]) - 1 : 0).join('\n');
+        const included = processor.parse(text);
+        walk(included, dirname(path));
+        return included.children;
+      }
+    }
+    return null;
+  };
+  const walk = (parent, dir) => {
+    if (!parent.children) return;
+    parent.children = parent.children.flatMap((child) => {
+      const replacement = expand(child, dir);
+      if (replacement) return replacement;
+      walk(child, dir);
+      return [child];
+    });
+  };
+  return (tree, file) => walk(tree, dirname(file.path));
+}
diff --git a/packages/theme/src/remark/md-links.mjs b/packages/theme/src/remark/md-links.mjs
new file mode 100644
index 0000000..8df0ef1
--- /dev/null
+++ b/packages/theme/src/remark/md-links.mjs
@@ -0,0 +1,42 @@
+// SPDX-License-Identifier: Apache-2.0
+// Copyright 2026 The OCX Authors
+
+import { posix, relative, sep } from 'node:path';
+
+import { visit } from 'unist-util-visit';
+
+const PAGE_LINK = /^([^#?]+)\.mdx?([#?].*)?$/;
+const EXTERNAL = /^[a-z][a-z0-9+.-]*:|^\/\//i;
+
+/** The route a `.md` link target names, keeping its `#fragment`/`?query`; the target itself when it is not one. */
+function route(target, page, base) {
+  const match = PAGE_LINK.exec(target);
+  if (!match || EXTERNAL.test(target)) return target;
+  const path = match[1].startsWith('/')
+    ? posix.normalize(match[1].slice(1))
+    : posix.join(posix.dirname(page), match[1]);
+  if (path.startsWith('..')) return target;
+  const dir = path.replace(/(^|\/)index$/, '');
+  return `${base}${dir ? `${dir}/` : ''}${match[2] ?? ''}`;
+}
+
+/**
+ * Remark plugin: rewrites `link` and `definition` URLs that name a `.md`/`.mdx` page inside the docs tree to the
+ * page's route, `base` + directory + trailing slash, keeping the `#fragment`. `index` pages route to their
+ * directory. External URLs, bare `#fragment` links and targets leaving the tree are left alone. Code and inline
+ * code are never touched.
+ *
+ * @param {{ root: string, base: string }} options `root` is the absolute docs content root; `base` the site path
+ *   of that root, for example `/docs/`. The page's own path is `file.path` relative to `root`.
+ * @returns {import('unified').Plugin<[], import('mdast').Root>}
+ */
+export function remarkMdLinks({ root, base }) {
+  const prefix = base.endsWith('/') ? base : `${base}/`;
+  return (tree, file) => {
+    const page = relative(root, file.path).split(sep).join('/');
+    if (page.startsWith('..')) return;
+    visit(tree, ['link', 'definition'], (node) => {
+      node.url = route(node.url, page, prefix);
+    });
+  };
+}
diff --git a/packages/theme/src/starlight/index.mjs b/packages/theme/src/starlight/index.mjs
index 1eb4b32..d36c472 100644
--- a/packages/theme/src/starlight/index.mjs
+++ b/packages/theme/src/starlight/index.mjs
@@ -6,6 +6,9 @@ import { fontProviders } from 'astro/config';
 import pkg from '../../package.json' with { type: 'json' };
 import nav from '../nav.json' with { type: 'json' };
 import { mergeTargets } from '../registry.mjs';
+import { remarkHeadingIds } from '../remark/heading-ids.mjs';
+import { remarkIncludes } from '../remark/includes.mjs';
+import { remarkMdLinks } from '../remark/md-links.mjs';
 import { EXPRESSIVE_CODE } from './ec.mjs';
 import { italicOptional } from './fonts.mjs';
 
@@ -89,6 +92,7 @@ const OPTIONS_ID = 'virtual:ocx-theme/options';
  * @typedef {{ label: string, href: string }} Crumb
  * @typedef {{
  *   breadcrumbs?: false | { root?: Crumb | false },
+ *   docs?: { root: string, srcDir?: string },
  * }} OcxThemeOptions
  */
 
@@ -96,10 +100,13 @@ const OPTIONS_ID = 'virtual:ocx-theme/options';
  * Starlight plugin: OCX tokens, base styles, overrides, code theme, search merge.
  * `breadcrumbs`: the trail above every page title (default on, no root crumb: section first);
  * `false` drops it, `{ root: { label, href } }` adds a root crumb.
+ * Trailing `{#id}` on a heading is always its anchor. `docs` adds the migrated-docs remark plugins:
+ * `root` is the absolute content directory whose `.md` links become routes under the site base, `srcDir`
+ * (default `root`) the directory `<<< @/` includes resolve against.
  * @param {OcxThemeOptions} [options]
  * @returns {import('@astrojs/starlight/types').StarlightPlugin}
  */
-export default function ocxTheme({ breadcrumbs = {} } = {}) {
+export default function ocxTheme({ breadcrumbs = {}, docs } = {}) {
   // Defaults live in the route middleware, which reads this.
   const runtime = { breadcrumbs };
   return {
@@ -125,6 +132,17 @@ export default function ocxTheme({ breadcrumbs = {} } = {}) {
                 injectRoute({ pattern: '/favicon.svg', entrypoint: FAVICON, prerender: true });
               updateConfig({
                 site: SITE,
+                markdown: {
+                  remarkPlugins: [
+                    remarkHeadingIds,
+                    ...(docs
+                      ? [
+                          [remarkMdLinks, { root: docs.root, base: own }],
+                          [remarkIncludes, { srcDir: docs.srcDir ?? docs.root }],
+                        ]
+                      : []),
+                  ],
+                },
                 // Per-component stylesheets (ui primitives, explorer) inline instead of each
                 // costing a render-blocking request, which Lighthouse charges as an extra round trip
                 // before LCP. The shared Starlight bundle stays external and cached. Other assets keep
diff --git a/packages/theme/test/components-terminal.test.ts b/packages/theme/test/components-terminal.test.ts
index d28a48c..c2e2d5c 100644
--- a/packages/theme/test/components-terminal.test.ts
+++ b/packages/theme/test/components-terminal.test.ts
@@ -362,12 +362,26 @@ describe('WP13.1 Terminal client', () => {
     expect(r.root.querySelector<HTMLElement>('.ocx-terminal__start')?.hidden).toBe(false);
   });
 
-  it('WP13.1 Terminal: prefetches the cast on mount, not on first click, even when collapsed (2210983da)', async () => {
+  it('a collapsed terminal fetches its cast on the first expand, not on mount or in view', async () => {
     const h = await mount({ src: '/casts/x.cast', collapsed: true });
-    expect(h.fetchMock).toHaveBeenCalledTimes(1);
+    h.intersect(true);
+    await flush();
+    expect(h.fetchMock).not.toHaveBeenCalled();
+    await h.toggle();
+    await vi.waitFor(() => expect(h.fetchMock).toHaveBeenCalledTimes(1));
     expect(String(h.fetchMock.mock.calls[0]?.[0])).toBe(h.root.dataset['src']);
   });
 
+  it('an open terminal fetches its cast once it nears the viewport, not on mount', async () => {
+    const h = await mount({ src: '/casts/x.cast', rows: 8 });
+    await flush();
+    expect(h.fetchMock).not.toHaveBeenCalled();
+    h.intersect(true);
+    h.intersect(true);
+    await vi.waitFor(() => expect(h.fetchMock).toHaveBeenCalledTimes(1));
+    expect(h.load).not.toHaveBeenCalled();
+  });
+
   it('WP13.1 Terminal: collapsed terminal does not import the player before the first expand (Lighthouse)', async () => {
     const h = await mount({ src: '/casts/x.cast', collapsed: true });
     h.intersect(true);
@@ -401,7 +415,7 @@ describe('WP13.1 Terminal client', () => {
     expect(createOpts(h)['autoPlay']).toBe(true);
   });
 
-  it('WP13.1 Terminal: the player is created from the prefetched data, with no second fetch (2210983da)', async () => {
+  it('WP13.1 Terminal: the player is created from the fetched data, with no second fetch (2210983da)', async () => {
     const h = await mount({ src: '/casts/x.cast', collapsed: true });
     await h.toggle();
     await vi.waitFor(() => expect(h.create).toHaveBeenCalledTimes(1));
@@ -519,6 +533,8 @@ describe('WP13.1 Terminal client', () => {
 
   it('WP13.1 Terminal: the cast prefetch is low priority so it does not compete with main content (WP13.1)', async () => {
     const h = await mount({ src: '/casts/x.cast' });
+    h.intersect(true);
+    await vi.waitFor(() => expect(h.fetchMock).toHaveBeenCalledTimes(1));
     expect(h.fetchMock.mock.calls[0]?.[1]).toMatchObject({ priority: 'low' });
   });
 
diff --git a/packages/theme/test/registry.test.ts b/packages/theme/test/registry.test.ts
index ca5e51e..e8ff253 100644
--- a/packages/theme/test/registry.test.ts
+++ b/packages/theme/test/registry.test.ts
@@ -220,7 +220,7 @@ describe('C-008 pure registry functions', () => {
   });
 
   it.each([
-    ['ocx-sh/website', ['/', '/integrations/', '/apps/', '/install/']],
+    ['ocx-sh/website', ['/', '/integrations/', '/apps/', '/install/', '/team/']],
     ['ocx-sh/ocx', ['/docs/', '/schemas/']],
     ['ocx-sh/none', []],
   ])('C-008: claimsOf(%s)', (repo, paths) => {
diff --git a/packages/theme/test/remark-heading-ids.test.mjs b/packages/theme/test/remark-heading-ids.test.mjs
new file mode 100644
index 0000000..893b258
--- /dev/null
+++ b/packages/theme/test/remark-heading-ids.test.mjs
@@ -0,0 +1,55 @@
+// SPDX-License-Identifier: Apache-2.0
+// Copyright 2026 The OCX Authors
+
+import assert from 'node:assert/strict';
+import { test } from 'vitest';
+
+import { remarkHeadingIds } from '../src/remark/heading-ids.mjs';
+
+// The parser packages arrive with the site's package.json; import them when a test runs.
+async function headings(source, { mdx = false } = {}) {
+  const { unified } = await import('unified');
+  const { default: remarkParse } = await import('remark-parse');
+  const { VFile } = await import('vfile');
+  const processor = unified().use(remarkParse);
+  if (mdx) processor.use((await import('remark-mdx')).default);
+  processor.use(remarkHeadingIds);
+  const file = new VFile({ value: source, path: '/docs/page.md' });
+  const tree = await processor.run(processor.parse(file), file);
+  return tree.children.filter((node) => node.type === 'heading');
+}
+
+const text = (node) => node.children.map((child) => child.value ?? '').join('');
+
+test('markdown {#id} becomes the heading id and leaves the text', async () => {
+  const [heading] = await headings('## Install the CLI {#install-cli}\n');
+  assert.equal(heading.data?.hProperties?.id, 'install-cli');
+  assert.equal(text(heading), 'Install the CLI');
+});
+
+test('MDX \\{#id\\} becomes the heading id and leaves the text', async () => {
+  const [heading] = await headings('## Install the CLI \\{#install-cli\\}\n', { mdx: true });
+  assert.equal(heading.data?.hProperties?.id, 'install-cli');
+  assert.equal(text(heading), 'Install the CLI');
+});
+
+test('a suffix after inline code drops the whole trailing text node', async () => {
+  const [heading] = await headings('### `OCX_HOME` {#ocx-home}\n');
+  assert.equal(heading.data?.hProperties?.id, 'ocx-home');
+  assert.deepEqual(
+    heading.children.map((child) => child.type),
+    ['inlineCode'],
+  );
+});
+
+test('a heading without a suffix is untouched', async () => {
+  const [heading] = await headings('## Plain heading\n');
+  assert.equal(heading.data, undefined);
+  assert.equal(text(heading), 'Plain heading');
+});
+
+test('a brace group that is not a trailing id is untouched', async () => {
+  const [heading] = await headings('## Set {#not-last} twice\n');
+  assert.equal(heading.data, undefined);
+  assert.equal(text(heading), 'Set {#not-last} twice');
+});
diff --git a/packages/theme/test/remark-includes.test.mjs b/packages/theme/test/remark-includes.test.mjs
new file mode 100644
index 0000000..def4cb8
--- /dev/null
+++ b/packages/theme/test/remark-includes.test.mjs
@@ -0,0 +1,57 @@
+// SPDX-License-Identifier: Apache-2.0
+// Copyright 2026 The OCX Authors
+
+import assert from 'node:assert/strict';
+import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
+import { tmpdir } from 'node:os';
+import { join } from 'node:path';
+import { test } from 'vitest';
+
+import { remarkIncludes } from '../src/remark/includes.mjs';
+
+function site() {
+  const srcDir = mkdtempSync(join(tmpdir(), 'includes-'));
+  mkdirSync(join(srcDir, '_scripts'));
+  mkdirSync(join(srcDir, 'docs'));
+  writeFileSync(join(srcDir, '_scripts', 'demo.sh'), '#!/bin/sh\necho demo\n');
+  writeFileSync(join(srcDir, 'snippet.md'), 'dropped one\ndropped two\n# Kept heading\n\nkept body\n');
+  return srcDir;
+}
+
+async function run(srcDir, source) {
+  const { unified } = await import('unified');
+  const { default: remarkParse } = await import('remark-parse');
+  const { VFile } = await import('vfile');
+  const processor = unified().use(remarkParse).use(remarkIncludes, { srcDir });
+  const file = new VFile({ value: source, path: join(srcDir, 'docs', 'page.md') });
+  return processor.run(processor.parse(file), file);
+}
+
+test('<<< @/path{lang} becomes a fenced block of the file', async () => {
+  const tree = await run(site(), '<<< @/_scripts/demo.sh{sh}\n');
+  assert.equal(tree.children.length, 1);
+  assert.deepEqual(
+    { type: tree.children[0].type, lang: tree.children[0].lang, value: tree.children[0].value },
+    { type: 'code', lang: 'sh', value: '#!/bin/sh\necho demo' },
+  );
+});
+
+test('<!--@include: path{3,}--> splices lines 3 to the end as markdown', async () => {
+  const tree = await run(site(), '<!--@include: ../snippet.md{3,}-->\n');
+  assert.deepEqual(
+    tree.children.map((node) => node.type),
+    ['heading', 'paragraph'],
+  );
+  assert.equal(tree.children[0].children[0].value, 'Kept heading');
+  assert.equal(tree.children[1].children[0].value, 'kept body');
+});
+
+test('an include without a range takes the whole file', async () => {
+  const tree = await run(site(), '<!--@include: ../snippet.md-->\n');
+  assert.equal(tree.children[0].type, 'paragraph');
+  assert.match(tree.children[0].children[0].value, /^dropped one/);
+});
+
+test('a missing file is an error that names the path', async () => {
+  await assert.rejects(run(site(), '<<< @/_scripts/absent.sh{sh}\n'), /absent\.sh/);
+});
diff --git a/packages/theme/test/remark-md-links.test.mjs b/packages/theme/test/remark-md-links.test.mjs
new file mode 100644
index 0000000..4d0bd3d
--- /dev/null
+++ b/packages/theme/test/remark-md-links.test.mjs
@@ -0,0 +1,54 @@
+// SPDX-License-Identifier: Apache-2.0
+// Copyright 2026 The OCX Authors
+
+import assert from 'node:assert/strict';
+import { test } from 'vitest';
+
+import { remarkMdLinks } from '../src/remark/md-links.mjs';
+
+const ROOT = '/site/content/docs';
+const OPTIONS = { root: ROOT, base: '/docs/' };
+
+async function run(source, page = 'guide/quickstart.md') {
+  const { unified } = await import('unified');
+  const { default: remarkParse } = await import('remark-parse');
+  const { VFile } = await import('vfile');
+  const processor = unified().use(remarkParse).use(remarkMdLinks, OPTIONS);
+  const file = new VFile({ value: source, path: `${ROOT}/${page}` });
+  return processor.run(processor.parse(file), file);
+}
+
+const urls = (tree) => {
+  const found = [];
+  const walk = (node) => {
+    if (node.type === 'link' || node.type === 'definition') found.push(node.url);
+    node.children?.forEach(walk);
+  };
+  walk(tree);
+  return found;
+};
+
+test('a relative .md link with a fragment becomes base + route + trailing slash + fragment', async () => {
+  assert.deepEqual(urls(await run('[a](./install.md#cli)\n')), ['/docs/guide/install/#cli']);
+});
+
+test('a parent-relative link and an index page route to their directory', async () => {
+  const tree = await run('[a](../reference/index.md) [b](../reference/environment.md)\n');
+  assert.deepEqual(urls(tree), ['/docs/reference/', '/docs/reference/environment/']);
+});
+
+test('a reference-style definition is rewritten', async () => {
+  assert.deepEqual(urls(await run('[a][ref]\n\n[ref]: ./install.md\n')), ['/docs/guide/install/']);
+});
+
+test('external, fragment-only and tree-leaving links are untouched', async () => {
+  const tree = await run('[a](https://example.com/x.md) [b](#local) [c](../../../CHANGELOG.md)\n');
+  assert.deepEqual(urls(tree), ['https://example.com/x.md', '#local', '../../../CHANGELOG.md']);
+});
+
+test('text inside a code fence and inline code is untouched', async () => {
+  const tree = await run('[a](./x.md#a)\n\n```md\n[b](./x.md#a)\n```\n\n`[c](./x.md#a)`\n');
+  assert.deepEqual(urls(tree), ['/docs/guide/x/#a']);
+  const fence = tree.children.find((node) => node.type === 'code');
+  assert.equal(fence.value, '[b](./x.md#a)');
+});
diff --git a/site/public/team/kate.jpg b/site/public/team/kate.jpg
new file mode 100644
index 0000000000000000000000000000000000000000..4dee1247a8ab36c1e534825055276eb477b0d235
GIT binary patch
literal 20806
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

literal 0
HcmV?d00001

diff --git a/site/src/content/docs/index.mdx b/site/src/content/docs/index.mdx
index b89cb2a..9da185c 100644
--- a/site/src/content/docs/index.mdx
+++ b/site/src/content/docs/index.mdx
@@ -105,7 +105,18 @@ Compose multiple packages in a single invocation. Each one contributes its varia
   <FeatureIcon name="blocks" />
 </FeatureSection>
 
-<FeatureSection title="Built for Automation" flip>
+<FeatureSection title="Content-Addressed and Deduplicated" flip>
+  <Fragment slot="text">
+
+Every object in the store is identified by its SHA-256 digest, a cryptographic fingerprint of its contents. If `astral-sh/uv:0.10` and `astral-sh/uv:latest` resolve to the same build, they share one directory on disk. Storage scales with distinct builds, not with the number of tags pointing at them.
+
+This also means verification is built in. A path under `sha256:…/` never changes its contents. Pin any package to a digest and you have a lockfile-free guarantee: no registry queries, no index lookups, just the hash.
+
+  </Fragment>
+  <FeatureIcon name="lock" />
+</FeatureSection>
+
+<FeatureSection title="Built for Automation">
   <Fragment slot="text">
 
 Every command returns structured JSON with `--format json`. Exit codes are meaningful. Environment variables compose cleanly. There is no interactive prompt, no "press Y to continue", no color code that breaks your parser.
@@ -116,7 +127,7 @@ Every command returns structured JSON with `--format json`. Exit codes are meani
   <FeatureIcon name="workflow" />
 </FeatureSection>
 
-<FeatureSection title="Relocatable and Air-Gap Ready">
+<FeatureSection title="Relocatable and Air-Gap Ready" flip>
   <Fragment slot="text">
 
 Every install is a self-contained directory: no global state, no registry database, no fragile symlinks into system paths. Copy the store to a USB drive, zip it into a CI cache artifact, or `scp` it to a machine behind a firewall.
@@ -131,6 +142,7 @@ The local index snapshot plus the content-addressed object store is everything `
 
 Join the community and help shape what comes next.
 
+- [Team](/team/): meet the people behind ocx.
 - [Roadmap](/docs/roadmap): see what we are building and what is coming next.
 - [Package catalog](https://index.ocx.sh): browse available packages on the public registry.
 - [Discord](https://discord.gg/mT2UCF8CVe): ask questions, share feedback, and follow development.
diff --git a/site/src/content/docs/team/index.mdx b/site/src/content/docs/team/index.mdx
new file mode 100644
index 0000000..427b8dc
--- /dev/null
+++ b/site/src/content/docs/team/index.mdx
@@ -0,0 +1,23 @@
+---
+title: Team
+description: The people who build and maintain ocx.
+---
+
+{/* doc_type: reference */}
+{/* doc_tier: integration */}
+
+import Avatar from '@ocx-sh/theme/components/ui/Avatar.astro';
+
+The people who build, maintain and promote ocx.
+
+## Michael Herwig
+
+<Avatar name="Michael Herwig" size="lg" />
+
+Creator and maintainer. Find him on [GitHub](https://www.github.com/michael-herwig), [GitLab](https://gitlab.com/michael-herwig) and [LinkedIn](https://www.linkedin.com/in/herwigm).
+
+## Katherine Draheim
+
+<Avatar name="Katherine Draheim" src="/team/kate.jpg" size="lg" />
+
+Marketing and community lead. Find her on [GitHub](https://www.github.com/kathescode) and [LinkedIn](https://www.linkedin.com/in/katherine-draheim/).
diff --git a/site/src/dist.test.ts b/site/src/dist.test.ts
index 811daa1..f35ee8b 100644
--- a/site/src/dist.test.ts
+++ b/site/src/dist.test.ts
@@ -20,12 +20,12 @@ describe('site dist', () => {
   });
 
   it('has the root claims, 404 and no other top-level directory', () => {
-    for (const f of ['index', 'integrations/index', 'apps/index', 'install/index', '404'])
+    for (const f of ['index', 'integrations/index', 'apps/index', 'install/index', 'team/index', '404'])
       expect(existsSync(`${dist}${f}.html`), f).toBe(true);
     const dirs = readdirSync(dist, { withFileTypes: true })
       .filter((e) => e.isDirectory())
       .map((e) => e.name);
-    expect(dirs.toSorted()).toEqual(['_astro', 'apps', 'install', 'integrations', 'pagefind']);
+    expect(dirs.toSorted()).toEqual(['_astro', 'apps', 'install', 'integrations', 'pagefind', 'team']);
   });
 
   it('names no legacy section bundle: the search chunk and every page leave out /docs/pagefind/ and kin', () => {
@@ -38,7 +38,7 @@ describe('site dist', () => {
       ...readdirSync(`${dist}_astro`)
         .filter((f) => f.endsWith('.js'))
         .map((f) => `_astro/${f}`),
-      ...['index', 'integrations/index', 'apps/index', 'install/index', '404'].map((f) => `${f}.html`),
+      ...['index', 'integrations/index', 'apps/index', 'install/index', 'team/index', '404'].map((f) => `${f}.html`),
     ];
     for (const f of built) for (const b of bundles) expect(read(f), `${f}: ${b}`).not.toContain(b);
   });
@@ -63,6 +63,7 @@ describe('site dist', () => {
       'https://ocx.sh/apps/',
       'https://ocx.sh/install/',
       'https://ocx.sh/integrations/',
+      'https://ocx.sh/team/',
     ]);
   });
 });
diff --git a/site/src/landing.test.ts b/site/src/landing.test.ts
index e57d7ab..1f8c4d8 100644
--- a/site/src/landing.test.ts
+++ b/site/src/landing.test.ts
@@ -25,11 +25,11 @@ describe('landing page (C-306)', () => {
       expect(h, href).toContain(`href="${href}"`);
   });
 
-  it('has the install tab set, four feature cards, five sections and the early-development banner', () => {
+  it('has the install tab set, four feature cards, six sections and the early-development banner', () => {
     const h = html();
     expect(h.match(/role="tab"/g)?.length).toBeGreaterThanOrEqual(5);
     expect(h.match(/class="[^"]*landing-cards__card/g)).toHaveLength(4);
-    expect(h.match(/class="ocx-feature"/g) ?? h.match(/<section class="ocx-feature/g)).toHaveLength(5);
+    expect(h.match(/class="ocx-feature"/g) ?? h.match(/<section class="ocx-feature/g)).toHaveLength(6);
     expect(h).toContain('Early Development');
   });
 
diff --git a/tests/budgets.mjs b/tests/budgets.mjs
index f06fb1f..b9b0700 100644
--- a/tests/budgets.mjs
+++ b/tests/budgets.mjs
@@ -101,14 +101,14 @@ export function budgetOf(path) {
 
 /**
  * One pathname pattern per class (regex source, anchored at the path start). The root site's pages
- * (landing, hubs, install, 404) are `content`: a new site page matches no class until it is listed here.
+ * (landing, hubs, install, team, 404) are `content`: a new site page matches no class until it is listed here.
  * @type {Record<PageClass, string>}
  */
 export const CLASS_PATTERNS = {
   showcase: '^/docs/components/',
   story: '^/docs/stories/',
   content:
-    '^(?:/docs/(?:probe/[^/]+/|shell/(?:neutral/)?|404\\.html|previews/)?|/|/integrations/|/apps/|/install/|/404\\.html)$',
+    '^(?:/docs/(?:probe/[^/]+/|shell/(?:neutral/)?|404\\.html|previews/)?|/|/integrations/|/apps/|/install/|/team/|/404\\.html)$',
 };
 
 /** Plain Astro pages on the Shell layout (`examples/starlight/src/pages/shell/`): built, not docs-tree files. */
diff --git a/tests/e2e/components-terminal.spec.ts b/tests/e2e/components-terminal.spec.ts
index 6bbe69e..17b7e31 100644
--- a/tests/e2e/components-terminal.spec.ts
+++ b/tests/e2e/components-terminal.spec.ts
@@ -39,19 +39,24 @@ function recordRequests(page: Page): Request[] {
   return seen;
 }
 
-test('WP13.1 Terminal: prefetches its cast on load, before any click (2210983da)', async ({ page }) => {
-  for (const [path, cast] of [
-    [PAGE, COLLAPSED_CAST],
-    [OPEN_PAGE, OPEN_CAST],
-  ] as const) {
-    const seen = recordRequests(page);
-    await page.goto(path);
-    await page.waitForLoadState('networkidle');
-    expect(
-      seen.map((r) => new URL(r.url()).pathname).filter((p) => p.endsWith('.cast')),
-      path,
-    ).toContain(cast);
-  }
+test('Terminal: a collapsed cast is fetched on expand, an open one when scrolled near, none on load', async ({
+  page,
+}) => {
+  const castPaths = (seen: Request[]) =>
+    seen.map((r) => new URL(r.url()).pathname).filter((p) => p.endsWith('.cast'));
+
+  let seen = recordRequests(page);
+  await page.goto(PAGE);
+  await page.waitForLoadState('networkidle');
+  expect(castPaths(seen)).not.toContain(COLLAPSED_CAST);
+  await toggle(page).first().click();
+  await expect.poll(() => castPaths(seen)).toContain(COLLAPSED_CAST);
+
+  seen = recordRequests(page);
+  await page.goto(OPEN_PAGE);
+  await page.waitForLoadState('networkidle');
+  await open(page).first().scrollIntoViewIfNeeded();
+  await expect.poll(() => castPaths(seen)).toContain(OPEN_CAST);
 });
 
 test('WP13.1 Terminal: base-URL resolution — casts are requested under /docs/ and answer 200 (WP13.1)', async ({
@@ -61,10 +66,12 @@ test('WP13.1 Terminal: base-URL resolution — casts are requested under /docs/
   page.on('response', (r) => {
     if (r.url().endsWith('.cast')) responses.push([new URL(r.url()).pathname, r.status()]);
   });
-  for (const path of [PAGE, OPEN_PAGE]) {
-    await page.goto(path);
-    await page.waitForLoadState('networkidle');
-  }
+  await page.goto(PAGE);
+  await toggle(page).first().click();
+  await page.waitForLoadState('networkidle');
+  await page.goto(OPEN_PAGE);
+  await open(page).first().scrollIntoViewIfNeeded();
+  await page.waitForLoadState('networkidle');
   expect(responses.length).toBeGreaterThanOrEqual(2);
   for (const [path, status] of responses) {
     expect(path).toMatch(/^\/docs\/casts\//);

--- Changes ---

packages/theme/package.json
  @@ -119,7 +119,8 @@
  -    "asciinema-player": "3.15.1"
  +    "asciinema-player": "3.15.1",
  +    "unist-util-visit": "^5.0.0"
     },
     "peerDependencies": {
       "@astrojs/starlight": ">=0.42.0 <0.43",
  @@ -136,7 +137,11 @@
  +    "remark-mdx": "^3.1.0",
  +    "remark-parse": "^11.0.0",
       "astro": "7.3.5",
  -    "typescript": "^6.0.3"
  +    "typescript": "^6.0.3",
  +    "unified": "^11.0.5",
  +    "vfile": "^6.0.3"
     }
   }
  +7 -2

packages/theme/src/components/Terminal.astro
  @@ -1,8 +1,8 @@
  -// The cast is fetched on page load; the player JS + CSS only when a player is created: an open
  -// terminal on first hover/focus/touch/click (near the viewport when autoPlay), a collapsed one on
  -// first expand.
  +// The cast is fetched when the terminal nears the viewport (open) or first expands (collapsed); the
  +// player JS + CSS only when a player is created: an open terminal on first hover/focus/touch/click
  +// (near the viewport when autoPlay), a collapsed one on first expand.
   // A collapsed terminal is a Zag collapsible (C-142): chrome = trigger, player area = content.
   import { base } from 'astro:config/client';
   import { withBase } from './base-url.mjs';
  +3 -3

packages/theme/src/components/terminal.mjs
  @@ -237,8 +237,8 @@ function wireControls(root, player) {
  - * Wire one rendered `.ocx-terminal` root: prefetch its cast immediately, and create the player
  - * lazily: an open terminal on its first hover, focus, touch or start click (as it nears the
  + * Wire one rendered `.ocx-terminal` root: fetch its cast when it is seen or opened, and create the
  + * player lazily: an open terminal on its first hover, focus, touch or start click (as it nears the
    * viewport only with `autoPlay`); a collapsed one (a Zag collapsible, C-142) on its first expand,
    * reported by `ocx:collapsible:change`.
    * @param {HTMLElement} root
  @@ -250,13 +250,21 @@ export function mountTerminal(root, loadPlayer = loadDefault) {
  -  // Prefetch on mount, collapsed or not, so expanding is instant (ocx 2210983da).
  -  // Low priority: many casts on one page must not compete with the main content.
  -  const data = fetch(root.dataset['src'] ?? '', { priority: 'low' }).then((r) => {
  -    if (!r.ok) throw new Error(`HTTP ${r.status}`);
  -    return r.text();
  -  });
  -  data.catch(() => {}); // surfaced in the player area once a player is wanted
  +  // The cast is fetched when the terminal is seen (an open one near the viewport) or opened (a collapsed
  +  // one), never on page load: a docs page with many casts must not pay for the ones nobody scrolls to.
  +  // Low priority: casts must not compete with the main content.
  +  /** @type {Promise<string> | undefined} */
  +  let cast;
  +  const fetchCast = () => {
  +    if (!cast) {
  +      cast = fetch(root.dataset['src'] ?? '', { priority: 'low' }).then((r) => {
  +        if (!r.ok) throw new Error(`HTTP ${r.status}`);
  +        return r.text();
  +      });
  +      cast.catch(() => {}); // surfaced in the player area once a player is wanted
  +    }
  +    return cast;
  +  };
   
     // Our start overlay (SSR'd; asciinema's own is hidden in CSS): hidden once playback starts, and
     // a click starts it through the same create path as everything else.
  @@ -266,12 +274,12 @@ export function mountTerminal(root, loadPlayer = loadDefault) {
  -    (player ??= Promise.all([loadPlayer(), data, document.fonts?.ready])
  +    (player ??= Promise.all([loadPlayer(), fetchCast(), document.fonts?.ready])
         .then(([m, d]) => {
           const p = m.create({ data: d }, area, {
             ...opts,
             autoPlay,
  -          preload: true, // the data is already here: show 00:00, not --:--, before play
  +          preload: true, // the data is already here (create waits for it): show 00:00, not --:--, before play
             theme: 'ocx',
             terminalFontFamily: 'var(--ocx-font-mono)',
             controls: false, // our own bar (below) drives play/pause/seek/fullscreen through the player API
  @@ -296,9 +304,20 @@ export function mountTerminal(root, loadPlayer = loadDefault) {
  -    // Nothing loads before interaction: the SSR box and start overlay already hold the layout.
  +    // The player loads on interaction (the SSR box and start overlay already hold the layout); only the
  +    // cast text is fetched ahead, once the terminal nears the viewport.
  +    const seen = new IntersectionObserver(
  +      (entries) => {
  +        if (!entries.some((e) => e.isIntersecting)) return;
  +        seen.disconnect();
  +        void fetchCast();
  +      },
  +      { rootMargin: '200px' },
  +    );
  +    seen.observe(root);
       const arm = () => {
         for (const type of INTERACTION) root.removeEventListener(type, arm);
  +      seen.disconnect();
         void create(false);
       };
       for (const type of INTERACTION) root.addEventListener(type, arm, { passive: true });
  +31 -12

packages/theme/src/nav.json
  @@ -42,6 +42,7 @@
  +    { "path": "/team/", "repo": "ocx-sh/website", "search": false },
       { "path": "/docs/", "repo": "ocx-sh/ocx", "search": true },
       { "path": "/schemas/", "repo": "ocx-sh/ocx", "search": false },
       { "path": "/catalog/", "repo": "ocx-sh/index", "search": false },
  +1 -0

packages/theme/src/remark/heading-ids.mjs
  @@ -0,0 +1,25 @@
  +// SPDX-License-Identifier: Apache-2.0
  +// Copyright 2026 The OCX Authors
  +
  +import { visit } from 'unist-util-visit';
  +
  +/**
  + * Remark plugin: a heading whose last text ends in ` {#id}` (markdown) or the escaped ` \{#id\}` (MDX, where the
  + * parser already decoded the escapes) gets `data.hProperties.id = id`, and the suffix leaves the heading text.
  + * A last text node that is only the suffix is dropped. Headings without a suffix are left untouched.
  + *
  + * @returns {import('unified').Plugin<[], import('mdast').Root>}
  + */
  +export function remarkHeadingIds() {
  +  return (tree) => {
  +    visit(tree, 'heading', (heading) => {
  +      const last = heading.children.at(-1);
  +      const match = last?.type === 'text' ? /\s*\{#([^\s{}]+)\}\s*$/.exec(last.value) : null;
  +      if (!match) return;
  +      const id = match[1];
  +      last.value = last.value.slice(0, match.index);
  +      if (last.value === '') heading.children.pop();
  +      heading.data = { ...heading.data, id, hProperties: { ...heading.data?.hProperties, id } };
  +    });
  +  };
  +}
  +25 -0

packages/theme/src/remark/includes.mjs
  @@ -0,0 +1,62 @@
  +// SPDX-License-Identifier: Apache-2.0
  +// Copyright 2026 The OCX Authors
  +
  +import { readFileSync } from 'node:fs';
  +import { dirname, join, resolve } from 'node:path';
  +
  +const CODE_INCLUDE = /^<<<\s+@\/(\S+?)(?:\{(\S+)\})?$/;
  +const MD_INCLUDE = /^<!--@include:\s*(\S+?)(?:\{(\d+),\})?\s*-->$/;
  +
  +function read(path) {
  +  try {
  +    return readFileSync(path, 'utf8');
  +  } catch (cause) {
  +    throw new Error(`include: cannot read ${path}`, { cause });
  +  }
  +}
  +
  +/**
  + * Remark plugin for the two include forms the pages use.
  + *
  + * - A paragraph that is only `<<< @/<path>{lang}` becomes a `code` node with `lang` and the file's text, one
  + *   trailing newline removed. `@` is `srcDir`.
  + * - An `html` node that is only `<!--@include: <path>{N,}-->` is replaced by the file's lines N to the end
  + *   (1-based), parsed as markdown. `<path>` is relative to the including file; `{N,}` is optional.
  + *
  + * @param {{ srcDir: string }} options `srcDir` is the absolute directory `@` names.
  + * @returns {import('unified').Plugin<[], import('mdast').Root>}
  + */
  +export function remarkIncludes({ srcDir }) {
  +  const processor = this;
  +  // A replacement list for the node, or null to keep it and descend.
  +  const expand = (node, dir) => {
  +    if (node.type === 'paragraph' && node.children.length === 1 && node.children[0].type === 'text') {
  +      const code = CODE_INCLUDE.exec(node.children[0].value.trim());
  +      if (code) {
  +        const value = read(resolve(srcDir, code[1])).replace(/\n$/, '');
  +        return [{ type: 'code', lang: code[2] ?? null, meta: null, value }];
  +      }
  +    }
  +    if (node.type === 'html') {
  +      const md = MD_INCLUDE.exec(node.value.trim());
  +      if (md) {
  +        const path = join(dir, md[1]);
  +        const text = read(path).split('\n').slice(md[2] ? Number(md[2]) - 1 : 0).join('\n');
  +        const included = processor.parse(text);
  +        walk(included, dirname(path));
  +        return included.children;
  +      }
  +    }
  +    return null;
  +  };
  +  const walk = (parent, dir) => {
  +    if (!parent.children) return;
  +    parent.children = parent.children.flatMap((child) => {
  +      const replacement = expand(child, dir);
  +      if (replacement) return replacement;
  +      walk(child, dir);
  +      return [child];
  +    });
  +  };
  +  return (tree, file) => walk(tree, dirname(file.path));
  +}
  +62 -0

packages/theme/src/remark/md-links.mjs
  @@ -0,0 +1,42 @@
  +// SPDX-License-Identifier: Apache-2.0
  +// Copyright 2026 The OCX Authors
  +
  +import { posix, relative, sep } from 'node:path';
  +
  +import { visit } from 'unist-util-visit';
  +
  +const PAGE_LINK = /^([^#?]+)\.mdx?([#?].*)?$/;
  +const EXTERNAL = /^[a-z][a-z0-9+.-]*:|^\/\//i;
  +
  +/** The route a `.md` link target names, keeping its `#fragment`/`?query`; the target itself when it is not one. */
  +function route(target, page, base) {
  +  const match = PAGE_LINK.exec(target);
  +  if (!match || EXTERNAL.test(target)) return target;
  +  const path = match[1].startsWith('/')
  +    ? posix.normalize(match[1].slice(1))
  +    : posix.join(posix.dirname(page), match[1]);
  +  if (path.startsWith('..')) return target;
  +  const dir = path.replace(/(^|\/)index$/, '');
  +  return `${base}${dir ? `${dir}/` : ''}${match[2] ?? ''}`;
  +}
  +
  +/**
  + * Remark plugin: rewrites `link` and `definition` URLs that name a `.md`/`.mdx` page inside the docs tree to the
  + * page's route, `base` + directory + trailing slash, keeping the `#fragment`. `index` pages route to their
  + * directory. External URLs, bare `#fragment` links and targets leaving the tree are left alone. Code and inline
  + * code are never touched.
  + *
  + * @param {{ root: string, base: string }} options `root` is the absolute docs content root; `base` the site path
  + *   of that root, for example `/docs/`. The page's own path is `file.path` relative to `root`.
  + * @returns {import('unified').Plugin<[], import('mdast').Root>}
  + */
  +export function remarkMdLinks({ root, base }) {
  +  const prefix = base.endsWith('/') ? base : `${base}/`;
  +  return (tree, file) => {
  +    const page = relative(root, file.path).split(sep).join('/');
  +    if (page.startsWith('..')) return;
  +    visit(tree, ['link', 'definition'], (node) => {
  +      node.url = route(node.url, page, prefix);
  +    });
  +  };
  +}
  +42 -0

packages/theme/src/starlight/index.mjs
  @@ -6,6 +6,9 @@ import { fontProviders } from 'astro/config';
  +import { remarkHeadingIds } from '../remark/heading-ids.mjs';
  +import { remarkIncludes } from '../remark/includes.mjs';
  +import { remarkMdLinks } from '../remark/md-links.mjs';
   import { EXPRESSIVE_CODE } from './ec.mjs';
   import { italicOptional } from './fonts.mjs';
   
  @@ -89,6 +92,7 @@ const OPTIONS_ID = 'virtual:ocx-theme/options';
  + *   docs?: { root: string, srcDir?: string },
    * }} OcxThemeOptions
    */
   
  @@ -96,10 +100,13 @@ const OPTIONS_ID = 'virtual:ocx-theme/options';
  + * Trailing `{#id}` on a heading is always its anchor. `docs` adds the migrated-docs remark plugins:
  + * `root` is the absolute content directory whose `.md` links become routes under the site base, `srcDir`
  + * (default `root`) the directory `<<< @/` includes resolve against.
    * @param {OcxThemeOptions} [options]
    * @returns {import('@astrojs/starlight/types').StarlightPlugin}
    */
  -export default function ocxTheme({ breadcrumbs = {} } = {}) {
  +export default function ocxTheme({ breadcrumbs = {}, docs } = {}) {
     // Defaults live in the route middleware, which reads this.
     const runtime = { breadcrumbs };
     return {
  @@ -125,6 +132,17 @@ export default function ocxTheme({ breadcrumbs = {} } = {}) {
  +                markdown: {
  +                  remarkPlugins: [
  +                    remarkHeadingIds,
  +                    ...(docs
  +                      ? [
  +                          [remarkMdLinks, { root: docs.root, base: own }],
  +                          [remarkIncludes, { srcDir: docs.srcDir ?? docs.root }],
  +                        ]
  +                      : []),
  +                  ],
  +                },
                   // Per-component stylesheets (ui primitives, explorer) inline instead of each
                   // costing a render-blocking request, which Lighthouse charges as an extra round trip
                   // before LCP. The shared Starlight bundle stays external and cached. Other assets keep
  +19 -1

packages/theme/test/components-terminal.test.ts
  @@ -362,12 +362,26 @@ describe('WP13.1 Terminal client', () => {
  -  it('WP13.1 Terminal: prefetches the cast on mount, not on first click, even when collapsed (2210983da)', async () => {
  +  it('a collapsed terminal fetches its cast on the first expand, not on mount or in view', async () => {
       const h = await mount({ src: '/casts/x.cast', collapsed: true });
  -    expect(h.fetchMock).toHaveBeenCalledTimes(1);
  +    h.intersect(true);
  +    await flush();
  +    expect(h.fetchMock).not.toHaveBeenCalled();
  +    await h.toggle();
  +    await vi.waitFor(() => expect(h.fetchMock).toHaveBeenCalledTimes(1));
       expect(String(h.fetchMock.mock.calls[0]?.[0])).toBe(h.root.dataset['src']);
     });
   
  +  it('an open terminal fetches its cast once it nears the viewport, not on mount', async () => {
  +    const h = await mount({ src: '/casts/x.cast', rows: 8 });
  +    await flush();
  +    expect(h.fetchMock).not.toHaveBeenCalled();
  +    h.intersect(true);
  +    h.intersect(true);
  +    await vi.waitFor(() => expect(h.fetchMock).toHaveBeenCalledTimes(1));
  +    expect(h.load).not.toHaveBeenCalled();
  +  });
  +
     it('WP13.1 Terminal: collapsed terminal does not import the player before the first expand (Lighthouse)', async () => {
       const h = await mount({ src: '/casts/x.cast', collapsed: true });
       h.intersect(true);
  @@ -401,7 +415,7 @@ describe('WP13.1 Terminal client', () => {
  -  it('WP13.1 Terminal: the player is created from the prefetched data, with no second fetch (2210983da)', async () => {
  +  it('WP13.1 Terminal: the player is created from the fetched data, with no second fetch (2210983da)', async () => {
       const h = await mount({ src: '/casts/x.cast', collapsed: true });
       await h.toggle();
       await vi.waitFor(() => expect(h.create).toHaveBeenCalledTimes(1));
  @@ -519,6 +533,8 @@ describe('WP13.1 Terminal client', () => {
  +    h.intersect(true);
  +    await vi.waitFor(() => expect(h.fetchMock).toHaveBeenCalledTimes(1));
       expect(h.fetchMock.mock.calls[0]?.[1]).toMatchObject({ priority: 'low' });
     });
   
  +19 -3

packages/theme/test/registry.test.ts
  @@ -220,7 +220,7 @@ describe('C-008 pure registry functions', () => {
  -    ['ocx-sh/website', ['/', '/integrations/', '/apps/', '/install/']],
  +    ['ocx-sh/website', ['/', '/integrations/', '/apps/', '/install/', '/team/']],
       ['ocx-sh/ocx', ['/docs/', '/schemas/']],
       ['ocx-sh/none', []],
     ])('C-008: claimsOf(%s)', (repo, paths) => {
  +1 -1

packages/theme/test/remark-heading-ids.test.mjs
  @@ -0,0 +1,55 @@
  +// SPDX-License-Identifier: Apache-2.0
  +// Copyright 2026 The OCX Authors
  +
  +import assert from 'node:assert/strict';
  +import { test } from 'vitest';
  +
  +import { remarkHeadingIds } from '../src/remark/heading-ids.mjs';
  +
  +// The parser packages arrive with the site's package.json; import them when a test runs.
  +async function headings(source, { mdx = false } = {}) {
  +  const { unified } = await import('unified');
  +  const { default: remarkParse } = await import('remark-parse');
  +  const { VFile } = await import('vfile');
  +  const processor = unified().use(remarkParse);
  +  if (mdx) processor.use((await import('remark-mdx')).default);
  +  processor.use(remarkHeadingIds);
  +  const file = new VFile({ value: source, path: '/docs/page.md' });
  +  const tree = await processor.run(processor.parse(file), file);
  +  return tree.children.filter((node) => node.type === 'heading');
  +}
  +
  +const text = (node) => node.children.map((child) => child.value ?? '').join('');
  +
  +test('markdown {#id} becomes the heading id and leaves the text', async () => {
  +  const [heading] = await headings('## Install the CLI {#install-cli}\n');
  +  assert.equal(heading.data?.hProperties?.id, 'install-cli');
  +  assert.equal(text(heading), 'Install the CLI');
  +});
  +
  +test('MDX \\{#id\\} becomes the heading id and leaves the text', async () => {
  +  const [heading] = await headings('## Install the CLI \\{#install-cli\\}\n', { mdx: true });
  +  assert.equal(heading.data?.hProperties?.id, 'install-cli');
  +  assert.equal(text(heading), 'Install the CLI');
  +});
  +
  +test('a suffix after inline code drops the whole trailing text node', async () => {
  +  const [heading] = await headings('### `OCX_HOME` {#ocx-home}\n');
  +  assert.equal(heading.data?.hProperties?.id, 'ocx-home');
  +  assert.deepEqual(
  +    heading.children.map((child) => child.type),
  +    ['inlineCode'],
  +  );
  +});
  +
  +test('a heading without a suffix is untouched', async () => {
  +  const [heading] = await headings('## Plain heading\n');
  +  assert.equal(heading.data, undefined);
  +  assert.equal(text(heading), 'Plain heading');
  +});
  +
  +test('a brace group that is not a trailing id is untouched', async () => {
  +  const [heading] = await headings('## Set {#not-last} twice\n');
  +  assert.equal(heading.data, undefined);
  +  assert.equal(text(heading), 'Set {#not-last} twice');
  +});
  +55 -0

packages/theme/test/remark-includes.test.mjs
  @@ -0,0 +1,57 @@
  +// SPDX-License-Identifier: Apache-2.0
  +// Copyright 2026 The OCX Authors
  +
  +import assert from 'node:assert/strict';
  +import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
  +import { tmpdir } from 'node:os';
  +import { join } from 'node:path';
  +import { test } from 'vitest';
  +
  +import { remarkIncludes } from '../src/remark/includes.mjs';
  +
  +function site() {
  +  const srcDir = mkdtempSync(join(tmpdir(), 'includes-'));
  +  mkdirSync(join(srcDir, '_scripts'));
  +  mkdirSync(join(srcDir, 'docs'));
  +  writeFileSync(join(srcDir, '_scripts', 'demo.sh'), '#!/bin/sh\necho demo\n');
  +  writeFileSync(join(srcDir, 'snippet.md'), 'dropped one\ndropped two\n# Kept heading\n\nkept body\n');
  +  return srcDir;
  +}
  +
  +async function run(srcDir, source) {
  +  const { unified } = await import('unified');
  +  const { default: remarkParse } = await import('remark-parse');
  +  const { VFile } = await import('vfile');
  +  const processor = unified().use(remarkParse).use(remarkIncludes, { srcDir });
  +  const file = new VFile({ value: source, path: join(srcDir, 'docs', 'page.md') });
  +  return processor.run(processor.parse(file), file);
  +}
  +
  +test('<<< @/path{lang} becomes a fenced block of the file', async () => {
  +  const tree = await run(site(), '<<< @/_scripts/demo.sh{sh}\n');
  +  assert.equal(tree.children.length, 1);
  +  assert.deepEqual(
  +    { type: tree.children[0].type, lang: tree.children[0].lang, value: tree.children[0].value },
  +    { type: 'code', lang: 'sh', value: '#!/bin/sh\necho demo' },
  +  );
  +});
  +
  +test('<!--@include: path{3,}--> splices lines 3 to the end as markdown', async () => {
  +  const tree = await run(site(), '<!--@include: ../snippet.md{3,}-->\n');
  +  assert.deepEqual(
  +    tree.children.map((node) => node.type),
  +    ['heading', 'paragraph'],
  +  );
  +  assert.equal(tree.children[0].children[0].value, 'Kept heading');
  +  assert.equal(tree.children[1].children[0].value, 'kept body');
  +});
  +
  +test('an include without a range takes the whole file', async () => {
  +  const tree = await run(site(), '<!--@include: ../snippet.md-->\n');
  +  assert.equal(tree.children[0].type, 'paragraph');
  +  assert.match(tree.children[0].children[0].value, /^dropped one/);
  +});
  +
  +test('a missing file is an error that names the path', async () => {
  +  await assert.rejects(run(site(), '<<< @/_scripts/absent.sh{sh}\n'), /absent\.sh/);
  +});
  +57 -0

packages/theme/test/remark-md-links.test.mjs
  @@ -0,0 +1,54 @@
  +// SPDX-License-Identifier: Apache-2.0
  +// Copyright 2026 The OCX Authors
  +
  +import assert from 'node:assert/strict';
  +import { test } from 'vitest';
  +
  +import { remarkMdLinks } from '../src/remark/md-links.mjs';
  +
  +const ROOT = '/site/content/docs';
  +const OPTIONS = { root: ROOT, base: '/docs/' };
  +
  +async function run(source, page = 'guide/quickstart.md') {
  +  const { unified } = await import('unified');
  +  const { default: remarkParse } = await import('remark-parse');
  +  const { VFile } = await import('vfile');
  +  const processor = unified().use(remarkParse).use(remarkMdLinks, OPTIONS);
  +  const file = new VFile({ value: source, path: `${ROOT}/${page}` });
  +  return processor.run(processor.parse(file), file);
  +}
  +
  +const urls = (tree) => {
  +  const found = [];
  +  const walk = (node) => {
  +    if (node.type === 'link' || node.type === 'definition') found.push(node.url);
  +    node.children?.forEach(walk);
  +  };
  +  walk(tree);
  +  return found;
  +};
  +
  +test('a relative .md link with a fragment becomes base + route + trailing slash + fragment', async () => {
  +  assert.deepEqual(urls(await run('[a](./install.md#cli)\n')), ['/docs/guide/install/#cli']);
  +});
  +
  +test('a parent-relative link and an index page route to their directory', async () => {
  +  const tree = await run('[a](../reference/index.md) [b](../reference/environment.md)\n');
  +  assert.deepEqual(urls(tree), ['/docs/reference/', '/docs/reference/environment/']);
  +});
  +
  +test('a reference-style definition is rewritten', async () => {
  +  assert.deepEqual(urls(await run('[a][ref]\n\n[ref]: ./install.md\n')), ['/docs/guide/install/']);
  +});
  +
  +test('external, fragment-only and tree-leaving links are untouched', async () => {
  +  const tree = await run('[a](https://example.com/x.md) [b](#local) [c](../../../CHANGELOG.md)\n');
  +  assert.deepEqual(urls(tree), ['https://example.com/x.md', '#local', '../../../CHANGELOG.md']);
  +});
  +

... (more changes truncated)
  +48 -0
[full diff: rtk git diff --no-compact]
~~~~~~~~

## Page budget: sidebar and header cost (not in the patch above)

`ocx` ships `website/src/route-middleware.ts`: a Starlight route middleware that replaces every top-level sidebar
group except the current page's with one link to the group's first page. The theme's `Sidebar.astro` renders the
whole tree on every page (111 entries, about 310 elements, 3.4 KB gzip on ocx.sh/docs), and its top level ignores
`collapsed`, so no config option trims it. Suggested upstream shape: a `sidebar: { expand: 'current' }` option on
`ocxTheme()` that does this in the theme's own route middleware (it already rewrites `route.sidebar` for `flatten`).

Measured on the ocx docs (112 pages): chrome 597 elements and 7.4 KB gzip per page before, 399 and 5.7 KB after;
pages inside 14,200 B and 800 elements went from 7 to 31. The remaining chrome is the header, 196 elements and
3.25 KB gzip on every page, almost all of it the ecosystem mega-menu popover (`.ocx-mega`, an icon and a preview
per entry). Rendering that panel on first open instead would free about 2.5 KB and 150 elements per page, and lift
the pass count to about 54 on its own.
