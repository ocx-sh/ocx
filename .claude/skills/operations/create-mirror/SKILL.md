---
name: create-mirror
description: Interactive workflow to create a new ocx-mirror configuration by analyzing GitHub Release assets, detecting platforms, and inspecting archive contents to generate mirror YAML and metadata JSON files.
allowed-tools: Read, Write, Edit, Bash, Glob, Grep, WebFetch, AskUserQuestion
---

# Create Mirror Configuration

Interactive skill that generates a complete `mirror-{name}.yml` and associated `metadata/*.json` files for a new package by analyzing its GitHub Releases.

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
    - `bin/` directory containing executables → `${installPath}/bin` for PATH
    - Root-level executable (no `bin/` dir) → `${installPath}` for PATH
    - Platform-specific layouts (e.g. macOS `.app` bundles → `${installPath}/Foo.app/Contents/bin`)
    - `man/`, `share/man/` directories → MANPATH entry
    - `lib/`, `include/` directories → note for the user but don't auto-add env vars
    - Top-level wrapper directory (e.g. `cmake-3.28.0-linux-x86_64/bin/cmake`) — the mirror pipeline strips this automatically, so paths should be relative to the content root after stripping.

12. **Check if layouts differ across platforms.** macOS often has `.app` bundles, Windows may have flat layouts. If layouts differ, create platform-specific metadata files.

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

17. **Present a summary** to the user showing:
    - Generated files
    - Detected platforms
    - Asset patterns
    - Metadata layout (shared vs platform-specific)
    - Any warnings (missing platforms, ambiguous patterns, etc.)

### Phase 5: Cleanup

18. **Remove temp downloads** used for inspection.
19. **Suggest next steps**: run `ocx-mirror check mirrors/mirror-{name}.yml` for a dry-run.

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
