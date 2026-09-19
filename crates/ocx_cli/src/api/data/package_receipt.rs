// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::Serialize;

use crate::api::Printable;
use crate::build_receipt::BuildReceipt;

/// Report emitted by `ocx package receipt`: what `ocx package create` recorded
/// beside a bundle.
///
/// Plain format: one `label: value` row per recorded field.
///
/// JSON format: `{ "platform": "linux/amd64", "identifier": "..." }`, each key
/// absent when the build did not record it — the same contract as the file,
/// where absence means "not given", never `null`.
#[derive(Serialize, schemars::JsonSchema)]
pub struct PackageReceipt {
    /// The `--platform` the bundle was built for, in canonical grammar.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    pub platform: Option<String>,
    /// The `--identifier` the bundle was built to be pushed as.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    pub identifier: Option<String>,
}

impl From<&BuildReceipt> for PackageReceipt {
    fn from(receipt: &BuildReceipt) -> Self {
        Self {
            platform: receipt.platform.as_ref().map(ToString::to_string),
            identifier: receipt.identifier.as_ref().map(ToString::to_string),
        }
    }
}

impl Printable for PackageReceipt {
    fn print_plain(&self, data: &ocx_console::DataInterface) {
        let theme = data.theme();
        for (label, value) in [("platform:", &self.platform), ("identifier:", &self.identifier)] {
            if let Some(value) = value {
                println!("{} {}", theme.label(label), theme.tag(value));
            }
        }
    }
}
