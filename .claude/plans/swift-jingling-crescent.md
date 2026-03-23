# Phase 3: Mirror Variant Support

## Context

Phase 1 (variant-aware version parsing) and Phase 2 (cascade variant isolation) are complete. Phase 3 adds variant support to the mirror tool so a single YAML spec can declare multiple variants (e.g., `pgo.lto`, `debug`) of a package, each with its own asset patterns.

The cascade algebra already handles variant-track isolation (Phase 2). This phase wires variant context through the mirror pipeline: spec parsing → asset resolution → task building → push.

## Implementation Steps

### Step 1: `VariantSpec` type (`crates/ocx_mirror/src/spec/variant.rs` — NEW)

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct VariantSpec {
    pub name: String,
    #[serde(default)]
    pub default: bool,
    pub assets: AssetPatterns,
    #[serde(default)]
    pub metadata: Option<MetadataConfig>,
    #[serde(default)]
    pub asset_type: Option<AssetTypeConfig>,
}
```

Also define `EffectiveVariant` (resolved with inheritance):

```rust
#[derive(Debug, Clone)]
pub struct EffectiveVariant {
    pub name: Option<String>,     // None = legacy no-variant spec
    pub is_default: bool,
    pub assets: AssetPatterns,
    pub metadata: Option<MetadataConfig>,
    pub asset_type: Option<AssetTypeConfig>,
}
```

### Step 2: `MirrorSpec` changes (`crates/ocx_mirror/src/spec.rs`)

- Add `mod variant;` and re-exports
- Change `assets` from `AssetPatterns` to `Option<AssetPatterns>` with `#[serde(default)]`
- Add `#[serde(default)] pub variants: Option<Vec<VariantSpec>>`
- Add `effective_variants(&self) -> Vec<EffectiveVariant>`:
  - No `variants` key → single synthetic variant from top-level `assets`/`metadata`/`asset_type`
  - With `variants` → one per declared variant, inheriting top-level `metadata`/`asset_type` as fallbacks
- Add validation:
  - Either `assets` or `variants` must be present, not both, not neither
  - Exactly one variant `default: true`
  - Names match `^[a-z][a-z0-9.]*$`, not `"latest"`, no duplicates
  - Each variant's `assets` patterns validated (regex)
  - Each variant's `metadata` validated (file existence)

### Step 3: `VariantContext` on `MirrorTask` (`crates/ocx_mirror/src/pipeline/mirror_task.rs`)

```rust
#[derive(Debug, Clone)]
pub struct VariantContext {
    pub name: String,
    pub is_default: bool,
}

pub struct MirrorTask {
    // ... existing fields unchanged ...
    pub variant: Option<VariantContext>,  // NEW
}
```

### Step 4: `ResolvedVersion` gains variant field (`crates/ocx_mirror/src/filter.rs`)

```rust
pub struct ResolvedVersion {
    pub version: String,              // raw source version ("3.12.5")
    pub normalized_version: String,   // variant-prefixed + timestamped ("debug-3.12.5_20260322")
    pub variant: Option<String>,      // NEW: variant name for this resolution
    pub platforms: Vec<ResolvedPlatformAsset>,
    pub is_prerelease: bool,
}
```

**Critical subtlety**: The already-mirrored check (line 106) currently uses `Version::parse(&v.version)` where `version` is the raw source version. For variants, the registry has tags like `debug-3.12.5` (rolling). The check must compare against the variant-prefixed version to detect that `debug-3.12.5` is already mirrored. But min/max bounds (lines 56-78) must use the bare version (variant-aware `Ord` would incorrectly filter `debug-3.12.5` against `max: "4.0.0"` because `Some("debug") > None` in the Ord).

Fix in `filter_versions()`:
- **Min/max bounds**: continue using `Version::parse(&v.version)` (bare source version) — correct across all variants
- **Already-mirrored check**: construct variant-prefixed version:
  ```rust
  let check_version = match &v.variant {
      Some(name) => Version::parse(&format!("{name}-{}", v.version)),
      None => Version::parse(&v.version),
  }.expect("mirror versions must be valid");
  ```

### Step 5: Variant-aware `source_version_tags` in `sync.rs`

Currently (line 109-118), `source_version_tags` collects raw source versions to determine which registry tags need manifest fetches. For variants, it must also include variant-prefixed versions so that `debug-3.12.5` on the registry triggers a platform check:

```rust
let source_version_tags: HashSet<String> = resolved_versions
    .iter()
    .filter_map(|rv| {
        let v = Version::parse(&rv.version)?;
        // Include variant-prefixed form too
        let variant_prefixed = rv.variant.as_ref().map(|name| {
            Version::parse(&format!("{name}-{}", rv.version))
                .map(|vv| vv.to_string())
        }).flatten();
        Some(std::iter::once(v.to_string()).chain(variant_prefixed))
    })
    .flatten()
    .collect();
```

### Step 6: Variant iteration loop in `sync.rs`

Wrap the asset resolution + task building in a variant loop. Move from iterating `(version, platform)` to `(variant, version, platform)`:

