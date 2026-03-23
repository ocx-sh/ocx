// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::Deserialize;

use super::asset_type::AssetTypeConfig;
use super::assets::AssetPatterns;
use super::metadata_config::MetadataConfig;

/// A variant declaration in a mirror spec.
///
/// Each variant has its own asset patterns (the primary differentiator) and
/// can optionally override metadata and asset_type from the top-level spec.
///
/// The `name` field is optional: the default variant may omit it to produce
/// bare (unprefixed) version tags. Non-default variants must have a name.
#[derive(Debug, Clone, Deserialize)]
pub struct VariantSpec {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub default: bool,
    pub assets: AssetPatterns,
    #[serde(default)]
    pub metadata: Option<MetadataConfig>,
    #[serde(default)]
    pub asset_type: Option<AssetTypeConfig>,
}

/// A resolved variant with all inherited fields materialized.
///
/// Produced by [`MirrorSpec::effective_variants()`](super::MirrorSpec::effective_variants).
/// For legacy specs without a `variants` key, a single `EffectiveVariant` with
/// `name: None` is produced from the top-level fields.
#[derive(Debug, Clone)]
pub struct EffectiveVariant {
    /// Variant name, or `None` for legacy no-variant specs.
    pub name: Option<String>,
    /// Whether this is the default variant (always true for legacy specs).
    pub is_default: bool,
    /// Asset patterns for this variant.
    pub assets: AssetPatterns,
    /// Metadata config (variant override or inherited from top-level).
    pub metadata: Option<MetadataConfig>,
    /// Asset type config (variant override or inherited from top-level).
    pub asset_type: Option<AssetTypeConfig>,
}
