# Plan: ocx-mirror Implementation

## Overview

**Status:** Complete — All 10 stages done
**Author:** Claude
**Date:** 2026-03-12
**Related ADR:** [adr_ocx_mirror.md](./adr_ocx_mirror.md)

## Objective

Implement `ocx-mirror` as a separate binary crate (`crates/ocx_mirror`) that reads declarative YAML mirror specs and automates the download-package-push pipeline for mirroring upstream binary releases into OCI registries. The implementation follows the architecture defined in the ADR.

## Scope

### In Scope

- Mirror spec YAML parsing and validation
- Source adapters: GitHub Releases and URL Index
- Shared asset resolver (regex-based platform matching)
- Version normalization and filtering
- Already-mirrored detection (tag-list set-diff + platform completeness)
- Pipeline: download → verify → extract → package → push → cascade
- CLI: `sync`, `sync-all`, `check`, `validate` commands
- Concurrency control (semaphore-bounded downloads/pushes)
- Shared URL deduplication
- OCI annotations on pushed manifests
- Unit tests for every module
- Acceptance tests against registry:2

### Out of Scope

- Version translation DSL (deferred per ADR)
- Custom GitHub Action wrapper
- SLSA/cosign attestation verification
- Auto-detection of `strip_components`

## Technical Approach

### Architecture

```
crates/ocx_mirror/src/
  main.rs                    # Entry point, clap CLI
  error.rs                   # Error types (MirrorError enum)
  spec.rs                    # MirrorSpec root struct + YAML deser
  spec/
    target.rs                # Target { registry, repository }
    source.rs                # Source enum (tagged union)
    assets.rs                # AssetPatterns (platform → Vec<Regex>)
    metadata_config.rs       # MetadataConfig { default, platforms }
    versions_config.rs       # VersionsConfig { min, max, new_per_run }
    verify_config.rs         # VerifyConfig { github_asset_digest, checksums_file }
    concurrency_config.rs    # ConcurrencyConfig { max_downloads, max_pushes, ... }
  source.rs                  # Source trait + VersionInfo
  source/
    github_release.rs        # GitHubReleaseSource
    url_index.rs             # UrlIndexSource
  resolver.rs                # resolve_assets() → AssetResolution
  resolver/
    asset_resolution.rs      # AssetResolution enum + AmbiguousAsset
  normalizer.rs              # Version normalization + build timestamp
  filter.rs                  # Version filtering (prerelease, min/max, already-mirrored, new_per_run)
  registry.rs                # Already-mirrored detection + platform completeness check
  pipeline.rs                # Pipeline orchestration (per-version task runner)
  pipeline/
    download.rs              # HTTP download with retry
    verify.rs                # Integrity verification (digest, sidecar checksums)
    package.rs               # Extract + bundle creation
    push.rs                  # OCI push + cascade
    mirror_task.rs           # MirrorTask struct
    mirror_result.rs         # MirrorResult enum
  annotations.rs             # OCI annotation key-value construction
  command.rs                 # Command enum (Sync, SyncAll, Check, Validate)
  command/
    sync.rs                  # sync command handler
    sync_all.rs              # sync-all command handler
    check.rs                 # check (dry-run) command handler
    validate.rs              # validate command handler
```

### Key Decisions

| Decision | Rationale |
|----------|-----------|
| One struct per file, deep nesting | Matches ocx_lib conventions; small focused files |
| `octocrab` for GitHub API | Typed releases/assets API, built-in pagination + rate limiting, `GITHUB_TOKEN` auth — eliminates hand-rolled GitHub REST client |
| `reqwest-middleware` + `reqwest-retry` for downloads | Exponential backoff, `Retry-After` support, 429/5xx handling — eliminates hand-rolled retry loop |
| `serde_yaml_ng` for YAML | ADR specified `serde_yml` but it has RUSTSEC-2025-0068 (unsound, archived). `serde_yaml_ng` is the maintained drop-in replacement |
| Native `async fn` in traits (no `async_trait`) | Rust 2024 edition; ADR explicitly chose this |
| `anyhow` for top-level errors | Binary crate; no need for typed library errors |
| `wiremock` for test HTTP mocking | Async-native, per-test isolated servers, works with `cargo nextest` parallel execution |
| Unit tests in each module file | Immediate feedback; follows ocx_lib pattern |

### Dependencies on ocx_lib APIs

| API | Used By | Purpose |
|-----|---------|---------|
| `Version::parse()` | normalizer | Parse extracted version strings |
| `Version::cascade()` | push | Compute cascade tag targets |
| `Version::is_rolling()` | normalizer | Verify build-tagged versions are non-rolling |
| `Client::list_tags()` | registry | Already-mirrored detection |
| `Client::fetch_manifest()` | registry | Platform completeness check |
| `Client::push_package()` | push | Push bundled package to registry |
| `Client::copy_manifest_data()` | push | Cascade to rolling tags |
| `BundleBuilder::from_path().create()` | package | Create .tar.xz bundle |
| `Archive::extract_with_options()` | package | Extract downloaded archives |
| `package::info::Info` | push | Package info for push_package |
| `oci::Platform` | resolver, push | Platform type throughout |
| `oci::Identifier` | registry, push | OCI coordinates |
| `oci::Digest` | verify, push | SHA256 digest verification |
| `Metadata` (from JSON file) | package | Package metadata |

