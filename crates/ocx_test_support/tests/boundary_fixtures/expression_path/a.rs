// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: no `use`, one fully-qualified expression path.

pub fn stale(path: &std::path::Path) -> bool {
    crate::project::lock::is_stale(path)
}
