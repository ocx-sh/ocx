# Plan: Cascade Push — Refactor to Library Layer

<!--
Implementation Plan
Filename: artifacts/plan_cascade_refactoring.md
Owner: Architect
Handoff to: Builder (/builder)
Related Skills: implementing-code, testing
-->

## Overview

**Status:** Draft
**Author:** Claude
**Date:** 2026-03-12
**Related ADR:** [adr_cascade_platform_aware_push.md](./adr_cascade_platform_aware_push.md)

## Objective

Refactor the platform-aware cascade push implementation so that version algebra lives in `ocx_lib` (where it belongs), cascade orchestration lives in a dedicated library module, and the CLI command (`package_push.rs`) is a thin shell. The current implementation places `compute_blockers`, `manifest_has_platform`, `extract_platform_entry`, and the merge loop all inside the CLI command — mixing transport, version logic, and OCI plumbing in one file.

## Scope

### In Scope

- Add `cascade_excluding` to `version.rs` (returns the blocker that stopped it)
- Create `package::cascade` module for cascade orchestration (resolve tags + push)
- Move manifest helpers into `oci/client.rs` as private functions
- Simplify `package_push.rs` to thin orchestration
- Migrate and extend unit tests
- Preserve all existing acceptance tests (no behavior change)

### Out of Scope

