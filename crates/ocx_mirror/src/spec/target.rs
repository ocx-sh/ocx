// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Target {
    pub registry: String,
    pub repository: String,
}
