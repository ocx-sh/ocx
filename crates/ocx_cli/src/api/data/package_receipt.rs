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
/// JSON format: each key absent when the build did not record it — the same
/// contract as the file, where absence means "not given", never `null`.
#[derive(Serialize, schemars::JsonSchema)]
pub struct PackageReceipt {
    /// The `--platform` the bundle was built for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<ocx_oci::Platform>,
    /// The `--identifier` the bundle was built to be pushed as.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identifier: Option<ocx_oci::PackageRef>,
}

impl From<&BuildReceipt> for PackageReceipt {
    fn from(receipt: &BuildReceipt) -> Self {
        Self {
            platform: receipt.platform.clone(),
            identifier: receipt.identifier.clone(),
        }
    }
}

impl Printable for PackageReceipt {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "PackageReceipt";

    fn print_plain(&self, data: &ocx_console::DataInterface) {
        let theme = data.theme();
        let platform = self.platform.as_ref().map(ToString::to_string);
        let identifier = self.identifier.as_ref().map(ToString::to_string);
        for (label, value) in [("platform:", platform), ("identifier:", identifier)] {
            if let Some(value) = value {
                println!("{} {}", theme.label(label), theme.tag(value));
            }
        }
    }
}
