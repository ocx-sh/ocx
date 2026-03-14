# Plan: Website Package Catalog

## Overview

**Status:** Draft
**Date:** 2026-03-14

## Objective

Build a package catalog into the ocx.sh VitePress website. At build time, invoke the OCX CLI against the `ocx.sh` registry to gather all package data (metadata, logos, READMEs, tags, platforms) and generate static pages: a searchable listing and per-package detail pages.

## Scope

### In Scope

- Build-time data generation script (Python)
- Catalog listing page with search/filter
- Per-package detail pages via VitePress dynamic routes
- Taskfile integration
- Nav/sidebar updates

### Out of Scope

- Client-side registry calls (everything is static)
- Package publishing from the website
- Analytics or download counts
- CI integration (local/manual build for now)

## Key Decisions

| Decision | Rationale |
|----------|-----------|
| Python script for data gathering | Matches existing `sbom-to-markdown.py` pattern; already have `uv` in the stack |
| Keep READMEs as `.md` files | VitePress handles markdown natively; no need to pre-render to HTML |
| Per-package directories (`packages/{name}/`) | Colocates info.json, logo, and readme; cleaner than flat siblings |
| VitePress dynamic routes (`[pkg].paths.ts`) | One route per package without manual page creation |
| Data in gitignored `website/src/public/data/catalog/` | Parent `src/public/data` is already gitignored; regenerated on build |
| Separate `catalog:generate` task | Optional for local dev; required before website build in CI |
| No new component libraries | reka-ui (tabs, tooltips) + @vueuse/core (clipboard, URL params) already installed; VitePress CSS vars for all styling |
| Copy-to-clipboard install snippets | Each package card and detail page shows one-click copyable `ocx install` / `ocx exec` commands |

## Component Strategy

**All required libraries are already installed.** No new dependencies needed.

| Installed Package | Use For |
|---|---|
| `reka-ui` | Tabs on detail page (tag list / platform matrix / README sections). SSR-safe, already proven by `Tooltip.vue` |
| `@vueuse/core` | `useClipboard()` for copy-to-clipboard on install snippets; `useUrlSearchParams('history')` for shareable filter URLs (optional) |
| VitePress CSS vars | All styling — `--vp-c-bg-soft` (cards), `--vp-c-divider` (borders), `--vp-c-brand` (accents), `--vp-c-text-*` (typography) |

**Not adding:** PrimeVue, shadcn-vue, Tailwind — all have known SSR/VitePress friction and aren't needed.

**Optional:** `fuse.js` (~12 KB, zero deps) for fuzzy search. Substring `.includes()` works fine at catalog scale (~10-30 packages), so defer unless needed.

## Data Pipeline

### CLI Commands Used

```bash
# 1. Get all repos + tags
ocx index catalog --with-tags --format json

# 2. Per-package metadata, logo, README
ocx package info <repo> --save-readme <dir> --save-logo <dir> --format json

# 3. Per-package platform matrix
ocx index list <repo> --with-platforms --format json
```

### Output Structure

```
website/src/public/data/catalog/
  catalog.json              # Master index (array of package summaries)
  packages/
    {name}/
      info.json             # Package detail (tags, platforms, metadata)
      logo.{svg,png}        # Package logo (if available)
      readme.md             # Package README (if available)
```

### `catalog.json` Shape

```json
{
  "generated": "2026-03-14T12:00:00Z",
  "packages": [
    {
      "name": "cmake",
      "title": "CMake",
      "description": "Cross-platform build system generator",
      "keywords": ["cmake", "build"],
      "hasLogo": true,
      "logoExt": "svg",
      "tagCount": 12,
      "platforms": ["linux/amd64", "linux/arm64", "darwin/amd64", "darwin/arm64"],
      "latestTag": "3.29"
    }
  ]
}
```

All per-package assets live under `packages/{name}/`. Paths are derived from the name:
- Logo: `/data/catalog/packages/{name}/logo.{ext}`
- README: `/data/catalog/packages/{name}/readme.md`
- Detail: `/data/catalog/packages/{name}/info.json`

### Per-package `info.json` Shape

