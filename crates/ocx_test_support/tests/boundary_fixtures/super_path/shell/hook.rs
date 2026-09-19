// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: a sibling under `shell` that reaches only its parent.

pub fn count() -> usize {
    super::hook_count()
}
