// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_console::Cell;
use serde::Serialize;

use crate::api::Printable;

/// Report emitted by `ocx [--global] patch publish`.
///
/// Plain format: a single-row table (`Reference | Digest | Rules`) showing the
/// published patch repo reference, the manifest digest, and the descriptor rule
/// count.
#[derive(Serialize, schemars::JsonSchema)]
pub struct PatchPublishReport {
    /// Canonical reference the descriptor was published to
    /// (`registry/repository:__ocx.patch`).
    pub reference: String,
    /// Manifest digest of the pushed `__ocx.patch` artifact.
    pub manifest_digest: ocx_oci::Digest,
    /// Number of rules in the published descriptor.
    pub rules: usize,
}

impl PatchPublishReport {
    pub fn new(inner: ocx_package_manager::PatchPublishReport) -> Self {
        Self {
            reference: inner.patch_reference,
            manifest_digest: inner.manifest_digest,
            rules: inner.rule_count,
        }
    }
}

impl Printable for PatchPublishReport {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "PatchPublishReport";

    fn print_plain(&self, printer: &ocx_console::DataInterface) {
        // Column-major: `rows[c]` holds column c.
        let rows: [Vec<String>; 3] = [
            vec![self.reference.clone()],
            vec![self.manifest_digest.to_string()],
            vec![self.rules.to_string()],
        ];
        printer.print_table(
            &["Reference".into(), "Digest".into(), "Rules".into()],
            &rows.map(|c| c.into_iter().map(Cell::from).collect::<Vec<_>>()),
        );
    }
}
