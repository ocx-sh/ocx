---
paths:
  - crates/ocx_mirror/**
  - mirrors/**
---

# Mirror Subsystem

Separate crate (`ocx_mirror`) for mirroring upstream tool releases to OCI registries. YAML-configured, two-phase pipeline.

## Design Rationale

Separate crate because the mirror tool is a standalone binary with its own CLI, not part of the `ocx` package manager. Two-phase pipeline (prepare concurrently, push sequentially) ensures cascade tag ordering correctness — tags must be pushed in semver order so `latest` always points to the highest version. See `arch-principles.md` for the full pattern catalog.

## Module Map

| Path | Purpose |
|------|---------|
| `command/sync.rs` | Main sync command: spec → versions → filter → pipeline |
| `command/check.rs` | Dry-run sync |
| `command/validate.rs` | Spec validation only |
| `command/options.rs` | Shared `SyncOptions` (--exact-version, --latest, --fail-fast) |
| `spec/spec.rs` | `MirrorSpec` root, `load_spec()`, extends chain resolution |
| `spec/source.rs` | `Source` enum (GithubRelease, UrlIndex) |
| `spec/target.rs` | `Target` (registry + repository) |
| `spec/assets.rs` | `AssetPatterns` (platform → regex[] mapping) |
| `spec/asset_type.rs` | `AssetTypeConfig` (Archive vs Binary) |
| `spec/versions_config.rs` | Version filtering (min/max bounds, new_per_run, backfill order) |
| `spec/verify_config.rs` | Checksum verification options |
| `spec/metadata_config.rs` | Metadata.json path configuration |
| `spec/concurrency_config.rs` | Parallel download/push limits |
| `source/github_release.rs` | GitHub API client, tag pattern extraction |
| `source/url_index.rs` | JSON index fetching (remote, inline, generator) |
| `pipeline/orchestrator.rs` | `execute_mirror()`: prepare (concurrent) + push (sequential) |
| `pipeline/download.rs` | HTTP download with resumption |
| `pipeline/verify.rs` | Checksum verification |
| `pipeline/package.rs` | Extract archive, apply metadata, rebundle |
| `pipeline/push.rs` | Push to registry + cascade tag computation |
| `pipeline/mirror_task.rs` | `MirrorTask`: self-contained work unit |
| `pipeline/mirror_result.rs` | `MirrorResult`: Pushed/Skipped/Failed |
| `resolver.rs` | `resolve_assets()`: apply regex patterns to asset names |
| `filter.rs` | `filter_versions()`: apply bounds, prerelease skip, backfill cap |
| `normalizer.rs` | `normalize_version()`: add build timestamp |
| `error.rs` | `MirrorError`: SpecInvalid, SpecNotFound, ExecutionFailed, SourceError |

## Pipeline Architecture

**Two-phase**: prepare (concurrent) then push (sequential by version).

### Phase 1: Prepare (concurrent)

1. Fetch upstream versions (GitHub API or URL index)
2. Resolve assets per platform (regex matching)
3. Filter versions (min/max, prerelease, backfill cap)
4. Parallel: download → verify → bundle (two independent semaphores: I/O vs CPU)

### Phase 2: Push (sequential by version, oldest first)

1. Push bundle to registry
2. Cascade derived tags if enabled (X.Y.Z → X.Y → X → latest)
3. Track pushed (version, platform) pairs for cascade correctness

## Spec Format (YAML)

Key fields: `name`, `target` (registry + repo), `source` (GithubRelease or UrlIndex), `assets` (platform → regex[]), `asset_type` (Archive/Binary), `cascade`, `versions` (min/max/new_per_run/backfill), `verify`, `concurrency`.

Source types:
- `github_release`: `{owner, repo, tag_pattern}` — regex with `(?P<version>...)` capture
- `url_index`: Remote URL, inline versions, or generator command

Spec inheritance via `extends:` (shallow merge, child overrides parent).

## Mirror Configs

YAML files in `mirrors/` (e.g., `mirror-cmake.yml`, `mirror-go.yml`). Each defines one tool to mirror.

## Error Model

`MirrorError` enum with exit codes: 0 (success), 2 (spec invalid), 3 (execution failed), 4 (source error).