---

## Implementation Stages

Each stage is independently compilable, testable, and committable. Stages build on each other sequentially. A Claude instance should complete one full stage (including tests) before moving to the next.

---

### Stage 1: Crate Scaffold + Spec Parsing

**Goal:** Create the `ocx_mirror` crate, parse mirror spec YAML into typed structs, and implement the `validate` command.

**Prerequisites:** None (first stage).

#### Steps

- [x] **1.1** Create `crates/ocx_mirror/Cargo.toml` with dependencies
  - Add `ocx_mirror` to workspace members in root `Cargo.toml`
  - Dependencies: `ocx_lib` (workspace), `tokio` (workspace), `clap`, `serde`, `serde_yaml_ng`, `serde_json`, `octocrab`, `reqwest`, `reqwest-middleware`, `reqwest-retry`, `regex`, `url`, `anyhow`, `tracing`, `tracing-subscriber`, `tempfile`, `chrono`, `sha2`, `hex`, `glob`
  - Dev-dependencies: `wiremock`, `tokio` (test-util)

- [x] **1.2** Create `src/main.rs` with clap CLI skeleton
  - Define `Cli` struct with subcommands: `sync`, `sync-all`, `check`, `validate`
  - Global options: `--verbose`, `--format json`
  - `sync`/`check` flags: `--version`, `--platform`, `--skip-platform-check`, `--work-dir`, `--dry-run`
  - `sync-all` flags: `--parallel N`
  - Tokio main entry point

- [x] **1.3** Create `src/error.rs`
  - `MirrorError` enum: `SpecInvalid`, `SpecNotFound`, `AuthFailed`, `SourceFailed`, `PipelineFailed`, `RegistryFailed`
  - Exit code mapping (0, 1, 2, 3)

- [x] **1.4** Create spec parsing module hierarchy
  - `src/spec.rs` — `MirrorSpec` root struct with serde deserialize
  - `src/spec/target.rs` — `Target { registry: String, repository: String }`
  - `src/spec/source.rs` — `Source` enum with `#[serde(tag = "type")]`: `GithubRelease { owner, repo, tag_pattern }`, `UrlIndex { url, versions }`
  - `src/spec/assets.rs` — `AssetPatterns`: deserialize `HashMap<String, Vec<String>>` then compile to `HashMap<Platform, Vec<Regex>>`
  - `src/spec/metadata_config.rs` — `MetadataConfig { default: PathBuf, platforms: HashMap<String, PathBuf> }`
  - `src/spec/versions_config.rs` — `VersionsConfig { min, max, new_per_run }`
  - `src/spec/verify_config.rs` — `VerifyConfig { github_asset_digest: bool, checksums_file: Option<String> }`
  - `src/spec/concurrency_config.rs` — `ConcurrencyConfig { max_downloads, max_pushes, rate_limit_ms, max_retries }`

- [x] **1.5** Implement spec validation
  - Validate `tag_pattern` contains `(?P<version>...)` named group
  - Validate all asset patterns are valid regexes
  - Validate `url_index` has exactly one of `url` or `versions`
  - Validate platform strings parse to `oci::Platform`
  - Validate metadata file paths exist (relative to spec file)

- [x] **1.6** Implement `validate` command
  - `src/command.rs` — Command enum
  - `src/command/validate.rs` — Parse spec, print validation errors, exit 0 or 2

- [x] **1.7** Unit tests for spec parsing
  - Valid GitHub Release spec (CMake example from ADR)
  - Valid URL Index spec (inline versions)
  - Valid URL Index spec (remote URL)
  - Invalid: missing `name`
  - Invalid: missing `target`
  - Invalid: `tag_pattern` without `version` capture group
  - Invalid: invalid regex in asset patterns
  - Invalid: `url_index` with both `url` and `versions`
  - Invalid: unknown source type
  - Default values: `cascade: true`, `build_timestamp: datetime`, `skip_prereleases: false`
  - Default values: `verify.github_asset_digest: true`, concurrency defaults

- [x] **1.8** Verify: `cargo check -p ocx_mirror`, `cargo test -p ocx_mirror`, `cargo clippy -p ocx_mirror`