- Adding cascade logic to `PackageManager` (push doesn't need `FileStructure` or `Index`)
- Changing the acceptance test structure
- Website documentation changes

## Problem Analysis

### Why `compute_blockers` Is Fundamentally Incomplete

The current `compute_blockers` returns **at most one blocker** — the first version that would prevent cascade at some level. But removing that single blocker (because it lacks our platform) can **reveal new blockers** at the same or higher levels:

```
Push: 3.28.1 (amd64)
Others: 3.28.2 (arm64-only), 3.29.0 (arm64-only)

compute_blockers returns: [3.28.2]   — blocks at minor level
After removing 3.28.2:   [3.29.0]   — NOW blocks at major level
```

### Pre-Computing All Potential Blockers Is Wasteful

The earlier design (`cascade_potential_blockers`) collected ALL versions across ALL blocking ranges upfront. For an old patch like `3.2.1` when versions go up to `3.50.x`, this means fetching manifests for potentially hundreds of versions — when in practice the first blocker almost always has the platform and is a real blocker.

### Current Responsibility Violations

| Function | Current Location | Correct Location | Reason |
|----------|-----------------|-----------------|--------|
| `compute_blockers` | `package_push.rs` | Eliminated — replaced by iterative `cascade_excluding` | Pure version algebra — no I/O |
| `manifest_has_platform` | `package_push.rs` | `client.rs` (private) | OCI manifest inspection |
| `extract_platform_entry` | `package_push.rs` | `client.rs` (private) | OCI manifest inspection |
| Cascade merge loop | `package_push.rs` | `package::cascade` (public) | Domain orchestration |
| Blocker fetch + cascade resolution | `package_push.rs` | `package::cascade` (public) | Domain orchestration |

## Technical Approach

### Design: Lazy Iterative Cascade with Blocker Return

The key insight: instead of pre-computing all potential blockers, **`cascade_excluding` returns the version that stopped it**. The caller iterates lazily — one manifest fetch per iteration:

1. Run `cascade_excluding` → get `(tags, is_latest, stopped_by)`
2. If `stopped_by` is `None` → cascade completed, done
3. Fetch ONE manifest for the blocker
4. If blocker has our platform → real blocker, done (cascade result is final)
5. If blocker lacks our platform → add to exclude set, goto 1

This is optimal:
- **Best case (typical):** 0 fetches (no blocker) or 1 fetch (first blocker has our platform)
- **Worst case:** Bounded by the number of non-platform versions in the cascade path, not all versions in the repo
- **Multi-level revelation handled naturally:** Removing a blocker at patch level lets cascade continue upward where it may hit a different blocker at minor level

### Three-Layer Architecture

```
┌─────────────────────────────────────────────────────┐
│  CLI: package_push.rs                                │
│  - Parse args, list tags, build package info         │
│  - Call cascade::push_with_cascade()                 │
├─────────────────────────────────────────────────────┤
│  Library: package::cascade                           │
│  - resolve_cascade_tags(): iterative blocker loop    │
│  - push_with_cascade(): resolve + push + merge       │
│  Composes Version algebra + Client transport         │
├──────────────────────┬──────────────────────────────┤
│  Version (pure)      │  Client (OCI transport)       │
│  - cascade_excluding │  - push_package               │
│  - cascade (compat)  │  - merge_platform_into_index  │
│  - parent chain walk │  - fetch_manifest             │
│                      │  - manifest_has_platform (priv)│
│                      │  - extract_platform_entry(priv)│
└──────────────────────┴──────────────────────────────┘
```

### Version Ordering Context

In `BTreeSet<Version>`, ordering places rolling versions GREATER than their children:

```
3.28 > 3.28.2 > 3.28.2+b1 > 3.28.1 > 3.28.1+b1 > 3.27 > ...
```

The parent chain walks: `3.28.1+b1 → 3.28.1 → 3.28 → 3 → (top)`.

At each level, the blocking range is `(current, parent)` in BTreeSet order. At the top level (no parent), the range is `(current, ∞)` — blockers here affect `is_latest`.

### `is_latest` Is Not Special

`is_latest` is simply the top level of the parent chain where `parent = None`. The range `(current, Unbounded)` captures all potential blockers for the latest flag. `cascade_excluding` handles it naturally: if `.find(|v| !exclude.contains(v))` returns `None` at the top level, `is_latest = true`.

### Key Decisions

| Decision | Rationale |
|----------|-----------|
| `cascade_excluding` returns `Option<Self>` blocker | Enables lazy iteration — caller fetches one manifest at a time instead of all potential blockers upfront |
| `cascade` delegates to `cascade_excluding` with empty set | DRY — one implementation, backward compatible |
| New `package::cascade` module | Composes version algebra + OCI transport without polluting either layer; CLI stays thin |
| NOT on `Client` | Client is OCI transport; cascade resolution mixes version algebra with transport — wrong abstraction level |
| NOT on `PackageManager` | Push doesn't need `FileStructure` or `Index` |
| `merge_platform_into_index` stays `pub(crate)` | Used by `package::cascade`, not by external callers |
| Manifest helpers move to `client.rs` as private | They inspect `oci::Manifest` which is transport-layer data |

## API Design

### `version.rs` — Modified `cascade_excluding`

```rust
/// Like `cascade`, but treats versions in `exclude` as if they don't exist.
///
/// Returns `(cascade_versions, is_latest, stopped_by)` where `stopped_by`
/// is the version that prevented further cascade (if any). The caller can
/// inspect this blocker (e.g. check its platform via manifest fetch) and
/// re-run with an expanded exclude set if the blocker should be ignored.
pub fn cascade_excluding(
    &self,
    others: impl IntoIterator<Item = Self>,
    exclude: &HashSet<Self>,
) -> (Vec<Self>, bool, Option<Self>) {
    let others: BTreeSet<Self> = others.into_iter().collect();
    let mut versions = vec![self.clone()];
    let mut is_latest = false;
    let mut stopped_by = None;
    let mut current = Some(self.clone());

    while let Some(current_version) = &current {
        let parent_version = current_version.parent();
        // .find() skips excluded versions
        let least_greater = others
            .range((Excluded(current_version), Unbounded))
            .find(|v| !exclude.contains(v));

        match least_greater {
            None => {
                // No (non-excluded) greater version — cascade continues upward
                if let Some(parent_version) = parent_version {
                    versions.push(parent_version.clone());
                    current = Some(parent_version);
                } else {
                    is_latest = true;
                    break;
                }
            }
            Some(least_greater) => {
                if parent_version <= Some(least_greater.clone()) {
                    // Gap is above our parent — cascade continues upward
                    if let Some(parent_version) = parent_version {
                        versions.push(parent_version.clone());
                        current = Some(parent_version);
                    } else {
                        is_latest = true;
                        break;
                    }
                } else {
                    // Blocker sits between current and parent — cascade stops
                    stopped_by = Some(least_greater.clone());
                    break;
                }
            }
        }
    }
    (versions, is_latest, stopped_by)
}

/// Computes the cascade chain for this version given existing versions.
/// Delegates to `cascade_excluding` with an empty exclusion set.
pub fn cascade(
    &self,
    others: impl IntoIterator<Item = Self>,
) -> (Vec<Self>, bool) {
    let (versions, is_latest, _stopped_by) = self.cascade_excluding(others, &HashSet::new());
    (versions, is_latest)
}
```

### `package/cascade.rs` — New Module

```rust
//! Platform-aware cascade push orchestration.
//!
//! Composes `Version` algebra with `Client` OCI transport to implement
//! cascade pushes that correctly handle multi-platform registries.

use std::collections::{BTreeSet, HashSet};

use crate::{
    log, oci,
    package::{self, version::Version},
    prelude::*,
};

/// Resolves cascade tags by iteratively running `cascade_excluding`,
/// fetching one blocker manifest at a time to check platform membership.
///
/// Returns the list of tags to cascade to (excluding the primary tag)
/// and whether this version should also become `latest`.
pub async fn resolve_cascade_tags(
    client: &oci::Client,
    identifier: &oci::Identifier,
    version: &Version,
    other_versions: BTreeSet<Version>,
    platform: &oci::Platform,
) -> Result<(Vec<String>, bool)> {
    let mut exclude = HashSet::new();

    loop {
        let (cascaded, is_latest, stopped_by) =
            version.cascade_excluding(other_versions.iter().cloned(), &exclude);

        let Some(blocker) = stopped_by else {
            // Cascade completed fully — no blocker
            let tags = build_cascade_tags(cascaded, is_latest);
            return Ok((tags, is_latest));
        };

        // Fetch ONE manifest to check if blocker has our platform
        let blocker_id = identifier.clone_with_tag(blocker.to_string());
        match client.fetch_manifest(&blocker_id).await {
            Ok((_, manifest)) => {
                if manifest_has_platform(&manifest, platform) {
                    // Real blocker — cascade result is final
                    let tags = build_cascade_tags(cascaded, is_latest);
                    return Ok((tags, is_latest));
                }
                // Not our platform — exclude and retry
                log::debug!(
                    "Blocker {blocker} lacks platform {platform}, excluding from cascade"
                );
                exclude.insert(blocker);
            }
            Err(e) => {
                // Can't verify — assume it blocks (conservative)
                log::debug!(
                    "Could not fetch manifest for blocker {blocker}, assuming it blocks: {e}"
                );
                let tags = build_cascade_tags(cascaded, is_latest);
                return Ok((tags, is_latest));
            }
        }
    }
}

/// Pushes a package to its primary tag, then merges the platform entry
/// into each cascade tag sequentially (most-specific → least-specific
/// for partial-failure safety).
///
/// Combines `resolve_cascade_tags` + `push_package` + cascade merge.
pub async fn push_with_cascade(
    client: &oci::Client,
    package_info: package::info::Info,
    file: impl AsRef<Path>,
    other_versions: BTreeSet<Version>,
    version: &Version,
) -> Result<(oci::Digest, oci::Manifest)> {
    // Resolve cascade targets
    let (cascade_tags, _is_latest) = resolve_cascade_tags(
        client,
        &package_info.identifier,
        version,
        other_versions,
        &package_info.platform,
    )
    .await?;

    // Push primary tag
    log::info!("Pushing package with identifier {}", package_info.identifier);
    let (digest, manifest) = client
        .push_package(package_info.clone(), file.as_ref().to_path_buf())
        .await?;

    // Cascade: merge platform entry into each rolling tag
    if !cascade_tags.is_empty() {
        let (manifest_sha256, manifest_size) =
            extract_platform_entry(&manifest, &package_info.platform)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Push succeeded but image index has no entry for platform {}",
                        package_info.platform
                    )
                })?;

        for tag in &cascade_tags {
            log::info!("Cascading to {}", tag);
            client
                .merge_platform_into_index(
                    &package_info.identifier,
                    tag.clone(),
                    &package_info.platform,
                    &manifest_sha256,
                    manifest_size,
                )
                .await?;
        }
    }

    Ok((digest, manifest))
}

/// Builds the list of cascade tag strings from cascade versions.
/// Skips the first entry (the pushed version itself) and appends "latest" if needed.
fn build_cascade_tags(cascaded: Vec<Version>, is_latest: bool) -> Vec<String> {
    let mut tags: Vec<String> = cascaded
        .into_iter()
        .skip(1)
        .map(|v| v.to_string())
        .collect();
    if is_latest {
        tags.push("latest".to_string());
    }
    tags
}

/// Returns the digest and size of the platform's manifest entry in an image index.
fn extract_platform_entry(
    manifest: &oci::Manifest,
    platform: &oci::Platform,
) -> Option<(String, i64)> {
    let oci::Manifest::ImageIndex(index) = manifest else {
        return None;
    };
    let native: oci::native::Platform = platform.clone().into();
    let target = Some(native);
    index
        .manifests
        .iter()
        .find(|e| e.platform == target)
        .map(|e| (e.digest.clone(), e.size))
}

/// Returns true if the manifest contains an entry for the given platform.
fn manifest_has_platform(manifest: &oci::Manifest, platform: &oci::Platform) -> bool {
    let oci::Manifest::ImageIndex(index) = manifest else {
        return false;
    };
    let native: oci::native::Platform = platform.clone().into();
    let target = Some(native);
    index.manifests.iter().any(|e| e.platform == target)
}
```

### `package_push.rs` — Simplified CLI Command

```rust
impl PackagePush {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifier = self.identifier.with_domain(context.default_registry())?;
        let metadata_path = match &self.metadata {
            Some(path) => path.clone(),
            None => crate::conventions::infer_metadata_file(&self.content)?,
        };

        log::info!(
            "Deploying package from {} with metadata {}",
            self.content.display(),
            metadata_path.display()
        );
        let metadata = package::metadata::Metadata::read_json_from_path(&metadata_path)?;
        let package_info = package::info::Info {
            identifier: identifier.clone(),
            metadata,
            platform: self.platform.clone(),
        };

        if self.cascade {
            let version = Version::parse(identifier.tag_or_latest()).ok_or_else(|| {
                anyhow::anyhow!(
                    "Tag is not a valid version, cannot cascade: {}",
                    identifier.tag_or_latest()
                )
            })?;

            let other_tags = match context.remote_client()?.list_tags(identifier.clone()).await {
                Ok(tags) => tags,
                Err(err) => {
                    if self.new {
                        log::info!("Failed to list tags, assuming new package: {err}");
                        Vec::new()
                    } else {
                        return Err(anyhow::anyhow!(
                            "Failed to list existing tags for {}: {err}", identifier
                        ));
                    }
                }
            };

            let other_versions: BTreeSet<Version> =
                other_tags.iter().filter_map(|t| Version::parse(t)).collect();

            package::cascade::push_with_cascade(
                context.remote_client()?,
                package_info,
                &self.content,
                other_versions,
                &version,
            )
            .await?;
        } else {
            log::info!("Pushing package with identifier {}", identifier);
            context
                .remote_client()?
                .push_package(package_info, self.content.clone())
                .await?;
        }

        Ok(ExitCode::SUCCESS)
    }
}
// No more compute_blockers, manifest_has_platform, extract_platform_entry,
// or cascade merge loop in this file.
```

## Implementation Steps

### Phase 1: Version Algebra (version.rs)

- [ ] **Step 1.1:** Modify `cascade_excluding` to return `(Vec<Self>, bool, Option<Self>)`
  - File: `crates/ocx_lib/src/package/version.rs`
  - Details: Add third tuple element `stopped_by: Option<Self>` — the version that blocked cascade. Same loop logic but captures the blocker instead of silently breaking.

- [ ] **Step 1.2:** Refactor `cascade` to delegate to `cascade_excluding`
  - File: `crates/ocx_lib/src/package/version.rs`
  - Details: `cascade` calls `cascade_excluding(others, &HashSet::new())` and discards `stopped_by`. No behavior change.

- [ ] **Step 1.3:** Add unit tests for `cascade_excluding` with blocker return
  - File: `crates/ocx_lib/src/package/version.rs` (inline `#[cfg(test)]`)
  - Details: See Testing Strategy below.

### Phase 2: Cascade Module (package/cascade.rs)

- [ ] **Step 2.1:** Create `crates/ocx_lib/src/package/cascade.rs`
  - Details: Contains `resolve_cascade_tags`, `push_with_cascade`, `build_cascade_tags`, `extract_platform_entry`, `manifest_has_platform`.

- [ ] **Step 2.2:** Add `pub mod cascade;` to `crates/ocx_lib/src/package.rs` (or `mod.rs`)
  - Details: Wire up the new module in the package namespace.

- [ ] **Step 2.3:** Ensure `merge_platform_into_index` is accessible from `package::cascade`
  - File: `crates/ocx_lib/src/oci/client.rs`
  - Details: Change visibility to `pub(crate)` if currently private, or keep `pub` if already public. No external callers outside `ocx_lib` should use it.

### Phase 3: Simplify CLI Command (package_push.rs)

- [ ] **Step 3.1:** Replace all cascade logic with `package::cascade::push_with_cascade`
  - File: `crates/ocx_cli/src/command/package_push.rs`
  - Details: Remove `compute_blockers`, `manifest_has_platform`, `extract_platform_entry`, the two-pass blocker fetch, the merge loop. The command just builds `other_versions` and calls `push_with_cascade`.

- [ ] **Step 3.2:** Migrate unit tests
  - Details: `compute_blockers` tests → replaced by `cascade_excluding` blocker-return tests in `version.rs`. `manifest_has_platform` / `extract_platform_entry` tests → move to `cascade.rs` `#[cfg(test)]` module. No tests remain in `package_push.rs` (all logic moved out).

### Phase 4: Verification

- [ ] **Step 4.1:** Run `cargo fmt`
- [ ] **Step 4.2:** Run `cargo clippy --workspace`
- [ ] **Step 4.3:** Run `cargo nextest run --workspace`
- [ ] **Step 4.4:** Run `task test` (acceptance tests)
- [ ] **Step 4.5:** Run `task verify` (full verification)

## Files to Modify

| File | Action | Description |
|------|--------|-------------|
| `crates/ocx_lib/src/package/version.rs` | Modify | Add `cascade_excluding` with blocker return; refactor `cascade` to delegate |
| `crates/ocx_lib/src/package/cascade.rs` | Create | `resolve_cascade_tags`, `push_with_cascade`, manifest helpers |
| `crates/ocx_lib/src/package.rs` | Modify | Add `pub mod cascade;` |
| `crates/ocx_lib/src/oci/client.rs` | Modify | Ensure `merge_platform_into_index` is `pub(crate)` |
| `crates/ocx_cli/src/command/package_push.rs` | Modify | Remove all cascade logic; call `package::cascade::push_with_cascade` |

## Dependencies

No new dependencies. Uses existing `std::collections::{BTreeSet, HashSet}` and async Rust.

## Testing Strategy

### Unit Tests — `version.rs` (cascade_excluding with blocker)

| Test | Description |
|------|-------------|
| `cascade_excluding_no_blocker` | No greater versions → `stopped_by = None`, `is_latest = true` |
| `cascade_excluding_blocker_at_patch` | Returns the patch-level blocker in `stopped_by` |
| `cascade_excluding_blocker_at_minor` | Returns the minor-level blocker in `stopped_by` |
| `cascade_excluding_blocker_at_major` | Returns the major-level blocker (affects `is_latest`) in `stopped_by` |
| `cascade_excluding_skip_reveals_next` | Excluding one blocker reveals a blocker at the next level |
| `cascade_excluding_skip_all_full_cascade` | Excluding all blockers → full cascade + `is_latest = true` |
| `cascade_excluding_empty_exclude_matches_cascade` | Same result as `cascade` for various inputs |
| `cascade_excluding_prerelease_with_build` | Prerelease build blocker returned correctly |
| `cascade_excluding_prerelease_without_build` | No cascade beyond own level, `stopped_by = None` |

### Unit Tests — `package/cascade.rs`

| Test | Description |
|------|-------------|
| `manifest_has_platform_empty_index` | Migrated from `package_push.rs` |
| `manifest_has_platform_matching` | Migrated from `package_push.rs` |
| `manifest_has_platform_non_matching` | Migrated from `package_push.rs` |
| `manifest_has_platform_image_manifest` | Migrated from `package_push.rs` |
| `extract_platform_entry_found` | Migrated from `package_push.rs` |
| `extract_platform_entry_missing` | Migrated from `package_push.rs` |
| `build_cascade_tags_with_latest` | Verifies tag list construction |
| `build_cascade_tags_without_latest` | Verifies skip(1) and no "latest" appended |

### Acceptance Tests

No changes. The three existing tests in `test/tests/test_cascade.py` verify end-to-end behavior:

| Test | What It Verifies |
|------|-----------------|
| `test_cascade_preserves_platforms` | Bug 2 fix: merge doesn't destroy other platform entries |
| `test_cascade_platform_aware_version_filter` | Bug 1 fix: other-platform versions don't block cascade |
| `test_cascade_new_package_all_levels` | Fresh package creates all rolling tags |

## Risks

| Risk | Mitigation |
|------|------------|
| `cascade` behavior change due to delegation | Existing tests cover all scenarios; `cascade_excluding` with empty set is identical |
| Iterative loop doesn't terminate | Each iteration adds one version to `exclude`; bounded by finite `others` set |
| `resolve_cascade_tags` error handling on fetch failure | Conservative: assume blocker is real, stop cascade (safe default) |
| `merge_platform_into_index` visibility change breaks external code | `ocx_cli` doesn't call it directly after refactoring; verify with `cargo check` |

## Checklist

### Before Starting

- [x] ADR approved (adr_cascade_platform_aware_push.md)
- [x] Current implementation passing all tests (138 unit + 37 acceptance)
- [x] Branch exists (`sion`)

### Before PR

- [ ] All tests passing (`task verify`)
- [ ] No clippy warnings
- [ ] `cargo fmt` clean
- [ ] Self-review: `package_push.rs` has no version algebra or manifest inspection

### Before Merge

- [ ] Code review approved
- [ ] Acceptance tests pass on CI

## Notes

- The `cascade` method's doc comment (version.rs:100-103) already says "the caller is responsible for filtering by platform" — this refactoring fulfills that contract properly.
- `push_with_cascade` cascades sequentially (most-specific → least-specific) for partial-failure safety: if cascading to `3` fails, `3.28` is already correct.
- `manifest_has_platform` is a free function (not a method on `Manifest`) because `Manifest` is from the `oci_client` crate and we can't add methods to foreign types without a newtype wrapper.
- The iterative approach fetches at most one manifest per cascade level (build → patch → minor → major → latest = 5 levels max), and typically 0 or 1 in practice since most versions have all platforms.
- `compute_blockers` is eliminated entirely — its role is replaced by the `stopped_by` return value from `cascade_excluding` plus the iterative loop in `resolve_cascade_tags`.

---

## Progress Log

| Date | Update |
|------|--------|
| 2026-03-12 | Initial implementation complete (all tests pass) |
| 2026-03-12 | Design flaw identified in `compute_blockers` (single-blocker limitation) |
| 2026-03-12 | Plan v1: hybrid pre-computed ranges + skip predicate |
| 2026-03-12 | Plan v2: lazy iterative cascade with blocker return + `package::cascade` module |
