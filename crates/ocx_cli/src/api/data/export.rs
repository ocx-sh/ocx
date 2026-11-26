// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Report data for `ocx package export` and `ocx export`.

use ocx_console::Cell;
use ocx_package_manager::{ExportOutcome, ExportPlatformStatus};
use ocx_util::fs::path::AbsolutePath;
use ocx_util::size::ByteSize;
use ocx_util::wire_words;
use serde::Serialize;

use crate::api::Printable;
use crate::api::data::sanitize_for_terminal;

wire_words! {
    /// What a run did with one tag of an exported or imported root.
    #[derive(Serialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
    pub enum TagStatus {
        /// The tag was not pinned before this run and is now.
        Added = "added",
        /// The tag was pinned to the digest it carries here already.
        Unchanged = "unchanged",
        /// The tag was pinned to another digest and moved; `home_digest` names the old one. Only for a named package.
        Replaced = "replaced",
        /// The tag is pinned to another digest and was kept; `home_digest` names it. Import only, never for a named package.
        Conflict = "conflict",
    }
}

impl From<ocx_index::TagStatus> for TagStatus {
    fn from(status: ocx_index::TagStatus) -> Self {
        match status {
            ocx_index::TagStatus::Added => Self::Added,
            ocx_index::TagStatus::Unchanged => Self::Unchanged,
            ocx_index::TagStatus::Replaced => Self::Replaced,
            ocx_index::TagStatus::Conflict => Self::Conflict,
        }
    }
}

wire_words! {
    /// Whether this run wrote a platform's blobs.
    #[derive(Serialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
    pub enum ExportPlatformWord {
        /// The blobs were written by this run.
        Written = "written",
        /// The directory held them already.
        Present = "present",
    }
}

impl From<ExportPlatformStatus> for ExportPlatformWord {
    fn from(status: ExportPlatformStatus) -> Self {
        match status {
            ExportPlatformStatus::Written => Self::Written,
            ExportPlatformStatus::Present => Self::Present,
        }
    }
}

/// One tag of an exported root.
#[derive(Serialize, schemars::JsonSchema)]
pub struct ExportedTag {
    /// The tag as the export's index pins it.
    pub tag: String,
    /// The image index digest the tag pins.
    pub digest: ocx_oci::Digest,
    /// `added`, `replaced` or `unchanged`; an export never reports `conflict`.
    pub status: TagStatus,
}

/// One platform of an exported package.
#[derive(Serialize, schemars::JsonSchema)]
pub struct ExportedPlatformRow {
    /// The platform the manifest serves.
    pub platform: ocx_oci::Platform,
    /// The platform manifest digest.
    pub digest: ocx_oci::Digest,
    /// Whether this run wrote the blobs.
    pub status: ExportPlatformWord,
}

/// One package of an export.
#[derive(Serialize, schemars::JsonSchema)]
pub struct ExportedPackageRow {
    /// The package as named; a lock pin reads `registry/repository@sha256:...`.
    pub identifier: ocx_oci::PackageRef,
    /// The tags written to the export's index; absent for a digest identifier and a lock pin.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<ExportedTag>>,
    /// One row per exported platform.
    pub manifests: Vec<ExportedPlatformRow>,
    /// Referrer manifests carried; 0 under `--no-referrers`.
    pub referrer_count: usize,
}

/// Result of `ocx package export` and `ocx export`.
///
/// An object root, not a list root: `output_dir`, `blob_count` and `written_size` belong to the run, not to a package.
/// Plain stdout is one row per package and platform.
#[derive(Serialize, schemars::JsonSchema)]
pub struct ExportReport {
    /// The export directory, or the archive file under `--archive`; absolute.
    pub output_dir: AbsolutePath,
    /// One entry per package, in request order.
    pub packages: Vec<ExportedPackageRow>,
    /// Blobs this run wrote.
    pub blob_count: usize,
    /// Bytes this run wrote.
    pub written_size: ByteSize,
}

impl ExportReport {
    pub fn from_outcome(outcome: ExportOutcome) -> Result<Self, ocx_util::error::FileError> {
        let output_dir = AbsolutePath::new(&outcome.output_dir).ok_or_else(|| {
            ocx_util::error::FileError::new(&outcome.output_dir, std::io::Error::other("not an absolute path"))
        })?;
        Ok(Self {
            output_dir,
            packages: outcome
                .packages
                .into_iter()
                .map(|package| ExportedPackageRow {
                    identifier: package.identifier,
                    tags: package.tags.map(|rows| {
                        rows.into_iter()
                            .map(|row| ExportedTag {
                                tag: row.tag,
                                digest: row.digest,
                                status: row.status.into(),
                            })
                            .collect()
                    }),
                    manifests: package
                        .platforms
                        .into_iter()
                        .map(|row| ExportedPlatformRow {
                            platform: row.platform,
                            digest: row.digest,
                            status: row.status.into(),
                        })
                        .collect(),
                    referrer_count: package.referrer_count,
                })
                .collect(),
            blob_count: outcome.blob_count,
            written_size: outcome.written_size.into(),
        })
    }

    /// The three plain columns as `print_table` writes them, assertable without a terminal.
    fn plain_rows(&self) -> [Vec<String>; 3] {
        let mut columns: [Vec<String>; 3] = Default::default();
        for package in &self.packages {
            for platform in &package.manifests {
                columns[0].push(sanitize_for_terminal(&package.identifier.to_string()));
                columns[1].push(platform.platform.to_string());
                columns[2].push(platform.status.as_str().to_owned());
            }
        }
        columns
    }
}

impl Printable for ExportReport {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "ExportReport";

    fn print_plain(&self, data: &ocx_console::DataInterface) {
        data.print_table(
            &["Package".into(), "Platform".into(), "Status".into()],
            &self
                .plain_rows()
                .map(|column| column.into_iter().map(Cell::from).collect::<Vec<_>>()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> ExportReport {
        ExportReport {
            output_dir: AbsolutePath::new(std::env::temp_dir()).expect("absolute temp dir"),
            packages: vec![ExportedPackageRow {
                identifier: "registry.test/team/tool:1.2".parse().expect("identifier"),
                tags: None,
                manifests: Vec::new(),
                referrer_count: 0,
            }],
            blob_count: 14,
            written_size: 52_428_800.into(),
        }
    }

    #[test]
    fn unset_tags_are_omitted_from_json() {
        let json = serde_json::to_value(report()).expect("serialize");
        let package = &json["packages"][0];
        assert!(
            package.get("tags").is_none(),
            "IC-04: an unset optional is omitted: {package}"
        );
        assert_eq!(json["blob_count"], 14);
        assert_eq!(json["written_size"], 52_428_800u64);
    }

    #[test]
    fn plain_has_one_row_per_package_and_platform() {
        let mut report = report();
        let platform: ocx_oci::Platform = "linux/amd64".parse().expect("platform");
        let digest = ocx_oci::Digest::Sha256("a".repeat(64));
        report.packages[0].manifests = vec![ExportedPlatformRow {
            platform,
            digest,
            status: ExportPlatformWord::Written,
        }];
        let [packages, platforms, statuses] = report.plain_rows();
        assert_eq!(packages, ["registry.test/team/tool:1.2"]);
        assert_eq!(platforms, ["linux/amd64"]);
        assert_eq!(statuses, ["written"]);
    }
}
