# Research: Signing dogfood, shared theme and SBOM display

## Metadata
- Date: 2026-11-13. Lane: codebase recon (read-only).
- Commits read: ocx `origin/main` (checkout `goat` at f5e6e66a4, clean); ocx-website 2f6e... tip `ac3359ef` (main, v0.5.0 tag); ocx-catalog `d560f56` (main); index `origin/main` 6c67672b (working tree is 329 commits behind); e2e-index worktree `e8437d5`; www-setup `origin/HEAD` 094649b; ocx-sdk-python `ed4e96a`; find_ocx `c9e496c`; ocx-marketing `374b18a`.
- `web` is not a git checkout and has no package.json; not a consumer.

## 1. Theme source, version, release, pins
- Source: `/home/mherwig/dev/ocx-website/packages/theme` (npm `@ocx-sh/theme`). GitHub repo `ocx-sh/website`, default branch `main`, public (`packages/theme/package.json` repository url; `gh repo view`).
- Version: 0.5.0 (`packages/theme/package.json:3`); npm dist-tag `latest` = 0.5.0 (`npm view`). Tags v0.1.0..v0.5.0 exist; v0.5.0 = `ac3359ef`.
- Release: one stable tag `vX.Y.Z` on `main` triggers `.github/workflows/release.yml` (`on.push.tags v[0-9]+.[0-9]+.[0-9]+`, line 9-10). Jobs: verify (tag == package.json version, tag commit is on main), gate (`check`, `pack`), then npm publish with provenance via trusted publishing (OIDC, no token), grim skills to `ghcr.io/ocx-sh/lore`, GitHub release. Version bump is a reviewed PR; tag is signed (`ocx-website/RELEASING.md:1-50`). Prerelease tags do not trigger it.
- Pins:
  - `ocx/website` origin/main: `"@ocx-sh/theme": "0.5.0"` exact (`website/package.json`, grep on `origin/main`).
  - `ocx-catalog` origin/main: `^0.5.0` in `package.json:58` and `docs/package.json`. Working tree at `ocx-catalog` shows `^0.3.0` in `package.json:58` and `docs/package.json:14` (working tree is stale vs origin/main; inferred, `git show origin/main:package.json` says `^0.5.0`). Catalog package version 0.6.3.
  - `index` origin/main: no direct theme dep; `@ocx-sh/catalog ^0.6.2` only (`package.json`). Working-tree checkout is stale: `@ocx-sh/catalog ^0.5.0`, vitepress 2.0.0-alpha.19, vue 3.5.41, behind origin/main by 329, `bot-tools/pyproject.toml` modified.
  - `/home/mherwig/dev/ocx/.agents/worktrees/e2e-index`: `@ocx-sh/catalog 0.6.3` exact (no theme dep), vitepress/vue as above; no `website/`.
- Theme version in `@ocx-sh/catalog` is transitive; the catalog lists the theme as a dependency and sets `ssr.noExternal ["@ocx-sh/catalog","@ocx-sh/theme"]` (`ocx-catalog/src/site/integration.ts:66`).

## 2. SBOM / dependency / licence components in the theme
- One exists: `DependencyExplorer` (SBOM viewer), added in the first theme commit `c6645d89` 2026-09-30, present in v0.1.0..v0.5.0 (all 10 tags contain it; v0.5.0 file starts "SBOM viewer (WP13.4, port of ocx DependencyExplorer.vue)").
- Files: `packages/theme/src/components/DependencyExplorer.astro`, `dependency-explorer.mjs`, `dependency-explorer.css`, `dependency-explorer.d.mts`. Exposed through the `./components/*.astro` export; no dedicated `./dependency-explorer` export in `package.json` (only `components/*.astro`).
- Props: `src?: string`, default `withBase('/data/dependencies.json', base)` (`DependencyExplorer.astro` Props). Also a DOM event `ocx:deps:load` whose `detail.data` can be the JSON or a promise of it (`dependency-explorer.d.mts`, `mountExplorer`).
- Data shape (custom JSON, not CycloneDX): `{ generated: string, binaries: Record<string, { version, license, target, summary:{total, required, excluded, uniqueLicenses, licenses:Record<string,number>}, components:[{name, version, license, description, author, scope, links:{cratesIo?, docsRs?, repository?, website?}}] }> }` (`dependency-explorer.d.mts`). It renders only `firstBinary(data)` (the first key of `binaries`).
- Behaviour: search over name+description AND an exact licence `<select>`; 3 summary stats (total, licences, generated); per-row expand keyed `name@version` (Zag collapsible); loading/error/empty states; row placeholders reserved at build time only when `src` is a `data:` URL, else capped at 12 (`RESERVE_MAX`). Render time: server renders the shell (summary, controls, skeleton rows); rows are built in the browser by `mountExplorer` via `fetch(data-src)` with a 15 s timeout, DOM `textContent` only.
- Other: showcase pages `examples/starlight/src/content/docs/components/dependency-explorer.mdx` and stories `.../stories/dependency-explorer/{default,states}.mdx`; tests `packages/theme/test/components-dependency-explorer.test.ts`, `tests/e2e/components-dependency-explorer*.spec.ts`. No licence-list, dependency-graph, or CycloneDX-reading component beyond this (see negative).
- Generic building blocks that exist: `KeyValueTable`, `Tree`/`TreeView`/`TreeNode`, `Collapsible`, `Accordion`, `Select`, `SearchField`, `Tag`, `Dialog`, `Tooltip`, `ui/DataTable` (referenced in `components-data-table.test.ts`).