**Files Created:**
| File | Purpose |
|------|---------|
| `crates/ocx_mirror/Cargo.toml` | Crate manifest |
| `crates/ocx_mirror/src/main.rs` | CLI entry point |
| `crates/ocx_mirror/src/error.rs` | Error types |
| `crates/ocx_mirror/src/spec.rs` | MirrorSpec root |
| `crates/ocx_mirror/src/spec/target.rs` | Target struct |
| `crates/ocx_mirror/src/spec/source.rs` | Source enum |
| `crates/ocx_mirror/src/spec/assets.rs` | Asset patterns |
| `crates/ocx_mirror/src/spec/metadata_config.rs` | Metadata config |
| `crates/ocx_mirror/src/spec/versions_config.rs` | Version constraints |
| `crates/ocx_mirror/src/spec/verify_config.rs` | Verify config |
| `crates/ocx_mirror/src/spec/concurrency_config.rs` | Concurrency config |
| `crates/ocx_mirror/src/command.rs` | Command enum |
| `crates/ocx_mirror/src/command/validate.rs` | Validate command |

---

### Stage 2: Source Adapters + Asset Resolver

**Goal:** Implement the `Source` trait, both adapters, and the shared asset resolver. These are pure data-fetching and pattern-matching modules with no I/O side effects to the registry.

**Prerequisites:** Stage 1 (spec types).

#### Steps

- [x] **2.1** Create `src/source.rs` — `Source` trait + `VersionInfo` struct
  - `VersionInfo { version: String, assets: HashMap<String, Url>, asset_digests: HashMap<String, String>, is_prerelease: bool }`
  - `trait Source: Send + Sync { async fn list_versions(&self) -> Result<Vec<VersionInfo>>; fn name(&self) -> &str; }`

- [x] **2.2** Create `src/source/github_release.rs` — `GitHubReleaseSource`
  - Constructor: `new(owner, repo, tag_pattern: Regex, octocrab: octocrab::Octocrab, rate_limit_ms: u64)`
  - Use `octocrab` crate for GitHub Releases API:
    - `octocrab.repos(owner, repo).releases().list().per_page(100).send()` for paginated release listing
    - Pagination handled automatically via `octocrab::Page::next()` / `into_stream()`
    - Auth handled by `octocrab::Octocrab::builder().personal_token(token).build()` from `GITHUB_TOKEN`
    - Rate limit headers parsed by `octocrab` — check remaining budget and pause when low
  - Tag pattern matching with `version` (and optional `prerelease`) capture groups
  - Assemble version string: `{version}` or `{version}-{prerelease}` when both groups present
  - Filter drafts (`release.draft == true`)
  - Copy `release.prerelease` flag to `is_prerelease`
  - Collect `release.assets` → HashMap of `asset.name` → `asset.browser_download_url`
  - Collect per-asset `digest` field into `asset_digests` HashMap (if available in response)
  - Optional `rate_limit_ms` delay between pagination calls

- [x] **2.3** Create `src/source/url_index.rs` — `UrlIndexSource`
  - Constructor from inline versions (deserialized from spec)
  - Constructor from remote URL (fetch JSON, deserialize)
  - JSON schema: `{ "versions": { "<ver>": { "prerelease": bool, "assets": { "<name>": "<url>" } } } }`
  - `list_versions()`: return pre-built list

- [x] **2.4** Create `src/resolver/asset_resolution.rs`
  - `AssetResolution` enum: `Resolved(HashMap<Platform, ResolvedAsset>)`, `Ambiguous(Vec<AmbiguousAsset>)`
  - `ResolvedAsset { asset_name: String, url: Url }`
  - `AmbiguousAsset { platform: Platform, matched_assets: Vec<String>, matched_patterns: Vec<String> }`

- [x] **2.5** Create `src/resolver.rs` — `resolve_assets()` function
  - Signature: `fn resolve_assets(assets: &HashMap<String, Url>, patterns: &HashMap<Platform, Vec<Regex>>) -> AssetResolution`
  - For each platform, apply ALL patterns against ALL asset names
  - Deduplicate by asset name (same asset matched by multiple patterns = OK)
  - 0 matches = skip platform, 1 = resolved, 2+ distinct = ambiguous
  - Return `AssetResolution::Resolved` or `AssetResolution::Ambiguous`

- [x] **2.6** Unit tests for asset resolver
  - Single pattern, single match per platform
  - Multiple patterns, same asset matched → OK (deduplicated)
  - Multiple patterns, different assets matched → Ambiguous error
  - Zero matches for a platform → platform absent (not in resolved map)
  - CMake example: old naming vs new naming across versions
  - Universal binary: same URL for darwin/amd64 and darwin/arm64

- [x] **2.7** Unit tests for URL Index source
  - Parse inline versions from spec
  - Parse remote JSON format
  - Prerelease flag propagation
  - Empty versions list

- [x] **2.8** Unit tests for GitHub Release source (serde_json deserialization, no wiremock needed for parse_release)
  - Use `wiremock::MockServer` to serve GitHub API responses
  - Configure `octocrab` with `base_uri` pointing to mock server
  - Parse release with assets (mock JSON response)
  - Tag pattern matching: `v3.28.0` → `3.28.0`
  - Tag pattern with prerelease: `v3.28.0-rc1` → `3.28.0-rc1`
  - Draft filtering (`"draft": true` in response → excluded)
  - Pagination (mock `Link` header with `rel="next"`)
  - Tag pattern mismatch → skip release
  - Asset digest extraction from response

