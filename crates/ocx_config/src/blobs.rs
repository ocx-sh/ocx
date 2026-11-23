// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `[blobs]` configuration section.
//!
//! Read from `config.toml` tiers only: the loader strips it from a project tier, because a
//! repository must not redirect where a machine reads or writes blobs.

use std::path::PathBuf;

use serde::Deserialize;

/// Where ocx looks for blobs before it contacts a registry, and where it keeps the ones it fetches.
// No `deny_unknown_fields`: a section written for a newer ocx must not brick an older binary.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
pub struct BlobsConfig {
    /// Directories searched, read-only, for blobs before a registry is contacted.
    ///
    /// A relative path resolves against the directory of the `config.toml` that declared it, and the
    /// `[managed]` tier accepts absolute paths only. A later tier replaces this list whole. More
    /// than 32 entries are refused (exit 78). Overridden by the `OCX_BLOB_DIR` environment variable and the
    /// `--blob-dir` flag.
    pub dirs: Option<Vec<PathBuf>>,

    /// Directory where blobs fetched from a registry are kept for later runs. Absent → no blob cache.
    ///
    /// A relative path resolves against the directory of the `config.toml` that declared it.
    /// Overridden by the `OCX_BLOB_CACHE_DIR` environment variable and the `--blob-cache-dir` flag.
    pub cache_dir: Option<PathBuf>,
}

impl BlobsConfig {
    /// Merge `other` into `self`; each key a higher tier sets replaces the lower tier's value.
    pub fn merge(&mut self, other: BlobsConfig) {
        if other.dirs.is_some() {
            self.dirs = other.dirs;
        }
        if other.cache_dir.is_some() {
            self.cache_dir = other.cache_dir;
        }
    }
}
