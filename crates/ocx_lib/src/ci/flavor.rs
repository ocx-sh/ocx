// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::package::metadata::env::modifier::ModifierKind;

// Conflict tracking at this layer is value-only. The `entry::Entry` stream
// passed to `write_entry` is already resolved and carries no package
// attribution, so both flavors call `ConstantTracker::track("", key, value)`
// with an empty package label. A conflict warning therefore identifies the
// key and the two values, not which packages set them — package attribution
// would have to be threaded down from the composer, which the CI export
// surface does not currently do.

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
