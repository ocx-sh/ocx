// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::oci::Platform;
use serde::Serialize;

/// Outcome of processing a single mirror task.
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum MirrorResult {
    Pushed {
        version: String,
        platform: Platform,
        digest: String,
    },
    #[allow(dead_code)]
    Skipped { version: String },
    Failed {
        version: String,
        platform: Platform,
        error: String,
    },
}
