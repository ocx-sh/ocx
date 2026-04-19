---
name: ocx-create-mirror
description: Use when creating a new ocx-mirror configuration for a GitHub-released tool. Inspects Release assets, detects platforms, examines archive contents, and produces mirror YAML, metadata JSON, README, and logo. Triggers: "create a mirror", "/ocx-create-mirror", "add mirror for <tool>".
disable-model-invocation: true
---

# Create Mirror Configuration

Interactive skill that generates a complete `mirrors/{name}/` directory containing `mirror.yml`, `metadata.json`, `README.md` (with frontmatter), and `logo.svg` for a new package by analyzing its GitHub Releases.

## Workflow

### Phase 1: Gather Input

1. **Ask for the GitHub repository** (owner/repo format, e.g. `Kitware/CMake`)
2. **Ask for the package name** — the short name used in OCX (e.g. `cmake`). Default: lowercase repo name.
3. **Ask for the target registry and repository** — default registry `dev.ocx.sh`, default repository is the package name.
4. **Ask for the minimum version** to mirror (semver string, e.g. `3.28.0`).

### Phase 2: Analyze GitHub Releases

Use the GitHub CLI (`gh`) to fetch release data — NOT the web API directly.

```bash
# Fetch recent releases (get ~10 for good coverage)
gh api "repos/{owner}/{repo}/releases?per_page=10" --jq '.[].tag_name'

# Fetch assets for a specific release
gh api "repos/{owner}/{repo}/releases/tags/{tag}" --jq '.assets[] | {name, size, content_type, browser_download_url}'
```

5. **Detect the tag pattern.** Look at tag names and derive a regex with `(?P<version>...)` capture group. Common patterns:
   - `v1.2.3` → `^v(?P<version>\d+\.\d+\.\d+)$`
   - `1.2.3` → `^(?P<version>\d+\.\d+\.\d+)$`
   - `release-1.2.3` → `^release-(?P<version>\d+\.\d+\.\d+)$`
   - Confirm with the user if ambiguous.

6. **Sample multiple releases.** Pick 2-3 releases spread across the version range (recent, middle, oldest above min) to account for naming changes over time. Collect all asset filenames.

7. **Derive platform mappings.** Match asset filenames to platforms using common naming conventions. See [`references/platform-detection.md`](./references/platform-detection.md) for the full platform mapping table and musl-vs-glibc decision process.

8. **Present the detected platforms and patterns to the user** for confirmation. Show which assets matched which platforms.

### Phase 3: Determine Asset Type and Inspect Contents

9. **Classify asset types.** Check file extensions:
   - `.tar.gz`, `.tar.xz`, `.tar.bz2`, `.tgz` → compressed tarball
   - `.zip` → zip archive
   - No extension or `.exe` → raw binary

   **Mixed asset types across platforms** (e.g. tarballs on Linux/macOS but a raw `.exe` on Windows — common for Rust CLI tools like lychee, ripgrep, fd) require the per-platform `asset_type` form. See [`references/archive-inspection.md`](./references/archive-inspection.md).

10. **For archives: download and inspect samples.** Download one archive per unique platform layout (typically: one Linux, one macOS, one Windows if they differ). Use a temp directory.

    ```bash
    # Download a sample asset
    curl -fsSL -o /tmp/ocx-mirror-inspect/{filename} "{download_url}"

    # Inspect tarball
    tar tf /tmp/ocx-mirror-inspect/{filename} | head -50

    # Inspect zip
    unzip -l /tmp/ocx-mirror-inspect/{filename} | head -50
    ```

11. **Analyze directory structure.** Detect top-level wrapper directories (→ `strip_components: 1`), `bin/` layouts, `.app` bundles, and `man/` locations. See [`references/archive-inspection.md`](./references/archive-inspection.md) for the full analysis rules, macOS gotchas (`__MACOSX/` junk, `.app` bundles, platform-specific metadata), and per-platform `strip_components` / `asset_type` forms.

12. **Verify Linux musl binaries are statically linked** when the chosen Linux asset is a non-Rust-triple musl variant. See [`references/platform-detection.md`](./references/platform-detection.md) for the decision process.

### Phase 4: Generate Configuration Files

13. **Generate metadata JSON and mirror YAML** using the templates in [`references/yaml-templates.md`](./references/yaml-templates.md). Write to `mirrors/{name}/metadata.json` and `mirrors/{name}/mirror.yml`. Add platform-specific metadata variants (e.g. `metadata-darwin.json`) when Phase 3 detected layout drift.

