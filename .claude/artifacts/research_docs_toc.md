# Research: Docs sidebar and table of contents against theme 0.5.0

**Date:** 2026-11-16
**Question:** The owner reports that the docs TOC "still has hard to distinguish captions and does
not use the proper new collapsible style". What renders today, what does the theme offer, what
must change where?
**Method:** read-only comparison of the built site (`website/dist/docs`), the OCX site config, and
the theme source at `ocx-sh/website` `main` (which equals `v0.5.0` for every sidebar and TOC file:
`git diff v0.5.0..main --stat` lists deploy files only).

## Findings

### A repin does not help

OCX already pins `@ocx-sh/theme` 0.5.0. The sidebar caption and collapsible styling
(theme commit `7eb2ec5`) and the phone menu (`8b63b40`) are in that release. The theme registers
its `Sidebar`, `TableOfContents` and `MobileTableOfContents` overrides automatically
(`packages/theme/src/starlight/index.mjs:99-106`); there is no option to switch them on.

### The OCX route middleware defeats the sidebar design

`website/src/route-middleware.ts:9-27` rewrites every top-level sidebar group that does not hold
the current page into a plain link to the group's first page, with `class="large"`.

- No stylesheet targets `.large`, so seven of eight group captions render as ordinary page links
  (13px sans regular, `fg-muted`). Only the current group keeps the caption style
  (mono caps, semibold).
- The rewritten entries are links, not groups, so nothing collapses or expands.
- The middleware exists for page weight: an expanded sidebar costs about 310 elements and
  3.4 KB gzip per page, against the 900-element Lighthouse budget.
- The theme defers only *nested* collapsed groups (`sidebar-groups.mjs:1-40`,
  `Sidebar.astro:46-48`). A top-level group always renders inline. Its own note warns that a
  middleware rewriting the sidebar makes groups render inline instead of deferred.

### The right-hand "On this page" list has a weak hierarchy

- Caption: mono 10.5px caps, semibold, `--ocx-color-fg-subtle` (`starlight.css:263`).
- h2 entries: 12.5px `fg-muted`. h3 entries: 12px regular, one indent step deeper
  (`TableOfContents.astro:77-81`, `starlight.css:267-271`).
- h2 and h3 differ by 0.5px and an indent. Nothing sets a heavier weight or a stronger colour on
  h2 rows.
- The desktop list has no collapsible form. The only details-based TOC is the phone one
  (`MobileTableOfContents.astro:13`). `components/Toc.astro` is a scroll-spy for pages without
  Starlight's TOC and is not wired into the override.

## Decision

1. **OCX sidebar:** nest the current top-level groups one level down under a small number of
   top-level headings in `website/astro.config.mjs`, so the theme's collapsible, deferred nested
   groups apply, and remove the flattening in `route-middleware.ts`. Gate: every page on the
   Lighthouse list stays at or under 900 elements; measure before and after.
2. **Theme TOC (theme PR):** give the desktop list a visible hierarchy in CSS only: h2 rows in
   `--ocx-color-fg` at medium weight, h3 rows muted, the caption distinct from both.
3. **Not decided here:** a collapsible desktop TOC (h3 lists folding under their h2). The owner's
   wording could mean it. It needs script and a design; it is listed as an open question in the
   final report.

## Open question

Whether "collapsible style details toc" means the sidebar's collapsible groups (decision 1) or a
new collapsible right-hand list (decision 3).

Answered 2026-11-16: both. The owner confirmed the desktop outline was meant; the theme now folds it
(`foldOutline`, see the amendment in `adr_docs_getting_started.md`).
