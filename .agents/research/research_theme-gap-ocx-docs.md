# Research: @ocx-sh/theme gaps for porting ocx docs

## Metadata

**Date:** 2026-10-11
**Domain:** web
**Triggered by:** `.agents/discussions/docs-next-level.md`
**Expires:** 2026-12-11

## Direct Answer

27 features mapped: 14 covered (code-group→Tabs, containers→asides, details, Tooltip,
Tree, PlatformIcons, DependencyExplorer, Terminal, Steps, TOC, sidebar, Pagefind, nav),
5 port-script work, 3 missing, 1 theme change. Converter exists:
`../ocx-website/scripts/samples/convert.ts` (ocx preview, `scripts/previews/sites.mjs:20`).

Theme changes needed (PR on ocx-website):
1. `{#id}` heading anchors (928 in 44 files; DOC-NAV-07 requires explicit anchors;
   `convert.ts:256` strips them today).
2. Lazy cast prefetch (`terminal.mjs:253-258` fetches all casts on mount).
3. Roadmap components (Icons8 assets forbidden by `design-rules.md:87-91`) — or rewrite page.
4. Optional remark plugin: `.md`-relative link rewrite (1179 links) + `<<<` includes (39).

Port work: `/schemas/` second deploy dist; `<!--@include CHANGELOG.md-->`;
`.mdx` for component pages; `trailingSlash: 'always'` changes every URL.
Constraints: astro ^7, starlight 0.42.x, Node ≥22, pnpm 11 git-subdir pin with
`allowBuilds`; `product-tech-strategy.md` says Bun/VitePress (to update).

Lighthouse-100 blockers: HTML cap 14,200 B gzip/page (`budgets.mjs:27`, ≈400 lines,
unmeasured) — 10 pages over (command-line, configuration, user-guide, environment,
metadata, patches, indices, announcing, env-composition, execution-records); cast
prefetch vs 147 KiB budget; ~50-item sidebar in every page; 800-element DOM cap
(storage.md trees); no ocx Lighthouse baseline yet.
