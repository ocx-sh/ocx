// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::package::metadata::env::modifier::ModifierKind;

/// Trait that each CI flavor must implement to export environment variables
/// into its runtime files.
pub(super) trait Flavor {
    /// Writes a single environment variable entry to the CI system's runtime files.
    ///
    /// For path-type variables, implementations may buffer values internally and
    /// defer writing until [`flush`](Flavor::flush) is called.
    fn write_entry(&mut self, key: &str, value: &str, kind: &ModifierKind) -> crate::Result<()>;

    /// Flushes any buffered state to the CI system's runtime files.
    fn flush(&mut self) -> crate::Result<()>;
}
