---
name: docs
description: Use when authoring or editing OCX documentation — user guide, reference pages, website content under `website/src/content/docs/`, Starlight pages, or doc narrative structure.
---

# OCX Documentation

Role: write user-facing docs for OCX website (`website/src/content/docs/`, Astro + Starlight).

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
- **Analogies in `:::note` callout boxes**, not inline
- **Commands in `<ShellSession>` frames**, one term per concept — `docs-style.md` § Terminal Commands, § Terms
- **Custom anchors on every heading** — `{#parent-subsection}`

## Relevant Rules (load explicitly for planning)

- `.claude/rules/docs-style.md` — OCX narrative + linking + anchor conventions
- `.claude/rules/subsystem-website.md` — Astro/Starlight layout, theme component imports (`<Tooltip>`, `<Tree>`, `<Tabs>`, `<Steps>`, `<Terminal>`, `<ShellSession>`, `<InstallCommand>`, etc.), styling, markdown extensions, generated content pipeline (auto-loads on `website/**`)
- `.claude/rules/product-context.md` — positioning, differentiators, competitive landscape
- `.claude/rules/quality-vite.md` — build tool conventions (if touching config)

## References

- `references/terminal-commands.md` — `ShellSession` mechanics: output fold, numbered badges and legend, imports, budget pages
- `references/vocabulary.md` — the decisions table (one term per concept), undecided items, glossary status
- `references/mdx-rename.md` — the `.md` to `.mdx` recipe
- `website/**` entry path, sidebar and page weight — `subsystem-website.md`

## Verification Facts

- Word-count lints (DOC-TYPE-07, DOC-TYPE-10) count an MDX `import` line as prose: put the import below the first `##` when one trips.
- The landing lint (`landing_check.py`) wants a literal fence, not a component, as the first command block of a landing page.
- `task website:check` fails on a page with no row in the inventory (`.claude/artifacts/docs_ia.md` section 5).
- NC1 (`test/lint/test_doc_binding.py`) scans `sh` and `bash` fences only: an `ocx` command on a walkthrough page is a `<<<` include of a tested doc script.
- Page weight: `task website:check:budget` counts start tags and gzip bytes per built page; budget → `subsystem-website.md` § Sidebar and Page Weight.

## Tool Preferences

- **WebFetch / WebSearch** — real-world examples from other package managers before comparisons
- **`task website:serve`** — Astro dev server for live preview
- **`task website:build`** — full build (schema → doc scripts → recordings → SBOM → Astro) into `website/dist/{docs,schemas}`

## Constraints

- NEVER edit generated content (`dependencies.md`, `.cast` files, schema JSON) — build pipeline overwrites
- NEVER inline links — reference-style only
- NEVER hardcode colors — use the Starlight/theme CSS variables (see `subsystem-website.md` styling)
- ALWAYS read source before documenting; never memory

## Handoff

- To Architect — docs revealing design ambiguity
- To Builder — code changes uncovered while writing docs

$ARGUMENTS