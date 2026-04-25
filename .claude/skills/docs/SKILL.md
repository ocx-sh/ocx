---
name: docs
description: Use when authoring or editing OCX documentation — user guide, reference pages, website content under `website/src/docs/`, VitePress pages, or doc narrative structure.
---

# OCX Documentation

Role: write user-facing docs for OCX website (`website/src/docs/`, VitePress).

## Workflow

1. **Read source code** — no memory docs
2. **Read product context** — `.claude/rules/product-context.md` before user-facing writing
3. **Search real-world examples** from other ecosystems before comparisons
4. **Identify problem** feature solves before solution
5. **Draft narratively** — idea → problem → solution → depth
6. **Verify internal links** point to existing sections with content
7. **Build locally** — `task website:serve`, check rendering

## Narrative Standards

- **Idea → problem → solution, then depth** — never start "OCX is a..."
- **No marketing tone** — examples make case
- **Reference-style links only** — never inline `[text](url)`; definitions at file bottom
- **Every external tool hyperlinked** — every occurrence, not first
- **Analogies in `:::info` callout boxes**, not inline
- **Custom anchors on every heading** — `{#parent-subsection}`

## Relevant Rules (load explicitly for planning)

- `.claude/rules/docs-style.md` — OCX narrative + linking + anchor conventions
- `.claude/rules/subsystem-website.md` — VitePress config, Vue component catalog (`<Tooltip>`, `<Tree>`, `<Steps>`, `<Terminal>`, `<Frame>`, `<Description>`, `<CopySnippet>`, `<PackageCatalog>`, etc.), styling, markdown extensions, frontmatter, generated content pipeline (auto-loads on `website/**`)
- `.claude/rules/product-context.md` — positioning, differentiators, competitive landscape
- `.claude/rules/quality-vite.md` — build tool conventions (if touching config)

## Tool Preferences

- **WebFetch / WebSearch** — real-world examples from other package managers before comparisons
- **`task website:serve`** — VitePress dev server (localhost:5173) for live preview
- **`task website:build`** — full build (schema → recordings → SBOM → catalog → VitePress)

## Constraints

- NEVER edit generated content (catalog pages, `dependencies.md`, `.cast` files, schema JSON) — build pipeline overwrites
- NEVER inline links — reference-style only
- NEVER hardcode colors — use VitePress CSS variables (see `subsystem-website.md` styling)
- ALWAYS read source before documenting; never memory

## Handoff

- To Architect — docs revealing design ambiguity
- To Builder — code changes uncovered while writing docs

$ARGUMENTS