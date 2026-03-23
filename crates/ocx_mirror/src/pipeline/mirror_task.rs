// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use ocx_lib::oci::Platform;
use url::Url;

use crate::spec::{AssetType, MetadataConfig, Target, VerifyConfig};

/// Variant context carried by a mirror task.
#[derive(Debug, Clone)]
pub struct VariantContext {
    /// Variant name (e.g., "debug", "pgo.lto"). Stored for diagnostics and future annotation support.
    #[allow(dead_code)]
    pub name: String,
    pub is_default: bool,
}

/// A single unit of work: download + verify + package + push one platform of one version.
/// Self-contained with all data needed for execution.
#[derive(Debug, Clone)]
pub struct MirrorTask {
    #[allow(dead_code)] // Original version kept for Debug output
    pub version: String,
    pub normalized_version: String,
    pub platform: Platform,
    pub download_url: Url,
    pub asset_name: String,
    pub target: Target,
    pub metadata_config: Option<MetadataConfig>,
    pub verify_config: Option<VerifyConfig>,
    pub cascade: bool,
    pub spec_dir: PathBuf,
    pub asset_type: AssetType,
    /// Variant context for variant-aware cascade and aliasing.
    pub variant: Option<VariantContext>,
}