```rust
let effective_variants = spec.effective_variants();
let mut all_resolved: Vec<ResolvedVersion> = Vec::new();

for variant in &effective_variants {
    let patterns = variant.assets.compiled().map_err(|e| MirrorError::SpecInvalid(vec![e]))?;

    for version_info in &upstream_versions {
        match resolver::resolve_assets(&version_info.assets, &patterns) {
            AssetResolution::Resolved(platforms) => {
                match normalizer::normalize_version(&version_info.version, &build_ts) {
                    Ok(normalized) => {
                        let tagged = match &variant.name {
                            Some(name) => format!("{name}-{normalized}"),
                            None => normalized,
                        };
                        all_resolved.push(ResolvedVersion {
                            version: version_info.version.clone(),
                            normalized_version: tagged,
                            variant: variant.name.clone(),
                            platforms,
                            is_prerelease: version_info.is_prerelease,
                        });
                    }
                    Err(e) => { log::warn!(...); }
                }
            }
            AssetResolution::Ambiguous(amb) => { /* warn */ }
        }
    }
}
```

Then filter and build tasks from `all_resolved`. Each `MirrorTask` gets:
- `variant: variant_name.map(|n| VariantContext { name: n, is_default: variant.is_default })`
- `metadata_config` from `EffectiveVariant` (variant override or inherited)
- `asset_type` from `EffectiveVariant`

The existing `spec.assets.compiled()` call moves inside the variant loop (each variant has its own patterns). Top-level `spec.metadata` and `spec.asset_type` become fallbacks through `EffectiveVariant`.

### Step 7: Default variant alias cascade (`crates/ocx_mirror/src/pipeline/push.rs`)

When pushing the default variant, cascade produces variant-prefixed tags (`pgo.lto-3.12.5`, `pgo.lto-3.12`, ..., `pgo.lto`). The default variant also needs unadorned aliases (`3.12.5`, `3.12`, ..., `latest`).

Add alias pass after the primary cascade:

```rust
pub async fn push_and_cascade(
    publisher: &Publisher,
    info: Info,
    bundle_path: &Path,
    cascade: bool,
    cascade_versions: &BTreeSet<Version>,
    variant: Option<&VariantContext>,  // NEW
) -> Result<MirrorResult> {
    // ... existing cascade push ...

    // Default variant aliasing: re-cascade with bare version tag
    if cascade && let Some(ctx) = variant && ctx.is_default {
        if let Some(version) = Version::parse(&version_str) && version.variant().is_some() {
            let bare = version.without_variant();
            let bare_info = /* clone info with bare version tag */;
            publisher.push_cascade(bare_info, bundle_path, cascade_versions.clone()).await?;
        }
    }
}
```

OCI registries handle duplicate blob uploads as no-ops (content-addressed), so the second push is just tag creation.

### Step 8: Thread variant through orchestrator (`crates/ocx_mirror/src/pipeline/orchestrator.rs`)

`push_task()` passes `task.variant.as_ref()` to `push_and_cascade()`.

### Step 9: `Version::without_variant()` (`crates/ocx_lib/src/package/version.rs`)

```rust
pub fn without_variant(&self) -> Version {
    Version { variant: None, ..self.clone() }
}
```

### Step 10: Tests

**Spec tests** (`crates/ocx_mirror/src/spec.rs` or `spec/variant.rs`):
- Parse YAML with variants (happy path)
- Parse YAML without variants (backward compat)
- Reject both `assets` and `variants` present
- Reject neither `assets` nor `variants`
- Exactly one default validation
- Variant naming rules: `[a-z][a-z0-9.]*`, reject `latest`, reject duplicates
- `effective_variants()` with/without variants
- Variant inherits and overrides top-level metadata/asset_type

**Filter tests** (`crates/ocx_mirror/src/filter.rs`):
- Variant-prefixed versions correctly detected as already-mirrored
- Min/max bounds applied to bare version, not variant-prefixed
- Different variants of same source version tracked independently

**Version test** (`crates/ocx_lib/src/package/version.rs`):
- `without_variant()` strips variant, preserves rest

## File Change Summary

| File | Change |
|------|--------|
| `crates/ocx_mirror/src/spec/variant.rs` | **NEW** — `VariantSpec`, `EffectiveVariant` |
| `crates/ocx_mirror/src/spec.rs` | `variants` field, `assets` optional, validation, `effective_variants()` |
| `crates/ocx_mirror/src/spec/assets.rs` | Make `AssetPatterns` Clone (needed by EffectiveVariant) |
| `crates/ocx_mirror/src/pipeline/mirror_task.rs` | `VariantContext`, `variant` field |
| `crates/ocx_mirror/src/filter.rs` | `variant` field on `ResolvedVersion`, variant-aware already-mirrored check |
| `crates/ocx_mirror/src/command/sync.rs` | Variant iteration loop, variant-aware source_version_tags |
| `crates/ocx_mirror/src/pipeline/push.rs` | Default variant alias cascade, new `variant` parameter |
| `crates/ocx_mirror/src/pipeline/orchestrator.rs` | Thread variant to push_task |
| `crates/ocx_lib/src/package/version.rs` | `without_variant()` method |

## Implementation Order

1. `Version::without_variant()` — no dependencies
2. `spec/variant.rs` — new types
3. `spec.rs` — variants field, assets optional, validation, effective_variants()
4. `mirror_task.rs` — VariantContext
5. `filter.rs` — variant field, variant-aware checks
6. `sync.rs` — variant loop (depends on 2-5)
7. `push.rs` — default alias cascade (depends on 1, 4)
8. `orchestrator.rs` — thread variant (depends on 4, 7)
9. Tests throughout

## Verification

1. `cargo check --workspace` after each step
2. `cargo nextest run --workspace` — existing tests pass (backward compat)
3. `task verify` after all changes
4. Manual: parse a multi-variant YAML spec and verify effective_variants() output