```json
{
  "name": "cmake",
  "title": "CMake",
  "description": "Cross-platform build system generator",
  "keywords": ["cmake", "build"],
  "hasLogo": true,
  "logoExt": "svg",
  "hasReadme": true,
  "tags": ["3.29", "3.28", "latest"],
  "platforms": {
    "linux/amd64": ["3.29", "3.28"],
    "darwin/arm64": ["3.29"]
  }
}
```

## Implementation Steps

### Phase 1: Data Generation Script

- [ ] **Step 1.1:** Create `scripts/catalog-generate.py`
  - Invokes the three CLI commands listed above
  - Aggregates into `catalog.json` + per-package JSON
  - Saves logos and READMEs into per-package directories
  - Args: `--ocx-binary`, `--output-dir`, `--registry`, `--package <name>` (selective refresh)
  - `--package` can be repeated; when specified, only fetches info for those packages (skips full catalog refresh, reuses existing `catalog.json` for the repo list)
  - Error handling: warn + skip on per-package failures, fail only if catalog command fails

- [ ] **Step 1.2:** Create `taskfiles/catalog.taskfile.yml`
  - `generate` task calling the Python script
  - Wire into root `taskfile.yml` includes

### Phase 2: Catalog Listing Page

- [ ] **Step 2.1:** Create `website/.vitepress/theme/components/CopySnippet.vue`
  - Reusable inline code snippet with a copy-to-clipboard button
  - Uses `useClipboard()` from `@vueuse/core`
  - Shows a brief "Copied!" confirmation (1.5s timeout, then revert to copy icon)
  - Props: `code` (string to copy/display), `label` (optional prefix like `$`)
  - Styled as a monospace inline block with `--vp-c-bg-soft` background
  - Used on both catalog cards and detail pages

- [ ] **Step 2.2:** Create `website/.vitepress/theme/components/PackageCatalog.vue`
  - Fetches `/data/catalog/catalog.json` on mount
  - Searchable/filterable grid of package cards
  - Each card: logo, title/name, description, platform icons, tag count
  - Each card includes a `<CopySnippet code="ocx install {name}" />` for one-click install
  - Links to `/docs/catalog/{name}` detail pages
  - Loading/error/empty states
  - Follow `DependencyExplorer.vue` patterns (CSS vars, responsive grid)

- [ ] **Step 2.3:** Create `website/src/docs/catalog.md`
  - Minimal markdown wrapper with `<PackageCatalog />` component

### Phase 3: Package Detail Pages

- [ ] **Step 3.1:** Create `website/src/docs/catalog/[pkg].paths.ts`
  - Reads `catalog.json`, generates one route per package
  - For each package, reads `packages/{name}/readme.md` (if it exists) and passes it via `params.content` so VitePress renders it at build time
  - Falls back to empty content if no README available

- [ ] **Step 3.2:** Create `website/src/docs/catalog/[pkg].md`
  - Dynamic route template with `<PackageDetail />` component
  - Uses `{{ $params.content }}` (or frontmatter content injection) to include the README below the component

- [ ] **Step 3.3:** Create `website/.vitepress/theme/components/PackageDetail.vue`
  - Fetches `/data/catalog/packages/{pkg}/info.json`
  - **Header banner:** logo (if available) + package title + description + keywords as tags
  - **Install snippets section:** copyable commands for common workflows:
    - `ocx install {name}:{tag}` — install latest
    - `ocx install {name}:{tag} --select` — install and set as current
    - `ocx exec {name}:{tag} -- {name} --version` — one-shot execution
  - Each snippet uses `<CopySnippet />` for one-click copy
  - **Tabs** (reka-ui `TabsRoot`/`TabsList`/`TabsTrigger`/`TabsContent`):
    - **Versions** tab: tag list with per-tag platform badges
    - **Platforms** tab: platform matrix showing which tags are available per platform
  - **README section:** rendered below the tabs as part of the page content
    - `[pkg].paths.ts` reads each package's `readme.md` and injects it via `params.content`, so VitePress renders it at build time — no client-side markdown parser needed
  - Graceful fallbacks: placeholder icon if no logo, "No README available" message, empty state for no tags

### Phase 4: Integration

