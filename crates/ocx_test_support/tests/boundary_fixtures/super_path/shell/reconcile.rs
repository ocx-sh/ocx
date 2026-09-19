// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: `super::super::super::project` from an inline module of
//! `shell::reconcile` resolves to `crate::project`.

pub fn plan() -> usize {
    super::hook_count()
}

mod inner {
    pub fn stale() -> bool {
        super::super::super::project::lock::is_stale()
    }
}
