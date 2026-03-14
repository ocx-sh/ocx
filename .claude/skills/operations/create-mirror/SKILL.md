---
name: create-mirror
description: Interactive workflow to create a new ocx-mirror configuration by analyzing GitHub Release assets, detecting platforms, and inspecting archive contents to generate mirror YAML, metadata JSON, description README with frontmatter, and logo.
allowed-tools: Read, Write, Edit, Bash, Glob, Grep, WebFetch, AskUserQuestion
---

# Create Mirror Configuration

Interactive skill that generates a complete `mirror-{name}.yml`, `metadata/*.json`, and `descriptions/{name}/` (README with frontmatter + logo) for a new package by analyzing its GitHub Releases.

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

7. **Derive platform mappings.** Match asset filenames to platforms using common naming conventions:

   | Platform | Common substrings |
   |----------|-------------------|
   | `linux/amd64` | `linux-x86_64`, `linux-amd64`, `linux64`, `Linux-x86_64` |
   | `linux/arm64` | `linux-aarch64`, `linux-arm64`, `Linux-aarch64` |
   | `darwin/amd64` | `darwin-x86_64`, `macos-x86_64`, `macOS-x86_64`, `macos-universal`, `Darwin-x86_64`, `apple-darwin` |
   | `darwin/arm64` | `darwin-arm64`, `macos-arm64`, `darwin-aarch64`, `macos-universal`, `apple-darwin` |
   | `windows/amd64` | `windows-x86_64`, `win64`, `windows-amd64`, `win-x64`, `pc-windows` |
   | `windows/arm64` | `windows-arm64`, `win-arm64` |

   - If a `universal` or `any` asset exists for macOS, map it to both `darwin/amd64` and `darwin/arm64`.
   - Build regex patterns per platform. Use `.*` for version segments, escape dots and special chars.
   - If asset names changed between versions, add multiple patterns per platform (ordered newest first).

8. **Present the detected platforms and patterns to the user** for confirmation. Show which assets matched which platforms.

### Phase 3: Determine Asset Type and Inspect Contents

9. **Classify asset types.** Check file extensions:
   - `.tar.gz`, `.tar.xz`, `.tar.bz2`, `.tgz` → compressed tarball
   - `.zip` → zip archive
   - No extension or `.exe` → raw binary

10. **For archives: download and inspect samples.** Download one archive per unique platform layout (typically: one Linux, one macOS, one Windows if they differ). Use a temp directory.

```bash
# Download a sample asset
curl -fsSL -o /tmp/ocx-mirror-inspect/{filename} "{download_url}"

# Inspect tarball
tar tf /tmp/ocx-mirror-inspect/{filename} | head -50

# Inspect zip
unzip -l /tmp/ocx-mirror-inspect/{filename} | head -50
```

11. **Analyze directory structure.** Look for:
    - **Top-level wrapper directory** (e.g. `cmake-3.28.0-linux-x86_64/bin/cmake`) — if ALL entries share a common prefix directory, this needs `strip_components: 1` in the mirror YAML so the rebundling process strips it. Count the depth of the common prefix to determine the correct value (usually 1).
    - `bin/` directory containing executables → `${installPath}/bin` for PATH
    - Root-level executable (no `bin/` dir) → `${installPath}` for PATH
    - Platform-specific layouts (e.g. macOS `.app` bundles → `${installPath}/Foo.app/Contents/bin`)
    - `man/`, `share/man/` directories → MANPATH entry
    - `lib/`, `include/` directories → note for the user but don't auto-add env vars
    - Paths in metadata should be relative to the content root **after stripping** (e.g. if the archive has `cmake-3.28/bin/cmake` and `strip_components: 1`, the metadata PATH should use `${installPath}/bin`).

    **Important:** `strip_components` in the mirror YAML controls rebundling (stripping the wrapper directory before creating the OCX package). This is separate from `strip_components` in metadata JSON, which tells OCX how to extract the package after downloading. Typically, the mirror YAML needs `strip_components` but the metadata does not (since the rebundled archive is already clean).

