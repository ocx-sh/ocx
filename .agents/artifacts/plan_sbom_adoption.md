# Plan: SBOM Adoption (cargo-auditable + Website Dependencies Page)

**ADR:** `adr_sbom_strategy.md`
**Branch:** `sion`
**Status:** Proposed

---

## Goal

1. Enable `cargo-auditable` so every released binary embeds its exact dependency tree
2. Generate a CycloneDX SBOM per release (attached to GitHub Release)
3. Publish a "Dependencies" Markdown page on the website, auto-generated from the SBOM

## Overview

```
dist-workspace.toml
  └── cargo-auditable = true        →  binary with .dep-v0 section
  └── cargo-cyclonedx = true        →  bom.json per platform in release

taskfile (local + CI website build)
  └── cargo-cyclonedx → bom.json    →  Python script → dependencies.md
                                         └── VitePress renders with site theme
```

---

## Steps

### Step 1: Enable cargo-auditable in cargo-dist

**File:** `dist-workspace.toml`

Add under `[dist]`:
```toml
cargo-auditable = true
```

This tells cargo-dist to use `cargo auditable build` instead of `cargo build`. Each platform binary gets a `.dep-v0` linker section with its exact compiled dependency tree. Trivy, Grype, and `cargo audit bin` can scan the binary directly.

**Prerequisite:** `cargo-auditable` must be installed in the build environment. cargo-dist handles this automatically when the config flag is set.

### Step 2: Enable cargo-cyclonedx in cargo-dist

**File:** `dist-workspace.toml`

Add under `[dist]`:
```toml
cargo-cyclonedx = true
```

This generates a CycloneDX `bom.xml` alongside each platform's release archive. The SBOM is distributed as a GitHub Release asset.

### Step 3: Regenerate release workflow

```sh
dist generate-ci
```

This regenerates `.github/workflows/release.yml` with the new cargo-auditable and cargo-cyclonedx steps. Commit both `dist-workspace.toml` and the regenerated workflow together.

### Step 4: Add local SBOM generation task

**New file:** `taskfiles/sbom.taskfile.yml`

```yaml
version: '3'

vars:
  SBOM_OUTPUT: "{{.ROOT_DIR}}/website/src/docs/reference/dependencies.md"

tasks:
  install:cyclonedx:
    internal: true
    cmds:
      - cargo install --locked cargo-cyclonedx
    status:
      - cargo cyclonedx --version

  generate:json:
    desc: Generate CycloneDX SBOM JSON
    deps:
      - install:cyclonedx
    cmds:
      - cargo cyclonedx --format json -p ocx

  generate:page:
    desc: Generate dependencies page for website
    deps:
      - generate:json
    cmds:
      - uv run {{.ROOT_DIR}}/scripts/sbom-to-markdown.py
          --input {{.ROOT_DIR}}/crates/ocx_cli/bom.json
          --output {{.SBOM_OUTPUT}}

  default:
    desc: Generate SBOM and dependencies page
    cmds:
      - task: generate:page
```

**Register in root `taskfile.yml`:**
```yaml
includes:
  sbom: ./taskfiles/sbom.taskfile.yml
```

### Step 5: Write the SBOM-to-Markdown converter script

**New file:** `scripts/sbom-to-markdown.py`

A small Python script (~50 lines) that:
1. Reads the CycloneDX JSON (`bom.json`)
2. Extracts components: name, version, license, purl, description
3. Outputs a VitePress-compatible Markdown file with:
   - YAML frontmatter (title, description)
   - Intro paragraph explaining what this page is
   - A Markdown table of all dependencies sorted by name
   - Generation timestamp

Example output structure:
```markdown
---
title: Dependencies
---

# Dependencies {#dependencies}

This page lists all third-party dependencies compiled into the `ocx` binary,
generated from the [CycloneDX SBOM][cyclonedx] at build time.

Last updated: 2026-03-13

## Components

| Name | Version | License | Description |
|------|---------|---------|-------------|
| clap | 4.5.x | MIT OR Apache-2.0 | Command-line argument parser |
| ... | ... | ... | ... |

[cyclonedx]: https://cyclonedx.org/
```

### Step 6: Integrate into website build pipeline

**File:** `website/taskfile.yml`

Add SBOM generation before VitePress build:
```yaml
build:
  deps:
    - install
  cmds:
    - task: :schema:generate
    - task: :recordings:build
    - task: :sbom:generate:page    # NEW
    - bunx vitepress build
```

### Step 7: Add to VitePress sidebar

**File:** `website/.vitepress/config.mts`

Add "Dependencies" under the Reference section:
```typescript
{
  text: "Reference",
  collapsed: true,
  items: [
    { text: "Command Line", link: "/docs/reference/command-line" },
    { text: "Environment", link: "/docs/reference/environment" },
    { text: "Metadata", link: "/docs/reference/metadata" },
    { text: "Dependencies", link: "/docs/reference/dependencies" },  // NEW
  ],
},
```

### Step 8: Add .gitignore entry

The generated `dependencies.md` should be gitignored (like the JSON schema) since it's regenerated on every build:

**File:** `website/.gitignore` (or root `.gitignore`)

```
src/docs/reference/dependencies.md
```

### Step 9: Verify

1. Run `task sbom` locally — confirm `bom.json` is generated and `dependencies.md` looks correct
2. Run `task website:build` — confirm VitePress builds with the new page
3. Run `task verify` — confirm nothing is broken
4. Create a pre-release tag and verify:
   - GitHub Release includes `bom.xml` alongside each platform archive
   - Binary contains `.dep-v0` section: `cargo auditable info target/release/ocx`
   - Website deploys with the Dependencies page

---

## Files Changed

| File | Change |
|------|--------|
| `dist-workspace.toml` | Add `cargo-auditable = true`, `cargo-cyclonedx = true` |
| `.github/workflows/release.yml` | Regenerated by `dist generate-ci` |
| `taskfiles/sbom.taskfile.yml` | New — SBOM generation tasks |
| `taskfile.yml` | Include `sbom` taskfile |
| `scripts/sbom-to-markdown.py` | New — CycloneDX JSON to Markdown converter |
| `website/taskfile.yml` | Add `:sbom:generate:page` to build pipeline |
| `website/.vitepress/config.mts` | Add Dependencies to sidebar |
| `website/.gitignore` | Ignore generated dependencies.md |

## Files NOT Changed

| File | Reason |
|------|--------|
| `.github/workflows/post-release-oci-publish.yml` | No SBOM in OCI packages (future phase) |
| `Cargo.toml` / `Cargo.lock` | cargo-auditable is a build tool, not a dependency |

## Out of Scope

- Signed attestations (`actions/attest-sbom`) — future phase
- SBOM as OCI referrer artifact — future phase
- Native Cargo SBOM (RFC 3553) — waiting for stabilization
- Per-platform SBOM comparison on website — overkill for now; single page from host-platform build is sufficient for the website
