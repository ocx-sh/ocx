// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Report type for `ocx index sign`.

use ocx_console::{Cell, DataInterface};
use ocx_util::fs::path::AbsolutePath;
use serde::Serialize;

use crate::api::Printable;
use crate::api::data::sanitize_for_terminal;

/// What `ocx index sign` wrote.
#[derive(Serialize, schemars::JsonSchema)]
pub struct IndexSignReport {
    /// The catalog file that was rewritten with its new `expires`.
    pub catalog_path: AbsolutePath,
    /// The detached signature file beside it.
    pub signature_path: AbsolutePath,
    /// Digest of the catalog bytes the signature covers.
    pub catalog_digest: ocx_oci::Digest,
    /// When the catalog stops being trusted.
    pub expires_at: ocx_util::time::Timestamp,
    /// Signing mechanism used: `"keyless-fulcio"`, or the key backend's own slug under a key.
    pub signer: String,
    /// Certificate SAN (identity) embedded in the Fulcio cert. Absent under a key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certificate_identity: Option<String>,
    /// Certificate OIDC issuer URL embedded in the Fulcio cert. Absent under a key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certificate_oidc_issuer: Option<String>,
    /// Which key model produced this signature.
    pub key_backend: ocx_trust::key_ref::KeyBackendKind,
    /// The signing key's cosign hint, in key mode only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_key_hint: Option<String>,
    /// The Rekor log index; absent when no upload happened.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tlog_index: Option<u64>,
}

impl Printable for IndexSignReport {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "IndexSignReport";

    fn print_plain(&self, printer: &DataInterface) {
        // `print_table` takes column-major rows: one vector per column.
        let path = |path: &AbsolutePath| Cell::from(sanitize_for_terminal(&path.as_path().display().to_string()));
        printer.print_table(
            &["Item".into(), "Value".into()],
            &[
                vec![Cell::from("catalog"), Cell::from("signature"), Cell::from("expires")],
                vec![
                    path(&self.catalog_path),
                    path(&self.signature_path),
                    Cell::from(self.expires_at.to_string()),
                ],
            ],
        );
    }
}
