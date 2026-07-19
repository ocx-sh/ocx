// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::file_structure::IndexStore;

/// Construction inputs for [`super::LocalIndex`].
///
/// A single self-contained [`IndexStore`] rooted at the index home holds
/// both the per-repository root documents and the verbatim,
/// digest-verified dispatch-object CAS (`o/sha256/<hex>.json`) — see
/// `adr_index_indirection.md` Decision A1.
pub struct Config {
    pub index_store: IndexStore,
}
