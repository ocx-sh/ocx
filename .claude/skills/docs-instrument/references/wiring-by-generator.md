# Wiring by generator

Read this when you know which generator the repository runs and need the exact
config key, path or flag for one mechanism.

Confidence differs by row. MkDocs Material, VitePress and mdBook rows were
measured on real builds during the research that produced this rule set.
Docusaurus and Starlight rows come from vendor documentation and need one
confirming build before you trust them. Rows marked "verify once" were never
run at all.

Contents: [Detection](#detection) · [Declaration carrier](#declaration-carrier) ·
[Link checking](#link-checking) · [Fence tags](#fence-tags) ·
[Navigation shape](#navigation-shape) · [Search](#search) ·
[Zero-result beacon](#zero-result-beacon) · [Interactive defaults](#interactive-defaults) ·
[Sphinx](#sphinx)

## Detection

Run this first. No hit means the DOC-NAV family reports not applicable and exits
0 (DOC-NAV-01). The VitePress paths are mandatory, because a discovery list of
`mkdocs.yml` and `SUMMARY.md` alone silently skips a VitePress site.

| Generator | Config file | Build output |
|---|---|---|
| MkDocs Material | `mkdocs.yml` | `site/` |
| VitePress | `.vitepress/config.*`, `docs/.vitepress/config.*`, `website/.vitepress/config.*` | `.vitepress/dist/` |
| mdBook | `book.toml`, `docs/book.toml` | `book/` |
| Docusaurus | `docusaurus.config.*` | `build/` |
| Starlight | `astro.config.*` | `dist/` |
| Sphinx | `docs/conf.py` | `_build/html/` (verify once) |

## Declaration carrier

One comment opener per markup family, inside the first 12 lines, never above
existing frontmatter, never as YAML frontmatter.

| Generator | Carrier | Why |
|---|---|---|
| MkDocs Material | `<!-- doc_type: how-to -->` | HTML comment invisible in output |
| VitePress | `<!-- ... -->` | comment stripped from output |
| mdBook | `<!-- ... -->` | frontmatter renders as a fake heading and enters the search index |
| Docusaurus | `{/* doc_type: how-to */}` | an HTML comment is a build error at the default `markdown.format: mdx` |
| Starlight | `<!-- ... -->` in `.md`, `{/* ... */}` in `.mdx` | same MDX compiler, same hard error |
| Sphinx MyST | `% doc_type: how-to` | MyST's own line comment |
| Sphinx reStructuredText | `.. doc_type: how-to` | rST comment |

Set `markdown.format: detect` in the Docusaurus config before a retrofit, or
every plain `.md` page is parsed as MDX. Starlight also needs `extend` in
`docsSchema()` before any new frontmatter key is accepted, which is one more
reason the carrier is a comment.

## Link checking

The generator's own strict build satisfies the built-output obligation. Do not
replace a working strict build with an external checker.

| Generator | Strict build | Fallback |
|---|---|---|
| MkDocs Material | `mkdocs build --strict` | `lychee --include-fragments site/` |
| mdBook | `mdbook-linkcheck` as a backend in `book.toml` | `lychee --include-fragments book/` |
| VitePress | none shipped | `lychee --include-fragments .vitepress/dist/` |
| Docusaurus | `onBrokenLinks: 'throw'` is the default, `onBrokenAnchors` defaults to warn, so raise it | `lychee --include-fragments build/` |
| Starlight | verify once, then fall back | `lychee --include-fragments dist/` |

The raw-markdown pass is separate and needs both a source root and an exclusion
for every page whose anchors are generated at build time. `checks/links_raw.py`
does the resolving and prints what it skipped. `checks/lychee.toml` is the
built-output config to copy.

## Fence tags

Use one hyphen-joined token everywhere, such as `python-no-run` or `shell-tier2`.
Measured on real builds: MkDocs Material falls back to Pygments `text`, mdBook to
`no-highlight`, VitePress to Shiki `txt` with a build warning. No generator loses
content.

The space-separated form is the syntax the tools themselves document, and it
corrupts a MkDocs Material page. `pymdownx.superfences` does not treat a fence
with whitespace in its info string as an opening fence. Everything after it is
swallowed into one wrongly classed block. mdBook and VitePress tolerate it. Use a
tool-native space attribute only on a site that will never render under MkDocs
Material (DOC-EX-21).

Enforce the tier list with markdownlint `MD040` `allowed_languages`, on changed
lines only.

## Navigation shape

What `checks/nav_depth.py` parses, and the trap in each.

| Generator | Structure | Trap |
|---|---|---|
| MkDocs Material | `nav:` nesting depth | a strict YAML loader hard-fails on `!ENV` and `!!python/name:` tags, measured on 4 of 7 real configs. Pass unknown tags through as raw scalars |
| VitePress | the `sidebar` array's `items` nesting | the config is JavaScript or TypeScript, so parse it as text, not as YAML |
| mdBook | `SUMMARY.md` indent depth, with `# Part Title` as a group divider | skip the file's own mandatory first `# ` line, which a naive grep counts as a divider |
| Docusaurus | `sidebars.*` | verify once |
| Starlight | `sidebar` in `astro.config.*` | verify once |

Breadcrumbs at depth 3. Material has `navigation.path` in `theme.features` since
9.7.0. VitePress and mdBook ship none, so that arm is a reading check.
Docusaurus renders breadcrumbs by default. Starlight, verify once.

## Search

Never add a synonym or query-relaxation key. None of lunr, minisearch or
elasticlunr reads `synonyms`, `removeWordsIfNoResults`, `optionalWords`,
`ignorePlurals` or `removeStopWords`, so the edit does nothing and raises no
error. Fix a zero-result query by rewording the page (DOC-NAV-11).

Never flatten title and body boosts to equal weight. mdBook defaults to
`boost-title = 2` against `boost-paragraph = 1`, so an absent
`[output.html.search]` section already complies. For VitePress, keep
`searchOptions.boost` absent or title above text.

## Zero-result beacon

Watch the rendered output or a documented public API, never the engine's
internal search file. Fire one named event and call the sink from the same file.
A grep for the event name alone cannot tell a wired beacon from a dead one. So
the check requires the listener registration and a `fetch(` or `sendBeacon(`
call in that same file. It then requires the event name to survive into the
built bundle.

| Generator | Where the script lives | What it watches |
|---|---|---|
| MkDocs Material | an `extra_javascript` entry in `mkdocs.yml`, a plain file under `docs/javascripts/` | a `MutationObserver` on the results container, keyed to the engine's own localized no-results string, never a CSS class |
| VitePress | `enhanceApp` in `.vitepress/theme/index.ts`, or a script referenced from `head` | the `.VPLocalSearchBox` results container's localized no-results string |
| mdBook | an `additional-js` entry in `book.toml`, never `theme/searcher.js` | the rendered `#searchresults` container's empty-state text |
| Docusaurus | a client module or a theme swizzle | the local-search plugin's rendered empty state, verify once |
| Starlight | a script in the site head | `pagefind.search()` returning zero results, since Starlight ships Pagefind |

Build-time verification for all rows. Grep the source file for the event name.
Confirm the listener and the network call sit in that same file. Then confirm the
name survives into the build output directory named in the detection table.

## Interactive defaults

| Generator | Copy button | Runnable code |
|---|---|---|
| MkDocs Material | off by default, set `content.code.copy` and `content.code.annotate` in `theme.features` | none |
| VitePress | shipped, injected unconditionally. Do not hand-roll one | none |
| mdBook | shipped unless `copyable = false`. Do not hand-roll one | state `runnable` and `editable` under `[output.html.playground]` whenever the book holds a Rust example |
| Docusaurus | shipped | none |
| Starlight | shipped via its code component | none |

A published Rust crate with runnable doc comments needs
`#![doc(html_playground_url = "https://play.rust-lang.org/")]` on the crate root,
or rustdoc renders no Run button.

## Sphinx

Sphinx is the one generator in this table with no measured fixture. Its carriers
are known from primary documentation, `%` for MyST and `..` for reStructuredText.
Everything else, including the build directory, the strict mode and the nav
parse, needs one confirming build before you wire a gate to it. Say so in the
manifest rather than assuming a MkDocs answer transfers.
