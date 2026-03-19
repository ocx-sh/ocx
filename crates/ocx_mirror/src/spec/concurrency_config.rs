// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[allow(dead_code)] // Fields read from YAML spec; used when concurrency control is implemented
pub struct ConcurrencyConfig {
    #[serde(default = "default_max_downloads")]
    pub max_downloads: usize,
    #[serde(default = "default_max_bundles")]
    pub max_bundles: usize,
    #[serde(default = "default_max_pushes")]
    pub max_pushes: usize,
    #[serde(default)]
    pub rate_limit_ms: u64,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// Number of compression threads per bundle task.
    /// `0` (default) = auto: `max(1, available_parallelism / max_bundles)`.
    /// `1` = single-threaded (no block overhead).
    #[serde(default)]
    pub compression_threads: usize,
}

impl Default for ConcurrencyConfig {
    fn default() -> Self {
        Self {
            max_downloads: default_max_downloads(),
            max_bundles: default_max_bundles(),
            max_pushes: default_max_pushes(),
            rate_limit_ms: 0,
            max_retries: default_max_retries(),
            compression_threads: 0,
        }
    }
}

fn default_max_downloads() -> usize {
    8
}

fn default_max_bundles() -> usize {
    std::thread::available_parallelism()
        .map(|p| (p.get() / 2).max(1))
        .unwrap_or(2)
}

fn default_max_pushes() -> usize {
    2
}

fn default_max_retries() -> u32 {
    3
}

/// Resolve `compression_threads = 0` (auto) to a concrete value based on available parallelism
/// and the number of concurrent bundle tasks.
///
/// - `compression_threads > 0`: returns it directly (explicit override).
/// - `compression_threads == 0` with `max_bundles <= 1`: returns `0` (auto-detect at library level).
/// - `compression_threads == 0` with `max_bundles > 1`: divides available cores across bundles.
pub fn resolve_compression_threads(compression_threads: usize, max_bundles: usize) -> u32 {
    if compression_threads > 0 {
        return compression_threads as u32;
    }
    if max_bundles <= 1 {
        return 0;
    }
    let base = ocx_lib::compression::default_threads();
    (base / max_bundles as u32).max(1)
}
