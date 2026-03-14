// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::package::metadata::env::modifier::ModifierKind;

/// Trait that each CI flavor must implement to export environment variables
/// into its runtime files.
pub(super) trait Flavor {
    /// Writes a single environment variable entry to the CI system's runtime files.
    fn write_entry(&self, key: &str, value: &str, kind: &ModifierKind) -> crate::Result<()>;
}
