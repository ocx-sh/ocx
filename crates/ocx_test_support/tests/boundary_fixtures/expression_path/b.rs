// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: a sibling that only names std.

use std::path::Path;

pub fn exists(path: &Path) -> bool {
    path.exists()
}
