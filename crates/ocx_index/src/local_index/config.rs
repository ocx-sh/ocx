// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::IndexStore;

/// Construction inputs for [`super::LocalIndex`] (`adr_index_indirection.md#a1`).
pub struct Config {
    pub index_store: IndexStore,
}
