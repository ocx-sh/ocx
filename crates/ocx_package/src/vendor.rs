// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The vendor manifest: `DIR/manifest.json` of `ocx vendor`, the document a build rule reads instead of the store.

mod environment;
mod package;
mod selection;

pub use environment::{Environment, EnvironmentItem, EnvironmentScope};
pub use package::{EntryPoint, FileEntry, LinkEntry, Package, Tree};
pub use selection::{Selection, Tool, ToolKind};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The one token a manifest value may contain; stands for the absolute path of `DIR`.
pub const DIR_TOKEN: &str = "@VENDOR_DIR@";

/// File name of the manifest inside `DIR`.
pub const MANIFEST_FILE: &str = "manifest.json";

/// Value of `schema_version`; bumped only through the contract ledger.
pub const SCHEMA_VERSION: u32 = 1;

/// Whether the package files are on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Content {
    /// Package files, launchers and sidecars exist.
    Present,
    /// `--manifest-only`: only the manifest exists.
    Absent,
}

/// The vendor manifest, `https://ocx.sh/schemas/vendor-manifest/v1.json`.
///
/// Every path field is relative to `DIR`, `/`-separated; every list is sorted by its stated key so the bytes are stable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VendorManifest {
    /// Always [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// `absent` after `--manifest-only`.
    pub content: Content,
    /// Canonical platform string of the run.
    pub platform: String,
    /// Platforms on which every selected package has a leaf, computed before leaf selection; sorted.
    pub available_platforms: Vec<String>,
    /// `sha256:` + hex digest of the platform and the sorted package digests; see [`vendor_id`].
    pub vendor_id: String,
    /// Always [`DIR_TOKEN`].
    pub dir_token: String,
    /// Every package of the closure, once; sorted by `digest`.
    pub packages: Vec<Package>,
    /// The selection's environment first, then entry-point environments sorted by `id`.
    pub environments: Vec<Environment>,
    /// One entry in v1.
    pub selections: Vec<Selection>,
    /// Every exposed name; sorted by `(selection, name)`.
    pub tools: Vec<Tool>,
}

/// The identity of a vendored set: `sha256:` + hex SHA-256 of `platform`, `\n`, then the sorted package digests
/// joined by `\n`. Two directories with equal ids hold the same files.
pub fn vendor_id(_platform: &str, _package_digests: &[&str]) -> String {
    unimplemented!("vendor_id")
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("vendor/example.json");

    #[test]
    fn example_round_trips() {
        let manifest: VendorManifest = serde_json::from_str(EXAMPLE).expect("example parses");
        let again = serde_json::to_value(&manifest).expect("serializes");
        let original: serde_json::Value = serde_json::from_str(EXAMPLE).expect("example is JSON");
        assert_eq!(original, again);
        assert_eq!(manifest.schema_version, SCHEMA_VERSION);
        assert_eq!(manifest.dir_token, DIR_TOKEN);
    }

    #[test]
    fn vendor_id_hashes_platform_then_sorted_digests() {
        // printf 'linux/amd64+libc.glibc\nsha256:1f2e…' | sha256sum
        let digest = "sha256:1f2e3d4c5b6a79881f2e3d4c5b6a79881f2e3d4c5b6a79881f2e3d4c5b6a7988";
        assert_eq!(
            vendor_id("linux/amd64+libc.glibc", &[digest]),
            "sha256:c4f5716a0f7b8fdb89a5d399a004e7920a2a1ed81f84fad7e4aaf41b2ad9eafb"
        );
    }
}
