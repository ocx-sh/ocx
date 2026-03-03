---
name: documentation
description: Write OCX documentation (user guide, reference pages, website content). Applies OCX-specific narrative structure, linking conventions, and VitePress component patterns.
allowed-tools: Read, Write, Edit, Glob, Grep, WebSearch, WebFetch
---

# OCX Documentation

All documentation for OCX lives in `website/src/docs/`. The site is built with VitePress.

## Rules

Follow `.claude/rules/documentation.md` — it contains OCX-specific guidelines distilled from review feedback. Key points:

- **Narrative structure**: idea → problem → solution, then depth
- **No marketing tone** — let examples make the case
- **Reference-style links** — never inline `[text](url)`, collect definitions at file bottom
- **Every external tool hyperlinked** — every occurrence, not just the first
- **Analogies in `:::info` callout boxes**, not inline prose
- **Real-world examples** — always search the internet before writing comparisons
- **Custom anchors** on every heading: `{#parent-subsection}`

## Product Context

Read `.claude/rules/product-context.md` for positioning, differentiators, and competitive landscape before writing any user-facing documentation.

## Available Vue Components

- `<Tree>` / `<Node>` / `<Description>` — filesystem trees with hover descriptions
- `<Tooltip term="label">text</Tooltip>` — inline term with hover popup
- `<Steps>` / `<Step>` / `<Description>` — numbered step lists
- `::: code-group` — tabbed code blocks for side-by-side comparisons

## Before Writing

1. Read the relevant source code — do not document from memory
2. Search the internet for real examples from other ecosystems
3. Identify the problem the feature solves before writing the solution
4. Verify internal links point to sections that exist and have content
5. Check that every external tool or project mentioned has a hyperlink