- [ ] **Step 4.1:** Register components (`CopySnippet`, `PackageCatalog`, `PackageDetail`) in `website/.vitepress/theme/index.mts`
- [ ] **Step 4.2:** Add "Catalog" to nav and sidebar in `website/.vitepress/config.mts`
- [ ] **Step 4.3:** Add `catalog:generate` before VitePress build in `website/taskfile.yml`
- [ ] **Step 4.4:** Graceful degradation when catalog data is missing (local dev without registry)

### Phase 5: Testing & Polish

- [ ] **Step 5.1:** Manual testing — run script, build site, verify listing + detail pages
- [ ] **Step 5.2:** Test with missing data (no logo, no README, no description)
- [ ] **Step 5.3:** Responsive layout testing (mobile/tablet/desktop)

## Files to Modify

| File | Action | Description |
|------|--------|-------------|
| `scripts/catalog-generate.py` | Create | Data generation script |
| `taskfiles/catalog.taskfile.yml` | Create | Taskfile for catalog generation |
| `website/.vitepress/theme/components/CopySnippet.vue` | Create | Reusable copy-to-clipboard code snippet |
| `website/.vitepress/theme/components/PackageCatalog.vue` | Create | Catalog listing component |
| `website/.vitepress/theme/components/PackageDetail.vue` | Create | Package detail component |
| `website/src/docs/catalog.md` | Create | Catalog listing page |
| `website/src/docs/catalog/[pkg].md` | Create | Dynamic detail page template |
| `website/src/docs/catalog/[pkg].paths.ts` | Create | Dynamic route loader |
| `taskfile.yml` | Modify | Add catalog taskfile include |
| `website/taskfile.yml` | Modify | Add catalog:generate to build chain |
| `website/.vitepress/config.mts` | Modify | Add Catalog to nav/sidebar |
| `website/.vitepress/theme/index.mts` | Modify | Register new components |

## Key Reference Files

These existing files inform the implementation patterns:

| File | Why |
|------|-----|
| `crates/ocx_cli/src/command/package_info.rs` | Package info command — JSON output shape, `--save-logo`, `--save-readme` flags |
| `crates/ocx_cli/src/command/package_describe.rs` | Package describe command — metadata output |
| `crates/ocx_cli/src/command/index_catalog.rs` | Catalog command — repo listing logic |
| `crates/ocx_cli/src/command/index_list.rs` | Index list command — tags + platforms |
| `crates/ocx_cli/src/api/data/catalog.rs` | JSON shape for `index catalog` output |
| `crates/ocx_cli/src/api/data/tag.rs` | JSON shape for `index list` output |
| `website/.vitepress/theme/components/DependencyExplorer.vue` | Reference Vue component pattern (fetch JSON, loading states, CSS vars) |
| `website/.vitepress/theme/components/Tooltip.vue` | Reference reka-ui import/usage pattern |
| `taskfiles/sbom.taskfile.yml` | Reference Taskfile pattern for build-time data generation |
| `website/.vitepress/config.mts` | VitePress config (nav, sidebar, plugins) |
| `website/.vitepress/theme/index.mts` | Global component registration |
| `website/taskfile.yml` | Website build task chain |

## Risks

| Risk | Mitigation |
|------|------------|
| Network required at build time | Make catalog:generate optional; components handle missing data gracefully |
| Registry rate limiting with many packages | Add asyncio parallelism in script; small catalog (~10-30 pkgs) is fine initially |
| Package READMEs with relative links/images | Rewrite to absolute URLs in the generation script (if source repo URL available from annotations) |
| README rendering | Handled at VitePress build time via `params.content` in dynamic routes — no client-side parser needed |
| `[pkg].paths.ts` needs data before VitePress build | Task ordering: catalog:generate runs before vitepress build |
| Logo format variability (SVG, PNG, none) | `<img>` tag with fallback placeholder |

## Notes

- `website/src/public/data` is already in `website/.gitignore`, so all generated catalog data is excluded from version control.
- The script should use `uv run` for execution, matching the Python tooling golden path.
- Use `--package <name>` for iterative development: regenerate a single package's metadata/logo/readme without re-fetching the entire catalog.
- Consider caching: if `catalog.json` exists and is recent, skip regeneration (add `--force` flag to override).