14. **Validate the generated spec.** If `ocx-mirror` binary is available:

    ```bash
    cargo run -p ocx_mirror -- validate mirrors/{name}/mirror.yml
    ```

### Phase 5: Generate Description Assets

15. **Download a logo.** Look for a logo in these locations (in order):
    - The repository's root for common logo files: check if `logo.svg`, `logo.png`, `icon.svg`, or `icon.png` exists at the repo root via `gh api "repos/{owner}/{repo}/contents/" --jq '.[].name'`
    - The project's website (look for an SVG or PNG logo in the HTML `<head>` or hero section)
    - The GitHub repository's social preview / OpenGraph image: `gh api "repos/{owner}/{repo}" --jq '.owner.avatar_url'` (fallback only)
    - **Prefer SVG over PNG** — SVGs are resolution-independent and typically smaller.
    - **Ask the user** if no logo is found automatically, or if multiple candidates exist. They may provide a URL or local path.
    - Download to `mirrors/{name}/logo.svg` (or `.png`).

    ```bash
    # Check repo root for logo files
    gh api "repos/{owner}/{repo}/contents/" --jq '[.[] | select(.name | test("^(logo|icon)\\.(svg|png)$"; "i")) | {name, download_url}]'

    # Download a logo
    curl -fsSL -o mirrors/{name}/logo.svg "{download_url}"
    ```

16. **Write the README with frontmatter.** Use the README template in [`references/yaml-templates.md`](./references/yaml-templates.md). Frontmatter `title` / `description` / `keywords` become OCI annotations via `ocx package describe`.

17. **Present a summary** to the user showing:
    - Generated files (mirror YAML, metadata JSON, README, logo)
    - Detected platforms
    - Asset patterns
    - Whether `strip_components` was set (and why)
    - Metadata layout (shared vs platform-specific)
    - README frontmatter values
    - Taskfile registration (`task mirror:{name}:sync`, `task mirror:{name}:describe`)
    - Any warnings (missing platforms, ambiguous patterns, missing logo, etc.)

### Phase 6: Register in Taskfile

18. **Add the mirror to `taskfiles/mirror.taskfile.yml`** using the includes + `sync-all` + `describe-all` pattern in [`references/yaml-templates.md`](./references/yaml-templates.md).

### Phase 7: Cleanup

19. **Remove temp downloads** used for inspection.
20. **Suggest next steps**: run `task mirror:{name}:sync -- --dry-run` to preview what would be mirrored.

## Key Rules

- **Always use `gh api`** for GitHub API access — never raw curl to api.github.com (auth handled by gh).
- **Sample multiple releases** — asset naming conventions often change between major versions.
- **Escape regex special characters** in asset patterns: `.` → `\\.`, `+` → `\\+`, etc.
- **Anchor asset patterns with `$`** — many projects publish sidecar files alongside archives (e.g. `.tar.gz.sha512`, `.tar.gz.sig`, `.tar.gz.asc`). Without a `$` anchor, a pattern like `tool-.*\\.tar\\.gz` will match all three files and trigger an "Ambiguous asset match" warning. Always end archive patterns with `$` (e.g. `tool-.*\\.tar\\.gz$`). Use `^` at the start too if needed to prevent partial prefix matches.
- **Order patterns newest-first** within each platform's list — the resolver tries them in order.
- **Strip version segments** in patterns — replace version numbers with `.*` so they match across versions.
- **Confirm with the user** before writing files — show what will be generated.
- **Do NOT use `verify.checksums_file`** for GitHub releases — it expects a full URL, not an asset filename. The `github_asset_digest: true` setting already verifies integrity via the GitHub API's per-asset digest, which is sufficient. The `checksums_file` option is only for `url_index` sources where you have a direct URL to the checksums file.
- **Default to `build_timestamp: none`** — most mirrors don't need timestamp-differentiated builds.
- **Default to `skip_prereleases: true`** — can be changed by the user.
- **Always generate a README with frontmatter** — the `title`, `description`, and `keywords` in the YAML frontmatter are extracted as OCI annotations by `ocx package describe` and stripped from the pushed README body. This is the primary way to set catalog metadata.
- **Prefer SVG logos** over PNG — they scale to any resolution and are typically smaller.

## References

- [`references/platform-detection.md`](./references/platform-detection.md) — platform mapping table, musl vs glibc
- [`references/archive-inspection.md`](./references/archive-inspection.md) — directory-layout analysis, macOS gotchas, per-platform forms
- [`references/yaml-templates.md`](./references/yaml-templates.md) — metadata.json, mirror.yml, README, Taskfile templates
- [`references/env-vars.md`](./references/env-vars.md) — common env var sets by tool type
