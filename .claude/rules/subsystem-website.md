---
paths:
  - website/**
---

# Website Subsystem

VitePress 2.0 documentation site at `website/`. Bun runtime, 15 custom Vue components, auto-generated content from CI.

## Design Rationale

VitePress (not Docusaurus/Astro) because it leverages the Vue ecosystem, provides excellent code block support, and generates a fast static site. Custom Vue components enable interactive documentation (file trees, tooltips, tabs) without heavy JS frameworks. Generated content pipeline (schema, recordings, SBOM, catalog) ensures single source of truth — docs are derived from code, not maintained separately.

## File Structure

| Path | Purpose |
|------|---------|
| `.vitepress/config.mts` | VitePress config (nav, sidebar, plugins, head) |
| `.vitepress/theme/index.mts` | Theme extension — registers all custom components globally |
| `.vitepress/theme/components/*.vue` | 15 custom Vue components (see below) |
| `src/index.md` | Homepage (`layout: home` with hero + features) |
| `src/docs/*.md` | Documentation pages |
| `src/docs/reference/*.md` | Reference pages (command-line, environment, metadata) |
| `src/docs/catalog/*.md` | **Generated** — per-package pages |
| `src/docs/reference/dependencies.md` | **Generated** — SBOM dependency list |
| `src/public/casts/*.cast` | **Generated** — asciinema recordings |
| `src/public/schemas/metadata/v1.json` | **Generated** — JSON schema from `ocx_schema` crate |
| `src/public/data/catalog/` | **Generated** — catalog JSON + per-package info/logos |
| `src/public/data/dependencies.json` | **Generated** — SBOM data |
| `taskfile.yml` | Website tasks (install, serve, build, deploy) |

## Task Commands

```bash
task website:serve     # Dev server (localhost:5173)
task website:build     # Full build (generates schema, recordings, sbom, catalog, then VitePress)
task website:deploy    # rsync to production (requires DEPLOY_HOST)
```

The build task chains: `schema:generate` → `recordings:build` → `sbom:generate:page` → `catalog:generate` → `vitepress build`.

**Never edit generated files** — they are overwritten by the build pipeline.

## VitePress Configuration

- **Source dir**: `src/`
- **Plugin**: `vitepress-plugin-group-icons` (file icons in code blocks, custom shell/powershell mappings)
- **Search**: local provider
- **Sidebar**: flat list with collapsed "Reference" group
- **Social links**: GitHub + Discord

## Custom Vue Components

All globally registered in `theme/index.mts` — use directly in `.md` files without imports.

### Content Components (use in documentation)

| Component | Props | Slots | Purpose |
|-----------|-------|-------|---------|
| `<Tooltip>` | `term: string`, `side?: 'top'\|'bottom'\|'left'\|'right'` (default: top), `delayDuration?: number` (default: 400) | default = popup content | Inline term with hover popup |
| `<Tree>` | *(none — introspects `<Node>` children)* | default = `<Node>` elements | Declarative file tree |
| `<Node>` | `name: string`, `icon?: string`, `openIcon?: string`, `open?: boolean` | default = child `<Node>`s + `<Description>` | Tree node with icon |
| `<Steps>` | *(none — introspects `<Step>` children)* | default = `<Step>` elements | Vertical stepper with detail panels |
| `<Step>` | `title: string`, `description?: string`, `status?: 'complete'\|'current'\|'upcoming'` | default = detail content + `<Description>` | Step with click-to-reveal panel |
| `<Description>` | *(none — marker)* | default = description text | Marker for parent VNode introspection |
| `<Terminal>` | `src?: string`, `title?: string`, `cols?: number`, `rows?: number`, `autoPlay?: boolean`, `speed?: number` (1), `idleTimeLimit?: number` (2), `loop?: boolean`, `fit?: string` ('width'), `collapsed?: boolean` | default = `<Frame>` elements (if no `src`) | Asciinema player with macOS chrome |
| `<Frame>` | `at: string \| number` (required) | default = terminal line text | Single frame in inline Terminal |
| `<CopySnippet>` | `code: string`, `label?: string` | *(none)* | Copy-to-clipboard button |

### Data Components (render generated content)

| Component | Data Source | Purpose |
|-----------|-----------|---------|
| `<PackageCatalog>` | `/data/catalog/catalog.json` | Searchable package grid with platform badges |
| `<PackageDetail>` | `/data/catalog/packages/{name}/info.json` | Individual package view with tag selector |
| `<DependencyExplorer>` | `/data/dependencies.json` | SBOM viewer with license breakdown |

### VNode Introspection Pattern

`Tree`, `Steps`, and `Terminal` (without `src`) use a **declarative marker pattern**: they don't render child slots directly. Instead, they introspect child VNodes (`<Node>`, `<Step>`, `<Frame>`) to extract props and build internal data structures. `<Description>` is a marker component whose content is read by parent introspection but never rendered by itself.

### Usage Examples

```vue
<!-- Tooltip -->
<Tooltip term="content-addressed">stored by digest, not name</Tooltip>

<!-- File tree -->
<Tree>
  <Node name="~/.ocx/" icon="🏠" open>
    <Node name="objects/" icon="📦">
      <Description>immutable content-addressed store</Description>
    </Node>
    <Node name="index/" icon="📋" />
    <Node name="installs/" icon="🔗" />
  </Node>
</Tree>

<!-- Steps -->
<Steps>
  <Step title="Install" status="complete">
    <Description>Download and extract</Description>
    Detail content shown on click...
  </Step>
  <Step title="Configure" status="current">
    <Description>Set up PATH</Description>
  </Step>
</Steps>

<!-- Terminal from .cast file -->
<Terminal src="/casts/install.cast" title="Installing a package" collapsed />

<!-- Terminal with inline frames -->
<Terminal title="Quick demo">
  <Frame at="0">$ ocx install cmake:3.28</Frame>
  <Frame at="1.5">Installed cmake:3.28 → ~/.ocx/objects/...</Frame>
</Terminal>
```

## Styling Patterns

- **CSS variables**: Use VitePress theme vars (`--vp-c-text-1`, `--vp-c-brand`, `--vp-c-divider`, `--vp-c-bg`, `--vp-c-bg-soft`, `--vp-font-family-mono`)
- **Dark mode**: Handled automatically by VitePress variable switching — never hardcode colors
- **Scoped styles**: Use `<style scoped>` in components (except Tooltip which uses portal)
- **Terminal colors**: `--term-color-0` through `--term-color-15` map ANSI colors to VitePress semantic colors
- **Responsive**: Components handle mobile via CSS media queries
- **Transitions**: Every element that pops, expands, collapses, or moves must animate both entry and exit. Use CSS animations or transitions with matching open/closed states — never show without a fade-in or hide without a fade-out

## VitePress Markdown Features

| Feature | Syntax |
|---------|--------|
| Callout boxes | `::: info\|tip\|warning\|details` |
| Code groups | `::: code-group` with labeled code blocks |
| Custom anchors | `## Heading {#custom-id}` |
| Frontmatter | `outline: deep`, `layout: home\|doc` |

## Frontmatter Conventions

- **Homepage**: `layout: home` + `hero` (name, text, tagline, actions) + `features` array
- **Content pages**: `outline: deep` for full heading outline
- **Catalog pages**: `title`, `description`, `head` meta, `prev`/`next` navigation (generated)

## Icons (Licensed Assets)

All icons are **Icons8 outline/line style**, black SVG on transparent background, licensed for OCX use.

### Directory Structure

| Path | Purpose |
|------|---------|
| `src/public/licensed/source/icons/` | Original downloads (named as downloaded, e.g. `layers.svg`) |
| `src/public/licensed/icons/` | Production copies (named by purpose, e.g. `roadmap-composable.svg`) |
| `src/public/licensed/source/icons/CATALOG.md` | Full catalog with descriptions and usage mapping |

### Workflow for New Icons

1. Download outline-style black SVG from Icons8
2. Place original in `licensed/source/icons/{name}.svg`
3. Copy to `licensed/icons/{context}-{purpose}.svg` (e.g. `cta-roadmap.svg`, `roadmap-interop.svg`, `feature-oci.svg`)
4. Update `CATALOG.md` — add to the appropriate category table and "Currently In Use" section
5. Tint with CSS filters (see below)

### Naming Convention

| Prefix | Used for | Example |
|--------|----------|---------|
| `feature-` | Index page feature cards | `feature-oci.svg` |
| `cta-` | CTA section cards | `cta-discord.svg` |
| `roadmap-` | Roadmap timeline nodes | `roadmap-composable.svg` |

### Icon Tinting

Icons are tinted using CSS `filter` chains. Never hardcode colors — always derive from VitePress CSS variables or use the filter technique for consistent light/dark mode.

**For `<img>` tags** (CTA cards, feature cards): CSS filter chains per icon class.

```css
/* Light mode */
.cta-icon-discord {
  filter: invert(27%) sepia(51%) saturate(3264%) hue-rotate(253deg) brightness(88%) contrast(93%);
}
/* Dark mode */
.dark .cta-icon-discord {
  filter: invert(76%) sepia(30%) saturate(1148%) hue-rotate(230deg) brightness(103%) contrast(97%);
}
```

**For dynamic accent colors** (roadmap nodes): CSS `mask-image` with `background-color` set to a CSS variable.

```vue
<span
  class="icon"
  :style="{
    maskImage: `url(${icon})`,
    WebkitMaskImage: `url(${icon})`,
    backgroundColor: 'var(--accent)',
  }"
/>
```

```css
.icon {
  display: inline-block;
  width: 28px;
  height: 28px;
  mask-size: contain;
  mask-repeat: no-repeat;
  mask-position: center;
}
```

This renders the icon shape filled with the accent color — works in both light and dark mode automatically.

## Writing Guidelines

See `.claude/rules/docs-style.md` for narrative structure, link conventions, callout usage, tooltip patterns, and precision rules.

## Generated Content Pipeline

| Content | Generator | Task |
|---------|-----------|------|
| JSON schema | `ocx_schema` crate | `task schema:generate` |
| Terminal recordings | pytest + registry:2 | `task recordings:build` |
| SBOM / dependencies | cargo-cyclonedx + Python | `task sbom:generate:page` |
| Package catalog | Python + ocx.sh registry | `task catalog:generate` |

## Quality Gate

During review-fix loops, run `task website:build` — not full `task verify`.
Full website build pipeline validates generated content, schema, and VitePress output.
