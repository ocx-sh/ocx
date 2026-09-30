// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Report data for `ocx package prune`.

use ocx_console::Cell;

use crate::api::Printable;
use crate::api::data::sanitize_for_terminal;

/// What `package prune` did to each selected tag, in processing order; printed on failure too.
pub type PackagePrune = ocx_package::prune::PruneOutcome;

impl Printable for PackagePrune {
    /// Table `Action Tag Digest Reason`.
    fn print_plain(&self, data: &ocx_console::DataInterface) {
        let theme = data.theme();
        let mut columns: [Vec<Cell>; 4] = Default::default();
        for row in &self.tags {
            columns[0].push(Cell::from(row.action.to_string()));
            // Registry-listed, so neutralized before it reaches a terminal.
            columns[1].push(Cell::from(theme.tag(sanitize_for_terminal(&row.tag))));
            let digest = row
                .digest
                .as_ref()
                .map(|digest| digest.to_short_string())
                .unwrap_or_default();
            columns[2].push(Cell::from(theme.digest(&digest)));
            columns[3].push(Cell::from(
                row.reason.map(|reason| reason.to_string()).unwrap_or_default(),
            ));
        }
        let headers: [ocx_console::Column; 4] = ["Action".into(), "Tag".into(), "Digest".into(), "Reason".into()];
        data.print_table(&headers, &columns);
    }
}
