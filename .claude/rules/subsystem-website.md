---
paths:
  - website/**
---

# Website Subsystem

Astro 7 + Starlight 0.42 docs site at `website/`, served under `https://ocx.sh/docs/`. Theme `@ocx-sh/theme` 0.1.1 (pinned exact), pnpm 11 via ocx (`ocx exec pnpm node -- pnpm …`; plain `pnpm` on PATH is v10). Auto-gen content from the build tasks.

## Design Rationale

Starlight + the shared `@ocx-sh/theme` — one theme across the ocx.sh sites; interactive docs (trees, tooltips, tabs) come from theme components, not local framework code. Generated pipeline (schema, recordings, SBOM) = single source of truth. Docs derived from code, not hand-maintained.

## File Structure

| Path | Purpose |
|------|---------|
| `astro.config.mjs` | Astro/Starlight config (base `/docs/`, sidebar, remark plugins, theme plugin) |
| `plugins/*.mjs` | Remark plugins: `includes` (`@/` = `src/`), `heading-ids`, `md-links`; tests beside them (`node --test plugins/`) |
| `plugins/redirects.mjs`, `redirects.d/<owner>.json` | Old-URL redirect map, one file per owner |
| `tests/` | Node tests (`node --test`): redirect fixtures, cluster set, `old-urls.test.mjs` (every URL in `old-urls.txt` still resolves over a built dist) |
| `old-urls.txt` | Every URL (and `page#anchor`) the previous site served, one per line |
| `src/components/Terminal.astro` | Local asciinema terminal component (the theme has none) |
| `src/content/docs/**/*.{md,mdx}` | Documentation pages — the content root |
| `src/content/docs/reference/*.md` | Reference pages (command-line, environment, metadata, ...) |
| `src/content/docs/reference/dependencies.md` | **Generated, gitignored** — SBOM dependency table |
| `src/content/docs/reference/{command-line/**,environment.md,configuration.md,project-file.md,exit-codes.md}` | **Generated and committed**, never hand-edited — `task website:reference:generate` rewrites them from `ocx_schema`; `task website:reference:check` fails on drift |
| `src/_scripts/` | **Generated** — doc-script snippets the pages include |
| `src/public/{casts,data,schemas}` | **Generated, gitignored** — asciinema recordings, SBOM data, the published JSON schemas |
| `src/public/schemas/{metadata,config,project,project-lock,patch,reports,execution-record,errors}/v*.json` | The eight schemas from the `ocx_schema` crate (`patch` = `ocx patch publish --descriptor` format; `reports` and `errors` = the `--format json` output contract) |
| `taskfile.yml` | Website tasks (install, serve, build, test, check, lint:links, check:budget, reference) |
| `BUILD.bazel`, `site.bzl` | Coarse Bazel rule `//website:site` (`task website:bazel:build`) |

## Reference Page Shape

A `doc_type: reference` page is an optional intro of at most two sentences, then entries. An entry is a
heading with an explicit `{#kebab-id}` followed only by a description of at most two sentences, fact
tables or `key: value` lists (type, default, env, flag), at most one fenced block and at most one
`See [x](...)` link line. No asides, rationale, conventions or non-entry sub-sections. Gates:
`test/lint/test_reference_shape.py` (shape) and `test/lint/test_docs_quality_gate.py` (declaration,
page type, reference shape: any finding fails, no exemption list). Wording
rules → [docs-style.md](./docs-style.md) § Reference Pages.

## Task Commands

```bash
task website:serve              # Dev server (astro dev)
task website:build              # Full build -> website/dist/{docs,schemas}
task website:lint:links         # lychee over dist (offline); run after website:build
task website:check:budget       # per-page gzip / DOM budget over dist/docs
task website:test               # node tests (plugins/, scripts/); part of task verify
task website:check              # link check + old-URL tests over a built dist; deploy-docs runs it before upload
task website:reference:generate # rewrite the five generated reference pages from ocx_schema
task website:reference:check    # fail when a committed generated reference page drifts
```

Build chain: `schema:default` -> `scripts:publish` -> `recordings:parallel` -> `sbom:generate:page` -> `astro build` (`outDir` `dist/docs`), then `dist/docs/schemas` moves to `dist/schemas`.

Site deploys ride CI: `.github/workflows/deploy-docs.yml` builds `task website:build` and publishes `website/dist` to Bunny storage on push to main.

**Never edit generated files** — build pipeline overwrites.

## Components and Markup

Pages are `.md` or `.mdx`; a page using a component is `.mdx` and imports it.

| Component | Import |
|-----------|--------|
| `Tabs`, `TabItem`, `Tooltip`, `Tree`, `PlatformIcons` | `@ocx-sh/theme/components/<Name>.astro` |
| `Node` (tree node), `Description` (tree annotation) | `@ocx-sh/theme/components/TreeNode.astro`, `TreeDescription.astro` |
| `Terminal` | `website/src/components/Terminal.astro` (relative import) |
| `Steps` | `@astrojs/starlight/components` |

Props live in the theme package; read the component source rather than copying signatures here. `<Terminal src="/casts/x.cast" title="..." collapsed />` plays a recording (src is base-relative; the component adds `/docs/`); inline frames are not supported.

Markdown: callouts are `:::note|tip|caution|danger`; custom anchors `## Heading {#custom-id}`; fenced-block titles `title="..."`. Frontmatter is Starlight's (`title`, `description`, `sidebar`, `template`).

## Styling

- Use Starlight/theme CSS custom properties (`--sl-color-*`, `--sl-font-mono`); never hardcode colors — light/dark switches through the variables.
- Every element that pops, expands, collapses or moves animates entry and exit.

## Icons (Licensed Assets)

Icons8 outline icons live under the gitignored `website/src/public/licensed/` — the license forbids repo redistribution; never commit them or build them into the site. Tint with CSS `filter` chains or `mask-image` + a CSS variable, never a hardcoded color.

## Writing Guidelines

See `.claude/rules/docs-style.md` for narrative structure, link conventions, callout usage, tooltip patterns, precision rules.

## Generated Content Pipeline

| Content | Generator | Task |
|---------|-----------|------|
| JSON schema | `ocx_schema` crate | `task schema:default` |
| Reference pages (CLI, env, config, project file, exit codes) | `ocx_schema` crate (`//crates/ocx_schema:docs`) | `task website:reference:generate` |
| Doc-script snippets | `website/scripts/publish_doc_scripts.py` | `task website:scripts:publish` |
| Terminal recordings | pytest + registry:2 | `task recordings:parallel` |
| SBOM / dependencies | cargo-cyclonedx + Python | `task sbom:generate:page` |

## Quality Gate

Per task / review-fix iteration for website-only changes: `task website:build` (validates generated content, schema, Astro output), then `task website:lint:links`, not full `task verify`. Full `task verify` runs once, at finalize; until then `task verify:mark` is always allowed — pick the level the change needs (`workflow-git.md` § Verification Levels).
