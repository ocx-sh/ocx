// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use ocx_lib::oci::Platform;
use url::Url;

use crate::spec::{MetadataConfig, Target, VerifyConfig};

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
    pub strip_components: Option<u8>,
}
