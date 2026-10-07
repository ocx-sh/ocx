// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::BTreeMap;

use ocx_console::Cell;
use serde::Serialize;

use crate::api::Printable;

/// One locked binding in the `ocx lock` report.
#[derive(Serialize, schemars::JsonSchema)]
pub struct LockEntry {
    /// The binding's key in `ocx.toml`.
    pub binding: String,
    /// The owning group: `default` for the top-level `[tools]` table, else the `[group.*]` key.
    pub group: String,
    /// The host platform's leaf digest; absent when no leaf, or more than one, fits the host.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<ocx_oci::Digest>,
    /// Every leaf the lock records, keyed by the canonical platform string
    /// (`os/arch[/variant][+feature,…]` or `any`).
    pub platform_digests: BTreeMap<String, ocx_oci::Digest>,
}

/// Report emitted by `ocx lock`, `ocx add` and `ocx remove`: every locked binding.
///
/// Plain format: three-column table (Binding | Group | Digest) of the host-platform leaf.
#[derive(Serialize, schemars::JsonSchema)]
pub struct LockReport {
    /// The locked bindings, in lock order.
    items: Vec<LockEntry>,
}

impl LockEntry {
    /// Builds a report entry from a [`LockedTool`](ocx_project::LockedTool); `digest` is the host leaf.
    pub fn from_tool(tool: &ocx_project::LockedTool, host: &ocx_oci::Platform) -> Self {
        let digest = match ocx_project::lookup_host_leaf(&tool.platforms, host) {
            ocx_oci::Selection::Found((digest, _key)) => Some(digest.clone()),
            ocx_oci::Selection::None | ocx_oci::Selection::Ambiguous(_) => None,
        };
        Self {
            binding: tool.name.clone(),
            group: tool.group.clone(),
            digest,
            platform_digests: tool.platforms.clone(),
        }
    }
}

impl LockReport {
    pub fn new(items: Vec<LockEntry>) -> Self {
        Self { items }
    }
}

impl Printable for LockReport {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "LockReport";

    fn print_plain(&self, printer: &ocx_console::DataInterface) {
        let mut rows: [Vec<String>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for entry in &self.items {
            rows[0].push(entry.binding.clone());
            rows[1].push(entry.group.clone());
            rows[2].push(entry.digest.as_ref().map(ToString::to_string).unwrap_or_default());
        }
        printer.print_table(
            &["Binding".into(), "Group".into(), "Digest".into()],
            &rows.map(|c| c.into_iter().map(Cell::from).collect::<Vec<_>>()),
        );
    }
}