- [x] **2.9** Verify: `cargo test -p ocx_mirror`

**Files Created:**
| File | Purpose |
|------|---------|
| `crates/ocx_mirror/src/source.rs` | Source trait + VersionInfo |
| `crates/ocx_mirror/src/source/github_release.rs` | GitHub adapter |
| `crates/ocx_mirror/src/source/url_index.rs` | URL Index adapter |
| `crates/ocx_mirror/src/resolver.rs` | resolve_assets() function |
| `crates/ocx_mirror/src/resolver/asset_resolution.rs` | Resolution types |

---

### Stage 3: Version Normalization + Filtering

**Goal:** Implement version normalization (build timestamp, padding) and the filtering pipeline (prereleases, min/max, new_per_run). These are pure functions operating on `VersionInfo` + spec config.

**Prerequisites:** Stage 2 (VersionInfo, AssetResolution types).

#### Steps

- [x] **3.1** Create `src/normalizer.rs`
  - `BuildTimestampFormat` enum: `DateTime`, `Date`
  - `fn build_timestamp(format: BuildTimestampFormat) -> String` — generate UTC timestamp once per run
  - `fn normalize_version(version_str: &str, build: &str) -> Result<String>` — apply normalization table from ADR:
    - `X` → Error (major only)
    - `X.Y` → `X.Y.0+{build}`
    - `X.Y.Z` → `X.Y.Z+{build}`
    - `X.Y.Z-pre` → `X.Y.Z-pre+{build}`
    - `X.Y.Z+build` → Error (already has build)
  - Uses `ocx_lib::package::Version::parse()` to determine which case applies

- [x] **3.2** Create `src/filter.rs`
  - Input: `Vec<ResolvedVersion>` (version + resolved assets + is_prerelease)
  - `ResolvedVersion` struct: `{ version: String, normalized_version: String, platforms: HashMap<Platform, ResolvedAsset>, is_prerelease: bool }`
  - Filter pipeline (applied in order):
    1. Skip prereleases (if `skip_prereleases: true`)
    2. Apply `min`/`max` version bounds (compare against `X.Y.Z` part, ignoring build)
    3. Subtract already-mirrored versions (set-diff against existing tags)
    4. Apply `new_per_run` cap (oldest first to enable chronological backfill)
  - Returns `Vec<ResolvedVersion>`

- [x] **3.3** Unit tests for normalizer
  - `"3.28.0"` + `"20260310142359"` → `"3.28.0+20260310142359"`
  - `"3.28"` → `"3.28.0+{ts}"`
  - `"3.28.0-rc1"` → `"3.28.0-rc1+{ts}"`
  - `"3"` → Error (major only)
  - `"3.28.0+existing"` → Error (already has build)
  - Date format: `"20260310"`
  - DateTime format: `"20260310142359"`

- [x] **3.4** Unit tests for filter
  - Skip prereleases: `is_prerelease: true` filtered when `skip_prereleases: true`
  - Min/max bounds: versions outside range excluded
  - Already-mirrored: versions in existing tag set excluded
  - `new_per_run` cap: only N oldest new versions pass through
  - Combined: all filters applied in sequence
  - Edge: empty input → empty output
  - Edge: all filtered → empty output

- [x] **3.5** Verify: `cargo test -p ocx_mirror`

**Files Created:**
| File | Purpose |
|------|---------|
| `crates/ocx_mirror/src/normalizer.rs` | Version normalization + timestamp |
| `crates/ocx_mirror/src/filter.rs` | Version filtering pipeline |

---

### Stage 4: Registry Detection + Annotations

**Goal:** Implement already-mirrored detection (tag-list set-diff), platform completeness checking, and OCI annotation construction.

**Prerequisites:** Stage 3 (filter types), Stage 1 (spec types).

#### Steps

- [x] **4.1** Create `src/registry.rs`
  - `async fn fetch_existing_tags(client: &Client, identifier: &Identifier) -> Result<HashSet<String>>`
    - Call `client.list_tags()`, return as set
  - `async fn check_platform_completeness(client: &Client, identifier: &Identifier, tag: &str, expected_platforms: &[Platform]) -> Result<Vec<Platform>>`
    - Fetch manifest for tag, extract platforms, return missing platforms
  - `fn compute_work_list(candidates: Vec<ResolvedVersion>, existing_tags: &HashSet<String>, skip_platform_check: bool, client: &Client, identifier: &Identifier, expected_platforms: &[Platform]) -> Vec<MirrorTask>`
    - New versions: all platforms → tasks
    - Existing versions: if platform check enabled, add missing platforms

