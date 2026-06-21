// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::cli::Cell;
use serde::Serialize;

use crate::api::Printable;

/// Report emitted by `ocx [--global] patch publish`.
///
/// Plain format: a single-row table (`Reference | Digest | Rules`) showing the
/// published patch repo reference, the manifest digest, and the descriptor rule
/// count.
///
/// JSON format: `{ "reference": "...", "manifest_digest": "...", "rules": N }`.
#[derive(Serialize)]
pub struct PatchPublishReport {
    /// Canonical reference the descriptor was published to
    /// (`registry/repository:__ocx.patch`).
    pub reference: String,
    /// Manifest digest of the pushed `__ocx.patch` artifact.
    pub manifest_digest: String,
    /// Number of rules in the published descriptor.
    pub rules: usize,
}

impl PatchPublishReport {
    /// Build a publish report from the library-layer
    /// [`ocx_lib::package_manager::PatchPublishReport`].
    pub fn new(inner: ocx_lib::package_manager::PatchPublishReport) -> Self {
        Self {
            reference: inner.patch_reference,
            manifest_digest: inner.manifest_digest.to_string(),
            rules: inner.rule_count,
        }
    }
}

impl Printable for PatchPublishReport {
    fn print_plain(&self, printer: &ocx_lib::cli::DataInterface) {
        // Column-major: each inner Vec is one column (Reference | Digest | Rules),
        // matching `print_table`'s contract (`rows[c]` holds the cells of column c).
        let rows: [Vec<String>; 3] = [
            vec![self.reference.clone()],
            vec![self.manifest_digest.clone()],
            vec![self.rules.to_string()],
        ];
        printer.print_table(
            &["Reference".into(), "Digest".into(), "Rules".into()],
            &rows.map(|c| c.into_iter().map(Cell::from).collect::<Vec<_>>()),
        );
    }
}
