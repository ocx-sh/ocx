// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::compression;

#[derive(Default)]
pub struct ExtractOptions {
    pub algorithm: Option<compression::CompressionAlgorithm>,
    pub strip_components: usize,
}
