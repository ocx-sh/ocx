---
name: ocx-create-mirror
description: Use when creating new ocx-mirror config for GitHub-released tool. Inspects Release assets, detects platforms, examines archive contents, produces mirror YAML, metadata JSON, README, logo. Triggers: "create a mirror", "/ocx-create-mirror", "add mirror for <tool>".
disable-model-invocation: true
---

# Create Mirror Configuration

Interactive skill. Generates complete `mirrors/{name}/` dir with `mirror.yml`, `metadata.json`, `README.md` (frontmatter), `logo.svg` for new package by analyzing GitHub Releases.

## Workflow

### Phase 1: Gather Input

1. **Ask for the GitHub repository** (owner/repo format, e.g. `Kitware/CMake`)
2. **Ask for the package name** — short name in OCX (e.g. `cmake`). Default: lowercase repo name.
3. **Ask for the target registry and repository** — default registry `dev.ocx.sh`, default repository = package name.
4. **Ask for the minimum version** to mirror (semver string, e.g. `3.28.0`).

### Phase 2: Analyze GitHub Releases

Use GitHub CLI (`gh`) for release data — NOT web API direct.

```bash
# Fetch recent releases (get ~10 for good coverage)
gh api "repos/{owner}/{repo}/releases?per_page=10" --jq '.[].tag_name'

# Fetch assets for a specific release
gh api "repos/{owner}/{repo}/releases/tags/{tag}" --jq '.assets[] | {name, size, content_type, browser_download_url}'
```

5. **Detect the tag pattern.** Look at tag names, derive regex with `(?P<version>...)` capture group. Common patterns:
   - `v1.2.3` → `^v(?P<version>\d+\.\d+\.\d+)$`
   - `1.2.3` → `^(?P<version>\d+\.\d+\.\d+)$`
   - `release-1.2.3` → `^release-(?P<version>\d+\.\d+\.\d+)$`
   - Confirm with user if ambiguous.

6. **Sample multiple releases.** Pick 2-3 releases across version range (recent, middle, oldest above min) to cover naming changes over time. Collect all asset filenames.

7. **Derive platform mappings.** Match asset filenames to platforms via common naming conventions. See [`references/platform-detection.md`](./references/platform-detection.md) for full platform mapping table and musl-vs-glibc decision.

8. **Present the detected platforms and patterns to the user** for confirmation. Show which assets matched which platforms.

### Phase 3: Determine Asset Type and Inspect Contents

9. **Classify asset types.** Check file extensions:
   - `.tar.gz`, `.tar.xz`, `.tar.bz2`, `.tgz` → compressed tarball
   - `.zip` → zip archive
   - No extension or `.exe` → raw binary

   **Mixed asset types across platforms** (e.g. tarballs on Linux/macOS, raw `.exe` on Windows — common for Rust CLIs like lychee, ripgrep, fd) need per-platform `asset_type` form. See [`references/archive-inspection.md`](./references/archive-inspection.md).

10. **For archives: download and inspect samples.** Download one archive per unique platform layout (typically: one Linux, one macOS, one Windows if differ). Use temp dir.

    ```bash
    # Download a sample asset
    curl -fsSL -o /tmp/ocx-mirror-inspect/{filename} "{download_url}"

    # Inspect tarball
    tar tf /tmp/ocx-mirror-inspect/{filename} | head -50

    # Inspect zip
    unzip -l /tmp/ocx-mirror-inspect/{filename} | head -50
    ```

11. **Analyze directory structure.** Detect top-level wrapper dirs (→ `strip_components: 1`), `bin/` layouts, `.app` bundles, `man/` locations. See [`references/archive-inspection.md`](./references/archive-inspection.md) for full analysis rules, macOS gotchas (`__MACOSX/` junk, `.app` bundles, platform-specific metadata), per-platform `strip_components` / `asset_type` forms.

12. **Verify Linux musl binaries are statically linked** when chosen Linux asset is non-Rust-triple musl variant. See [`references/platform-detection.md`](./references/platform-detection.md) for decision.

### Phase 4: Generate Configuration Files

13. **Generate metadata JSON and mirror YAML** using templates in [`references/yaml-templates.md`](./references/yaml-templates.md). Write to `mirrors/{name}/metadata.json` and `mirrors/{name}/mirror.yml`. Add platform-specific metadata variants (e.g. `metadata-darwin.json`) when Phase 3 detected layout drift.

