// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_package::metadata::env::modifier::ModifierKind;

/// Exports environment entries into one CI system's runtime files.
pub(super) trait Flavor {
    /// Writes one entry; `Path` and `List` values may be buffered until [`flush`](Flavor::flush).
    ///
    /// `separator` is a `List` entry's already-reconciled separator; `None` means the default `" "`.
    fn write_entry(
        &mut self,
        key: &str,
        value: &str,
        kind: &ModifierKind,
        separator: Option<&str>,
    ) -> Result<(), crate::ci::error::Error>;

    /// Writes any buffered `Path`/`List` values.
    fn flush(&mut self) -> Result<(), crate::ci::error::Error>;
}
