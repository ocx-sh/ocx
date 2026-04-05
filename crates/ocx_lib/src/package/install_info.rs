// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::Serialize;

use crate::oci;

use super::{metadata, resolved_package::ResolvedPackage};

#[derive(Debug, Clone, Serialize)]
pub struct InstallInfo {
    pub identifier: oci::PinnedIdentifier,
    pub metadata: metadata::Metadata,
    pub resolved: ResolvedPackage,
    pub content: std::path::PathBuf,
}
