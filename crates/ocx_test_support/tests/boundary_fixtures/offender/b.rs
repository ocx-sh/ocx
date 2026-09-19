// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: the one real offending line is the `use` below.

use crate::project::ProjectConfig;

pub fn tier(config: &ProjectConfig) -> u8 {
    config.tier()
}