## 3. OCX docs website SBOM page
- Page: `/reference/dependencies/` (sidebar entry `website/astro.config.mjs:180` on origin/main). The page file `website/src/content/docs/reference/dependencies.md` is generated and gitignored (`website/.gitignore:19-20`, root `.gitignore:90`), so it is not in the tree.
- It does not use `DependencyExplorer`. grep of `website/src` on origin/main finds no `DependencyExplorer` and no `dependencies.json`. The page is plain Markdown: one table row per licence with columns License, Count, and a comma list of `name version` (`scripts/sbom-to-markdown.py:generate_markdown`; docstring says a per-component row would exceed the DOM budget `website/scripts/check_budget.py`).
- Data pipeline (`website/sbom.taskfile.yml`): `cargo install --locked cargo-cyclonedx` then `cargo cyclonedx --format json --manifest-path crates/ocx_cli/Cargo.toml` writes `crates/ocx_cli/ocx.cdx.json` (gitignored `*.cdx.json`), then `uv run scripts/sbom-to-markdown.py --input ... --output website/src/content/docs/reference/dependencies.md`. Hooked into `website/taskfile.yml:115` (`build` runs `sbom:generate:page` before `astro build`).
- Granularity: one binary (`ocx_cli` crate manifest, its resolved Cargo dependency closure as cargo-cyclonedx emits it on the host target; inferred host-only, cargo-cyclonedx default), grouped by licence expression. Not per crate page, not per platform.
- The script still computes per component `name, version, license, description, author, scope, links{cratesIo,docsRs,repository,website}` (`build_component`), the same fields `DependencyExplorer` reads, but emits no `dependencies.json`. History: the explorer originated as `DependencyExplorer.vue` in ocx commit `7166c012d` ("feat: add SBOM generation and dependency explorer page"); the Starlight move `d7ea60886` replaced it with the generated table (`git log -S"DependencyExplorer"` hits both).
- Release workflow `.github/workflows/release.yml:214-257` also runs cargo-cyclonedx 0.5.5 (separate artefact for the release).
- Separate SBOM surfaces in the ocx CLI (not the docs page): `ocx package sbom` (`crates/ocx_cli/src/command/package_sbom.rs`, report `api/data/sbom.rs`, `ocx_sign/src/sbom/cyclonedx.rs`, `--summary` parses CycloneDX 1.5-1.7 component counts, `website/.../package-consuming.md:274-324`).

## 4. ocx-catalog theme use on package details
- Page `src/site/pages/package.astro` composes `Breadcrumbs` (theme `ui/Breadcrumbs`), `IdentityBlock`, `InstallCard`, `MetaRail`, `PlatformMatrix`, `ReadmePane`, `VersionList`, `DetailBanners` (site-local).
- Theme components used (imports): `ui/Tag` (variants incl. `stamp` for deprecated/yanked in `IdentityBlock.astro:48,55`; `variant="filter"` as button in the matrix), `ui/LogoTile`, `ui/Icon`, `ui/Link`, `ui/Dialog`, `ui/Button`, `ui/Select`, `ui/CommandBar`, `ui/Popover`, `ui/Kbd`, `ui/SearchField`, `ui/ToggleButton`, `ui/CommandPalette`, `ui/Drawer`, `CopyMenu`, `PlatformIcons` (via `OsGlyph.astro`), `ProseCodeCopy`, `Toc`, `ToggleGroup`; plus modules `copy`, `lazy`, `command-palette`, `logo-tile`, `markdown`, `icons`, `csp`, `tokens.css`, `prose-code.css`.
- Platforms card and support-matrix dialog: `PlatformMatrix.astro` uses theme `Dialog` (title "Support matrix", line 41), `Tag` filter chips, `Button`, `CopyMenu`; the table look is hand-drawn to match theme DataTable (`PlatformMatrix.astro:193`, with `client/support_matrix.ts`, `viewmodel/support_matrix.ts`).
- Dependency/SBOM display in the catalog: none. Only OCI annotation facts `org.opencontainers.image.{licenses,source,revision}` are shown (`MetaRail.astro:65-68`, `viewmodel/catalog.ts:504-563`, `lib/releaseFacts.ts`). `src/site/lib/platforms.ts:9` mentions SBOM/signature/provenance referrers only as non-platform manifests in the index, not displayed.