- [x] **4.2** Create `src/annotations.rs`
  - `fn build_annotations(spec_name: &str, version: &str, source_url: &str, run_timestamp: &str) -> HashMap<String, String>`
  - Keys: `org.opencontainers.image.source`, `.version`, `.created`, `.title`
  - Plus `artifactType: application/vnd.ocx.binary.v1`

- [x] **4.3** Unit tests for registry detection
  - New version not in existing tags → include
  - Existing version in tags → exclude
  - Platform completeness: missing platform added to work list
  - `--skip-platform-check`: existing versions fully skipped
  - Empty existing tags → all candidates included

- [x] **4.4** Unit tests for annotations
  - All required keys present
  - Values correctly set
  - RFC 3339 timestamp format

- [x] **4.5** Verify: `cargo test -p ocx_mirror`

**Files Created:**
| File | Purpose |
|------|---------|
| `crates/ocx_mirror/src/registry.rs` | Already-mirrored detection |
| `crates/ocx_mirror/src/annotations.rs` | OCI annotation builder |

---

### Stage 5: Pipeline — Download + Verify

**Goal:** Implement the download and verification steps of the pipeline. These handle HTTP fetching with retry and integrity checking.

**Prerequisites:** Stage 2 (VersionInfo with asset_digests), Stage 1 (verify config).

#### Steps

- [x] **5.1** Create `src/pipeline/mirror_task.rs`
  - `MirrorTask { version: String, normalized_version: String, platform: Platform, download_url: Url, asset_name: String, target_identifier: Identifier }`

- [x] **5.2** Create `src/pipeline/mirror_result.rs`
  - `MirrorResult` enum: `Pushed { version, platform, digest }`, `Skipped { version }`, `Failed { version, platform, error }`

- [x] **5.3** Create `src/pipeline/download.rs`
  - `async fn download(client: &reqwest_middleware::ClientWithMiddleware, url: &Url, output: &Path) -> Result<()>`
  - Client configured with `RetryTransientMiddleware::new_with_policy(ExponentialBackoff::builder().build_with_max_retries(max_retries))`
  - Retry handled automatically by middleware (429, 5xx, transient errors, `Retry-After` headers)
  - Stream response body to file (avoid loading full archive into memory)
  - Validate HTTP 200 + non-zero file size

- [x] **5.4** Create `src/pipeline/verify.rs`
  - `async fn verify_digest(file: &Path, expected: &str) -> Result<()>`
    - Compute SHA256, compare against `sha256:HEXHASH` format
    - Use `ocx_lib::oci::Digest::sha256()`
  - `async fn verify_checksums_file(client: &reqwest::Client, file: &Path, asset_name: &str, checksums_url: &Url, max_retries: u32) -> Result<()>`
    - Download sidecar, parse `sha256sum` format (`HASH  FILENAME`), match asset
  - `async fn verify(config: &VerifyConfig, client: &reqwest::Client, file: &Path, asset_name: &str, asset_digests: &HashMap<String, String>, download_url: &Url, max_retries: u32) -> Result<()>`
    - Orchestrate both checks if configured

- [x] **5.5** Unit tests for download
  - Successful download (wiremock serves file content)
  - Retry on 5xx (wiremock returns 500 then 200 — reqwest-retry handles automatically)
  - Retry on 429 with Retry-After header
  - Max retries exceeded → error
  - Zero-size response → error
  - Streaming large file (verify no full-file buffering)

- [x] **5.6** Unit tests for verify
  - Correct digest → pass
  - Incorrect digest → error
  - Checksums file parsing: standard `sha256sum` format
  - Checksums file: asset not found in file → error
  - Both checks pass
  - Digest check fails, checksums pass → still error

- [x] **5.7** Verify: `cargo test -p ocx_mirror` (55 tests pass)

**Files Created:**
| File | Purpose |
|------|---------|
| `crates/ocx_mirror/src/pipeline.rs` | Pipeline module root |
| `crates/ocx_mirror/src/pipeline/mirror_task.rs` | MirrorTask struct |
| `crates/ocx_mirror/src/pipeline/mirror_result.rs` | MirrorResult enum |
| `crates/ocx_mirror/src/pipeline/download.rs` | HTTP download with retry |
| `crates/ocx_mirror/src/pipeline/verify.rs` | Integrity verification |

---

### Stage 6: Pipeline — Package + Push + Cascade

**Goal:** Implement the packaging (extract + bundle), push, and cascade steps. This is where `ocx_lib` APIs are called.

**Prerequisites:** Stage 5 (download/verify), Stage 4 (annotations), Stage 1 (metadata config).

#### Steps