14. **Validate the generated spec.** If `ocx-mirror` binary available:

    ```bash
    cargo run -p ocx_mirror -- validate mirrors/{name}/mirror.yml
    ```

### Phase 5: Generate Description Assets

15. **Download a logo.** Look for logo in these locations (in order):
    - Repo root for common logo files: check `logo.svg`, `logo.png`, `icon.svg`, `icon.png` at repo root via `gh api "repos/{owner}/{repo}/contents/" --jq '.[].name'`
    - Project website (SVG or PNG logo in HTML `<head>` or hero section)
    - GitHub repo social preview / OpenGraph image: `gh api "repos/{owner}/{repo}" --jq '.owner.avatar_url'` (fallback only)
    - **Prefer SVG over PNG** — resolution-independent, smaller.
    - **Ask the user** if no logo found automatically, or multiple candidates. May provide URL or local path.
    - Download to `mirrors/{name}/logo.svg` (or `.png`).

    ```bash
    # Check repo root for logo files
    gh api "repos/{owner}/{repo}/contents/" --jq '[.[] | select(.name | test("^(logo|icon)\\.(svg|png)$"; "i")) | {name, download_url}]'

    # Download a logo
    curl -fsSL -o mirrors/{name}/logo.svg "{download_url}"
    ```

16. **Write the README with frontmatter.** Use README template in [`references/yaml-templates.md`](./references/yaml-templates.md). Frontmatter `title` / `description` / `keywords` become OCI annotations via `ocx package describe`.

17. **Present a summary** to user showing:
    - Generated files (mirror YAML, metadata JSON, README, logo)
    - Detected platforms
    - Asset patterns
    - Whether `strip_components` set (and why)
    - Metadata layout (shared vs platform-specific)
    - README frontmatter values
    - Taskfile registration (`task mirror:{name}:sync`, `task mirror:{name}:describe`)
    - Warnings (missing platforms, ambiguous patterns, missing logo, etc.)

### Phase 6: Register in Taskfile

18. **Add the mirror to `taskfiles/mirror.taskfile.yml`** using includes + `sync-all` + `describe-all` pattern in [`references/yaml-templates.md`](./references/yaml-templates.md).

### Phase 7: Cleanup

19. **Remove temp downloads** used for inspection.
20. **Suggest next steps**: run `task mirror:{name}:sync -- --dry-run` to preview mirror output.

## Key Rules

- **Always use `gh api`** for GitHub API — never raw curl to api.github.com (gh handles auth).
- **Sample multiple releases** — asset naming conventions change between major versions.
- **Escape regex special characters** in asset patterns: `.` → `\\.`, `+` → `\\+`, etc.
- **Anchor asset patterns with `$`** — many projects publish sidecar files alongside archives (e.g. `.tar.gz.sha512`, `.tar.gz.sig`, `.tar.gz.asc`). Without `$` anchor, pattern like `tool-.*\\.tar\\.gz` matches all three, triggers "Ambiguous asset match" warning. Always end archive patterns with `$` (e.g. `tool-.*\\.tar\\.gz$`). Use `^` at start too to prevent partial prefix matches.
- **Order patterns newest-first** within each platform's list — resolver tries in order.
- **Strip version segments** in patterns — replace version numbers with `.*` to match across versions.
- **Confirm with the user** before writing files — show what gets generated.
- **Do NOT use `verify.checksums_file`** for GitHub releases — expects full URL, not asset filename. `github_asset_digest: true` already verifies integrity via GitHub API per-asset digest, sufficient. `checksums_file` only for `url_index` sources with direct URL to checksums file.
- **Default to `build_timestamp: none`** — most mirrors no need timestamp-differentiated builds.
- **Default to `skip_prereleases: true`** — user can change.
- **Always generate a README with frontmatter** — `title`, `description`, `keywords` in YAML frontmatter extracted as OCI annotations by `ocx package describe`, stripped from pushed README body. Primary way to set catalog metadata.
- **Prefer SVG logos** over PNG — scale to any resolution, smaller.

## References

- [`references/platform-detection.md`](./references/platform-detection.md) — platform mapping table, musl vs glibc
- [`references/archive-inspection.md`](./references/archive-inspection.md) — directory-layout analysis, macOS gotchas, per-platform forms
- [`references/yaml-templates.md`](./references/yaml-templates.md) — metadata.json, mirror.yml, README, Taskfile templates
- [`references/env-vars.md`](./references/env-vars.md) — common env var sets by tool type