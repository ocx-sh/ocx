---
name: docs
description: Use when authoring or editing OCX documentation — user guide, reference pages, website content under `website/src/docs/`, VitePress pages, or doc narrative structure.
---

# OCX Documentation

Role: write user-facing documentation for the OCX website (`website/src/docs/`, VitePress).

## Workflow

1. **Read source code** — do not document from memory
2. **Read product context** — `.claude/rules/product-context.md` before any user-facing writing
3. **Search real-world examples** from other ecosystems before writing comparisons
4. **Identify the problem** the feature solves before writing the solution
5. **Draft narratively** — idea → problem → solution → depth
6. **Verify internal links** point to sections that exist and have content
7. **Build locally** — `task website:serve` and visually check rendering

## Narrative Standards

- **Idea → problem → solution, then depth** — never start with "OCX is a..."
- **No marketing tone** — let examples make the case
- **Reference-style links only** — never inline `[text](url)`; collect definitions at file bottom
- **Every external tool hyperlinked** — every occurrence, not just the first
- **Analogies in `:::info` callout boxes**, not inline prose
- **Custom anchors on every heading** — `{#parent-subsection}`

## Relevant Rules (load explicitly for planning)

- `.claude/rules/docs-style.md` — full OCX narrative + linking + anchor conventions
- `.claude/rules/subsystem-website.md` — VitePress config, Vue component catalog (`<Tooltip>`, `<Tree>`, `<Steps>`, `<Terminal>`, `<Frame>`, `<Description>`, `<CopySnippet>`, `<PackageCatalog>`, etc.), styling patterns, markdown extensions, frontmatter conventions, generated content pipeline (auto-loads on `website/**`)
- `.claude/rules/product-context.md` — positioning, differentiators, competitive landscape
- `.claude/rules/quality-vite.md` — build tool conventions (if touching config)

## Tool Preferences

- **WebFetch / WebSearch** — real-world examples from other package managers before writing comparisons
- **`task website:serve`** — VitePress dev server (localhost:5173) for live preview
- **`task website:build`** — full build (schema → recordings → SBOM → catalog → VitePress)

## Constraints

- NEVER edit generated content (catalog pages, `dependencies.md`, `.cast` files, schema JSON) — they are overwritten by the build pipeline
- NEVER inline links — reference-style only
- NEVER hardcode colors — use VitePress CSS variables (see `subsystem-website.md` styling patterns)
- ALWAYS read source before documenting; never from memory

## Handoff

- To Architect — for docs that reveal design ambiguity
- To Builder — for code changes uncovered while writing docs

$ARGUMENTS