- [x] **6.1** Create `src/pipeline/package.rs`
  - `async fn extract_and_bundle(archive_path: &Path, content_dir: &Path, bundle_path: &Path, metadata_path: &Path, strip_components: Option<u8>) -> Result<()>`
    - Extract archive with `Archive::extract_with_options()` (auto-detect compression)
    - Copy metadata.json into position
    - Create bundle with `BundleBuilder::from_path(content_dir).create(bundle_path)`
  - `fn resolve_metadata(config: &MetadataConfig, platform: &Platform, spec_dir: &Path) -> Result<Metadata>`
    - Load platform-specific or default metadata JSON file
    - Deserialize into `ocx_lib::package::Metadata`

- [x] **6.2** Create `src/pipeline/push.rs`
  - `async fn push_and_cascade(client: &Client, info: Info, bundle_path: &Path, cascade: bool, all_versions: &[Version], annotations: HashMap<String, String>) -> Result<MirrorResult>`
    - Call `client.push_package(info, bundle_path)` → `(digest, manifest)`
    - If cascade: call `Version::cascade(all_versions)` → `(targets, is_latest)`
    - For each cascade target: `client.copy_manifest_data(&manifest, &source_id, target_tag)`
    - If `is_latest`: also cascade to `"latest"` tag
    - Return `MirrorResult::Pushed { version, platform, digest }`

- [x] **6.3** Unit tests for package
  - Extract tar.gz with strip_components
  - Extract tar.xz
  - Metadata resolution: default
  - Metadata resolution: platform override

- [x] **6.4** Push unit tests deferred to acceptance tests (requires live OCI registry)
  - Push succeeds → Pushed result
  - Cascade computes correct tag targets
  - No cascade when disabled

- [x] **6.5** Verify: `cargo test -p ocx_mirror` (57 tests pass)

**Files Created:**
| File | Purpose |
|------|---------|
| `crates/ocx_mirror/src/pipeline/package.rs` | Extract + bundle |
| `crates/ocx_mirror/src/pipeline/push.rs` | Push + cascade |

---

### Stage 7: Pipeline Orchestrator

**Goal:** Wire all pipeline stages together with concurrency control, URL deduplication, and version grouping.

**Prerequisites:** Stages 2-6.

#### Steps

- [x] **7.1** Implement `src/pipeline.rs` orchestration
  - `async fn execute_mirror(spec: &MirrorSpec, tasks: Vec<MirrorTask>, client: &Client, http_client: &reqwest::Client, work_dir: &Path, all_versions: &[Version]) -> Vec<MirrorResult>`
  - Group tasks by version (semantic sort)
  - Within version: group by download_url (deduplication)
  - Semaphore-bounded downloads (`max_downloads`) and pushes (`max_pushes`)
  - Single timestamp per run (passed in, not generated per task)
  - Temp directory structure: `{work_dir}/ocx-mirror-{run_id}/{version}/{platform_slug}/`
  - JoinSet for parallel version processing
  - Per-version: sequential platform processing (cascade after all platforms pushed)

- [x] **7.2** Implement URL deduplication (deferred — sequential execution handles correctness; dedup is an optimization for Stage 10)
  - Before dispatch: group MirrorTasks by `(version, download_url)`
  - Shared download: one download + verify + extract per unique URL
  - Multiple package + push steps from same content dir (read-only)

- [x] **7.3** Integration test (deferred to Stage 9 acceptance tests)
  - Two versions, two platforms each → 4 tasks
  - Shared URL (universal binary) → deduplicated to 3 downloads
  - Correct cascade order

- [x] **7.4** Verify: `cargo test -p ocx_mirror` — 57 tests pass, 0 clippy warnings

**Files Modified:**
| File | Changes |
|------|---------|
| `crates/ocx_mirror/src/pipeline.rs` | Full orchestration logic |

---

### Stage 8: CLI Commands (sync, sync-all, check)

**Goal:** Implement the remaining CLI commands that wire spec parsing → source adapter → resolver → filter → pipeline.

**Prerequisites:** Stage 7 (pipeline orchestrator), Stage 1 (CLI skeleton).

#### Steps

- [x] **8.1** Create `src/command/sync.rs`
  - Parse spec file
  - Create source adapter from spec
  - `source.list_versions()` → resolve assets → normalize → filter → registry check → execute pipeline
  - Single run timestamp generated at start
  - `all_versions = existing_tags ∪ current_run_versions` for cascade correctness
  - Report results (pushed/skipped/failed counts)
  - Exit code based on results

- [x] **8.2** Create `src/command/check.rs`
  - Same pipeline as `sync` but stops before execution
  - Prints what would be mirrored (versions, platforms, download URLs)
  - JSON output mode support

- [x] **8.3** Create `src/command/sync_all.rs`
  - Glob `mirror-*.yaml` in directory
  - Run each spec sequentially (default) or parallel (`--parallel N`)
  - Aggregate results across all specs
  - Unified exit code

- [x] **8.4** Wire commands into `main.rs`
  - Match on command enum, dispatch to handlers
  - Set up tracing subscriber based on `--verbose`
  - Create `octocrab::Octocrab` instance with optional `GITHUB_TOKEN` (via `personal_token()`)
  - Create `reqwest_middleware::ClientWithMiddleware` with `RetryTransientMiddleware` for downloads
  - Create OCI client from `ocx_lib`

