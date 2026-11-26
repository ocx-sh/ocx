// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One package of the closure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Package {
    /// Pinned identifier `registry/repository[:tag]@sha256:<hex>`.
    pub identifier: String,
    /// Platform-manifest digest.
    pub digest: String,
    /// `packages/<registry>/<algorithm>/<2 hex>/<30 hex>`.
    pub package_dir: String,
    /// `<package_dir>/content`.
    pub content_dir: String,
    /// Digests of direct dependencies; sorted.
    pub dependencies: Vec<String>,
    /// `false` when the metadata claims neither binaries nor entry points.
    pub names_declared: bool,
    /// The package's own launchers; sorted by `name`.
    pub entrypoints: Vec<EntryPoint>,
    /// Present iff `content` is `present`.
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree: Option<Tree>,
}

/// A launcher of the package under `<package_dir>/entrypoints/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EntryPoint {
    /// The entry point's name.
    pub name: String,
    /// Ends in `.exe` for a Windows platform.
    pub launcher_path: String,
    /// Id of the entry point's private environment.
    pub environment: String,
}

/// The files and links under a package's `content_dir`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Tree {
    /// Every regular file; sorted by `path`.
    pub files: Vec<FileEntry>,
    /// Every symlink; sorted by `path`.
    pub links: Vec<LinkEntry>,
    /// Top-most directories with no link at or below them; sorted.
    pub link_free_dirs: Vec<String>,
}

/// A regular file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FileEntry {
    /// Relative to `DIR`.
    pub path: String,
    /// Whether the executable bit is set.
    pub executable: bool,
}

/// A symlink.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LinkEntry {
    /// Relative to `DIR`.
    pub path: String,
    /// Relative; resolves inside `content_dir`.
    pub target: String,
}