12. **Check if layouts differ across platforms.** macOS often has `.app` bundles, Windows may have flat layouts. If layouts differ, create platform-specific metadata files. Check strip_components consistency across platforms — if all platforms have a top-level wrapper directory, a single `strip_components` value works. If some don't (e.g. Windows zips are sometimes flat), note this as a potential issue for the user.

13. **For raw binaries:** the metadata just needs a PATH entry pointing to `${installPath}` (the binary lands directly in the content root).

### Phase 4: Generate Configuration Files

14. **Generate metadata JSON files.** Write to `mirrors/metadata/{name}.json` (and platform variants if needed):

```json
{
  "type": "bundle",
  "version": 1,
  "env": [
    {
      "key": "PATH",
      "type": "path",
      "required": true,
      "value": "${installPath}/bin"
    }
  ]
}
```

15. **Generate the mirror YAML.** Write to `mirrors/mirror-{name}.yml`:

```yaml
name: {name}
target:
  registry: {registry}
  repository: {repository}

source:
  type: github_release
  owner: {owner}
  repo: {repo}
  tag_pattern: "{pattern}"

assets:
  {platform}:
    - "{regex}"
  # ... per detected platform

# Only include if archives have a top-level wrapper directory
# strip_components: 1

metadata:
  default: metadata/{name}.json
  # platforms: ... (only if platform-specific metadata needed)

skip_prereleases: true
cascade: true
build_timestamp: none

versions:
  min: "{min_version}"
  new_per_run: 10

verify:
  github_asset_digest: true

concurrency:
  max_downloads: 8
  max_bundles: 4
  max_pushes: 2
  rate_limit_ms: 100
  max_retries: 3
  compression_threads: 0
```

16. **Validate the generated spec.** If `ocx-mirror` binary is available:

```bash
cargo run -p ocx_mirror -- validate mirrors/mirror-{name}.yml
```

### Phase 5: Generate Description Assets

17. **Create the description directory** at `mirrors/descriptions/{name}/`.

18. **Download a logo.** Look for a logo in these locations (in order):
    - The GitHub repository's social preview / OpenGraph image: `gh api "repos/{owner}/{repo}" --jq '.owner.avatar_url'` (fallback only)
    - The repository's root for common logo files: check if `logo.svg`, `logo.png`, `icon.svg`, or `icon.png` exists at the repo root via `gh api "repos/{owner}/{repo}/contents/" --jq '.[].name'`
    - The project's website (look for an SVG or PNG logo in the HTML `<head>` or hero section)
    - **Prefer SVG over PNG** — SVGs are resolution-independent and typically smaller.
    - **Ask the user** if no logo is found automatically, or if multiple candidates exist. They may provide a URL or local path.
    - Download to `mirrors/descriptions/{name}/logo.svg` (or `.png`).

```bash
# Check repo root for logo files
gh api "repos/{owner}/{repo}/contents/" --jq '[.[] | select(.name | test("^(logo|icon)\\.(svg|png)$"; "i")) | {name, download_url}]'

# Download a logo
curl -fsSL -o mirrors/descriptions/{name}/logo.svg "{download_url}"
```

19. **Write the README with frontmatter.** Generate `mirrors/descriptions/{name}/README.md`:

