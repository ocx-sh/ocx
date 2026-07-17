// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::BTreeMap;

use ocx_lib::cli::Cell;
use serde::Serialize;

use crate::api::Printable;

/// A single locked tool entry in the `ocx lock` report.
///
/// `binding` is the TOML key in `ocx.toml` (the local binding name);
/// `group` is `"default"` for entries from the top-level `[tools]`
/// table or the named `[group.*]` key. `digest` is the host-platform
/// leaf digest (the primary digest column). `platforms` maps each
/// shipped platform's lossless key string to its leaf digest — the full
/// available-only map surfaced in verbose / JSON output.
#[derive(Serialize)]
pub struct LockEntry {
    pub binding: String,
    pub group: String,
    pub digest: String,
    pub platforms: BTreeMap<String, String>,
}

/// Report emitted by `ocx lock` after a successful resolve.
///
/// Plain format: three-column table (Binding | Group | Digest) where the
/// digest column is the host-platform leaf digest.
///
/// JSON format: array of `{ binding, group, digest, platforms }` objects,
/// where `platforms` is the full available-only platform-key → digest map.
#[derive(Serialize)]
#[serde(transparent)]
pub struct LockReport {
    entries: Vec<LockEntry>,
}

impl LockEntry {
    /// Build a report entry from an in-memory [`LockedTool`], selecting the
    /// host-platform leaf as the primary `digest` column and projecting the
    /// full available-only map into `platforms`.
    ///
    /// The primary digest is the host→`Any`-offer leaf; the full
    /// available-only map surfaces in verbose / JSON output. When the host
    /// leaf is absent OR ambiguous (the publisher does not ship this
    /// platform, or two entries tie), the primary digest falls back to empty
    /// rather than fabricating one or erroring out of a report command.
    pub fn from_tool(tool: &ocx_lib::project::LockedTool, host: &ocx_lib::oci::Platform) -> Self {
        let digest = match ocx_lib::project::lookup_host_leaf(&tool.platforms, host) {
            ocx_lib::oci::Selection::Found((digest, _key)) => digest.to_string(),
            ocx_lib::oci::Selection::None | ocx_lib::oci::Selection::Ambiguous(_) => String::new(),
        };
        let platforms: BTreeMap<String, String> =
            tool.platforms.iter().map(|(k, v)| (k.clone(), v.to_string())).collect();

        Self {
            binding: tool.name.clone(),
            group: tool.group.clone(),
            digest,
            platforms,
        }
    }
}

impl LockReport {
    pub fn new(entries: Vec<LockEntry>) -> Self {
        Self { entries }
    }
}

impl Printable for LockReport {
    fn print_plain(&self, printer: &ocx_lib::cli::DataInterface) {
        let mut rows: [Vec<String>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for entry in &self.entries {
            rows[0].push(entry.binding.clone());
            rows[1].push(entry.group.clone());
            rows[2].push(entry.digest.clone());
        }
        printer.print_table(
            &["Binding".into(), "Group".into(), "Digest".into()],
            &rows.map(|c| c.into_iter().map(Cell::from).collect::<Vec<_>>()),
        );
    }
}