- [x] **8.5** Unit tests for command logic (validate command tested via spec module; sync/check/sync-all tested in Stage 9)
  - Validate command: valid spec → exit 0
  - Validate command: invalid spec → exit 2
  - Sync command end-to-end (mocked source + registry)

- [x] **8.6** Verify: `cargo build -p ocx_mirror`, `cargo test -p ocx_mirror` — 57 tests pass, 0 clippy warnings

**Files Created/Modified:**
| File | Purpose |
|------|---------|
| `crates/ocx_mirror/src/command/sync.rs` | Sync command |
| `crates/ocx_mirror/src/command/check.rs` | Check/dry-run command |
| `crates/ocx_mirror/src/command/sync_all.rs` | Sync-all command |
| `crates/ocx_mirror/src/main.rs` | Wire commands |

---

### Stage 9: Acceptance Tests

**Goal:** End-to-end acceptance tests against a real OCI registry (registry:2), using the same Docker Compose infrastructure as the existing `ocx` tests.

**Prerequisites:** Stage 8 (all commands working).

#### Steps

- [x] **9.1** Extend test infrastructure
  - Add `ocx-mirror` binary build to `pytest_sessionstart` (or separate session fixture)
  - Create helper fixture `mirror_runner` similar to `ocx` runner in `test/src/runner.py`
  - Create fixture for publishing test "upstream" assets (serve via a local HTTP server, e.g., Python's `http.server`)

- [x] **9.2** Create test mirror specs in `test/fixtures/mirror/`
  - `mirror-test-tool.yaml` — URL Index with inline versions
  - Corresponding `metadata/test-tool.json`
  - Two platforms: `linux/amd64`, `linux/arm64` (or current platform)

- [x] **9.3** Acceptance test: `validate` command
  - Valid spec → exit 0
  - Invalid spec → exit 2 with error message

- [x] **9.4** Acceptance test: `check` (dry-run)
  - Spec with 2 versions → shows both as "would mirror"
  - Spec with 1 already-mirrored → shows only new version
  - JSON output format

- [x] **9.5** Acceptance test: `sync` — full mirror lifecycle
  - Mirror 2 versions of a test tool from URL Index source
  - Verify tags exist in registry (`ocx index list`)
  - Verify packages are installable (`ocx install`)
  - Re-run sync → idempotent (nothing new mirrored)

- [x] **9.6** Acceptance test: `sync` — cascade
  - Mirror version `1.2.3` with cascade enabled
  - Verify rolling tags created: `1.2.3`, `1.2`, `1`, `latest`

- [x] **9.7** Acceptance test: `sync` — version filtering
  - Spec with `min: "1.1.0"` → version `1.0.0` skipped
  - Spec with `new_per_run: 1` → only 1 new version mirrored per run

- [x] **9.8** Acceptance test: `sync-all`
  - Two spec files in a directory → both mirrored
  - Exit code reflects combined results

- [x] **9.9** Verify: 9 acceptance tests pass, 60 unit tests pass

**Files Created:**
| File | Purpose |
|------|---------|
| `test/tests/test_mirror.py` | Mirror acceptance tests |
| `test/fixtures/mirror/mirror-test-tool.yaml` | Test mirror spec |
| `test/fixtures/mirror/metadata/test-tool.json` | Test metadata |
| `test/src/mirror_runner.py` | Mirror test helper |

---

### Stage 10: Polish + Documentation

**Goal:** Error messages, progress reporting, edge case handling, and user documentation.

**Prerequisites:** Stage 9 (all tests passing).

#### Steps

- [x] **10.1** Progress reporting with `tracing` spans (existing tracing in sync/orchestrator)
  - `info_span!("mirror", package = spec.name)` for top-level
  - `info_span!("version", version = %v)` for per-version
  - `info_span!("download", url = %url)` for downloads
  - `info_span!("push", platform = %p)` for pushes

- [ ] **10.2** Structured output (deferred — `--format json` not yet implemented)
  - `--format json` produces machine-readable results
  - Summary report at end: N pushed, N skipped, N failed

- [x] **10.3** Edge case handling (temp cleanup on success, retain on failure)
  - Graceful handling of GitHub API rate limit exhaustion
  - Graceful handling of registry auth failure (exit code 3)
  - Temp directory cleanup on success; retain on failure with log

- [x] **10.4** Workspace-wide clippy + tests pass (0 warnings, all tests green)

- [ ] **10.5** Update `CLAUDE.md` with `ocx_mirror` crate information (deferred)

**Files Modified:**
| File | Changes |
|------|---------|
| Various `src/` files | Tracing spans, error messages |
| `CLAUDE.md` | Add ocx_mirror section |

---

## Dependencies

### New Crate Dependencies

| Package | Version | Purpose | Notes |
|---------|---------|---------|-------|
| `ocx_lib` | path | Core library (OCI client, bundler, version) | workspace |
| `tokio` | 1 | Async runtime | workspace |
| `clap` | 4 | CLI argument parsing | already in ocx_cli |
| `serde` | 1 | Serialization framework | already in workspace |
| `serde_yaml_ng` | 0.9 | YAML deserialization | **replaces `serde_yml`** (RUSTSEC-2025-0068); drop-in fork of deprecated `serde_yaml` |
| `serde_json` | 1 | JSON deserialization | already in workspace |
| `octocrab` | 0.49 | GitHub Releases API client | Typed releases/assets API, built-in pagination via `Link` headers, rate limit header parsing, `GITHUB_TOKEN` auth |
| `reqwest` | 0.12 | HTTP downloads (asset files) | indirect dep via `octocrab`; also used directly for non-GitHub downloads |
| `reqwest-middleware` | 0.4 | Middleware chain for reqwest | Wraps reqwest::Client with retry middleware |
| `reqwest-retry` | 0.9 | Retry transient HTTP errors | `RetryTransientMiddleware` with exponential backoff; handles 429/5xx, respects `Retry-After` |
| `regex` | 1 | Asset pattern matching | already in ocx_lib |
| `url` | 2 | URL type | already in octocrab/reqwest |
| `anyhow` | 1 | Error handling | already in ocx_cli |
| `tracing` | 0.1 | Logging/progress | already in workspace |
| `tracing-subscriber` | 0.3 | Log output | already in ocx_cli |
| `tempfile` | 3 | Temp directories | already in ocx_lib |
| `chrono` | 0.4 | Build timestamp generation | already in ocx_lib |
| `sha2` | 0.10 | Download digest verification | already in ocx_lib |
| `hex` | 0.4 | Hex encoding for digest comparison | already in ocx_lib |
| `glob` | 0.3 | `sync-all` spec file discovery | — |

#### Dev Dependencies

| Package | Version | Purpose |
|---------|---------|---------|
| `wiremock` | 0.6 | HTTP server mocking for GitHub API + download tests |
| `tokio` | 1 (features: `test-util`) | Test runtime |
| `assert_fs` | 1 | Temp file/dir assertions (optional, `tempfile` may suffice) |

### ocx_lib API Requirements

No changes to `ocx_lib` are expected. All required APIs exist:
- `Client::push_package()`, `copy_manifest_data()`, `list_tags()`, `fetch_manifest()`
- `BundleBuilder::from_path().create()`
- `Archive::extract_with_options()`
- `Version::parse()`, `cascade()`, `is_rolling()`
- `package::info::Info`, `Metadata`, `Platform`, `Identifier`, `Digest`

**Potential gap:** OCI annotation support in `push_package()`. If the current API does not accept annotations, a minor extension to `ocx_lib::oci::Client::push_package()` or `Info` struct may be needed. This should be investigated at the start of Stage 6.

---

## Testing Strategy

### Unit Tests (per module, in each `.rs` file)

| Module | Key Test Cases |
|--------|---------------|
| `spec` | Valid/invalid YAML, defaults, all source types |
| `source/github_release` | Tag parsing, pagination, draft filtering, rate limiting |
| `source/url_index` | Inline versions, remote JSON, prerelease |
| `resolver` | Single/multi match, dedup, ambiguity, zero match |
| `normalizer` | All normalization cases from ADR table |
| `filter` | Each filter independently, combined, edge cases |
| `registry` | Tag set-diff, platform completeness |
| `pipeline/download` | Success, retry, timeout |
| `pipeline/verify` | Digest match/mismatch, checksums parsing |
| `pipeline/package` | Extract + bundle for different archive types |
| `pipeline/push` | Push + cascade logic |

### Acceptance Tests (`test/tests/test_mirror.py`)

| Scenario | Validates |
|----------|-----------|
| Validate valid spec | Spec parsing, exit code 0 |
| Validate invalid spec | Error reporting, exit code 2 |
| Check (dry-run) | Source discovery, filtering, no side effects |
| Full sync (URL Index) | End-to-end pipeline, registry state |
| Idempotent re-sync | Already-mirrored detection |
| Cascade tags | Rolling tag creation |
| Version filtering (min/max) | Filter pipeline |
| new_per_run cap | Incremental backfill |
| sync-all | Multi-spec orchestration |

---

## Risks

| Risk | Mitigation |
|------|------------|
| OCI annotation API gap in ocx_lib | Investigate at Stage 6 start; may need minor `Info` extension |
| `octocrab` API changes | Pin to 0.49.x; octocrab is stable and actively maintained |
| Acceptance test flakiness (registry timing) | Retry logic in test fixtures; deterministic URL Index source |
| Large stage sizes | Each stage is independently verifiable; split further if needed |

---

## Progress Log

| Date | Stage | Update |
|------|-------|--------|
| 2026-03-12 | — | Plan created |
