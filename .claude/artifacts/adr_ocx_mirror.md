# ADR: ocx-mirror -- Binary Mirroring Tool

## Status

Proposed

## Context

OCX enables teams to install pre-built binaries from OCI registries. However, many popular tools (CMake, Node.js, Go, ripgrep, fd, etc.) distribute their releases through GitHub Releases, project websites, or language-specific registries -- not OCI registries. Before end users can `ocx install cmake:3.28`, someone must package each release for every platform and push it to an OCI registry.

Today this is a manual, per-tool process:

1. Download the release archive for each platform.
2. Extract it, arrange the directory layout.
3. Write a `metadata.json` with the correct env vars and `strip_components`.
4. Run `ocx package create` and `ocx package push --platform <os/arch> --cascade` for each platform.
5. Repeat for every new version.

This is tedious, error-prone, and does not scale. The OCX ecosystem needs a way to automate mirroring from upstream sources into OCI registries.

### Requirements

1. **Declarative mirror specs**: A YAML file describes what to mirror -- source, version filters, platform mappings, metadata template, target registry/repo.
2. **Incremental sync**: Only mirror versions not already present in the target registry.
3. **Multi-source**: GitHub Releases as the primary adapter, with a generic URL index for other sources, and an extension point for future adapters.
4. **Multi-platform**: Each version may have binaries for multiple OS/arch combinations. The tool must push each platform variant and build a multi-platform OCI Image Index.
5. **Idempotent and resumable**: Interrupted runs can be re-run safely. Each version+platform is an atomic unit.
6. **CI-friendly**: Easy to run in GitHub Actions on a schedule.

## Decision

Build `ocx-mirror` as a **separate binary crate** (`crates/ocx_mirror`) that depends on `ocx_lib` as a library. It will NOT be a subcommand of `ocx` itself. The tool reads a mirror spec YAML file and executes a download-package-push pipeline for each version+platform pair that is not already present in the target registry.

## Mirror Spec Format

The mirror spec is a YAML file that describes a single package to mirror. One file per package. The filename convention is `mirror-{name}.yaml` (e.g., `mirror-cmake.yaml`).

### Canonical Internal Representation

All source adapters — regardless of type — produce the same `VersionInfo` structure: a version string plus a flat map of **asset name → download URL**. This mirrors exactly what GitHub Releases exposes: a list of named file assets attached to a release. The `url_index` source produces the same shape.

Platform resolution — mapping asset names to `(platform, URL)` pairs — is a **shared pipeline step** applied after the adapter returns, using the spec-level `assets:` patterns. This means:

- Asset pattern configuration lives once in the spec, not duplicated per source type.
- Both source types go through identical resolution logic.
- Ambiguity (two assets matching the same platform's patterns) is caught uniformly and always produces an error.

### Schema

```yaml
# Required. Human-readable name, used in logging only.
name: string

# Required. Target OCI registry and repository.
target:
  registry: string           # e.g., "ocx.sh", "ghcr.io/myorg"
  repository: string         # e.g., "cmake", "tools/cmake"

# Required. Source configuration. The 'type' field is a discriminant.
source:
  type: github_release | url_index   # Required.

  # ── github_release fields ──────────────────────────────────────────────
  owner: string              # GitHub org or user
  repo: string               # GitHub repository name

  # Regex with a named capture group 'version' that extracts the version
  # string from the release tag name. The captured string is passed directly
  # to Version::parse(), so it must yield a valid OCX version (X.Y.Z or
  # X.Y.Z-prerelease). If a release tag does not match this pattern, that
  # release is skipped with a warning — not a hard error.
  #
  # Default: "^v?(?P<version>\\d+\\.\\d+\\.\\d+)(?:-(?P<prerelease>[0-9a-zA-Z]+))?$"
  #   Matches: v3.28.0, 3.28.0, v3.28.0-rc1, v3.28.0-beta2, 3.28.0-alpha
  #   Does not match: date-based tags, tags with extra prefixes/suffixes, or
  #   prerelease tokens containing dots or hyphens (e.g. "rc.1", "beta-1").
  #   The 'version' group captures X.Y.Z; the optional 'prerelease' group
  #   captures the prerelease token. Both are combined into the OCX version
  #   string as X.Y.Z-prerelease before normalization.
  #
  # IMPORTANT: since skip_prereleases defaults to false (pre-releases included),
  # the pattern must match pre-release tag names too. If the default does not
  # cover the source's pre-release naming convention, override tag_pattern to
  # capture both stable and pre-release forms. A pre-release release that fails
  # to match tag_pattern is silently skipped — it will not be mirrored.
  tag_pattern: string

  # ── url_index fields ───────────────────────────────────────────────────
  # Exactly one of 'url' or 'versions' must be provided.

  # URL to a remote JSON file with the asset index.
  # Schema: see "url_index JSON schema" below.
  url: string

  # OR: inline the version → assets map directly.
  # Same shape as the remote JSON format.
  versions:
    "<version>":
      prerelease: boolean      # Optional. Default false.
      assets:
        "<asset-filename>": string  # e.g., "my-tool-1.2.0-linux-amd64.tar.gz": "https://..."

# Required. Asset selection rules — shared by ALL source types.
# Maps platform (os/arch) to an ORDERED LIST of asset filename regexes.
# ALL patterns in the list are applied against the full asset set for a version.
# Exactly one distinct asset must match across all patterns for a given platform.
# Zero matches → platform absent for this version (not an error; skipped silently).
# More than one distinct asset matched → error: ambiguous; mirror aborts this version.
assets:
  linux/amd64:
    - string               # e.g., "cmake-.*-linux-x86_64\\.tar\\.gz"
    - string               # additional pattern covering older naming convention
  linux/arm64:
    - string
  darwin/amd64:
    - string
  darwin/arm64:
    - string
  windows/amd64:
    - string

# Optional. Metadata configuration. Points to existing OCX metadata JSON files
# (same format as used by 'ocx package create'). Reuses the established format
# rather than duplicating it in YAML.
metadata:
  # Default metadata applied to all platforms not listed under platforms:.
  default: path/to/metadata.json
  # Per-platform overrides. Critical for cases like macOS where .app bundles
  # require different env var setup (e.g., PATH into CMake.app/Contents/bin).
  platforms:
    "<os/arch>": path/to/platform-specific-metadata.json

# Optional. Build timestamp format.
# The mirrored tag is always fully-specified: X.Y.Z+{timestamp} (non-rolling).
# Cascade then produces the rolling parents: X.Y.Z, X.Y, X, and optionally latest.
# The timestamp is always UTC. Valid characters for a build fragment: [0-9a-zA-Z]+
# (from version.rs — no separators other than none).
#
#   datetime  (default) — YYYYMMDDHHmmss  e.g. 20260310142359  (second granularity)
#   date                — YYYYMMDD         e.g. 20260310        (day granularity)
#
# Use 'date' only when a single mirror run per day is guaranteed and you prefer
# shorter, more readable tags. 'datetime' is safer for automated pipelines.
build_timestamp: datetime | date   # Default: datetime

# Optional. Controls cascade behavior when pushing.
cascade: boolean             # Default: true

# Optional. Version constraints.
versions:
  # Minimum version to mirror (inclusive). Older versions are skipped permanently.
  min: string                # e.g., "3.20.0"
  # Maximum version to mirror (inclusive). Newer versions are skipped permanently.
  max: string                # e.g., "4.0.0"
  # Maximum number of NEW (not yet mirrored) versions to upload in a single run.
  # Does NOT cap the total number of mirrored versions. Purpose: backfill large
  # histories incrementally across multiple scheduled runs without overloading
  # the registry or CI runner in one shot. Absent = no per-run limit.
  new_per_run: integer       # e.g., 10

# Optional. Pre-release filtering.
# Applies to all source types: github_release uses the API is_prerelease flag;
# url_index uses an optional 'prerelease: true' field per version entry.
# Both set is_prerelease on the canonical VersionInfo — the filter is source-agnostic.
# Default: false — pre-releases are included. Set to true to exclude them.
# Note: draft releases (is_draft: true in the GitHub API) are ALWAYS filtered regardless
# of this setting. Drafts are unpublished and must never be mirrored. This is not
# configurable. skip_prereleases controls only published pre-release entries.
skip_prereleases: boolean

# Optional. Download verification.
# Controls integrity checks on downloaded binaries before they are re-hosted.
verify:
  # Verify against the SHA256 digest returned by the GitHub Releases API for each asset.
  # Only applies to the github_release source type (the REST API exposes per-asset digests).
  # Default: true. Set to false only if targeting a source where digests are unavailable.
  github_asset_digest: boolean   # Default: true

  # Additionally verify against a sidecar checksum file attached to the release.
  # The value is a filename pattern that may reference the asset name and version.
  # Examples: "{asset}.sha256", "checksums.txt", "{name}-{version}-checksums.txt"
  # The sidecar is downloaded, parsed (sha256sum format: "HASH  FILENAME" lines),
  # and cross-checked against the downloaded file. If present and mismatch → abort.
  # Absent = no sidecar check. Verification fails if the declared sidecar is not found.
  checksums_file: string         # Optional. No default (sidecar check disabled).

# Optional. Concurrency and rate limiting.
concurrency:
  max_downloads: integer     # Default: 4. Max parallel downloads.
  max_pushes: integer        # Default: 2. Max parallel pushes.
  # Delay between GitHub Releases API pagination calls (ms). Helps stay under the
  # 5,000 req/hour authenticated rate limit for large repositories with many releases.
  # The adapter also reads X-RateLimit-Remaining and X-RateLimit-Reset response headers
  # and pauses automatically when remaining < 10, regardless of this setting.
  rate_limit_ms: integer     # Default: 0.
  # Maximum retry attempts for transient failures (download errors, registry 5xx, 429).
  # Uses exponential backoff starting at 1s, doubling up to 60s. Respects Retry-After
  # and X-RateLimit-Reset headers when present.
  max_retries: integer       # Default: 3.
```

### Example: GitHub Releases (CMake)

CMake is a good example of a real-world naming change: older releases used `cmake-3.x.y-Linux-x86_64.tar.gz` (capital L), newer releases use `cmake-3.x.y-linux-x86_64.tar.gz`. The asset pattern list handles this gracefully. macOS uses a universal binary that covers both amd64 and arm64 — the same URL for both platforms, but different metadata because macOS apps may have different env conventions.

```yaml
name: cmake
target:
  registry: ocx.sh
  repository: cmake

source:
  type: github_release
  owner: Kitware
  repo: CMake
  tag_pattern: "^v(?P<version>\\d+\\.\\d+\\.\\d+)$"

# All patterns are applied to the full asset list for each release.
# Exactly one match per platform is required; two matches = error.
# The list covers naming changes across versions — all patterns stay active.
assets:
  linux/amd64:
    - "cmake-.*-linux-x86_64\\.tar\\.gz"
    - "cmake-.*-Linux-x86_64\\.tar\\.gz"    # pre-3.25 naming
  linux/arm64:
    - "cmake-.*-linux-aarch64\\.tar\\.gz"
    - "cmake-.*-Linux-aarch64\\.tar\\.gz"
  darwin/amd64:
    - "cmake-.*-macos-universal\\.tar\\.gz"
    - "cmake-.*-Darwin-x86_64\\.tar\\.gz"   # pre-universal binary era
  darwin/arm64:
    - "cmake-.*-macos-universal\\.tar\\.gz"
  windows/amd64:
    - "cmake-.*-windows-x86_64\\.zip"
    - "cmake-.*-win64-x64\\.zip"

metadata:
  default: metadata/cmake.json           # linux, windows — bin/ layout, PATH only
  platforms:
    darwin/amd64: metadata/cmake-darwin.json
    darwin/arm64: metadata/cmake-darwin.json

cascade: true

versions:
  min: "3.20.0"
  new_per_run: 10   # backfill 10 versions per scheduled run
```

Corresponding `metadata/cmake.json` (standard OCX format, reused as-is):
```json
{
  "type": "bundle",
  "version": 1,
  "strip_components": 1,
  "env": [
    { "key": "PATH", "type": "path", "required": true, "value": "${installPath}/bin" }
  ]
}
```

`metadata/cmake-darwin.json` (macOS may need CMAKE_ROOT pointing elsewhere):
```json
{
  "type": "bundle",
  "version": 1,
  "strip_components": 1,
  "env": [
    { "key": "PATH", "type": "path", "required": true, "value": "${installPath}/CMake.app/Contents/bin" },
    { "key": "CMAKE_ROOT", "type": "constant", "value": "${installPath}/CMake.app/Contents/share/cmake-3" }
  ]
}
```

### Example: Generic URL Index (custom tool)

The `url_index` source uses the same asset-name → URL shape as GitHub Releases. Platform resolution uses the same spec-level `assets:` patterns. This means you can switch a mirror from `url_index` to `github_release` (or vice versa) without touching `assets:`, `metadata:`, or any other field.

```yaml
name: my-tool
target:
  registry: ghcr.io/myorg
  repository: tools/my-tool

source:
  type: url_index
  versions:
    "1.2.0":
      assets:
        "my-tool-1.2.0-linux-amd64.tar.gz":  "https://releases.example.com/my-tool/1.2.0/my-tool-linux-amd64.tar.gz"
        "my-tool-1.2.0-darwin-arm64.tar.gz":  "https://releases.example.com/my-tool/1.2.0/my-tool-darwin-arm64.tar.gz"
    "1.3.0":
      assets:
        "my-tool-1.3.0-linux-amd64.tar.gz":  "https://releases.example.com/my-tool/1.3.0/my-tool-linux-amd64.tar.gz"
        "my-tool-1.3.0-darwin-arm64.tar.gz":  "https://releases.example.com/my-tool/1.3.0/my-tool-darwin-arm64.tar.gz"
    "2.0.0-beta.1":
      prerelease: true
      assets:
        "my-tool-2.0.0-beta.1-linux-amd64.tar.gz": "https://releases.example.com/my-tool/2.0.0-beta.1/my-tool-2.0.0-beta.1-linux-amd64.tar.gz"

assets:
  linux/amd64:
    - "my-tool-.*-linux-amd64\\.tar\\.gz"
  darwin/arm64:
    - "my-tool-.*-darwin-arm64\\.tar\\.gz"

metadata:
  default: metadata/my-tool.json

cascade: true
```

The `url_index` source can also fetch its asset map from a remote JSON file:

```yaml
source:
  type: url_index
  url: "https://releases.example.com/my-tool/ocx-index.json"

assets:
  linux/amd64:
    - "my-tool-.*-linux-amd64\\.tar\\.gz"
  darwin/arm64:
    - "my-tool-.*-darwin-arm64\\.tar\\.gz"

metadata:
  default: metadata/my-tool.json
```

#### url_index JSON schema

```json
{
  "versions": {
    "1.2.0": {
      "assets": {
        "my-tool-1.2.0-linux-amd64.tar.gz": "https://...",
        "my-tool-1.2.0-darwin-arm64.tar.gz": "https://..."
      }
    },
    "2.0.0-beta.1": {
      "prerelease": true,
      "assets": {
        "my-tool-2.0.0-beta.1-linux-amd64.tar.gz": "https://..."
      }
    }
  }
}
```

Each version has an `assets` object (filename → URL) and an optional `prerelease` boolean (default false). `prerelease` is a sibling of `assets`, not inside it, keeping the two concerns clearly separated. This format is intentionally simple to generate from a script, CI job, or any language.

## Architecture

### System Overview

```
                                 Mirror Spec (YAML)
                                        |
                                        v
                              +-------------------+
                              |  ocx-mirror sync  |
                              +-------------------+
                                        |
                    +-------------------+-------------------+
                    |                                       |
                    v                                       v
            +---------------+                     +------------------+
            | Source Adapter |                     | Registry Checker |
            | (GitHub, URL) |                     | (already-mirrored|
            +---------------+                     |  detection)      |
                    |                             +------------------+
                    v                                       |
           Vec<VersionInfo>                                 |
                    |                                       v
                    +----------> Filter/Diff <--------------+
                                    |
                                    v
                          Vec<MirrorTask>
                                    |
                     +--------------+--------------+
                     |              |              |
                     v              v              v
               +-----------+  +-----------+  +-----------+
               | Pipeline  |  | Pipeline  |  | Pipeline  |
               | v1/linux  |  | v1/darwin |  | v2/linux  |
               +-----------+  +-----------+  +-----------+
                     |              |              |
              download -> extract -> package -> push
                     |              |              |
                     v              v              v
                        OCI Registry (target)
```

### Data Flow

1. **Parse** the mirror spec YAML into a typed `MirrorSpec` struct. Validate that all `assets:` patterns are valid regexes and that `tag_pattern` (for `github_release`) contains `(?P<version>...)`. Fail fast on invalid specs.
2. **Discover** available versions from the source adapter. Each version produces a `VersionInfo` with version string, raw named assets (filename → URL), and `is_prerelease` flag.
3. **Resolve assets** for each `VersionInfo`: apply all spec-level `assets:` patterns against each version's asset names. Exactly one asset must match per platform. Zero matches = platform absent (silently skipped for this version). More than one match = `Ambiguous` error: abort this version, log which assets matched which patterns, continue with other versions.
4. **Normalize versions**: for each `VersionInfo`, parse the extracted version string and pad/reject per the normalization table. Versions with a source-supplied build fragment are rejected with an error. Versions with major-only are rejected. `X.Y` → `X.Y.0`. All accepted versions receive the run-start build timestamp: `X.Y.Z+{timestamp}`.
5. **Filter** versions: drop pre-releases (if `skip_prereleases: true`), apply `min`/`max` version bounds (matched against the normalized `X.Y.Z` part, ignoring the build), subtract already-mirrored versions (tag-list set-diff against the target registry, matching on the rolling `X.Y.Z` tag since each run may produce a different build fragment), then apply `new_per_run` cap to the remainder.
5. **Execute** the pipeline for each remaining (version, platform) pair, with bounded concurrency.
6. **Cascade** after the primary tag push, copying the manifest to rolling tags.

### Version Normalization

Every version pushed by `ocx-mirror` must be a **fully-specified, non-rolling** OCX version: `major.minor.patch+build`. `Version::is_rolling()` returns `false` only when a build fragment is present — without it, `3.28.1` is a rolling tag, not a pinned one.

Normalization happens between asset resolution and pipeline execution, using the version string extracted by `tag_pattern` (or provided directly in `url_index`). The captured string is fed to `Version::parse()` first; the result determines which normalization rule applies.

| Extracted / parsed version | Action |
|---|---|
| `X` (major only) | **Error** — too ambiguous to pad safely; minor and patch unknown |
| `X.Y` (major.minor) | Normalize to `X.Y.0`; append build timestamp → `X.Y.0+{ts}` |
| `X.Y.Z` (full patch) | Append build timestamp → `X.Y.Z+{ts}` |
| `X.Y.Z-pre` (prerelease) | Append build timestamp → `X.Y.Z-pre+{ts}` |
| `X.Y.Z+build` | **Error** — source already carries a build fragment; cannot replace or append |

The pre-release form `X.Y.Z-pre` is produced when `tag_pattern` contains both a `version` and a `prerelease` named capture group. The resolver assembles the OCX version string as `{version}-{prerelease}` when both groups are present, or just `{version}` when `prerelease` is absent. For the default pattern applied to tag `v3.28.0-rc1`: `version` = `3.28.0`, `prerelease` = `rc1` → assembled string `3.28.0-rc1` → `Version::parse("3.28.0-rc1")` → pushed as `3.28.0-rc1+{ts}`. Note: the prerelease token must satisfy `[0-9a-zA-Z]+` — a single alphanumeric segment. Tokens like `rc.1` or `beta-2` do not parse and cause the version to be skipped with a warning.

The `is_prerelease` flag from the source (GitHub API or `url_index`) and the presence of a prerelease component in the parsed version are **independent**:
- Source says `is_prerelease: true`, tag matches pattern, parsed version has no prerelease component (e.g., tag `v4.0.0-nightly` captured as `4.0.0`) → pushed as `4.0.0+{ts}`; `is_prerelease` is only used for the `skip_prereleases` filter, not to inject a prerelease label.
- Source says `is_prerelease: false`, but tag pattern captures `3.28.0-rc1` → pushed as `3.28.0-rc1+{ts}`; the version string is authoritative.

The resulting pushed tag is always `X.Y.Z+{timestamp}` (or `X.Y.Z-pre+{timestamp}` for pre-releases). With cascade enabled, OCX then creates the rolling parent chain: `X.Y.Z+{ts}` → `X.Y.Z` → `X.Y` → `X` → `latest` (if newest).

**Build timestamp generation** (always UTC):

```rust
use chrono::Utc;

pub enum BuildTimestampFormat {
    /// YYYYMMDDHHmmss — second granularity. Default.
    DateTime,
    /// YYYYMMDD — day granularity.
    Date,
}

pub fn build_timestamp(format: BuildTimestampFormat) -> String {
    let now = Utc::now();
    match format {
        BuildTimestampFormat::DateTime => now.format("%Y%m%d%H%M%S").to_string(),
        BuildTimestampFormat::Date     => now.format("%Y%m%d").to_string(),
    }
}
```

Both formats produce only `[0-9]` digits, which satisfy the build fragment constraint `[0-9a-zA-Z]+` from `version.rs`. The `T` ISO separator is technically valid in a build fragment but avoided for clarity — pure digits are unambiguous and sort lexicographically by time.

**Single timestamp per run**: the timestamp is generated once when the mirror run starts, not per version. All versions mirrored in the same run share the same build fragment. This ensures that if the same run pushes `3.28.0` and `3.28.1`, their build tags reflect the same mirror event rather than slightly different wall-clock times.

### Key Data Types

```rust
/// A single version discovered from an upstream source.
///
/// This is the canonical internal representation shared by all source adapters.
/// Adapters produce raw named assets; platform resolution happens afterward
/// as a shared pipeline step using the spec-level `assets:` patterns.
pub struct VersionInfo {
    /// The version string (e.g., "3.28.0"). Used as the OCI tag.
    pub version: String,
    /// Raw named assets for this version: asset filename → download URL.
    /// Mirrors the GitHub Releases model exactly. Platform resolution
    /// (applying spec-level regex patterns to these names) is deferred.
    pub assets: HashMap<String, url::Url>,
    /// Per-asset integrity digests: asset filename → "sha256:HEXHASH".
    /// Populated by the GitHub adapter from the per-asset `digest` field
    /// (available since June 2025). Empty for url_index sources unless
    /// the index JSON provides digests explicitly.
    pub asset_digests: HashMap<String, String>,
    /// True if this release is a pre-release (alpha, beta, rc, nightly, etc.).
    /// Sourced from: GitHub API `is_prerelease` flag, or `prerelease: true` in
    /// a url_index version entry. Filtered out when `skip_prereleases: true`.
    /// Note: `is_draft` releases are filtered by the adapter before VersionInfo
    /// is created — they never appear here.
    pub is_prerelease: bool,
}

/// Asset resolution result for one version — produced by the shared resolver.
///
/// Errors here abort mirroring for this version (never silently skip).
pub enum AssetResolution {
    /// Exactly one asset matched for each platform that had any match.
    Resolved(HashMap<oci::Platform, url::Url>),
    /// One or more platforms matched more than one asset. Lists all conflicts.
    Ambiguous(Vec<AmbiguousAsset>),
}

pub struct AmbiguousAsset {
    pub platform: oci::Platform,
    pub matched_assets: Vec<String>,   // asset filenames that all matched
    pub matched_patterns: Vec<String>, // the patterns that caused the matches
}

/// A single unit of work: download + package + push one platform of one version.
pub struct MirrorTask {
    pub version: String,
    pub platform: oci::Platform,
    pub download_url: url::Url,
    pub target_identifier: oci::Identifier,
}

/// Result of executing a single MirrorTask.
pub enum MirrorResult {
    /// Successfully pushed to the registry.
    Pushed {
        version: String,
        platform: oci::Platform,
        digest: oci::Digest,
    },
    /// Already present in the registry, skipped.
    Skipped {
        version: String,
    },
    /// Failed with an error.
    Failed {
        version: String,
        platform: oci::Platform,
        error: String,
    },
}
```

## Source Adapters

### Trait Definition

```rust
/// A source of upstream binary releases that can be mirrored into an OCX registry.
///
/// Implementations must be cheaply cloneable (they will be shared across
/// concurrent tasks) and safe to call from multiple threads.
///
/// Note: uses native async-in-trait (stable since Rust 1.75 / Rust edition 2024).
/// The `async_trait` macro is not used.
pub trait Source: Send + Sync {
    /// Discovers all available versions from the upstream source.
    ///
    /// Returns versions in no particular order. Filtering (min/max/limit)
    /// is applied by the caller after this method returns.
    async fn list_versions(&self) -> Result<Vec<VersionInfo>>;

    /// Returns a human-readable name for this source (used in logging).
    fn name(&self) -> &str;
}
```

### GitHub Releases Adapter

The adapter's only responsibility is fetching the release list and mapping it to `VersionInfo`. It does **not** resolve platforms — that is the shared resolver's job.

```rust
pub struct GitHubReleaseSource {
    owner: String,
    repo: String,
    /// Compiled from spec `tag_pattern`. Must contain a named capture group `version`.
    tag_pattern: regex::Regex,
    client: reqwest::Client,
    rate_limit_ms: u64,
}

#[async_trait]
impl Source for GitHubReleaseSource {
    async fn list_versions(&self) -> Result<Vec<VersionInfo>> {
        // 1. Paginate through GET /repos/{owner}/{repo}/releases
        //    using Accept: application/vnd.github+json
        //    and optional GITHUB_TOKEN for higher rate limits.
        //
        // 2. For each release:
        //    a. Apply tag_pattern to tag_name. Skip if no match.
        //       Extract version string from capture group `version`.
        //    b. Collect all release assets into a HashMap<String, Url>:
        //       asset.name -> asset.browser_download_url.
        //    c. Copy release.is_prerelease to VersionInfo::is_prerelease.
        //    d. Emit VersionInfo { version, assets, is_prerelease }.
        //       No platform resolution here — just raw asset names.
        //
        // 3. Sleep rate_limit_ms between paginated API calls.
        todo!()
    }

    fn name(&self) -> &str {
        "github_release"
    }
}
```

Key design points:

- **Authentication**: Reads `GITHUB_TOKEN` env var. Without it, the GitHub REST API rate limit is 60 requests/hour per source IP (GitHub tightened unauthenticated limits in May 2025). With a token: 5,000 requests/hour. `GITHUB_TOKEN` is effectively required for any mirror covering more than a handful of versions. The adapter reads `X-RateLimit-Remaining` and `X-RateLimit-Reset` headers and pauses automatically when the budget is nearly exhausted, regardless of `rate_limit_ms`.
- **Pagination**: GitHub returns up to 100 releases per page. The adapter paginates using `Link` response headers (`rel="next"` / `rel="last"`), not by constructing `page=N` URLs manually, to remain correct if the API changes pagination behaviour.
- **Draft filtering**: Releases with `is_draft: true` are silently dropped before emitting `VersionInfo`. Draft releases are unpublished; mirroring them would expose content that the upstream author has not yet made public. This filter is always-on and not configurable — it is not the same as `skip_prereleases`.
- **Raw assets, no platform resolution**: The adapter emits all assets as-is. Platform resolution (which asset matches which platform) is handled by the shared resolver in the next pipeline step, using the spec-level `assets:` patterns.
- **Per-asset SHA256 digest**: As of June 2025, the GitHub Releases API returns a `digest` field (`sha256:HEXHASH`) on each release asset object. The adapter stores this alongside the download URL in an extended `VersionInfo.asset_digests: HashMap<String, String>` map (asset name → digest string). The pipeline uses this to verify each download before packaging, when `verify.github_asset_digest` is enabled (the default).
- **Pre-release handling**: `release.is_prerelease` is copied directly to `VersionInfo::is_prerelease`. The adapter always reports pre-releases; the pipeline filter decides whether to use them based on `skip_prereleases`.
- **Version extraction**: `tag_pattern` must contain `(?P<version>...)`. Validated at spec parse time.

### URL Index Adapter

The adapter produces the same `VersionInfo` shape as the GitHub adapter — raw named assets, no platform resolution.

```rust
pub struct UrlIndexSource {
    /// Pre-populated at construction time (from inline spec or fetched remote JSON).
    versions: Vec<VersionInfo>,
}

#[async_trait]
impl Source for UrlIndexSource {
    async fn list_versions(&self) -> Result<Vec<VersionInfo>> {
        // Already loaded at construction — just return the pre-built list.
        Ok(self.versions.clone())
    }

    fn name(&self) -> &str {
        "url_index"
    }
}
```

When `url:` is provided, the adapter fetches the remote JSON at construction time and deserializes it into `Vec<VersionInfo>`. Each version entry has an `assets` sub-object (filename → URL) and an optional `prerelease` boolean sibling. The `prerelease` field is stored in `VersionInfo::is_prerelease`; the `assets` sub-object becomes `VersionInfo::assets`. Neither field bleeds into the other. The resulting `VersionInfo` is identical in structure to what the GitHub adapter produces, so both flow through the same shared resolver.

### Shared Asset Resolver

The resolver sits between the source adapter and the pipeline. It is not a trait — it is a plain function called once per `VersionInfo`:

```rust
/// Resolves raw named assets to per-platform download URLs.
///
/// Applies ALL patterns for each platform against ALL asset names in the version.
/// Returns Resolved if each platform matched exactly one asset,
/// or Ambiguous listing every conflicting (platform, asset, pattern) triple.
pub fn resolve_assets(
    assets: &HashMap<String, url::Url>,
    patterns: &HashMap<oci::Platform, Vec<regex::Regex>>,
) -> AssetResolution {
    let mut resolved: HashMap<oci::Platform, url::Url> = HashMap::new();
    let mut ambiguous: Vec<AmbiguousAsset> = Vec::new();

    for (platform, regexes) in patterns {
        let mut matches: Vec<(String, String)> = Vec::new(); // (asset_name, pattern)

        for regex in regexes {
            for (asset_name, url) in assets {
                if regex.is_match(asset_name) {
                    matches.push((asset_name.clone(), regex.as_str().to_string()));
                }
            }
        }

        // Deduplicate by asset name (same asset matched by multiple patterns is fine).
        let unique_urls: HashMap<&str, &url::Url> = matches.iter()
            .map(|(name, _)| (name.as_str(), assets.get(name).unwrap()))
            .collect();

        match unique_urls.len() {
            0 => {}  // Platform absent for this version — silently skip.
            1 => { resolved.insert(platform.clone(), (*unique_urls.values().next().unwrap()).clone()); }
            _ => {
                ambiguous.push(AmbiguousAsset {
                    platform: platform.clone(),
                    matched_assets: unique_urls.keys().map(|s| s.to_string()).collect(),
                    matched_patterns: matches.iter().map(|(_, p)| p.clone()).collect(),
                });
            }
        }
    }

    if ambiguous.is_empty() {
        AssetResolution::Resolved(resolved)
    } else {
        AssetResolution::Ambiguous(ambiguous)
    }
}
```

**Ambiguity rule**: the same asset matching multiple patterns for the same platform is **not** an error — the patterns are all equivalent aliases for the same file. Two *different* assets matching patterns for the same platform **is** an error. The deduplication step (grouping by asset name before counting) enforces this: multiple pattern hits on the same filename collapse to one, but multiple distinct filenames remain distinct.

### Future Adapters

New adapters implement the `Source` trait. Likely candidates:

- **Cargo crates** (binary crates with pre-built releases)
- **npm packages** (published tarballs)
- **HashiCorp Releases** (structured release API at `releases.hashicorp.com`)
- **Direct URL template** (URL with `{version}`, `{os}`, `{arch}` placeholders)

Adding a new adapter requires:
1. Implement `Source` for the new type.
2. Add a new variant to the `source:` section of the mirror spec.
3. Add deserialization in the spec parser.

No changes to the pipeline, registry checker, or cascade logic.

## Already-Mirrored Detection

### Algorithm

**Option B: Fetch tag list once, set-diff.** This is the chosen approach.

```
1. client.list_tags(target_identifier) -> Vec<String>
2. Parse each tag as a Version.
3. For each VersionInfo from the source:
     if version string is in the tag set -> skip (already mirrored)
     else -> include in the work list
```

### Rationale

Option A (check each tag individually via `fetch_manifest_digest`) requires N API calls where N is the number of candidate versions. For a tool like CMake with 50+ versions, this is 50+ HEAD requests to the registry.

Option B requires exactly 1 paginated API call to `list_tags`, then does an in-memory set difference. This is:

- **Faster**: One API call vs. N. Tag listing is paginated but typically returns all tags in 1-2 pages.
- **Cheaper**: Fewer registry API calls, lower risk of rate limiting.
- **Sufficient**: We only need to know whether a tag exists, not its digest. The idempotency guarantee comes from the fact that `push_package` + `update_image_index` replaces the platform entry atomically (see `index.manifests.retain(|entry| entry.platform != platform)` in `Client::update_image_index`).

### Edge Case: Partial Platform Mirrors

A version may be tagged in the registry but only have a subset of platforms (e.g., the previous mirror run was interrupted after pushing `linux/amd64` but before `darwin/arm64`). The tag-list approach would skip this version entirely, silently leaving the image index incomplete.

**Mitigation (always-on)**: For each version found in the tag-list set-diff (i.e., the tag already exists), fetch its image index manifest and compare the platform set against the platforms declared in the spec. If any declared platforms are missing, add those (version, platform) pairs back to the work list. This requires exactly one extra API call per already-present version — the number of already-present versions grows over time, but so does the set of tasks being skipped, so the overhead is proportional.

This check is always enabled. A CLI flag `--skip-platform-check` can disable it for speed when the operator knows the registry is consistent (e.g., a freshly seeded registry with no interrupted runs). The default is to always check.

`--version 3.28.0` still bypasses the already-mirrored check entirely for the specified version (useful for force re-push after registry corruption).

## Pipeline Design

### Per-Task Pipeline

Each `MirrorTask` (one version + one platform) executes this pipeline:

```
1. DOWNLOAD
   - HTTP GET the download_url into a temp file.
   - Validate: non-zero size, HTTP 200.
   - Temp file path: {work_dir}/{version}/{platform_slug}/download.{ext}
   - Retry up to max_retries on transient failures (5xx, timeout, 429).
     Respects Retry-After and X-RateLimit-Reset headers. Exponential backoff
     starting at 1s, doubling up to 60s.

2. VERIFY (if verify.* enabled in spec)
   - If verify.github_asset_digest: true (default):
       SHA256 the downloaded file; compare against VersionInfo.asset_digests[asset_name].
       Abort this task with error on mismatch.
   - If verify.checksums_file is set:
       Download the sidecar file (same retry logic), parse sha256sum format,
       look up the asset filename, compare SHA256. Abort on mismatch or missing entry.
   - Both checks may be active simultaneously. Either failure aborts.

3. EXTRACT (if needed)
   - If the downloaded file is an archive (tar.gz, tar.xz, zip):
     extract to {work_dir}/{version}/{platform_slug}/content/
   - If it is a single binary: place it in content/bin/{name}
   - Apply strip_components from the metadata template.

4. WRITE METADATA
   - Render the metadata template into content/../metadata.json
   - The metadata is identical for all platforms of the same package.

5. PACKAGE
   - Call ocx_lib::package::bundle::BundleBuilder::from_path(content_dir)
       .create(bundle_path)
   - Produces a .tar.xz bundle.

6. PUSH
   - Build an ocx_lib::package::info::Info {
       identifier: target_identifier,
       metadata: parsed_metadata,
       platform: task.platform,
     }
   - Call client.push_package(info, bundle_path) -> (Digest, Manifest)
   - This pushes the blob, config, image manifest, and updates the image index.
   - Attach OCI annotations to the manifest (see OCI Annotations below).
   - Retry up to max_retries on transient registry errors (5xx, 429).

7. CASCADE
   - If cascade is enabled:
     a. Parse the version string.
     b. Compute cascade targets via Version::cascade(all_versions),
        where all_versions = existing_tags ∪ current_run_versions (see Cascade Correctness).
     c. For each cascade tag + "latest" (if is_latest):
        client.copy_manifest_data(&manifest, &source_id, cascade_tag)
```

### OCI Annotations

All manifests pushed by `ocx-mirror` carry the following standard OCI annotations
(defined in OCI Image Spec v1.1 `annotations.md`):

| Annotation | Value |
|---|---|
| `org.opencontainers.image.source` | Upstream release URL (GitHub release page or asset URL) |
| `org.opencontainers.image.version` | Upstream version string (e.g., `3.28.0`) |
| `org.opencontainers.image.created` | Mirror run start timestamp in RFC 3339 format |
| `org.opencontainers.image.title` | Package name from the spec |

These annotations enable auditability: any consumer of the OCI artifact can discover its upstream origin and when it was mirrored using `skopeo inspect` or registry UIs, without consulting the mirror spec file.

The manifest should also set `artifactType: application/vnd.ocx.binary.v1` to distinguish binary artifacts from container images in registries that display artifact type (GHCR, Harbor, Zot).

### Cascade Correctness

The cascade step must know the full universe of versions to correctly determine cascade targets and `latest`. Using only `existing_tags` (fetched before the run) is incorrect when a single run pushes multiple versions: the cascade for `3.28.0` must know that `3.28.1` is also being pushed in this run, or it may incorrectly promote `3.28.0` to `latest`.

**Fix**: Before dispatching any tasks, compute `all_versions = existing_tags ∪ current_run_versions` (the full sorted version list, including both what was already in the registry and what this run will push). Pass `all_versions` into each cascade call. The already-mirrored filter uses only `existing_tags`; the cascade target computation uses `all_versions`.

Note: `existing_tags` is fetched once at the start of the run and cached. It is intentionally not re-fetched during the run — doing so would introduce a race between concurrent cascade operations. The union approach is correct because `current_run_versions` is fully known before any tasks start.

### Library Calls, Not Shell-Outs

The pipeline calls `ocx_lib` functions directly:

- `package::bundle::BundleBuilder::from_path().create()` for bundling
- `Client::push_package()` for pushing
- `Client::copy_manifest_data()` for cascade
- `Client::list_tags()` for already-mirrored detection
- `Version::cascade()` for computing cascade targets

This avoids the overhead and fragility of shelling out to the `ocx` binary. It also avoids requiring `ocx` to be installed or on `PATH`.

### Parallelism

```
                    Semaphore(max_downloads)
                           |
    +----------+-----------+-----------+----------+
    | download | download  | download  | download |   (bounded)
    +----------+-----------+-----------+----------+
         |          |           |           |
         v          v           v           v
      package    package     package     package      (CPU-bound, unbounded)
         |          |           |           |
         v          v           v           v
    +--------+--------+--------+--------+
    |  push  |  push  |  push  |  push  |             (bounded)
    +--------+--------+--------+--------+
                    Semaphore(max_pushes)
```

Implementation uses `tokio::sync::Semaphore` for bounding concurrent downloads and pushes. The package step (compression) is CPU-bound but fast relative to I/O, so it is not separately bounded.

All tasks for a single version across all platforms are grouped. Cascade happens after all platforms of a version are pushed, because the cascade copies the image index (which includes all platform entries).

```rust
// Pseudocode for the execution loop
let download_sem = Arc::new(Semaphore::new(spec.concurrency.max_downloads));
let push_sem = Arc::new(Semaphore::new(spec.concurrency.max_pushes));

// Group tasks by version for cascade ordering.
// IMPORTANT: must use semantic version order (not lexicographic string order).
// "10.0.0" < "9.0.0" lexicographically — BTreeMap<String, _> is wrong for version keys.
// Use a newtype wrapper with Ord derived from Version's semantic comparison,
// or collect into a Vec and sort with a semantic comparator before iterating.
let tasks_by_version: Vec<(Version, Vec<MirrorTask>)> = group_by_version_semantic(tasks);

let mut join_set = JoinSet::new();

for (version, platform_tasks) in tasks_by_version {
    let download_sem = download_sem.clone();
    let push_sem = push_sem.clone();
    let client = client.clone();
    let spec = spec.clone();

    join_set.spawn(async move {
        // Push all platforms for this version.
        let mut last_manifest = None;
        let mut source_identifier = None;

        for task in platform_tasks {
            // Download (bounded)
            let _download_permit = download_sem.acquire().await?;
            let archive_path = download(&task).await?;
            drop(_download_permit);

            // Package (unbounded, CPU)
            let bundle_path = package(&task, &archive_path, &spec.metadata).await?;

            // Push (bounded)
            let _push_permit = push_sem.acquire().await?;
            let (digest, manifest) = client
                .push_package(task.build_info(&spec.metadata), &bundle_path)
                .await?;
            drop(_push_permit);

            last_manifest = Some(manifest);
            source_identifier = Some(task.target_identifier.clone_with_digest(digest));
        }

        // Cascade after all platforms are pushed.
        if spec.cascade {
            if let (Some(manifest), Some(source_id)) = (last_manifest, source_identifier) {
                // Pass all_versions (existing ∪ current run) for correct cascade targeting.
                cascade(&client, &manifest, &source_id, &version, &all_versions).await?;
            }
        }

        Ok::<_, Error>(version)
    });
}

// Collect results.
while let Some(result) = join_set.join_next().await { ... }
```

### Shared URL Deduplication

Multiple platforms may map to the same download URL. The canonical example is a universal binary (e.g., `cmake-macos-universal.tar.gz`) declared for both `darwin/amd64` and `darwin/arm64`. Without deduplication, the pipeline would download, verify, and extract the archive twice.

Before dispatching tasks, group `MirrorTask`s for a given version by `download_url`. Tasks that share a URL share a single download+verify+extract step; each then runs its own package+push step from the same extracted content directory. The content directory is treated as read-only after extraction so multiple concurrent package tasks can operate on it safely.

```
download_url → content_dir  (1:1, done once)
platform     → bundle_path  (N:1 content_dir, done per platform)
```

This is significant for tools that publish universal binaries or single-archive multi-platform distributions.

### Temp Directory Management

Each mirror run uses a top-level temp directory (default: system temp, overridable via `--work-dir`). Structure:

```
{work_dir}/ocx-mirror-{run_id}/
  {version}/
    {platform_slug}/
      download.tar.gz       # raw download
      content/               # extracted content
      bundle.tar.xz         # packaged bundle
      metadata.json          # rendered metadata
```

The entire `ocx-mirror-{run_id}` directory is cleaned up on successful completion. On failure, it is retained for debugging (with a log message showing the path).

## GitHub Actions Integration

### Recommended Approach: Document the CLI

Rather than building a custom GitHub Action (which adds maintenance burden and a JavaScript/TypeScript dependency), document how to use `ocx-mirror` directly in a workflow. The CLI is a single static binary, easy to install.

### Example Workflow

```yaml
name: Mirror CMake releases
on:
  schedule:
    - cron: '0 6 * * 1'   # Weekly, Monday 6:00 UTC
  workflow_dispatch:        # Manual trigger

permissions:
  packages: write           # For GHCR push

jobs:
  mirror:
    runs-on: ubuntu-latest
    steps:
      - name: Checkout mirror specs
        uses: actions/checkout@v4

      - name: Install ocx-mirror
        run: |
          curl -fsSL https://ocx.sh/install-mirror.sh | sh
          echo "$HOME/.ocx/bin" >> "$GITHUB_PATH"

      - name: Sync all packages
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          OCX_AUTH_GHCR_IO_USERNAME: ${{ github.actor }}
          OCX_AUTH_GHCR_IO_PASSWORD: ${{ secrets.GITHUB_TOKEN }}
        run: |
          ocx-mirror sync-all mirrors/ --verbose
```

`sync-all` runs all specs in the `mirrors/` directory sequentially (one per package). For independent packages with no shared registry rate limit concerns, `--parallel N` can speed up the run at the cost of multiplied concurrency toward the registry. Sequential is the safe default for scheduled runs targeting a shared registry.

### Future: Official Action

If adoption warrants it, an `ocx-sh/mirror-action` composite action can wrap the CLI:

```yaml
# action.yml (hypothetical)
name: 'OCX Mirror'
description: 'Mirror binary releases into an OCX registry'
inputs:
  spec:
    description: 'Path to mirror spec YAML'
    required: true
  version:
    description: 'ocx-mirror version to install'
    default: 'latest'
runs:
  using: composite
  steps:
    - run: curl -fsSL https://ocx.sh/install-mirror.sh | sh
      shell: bash
    - run: ocx-mirror sync ${{ inputs.spec }} --verbose
      shell: bash
```

This is deferred until the CLI stabilizes.

## CLI Design

### Commands

```
ocx-mirror sync <spec.yaml> [options]     # Primary command: run a mirror sync
ocx-mirror sync-all <dir> [options]       # Sync all mirror-*.yaml specs in a directory
ocx-mirror check <spec.yaml> [options]    # Dry-run: show what would be mirrored
ocx-mirror validate <spec.yaml>           # Validate spec file syntax
```

`sync-all` globs `mirror-*.yaml` in the given directory and runs each spec sequentially (or in parallel with `--parallel N`). It is the primary CI entry point for organizations with multiple packages: a single step instead of one step per package, with a unified exit code and summary report. Sequential by default to avoid registry rate limiting; `--parallel N` enables parallel syncs up to N concurrently.

### Flags for `sync` and `check`

```
--verbose, -v           Increase log verbosity (repeatable)
--version <ver>         Mirror only this specific version (bypasses already-mirrored check)
--platform <os/arch>    Mirror only this specific platform
--skip-platform-check   Skip image index platform completeness check for already-present
                        versions (faster, but silently ignores partially-mirrored versions)
--work-dir <path>       Override temp working directory
--dry-run               Same as `check` -- show plan without executing
--format json           Machine-readable output
```

### Exit Codes

| Code | Meaning |
|------|---------|
| 0    | All tasks succeeded (or nothing to do) |
| 1    | One or more tasks failed (partial success) |
| 2    | Spec file invalid or unreadable |
| 3    | Authentication failure |

## Crate Structure

### Workspace Layout

```
crates/
  ocx_lib/              # Existing. No changes needed.
  ocx_cli/              # Existing. No changes needed.
  ocx_mirror/           # NEW. Binary crate.
    Cargo.toml
    src/
      main.rs           # Entry point, clap CLI definition
      spec.rs           # MirrorSpec YAML deserialization
      source.rs         # Source trait + VersionInfo
      source/
        github_release.rs
        url_index.rs
      verify.rs         # Download integrity checks (GitHub digest, sidecar checksums)
      annotations.rs    # OCI annotation construction
      pipeline.rs       # Download -> verify -> package -> push pipeline
      registry.rs       # Already-mirrored detection, platform completeness, cascade logic
      error.rs          # Error types
```

### Why a Separate Binary Crate (Not a Subcommand)

1. **Dependency divergence**: `ocx-mirror` needs `reqwest` (HTTP client for downloading release assets and GitHub API), `serde_yml` (spec parsing), and `regex` (asset pattern matching). The main `ocx` binary has none of these. Adding them would bloat `ocx` for every user, when only mirror operators need them.

2. **Distribution scope**: `ocx` is installed on every developer machine and CI runner. `ocx-mirror` is run by a single CI job per organization. Different distribution targets, different binary sizes.

3. **Release cadence**: Mirror spec format and source adapters will evolve faster than the core `ocx` package manager. A separate binary can release independently.

4. **Single responsibility**: `ocx` is a package manager (consumer). `ocx-mirror` is a package publisher (producer). These are distinct roles.

### Why NOT a Separate Library Crate

The mirror logic does not need to be consumed as a library by other Rust code. All reusable logic (OCI push, bundle creation, version cascade) already lives in `ocx_lib`. The mirror-specific code (spec parsing, source adapters, pipeline orchestration) is application logic, not library logic. If a library boundary becomes necessary later, it can be extracted from the binary crate with minimal refactoring.

### Cargo.toml

```toml
[package]
name = "ocx_mirror"
edition = "2024"
license.workspace = true

[[bin]]
name = "ocx-mirror"
path = "src/main.rs"

[dependencies]
ocx_lib = { path = "../ocx_lib" }
tokio = { workspace = true }
clap = { version = "4", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
serde_yml = "0.0.12"     # maintained fork of deprecated serde_yaml; drop-in compatible
serde_json = "1"
reqwest = { version = "0.12", features = ["json", "stream"] }
regex = "1"
url = { version = "2", features = ["serde"] }
anyhow = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
tempfile = "3"
```

## Deferred: Version Translation

Some upstream projects use version schemes that differ from OCX's semver-inspired tags. Examples:

- **Date-based**: `2024.03.11` (Go nightly builds, some rolling releases)
- **Prefixed non-numeric**: `release-20240311`, `node-v20.0.0` where the desired OCI tag is `20.0.0`
- **Non-standard separators**: `3_28_0`

The `tag_pattern` named capture group already handles the common cases of prefix stripping (capture only the numeric part). This covers ~95% of real-world tools.

Full version translation — mapping one version scheme to a different OCI tag format — is **deferred**. When needed, it can be added as a `version_map:` field in the spec that provides an explicit `upstream_tag → ocx_tag` mapping, or as a simple `tag_transform:` regex substitution. Neither is designed here because the escape hatch is straightforward: generate a `url_index` JSON with already-normalized version keys. For non-standard sources, a small script that produces the canonical JSON is simpler and more maintainable than a general-purpose transformation DSL in the spec format.

## Trade-offs

### Chose: Tag-list set-diff for already-mirrored detection

**Over**: Per-tag `fetch_manifest_digest` calls.

**Why**: One API call scales to any number of versions. The trade-off is that a partially-mirrored version (some platforms pushed, others not) appears as "already mirrored" and is skipped. This is acceptable because: (a) partial mirrors are rare (only on interrupted runs), (b) `--check-platforms` handles the rare case, and (c) `--version X` forces re-push.

### Chose: Separate binary crate

**Over**: Subcommand of `ocx`, or a completely separate repository.

**Why**: Subcommand bloats `ocx` with mirror-only dependencies. Separate repository duplicates build infrastructure and makes it harder to stay in sync with `ocx_lib` API changes. A workspace member crate is the sweet spot: same build, same CI, shared `ocx_lib`, independent binary.

### Chose: Direct `ocx_lib` calls

**Over**: Shelling out to the `ocx` binary.

**Why**: Library calls are faster (no process spawn), more reliable (no PATH dependency), provide better error handling (typed errors vs. parsing stderr), and allow sharing the OCI client connection. The cost is tighter coupling to `ocx_lib` internals, but since both crates live in the same workspace, this coupling is manageable and caught by the compiler.

### Chose: YAML for mirror specs; metadata stays as JSON files

**Over**: Inlining metadata in the YAML spec.

**Why**: YAML is the right format for human-authored, commented configuration. But metadata.json already has a well-defined OCX format, is already used by `ocx package create`, and may be shared across tools. Pointing to existing JSON files from the YAML spec reuses that investment — no duplication, no translation layer. Per-platform metadata pointers (e.g., darwin needing a different PATH pointing into a `.app` bundle) are handled by the `platforms:` section.

### Chose: Asset patterns at spec top-level (shared by all source types)

**Over**: Asset patterns inside each source type config.

**Why**: Both `github_release` and `url_index` produce the same `VersionInfo` shape (raw named assets). Putting patterns inside the source block would duplicate them if the source type changes, and would require each adapter to implement its own resolution logic. A single shared resolver with a single patterns declaration is DRY, testable in isolation, and ensures consistent ambiguity detection regardless of source.

### Chose: All patterns applied, exactly one asset per platform (ambiguity = error)

**Over**: Ordered list with first-match-wins.

**Why**: First-match-wins silently hides the case where two patterns both match different assets — an operator mistake that would cause the wrong binary to be pushed for some versions. Applying all patterns and erroring on multiple distinct matching assets makes misconfigurations visible immediately rather than silently mirroring the wrong file. The list order still matters for documentation clarity but has no operational effect.

### Chose: Asset patterns as list (multiple patterns per platform)

**Over**: Single pattern per platform.

**Why**: Upstream projects routinely rename assets between major versions (e.g., CMake changed `Linux` → `linux` in the archive filename). A single regex either becomes a complex alternation or silently misses older releases. A list of patterns, all applied, covers naming drift across versions without modifying existing entries.

### Chose: Named capture group for version extraction (`tag_pattern`)

**Over**: `strip_tag_prefix` string substitution.

**Why**: A prefix-strip only handles `v1.2.3` → `1.2.3`. Real projects use conventions like `release/1.2.3`, `tool-v1.2.3`, `2024.03.11`, or `v1.2.3-stable`. A regex with `(?P<version>...)` handles all of these uniformly, with no special cases.

### Chose: Default `tag_pattern` includes optional prerelease suffix

**Over**: Default matching only `X.Y.Z` (stable).

**Why**: `skip_prereleases` defaults to false — pre-releases are included by default. A default pattern that only matches stable tags would silently skip every pre-release tag without explanation. The extended default, with a separate `(?P<prerelease>[0-9a-zA-Z]+)?` named group, matches both `v3.28.0` and `v3.28.0-rc1` with no user action, and makes the prerelease component explicit rather than embedded in the `version` capture. Pre-releases with non-standard suffixes (dots, hyphens in prerelease token) are skipped with a warning, requiring an explicit `tag_pattern` override.

### Chose: `new_per_run` (not a permanent version cap)

**Over**: A `limit` that caps total mirrored versions.

**Why**: A permanent cap would mean historical versions beyond the limit are never mirrored, even after the cap is raised. `new_per_run` is a rate-limiter on each individual run — it allows backfilling a large release history incrementally across multiple scheduled runs without overwhelming the registry or CI runner in one shot. All versions satisfying `min`/`max` will eventually be mirrored; `new_per_run` just controls the pace.

### Chose: One spec file per package

**Over**: A single file listing all packages.

**Why**: Each package has different source configuration, asset patterns, and metadata. Separate files are easier to review in PRs, can be owned by different teams, and avoid merge conflicts. A future `ocx-mirror sync-all mirrors/` command can glob all spec files in a directory.

### Chose: `type:` discriminant field in `source:`

**Over**: Separate `github_release:` / `url_index:` sibling keys.

**Why**: A `type:` discriminant maps cleanly to a serde `#[serde(tag = "type")]` enum. It reads as "this source IS a github_release" rather than "this source HAS a github_release sub-object". It also prevents the ambiguity of having two source-type keys present simultaneously.

### Chose: Always push `X.Y.Z+{timestamp}` as the primary tag

**Over**: Pushing `X.Y.Z` directly (no build fragment).

**Why**: `X.Y.Z` without a build is a rolling tag in OCX (`Version::is_rolling()` = true). Rolling tags can be silently overwritten by the next cascade. The build-tagged version `X.Y.Z+{timestamp}` is non-rolling and content-addressed — it permanently records *when* this binary was mirrored. This supports audit trails, pinning to a specific mirror event, and the full cascade chain (build → patch → minor → major → latest). The downside is slightly longer tag names, but the rolling `X.Y.Z` tag is still created via cascade.

### Chose: Single timestamp per run (not per version)

**Over**: Generating a new timestamp for each version individually.

**Why**: A single run timestamp makes it obvious that a set of build-tagged versions all came from the same mirror event. It also means `ocx-mirror` can be run multiple times and produce distinct build tags each time (useful for re-mirroring after a registry incident). Per-version timestamps would produce near-identical values and make it harder to correlate which versions were pushed together.

### Chose: Error on major-only versions; pad `X.Y` to `X.Y.0`

**Over**: Padding all partial versions, or erroring on all non-patch versions.

**Why**: `X.Y.0` is an unambiguous, conventional interpretation of a two-part version. `X.0.0` for a major-only version is not — a major version tag (e.g., `release-3`) could mean any patch of major 3, and silently assigning 0.0 would produce misleading OCI tags. Erroring on major-only is the safe choice; the spec author must fix `tag_pattern` to capture the correct granularity.

### Chose: `skip_prereleases` (default false) over `include_prereleases` (default false)

**Why**: All boolean flags default to false. A flag named `include_prereleases` with default false is confusing — the name implies an affirmative action but the default silently applies a filter. `skip_prereleases: true` is an explicit opt-in to filtering: enabling it *does something* (skips pre-releases). Disabled (default) = no filtering applied = all versions included. This convention makes every boolean flag in the spec a conscious activation, never a silent default behavior.

### Chose: Document CLI usage in Actions (no custom action yet)

**Over**: Building an `ocx-sh/mirror-action` immediately.

**Why**: A composite action is trivial to add later but premature now. The CLI interface is still being designed and will change. A custom action adds a maintenance surface (action.yml, versioning, README, marketplace listing) with minimal value over `run: ocx-mirror sync-all mirrors/`.

### Chose: GitHub native asset digest as default verification; sidecar as optional second layer

**Over**: No verification (too risky), or requiring cosign/SLSA (too high an adoption bar).

**Why**: As of June 2025, the GitHub Releases REST API returns a `digest: sha256:HEXHASH` field on every asset object — the same API call the adapter already makes. This is free, requires no publisher action, and covers the MITM and CDN tampering threat. A sidecar checksum file (`checksums.txt`, `{asset}.sha256`) is a common convention (GoReleaser, HashiCorp, many open source projects) and adds a second layer for projects that publish them. Both can be active simultaneously. SLSA/cosign attestation verification is not required because adoption remains low and adding it as a mandatory step would break most mirrors today; it may be added as an optional layer later.

### Chose: Always-on platform completeness check; `--skip-platform-check` to opt out

**Over**: Opt-in `--check-platforms` flag.

**Why**: A version that appears complete in the tag list but is missing platforms is silently broken — consumers on the missing platforms get a resolution error. Defaulting to correctness (always check) means interrupted runs are automatically repaired on the next scheduled run. The extra API cost (one manifest fetch per already-present version) is proportional and bounded: as the registry grows, so does the set of tasks being skipped, and the check is a lightweight HEAD/GET per existing tag.

### Chose: Semantic version sort for task ordering and cascade

**Over**: Lexicographic sort (BTreeMap<String, ...>).

**Why**: `"10.0.0" < "9.0.0"` lexicographically. Any tool with a major version ≥ 10 (Node.js, many others) would get incorrect cascade ordering and wrong `latest` detection under a string sort. The `Version` type already implements semantic comparison; using it as the sort key is correct and costs nothing.

### Chose: Share download across platforms with same URL (deduplication)

**Over**: Unconditional per-platform download.

**Why**: Universal binaries (macOS arm64 + amd64 from the same `.tar.gz`, single-archive cross-platform distributions) are common. Downloading the same URL twice wastes bandwidth and doubles extraction time and disk use. Grouping tasks by `download_url` and sharing the extracted content directory across packaging steps is a straightforward optimization with no correctness risk (content is read-only after extraction).

### Chose: Native async-in-trait over `async_trait` macro

**Over**: `#[async_trait]` from the `async-trait` crate.

**Why**: `async fn` in traits has been stable since Rust 1.75 (December 2023). The workspace already targets Rust edition 2024. Using the `async_trait` macro adds an unnecessary dependency and generates less efficient code (boxing every future). The native form is idiomatic for any Rust 2024 codebase.

### Chose: `serde_yml` over `serde_yaml`

**Over**: `serde_yaml = "0.9"`.

**Why**: `serde_yaml` was officially deprecated by its author in 2023 and the repository is archived. It has known correctness issues with YAML anchors, merge keys, and duplicate keys. `serde_yml` is a community-maintained drop-in fork with the same API surface, actively maintained, and is the de facto recommended migration path.

## Consequences

### What This Enables

- **Automated package onboarding**: New tools can be mirrored with a single YAML file and a CI schedule. No manual download-package-push loop.
- **Community contributions**: Mirror specs can live in a public repository (e.g., `ocx-sh/mirrors`). Contributors add a YAML file; CI does the rest.
- **Registry population at scale**: The `ocx.sh` default registry can be populated with hundreds of tools programmatically.
- **Reproducible mirroring**: The spec file is the single source of truth. Re-running it produces the same result (idempotent).

### What This Constrains

- **`ocx_lib` API stability**: The mirror crate depends on `Client::push_package`, `Client::copy_manifest_data`, `Client::list_tags`, `BundleBuilder`, `Version::cascade`, `package::info::Info`, `oci::Platform`, and the annotation/manifest extension points added for OCI annotation support. Changes to these APIs require updating the mirror crate in the same commit.
- **`ocx_lib` annotation support**: Pushing OCI annotations requires `Client::push_package` to accept a map of annotation key-value pairs (or a manifest builder that supports annotations). If the current API does not support this, a targeted extension is needed before `ocx-mirror` can attach `org.opencontainers.image.*` annotations. This is a minor change but must be coordinated.
- **GitHub API dependency**: The primary source adapter depends on the GitHub Releases API. Rate limits (60/hour unauthenticated, 5000/hour with token) constrain how many packages can be mirrored in a single run without a token.
- **No automatic format detection**: The pipeline does not auto-detect whether a downloaded file is a tar.gz with a top-level directory (requiring `strip_components: 1`) or a flat archive. The spec author must know this and set `strip_components` correctly. A future improvement could add auto-detection heuristics.

### Risks

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| GitHub API rate limiting blocks large mirrors | Medium | Medium | `GITHUB_TOKEN` required for any meaningful use (documented). Adapter respects `X-RateLimit-Remaining` / `X-RateLimit-Reset` headers and pauses proactively. Configurable `rate_limit_ms` between pagination calls. |
| Upstream release naming changes break asset patterns | Medium | Low | Per-version; other versions unaffected. Regex list handles naming drift across versions. |
| Compromised upstream release asset | Low | High | GitHub native asset digest check (default on) detects tampering. Optional sidecar checksum verification as second layer. |
| ocx_lib API churn breaks mirror crate | Low | Medium | Same workspace; compiler catches breaks at the call sites. |
| Large downloads exhaust disk in CI | Low | Medium | Shared URL deduplication reduces redundant downloads. Clean up per-version temp directories on success. Document expected disk use (e.g., cmake-3.x: ~50 MB per platform). |
| Registry push rate limiting (Docker Hub, GHCR) | Medium | Medium | Configurable `max_pushes` semaphore. Retry with exponential backoff (1s → 60s). Respects `Retry-After` header. |
| Partial platform state after interrupted run | Low | Medium | Always-on platform completeness check repairs missing platforms on the next run automatically. |
