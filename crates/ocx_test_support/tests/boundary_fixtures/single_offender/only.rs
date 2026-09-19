// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: a one-file tree whose only file reaches `crate::project` — the
//! shape a one-file shell takes the moment content lands in its `lib.rs`.

use crate::project::ProjectConfig;

pub fn tier(config: &ProjectConfig) -> u8 {
    config.tier()
}
