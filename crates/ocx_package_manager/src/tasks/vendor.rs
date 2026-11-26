// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx vendor` and `ocx package vendor`: write a relocatable directory of packages, launchers and a manifest.

use std::path::PathBuf;

use ocx_oci::{Digest, PackageRef, Platform};
use ocx_project::LockedTool;

use crate::PackageManager;

/// What to vendor.
#[derive(Debug, Clone)]
pub enum VendorSelection {
    /// The project lock's tools in `groups`, narrowed to `names` when it is not empty.
    Project {
        tools: Vec<LockedTool>,
        groups: Vec<String>,
        names: Vec<String>,
    },
    /// Named packages, resolved as every `ocx package` verb resolves them.
    Packages(Vec<PackageRef>),
}

/// What `ocx vendor` and `ocx package vendor` were asked to write.
#[derive(Debug, Clone)]
pub struct VendorRequest {
    /// The vendor directory: absent or empty, never a symlink.
    pub output: PathBuf,
    pub platform: Platform,
    pub selection: VendorSelection,
    /// Write `manifest.json` only: no package content, no launcher.
    pub manifest_only: bool,
    /// Create no symlink under the directory.
    pub omit_links: bool,
}

/// Whether the run wrote package content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VendorContent {
    Present,
    Absent,
}

/// One package of a vendored set.
#[derive(Debug, Clone)]
pub struct VendoredPackage {
    pub identifier: PackageRef,
    pub digest: Digest,
}

#[derive(Debug, Clone)]
pub struct VendorOutcome {
    pub output_dir: PathBuf,
    pub manifest_path: PathBuf,
    pub content: VendorContent,
    pub platform: Platform,
    pub packages: Vec<VendoredPackage>,
    pub tool_count: usize,
    /// Bytes this run wrote.
    pub written_size: u64,
}

/// Why a vendor run refused or failed.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
pub enum VendorError {
    /// The output directory exists and is not an empty directory, or is a symlink.
    #[error("vendor output '{}' exists and is not an empty directory", .path.display())]
    #[exit(
        UsageError,
        slug = "vendor_output_not_empty",
        summary = "The vendor output directory exists and is not an empty directory"
    )]
    OutputNotEmpty { path: PathBuf },
    /// A resolved value or an environment record cannot be written as a vendor-relative value.
    #[error("value cannot be vendored: {reason}")]
    #[exit(
        DataError,
        slug = "vendor_value_unrepresentable",
        summary = "A value cannot be written relative to the vendor directory"
    )]
    ValueUnrepresentable { reason: String },
    /// A program, launcher or link target breaks the vendor path rules.
    #[error("path cannot be vendored: {path}")]
    #[exit(
        DataError,
        slug = "vendor_path_unrepresentable",
        summary = "A path cannot be written inside the vendor directory"
    )]
    PathUnrepresentable { path: String },
    /// Resolution, pull, store or I/O failure; classified as its own error.
    #[error(transparent)]
    #[exit(delegate = 0)]
    Package(#[from] crate::Error),
}

impl PackageManager {
    /// Writes the vendor directory `request` describes.
    ///
    /// # Errors
    ///
    /// [`VendorError::OutputNotEmpty`] before any network request, [`VendorError::ValueUnrepresentable`] and
    /// [`VendorError::PathUnrepresentable`] for a value or path the directory cannot hold; resolution, pull and
    /// I/O failures as [`VendorError::Package`].
    pub async fn vendor(&self, request: VendorRequest) -> Result<VendorOutcome, VendorError> {
        let _ = request;
        unimplemented!("PackageManager::vendor")
    }
}

#[cfg(test)]
mod tests {
    use ocx_exit::{ClassifyExitCode, ExitCode};

    use super::*;

    #[test]
    fn output_not_empty_is_a_usage_error_with_its_slug() {
        let error = VendorError::OutputNotEmpty {
            path: PathBuf::from("/out"),
        };
        assert_eq!(error.classify(), Some(ExitCode::UsageError));
    }

    #[test]
    fn unrepresentable_errors_are_data_errors() {
        let value = VendorError::ValueUnrepresentable { reason: "x".into() };
        let path = VendorError::PathUnrepresentable { path: "/x".into() };
        assert_eq!(value.classify(), Some(ExitCode::DataError));
        assert_eq!(path.classify(), Some(ExitCode::DataError));
    }

    /// Red until P7: a vendor run into an absent directory writes its manifest.
    #[tokio::test]
    async fn vendor_manifest_only_writes_manifest_json() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fs = ocx_store::file_structure::FileStructure::with_root(tmp.path().join("home"));
        let index = ocx_index::Index::from_chained(
            ocx_index::LocalIndex::new(ocx_index::LocalConfig {
                index_store: ocx_index::IndexStore::new(tmp.path().join("home/index")),
            }),
            vec![],
            ocx_index::ChainMode::Offline,
        );
        let manager = PackageManager::new(fs, index, None, "localhost:5000");
        let output = tmp.path().join("out");
        let outcome = manager
            .vendor(VendorRequest {
                output: output.clone(),
                platform: Platform::any(),
                selection: VendorSelection::Packages(Vec::new()),
                manifest_only: true,
                omit_links: false,
            })
            .await
            .expect("vendor writes a manifest");
        assert!(outcome.manifest_path.is_file());
        assert_eq!(outcome.content, VendorContent::Absent);
    }
}
