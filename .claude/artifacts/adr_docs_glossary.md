# ADR: One shared glossary, linked from the docs by a build-time plugin

## Metadata

**Status:** Accepted (items under "Left to the owner" are open)
**Date:** 2026-11-16
**Deciders:** owner (goal statement), docs-overhaul orchestrator
**Domain Tags:** frontend
**Research artifact:** [`research_docs_glossary.md`](./research_docs_glossary.md), data
[`research_docs_glossary_terms.json`](./research_docs_glossary_terms.json)

## Context

The owner asked for a shared glossary: one meaning per word across the docs, and each usage of a
glossary word shown as a glossary item with a tooltip and a link to its glossary section.

A crawl of the corpus found 152 terms, 46 groups of conflicting usage (407 banned forms on 81
pages, 62 of them in generated reference pages) and 10 wrong statements. The site also has a hard
page-weight gate: 900 DOM elements and 100 in four Lighthouse categories on 60 gated pages.

## Decision

1. **One meaning per word first.** The conflict table (C1–C45) in the research artifact is the
   approved vocabulary. Pages are reworded to it before any term is linked, because a linked
   homonym points at the wrong definition. Generated reference pages are fixed in their Rust doc
   strings.
2. **Mechanism: a remark plugin in `@ocx-sh/theme`**, fed by `website/src/data/glossary.json`. No
   page source changes; `.md`, `.mdx` and generated pages are covered alike.
3. **One element per linked usage:** `<a class="ocx-term" href="/docs/reference/glossary/#id">`.
   The definition is not in the page. A delegated listener loads one shared popover on first hover
   or focus and fills it from `/docs/glossary.json`. Without script the term is a plain link.
   Escape closes the popover; the pointer may move onto it.
4. **Frequency is a plugin option** (`page` | `section` | `all`). OCX ships `all`: every usage of a
   term links (reversed 2026-11-16, see "Why every usage"). A usage in another sense is fixed by
   the full term, an `except` phrase, the row's `excludePages` or a reword, in that order.
5. **The DOM budget is not raised and no page leaves the gated list.** Headroom comes from the
   theme: a lighter site header and a one-element heading anchor. A page still over budget is
   trimmed or split.
6. **Glossary page** `reference/glossary.md` is generated from the data file and drift-checked,
   grouped by category, one `### Term {#id}` entry each. It is off the gated list.
7. **Honesty lint** (`website/tests/glossary.test.mjs`): the data validates, every learn-more
   target resolves, the page is fresh, and no banned synonym (`avoid`) appears in source prose.
8. **Hand-written `<Tooltip>` uses in OCX docs are retired** once the plugin lands. The component
   stays in the theme.

## Why every usage

"Every usage" was the request. The first measurement, with the original page chrome, put 14 of 58
gated pages over the budget (`explanation/versioning` at 1087 elements) and up to 638 tab stops on
one page, so `page` was expected. On 2026-11-16 it was measured again with the trimmed page chrome
(lighter header, one-element heading anchor): only two gated pages were over budget, so `all`
ships. The option in decision 4 keeps the choice a one-line change.

## Consequences

- Authors write plain prose; linking is automatic. A new term is one row in the data file.
- A term used in a second sense is a lint failure or a wrong link, so vocabulary drift is visible.
- The theme carries about 0.3 KB gzip of shared script and 0.1 KB of shared CSS on every page.
- A term split across inline markup is not linked.

## Left to the owner

- **2026-11-16, C46 decided:** the product is `OCX` in prose and `ocx` is only the command, in code font. Evidence: `OCX` was the majority form in prose (about 680 against 250) and the owner-authored project docs use it. The lowercase wordmark stays in the site title, the logo and the README heading. `website/tests/product-name.test.mjs` enforces it.
- **C16, the "Visibility" label** on environment-variable entries collides with entry visibility.
  The proposed label "Audience" changes a generated reference page; untouched.
- **D8, `ocx remove <IDENTIFIER>`** accepts binding names. The fix renames a CLI value name, which
  is a `cli.json` change and needs a contract-ledger row; untouched.