```markdown
---
title: {display_name}
description: {one_line_description}
keywords: {comma_separated_keywords}
---

# {display_name}

{2-3 sentence description of what the tool is and does. Research the project's
GitHub description and website to write an accurate summary.}

## What's included

{List the main executables or components included in the package. Derive this
from the archive inspection in Phase 3.}

## Usage with OCX

\```sh
# Install a specific version
ocx install {name}:{example_version}

# Run directly
ocx exec {name}:{example_version} -- {main_command} --version

# Set as current
ocx install {name}:{example_version} --select
\```

## Links

- [{display_name} Documentation]({docs_url})
- [{display_name} on GitHub](https://github.com/{owner}/{repo})
```

    **Frontmatter fields:**
    - `title`: Human-readable display name (e.g. "CMake", "Go Task", "Buf")
    - `description`: One-line summary suitable for catalog display (max ~100 chars)
    - `keywords`: Comma-separated search terms — include the tool name, language ecosystem, and category (e.g. `cmake,build,cpp,c,build-system,cross-platform`)

    **Body content:**
    - Research the project by reading its GitHub description (`gh api "repos/{owner}/{repo}" --jq '.description'`) and website
    - List executables found during archive inspection
    - Use the minimum version from Phase 1 as the example version
    - Include links to docs and GitHub

20. **Present a summary** to the user showing:
    - Generated files (mirror YAML, metadata JSON, README, logo)
    - Detected platforms
    - Asset patterns
    - Whether `strip_components` was set (and why)
    - Metadata layout (shared vs platform-specific)
    - README frontmatter values
    - Any warnings (missing platforms, ambiguous patterns, missing logo, etc.)

### Phase 6: Cleanup

21. **Remove temp downloads** used for inspection.
22. **Suggest next steps**: run `ocx-mirror check mirrors/mirror-{name}.yml` for a dry-run.

## Key Rules

- **Always use `gh api`** for GitHub API access — never raw curl to api.github.com (auth handled by gh).
- **Sample multiple releases** — asset naming conventions often change between major versions.
- **Escape regex special characters** in asset patterns: `.` → `\\.`, `+` → `\\+`, etc.
- **Order patterns newest-first** within each platform's list — the resolver tries them in order.
- **Strip version segments** in patterns — replace version numbers with `.*` so they match across versions.
- **Confirm with the user** before writing files — show what will be generated.
- **Do NOT use `verify.checksums_file`** for GitHub releases — it expects a full URL, not an asset filename. The `github_asset_digest: true` setting already verifies integrity via the GitHub API's per-asset digest, which is sufficient. The `checksums_file` option is only for `url_index` sources where you have a direct URL to the checksums file.
- **Default to `build_timestamp: none`** — most mirrors don't need timestamp-differentiated builds.
- **Default to `skip_prereleases: true`** — can be changed by the user.
- **Always generate a README with frontmatter** — the `title`, `description`, and `keywords` in the YAML frontmatter are extracted as OCI annotations by `ocx package describe` and stripped from the pushed README body. This is the primary way to set catalog metadata.
- **Prefer SVG logos** over PNG — they scale to any resolution and are typically smaller.

## Reference: Platform Detection Heuristics

Asset filenames encode platform info in various ways. Common patterns ranked by reliability:

1. **Explicit os-arch**: `tool-linux-amd64.tar.gz` — highest confidence
2. **Explicit os_arch**: `tool-linux_amd64.tar.gz` — high confidence
3. **Rust triple**: `tool-x86_64-unknown-linux-gnu.tar.gz` — high confidence
4. **Go-style**: `tool_Linux_x86_64.tar.gz` — high confidence (note capital L)
5. **Loose match**: `tool-linux64.tar.gz` — medium confidence, confirm with user

When multiple assets could match the same platform (e.g. both `-gnu` and `-musl` variants for Linux), ask the user which to prefer. Default to `gnu`/`glibc` variants.

## Reference: Common Environment Variables by Tool Type

| Tool Type | Env Vars |
|-----------|----------|
| CLI tool (single binary) | `PATH` |
| SDK/toolchain | `PATH`, `{TOOL}_HOME` (e.g. `JAVA_HOME`, `GOROOT`) |
| Library | `LD_LIBRARY_PATH` / `DYLD_LIBRARY_PATH`, `PKG_CONFIG_PATH` |
| Tool with man pages | `PATH`, `MANPATH` |
