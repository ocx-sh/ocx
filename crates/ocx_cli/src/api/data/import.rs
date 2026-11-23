// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Report data for `ocx package import` and `ocx import`.

use ocx_console::Cell;
use ocx_package_manager::{ImportOutcome, ImportPlatformStatus};
use ocx_util::wire_words;
use serde::Serialize;

use crate::api::Printable;
use crate::api::data::export::TagStatus;
use crate::api::data::sanitize_for_terminal;

wire_words! {
    /// Whether this run put a platform into the home.
    #[derive(Serialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
    pub enum ImportPlatformWord {
        /// This run assembled the package in the home.
        Materialized = "materialized",
        /// The home held it already.
        Present = "present",
    }
}

impl From<ImportPlatformStatus> for ImportPlatformWord {
    fn from(status: ImportPlatformStatus) -> Self {
        match status {
            ImportPlatformStatus::Materialized => Self::Materialized,
            ImportPlatformStatus::Present => Self::Present,
        }
    }
}

/// One tag of an imported package.
#[derive(Serialize, schemars::JsonSchema)]
pub struct ImportedTag {
    /// The tag as the export lists it.
    pub tag: String,
    /// The digest the export pins the tag to.
    pub digest: ocx_oci::Digest,
    /// What the import did with the tag; `conflict` leaves the home's pin alone and exits 0.
    pub status: TagStatus,
    /// The digest the home pinned before (`replaced`) or still pins (`conflict`); absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub home_digest: Option<ocx_oci::Digest>,
}

/// One platform of an imported package.
#[derive(Serialize, schemars::JsonSchema)]
pub struct ImportedPlatformRow {
    /// The platform the manifest serves.
    pub platform: ocx_oci::Platform,
    /// The platform manifest digest.
    pub digest: ocx_oci::Digest,
    /// Whether this run assembled it or the home held it already.
    pub status: ImportPlatformWord,
}

/// One package of an import.
#[derive(Serialize, schemars::JsonSchema)]
pub struct ImportedItem {
    /// The package as listed; a root import reads the lock pin, `registry/repository@sha256:...`.
    pub identifier: ocx_oci::PackageRef,
    /// The tags the export lists; absent for a digest identifier and a root import.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<ImportedTag>>,
    /// Platforms assembled for `added`, `unchanged` and `replaced` tags; a `conflict` tag adds none.
    pub manifests: Vec<ImportedPlatformRow>,
}

/// Result of `ocx package import` and `ocx import`.
///
/// Plain stdout is one row per tag, then one row per platform, so the tag's status and each platform's stay apart.
#[derive(Serialize, schemars::JsonSchema)]
pub struct ImportReport {
    /// One entry per package, in the order the export lists them.
    pub items: Vec<ImportedItem>,
}

impl ImportReport {
    pub fn from_outcome(outcome: ImportOutcome) -> Self {
        Self {
            items: outcome
                .items
                .into_iter()
                .map(|item| ImportedItem {
                    identifier: item.identifier,
                    tags: item.tags.map(|rows| {
                        rows.into_iter()
                            .map(|row| ImportedTag {
                                tag: row.tag,
                                digest: row.digest,
                                status: row.status.into(),
                                home_digest: row.home_digest,
                            })
                            .collect()
                    }),
                    manifests: item
                        .platforms
                        .into_iter()
                        .map(|row| ImportedPlatformRow {
                            platform: row.platform,
                            digest: row.digest,
                            status: row.status.into(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    /// The four plain columns as `print_table` writes them, assertable without a terminal.
    fn plain_rows(&self) -> [Vec<String>; 4] {
        let mut columns: [Vec<String>; 4] = Default::default();
        let mut push = |package: &str, tag: &str, platform: &str, status: &str| {
            columns[0].push(sanitize_for_terminal(package));
            columns[1].push(sanitize_for_terminal(tag));
            columns[2].push(platform.to_owned());
            columns[3].push(status.to_owned());
        };
        for item in &self.items {
            for tag in item.tags.iter().flatten() {
                push(&item.identifier.to_string(), &tag.tag, "", tag.status.as_str());
            }
            for platform in &item.manifests {
                push(
                    &item.identifier.to_string(),
                    "",
                    &platform.platform.to_string(),
                    platform.status.as_str(),
                );
            }
        }
        columns
    }
}

impl Printable for ImportReport {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "ImportReport";

    fn print_plain(&self, data: &ocx_console::DataInterface) {
        data.print_table(
            &["Package".into(), "Tag".into(), "Platform".into(), "Status".into()],
            &self
                .plain_rows()
                .map(|column| column.into_iter().map(Cell::from).collect::<Vec<_>>()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest() -> ocx_oci::Digest {
        ocx_oci::Digest::Sha256("a".repeat(64))
    }

    #[test]
    fn home_digest_is_omitted_unless_set() {
        let tag = |home_digest| ImportedTag {
            tag: "1".into(),
            digest: digest(),
            status: TagStatus::Added,
            home_digest,
        };
        let plain = serde_json::to_value(tag(None)).expect("serialize");
        let moved = serde_json::to_value(tag(Some(digest()))).expect("serialize");
        assert!(
            plain.get("home_digest").is_none(),
            "IC-04: omitted, never null: {plain}"
        );
        assert!(moved.get("home_digest").is_some());
    }

    #[test]
    fn plain_lists_tags_then_platforms() {
        let report = ImportReport {
            items: vec![ImportedItem {
                identifier: "registry.test/team/tool".parse().expect("identifier"),
                tags: Some(vec![ImportedTag {
                    tag: "1.2".into(),
                    digest: digest(),
                    status: TagStatus::Conflict,
                    home_digest: Some(digest()),
                }]),
                manifests: vec![ImportedPlatformRow {
                    platform: "linux/amd64".parse().expect("platform"),
                    digest: digest(),
                    status: ImportPlatformWord::Materialized,
                }],
            }],
        };
        let [_, tags, platforms, statuses] = report.plain_rows();
        assert_eq!(tags, ["1.2", ""]);
        assert_eq!(platforms, ["", "linux/amd64"]);
        assert_eq!(statuses, ["conflict", "materialized"]);
    }
}
