// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct VerifyConfig {
    #[serde(default = "default_true")]
    pub github_asset_digest: bool,
    pub checksums_file: Option<String>,
}

fn default_true() -> bool {
    true
}
