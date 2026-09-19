// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: reaches `project` only through the lib-root re-export, which an
//! empty table would silently forgive.

use crate::ProjectConfig;

pub fn tier(config: &ProjectConfig) -> u8 {
    config.tier()
}