## 5. Other theme consumers
- `www-setup/site`: git dependency `github:ocx-sh/website#e86b830...&path:/packages/theme` on origin/HEAD (working tree `#aa0f30f...`). No SBOM/dependency display found (grep).
- `ocx-sdk-python/site`: `0.3.0` exact; matches only CLI `sbom` doc text in `reference/api.md` and `command-map/package-tier.md`, no component.
- `find_ocx/site`: `0.5.0`; no SBOM hits.
- `ocx-marketing/learn`: git dependency `github:ocx-sh/website#ac3359ef...&path:/packages/theme` (the v0.5.0 commit). `ocx-marketing/.agents/repos/website-*` are cloned copies of ocx-sh/website, not consumers.
- Worktrees `ocx-evelynn`, `ocx-sion`, `ocx-g2fix-ci-clone` (copies of ocx) and `ocx-mirror/external/ocx` (submodule) carry `website/package.json` with the theme, same docs site.
- `ocx-mirror`, `ocx-mirror-sdk`, `ocx-indexbot`, `mirror-pypi`: no theme/catalog dependency in any package.json on origin/HEAD (all printed nothing).
- Catalog (`@ocx-sh/catalog`) consumers: `index`, e2e-index worktree.

## 6. Repositories (origin, default branch; `gh repo view`)
| checkout | origin | default branch | visibility |
|---|---|---|---|
| ocx-mirror | ocx-sh/ocx-mirror | main | PUBLIC |
| ocx-mirror-sdk | ocx-sh/ocx-mirror-sdk | main (local checkout is on `fix/docs-site-url`) | PUBLIC |
| ocx-indexbot | ocx-sh/indexbot | main | PUBLIC |
| ocx-catalog | ocx-sh/catalog | main | PUBLIC |
| index | ocx-sh/index | main | PUBLIC |
| ocx-website (theme repo) | ocx-sh/website | main | PUBLIC |
| mirror-pypi | ocx-contrib/mirror-pypi | main | PUBLIC |
Also: `ocx` -> ocx-sh/ocx; `www-setup` -> ocx-sh/www-setup; `ocx-marketing` -> ocx-sh/marketing.
Local dir names differ from repo names for indexbot (`ocx-indexbot`), catalog (`ocx-catalog`), website (`ocx-website`).

negative:
- The assumption "no SBOM/dependency component exists in the theme" is wrong: `DependencyExplorer` exists since v0.1.0. It reads a custom `dependencies.json`, not CycloneDX, and nothing in ocx/website, ocx-catalog or any other checked consumer instantiates it (grep: only theme source, its tests/showcase, and `scripts/samples/convert.ts`).
- No theme component reads CycloneDX/SPDX directly; none draws a dependency graph; no standalone licence-list component (licence filter/summary is inside DependencyExplorer). No `dependencies.json` generator exists in ocx origin/main (the python script emits Markdown only).
- ocx-catalog has no SBOM or dependency view, no OCI-referrer reading for SBOMs in `src/site` or `src/viewmodel` (grep sbom hits only the comment at `lib/platforms.ts:9`).
- Not looked at: theme `ui/DataTable` source details, npm-registry tarball contents (read from git tags instead), web repo (not a checkout).

leads:
- `DependencyExplorer.astro` + `ocx/scripts/sbom-to-markdown.py:build_component` share the field set; emitting `{generated, binaries:{ocx:{...}}}` JSON from that script would feed the component (inferred, not tested).
- `mountExplorer` accepts data via `ocx:deps:load` `detail.data` (browser), which suits a catalog page that fetches an SBOM referrer lazily.
- Pin drift: sdk-python on theme 0.3.0, catalog working tree `^0.3.0` vs origin/main `^0.5.0`, `index` working tree far behind origin/main; www-setup and ocx-marketing pin ocx-sh/website commit SHAs instead of npm.
