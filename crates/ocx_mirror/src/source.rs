// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub mod github_release;
pub mod url_index;

use std::collections::HashMap;

use url::Url;

/// Information about a single upstream version, produced by source adapters.
#[derive(Debug, Clone)]
pub struct VersionInfo {
    pub version: String,
    pub assets: HashMap<String, Url>,
    pub is_prerelease: bool,
}
