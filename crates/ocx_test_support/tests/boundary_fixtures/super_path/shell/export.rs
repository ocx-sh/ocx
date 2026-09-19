// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: `use super::super::project::…` from `shell::export` resolves to
//! `crate::project` — the `use`-statement arm of `super` resolution.

use super::super::project::lock;

pub fn stale() -> bool {
    lock::is_stale()
}
