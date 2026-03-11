// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::oci::Platform;
use url::Url;

/// Result of resolving assets for a version across all platforms.
#[derive(Debug)]
pub enum AssetResolution {
    /// All platforms resolved to exactly one asset each.
    Resolved(Vec<ResolvedPlatformAsset>),
    /// One or more platforms matched multiple distinct assets.
    Ambiguous(Vec<AmbiguousAsset>),
}

/// A single platform's resolved asset.
#[derive(Debug, Clone)]
pub struct ResolvedPlatformAsset {
    pub platform: Platform,
    pub asset_name: String,
    pub url: Url,
}

/// A platform that matched multiple distinct assets — this is an error.
#[derive(Debug)]
pub struct AmbiguousAsset {
    pub platform: Platform,
    pub matched_assets: Vec<String>,
}
