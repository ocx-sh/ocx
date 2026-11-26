// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Report data for `ocx vendor` and `ocx package vendor`.

use ocx_console::Cell;
use ocx_package_manager::{VendorContent, VendorOutcome};
use ocx_util::fs::path::AbsolutePath;
use ocx_util::size::ByteSize;
use ocx_util::wire_words;
use serde::Serialize;

use crate::api::Printable;
use crate::api::data::sanitize_for_terminal;

wire_words! {
    /// Whether the run wrote package content.
    #[derive(Serialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
    pub enum VendorContentWord {
        /// Package content and launchers were written.
        Present = "present",
        /// Only `manifest.json` was written.
        Absent = "absent",
    }
}

impl From<VendorContent> for VendorContentWord {
    fn from(content: VendorContent) -> Self {
        match content {
            VendorContent::Present => Self::Present,
            VendorContent::Absent => Self::Absent,
        }
    }
}

/// One package of a vendored set.
#[derive(Serialize, schemars::JsonSchema)]
pub struct VendoredPackageRow {
    /// The package as resolved, pinned by digest.
    pub identifier: ocx_oci::PackageRef,
    /// The platform manifest digest.
    pub digest: ocx_oci::Digest,
}

/// Result of `ocx vendor` and `ocx package vendor`.
#[derive(Serialize, schemars::JsonSchema)]
pub struct VendorReport {
    /// The `ocx` that wrote the directory.
    pub ocx_version: String,
    /// The vendor directory, absolute.
    pub output_dir: AbsolutePath,
    /// The written `manifest.json`, absolute.
    pub manifest_path: AbsolutePath,
    /// `present`, or `absent` under `--manifest-only`.
    pub content: VendorContentWord,
    /// The platform the set was resolved for.
    pub platform: String,
    /// One entry per package, in manifest order.
    pub packages: Vec<VendoredPackageRow>,
    /// Tools the manifest lists.
    pub tool_count: usize,
    /// Bytes this run wrote.
    pub written_size: ByteSize,
}

impl VendorReport {
    pub fn from_outcome(outcome: VendorOutcome) -> Result<Self, ocx_util::error::FileError> {
        let absolute = |path: &std::path::Path| {
            AbsolutePath::new(path)
                .ok_or_else(|| ocx_util::error::FileError::new(path, std::io::Error::other("not an absolute path")))
        };
        Ok(Self {
            ocx_version: env!("CARGO_PKG_VERSION").to_owned(),
            output_dir: absolute(&outcome.output_dir)?,
            manifest_path: absolute(&outcome.manifest_path)?,
            content: outcome.content.into(),
            platform: outcome.platform.to_string(),
            packages: outcome
                .packages
                .into_iter()
                .map(|package| VendoredPackageRow {
                    identifier: package.identifier,
                    digest: package.digest,
                })
                .collect(),
            tool_count: outcome.tool_count,
            written_size: outcome.written_size.into(),
        })
    }

    /// The two plain columns as `print_table` writes them, assertable without a terminal.
    fn plain_rows(&self) -> [Vec<String>; 2] {
        let mut columns: [Vec<String>; 2] = Default::default();
        for package in &self.packages {
            columns[0].push(sanitize_for_terminal(&package.identifier.to_string()));
            columns[1].push(package.digest.to_string());
        }
        columns
    }
}

impl Printable for VendorReport {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "VendorReport";

    fn print_plain(&self, data: &ocx_console::DataInterface) {
        data.print_table(
            &["Package".into(), "Digest".into()],
            &self
                .plain_rows()
                .map(|column| column.into_iter().map(Cell::from).collect::<Vec<_>>()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> VendorReport {
        VendorReport {
            ocx_version: "0.7.0".to_owned(),
            output_dir: AbsolutePath::new(std::env::temp_dir()).expect("absolute temp dir"),
            manifest_path: AbsolutePath::new(std::env::temp_dir().join("manifest.json")).expect("absolute"),
            content: VendorContentWord::Present,
            platform: "linux/amd64".to_owned(),
            packages: vec![VendoredPackageRow {
                identifier: "registry.test/team/tool:1.2".parse().expect("identifier"),
                digest: ocx_oci::Digest::Sha256("a".repeat(64)),
            }],
            tool_count: 1,
            written_size: 61_234_567.into(),
        }
    }

    #[test]
    fn json_carries_the_design_fields() {
        let json = serde_json::to_value(report()).expect("serialize");
        assert_eq!(json["content"], "present");
        assert_eq!(json["platform"], "linux/amd64");
        assert_eq!(json["tool_count"], 1);
        assert_eq!(json["written_size"], 61_234_567u64);
        assert_eq!(json["packages"][0]["digest"], format!("sha256:{}", "a".repeat(64)));
    }

    #[test]
    fn plain_has_one_row_per_package() {
        let [packages, digests] = report().plain_rows();
        assert_eq!(packages, ["registry.test/team/tool:1.2"]);
        assert_eq!(digests, [format!("sha256:{}", "a".repeat(64))]);
    }
}
